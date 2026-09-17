//! QQ 音乐扫码登录。
//!
//! QQ 的二维码是**图片**（`ptqrshow` 直接返回 PNG），不是可以自己编码的链接，
//! 所以会话里带的是 PNG 的 base64，由界面负责画到终端。
//!
//! 流程：
//! 1. `xlogin` 预热拿 `pt_login_sig` 等 cookie；
//! 2. `ptqrshow` 取 PNG，并从 `Set-Cookie` 里拿 `qrsig`；
//! 3. `ptqrlogin` 轮询，`ptqrtoken = hash33(qrsig)`，状态码
//!    0 成功 / 65 过期 / 66 等待 / 67 已扫码；
//! 4. 成功后跟着 `ptuiCB` 给出的跳转地址走完 OAuth 链路，收集沿途 cookie。

use std::collections::BTreeMap;

use base64::Engine;
use lx_core::model::login::{QrLoginResult, QrLoginSession, QrLoginStatus};
use lx_core::model::source::SourceId;
use lx_core::traits::source::FetchError;

use crate::http;
use crate::http::SendWithRetry;

use super::session;

const XLOGIN_API: &str = "https://xui.ptlogin2.qq.com/cgi-bin/xlogin";
const QR_SHOW_API: &str = "https://xui.ptlogin2.qq.com/ssl/ptqrshow";
const QR_CHECK_API: &str = "https://xui.ptlogin2.qq.com/ssl/ptqrlogin";
const OAUTH_JUMP: &str = "https://graph.qq.com/oauth2.0/login_jump";
const OAUTH_CLIENT_ID: &str = "100497308";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";
const REFERER: &str = "https://xui.ptlogin2.qq.com/";
/// OAuth 跳转最多跟这么多跳，避免异常链接导致死循环。
const MAX_REDIRECTS: usize = 10;

/// QQ 的 `hash33`：`h += (h << 5) + c`，最后取低 31 位。
pub(super) fn hash33(value: &str) -> u32 {
    let mut hash: u32 = 0;
    for byte in value.bytes() {
        hash = hash
            .wrapping_shl(5)
            .wrapping_add(hash)
            .wrapping_add(u32::from(byte));
    }
    hash & 0x7fff_ffff
}

/// 解析 `ptuiCB('code','0','url','0','message','nickname')`。
pub(super) fn parse_ptui_cb(raw: &str) -> Option<(String, String, String, String)> {
    let start = raw.find("ptuiCB(")?;
    let body = &raw[start + "ptuiCB(".len()..];
    let end = body.find(')')?;
    let parts = body[..end]
        .split(',')
        .map(|part| part.trim().trim_matches('\'').to_string())
        .collect::<Vec<_>>();
    if parts.len() < 5 {
        return None;
    }
    Some((
        parts[0].clone(),
        parts[2].clone(),
        parts[4].clone(),
        parts.get(5).cloned().unwrap_or_default(),
    ))
}

fn status_from_code(code: &str) -> QrLoginStatus {
    match code {
        "0" => QrLoginStatus::Success,
        "65" => QrLoginStatus::Expired,
        "66" => QrLoginStatus::Waiting,
        "67" => QrLoginStatus::Scanned,
        _ => QrLoginStatus::Failed,
    }
}

fn cookie_header(cookies: &BTreeMap<String, String>) -> Option<String> {
    if cookies.is_empty() {
        return None;
    }
    let mut pairs = cookies
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>();
    pairs.sort();
    Some(pairs.join("; "))
}

/// 收集响应里的 `Set-Cookie`（只取键值，丢弃属性）。
fn collect_cookies(headers: &reqwest::header::HeaderMap, into: &mut BTreeMap<String, String>) {
    for value in headers.get_all(reqwest::header::SET_COOKIE) {
        if let Ok(value) = value.to_str()
            && let Some(pair) = value.split(';').next()
            && let Some((name, value)) = pair.split_once('=')
            && !name.trim().is_empty()
        {
            into.insert(name.trim().to_string(), value.trim().to_string());
        }
    }
}

/// 预热：登录页本身会下发 `pt_login_sig` 等 cookie，后面的请求都要带上。
async fn warmup() -> BTreeMap<String, String> {
    let mut cookies = BTreeMap::new();
    let url = format!(
        "{XLOGIN_API}?appid=716027609&daid=383&style=33&login_text=登录&hide_title_bar=1&hide_border=1&target=self&s_url={OAUTH_JUMP}&pt_3rd_aid={OAUTH_CLIENT_ID}&theme=2"
    );
    if let Ok(response) = http::client()
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Referer", REFERER)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
    {
        collect_cookies(response.headers(), &mut cookies);
    }
    cookies
}

pub async fn create() -> Result<QrLoginSession, FetchError> {
    let mut cookies = warmup().await;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or_default();
    let url = format!(
        "{QR_SHOW_API}?appid=716027609&e=2&l=M&s=3&d=72&v=4&t={timestamp}&daid=383&pt_3rd_aid={OAUTH_CLIENT_ID}&u1={OAUTH_JUMP}"
    );
    let mut request = http::client()
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Referer", REFERER);
    if let Some(cookie) = cookie_header(&cookies) {
        request = request.header("Cookie", cookie);
    }
    let response = request
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?;
    collect_cookies(response.headers(), &mut cookies);
    let png = response
        .bytes()
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?;
    let qrsig = cookies
        .get("qrsig")
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| FetchError::Other("QQ 二维码没有返回 qrsig".to_string()))?
        .to_string();
    // 轮询要用到预热阶段的 cookie，先暂存起来，随下次 check 一起带上。
    let mut carried = cookies;
    carried.remove("qrsig");
    session::save_pending(&carried).map_err(FetchError::Other)?;
    Ok(QrLoginSession {
        source: SourceId::Tx,
        key: qrsig,
        url: String::new(),
        image_png: Some(base64::engine::general_purpose::STANDARD.encode(png)),
        expires_in: 180,
    })
}

pub async fn check(key: &str) -> Result<QrLoginResult, FetchError> {
    let qrsig = key.trim();
    if qrsig.is_empty() {
        return Err(FetchError::Other("QQ 二维码 key 为空".to_string()));
    }
    let mut cookies = session::pending().unwrap_or_default();
    cookies.insert("qrsig".to_string(), qrsig.to_string());
    let action = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let url = format!(
        "{QR_CHECK_API}?u1={OAUTH_JUMP}&ptqrtoken={}&ptredirect=0&h=1&t=1&g=1&from_ui=1&ptlang=2052&action=0-0-{action}&js_ver=26071711&js_type=1&login_sig=&pt_uistyle=40&aid=716027609&daid=383&pt_3rd_aid={OAUTH_CLIENT_ID}",
        hash33(qrsig)
    );
    let mut request = http::client()
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Referer", REFERER);
    if let Some(cookie) = cookie_header(&cookies) {
        request = request.header("Cookie", cookie);
    }
    let body = request
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .text()
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?;
    let (code, redirect, message, nickname) =
        parse_ptui_cb(&body).ok_or_else(|| FetchError::Other("QQ 登录状态解析失败".to_string()))?;
    let status = status_from_code(&code);
    let mut result = QrLoginResult::new(
        status,
        if message.trim().is_empty() {
            "登录状态未知".to_string()
        } else {
            message
        },
    );
    if status != QrLoginStatus::Success {
        return Ok(result);
    }

    // 成功后跟着跳转把 OAuth 链路上的 cookie 收全。
    let collected = follow_redirects(&redirect, cookies).await;
    if !collected.contains_key("uin") && !collected.contains_key("qqmusic_key") {
        let mut failed = QrLoginResult::new(
            QrLoginStatus::Failed,
            "QQ 登录成功但未拿到音乐凭证，请重新扫码".to_string(),
        );
        failed.cookies = collected;
        return Ok(failed);
    }
    session::save_login(&collected).map_err(FetchError::Other)?;
    result.cookies = collected;
    result.user_name = (!nickname.trim().is_empty()).then_some(nickname);
    Ok(result)
}

/// 手动跟随跳转：每跳都收集 cookie，最多 [`MAX_REDIRECTS`] 跳。
async fn follow_redirects(
    first: &str,
    cookies: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut collected = cookies;
    let mut location = first.trim().to_string();
    for _ in 0..MAX_REDIRECTS {
        if location.is_empty() {
            break;
        }
        let mut request = http::client()
            .get(&location)
            .header("User-Agent", USER_AGENT)
            .header("Referer", REFERER);
        if let Some(cookie) = cookie_header(&collected) {
            request = request.header("Cookie", cookie);
        }
        let Ok(response) = request.send_with_retry(crate::http::RETRY_ATTEMPTS).await else {
            break;
        };
        collect_cookies(response.headers(), &mut collected);
        let next = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
            .unwrap_or_default();
        if next.is_empty() {
            break;
        }
        location = resolve_location(&location, &next);
    }
    collected
}

/// 相对跳转要基于当前地址补全。
fn resolve_location(current: &str, next: &str) -> String {
    if next.starts_with("http") {
        return next.to_string();
    }
    let Some((scheme, rest)) = current.split_once("://") else {
        return next.to_string();
    };
    let host = rest.split('/').next().unwrap_or_default();
    // 相对地址要么以 `/` 开头，要么是当前路径的最后一段替换。
    let path = if next.starts_with('/') {
        next.to_string()
    } else {
        format!("/{next}")
    };
    format!("{scheme}://{host}{path}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash33_matches_the_rolling_formula() {
        let expected = {
            let mut hash: u32 = 0;
            for byte in "abc".bytes() {
                hash = (hash << 5).wrapping_add(hash).wrapping_add(u32::from(byte));
            }
            hash & 0x7fff_ffff
        };
        assert_eq!(hash33("abc"), expected);
        assert_eq!(hash33(""), 0);
    }

    #[test]
    fn parses_ptui_callback() {
        let raw = "ptuiCB('0','0','https://ptlogin2.qq.com/login?u1=x','0','登录成功！', '昵称');";
        let (code, redirect, message, nickname) = parse_ptui_cb(raw).unwrap();
        assert_eq!(code, "0");
        assert!(redirect.starts_with("https://ptlogin2.qq.com/login"));
        assert_eq!(message, "登录成功！");
        assert_eq!(nickname, "昵称");

        let waiting = "ptuiCB('66','0','','0','二维码未失效。','');";
        assert_eq!(parse_ptui_cb(waiting).unwrap().0, "66");
        assert!(parse_ptui_cb("no callback here").is_none());
    }

    #[test]
    fn maps_status_codes() {
        assert_eq!(status_from_code("0"), QrLoginStatus::Success);
        assert_eq!(status_from_code("65"), QrLoginStatus::Expired);
        assert_eq!(status_from_code("66"), QrLoginStatus::Waiting);
        assert_eq!(status_from_code("67"), QrLoginStatus::Scanned);
        assert_eq!(status_from_code("99"), QrLoginStatus::Failed);
    }

    #[test]
    fn resolves_relative_redirects_against_the_current_host() {
        assert_eq!(
            resolve_location("https://a.com/x", "https://b.com/y"),
            "https://b.com/y"
        );
        assert_eq!(resolve_location("https://a.com/x", "/y"), "https://a.com/y");
        assert_eq!(resolve_location("https://a.com/x", "y"), "https://a.com/y");
        assert_eq!(resolve_location("not-a-url", "/y"), "/y");
    }
}
