use crate::{
    model::{Connection, Settings, Status},
    store::Credentials,
};
use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{Client, Response, cookie::Jar, redirect::Policy};
use rsa::{Pkcs1v15Encrypt, RsaPublicKey, pkcs8::DecodePublicKey, rand_core::OsRng};
use scraper::{Html, Selector};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use url::Url;
use zeroize::Zeroize;

pub fn parse_key(text: &str) -> Result<RsaPublicKey> {
    let key = if text.contains("-----BEGIN PUBLIC KEY-----") {
        RsaPublicKey::from_public_key_pem(text)
    } else {
        let der = STANDARD
            .decode(text.split_whitespace().collect::<String>())
            .map_err(|_| anyhow::anyhow!("RSA 公钥不是有效的 Base64"))?;
        RsaPublicKey::from_public_key_der(&der)
    }
    .map_err(|_| anyhow::anyhow!("RSA 公钥格式无效，应为 SubjectPublicKeyInfo 公钥"))?;
    use rsa::traits::PublicKeyParts;
    anyhow::ensure!(
        (2048..=8192).contains(&key.n().bits()),
        "RSA 公钥应为 2048–8192 位"
    );
    Ok(key)
}

pub fn encrypt_credentials(username: &str, password: &str, key: &str) -> Result<String> {
    let key = parse_key(key)?;
    let mut plain = [username.as_bytes(), password.as_bytes()].concat();
    let encrypted = key.encrypt(&mut OsRng, Pkcs1v15Encrypt, &plain);
    plain.zeroize();
    Ok(STANDARD.encode(encrypted.map_err(|_| anyhow::anyhow!("账号密码长度超出 RSA 公钥容量"))?))
}

pub fn login_fields(text: &str) -> (String, String, String) {
    let document = Html::parse_document(text);
    let inputs = Selector::parse("input").unwrap();
    let mut lt = String::new();
    let mut execution = String::new();
    for input in document.select(&inputs) {
        let name = input
            .value()
            .attr("name")
            .or_else(|| input.value().attr("id"));
        match name {
            Some("lt") => lt = input.value().attr("value").unwrap_or("").into(),
            Some("execution") => execution = input.value().attr("value").unwrap_or("").into(),
            _ => {}
        }
    }
    let error = document
        .select(&Selector::parse("#errormsghide").unwrap())
        .flat_map(|el| el.text())
        .collect::<String>()
        .trim()
        .to_string();
    (lt, execution, error)
}

pub fn parse_status(text: &str, now: i64) -> Result<Status> {
    let text = text.trim().trim_end_matches(';').trim();
    let json = if text.starts_with('{') {
        text
    } else {
        let (callback, rest) = text.split_once('(').context("状态接口不是 JSON/JSONP")?;
        anyhow::ensure!(
            !callback.is_empty()
                && callback
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '$'),
            "JSONP 回调格式无效"
        );
        rest.strip_suffix(')').context("JSONP 格式无效")?
    };
    let p: Value = serde_json::from_str(json).context("状态接口返回无效 JSON")?;
    anyhow::ensure!(p.is_object(), "状态接口返回格式无效");
    let error = p.get("error").and_then(Value::as_str).unwrap_or("");
    let state = match error {
        "ok" => Connection::Online,
        "not_online_error" | "not_online" | "user_not_online" => Connection::Offline,
        _ => anyhow::bail!("状态接口返回未知状态，无法确认是否在线"),
    };
    let number = |name: &str| {
        p.get(name).and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
    };
    let upstream_since = number("online_time").filter(|n| *n >= 946684800 && *n <= now);
    let since = upstream_since.or_else(|| {
        number("sum_seconds")
            .filter(|n| *n >= 0 && *n < 10 * 365 * 86400)
            .map(|s| now - s)
    });
    let string = |names: &[&str]| {
        names.iter().find_map(|n| {
            p.get(*n)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
    };
    Ok(Status {
        state: state.clone(),
        username: if state == Connection::Online {
            string(&["user_name", "username"])
        } else {
            None
        },
        ip: string(&["online_ip", "client_ip", "ip"]),
        online_since: if state == Connection::Online {
            since
        } else {
            None
        },
        duration_source: if state == Connection::Online && since.is_some() {
            Some("gateway".into())
        } else {
            None
        },
        checked_at: Some(now),
        message: if state == Connection::Online {
            "校园网已认证"
        } else {
            "当前尚未建立校园网认证"
        }
        .into(),
        busy: None,
    })
}

fn network_error(e: reqwest::Error) -> anyhow::Error {
    // Do not expose reqwest's Display: it can contain a service ticket in its URL.
    anyhow::anyhow!(if e.is_timeout() {
        "认证服务器请求超时"
    } else if e.is_connect() {
        "无法连接认证服务器，请检查校园网连接、DNS 或 TLS 证书"
    } else {
        "认证服务器请求失败"
    })
}

pub struct AuthClient {
    client: Client,
    settings: Settings,
}

impl AuthClient {
    pub fn new(settings: &Settings) -> Result<Self> {
        let settings = settings.effective();
        let client = Client::builder()
            .cookie_provider(Arc::new(Jar::default()))
            .redirect(Policy::none())
            .no_proxy()
            .timeout(Duration::from_secs(settings.timeout_seconds))
            .user_agent("NEU-Campus-Web/2.0")
            .build()
            .context("无法初始化认证客户端")?;
        Ok(Self { client, settings })
    }

    fn ipgw(&self, path: &str) -> Result<Url> {
        Url::parse(&self.settings.ipgw_base_url)?
            .join(path)
            .context("校园网地址无效")
    }

    fn allowed(&self, url: &Url) -> bool {
        let cas = Url::parse(&self.settings.cas_login_url).ok();
        let ipgw = Url::parse(&self.settings.ipgw_base_url).ok();
        if !url.username().is_empty() || url.password().is_some() {
            return false;
        }
        [cas, ipgw].into_iter().flatten().any(|base| {
            url.host_str() == base.host_str()
                && (url.scheme() == base.scheme()
                    && url.port_or_known_default() == base.port_or_known_default()
                    || url.scheme() == "http"
                        && base.host_str() == ipgw_host(&self.settings).as_deref()
                        && url.port_or_known_default() == Some(80))
        })
    }

    fn ticket_url(&self, url: &Url) -> bool {
        url.host_str() == ipgw_host(&self.settings).as_deref()
            && url.path().ends_with("/srun_portal_sso")
            && url
                .query_pairs()
                .any(|(k, v)| k == "ticket" && !v.is_empty())
    }

    async fn get_follow(
        &self,
        mut url: Url,
        stop_at_ticket: bool,
    ) -> Result<(Option<Response>, Url)> {
        for _ in 0..=10 {
            anyhow::ensure!(self.allowed(&url), "认证重定向指向未配置的服务器");
            if stop_at_ticket && self.ticket_url(&url) {
                return Ok((None, url));
            }
            let response = self
                .client
                .get(url.clone())
                .send()
                .await
                .map_err(network_error)?;
            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .context("认证跳转缺少地址")?
                    .to_str()
                    .context("认证跳转地址无效")?;
                url = url.join(location).context("认证跳转地址无效")?;
            } else {
                anyhow::ensure!(
                    response.status().is_success(),
                    "认证服务器返回 HTTP {}",
                    response.status().as_u16()
                );
                return Ok((Some(response), url));
            }
        }
        anyhow::bail!("认证重定向次数过多")
    }

    pub async fn status(&self) -> Result<Status> {
        let mut url = self.ipgw("/cgi-bin/rad_user_info")?;
        url.query_pairs_mut().append_pair("callback", "json");
        let r = self
            .client
            .get(url)
            .header("Referer", self.ipgw("/srun_portal_success")?.as_str())
            .send()
            .await
            .map_err(network_error)?;
        anyhow::ensure!(
            r.status().is_success(),
            "状态接口返回 HTTP {}",
            r.status().as_u16()
        );
        let text = r.text().await.map_err(network_error)?;
        // Upstream body is never included in errors or logs.
        parse_status(&text, chrono::Utc::now().timestamp())
    }

    async fn service(&self) -> Result<Url> {
        let mut portal = self.ipgw("/srun_portal_pc")?;
        portal
            .query_pairs_mut()
            .append_pair("ac_id", &self.settings.ac_id.to_string());
        let (_, final_url) = self.get_follow(portal, false).await?;
        let mut service = self.ipgw("/srun_portal_sso")?;
        // The production gateway currently uses an HTTP service URL, even with HTTPS CAS.
        if service.host_str() == Some("ipgw.neu.edu.cn") {
            service.set_scheme("http").ok();
        }
        let fallback = format!("ac_id={}", self.settings.ac_id);
        service.set_query(Some(
            final_url
                .query()
                .filter(|q| !q.is_empty())
                .unwrap_or(&fallback),
        ));
        Ok(service)
    }

    pub async fn login(&self, credentials: &Credentials) -> Result<()> {
        let service = self.service().await?;
        let mut cas = Url::parse(&self.settings.cas_login_url)?;
        cas.query_pairs_mut()
            .append_pair("service", service.as_str());
        let (page, page_url) = self.get_follow(cas.clone(), false).await?;
        let text = page
            .context("未取得认证页面")?
            .text()
            .await
            .map_err(network_error)?;
        let (lt, execution, _) = login_fields(&text);
        anyhow::ensure!(
            !lt.is_empty() && !execution.is_empty(),
            "统一身份认证页面缺少 lt/execution，页面结构可能已变化"
        );
        let rsa = encrypt_credentials(
            &credentials.username,
            &credentials.password,
            &self.settings.rsa_public_key,
        )?;
        let form = [
            ("rsa", rsa),
            ("ul", credentials.username.len().to_string()),
            ("pl", credentials.password.len().to_string()),
            ("lt", lt),
            ("execution", execution),
            ("_eventId", "submit".into()),
            ("t_un", String::new()),
            ("t_pd", String::new()),
            ("t_c", String::new()),
        ];
        let r = self
            .client
            .post(cas)
            .header("Referer", page_url.as_str())
            .form(&form)
            .send()
            .await
            .map_err(network_error)?;
        let ticket = if r.status().is_redirection() {
            let location = r
                .headers()
                .get(reqwest::header::LOCATION)
                .context("登录响应缺少跳转地址")?
                .to_str()
                .context("登录跳转地址无效")?;
            let target = r.url().join(location).context("登录跳转地址无效")?;
            anyhow::ensure!(self.allowed(&target), "登录响应跳转到未配置的服务器");
            if self.ticket_url(&target) {
                target
            } else {
                let (response, url) = self.get_follow(target, true).await?;
                anyhow::ensure!(
                    response.is_none() && self.ticket_url(&url),
                    "未从认证跳转中取得 SSO ticket"
                );
                url
            }
        } else {
            anyhow::ensure!(
                r.status().is_success(),
                "统一身份认证返回 HTTP {}",
                r.status().as_u16()
            );
            let text = r.text().await.map_err(network_error)?;
            let (_, _, error) = login_fields(&text);
            // Map server text to a fixed explanation; never echo credentials or tokens.
            let message = if error.contains("验证码") {
                "认证要求验证码，当前自动登录流程无法完成"
            } else if error.contains("锁") {
                "认证账号被锁定，请到统一身份认证页面处理"
            } else if !error.is_empty() {
                "统一身份认证失败，请检查账号密码"
            } else {
                "统一身份认证未返回预期跳转，页面结构可能已变化"
            };
            anyhow::bail!(message);
        };
        let mut api = self.ipgw("/v1/srun_portal_sso")?;
        api.set_query(ticket.query());
        let response = self
            .client
            .get(api)
            .header("Referer", self.ipgw("/srun_portal_pc")?.as_str())
            .send()
            .await
            .map_err(network_error)?;
        anyhow::ensure!(
            response.status().is_success(),
            "校园网认证返回 HTTP {}",
            response.status().as_u16()
        );
        let text = response.text().await.map_err(network_error)?;
        let success = match serde_json::from_str::<Value>(&text) {
            Ok(p) => {
                matches!(
                    p.get("message").and_then(Value::as_str),
                    Some("success" | "ip_already_online_error")
                ) || p.get("error").and_then(Value::as_str) == Some("ok")
            }
            Err(_) => text.trim() == "success",
        };
        anyhow::ensure!(
            success,
            "校园网未确认接受认证，请检查账号权限或认证接口配置"
        );
        Ok(())
    }

    pub async fn logout(&self) -> Result<()> {
        let mut url = self.ipgw("/cgi-bin/srun_portal")?;
        url.query_pairs_mut()
            .append_pair("action", "logout")
            .append_pair("username", "");
        let r = self
            .client
            .get(url)
            .header("Referer", self.ipgw("/srun_portal_success")?.as_str())
            .send()
            .await
            .map_err(network_error)?;
        anyhow::ensure!(
            r.status().is_success(),
            "注销接口返回 HTTP {}",
            r.status().as_u16()
        );
        Ok(())
    }
}

fn ipgw_host(settings: &Settings) -> Option<String> {
    Url::parse(&settings.ipgw_base_url)
        .ok()?
        .host_str()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::{
        RsaPrivateKey,
        pkcs8::{EncodePublicKey, LineEnding},
    };
    #[test]
    fn rsa_encrypts_utf8_and_does_not_use_deterministic_padding() {
        let key = RsaPrivateKey::new(&mut OsRng, 2048).unwrap();
        let public = key
            .to_public_key()
            .to_public_key_pem(LineEnding::LF)
            .unwrap();
        let a = encrypt_credentials("学生", "密碼", &public).unwrap();
        let b = encrypt_credentials("学生", "密碼", &public).unwrap();
        assert_ne!(a, b);
        assert_eq!(
            key.decrypt(Pkcs1v15Encrypt, &STANDARD.decode(a).unwrap())
                .unwrap(),
            "学生密碼".as_bytes()
        );
        assert!(encrypt_credentials("test", &"x".repeat(300), &public).is_err());
        assert!(parse_key(crate::model::DEFAULT_KEY).is_ok());
    }
    #[test]
    fn status_formats_and_unknown_are_distinct() {
        let p = parse_status(
            "json({\"error\":\"ok\",\"user_name\":\"s\",\"online_time\":1700000000});",
            1800000000,
        )
        .unwrap();
        assert_eq!(p.state, Connection::Online);
        assert_eq!(p.online_since, Some(1700000000));
        assert_eq!(
            parse_status("{\"error\":\"not_online_error\"}", 1800000000)
                .unwrap()
                .state,
            Connection::Offline
        );
        assert!(parse_status("{\"error\":\"server_error\"}", 1800000000).is_err());
        assert!(parse_status("<html>error</html>", 1800000000).is_err());
        assert!(parse_status("json({\"error\":\"ok\"}", 1800000000).is_err());
    }
    #[test]
    fn parses_hidden_fields_and_nested_errors() {
        let (lt, e, error) = login_fields(
            "<input name='lt' value='ticket&amp;x'><input id='execution' value='e1s1'><span id='errormsghide'><b>密码</b>错误</span>",
        );
        assert_eq!(lt, "ticket&x");
        assert_eq!(e, "e1s1");
        assert_eq!(error, "密码错误");
    }
}
