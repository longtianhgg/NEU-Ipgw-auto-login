use serde::{Deserialize, Serialize};

pub const DEFAULT_KEY: &str = concat!(
    "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAnjA28DLKXZzxbKmo9/1WkVLf",
    "1mr+wtLXLXt6sC4WiBCtsbzF5ewm7ARZeAdS3iZtqlYPn6IcUoOw42H8nAK/tfFcIb6d",
    "Z1K0atn0U39oWCGPzYuKtLJeMuNZiDXVuAXtojrckOjLW9B3gUnaNGLuIx0fYe66l0o9",
    "WjU2cGLNZQfiIxs2h00z1EA9IdSnVxiVQWSD+lsP3JZXh2TT287la4Y4603SQNKTK/Qv",
    "XfcmccwTEd1IW6HwGxD6QrkInBiHisKWxmveN7UDSaQRZ/J97G0YC32pD38WT53izXeK",
    "0p/kU/X37VP555um1wVWFvPIuc9I7gMP1+hq5a+X6c++tQIDAQAB"
);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub auto_refresh: bool,
    pub refresh_interval_seconds: u64,
    pub timeout_seconds: u64,
    pub auth_mode: String,
    pub cas_login_url: String,
    pub ipgw_base_url: String,
    pub ac_id: u32,
    pub rsa_public_key: String,
    pub theme: String,
    pub opacity: u8,
    pub blur: u8,
    pub background: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_refresh: true,
            refresh_interval_seconds: 15,
            timeout_seconds: 12,
            auth_mode: "auto".into(),
            cas_login_url: "https://pass.neu.edu.cn/tpass/login".into(),
            ipgw_base_url: "https://ipgw.neu.edu.cn".into(),
            ac_id: 1,
            rsa_public_key: DEFAULT_KEY.into(),
            theme: "system".into(),
            opacity: 65,
            blur: 18,
            background: "default".into(),
        }
    }
}

impl Settings {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            (5..=3600).contains(&self.refresh_interval_seconds),
            "检测间隔应为 5–3600 秒"
        );
        anyhow::ensure!(
            (2..=60).contains(&self.timeout_seconds),
            "请求超时应为 2–60 秒"
        );
        anyhow::ensure!(
            matches!(self.auth_mode.as_str(), "auto" | "custom"),
            "认证模式无效"
        );
        anyhow::ensure!(
            matches!(self.theme.as_str(), "light" | "dark" | "system"),
            "主题无效"
        );
        anyhow::ensure!(
            (15..=95).contains(&self.opacity) && self.blur <= 40,
            "外观参数超出范围"
        );
        anyhow::ensure!((1..=999).contains(&self.ac_id), "ac_id 应为 1–999");
        for address in [&self.cas_login_url, &self.ipgw_base_url] {
            let u = url::Url::parse(address).map_err(|_| anyhow::anyhow!("认证地址无效"))?;
            anyhow::ensure!(
                u.scheme() == "https"
                    && u.host_str().is_some()
                    && u.username().is_empty()
                    && u.password().is_none()
                    && u.fragment().is_none(),
                "认证地址必须是无用户信息的 HTTPS 地址"
            );
        }
        let base = url::Url::parse(&self.ipgw_base_url)?;
        anyhow::ensure!(
            base.path() == "/" && base.query().is_none(),
            "校园网地址只能包含协议、域名和端口"
        );
        crate::auth::parse_key(&self.rsa_public_key)?;
        Ok(())
    }

    pub fn effective(&self) -> Self {
        if self.auth_mode == "custom" {
            return self.clone();
        }
        let defaults = Self::default();
        Self {
            cas_login_url: defaults.cas_login_url,
            ipgw_base_url: defaults.ipgw_base_url,
            ac_id: defaults.ac_id,
            rsa_public_key: defaults.rsa_public_key,
            ..self.clone()
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Connection {
    Online,
    Offline,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    pub state: Connection,
    pub username: Option<String>,
    pub ip: Option<String>,
    pub online_since: Option<i64>,
    pub duration_source: Option<String>,
    pub checked_at: Option<i64>,
    pub message: String,
    pub busy: Option<String>,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            state: Connection::Unknown,
            username: None,
            ip: None,
            online_since: None,
            duration_source: None,
            checked_at: None,
            message: "尚未检测校园网状态".into(),
            busy: None,
        }
    }
}

#[derive(Serialize)]
pub struct AccountView {
    pub username: String,
    pub has_password: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountInput {
    pub username: String,
    pub password: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct LogEntry {
    pub id: i64,
    pub time: i64,
    pub action: String,
    pub success: bool,
    pub message: String,
}

#[derive(Serialize)]
pub struct LogPage {
    pub items: Vec<LogEntry>,
    pub total: i64,
    pub page: u32,
    pub page_size: u32,
}

#[derive(Clone, Serialize)]
pub struct Background {
    pub id: String,
    pub name: String,
    pub url: String,
    pub source: String,
}
