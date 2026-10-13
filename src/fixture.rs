//! A local fake gateway. No real accounts or school servers are contacted by tests.
use crate::model::Settings;
use axum::{
    Form, Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use rsa::{
    Pkcs1v15Encrypt, RsaPrivateKey,
    pkcs8::{EncodePublicKey, LineEnding},
    rand_core::OsRng,
};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Clone, Copy)]
pub enum Mode {
    Normal = 0,
    WrongPassword = 1,
    MissingFields = 2,
    MalformedApi = 3,
    LogoutStaysOnline = 4,
    UnknownStatus = 5,
    SlowStatus = 6,
}
pub struct Mock {
    mode: AtomicU8,
    pub online: AtomicBool,
    pub posts: AtomicUsize,
    pub redeemed: AtomicUsize,
    key: RsaPrivateKey,
}
pub struct Fixture {
    pub settings: Settings,
    pub state: Arc<Mock>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    pub async fn start() -> Self {
        let key = RsaPrivateKey::new(&mut OsRng, 2048).unwrap();
        let public = key
            .to_public_key()
            .to_public_key_pem(LineEnding::LF)
            .unwrap();
        let state = Arc::new(Mock {
            mode: AtomicU8::new(0),
            online: AtomicBool::new(false),
            posts: AtomicUsize::new(0),
            redeemed: AtomicUsize::new(0),
            key,
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let routes = Router::new()
            .route(
                "/srun_portal_pc",
                get(|| async {
                    (
                        StatusCode::FOUND,
                        [("location", "/portal?ac_id=33&ip=10.0.0.2")],
                        "",
                    )
                }),
            )
            .route("/portal", get(|| async { "gateway portal" }))
            .route("/tpass/login", get(cas_page).post(cas_submit))
            .route("/srun_portal_sso", get(portal_ticket))
            .route("/v1/srun_portal_sso", get(redeem))
            .route("/cgi-bin/rad_user_info", get(status))
            .route("/cgi-bin/srun_portal", get(logout))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, routes).await.unwrap();
        });
        let settings = Settings {
            auth_mode: "custom".into(),
            cas_login_url: format!("{address}/tpass/login"),
            ipgw_base_url: address,
            rsa_public_key: public,
            timeout_seconds: 2,
            ..Settings::default()
        };
        Self {
            settings,
            state,
            task,
        }
    }
    pub fn mode(&self, mode: Mode) {
        self.state.mode.store(mode as u8, Ordering::SeqCst);
    }
}
async fn cas_page(
    State(m): State<Arc<Mock>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    assert!(q["service"].contains("ac_id=33"));
    assert!(q["service"].contains("ip=10.0.0.2"));
    let page = if m.mode.load(Ordering::SeqCst) == Mode::MissingFields as u8 {
        "<html>changed page</html>"
    } else {
        "<input name='lt' value='LT-MOCK'><input name='execution' value='e1s1'>"
    };
    ([("set-cookie", "prep=bound; Path=/")], page).into_response()
}
async fn cas_submit(
    State(m): State<Arc<Mock>>,
    headers: HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    m.posts.fetch_add(1, Ordering::SeqCst);
    assert!(headers["cookie"].to_str().unwrap().contains("prep=bound"));
    assert_eq!(form["lt"], "LT-MOCK");
    assert_eq!(form["execution"], "e1s1");
    assert_eq!(form["_eventId"], "submit");
    assert_eq!(form["ul"], "学生".len().to_string());
    assert_eq!(form["pl"], "密碼".len().to_string());
    assert_eq!(
        m.key
            .decrypt(Pkcs1v15Encrypt, &STANDARD.decode(&form["rsa"]).unwrap())
            .unwrap(),
        "学生密碼".as_bytes()
    );
    if m.mode.load(Ordering::SeqCst) == Mode::WrongPassword as u8 {
        return "<span id='errormsghide'>密码错误 TEST-SECRET-TOKEN</span>".into_response();
    }
    (
        StatusCode::FOUND,
        [
            ("set-cookie", "sso=ready; Path=/"),
            (
                "location",
                "/srun_portal_sso?ac_id=33&ticket=TEST-SECRET-TOKEN",
            ),
        ],
        "",
    )
        .into_response()
}
async fn portal_ticket() -> Response {
    panic!("CAS ticket must be redeemed only at the API, not consumed by the HTML portal")
}
async fn redeem(
    State(m): State<Arc<Mock>>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    assert_eq!(q["ticket"], "TEST-SECRET-TOKEN");
    assert_eq!(q["ac_id"], "33");
    assert!(headers["cookie"].to_str().unwrap().contains("sso=ready"));
    m.redeemed.fetch_add(1, Ordering::SeqCst);
    if m.mode.load(Ordering::SeqCst) == Mode::MalformedApi as u8 {
        return "<html>TEST-SECRET-TOKEN</html>".into_response();
    }
    m.online.store(true, Ordering::SeqCst);
    Json(serde_json::json!({"code":0,"message":"success"})).into_response()
}
async fn status(State(m): State<Arc<Mock>>) -> Response {
    let mode = m.mode.load(Ordering::SeqCst);
    if mode == Mode::SlowStatus as u8 {
        tokio::time::sleep(Duration::from_millis(1400)).await;
    }
    if mode == Mode::UnknownStatus as u8 {
        return "<html>server error TEST-SECRET-TOKEN</html>".into_response();
    }
    if m.online.load(Ordering::SeqCst) {
        format!("json({{\"error\":\"ok\",\"user_name\":\"学生\",\"online_ip\":\"10.0.0.2\",\"online_time\":{}}});",chrono::Utc::now().timestamp()-90).into_response()
    } else {
        "json({\"error\":\"not_online_error\"})".into_response()
    }
}
async fn logout(State(m): State<Arc<Mock>>, Query(q): Query<HashMap<String, String>>) -> Response {
    assert_eq!(q["action"], "logout");
    assert_eq!(q["username"], "");
    if m.mode.load(Ordering::SeqCst) != Mode::LogoutStaysOnline as u8 {
        m.online.store(false, Ordering::SeqCst);
    }
    "logout request accepted".into_response()
}
