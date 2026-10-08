//! QQ 音乐扫码登录。
//!
//! 同一份登录态有两条互不相干的扫码渠道，由 [`QrLoginKind`] 选择：
//!
//! - `Standard`（QQ 扫码）：`ptqrshow` 返回 **PNG**，`ptqrlogin` 轮询到
//!   `ptuiCB` 后跟着 OAuth 跳转收集 cookie；
//! - `WeChat`（微信扫码）：`open.weixin.qq.com` 返回 **JPEG**，长轮询
//!   `lp.open.weixin.qq.com` 拿到 `wx_code` 后到 `musicu.fcg` 换 musickey。
//!
//! 两条渠道的二维码都是**图片**，不是能自己重新编码的链接（微信二维码的内容
//! 是 `connect/confirm?uuid=…`，自己编成 QR 扫不出来），所以会话里带的是图片的
//! base64，由界面负责画到终端。
//!
//! `ptlogin2.qq.com` 那条链路没有微信变体：实测改参数会被忽略，不带 `qrsig`
//! 时直接 403。微信必须是独立代码路径，不能靠改参数复用 QQ 流程。
//!
//! QQ 流程：
//! 1. `xlogin` 预热拿 `pt_login_sig` 等 cookie；
//! 2. `ptqrshow` 取 PNG，并从 `Set-Cookie` 里拿 `qrsig`；
//! 3. `ptqrlogin` 轮询，`ptqrtoken = hash33(qrsig)`，状态码
//!    0 成功 / 65 过期 / 66 等待 / 67 已扫码；
//! 4. 成功后跟着 `ptuiCB` 给出的跳转地址走完 OAuth 链路，收集沿途 cookie。

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use base64::Engine;
use lx_core::model::login::{QrLoginKind, QrLoginResult, QrLoginSession, QrLoginStatus};
use lx_core::model::source::SourceId;
use lx_core::traits::source::FetchError;
use serde_json::{Value, json};

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

// ── 微信渠道 ────────────────────────────────────────────────────────────
//
// 常量全部来自 y.qq.com 网页版自己的请求（见 docs/research/qq-music-wechat-login.md）。

/// 微信开放平台「QQ 音乐」网页登录应用。
const WX_APP_ID: &str = "wx48db31d50e334801";
/// 探二维码 uuid 的页面。
const WX_QR_CONNECT_API: &str = "https://open.weixin.qq.com/connect/qrconnect";
/// 二维码图片：`{WX_QRCODE_API}/{uuid}`。
const WX_QRCODE_API: &str = "https://open.weixin.qq.com/connect/qrcode";
/// 长轮询接口（服务端约 15 秒才回一次）。
const WX_QR_CHECK_API: &str = "https://lp.open.weixin.qq.com/connect/l/qrconnect";
/// 官方 `wx_redirect.html` 页面自己带的参数，微信只校验其中的 `login_type`。
const WX_REDIRECT_URI: &str =
    "https://y.qq.com/portal/wx_redirect.html?login_type=2&surl=https%3A%2F%2Fy.qq.com%2F";
/// 换凭据的接口；其余域名是备用（主域名偶尔对陌生 IP 风控）。
const MUSICU_API: &str = "https://u.y.qq.com/cgi-bin/musicu.fcg";
const MUSICU_FALLBACKS: [&str; 2] = [
    "https://szu.y.qq.com/cgi-bin/musicu.fcg",
    "https://shu.y.qq.com/cgi-bin/musicu.fcg",
];
const WX_REFERER: &str = "https://y.qq.com/";
/// 轮询 key 的前缀：`check` 靠它区分渠道，`SourceCapabilities` 因此不用加字段。
const WX_KEY_PREFIX: &str = "wx:";
/// 微信二维码有效期（秒）：服务端不下发，与官方页面一致按 5 分钟算。
const WX_EXPIRES_IN: u64 = 300;
/// 长轮询单次的最长等待，必须盖过全局 15 秒超时。
const WX_POLL_TIMEOUT: Duration = Duration::from_secs(35);

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

/// 默认渠道（QQ 扫码）的二维码。
pub async fn create() -> Result<QrLoginSession, FetchError> {
    create_kind(QrLoginKind::Standard).await
}

/// 按渠道创建二维码会话。
pub async fn create_kind(kind: QrLoginKind) -> Result<QrLoginSession, FetchError> {
    match kind {
        QrLoginKind::Standard => create_qq().await,
        QrLoginKind::WeChat => create_wechat().await,
    }
}

async fn create_qq() -> Result<QrLoginSession, FetchError> {
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
    session::save_pending(&qrsig, &carried).map_err(FetchError::Other)?;
    Ok(QrLoginSession {
        source: SourceId::Tx,
        kind: QrLoginKind::Standard,
        key: qrsig,
        url: String::new(),
        image_png: Some(base64::engine::general_purpose::STANDARD.encode(png)),
        expires_in: 180,
    })
}

/// 轮询一次扫码状态。
///
/// 微信渠道的 key 带 `wx:` 前缀（见 [`WX_KEY_PREFIX`]），其余按 QQ 处理。
pub async fn check(key: &str) -> Result<QrLoginResult, FetchError> {
    if let Some(uuid) = key.trim().strip_prefix(WX_KEY_PREFIX) {
        return check_wechat(uuid).await;
    }
    check_qq(key).await
}

async fn check_qq(key: &str) -> Result<QrLoginResult, FetchError> {
    let qrsig = key.trim();
    if qrsig.is_empty() {
        return Err(FetchError::Other("QQ 二维码 key 为空".to_string()));
    }
    let mut cookies = session::pending(qrsig).unwrap_or_default();
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
        return Ok(QrLoginResult::new(
            QrLoginStatus::Failed,
            "QQ 登录成功但未拿到音乐凭证，请重新扫码".to_string(),
        ));
    }
    session::save_login(&collected).map_err(FetchError::Other)?;
    session::clear_pending(qrsig);
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

// ── 微信渠道实现 ────────────────────────────────────────────────────────

/// 微信长轮询的一次结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WxPoll {
    /// 还没人扫码。
    Waiting,
    /// 已扫码，等用户在手机上确认。
    Scanned,
    /// 已确认，`wx_code` 可以去换登录凭据。
    Confirmed(String),
    /// 二维码过期。
    Expired,
    /// 失败，附服务端说明。
    Failed(String),
}

/// 「上一轮已经见到 404（已扫码）」的标记。
///
/// 微信长轮询是有状态的：服务端第一次告知"已扫码"之后，后续每一轮都要回带
/// `last=404`，否则会退回"等待扫码"重新开始。状态跨请求，放在模块级 map 里，
/// 终态时清掉以免长期占用。
static WX_POLL_SEEN_SCAN: OnceLock<Mutex<BTreeMap<String, bool>>> = OnceLock::new();

fn wx_poll_guard() -> std::sync::MutexGuard<'static, BTreeMap<String, bool>> {
    WX_POLL_SEEN_SCAN
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

fn wx_seen_scan(uuid: &str) -> bool {
    wx_poll_guard().get(uuid).copied().unwrap_or(false)
}

fn wx_mark_seen_scan(uuid: &str) {
    wx_poll_guard().insert(uuid.to_string(), true);
}

fn wx_clear_poll_state(uuid: &str) {
    wx_poll_guard().remove(uuid);
}

/// 轮询用的 URL；`seen_scan` 决定要不要回带 `last=404`。
fn wx_poll_url(uuid: &str, seen_scan: bool) -> String {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let last = if seen_scan { "&last=404" } else { "" };
    format!("{WX_QR_CHECK_API}?uuid={uuid}&_={nonce}{last}")
}

/// 从 `qrconnect` 页面里抠出 uuid。
///
/// 页面里至少有两处能拿到：`connect/l/qrconnect?uuid=…`（JS 变量）和
/// `/connect/qrcode/{uuid}`（二维码图片地址），按这个优先级取第一个。
fn extract_wx_uuid(html: &str) -> Option<String> {
    const MARKERS: [&str; 2] = ["connect/l/qrconnect?uuid=", "/connect/qrcode/"];
    for marker in MARKERS {
        if let Some(rest) = html.split(marker).nth(1) {
            let uuid = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect::<String>();
            if !uuid.is_empty() {
                return Some(uuid);
            }
        }
    }
    None
}

/// 解析 `window.wx_errcode=408;window.wx_code='';`。
fn parse_wx_poll(body: &str) -> Option<(String, String)> {
    let errcode = body
        .split("wx_errcode=")
        .nth(1)?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    if errcode.is_empty() {
        return None;
    }
    let code = body
        .split("wx_code='")
        .nth(1)
        .and_then(|rest| rest.split('\'').next())
        .unwrap_or_default()
        .to_string();
    Some((errcode, code))
}

/// 微信错误码 → 轮询结果。
///
/// 408=未扫码 / 404=已扫码 / 402=已过期 / 403=用户拒绝 / 405=已确认。
fn wx_status(errcode: &str, code: &str) -> WxPoll {
    match errcode {
        "408" => WxPoll::Waiting,
        "404" => WxPoll::Scanned,
        "402" => WxPoll::Expired,
        "403" => WxPoll::Failed("微信登录被拒绝".to_string()),
        "405" => {
            let code = code.trim();
            if code.is_empty() {
                WxPoll::Failed("微信已确认但没有返回 code".to_string())
            } else {
                WxPoll::Confirmed(code.to_string())
            }
        }
        other => WxPoll::Failed(format!("微信登录失败（{other}）")),
    }
}

/// 失败信息里带一小段响应体，方便排查风控页之类的情况。
fn body_excerpt(body: &str) -> String {
    body.trim().chars().take(80).collect()
}

/// 微信渠道：要一张二维码图片。
async fn create_wechat() -> Result<QrLoginSession, FetchError> {
    let state = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let url = format!(
        "{WX_QR_CONNECT_API}?appid={WX_APP_ID}&redirect_uri={}&response_type=code&scope=snsapi_login&state={state}",
        urlencoding::encode(WX_REDIRECT_URI),
    );
    let html = http::client()
        .get(&url)
        .header("User-Agent", USER_AGENT)
        .header("Referer", WX_REFERER)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .text()
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?;
    let uuid = extract_wx_uuid(&html)
        .ok_or_else(|| FetchError::Parse("微信二维码页面里没有 uuid".to_string()))?;
    let jpeg = http::client()
        .get(format!("{WX_QRCODE_API}/{uuid}"))
        .header("User-Agent", USER_AGENT)
        .header("Referer", WX_REFERER)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .bytes()
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?;
    if jpeg.is_empty() {
        return Err(FetchError::Other("微信二维码图片为空".to_string()));
    }
    wx_clear_poll_state(&uuid);
    Ok(QrLoginSession {
        source: SourceId::Tx,
        kind: QrLoginKind::WeChat,
        key: format!("{WX_KEY_PREFIX}{uuid}"),
        // 微信二维码不能自己重编码，必须原样贴图片，所以 url 留空。
        url: String::new(),
        image_png: Some(base64::engine::general_purpose::STANDARD.encode(jpeg)),
        expires_in: WX_EXPIRES_IN,
    })
}

/// 轮询一次微信登录状态。
///
/// 服务端会阻塞约 15 秒才回，所以这里单独覆盖超时。`last=404` 的回带状态记在
/// 模块级 map 里，调用方（界面或探针）只管反复调用即可。
pub async fn poll_wechat(uuid: &str) -> Result<WxPoll, FetchError> {
    let uuid = uuid.trim();
    if uuid.is_empty() {
        return Err(FetchError::Other("微信二维码 uuid 为空".to_string()));
    }
    let body = http::client()
        .get(wx_poll_url(uuid, wx_seen_scan(uuid)))
        .timeout(WX_POLL_TIMEOUT)
        .header("User-Agent", USER_AGENT)
        .header("Referer", WX_REFERER)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .text()
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?;
    let outcome = match parse_wx_poll(&body) {
        Some((errcode, code)) => wx_status(&errcode, &code),
        None => WxPoll::Failed(format!("微信登录状态解析失败：{}", body_excerpt(&body))),
    };
    match &outcome {
        // 只有「已扫码」需要延续 last=404；终态顺手清掉状态。
        WxPoll::Scanned => wx_mark_seen_scan(uuid),
        WxPoll::Waiting | WxPoll::Confirmed(_) | WxPoll::Expired | WxPoll::Failed(_) => {
            wx_clear_poll_state(uuid);
        }
    }
    Ok(outcome)
}

/// 用 `wx_code` 换登录凭据，返回 `musicu` 的**原始 JSON，不落盘**。
///
/// 正式登录走 [`check_wechat`]；这个函数同时给探针用来核对服务端到底返回了什么。
pub async fn exchange_wechat(code: &str) -> Result<Value, FetchError> {
    let code = code.trim();
    if code.is_empty() {
        return Err(FetchError::Other("微信登录 code 为空".to_string()));
    }
    let body = json!({
        "comm": {
            "tmeAppID": "qqmusic",
            "tmeLoginType": "1",
            "g_tk": 5381,
            "platform": "yqq",
            "ct": 24,
            "cv": 0,
        },
        "req": {
            "module": "music.login.LoginServer",
            "method": "Login",
            "param": { "strAppid": WX_APP_ID, "code": code },
        },
    });
    let payload =
        serde_json::to_string(&body).map_err(|error| FetchError::Parse(error.to_string()))?;
    let mut last_error = None;
    for api in std::iter::once(MUSICU_API).chain(MUSICU_FALLBACKS) {
        // 刻意只带 login_type=2，不掺本机已有的 QQ 登录 cookie。
        let response = http::client()
            .post(api)
            .header("User-Agent", USER_AGENT)
            .header("Referer", WX_REFERER)
            .header("Origin", "https://y.qq.com")
            .header("Cookie", "login_type=2")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(payload.clone())
            .send_with_retry(crate::http::RETRY_ATTEMPTS)
            .await;
        match response {
            Ok(response) => {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                match serde_json::from_str::<Value>(&text) {
                    Ok(value) => return Ok(value),
                    Err(error) => {
                        last_error = Some(format!(
                            "{api} 返回 {status} 但 JSON 解析失败：{error}（{}）",
                            body_excerpt(&text)
                        ));
                    }
                }
            }
            Err(error) => last_error = Some(format!("{api} 请求失败：{error}")),
        }
    }
    Err(FetchError::Network(
        last_error.unwrap_or_else(|| "微信登录凭据换取失败".to_string()),
    ))
}

/// 微信账号在 QQ 音乐里的数字 ID。
///
/// `str_musicid` 是字符串、`musicid` 可能是数字，两种都实测出现过。
fn wechat_uin(data: &Value) -> Option<String> {
    if let Some(value) = data["str_musicid"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(value.to_string());
    }
    match &data["musicid"] {
        Value::String(value) if !value.trim().is_empty() => Some(value.trim().to_string()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

/// 从 `musicu` 响应里抽出要写入会话的 cookie。
///
/// 成功判据：外层 `code == 0`、`req.code == 0`、`req.data.musickey` 非空。
/// 微信账号的 musickey 形如 `W_X_…`；身份 cookie 两个名字都写，因为 QQ 音乐
/// 不同接口读的字段不一样（`uin` 是绝大多数接口用的那个）。
fn wechat_cookies(response: &Value) -> Option<BTreeMap<String, String>> {
    if response["code"].as_i64() != Some(0) || response["req"]["code"].as_i64() != Some(0) {
        return None;
    }
    let data = &response["req"]["data"];
    let musickey = data["musickey"].as_str()?.trim();
    if musickey.is_empty() {
        return None;
    }
    let mut cookies = BTreeMap::new();
    cookies.insert("login_type".to_string(), "2".to_string());
    if let Some(uin) = wechat_uin(data) {
        cookies.insert("uin".to_string(), uin.clone());
        cookies.insert("wxuin".to_string(), uin);
    }
    for name in ["qqmusic_key", "qm_keyst"] {
        cookies.insert(name.to_string(), musickey.to_string());
    }
    for (cookie, field) in [
        ("wxopenid", "openid"),
        ("wxunionid", "unionid"),
        ("wxaccess_token", "access_token"),
        ("wxrefresh_token", "refresh_token"),
    ] {
        if let Some(value) = data[field]
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            cookies.insert(cookie.to_string(), value.to_string());
        }
    }
    Some(cookies)
}

/// 兑换成功但没拿到 musickey 时，把服务端错误码带进界面提示。
fn wechat_failure_message(response: &Value) -> String {
    let outer = response["code"].as_i64();
    let inner = response["req"]["code"].as_i64();
    if outer == Some(0) && inner == Some(0) {
        return "微信登录成功但未拿到音乐凭证，请重新扫码".to_string();
    }
    match inner.or(outer) {
        Some(code) => format!("微信登录失败（服务端返回 {code}）"),
        None => "微信登录失败".to_string(),
    }
}

/// 微信渠道：拿到 `wx_code` 后换 musickey 并落盘。
async fn check_wechat(uuid: &str) -> Result<QrLoginResult, FetchError> {
    match poll_wechat(uuid).await? {
        WxPoll::Waiting => Ok(QrLoginResult::new(QrLoginStatus::Waiting, "等待扫码")),
        WxPoll::Scanned => Ok(QrLoginResult::new(
            QrLoginStatus::Scanned,
            "已扫码，请在微信中确认",
        )),
        WxPoll::Expired => Ok(QrLoginResult::new(
            QrLoginStatus::Expired,
            "微信二维码已过期，请重新生成",
        )),
        WxPoll::Failed(message) => Ok(QrLoginResult::new(QrLoginStatus::Failed, message)),
        WxPoll::Confirmed(code) => {
            let response = exchange_wechat(&code).await?;
            let Some(cookies) = wechat_cookies(&response) else {
                return Ok(QrLoginResult::new(
                    QrLoginStatus::Failed,
                    wechat_failure_message(&response),
                ));
            };
            session::save_login(&cookies).map_err(FetchError::Other)?;
            // 兑换响应里没有实测到的昵称字段，账号显示名留给会话里的 user_id。
            Ok(QrLoginResult::new(QrLoginStatus::Success, "登录成功"))
        }
    }
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

    // ── 微信渠道 ────────────────────────────────────────────────────────

    /// 一份「换凭据成功」的真实字段形状（来自调研文档记录的响应）。
    fn wx_success_response() -> Value {
        json!({
            "code": 0,
            "req": {
                "code": 0,
                "data": {
                    "musickey": "W_X_key",
                    "str_musicid": "123456",
                    "openid": "openid-1",
                    "unionid": "unionid-1",
                    "access_token": "at",
                    "refresh_token": "rt",
                },
            },
        })
    }

    #[test]
    fn extracts_uuid_from_the_qr_page() {
        let js = r#"<script>var u="https://open.weixin.qq.com/connect/l/qrconnect?uuid=061AbC-dEf_12";</script>"#;
        assert_eq!(
            extract_wx_uuid(js).as_deref(),
            Some("061AbC-dEf_12"),
            "优先从轮询地址里取 uuid"
        );
        let image = r#"<img src="https://open.weixin.qq.com/connect/qrcode/061gVkgZ2dwlGa1u">"#;
        assert_eq!(
            extract_wx_uuid(image).as_deref(),
            Some("061gVkgZ2dwlGa1u"),
            "没有轮询地址时退到二维码图片地址"
        );
        assert!(extract_wx_uuid("<html>no uuid here</html>").is_none());
    }

    #[test]
    fn parses_wechat_poll_states() {
        let waiting = parse_wx_poll("window.wx_errcode=408;window.wx_code='';").unwrap();
        assert_eq!(waiting, ("408".to_string(), String::new()));
        let scanned = parse_wx_poll("window.wx_errcode=404;window.wx_code='';").unwrap();
        assert_eq!(scanned.0, "404");
        let confirmed = parse_wx_poll("window.wx_errcode=405;window.wx_code='0110abc';").unwrap();
        assert_eq!(confirmed, ("405".to_string(), "0110abc".to_string()));
        // 风控页/错误页不是这个格式，必须识别成解析失败而不是瞎猜状态。
        assert!(parse_wx_poll("<html>403 Forbidden</html>").is_none());
    }

    #[test]
    fn maps_wechat_error_codes() {
        assert_eq!(wx_status("408", ""), WxPoll::Waiting);
        assert_eq!(wx_status("404", ""), WxPoll::Scanned);
        assert_eq!(wx_status("402", ""), WxPoll::Expired);
        assert_eq!(
            wx_status("405", "code-1"),
            WxPoll::Confirmed("code-1".to_string())
        );
        assert!(matches!(wx_status("405", ""), WxPoll::Failed(_)));
        assert!(matches!(wx_status("403", ""), WxPoll::Failed(_)));
        assert!(matches!(wx_status("500", ""), WxPoll::Failed(_)));
    }

    #[test]
    fn poll_url_only_carries_last_after_a_scan() {
        let waiting = wx_poll_url("uuid-1", false);
        assert!(waiting.contains("uuid=uuid-1"));
        assert!(!waiting.contains("last="), "还没扫码时不能带 last");

        let scanned = wx_poll_url("uuid-1", true);
        assert!(
            scanned.contains("&last=404"),
            "已扫码后每轮都要回带 last=404，否则会退回等待扫码"
        );
    }

    #[test]
    fn wechat_cookies_require_a_musickey() {
        let cookies = wechat_cookies(&wx_success_response()).unwrap();
        assert_eq!(cookies.get("uin").map(String::as_str), Some("123456"));
        assert_eq!(cookies.get("wxuin").map(String::as_str), Some("123456"));
        assert_eq!(
            cookies.get("qqmusic_key").map(String::as_str),
            Some("W_X_key")
        );
        assert_eq!(cookies.get("qm_keyst").map(String::as_str), Some("W_X_key"));
        assert_eq!(cookies.get("login_type").map(String::as_str), Some("2"));
        assert_eq!(
            cookies.get("wxopenid").map(String::as_str),
            Some("openid-1")
        );
        assert_eq!(
            cookies.get("wxrefresh_token").map(String::as_str),
            Some("rt")
        );

        // `musicid` 是数字时也要能当身份用。
        let mut numeric = wx_success_response();
        numeric["req"]["data"]["str_musicid"] = Value::Null;
        numeric["req"]["data"]["musicid"] = json!(654321);
        let cookies = wechat_cookies(&numeric).unwrap();
        assert_eq!(cookies.get("uin").map(String::as_str), Some("654321"));

        // 服务端说失败时不能写出半套登录态。
        let mut failed = wx_success_response();
        failed["req"]["code"] = json!(1000);
        assert!(wechat_cookies(&failed).is_none());
        assert!(wechat_failure_message(&failed).contains("1000"));

        let mut empty = wx_success_response();
        empty["req"]["data"]["musickey"] = json!("");
        assert!(wechat_cookies(&empty).is_none());
        assert!(wechat_failure_message(&empty).contains("音乐凭证"));
    }
}
