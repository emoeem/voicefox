//! QQ 音乐登录态。
//!
//! 登录过程中间需要暂存预热 cookie（`pending`），登录成功后把 OAuth 链路上
//! 收集到的全部 cookie 写进通用会话存储。

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use lx_core::model::source::SourceId;

use crate::session::{SessionStore, SourceSession};

/// 登录凭据：QQ 音乐以 `uin` + `qqmusic_key` 判定已登录。
const LOGIN_COOKIES: [&str; 2] = ["uin", "qqmusic_key"];

static STORE: OnceLock<SessionStore> = OnceLock::new();
static PENDING: OnceLock<Mutex<BTreeMap<String, String>>> = OnceLock::new();

pub(super) fn store() -> &'static SessionStore {
    STORE.get_or_init(|| SessionStore::load(SourceId::Tx))
}

fn pending_store() -> &'static Mutex<BTreeMap<String, String>> {
    PENDING.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub(super) fn snapshot() -> SourceSession {
    store().snapshot()
}

pub(super) fn cookie_header() -> Option<String> {
    let session = snapshot();
    if session.cookies.is_empty() {
        return None;
    }
    let mut pairs = session
        .cookies
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>();
    pairs.sort();
    Some(pairs.join("; "))
}

/// 已登录：两个关键 cookie 都在才算。
pub(super) fn is_logged_in() -> bool {
    let session = snapshot();
    LOGIN_COOKIES.iter().all(|name| session.has_cookie(name))
}

/// 暂存登录过程中的预热 cookie。
pub(super) fn save_pending(cookies: &BTreeMap<String, String>) -> Result<(), String> {
    let mut guard = pending_store()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    *guard = cookies.clone();
    Ok(())
}

pub(super) fn pending() -> Option<BTreeMap<String, String>> {
    let guard = pending_store()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    (!guard.is_empty()).then(|| guard.clone())
}

pub(super) fn save_login(cookies: &BTreeMap<String, String>) -> Result<(), String> {
    let user_id = cookies.get("uin").cloned();
    store().update(|session| {
        for (name, value) in cookies {
            session.set_cookie(name, value);
        }
        if let Some(user_id) = user_id {
            session.user_id = Some(user_id);
        }
    })?;
    let mut guard = pending_store()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    guard.clear();
    Ok(())
}

pub(super) fn logout() -> Result<(), String> {
    if let Ok(mut guard) = pending_store().lock() {
        guard.clear();
    }
    store().clear()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_requires_both_credentials() {
        let mut session = SourceSession::default();
        assert!(!LOGIN_COOKIES.iter().all(|name| session.has_cookie(name)));
        session.set_cookie("uin", "o123");
        assert!(!LOGIN_COOKIES.iter().all(|name| session.has_cookie(name)));
        session.set_cookie("qqmusic_key", "key");
        assert!(LOGIN_COOKIES.iter().all(|name| session.has_cookie(name)));
    }

    #[test]
    fn pending_round_trips() {
        let mut cookies = BTreeMap::new();
        cookies.insert("pt_login_sig".to_string(), "sig".to_string());
        save_pending(&cookies).unwrap();
        assert_eq!(
            pending().and_then(|cookies| cookies.get("pt_login_sig").cloned()),
            Some("sig".to_string())
        );
        save_pending(&BTreeMap::new()).unwrap();
        assert!(pending().is_none());
    }
}
