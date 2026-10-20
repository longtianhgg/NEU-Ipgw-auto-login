use crate::model::{AccountView, LogEntry, LogPage, Settings};
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use anyhow::{Context, Result};
use rsa::rand_core::{OsRng, RngCore};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Mutex,
};
use zeroize::Zeroize;

pub struct Credentials {
    pub username: String,
    pub password: String,
}
impl Drop for Credentials {
    fn drop(&mut self) {
        self.password.zeroize();
    }
}

pub struct Store {
    pub(crate) db: Mutex<Connection>,
    cipher: Aes256Gcm,
    pub directory: PathBuf,
}

impl Store {
    pub fn open(directory: &Path) -> Result<Self> {
        fs::create_dir_all(directory).context("无法创建数据目录")?;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        let directory = directory.canonicalize()?;
        let key_path = directory.join("secret.key");
        let db_path = directory.join("neu.db");
        let mut key = if key_path.exists() {
            anyhow::ensure!(
                !key_path.symlink_metadata()?.file_type().is_symlink(),
                "密钥文件不能是符号链接"
            );
            fs::read(&key_path)?
        } else {
            anyhow::ensure!(
                !db_path.exists(),
                "数据库已存在但密钥丢失，请同时恢复数据库和密钥"
            );
            let mut bytes = vec![0u8; 32];
            OsRng.fill_bytes(&mut bytes);
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&key_path)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            bytes
        };
        anyhow::ensure!(key.len() == 32, "密钥文件损坏");
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))?;
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| anyhow::anyhow!("密钥无效"))?;
        key.zeroize();
        if db_path.exists() {
            anyhow::ensure!(
                !db_path.symlink_metadata()?.file_type().is_symlink(),
                "数据库不能是符号链接"
            );
        }
        let db = Connection::open(&db_path)?;
        fs::set_permissions(&db_path, fs::Permissions::from_mode(0o600))?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch("PRAGMA journal_mode=DELETE;
            CREATE TABLE IF NOT EXISTS traffic_meta (id INTEGER PRIMARY KEY CHECK(id=1), account TEXT);
            CREATE TABLE IF NOT EXISTS traffic_cache (account TEXT NOT NULL, month TEXT NOT NULL, bytes INTEGER NOT NULL CHECK(bytes>=0), updated_at INTEGER NOT NULL, PRIMARY KEY(account,month));
            CREATE TABLE IF NOT EXISTS traffic_connections (id INTEGER PRIMARY KEY, account TEXT NOT NULL, month TEXT NOT NULL, started_at INTEGER NOT NULL, ended_at INTEGER, gateway_start INTEGER, baseline_bytes INTEGER CHECK(baseline_bytes>=0), bytes INTEGER CHECK(bytes>=0), state TEXT NOT NULL, reason TEXT);
            CREATE TABLE IF NOT EXISTS account (id INTEGER PRIMARY KEY CHECK(id=1), username TEXT NOT NULL, password BLOB NOT NULL);
            CREATE TABLE IF NOT EXISTS settings (id INTEGER PRIMARY KEY CHECK(id=1), value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS logs (id INTEGER PRIMARY KEY AUTOINCREMENT, time INTEGER NOT NULL, action TEXT NOT NULL, success INTEGER NOT NULL, message TEXT NOT NULL);")?;
        fs::create_dir_all(directory.join("backgrounds"))?;
        fs::set_permissions(
            directory.join("backgrounds"),
            fs::Permissions::from_mode(0o700),
        )?;
        Ok(Self {
            db: Mutex::new(db),
            cipher,
            directory,
        })
    }

    fn decrypt(&self, username: &str, value: &[u8]) -> Result<String> {
        anyhow::ensure!(value.len() >= 28, "密码数据损坏");
        let bytes = self
            .cipher
            .decrypt(
                Nonce::from_slice(&value[..12]),
                Payload {
                    msg: &value[12..],
                    aad: username.as_bytes(),
                },
            )
            .map_err(|_| anyhow::anyhow!("无法解密账号，请检查密钥或重新保存账号"))?;
        String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("密码数据损坏"))
    }

    pub fn credentials(&self) -> Result<Option<Credentials>> {
        let db = self.db.lock().unwrap();
        let row: Option<(String, Vec<u8>)> = db
            .query_row(
                "SELECT username,password FROM account WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(username, encrypted)| {
            Ok(Credentials {
                password: self.decrypt(&username, &encrypted)?,
                username,
            })
        })
        .transpose()
    }

    pub fn account(&self) -> Result<AccountView> {
        let db = self.db.lock().unwrap();
        Ok(db
            .query_row("SELECT username FROM account WHERE id=1", [], |r| {
                Ok(AccountView {
                    username: r.get(0)?,
                    has_password: true,
                })
            })
            .optional()?
            .unwrap_or(AccountView {
                username: String::new(),
                has_password: false,
            }))
    }

    pub fn save_account(&self, username: &str, password: Option<&str>) -> Result<()> {
        let username = username.trim();
        anyhow::ensure!(
            !username.is_empty()
                && username.len() <= 128
                && !username.chars().any(char::is_control),
            "请输入有效账号（最多 128 字节）"
        );
        let mut db = self.db.lock().unwrap();
        let mut secret = match password.filter(|p| !p.is_empty()) {
            Some(p) => p.to_string(),
            None => {
                let row: Option<(String, Vec<u8>)> = db
                    .query_row(
                        "SELECT username,password FROM account WHERE id=1",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                let (old_user, bytes) = row.context("首次保存时必须填写密码")?;
                self.decrypt(&old_user, &bytes)?
            }
        };
        if secret.len() > 512 {
            secret.zeroize();
            anyhow::bail!("密码过长");
        }
        let mut nonce = [0u8; 12];
        OsRng.fill_bytes(&mut nonce);
        let encrypted = self.cipher.encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: secret.as_bytes(),
                aad: username.as_bytes(),
            },
        );
        secret.zeroize();
        let mut value = nonce.to_vec();
        value.extend(encrypted.map_err(|_| anyhow::anyhow!("密码加密失败"))?);
        let tx = db.transaction()?;
        tx.execute("INSERT INTO account VALUES(1,?1,?2) ON CONFLICT(id) DO UPDATE SET username=excluded.username,password=excluded.password", params![username, value])?;
        tx.commit()?;
        Ok(())
    }

    pub fn settings(&self) -> Result<Settings> {
        let db = self.db.lock().unwrap();
        let text: Option<String> = db
            .query_row("SELECT value FROM settings WHERE id=1", [], |r| r.get(0))
            .optional()?;
        match text {
            Some(s) => serde_json::from_str(&s).context("设置数据损坏"),
            None => Ok(Settings::default()),
        }
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<()> {
        let value = serde_json::to_string(settings)?;
        self.db.lock().unwrap().execute(
            "INSERT INTO settings VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET value=excluded.value",
            [value],
        )?;
        Ok(())
    }

    pub fn log(&self, action: &str, success: bool, message: &str) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        tx.execute(
            "INSERT INTO logs(time,action,success,message) VALUES(?1,?2,?3,?4)",
            params![chrono::Utc::now().timestamp(), action, success, message],
        )?;
        tx.execute(
            "DELETE FROM logs WHERE id NOT IN (SELECT id FROM logs ORDER BY id DESC LIMIT 1000)",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn logs(&self, page: u32) -> Result<LogPage> {
        let db = self.db.lock().unwrap();
        let page = page.max(1);
        let total = db.query_row("SELECT COUNT(*) FROM logs", [], |r| r.get(0))?;
        let mut q = db.prepare(
            "SELECT id,time,action,success,message FROM logs ORDER BY id DESC LIMIT 20 OFFSET ?1",
        )?;
        let items = q
            .query_map([i64::from(page - 1) * 20], |r| {
                Ok(LogEntry {
                    id: r.get(0)?,
                    time: r.get(1)?,
                    action: r.get(2)?,
                    success: r.get(3)?,
                    message: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(LogPage {
            items,
            total,
            page,
            page_size: 20,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encrypted_account_survives_restart_and_username_change() {
        let dir = tempfile::tempdir().unwrap();
        {
            let s = Store::open(dir.path()).unwrap();
            assert!(!s.account().unwrap().has_password);
            assert!(s.save_account("test", None).is_err());
            s.save_account("student", Some("secret-password-密码"))
                .unwrap();
            s.save_account("new-student", Some("")).unwrap();
            let c = s.credentials().unwrap().unwrap();
            assert_eq!(c.username, "new-student");
            assert_eq!(c.password, "secret-password-密码");
            let settings = Settings {
                theme: "dark".into(),
                ..Settings::default()
            };
            s.save_settings(&settings).unwrap();
            s.log("login", true, "登录成功").unwrap();
            assert!(
                !String::from_utf8_lossy(&fs::read(dir.path().join("neu.db")).unwrap())
                    .contains("secret-password")
            );
        }
        let s = Store::open(dir.path()).unwrap();
        assert_eq!(
            s.credentials().unwrap().unwrap().password,
            "secret-password-密码"
        );
        assert_eq!(s.settings().unwrap().theme, "dark");
        assert_eq!(s.logs(1).unwrap().total, 1);
        for file in ["neu.db", "secret.key"] {
            assert_eq!(
                fs::metadata(dir.path().join(file))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
    #[test]
    fn missing_key_is_not_silently_replaced() {
        let dir = tempfile::tempdir().unwrap();
        drop(Store::open(dir.path()).unwrap());
        fs::remove_file(dir.path().join("secret.key")).unwrap();
        assert!(Store::open(dir.path()).is_err());
    }

    #[test]
    fn history_is_paginated_newest_first_and_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        for n in 0..1025 {
            s.log("status", true, &format!("event-{n}")).unwrap();
        }
        let first = s.logs(1).unwrap();
        let second = s.logs(2).unwrap();
        let last = s.logs(50).unwrap();
        assert_eq!(first.total, 1000);
        assert_eq!(first.items.len(), 20);
        assert_eq!(first.items[0].message, "event-1024");
        assert_eq!(second.items[0].message, "event-1004");
        assert_eq!(last.items.last().unwrap().message, "event-25");
        assert!(s.logs(51).unwrap().items.is_empty());
        assert_eq!(s.logs(0).unwrap().page, 1);
    }
}
