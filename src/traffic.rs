//! Official account totals only. Live all_bytes is deliberately never consumed.
use crate::{
    model::{Connection, Status},
    store::Store,
};
use anyhow::Result;
use chrono::{DateTime, FixedOffset};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub fn month(now: i64) -> String {
    DateTime::from_timestamp(now, 0)
        .expect("valid clock")
        .with_timezone(&FixedOffset::east_opt(8 * 3600).unwrap())
        .format("%Y-%m")
        .to_string()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Monthly {
    pub account: String,
    pub month: String,
    pub bytes: i64,
    pub updated_at: i64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub account: String,
    pub month: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub gateway_start: Option<i64>,
    pub baseline_bytes: Option<i64>,
    pub bytes: Option<i64>,
    /// pending, settled, unavailable
    pub state: String,
    pub reason: Option<String>,
}
impl Session {
    fn invalidate(&mut self, reason: &str) {
        self.state = "unavailable".into();
        self.bytes = None;
        self.reason = Some(reason.into());
    }
    fn settle(&mut self, total: i64, current_month: &str) {
        if self.state != "pending" {
            return;
        }
        if self.month != current_month {
            self.invalidate("连接跨月或月累计已重置");
        } else if let Some(start) = self.baseline_bytes {
            match total.checked_sub(start).filter(|n| *n >= 0) {
                Some(bytes) => {
                    self.bytes = Some(bytes);
                    self.state = "settled".into();
                }
                None => self.invalidate("官方月累计异常回退"),
            }
        } else {
            self.invalidate("缺少建立连接时的官方月累计");
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Traffic {
    pub account: Option<String>,
    pub month: String,
    pub monthly: Option<Monthly>,
    pub cached: bool,
    pub current: Option<Session>,
    pub last: Option<Session>,
    pub error: Option<String>,
}

/// Stored atomically in additive SQLite tables. Records retain their own
/// account, month and boundaries; saving credentials never reattributes them.
#[derive(Default, Serialize, Deserialize)]
struct Ledger {
    account: Option<String>,
    cache: Vec<Monthly>,
    sessions: Vec<Session>,
}
impl Ledger {
    fn observe(&mut self, s: &Status, login: bool, now: i64) {
        let this_month = month(now);
        // A timeout is not a disconnect, including across a process restart.
        if s.state == Connection::Unknown {
            return;
        }
        for session in &mut self.sessions {
            if session.state == "pending" && session.month != this_month {
                session.invalidate("连接跨月或月累计已重置");
            }
        }
        let account = s.username.as_ref().filter(|a| !a.is_empty());
        let total = s.gateway_bytes.filter(|n| *n >= 0);
        let highwater = account.and_then(|a| {
            self.cache
                .iter()
                .filter(|m| m.account == *a && m.month == this_month)
                .map(|m| m.bytes)
                .chain(
                    self.sessions
                        .iter()
                        .filter(|c| c.account == *a && c.month == this_month)
                        .filter_map(|c| {
                            c.baseline_bytes
                                .and_then(|b| b.checked_add(c.bytes.unwrap_or(0)))
                        }),
                )
                .max()
        });
        let regressed = matches!((total, highwater), (Some(bytes), Some(old)) if bytes < old);
        if let (Some(account), Some(bytes)) = (account, total) {
            if regressed {
                for session in self
                    .sessions
                    .iter_mut()
                    .filter(|c| c.account == *account && c.state == "pending")
                {
                    session.invalidate("官方月累计异常回退");
                }
            }
            self.cache
                .retain(|m| m.account != *account || m.month != this_month);
            self.cache.push(Monthly {
                account: account.clone(),
                month: this_month.clone(),
                bytes,
                updated_at: now,
            });
        }
        let active = self.sessions.iter().position(|c| c.ended_at.is_none());
        if s.state == Connection::Offline {
            if let Some(i) = active {
                self.sessions[i].ended_at = Some(now);
            }
        } else if let Some(account) = account {
            self.account = Some(account.clone());
            let changed = active.is_some_and(|i| {
                let c = &self.sessions[i];
                c.account != *account
                    || matches!((c.gateway_start, s.gateway_start), (Some(a),Some(b)) if a != b)
            });
            if changed {
                let old = &mut self.sessions[active.unwrap()];
                old.ended_at = Some(now);
                old.invalidate("检测到外部重连或账号切换，连接边界不完整");
            }
            if active.is_none() || changed {
                let reused_identity = s.gateway_start.is_some_and(|start| {
                    self.sessions
                        .iter()
                        .rev()
                        .find(|c| c.account == *account)
                        .is_some_and(|c| c.gateway_start == Some(start))
                });
                if reused_identity {
                    for previous in self
                        .sessions
                        .iter_mut()
                        .filter(|c| c.account == *account && c.state == "pending")
                    {
                        previous.invalidate("登录后网关连接标识未变化，无法确认新连接边界");
                    }
                }
                if total.is_none() {
                    for previous in self.sessions.iter_mut().filter(|c| {
                        c.account == *account && c.ended_at.is_some() && c.state == "pending"
                    }) {
                        previous.invalidate("下一连接开始时未取得累计值，结算边界不完整");
                    }
                }
                let mut session = Session {
                    account: account.clone(),
                    month: this_month.clone(),
                    started_at: now,
                    ended_at: None,
                    gateway_start: s.gateway_start,
                    baseline_bytes: if login { total } else { None },
                    bytes: None,
                    state: "pending".into(),
                    reason: None,
                };
                if !login {
                    session.invalidate("接管已有或外部建立的连接，缺少登录时基线");
                } else if total.is_none() {
                    session.invalidate("登录时未取得有效的官方月累计");
                }
                if s.gateway_start.is_none() {
                    session.invalidate("缺少网关连接开始标识，无法确认连接边界");
                }
                if reused_identity {
                    session.invalidate("登录后网关连接标识未变化，无法确认新连接边界");
                }
                if regressed {
                    session.invalidate("登录时官方月累计异常回退，无法建立可靠基线");
                }
                // A login whose gateway start belongs to last month cannot be
                // treated as a fresh connection even if the CAS accepted it.
                if s.gateway_start.is_some_and(|t| month(t) != this_month) {
                    session.invalidate("连接起点跨月，无法建立本月基线");
                }
                self.sessions.push(session);
            } else if let Some(i) = active {
                // Never replace an established baseline on a repeated login.
                // If the identity was initially absent, adopting a later one
                // cannot retroactively prove that no reconnect happened.
                if self.sessions[i].gateway_start.is_none() && s.gateway_start.is_some() {
                    self.sessions[i].invalidate("连接开始标识曾缺失，无法确认连接边界");
                    self.sessions[i].gateway_start = s.gateway_start;
                }
            }
        }
        // A total without a gateway account cannot be assigned to cached or
        // saved credentials. Offline APIs commonly provide neither.
        if let (Some(account), Some(bytes)) = (account, total) {
            // Audit stored baselines too: a counter that moved backwards must
            // never become a plausible zero-byte starting point after restart.
            let mut prior = std::collections::BTreeMap::<(&str, &str), i64>::new();
            for c in &mut self.sessions {
                let key = (c.account.as_str(), c.month.as_str());
                if let Some(baseline) = c.baseline_bytes {
                    if c.state == "pending" && prior.get(&key).is_some_and(|old| baseline < *old) {
                        // Assign fields directly to keep account/month borrows.
                        c.state = "unavailable".into();
                        c.bytes = None;
                        c.reason = Some("连接基线低于同月先前记录，累计曾异常回退".into());
                    }
                    prior
                        .entry(key)
                        .and_modify(|old| *old = (*old).max(baseline))
                        .or_insert(baseline);
                }
            }
            for c in self
                .sessions
                .iter_mut()
                .filter(|c| c.account == *account && c.ended_at.is_some())
            {
                c.settle(bytes, &this_month);
            }
        }
    }
    fn view(&self, now: i64, online: bool) -> Traffic {
        let month = month(now);
        let account = self.account.clone();
        let monthly = self
            .cache
            .iter()
            .find(|m| Some(&m.account) == account.as_ref() && m.month == month)
            .cloned();
        let for_account = |s: &&Session| Some(&s.account) == account.as_ref();
        let mut current = self
            .sessions
            .iter()
            .filter(for_account)
            .find(|s| s.ended_at.is_none())
            .cloned();
        let mut last = self
            .sessions
            .iter()
            .rev()
            .filter(for_account)
            .find(|s| s.ended_at.is_some())
            .cloned();
        for c in [&mut current, &mut last].into_iter().flatten() {
            if c.state == "pending" && c.month != month {
                c.invalidate("连接跨月或月累计已重置");
            }
        }
        Traffic {
            account,
            month,
            cached: !online || monthly.as_ref().is_some_and(|m| m.updated_at != now),
            monthly,
            current,
            last,
            error: None,
        }
    }
}
impl Store {
    pub fn traffic(&self, observation: Option<(&Status, bool)>, now: i64) -> Result<Traffic> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        let account = tx
            .query_row("SELECT account FROM traffic_meta WHERE id=1", [], |r| {
                r.get(0)
            })
            .optional()?
            .flatten();
        let cache = {
            let mut q = tx.prepare("SELECT account,month,bytes,updated_at FROM traffic_cache")?;
            q.query_map([], |r| {
                Ok(Monthly {
                    account: r.get(0)?,
                    month: r.get(1)?,
                    bytes: r.get(2)?,
                    updated_at: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let sessions = {
            let mut q = tx.prepare("SELECT account,month,started_at,ended_at,gateway_start,baseline_bytes,bytes,state,reason FROM traffic_connections ORDER BY id")?;
            q.query_map([], |r| {
                Ok(Session {
                    account: r.get(0)?,
                    month: r.get(1)?,
                    started_at: r.get(2)?,
                    ended_at: r.get(3)?,
                    gateway_start: r.get(4)?,
                    baseline_bytes: r.get(5)?,
                    bytes: r.get(6)?,
                    state: r.get(7)?,
                    reason: r.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut ledger = Ledger {
            account,
            cache,
            sessions,
        };
        if let Some((s, login)) = observation {
            let previous = ledger.sessions.clone();
            let old_cache = ledger.cache.clone();
            ledger.observe(s, login, now);
            tx.execute("INSERT INTO traffic_meta VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET account=excluded.account", [&ledger.account])?;
            for m in &ledger.cache {
                if !old_cache.contains(m) {
                    tx.execute("INSERT INTO traffic_cache VALUES(?1,?2,?3,?4) ON CONFLICT(account,month) DO UPDATE SET bytes=excluded.bytes,updated_at=excluded.updated_at", params![m.account,m.month,m.bytes,m.updated_at])?;
                }
            }
            for (index, c) in ledger.sessions.iter().enumerate() {
                if previous.get(index) == Some(c) {
                    continue;
                }
                let id = i64::try_from(index)?
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("连接记录数量超出范围"))?;
                tx.execute("INSERT INTO traffic_connections VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(id) DO UPDATE SET ended_at=excluded.ended_at,gateway_start=excluded.gateway_start,bytes=excluded.bytes,state=excluded.state,reason=excluded.reason", params![id,c.account,c.month,c.started_at,c.ended_at,c.gateway_start,c.baseline_bytes,c.bytes,c.state,c.reason])?;
            }
        }
        let result = ledger.view(
            now,
            observation
                .is_some_and(|(s, _)| s.state == Connection::Online && s.gateway_bytes.is_some()),
        );
        tx.commit()?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const NOW: i64 = 1792485000;
    fn online(account: &str, total: Option<i64>, start: i64) -> Status {
        Status {
            state: Connection::Online,
            username: Some(account.into()),
            gateway_bytes: total,
            gateway_start: Some(start),
            checked_at: Some(NOW),
            ..Default::default()
        }
    }
    fn observe(store: &Store, s: &Status, login: bool, time: i64) -> Traffic {
        store.traffic(Some((s, login)), time).unwrap()
    }
    #[test]
    fn delayed_settlement_survives_restart_and_repeated_login() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let start = 7_361_325_863;
        let s = online("a", Some(start), NOW);
        let first = observe(&store, &s, true, NOW);
        assert_eq!(first.current.unwrap().baseline_bytes, Some(start));
        let repeated = observe(&store, &online("a", Some(start + 100), NOW), true, NOW + 1);
        assert_eq!(repeated.current.unwrap().baseline_bytes, Some(start));
        // Unknown detection and failed logout leave the active baseline intact.
        observe(&store, &Status::default(), false, NOW + 2);
        observe(&store, &online("a", Some(start + 100), NOW), false, NOW + 3);
        observe(
            &store,
            &Status {
                state: Connection::Offline,
                ..Default::default()
            },
            false,
            NOW + 4,
        );
        drop(store);
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(
            store.traffic(None, NOW + 5).unwrap().last.unwrap().state,
            "pending"
        );
        let t = observe(
            &store,
            &online("a", Some(start + 5_000_000_000), NOW + 6),
            true,
            NOW + 6,
        );
        assert_eq!(t.last.unwrap().bytes, Some(5_000_000_000));
        assert_eq!(
            t.current.unwrap().baseline_bytes,
            Some(start + 5_000_000_000)
        );
        let t = observe(&store, &Status::default(), false, NOW + 7);
        assert_eq!(t.current.unwrap().state, "pending");
        assert!(t.cached);
    }
    #[test]
    fn adoption_reconnect_accounts_and_missing_baselines_are_not_estimated() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let t = observe(&store, &online("a", Some(100), NOW), false, NOW);
        assert_eq!(t.current.unwrap().state, "unavailable");
        observe(
            &store,
            &Status {
                state: Connection::Offline,
                ..Default::default()
            },
            false,
            NOW + 1,
        );
        observe(&store, &online("a", Some(100), NOW + 2), true, NOW + 2);
        let t = observe(&store, &online("a", Some(200), NOW + 3), false, NOW + 3);
        assert_eq!(t.last.unwrap().state, "unavailable");
        assert_eq!(t.current.unwrap().state, "unavailable");
        let t = observe(&store, &online("b", Some(0), NOW + 4), false, NOW + 4);
        assert_eq!(t.monthly.unwrap().bytes, 0);
        assert_eq!(t.account.as_deref(), Some("b"));
        observe(
            &store,
            &Status {
                state: Connection::Offline,
                ..Default::default()
            },
            false,
            NOW + 5,
        );
        let t = observe(&store, &online("b", None, NOW + 6), true, NOW + 6);
        assert!(t.current.unwrap().baseline_bytes.is_none());
    }
    #[test]
    fn rollback_zero_and_offline_official_total() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        observe(&store, &online("a", Some(0), NOW), true, NOW);
        let mut offline = online("a", Some(0), NOW);
        offline.state = Connection::Offline;
        let t = observe(&store, &offline, false, NOW + 1);
        assert_eq!(t.last.unwrap().bytes, Some(0));
        observe(&store, &online("a", Some(900), NOW + 2), true, NOW + 2);
        let t = observe(&store, &online("a", Some(1), NOW + 2), false, NOW + 3);
        let c = t.current.unwrap();
        assert_eq!(c.state, "unavailable");
        assert!(c.bytes.is_none());
        assert!(c.reason.unwrap().contains("回退"));
    }
    #[test]
    fn rollback_during_new_login_cannot_create_a_zero_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        observe(&store, &online("a", Some(900), NOW), true, NOW);
        observe(
            &store,
            &Status {
                state: Connection::Offline,
                ..Default::default()
            },
            false,
            NOW + 1,
        );
        let t = observe(&store, &online("a", Some(0), NOW + 2), true, NOW + 2);
        assert_eq!(t.current.unwrap().state, "unavailable");
        assert_eq!(t.last.unwrap().state, "unavailable");
        let t = observe(&store, &online("a", Some(999), NOW + 2), false, NOW + 3);
        assert_eq!(t.current.unwrap().state, "unavailable");
    }

    #[test]
    fn beijing_month_boundary_hides_old_cache_and_invalidates_pending() {
        let end = chrono::DateTime::parse_from_rfc3339("2026-10-31T23:59:59+08:00")
            .unwrap()
            .timestamp();
        assert_eq!(month(end), "2026-10");
        assert_eq!(month(end + 1), "2026-11");
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        observe(&store, &online("a", Some(9999), end), true, end);
        let next = store.traffic(None, end + 1).unwrap();
        assert!(next.monthly.is_none());
        assert_eq!(next.current.unwrap().state, "unavailable");
        let t = observe(&store, &online("a", Some(0), end), false, end + 1);
        assert_eq!(t.monthly.unwrap().bytes, 0);
        assert_eq!(t.current.unwrap().state, "unavailable");
    }
    #[test]
    fn missing_total_at_next_connection_prevents_multi_connection_settlement() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        observe(&store, &online("a", Some(10), NOW), true, NOW);
        observe(
            &store,
            &Status {
                state: Connection::Offline,
                ..Default::default()
            },
            false,
            NOW + 1,
        );
        let t = observe(&store, &online("a", None, NOW + 2), true, NOW + 2);
        assert_eq!(t.last.unwrap().state, "unavailable");
    }
}
