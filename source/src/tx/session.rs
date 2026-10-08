//! QQ 音乐登录态。
//!
//! 登录过程中间需要暂存预热 cookie（`pending`），登录成功后把 OAuth 链路上
//! 收集到的全部 cookie 写进通用会话存储。

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use lx_core::model::source::SourceId;

use crate::session::{SessionStore, SourceSession};

/// 登录凭据分两半：身份 + 音乐凭证，两半各命中一个才算已登录。
///
/// QQ 扫码写 `uin` + `qqmusic_key`；微信扫码写 `wxuin`（值同 `uin`）+
/// `qm_keyst`（值同 `qqmusic_key`，微信账号的 musickey 前缀是 `W_X_`）。
/// 两组都接受，是因为历史登录数据里两种命名都可能存在。
const LOGIN_ID_COOKIES: [&str; 2] = ["uin", "wxuin"];
const LOGIN_KEY_COOKIES: [&str; 2] = ["qqmusic_key", "qm_keyst"];

static STORE: OnceLock<SessionStore> = OnceLock::new();
static PENDING: OnceLock<Mutex<BTreeMap<String, BTreeMap<String, String>>>> = OnceLock::new();

pub(super) fn store() -> &'static SessionStore {
    STORE.get_or_init(|| SessionStore::load(SourceId::Tx))
}

fn pending_store() -> &'static Mutex<BTreeMap<String, BTreeMap<String, String>>> {
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

/// 已登录：身份与音乐凭证各至少一个都在才算。
pub(super) fn is_logged_in() -> bool {
    logged_in(&snapshot())
}

/// 判定逻辑独立成函数，便于不碰全局存储的单测。
fn logged_in(session: &SourceSession) -> bool {
    LOGIN_ID_COOKIES.iter().any(|name| session.has_cookie(name))
        && LOGIN_KEY_COOKIES
            .iter()
            .any(|name| session.has_cookie(name))
}

/// 暂存登录过程中的预热 cookie。
pub(super) fn save_pending(key: &str, cookies: &BTreeMap<String, String>) -> Result<(), String> {
    let mut guard = pending_store()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    guard.insert(key.to_string(), cookies.clone());
    Ok(())
}

pub(super) fn pending(key: &str) -> Option<BTreeMap<String, String>> {
    let guard = pending_store()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    guard.get(key).cloned()
}

pub(super) fn clear_pending(key: &str) {
    if let Ok(mut guard) = pending_store().lock() {
        guard.remove(key);
    }
}

pub(super) fn save_login(cookies: &BTreeMap<String, String>) -> Result<(), String> {
    let user_id = cookies.get("uin").or_else(|| cookies.get("wxuin")).cloned();
    store().update(|session| {
        for (name, value) in cookies {
            session.set_cookie(name, value);
        }
        if let Some(user_id) = user_id {
            session.user_id = Some(user_id);
        }
    })?;
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
    fn login_requires_an_identity_and_a_key() {
        let mut session = SourceSession::default();
        assert!(!logged_in(&session));
        session.set_cookie("uin", "o123");
        assert!(!logged_in(&session), "只有身份、没有音乐凭证，不算登录");
        session.set_cookie("qqmusic_key", "key");
        assert!(logged_in(&session));

        // 微信扫码写的是另一组命名，必须同样识别。
        let mut wechat = SourceSession::default();
        assert!(!logged_in(&wechat));
        wechat.set_cookie("wxuin", "123456");
        assert!(!logged_in(&wechat));
        wechat.set_cookie("qm_keyst", "W_X_key");
        assert!(logged_in(&wechat), "微信登录态用的是 wxuin + qm_keyst");
    }

    #[test]
    fn pending_round_trips() {
        let mut cookies = BTreeMap::new();
        cookies.insert("pt_login_sig".to_string(), "sig".to_string());
        save_pending("test-key", &cookies).unwrap();
        assert_eq!(
            pending("test-key").and_then(|cookies| cookies.get("pt_login_sig").cloned()),
            Some("sig".to_string())
        );
        save_pending("test-key", &BTreeMap::new()).unwrap();
        assert!(pending("test-key").is_some());
        clear_pending("test-key");
        assert!(pending("test-key").is_none());
    }
}
