use crate::{auth::AuthClient, model::*, store::Store};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path as WebPath, Query, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use rsa::rand_core::{OsRng, RngCore};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    io::Cursor,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Mutex, Notify, RwLock, broadcast};
use zeroize::Zeroize;

const MAX_IMAGE: usize = 10 * 1024 * 1024;

#[derive(Clone)]
pub struct AppState(Arc<Inner>);
struct Inner {
    store: Store,
    settings: RwLock<Settings>,
    status: RwLock<Status>,
    operation: Mutex<()>,
    notify: Notify,
    events: broadcast::Sender<String>,
    image_directory: Option<PathBuf>,
}

#[derive(Debug)]
pub struct ApiError(pub StatusCode, pub String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self(StatusCode::BAD_REQUEST, e.to_string())
    }
}
type ApiResult<T> = Result<T, ApiError>;
fn internal(e: anyhow::Error) -> ApiError {
    tracing::error!(error=%e,"存储操作失败");
    ApiError(
        StatusCode::INTERNAL_SERVER_ERROR,
        "数据存储失败，请检查服务日志".into(),
    )
}

impl AppState {
    pub fn new(store: Store, image_directory: Option<PathBuf>) -> anyhow::Result<Self> {
        let settings = store.settings()?;
        settings.validate()?;
        let image_directory = image_directory.map(|p| p.canonicalize()).transpose()?;
        if let Some(dir) = &image_directory {
            anyhow::ensure!(dir.is_dir(), "图片目录不是文件夹");
        }
        let (events, _) = broadcast::channel(32);
        Ok(Self(Arc::new(Inner {
            store,
            settings: RwLock::new(settings),
            status: RwLock::new(Status::default()),
            operation: Mutex::new(()),
            notify: Notify::new(),
            events,
            image_directory,
        })))
    }

    pub async fn snapshot(&self) -> ApiResult<Value> {
        Ok(
            json!({"status":self.0.status.read().await.clone(),"account":self.0.store.account().map_err(internal)?,
            "settings":self.0.settings.read().await.clone(),"logs":self.0.store.logs(1).map_err(internal)?}),
        )
    }

    async fn publish(&self) {
        if let Ok(value) = self.snapshot().await {
            let _ = self.0.events.send(value.to_string());
        }
    }

    async fn set_busy(&self, action: Option<&str>) {
        self.0.status.write().await.busy = action.map(str::to_owned);
        self.publish().await;
    }

    async fn check(&self, client: &AuthClient) -> Status {
        let previous = self.0.status.read().await.clone();
        let mut status = match client.status().await {
            Ok(mut s) => {
                if s.state == Connection::Online && s.online_since.is_none() {
                    let same_account = s.username == previous.username;
                    // Preserve the observed start through temporary detection errors.
                    s.online_since = if same_account {
                        previous.online_since
                    } else {
                        None
                    }
                    .or(s.checked_at);
                    s.duration_source = Some("observed".into());
                }
                s
            }
            Err(e) => Status {
                state: Connection::Unknown,
                message: e.to_string(),
                checked_at: Some(chrono::Utc::now().timestamp()),
                online_since: previous.online_since,
                duration_source: previous.duration_source.clone(),
                username: previous.username.clone(),
                ip: previous.ip.clone(),
                busy: None,
            },
        };
        status.busy = previous.busy;
        *self.0.status.write().await = status.clone();
        // Record changes and error transitions, not every periodic tick.
        if status.state != previous.state
            || status.state == Connection::Unknown && status.message != previous.message
        {
            let _ = self.0.store.log(
                "status",
                status.state != Connection::Unknown,
                &status.message,
            );
        }
        self.publish().await;
        status
    }

    async fn refresh_locked(&self) -> ApiResult<Status> {
        self.set_busy(Some("refresh")).await;
        let settings = self.0.settings.read().await.clone();
        let result = match AuthClient::new(&settings) {
            Ok(c) => Ok(self.check(&c).await),
            Err(e) => Err(e.into()),
        };
        self.set_busy(None).await;
        result.map(|mut s| {
            s.busy = None;
            s
        })
    }

    pub fn start_monitor(&self) -> tokio::task::JoinHandle<()> {
        let app = self.clone();
        tokio::spawn(async move {
            loop {
                let settings = app.0.settings.read().await.clone();
                if settings.auto_refresh
                    && let Ok(_guard) = app.0.operation.try_lock()
                {
                    let _ = app.refresh_locked().await;
                }
                if settings.auto_refresh {
                    tokio::select! { _=tokio::time::sleep(Duration::from_secs(settings.refresh_interval_seconds))=>{}, _=app.0.notify.notified()=>{} }
                } else {
                    app.0.notify.notified().await;
                }
            }
        })
    }

    pub async fn operate(&self, action: &str) -> ApiResult<Value> {
        let _guard = self
            .0
            .operation
            .try_lock()
            .map_err(|_| ApiError(StatusCode::CONFLICT, "另一个操作正在进行，请稍候".into()))?;
        let settings = self.0.settings.read().await.clone();
        let client = AuthClient::new(&settings)?;
        let credentials = if action == "login" {
            Some(
                self.0
                    .store
                    .credentials()
                    .map_err(internal)?
                    .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "请先保存账号密码".into()))?,
            )
        } else {
            None
        };
        self.set_busy(Some(action)).await;
        let before = self.check(&client).await;
        let already = action == "login" && before.state == Connection::Online
            || action == "logout" && before.state == Connection::Offline;
        let (success, message) = if already {
            (
                true,
                if action == "login" {
                    "当前已经在线"
                } else {
                    "当前已经离线"
                }
                .to_string(),
            )
        } else {
            let requested = if action == "login" {
                client.login(credentials.as_ref().unwrap()).await
            } else {
                client.logout().await
            };
            match requested {
                Err(e) => (false, e.to_string()),
                Ok(()) => {
                    let expected = if action == "login" {
                        Connection::Online
                    } else {
                        Connection::Offline
                    };
                    let mut after = self.check(&client).await;
                    for delay in [400, 800] {
                        if after.state == expected {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                        after = self.check(&client).await;
                    }
                    if after.state == expected {
                        (
                            true,
                            if action == "login" {
                                "校园网认证成功"
                            } else {
                                "已退出校园网，注销成功"
                            }
                            .into(),
                        )
                    } else if after.state == Connection::Unknown {
                        (
                            false,
                            format!(
                                "{}请求已提交，但无法确认状态：{}",
                                if action == "login" {
                                    "登录"
                                } else {
                                    "注销"
                                },
                                after.message
                            ),
                        )
                    } else {
                        (
                            false,
                            if action == "login" {
                                "已提交认证，但当前设备仍未在线"
                            } else {
                                "已发送注销请求，但当前设备仍显示在线"
                            }
                            .into(),
                        )
                    }
                }
            }
        };
        // Secrets are never present in the fixed/auth-classified result messages.
        let logged = self
            .0
            .store
            .log(action, success, &message)
            .map_err(internal);
        self.set_busy(None).await;
        logged?;
        let value = json!({"success":success,"message":message,"status":self.0.status.read().await.clone()});
        if success {
            Ok(value)
        } else {
            Err(ApiError(StatusCode::BAD_GATEWAY, message))
        }
    }

    fn background_files(&self) -> Vec<(Background, PathBuf)> {
        let mut files = Vec::new();
        let roots = [
            ("upload", Some(self.0.store.directory.join("backgrounds"))),
            ("library", self.0.image_directory.clone()),
        ];
        for (source, root) in roots {
            let Some(root) = root else { continue };
            let Ok(entries) = std::fs::read_dir(&root) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !safe_image_path(&root, &path) {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                // Hex-encode the filename. API IDs never become filesystem paths.
                let id = format!(
                    "{source}-{}",
                    name.as_bytes()
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>()
                );
                files.push((
                    Background {
                        id: id.clone(),
                        name,
                        url: format!("/api/backgrounds/{id}"),
                        source: source.into(),
                    },
                    path,
                ));
            }
        }
        files.sort_by(|a, b| a.0.id.cmp(&b.0.id));
        files
    }
}

pub fn safe_image_path(root: &Path, path: &Path) -> bool {
    let Ok(meta) = path.symlink_metadata() else {
        return false;
    };
    if !meta.is_file() || meta.len() > MAX_IMAGE as u64 || meta.file_type().is_symlink() {
        return false;
    }
    let extension = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "webp") {
        return false;
    }
    match (root.canonicalize(), path.canonicalize()) {
        (Ok(r), Ok(p)) => p.starts_with(r),
        _ => false,
    }
}

pub fn router(app: AppState) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async {
                asset(
                    "text/html; charset=utf-8",
                    include_bytes!("../web/index.html"),
                )
            }),
        )
        .route(
            "/assets/style.css",
            get(|| async {
                asset(
                    "text/css; charset=utf-8",
                    include_bytes!("../web/style.css"),
                )
            }),
        )
        .route(
            "/assets/app.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_bytes!("../web/app.js"),
                )
            }),
        )
        .route(
            "/assets/background.svg",
            get(|| async { asset("image/svg+xml", include_bytes!("../web/background.svg")) }),
        )
        .route(
            "/assets/logo.svg",
            get(|| async { asset("image/svg+xml", include_bytes!("../web/logo.svg")) }),
        )
        .route(
            "/healthz",
            get(|| async { Json(json!({"ok":true,"version":env!("CARGO_PKG_VERSION")})) }),
        )
        .route("/api/status", get(status))
        .route("/api/status/refresh", post(refresh))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/account", get(account).put(save_account))
        .route("/api/account/reveal", post(reveal))
        .route("/api/settings", get(settings).put(save_settings))
        .route("/api/logs", get(logs))
        .route("/api/events", get(events))
        .route("/api/backgrounds", get(backgrounds).post(upload))
        .route("/api/backgrounds/{id}", get(background))
        .layer(DefaultBodyLimit::max(MAX_IMAGE + 1024 * 1024))
        .layer(middleware::from_fn(guard))
        .with_state(app)
}

fn asset(content_type: &str, bytes: &'static [u8]) -> Response {
    ([(header::CONTENT_TYPE, content_type)], bytes).into_response()
}

async fn guard(request: Request, next: Next) -> Response {
    if !matches!(
        *request.method(),
        axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS
    ) {
        if request
            .headers()
            .get("x-neu-request")
            .and_then(|h| h.to_str().ok())
            != Some("1")
        {
            return ApiError(StatusCode::FORBIDDEN, "请求缺少同源操作标识".into()).into_response();
        }
        if request
            .headers()
            .get("sec-fetch-site")
            .and_then(|h| h.to_str().ok())
            == Some("cross-site")
        {
            return ApiError(StatusCode::FORBIDDEN, "不允许跨站操作".into()).into_response();
        }
        if let Some(origin) = request.headers().get(header::ORIGIN) {
            let origin = origin.to_str().ok().and_then(|s| url::Url::parse(s).ok());
            let host = request
                .headers()
                .get(header::HOST)
                .and_then(|h| h.to_str().ok())
                .and_then(|s| url::Url::parse(&format!("http://{s}")).ok());
            let matches = match (origin, host) {
                (Some(o), Some(h)) => {
                    o.host_str() == h.host_str()
                        && o.port() == h.port()
                        && matches!(o.scheme(), "http" | "https")
                }
                _ => false,
            };
            if !matches {
                return ApiError(StatusCode::FORBIDDEN, "不允许跨站操作".into()).into_response();
            }
        }
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY,HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' blob: data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'"));
    response
}

async fn status(State(app): State<AppState>) -> Json<Status> {
    Json(app.0.status.read().await.clone())
}
async fn refresh(State(app): State<AppState>) -> ApiResult<Json<Status>> {
    // Finish the operation even if the browser closes its connection.
    let result = tokio::spawn(async move {
        let _guard = app
            .0
            .operation
            .try_lock()
            .map_err(|_| ApiError(StatusCode::CONFLICT, "另一个操作正在进行，请稍候".into()))?;
        app.refresh_locked().await
    })
    .await
    .map_err(|e| internal(e.into()))??;
    Ok(Json(result))
}
async fn login(State(app): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(
        tokio::spawn(async move { app.operate("login").await })
            .await
            .map_err(|e| internal(e.into()))??,
    ))
}
async fn logout(State(app): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(
        tokio::spawn(async move { app.operate("logout").await })
            .await
            .map_err(|e| internal(e.into()))??,
    ))
}
async fn account(State(app): State<AppState>) -> ApiResult<Json<AccountView>> {
    Ok(Json(app.0.store.account().map_err(internal)?))
}
async fn reveal(State(app): State<AppState>) -> ApiResult<Json<Value>> {
    let c = app
        .0
        .store
        .credentials()
        .map_err(internal)?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "尚未保存账号".into()))?;
    Ok(Json(json!({"password":c.password})))
}
async fn save_account(
    State(app): State<AppState>,
    Json(mut input): Json<AccountInput>,
) -> ApiResult<Json<AccountView>> {
    let _guard = app.0.operation.try_lock().map_err(|_| {
        ApiError(
            StatusCode::CONFLICT,
            "认证正在进行，暂时不能修改账号".into(),
        )
    })?;
    let result = app
        .0
        .store
        .save_account(&input.username, input.password.as_deref());
    if let Some(p) = &mut input.password {
        p.zeroize();
    }
    result?;
    app.0
        .store
        .log("account", true, "账号设置已保存")
        .map_err(internal)?;
    app.publish().await;
    Ok(Json(app.0.store.account().map_err(internal)?))
}
async fn settings(State(app): State<AppState>) -> Json<Settings> {
    Json(app.0.settings.read().await.clone())
}
async fn save_settings(
    State(app): State<AppState>,
    Json(settings): Json<Settings>,
) -> ApiResult<Json<Settings>> {
    let _guard = app.0.operation.try_lock().map_err(|_| {
        ApiError(
            StatusCode::CONFLICT,
            "认证正在进行，暂时不能修改设置".into(),
        )
    })?;
    settings.validate()?;
    if settings.background != "default"
        && !app
            .background_files()
            .iter()
            .any(|(b, _)| b.id == settings.background)
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "所选背景不存在，请重新选择".into(),
        ));
    }
    app.0.store.save_settings(&settings).map_err(internal)?;
    *app.0.settings.write().await = settings.clone();
    app.0
        .store
        .log("settings", true, "应用设置已保存")
        .map_err(internal)?;
    app.0.notify.notify_one();
    app.publish().await;
    Ok(Json(settings))
}
#[derive(Deserialize)]
struct Pagination {
    page: Option<u32>,
}
async fn logs(
    State(app): State<AppState>,
    Query(p): Query<Pagination>,
) -> ApiResult<Json<LogPage>> {
    Ok(Json(
        app.0.store.logs(p.page.unwrap_or(1)).map_err(internal)?,
    ))
}

async fn events(State(app): State<AppState>) -> ApiResult<impl IntoResponse> {
    let mut rx = app.0.events.subscribe();
    let initial = app.snapshot().await?.to_string();
    let stream = async_stream::stream! {
        yield Ok::<_,Infallible>(Event::default().event("update").data(initial));
        loop {
            match rx.recv().await {
                Ok(data)=>yield Ok(Event::default().event("update").data(data)),
                Err(broadcast::error::RecvError::Lagged(_))=>if let Ok(snapshot)=app.snapshot().await {yield Ok(Event::default().event("update").data(snapshot.to_string()));},
                Err(broadcast::error::RecvError::Closed)=>break,
            }
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

async fn backgrounds(State(app): State<AppState>) -> Json<Value> {
    let mut images = vec![Background {
        id: "default".into(),
        name: "山湖 · 日落".into(),
        url: "/assets/background.svg".into(),
        source: "builtin".into(),
    }];
    images.extend(app.background_files().into_iter().map(|(b, _)| b));
    Json(json!({"items":images,"library_configured":app.0.image_directory.is_some()}))
}
async fn background(
    State(app): State<AppState>,
    WebPath(id): WebPath<String>,
) -> ApiResult<Response> {
    let (_, path) = app
        .background_files()
        .into_iter()
        .find(|(b, _)| b.id == id)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "背景图片不存在".into()))?;
    let data = tokio::fs::read(&path)
        .await
        .map_err(|e| internal(e.into()))?;
    let format = image::guess_format(&data)
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "图片格式无效".into()))?;
    let mime = match format {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::WebP => "image/webp",
        _ => {
            return Err(ApiError(
                StatusCode::BAD_REQUEST,
                "仅支持 PNG、JPEG、WebP 图片".into(),
            ));
        }
    };
    Ok(([(header::CONTENT_TYPE, mime)], Body::from(data)).into_response())
}

pub fn validate_image(data: &[u8]) -> anyhow::Result<&'static str> {
    anyhow::ensure!(
        !data.is_empty() && data.len() <= MAX_IMAGE,
        "图片大小应为 1 字节至 10 MB"
    );
    let mut reader = image::ImageReader::new(Cursor::new(data)).with_guessed_format()?;
    let extension = match reader.format() {
        Some(image::ImageFormat::Png) => "png",
        Some(image::ImageFormat::Jpeg) => "jpg",
        Some(image::ImageFormat::WebP) => "webp",
        _ => anyhow::bail!("仅支持 PNG、JPEG、WebP 图片"),
    };
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    reader
        .decode()
        .map_err(|_| anyhow::anyhow!("图片损坏或分辨率过大，请选择较小的有效图片"))?;
    Ok(extension)
}
async fn upload(
    State(app): State<AppState>,
    mut multipart: Multipart,
) -> ApiResult<Json<Background>> {
    let field = multipart
        .next_field()
        .await
        .map_err(|_| {
            ApiError(
                StatusCode::BAD_REQUEST,
                "图片上传格式无效或超过 10 MB".into(),
            )
        })?
        .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "请选择图片".into()))?;
    if field.name() != Some("file") {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "上传字段应为 file".into(),
        ));
    }
    let data = field
        .bytes()
        .await
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "图片读取失败或超过 10 MB".into()))?;
    let ext = tokio::task::spawn_blocking({
        let data = data.clone();
        move || validate_image(&data)
    })
    .await
    .map_err(|e| internal(e.into()))??;
    let mut random = [0u8; 8];
    OsRng.fill_bytes(&mut random);
    let name = format!(
        "{}-{}.{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S"),
        random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
        ext
    );
    let path = app.0.store.directory.join("backgrounds").join(&name);
    tokio::fs::write(&path, &data)
        .await
        .map_err(|e| internal(e.into()))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| internal(e.into()))?;
    app.0
        .store
        .log("background", true, "背景图片已上传")
        .map_err(internal)?;
    app.publish().await;
    let b = app
        .background_files()
        .into_iter()
        .find(|(_, p)| *p == path)
        .context_api("无法读取已上传图片")?;
    Ok(Json(b.0))
}

trait ApiOption<T> {
    fn context_api(self, msg: &str) -> ApiResult<T>;
}
impl<T> ApiOption<T> for Option<T> {
    fn context_api(self, msg: &str) -> ApiResult<T> {
        self.ok_or_else(|| ApiError(StatusCode::INTERNAL_SERVER_ERROR, msg.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{Fixture, Mode};
    use axum::http::Request;
    use std::sync::atomic::Ordering;
    use tower::ServiceExt;

    async fn fixture_app(f: &Fixture, dir: &Path) -> AppState {
        let store = Store::open(dir).unwrap();
        store.save_account("学生", Some("密碼")).unwrap();
        let app = AppState::new(store, None).unwrap();
        // HTTP is allowed only in this in-process mock. The public settings API requires HTTPS.
        *app.0.settings.write().await = f.settings.clone();
        app
    }

    #[tokio::test]
    async fn full_sso_flow_and_verified_logout_with_idempotency() {
        let fixture = Fixture::start().await;
        let dir = tempfile::tempdir().unwrap();
        let app = fixture_app(&fixture, dir.path()).await;
        assert!(
            app.operate("login").await.unwrap()["success"]
                .as_bool()
                .unwrap()
        );
        assert_eq!(app.0.status.read().await.state, Connection::Online);
        assert_eq!(
            app.0.status.read().await.duration_source.as_deref(),
            Some("gateway")
        );
        assert_eq!(fixture.state.posts.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.state.redeemed.load(Ordering::SeqCst), 1);
        assert!(app.operate("login").await.is_ok());
        assert_eq!(fixture.state.posts.load(Ordering::SeqCst), 1);
        fixture.mode(Mode::LogoutStaysOnline);
        let error = match app.operate("logout").await {
            Err(e) => e,
            Ok(_) => panic!("HTTP 200 must not imply logout success"),
        };
        assert!(error.1.contains("仍显示在线"));
        assert_eq!(app.0.status.read().await.state, Connection::Online);
        fixture.mode(Mode::Normal);
        assert!(app.operate("logout").await.is_ok());
        assert_eq!(app.0.status.read().await.state, Connection::Offline);
        assert!(app.operate("logout").await.is_ok());
        let messages = serde_json::to_string(&app.0.store.logs(1).unwrap()).unwrap();
        assert!(!messages.contains("TEST-SECRET-TOKEN"));
        assert!(!messages.contains("密碼"));
    }

    #[tokio::test]
    async fn auth_errors_and_unconfirmed_results_do_not_leak_upstream_secrets() {
        let fixture = Fixture::start().await;
        let dir = tempfile::tempdir().unwrap();
        let app = fixture_app(&fixture, dir.path()).await;
        for mode in [Mode::WrongPassword, Mode::MissingFields, Mode::MalformedApi] {
            fixture.mode(mode);
            let error = match app.operate("login").await {
                Err(e) => e,
                Ok(_) => panic!("invalid auth should fail"),
            };
            assert!(!error.1.contains("TEST-SECRET-TOKEN"));
            assert!(app.0.status.read().await.busy.is_none());
        }
        fixture.mode(Mode::UnknownStatus);
        let error = match app.operate("login").await {
            Err(e) => e,
            Ok(_) => panic!("unknown status cannot confirm login"),
        };
        assert!(error.1.contains("无法确认状态"));
        assert_eq!(app.0.status.read().await.state, Connection::Unknown);
    }

    #[tokio::test]
    async fn timeout_is_unknown_and_concurrent_operations_are_rejected() {
        let fixture = Fixture::start().await;
        let dir = tempfile::tempdir().unwrap();
        let app = fixture_app(&fixture, dir.path()).await;
        fixture.mode(Mode::SlowStatus);
        app.0.settings.write().await.timeout_seconds = 1;
        let (first, second) = tokio::join!(
            async {
                let _lock = app.0.operation.lock().await;
                app.refresh_locked().await
            },
            app.operate("logout")
        );
        assert_eq!(first.unwrap().state, Connection::Unknown);
        assert_eq!(second.err().unwrap().0, StatusCode::CONFLICT);
        assert!(app.0.status.read().await.message.contains("超时"));
        assert!(app.0.status.read().await.busy.is_none());
    }
    #[tokio::test]
    async fn account_api_blocks_cross_origin_and_never_returns_password_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let app = AppState::new(Store::open(dir.path()).unwrap(), None).unwrap();
        let routes = router(app);
        let body = r#"{"username":"s","password":"secret"}"#;
        let req = |origin: &str| {
            Request::builder()
                .method("PUT")
                .uri("/api/account")
                .header("content-type", "application/json")
                .header("host", "localhost:11099")
                .header("origin", origin)
                .header("x-neu-request", "1")
                .body(Body::from(body))
                .unwrap()
        };
        assert_eq!(
            routes
                .clone()
                .oneshot(req("http://evil.example"))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let r = routes
            .clone()
            .oneshot(req("http://localhost:11099"))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");
        let bytes = axum::body::to_bytes(r.into_body(), 8192).await.unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("secret"));
        let r = routes
            .oneshot(
                Request::builder()
                    .uri("/api/account")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(r.into_body(), 8192).await.unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("secret"));
    }
    #[test]
    fn uploads_require_decodable_images_and_paths_cannot_escape_directory() {
        let mut buffer = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 2)
            .write_to(&mut buffer, image::ImageFormat::Png)
            .unwrap();
        assert_eq!(validate_image(buffer.get_ref()).unwrap(), "png");
        assert!(validate_image(b"<svg><script>alert(1)</script></svg>").is_err());
        assert!(validate_image(b"\x89PNG\r\n\x1a\ntruncated").is_err());
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let source = outside.path().join("secret.png");
        std::fs::write(&source, b"test").unwrap();
        std::os::unix::fs::symlink(&source, dir.path().join("escape.png")).unwrap();
        assert!(!safe_image_path(dir.path(), &source));
        assert!(!safe_image_path(dir.path(), &dir.path().join("escape.png")));
    }
}
