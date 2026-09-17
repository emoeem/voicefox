//! 网易云登录态。
//!
//! 会话统一存在通用 `SessionStore` 里（`~/.config/voicefox/session/wy.json`），
//! 这里只提供进程内单例与「要不要带 cookie」的判断，避免每个请求点都去
//! 读一次文件。

use std::sync::OnceLock;

use lx_core::model::source::SourceId;

use crate::session::{SessionStore, SourceSession};

/// 登录凭据 cookie：网易云靠 `MUSIC_U` 判定登录。
const LOGIN_COOKIE: &str = "MUSIC_U";

static STORE: OnceLock<SessionStore> = OnceLock::new();

pub(super) fn store() -> &'static SessionStore {
    STORE.get_or_init(|| SessionStore::load(SourceId::Wy))
}

pub(super) fn snapshot() -> SourceSession {
    store().snapshot()
}

/// 请求头里的 Cookie；未登录或没有 cookie 时返回 `None`。
pub(super) fn cookie_header() -> Option<String> {
    let session = snapshot();
    session
        .cookie_header_of(&[
            LOGIN_COOKIE,
            "MUSIC_A",
            "__csrf",
            "NMTID",
            "JSESSIONID-WYYY",
        ])
        .or_else(|| {
            (!session.cookies.is_empty()).then(|| {
                session
                    .cookies
                    .iter()
                    .map(|(name, value)| format!("{name}={value}"))
                    .collect::<Vec<_>>()
                    .join("; ")
            })
        })
}

pub(super) fn is_logged_in() -> bool {
    snapshot().has_cookie(LOGIN_COOKIE)
}

/// 扫码成功后写入 cookie。
pub(super) fn save_cookies(
    cookies: &std::collections::BTreeMap<String, String>,
) -> Result<(), String> {
    store().update(|session| {
        for (name, value) in cookies {
            session.set_cookie(name, value);
        }
    })
}

pub(super) fn logout() -> Result<(), String> {
    store().clear()
}
