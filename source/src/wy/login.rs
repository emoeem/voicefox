//! 网易云扫码登录。
//!
//! - 生成：POST `/api/login/qrcode/unikey`（表单 `type=3`）拿到 `unikey`；
//! - 轮询：POST `/api/login/qrcode/client/login`（表单 `key` + `type=3`），
//!   状态码 800 过期 / 801 等待 / 802 已扫码 / 803 成功，成功后响应体与
//!   `Set-Cookie` 里都带登录票据，两者都收下来。

use std::collections::BTreeMap;

use lx_core::model::login::{QrLoginKind, QrLoginResult, QrLoginSession, QrLoginStatus};
use lx_core::model::source::SourceId;
use lx_core::traits::source::FetchError;
use serde_json::Value;

use crate::http;
use crate::http::SendWithRetry;

use super::session;
use super::{ACCOUNT_API, REFERER, USER_AGENT};

/// 扫码登录接口路径；主机在 `LOGIN_HOSTS` 里按可用性顺序尝试。
const QR_KEY_PATH: &str = "/api/login/qrcode/unikey";
const QR_CHECK_PATH: &str = "/api/login/qrcode/client/login";

/// 请求账号信息，返回 (昵称, uid)。
///
/// 与 [`refresh`] 共用：验证会话的同时把账号名写进会话存储，设置页和登录
/// 成功页才能显示「谁登录了」（此前只写 cookie，`user_name` 永远为空）。
/// 服务端明确表示没有登录态时记一次失效标记。
async fn fetch_account() -> Result<(Option<String>, Option<String>), FetchError> {
    let response = super::with_cookie(http::client().get(ACCOUNT_API))
        .header("User-Agent", USER_AGENT)
        .header("Referer", REFERER)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?;
    let json: Value = response
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))?;
    if json["code"].as_i64() != Some(200) {
        if super::response_requires_login(&json) {
            session::mark_login_expired();
        }
        return Ok((None, None));
    }
    let name = json["profile"]["nickname"]
        .as_str()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string);
    let uid = json["account"]["id"]
        .as_i64()
        .or_else(|| json["profile"]["userId"].as_i64())
        .filter(|uid| *uid > 0)
        .map(|uid| uid.to_string());
    session::save_account(name.clone(), uid.clone());
    Ok((name, uid))
}

/// 探测当前网易云会话是否仍然有效。
///
/// 公开接口**没有可用的会话续期端点**：实测 `api/login/token/refresh` 返回
/// `code=400`、`api/login/refresh` 返回 `code=404`。所以这里不做「延长会话」，
/// 只做「探测」：
///
/// - `Ok(true)`  会话可用（顺带把昵称 / uid 写进会话存储）；
/// - `Ok(false)` 服务端明确表示没有登录态（`code != 200` 或拿不到 uid）；
/// - `Err(_)`    网络/解析问题，无法判定——调用方应当继续尝试，而不是据此
///   判定失效。
///
/// 无论结果如何都**不清除本地 cookie**：最终失效判定留给真正需要登录的接口。
pub async fn refresh() -> Result<bool, FetchError> {
    if !session::is_logged_in() {
        return Ok(false);
    }
    let (_, uid) = fetch_account().await?;
    Ok(uid.is_some())
}

/// 顺序尝试的扫码登录主机。
///
/// 同一套 `/api/login/qrcode/*` 接口在这三个主机上都可用（实测各 4/4 成功），
/// 但它们解析到不同的 IP（本例中 `interface.music.163.com` 与
/// `music.163.com` 各只有一个 A 记录）。单个主机抽风时表现就是「生成二维码
/// 失败」，所以按顺序回退，比只认一个域名稳。
const LOGIN_HOSTS: [&str; 3] = [
    "interface.music.163.com",
    "interface3.music.163.com",
    "music.163.com",
];

/// 依次尝试 `LOGIN_HOSTS`，返回第一个成功解析出 JSON 的响应。
///
/// 只在**传输层或解析层**失败时换主机；只要拿到了 JSON 就直接返回，业务码
/// 交给调用方判断（二维码过期之类的业务失败换主机也没有意义）。
async fn post_first_available(
    path: &str,
    body: &str,
    what: &str,
) -> Result<(reqwest::header::HeaderMap, Value), FetchError> {
    let mut last_error: Option<FetchError> = None;
    for host in LOGIN_HOSTS {
        let url = format!("https://{host}{path}");
        let sent = http::client()
            .post(&url)
            .header("User-Agent", USER_AGENT)
            .header("Referer", REFERER)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body.to_string())
            .send_with_retry(crate::http::RETRY_ATTEMPTS)
            .await;
        match sent {
            Ok(response) => {
                let headers = response.headers().clone();
                match response.json::<Value>().await {
                    Ok(json) => return Ok((headers, json)),
                    Err(error) => {
                        tracing::warn!("{what}：主机 {host} 返回了无法解析的响应: {error}");
                        last_error = Some(FetchError::Parse(error.to_string()));
                    }
                }
            }
            Err(error) => {
                tracing::warn!("{what}：主机 {host} 请求失败: {error}");
                last_error = Some(FetchError::Network(error.to_string()));
            }
        }
    }
    Err(last_error.unwrap_or_else(|| FetchError::Network(format!("{what}：所有登录主机均不可用"))))
}

pub async fn create() -> Result<QrLoginSession, FetchError> {
    let (_, json) = post_first_available(QR_KEY_PATH, "type=3", "网易云二维码生成").await?;
    if json["code"].as_i64() != Some(200) {
        return Err(FetchError::Other("网易云二维码生成失败".to_string()));
    }
    let key = json["unikey"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| FetchError::Parse("网易云二维码 key 为空".to_string()))?;
    Ok(QrLoginSession {
        source: SourceId::Wy,
        kind: QrLoginKind::Standard,
        key: key.to_string(),
        url: format!("https://music.163.com/login?codekey={key}"),
        image_png: None,
        expires_in: 300,
    })
}

pub async fn check(key: &str) -> Result<QrLoginResult, FetchError> {
    let key = key.trim();
    if key.is_empty() {
        return Err(FetchError::Other("网易云二维码 key 为空".to_string()));
    }
    let (headers, json) = post_first_available(
        QR_CHECK_PATH,
        &format!("key={key}&type=3"),
        "网易云登录轮询",
    )
    .await?;
    let code = json["code"].as_i64().unwrap_or_default();
    let status = status_from_code(code, json["message"].as_str().unwrap_or_default());

    // 成功时收集 Set-Cookie 与响应体里的 cookie 串，两者合并后写入存储。
    let mut cookies = BTreeMap::new();
    if status == QrLoginStatus::Success {
        for value in headers.get_all(reqwest::header::SET_COOKIE) {
            if let Ok(value) = value.to_str()
                && let Some(pair) = value.split(';').next()
                && let Some((name, value)) = pair.split_once('=')
            {
                cookies.insert(name.trim().to_string(), value.trim().to_string());
            }
        }
        if let Some(raw) = json["cookie"].as_str() {
            for pair in raw.split(';') {
                if let Some((name, value)) = pair.split_once('=') {
                    cookies.insert(name.trim().to_string(), value.trim().to_string());
                }
            }
        }
        if !cookies.is_empty() {
            session::save_cookies(&cookies).map_err(FetchError::Other)?;
        }
    }

    let mut result = QrLoginResult::new(status, message_for(status, &json));
    if status == QrLoginStatus::Success {
        // 登录成功即拉一次账号信息：昵称写进会话存储，设置页与登录成功页
        // 才能显示「谁登录了」。拉取失败不阻断登录——cookie 已保存，
        // 之后任何一次 refresh 都会补齐。
        if let Ok((name, _)) = fetch_account().await {
            result.user_name = name;
        }
    }
    Ok(result)
}

/// 把轮询返回的业务码映射成页面状态。
///
/// 关键点：**不是 200 系里所有非 803 的返回都等于「登录失败」**。网易云风控
/// 会在轮询接口上返回「请完成验证操作」「操作频繁」这类文案，此时二维码本身
/// 仍然有效；如果把它归成 `Failed`，二维码页面会直接切成错误态并停止轮询，
/// 用户只能重来。所以这类响应必须归成 `RiskControl`，由页面保持轮询。
fn status_from_code(code: i64, message: &str) -> QrLoginStatus {
    match code {
        800 => QrLoginStatus::Expired,
        801 => QrLoginStatus::Waiting,
        802 => QrLoginStatus::Scanned,
        803 => QrLoginStatus::Success,
        // 服务端暂时异常（5xx 业务码）：二维码还有效，继续轮询。
        500 | 502 | 503 => QrLoginStatus::ServerError,
        _ => {
            if message.contains("验证") || message.contains("频繁") || message.contains("风险")
            {
                QrLoginStatus::RiskControl
            } else {
                QrLoginStatus::Failed
            }
        }
    }
}

fn message_for(status: QrLoginStatus, json: &Value) -> String {
    let message = json["message"].as_str().unwrap_or_default().trim();
    if !message.is_empty() {
        return message.to_string();
    }
    match status {
        QrLoginStatus::Waiting => "等待扫码",
        QrLoginStatus::Scanned => "已扫码，请在手机上确认",
        QrLoginStatus::Success => "登录成功",
        QrLoginStatus::Expired => "二维码已过期",
        QrLoginStatus::Failed => "登录失败",
        QrLoginStatus::NetworkError => "网络错误，正在重试",
        QrLoginStatus::RiskControl => "触发验证/风控，正在重试",
        QrLoginStatus::ServerError => "服务端暂时异常，正在重试",
        QrLoginStatus::InvalidSession => "登录会话已失效",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_netease_qr_codes() {
        assert_eq!(status_from_code(800, ""), QrLoginStatus::Expired);
        assert_eq!(status_from_code(801, ""), QrLoginStatus::Waiting);
        assert_eq!(status_from_code(802, ""), QrLoginStatus::Scanned);
        assert_eq!(status_from_code(803, ""), QrLoginStatus::Success);
        assert_eq!(status_from_code(404, ""), QrLoginStatus::Failed);
        assert_eq!(status_from_code(500, ""), QrLoginStatus::ServerError);
    }

    #[test]
    fn risk_control_keeps_the_qr_code_alive() {
        // 风控文案必须归成 RiskControl：归成 Failed 会让二维码页面直接报错并
        // 停止轮询，而实际上二维码还有效。
        assert_eq!(
            status_from_code(882, "请完成验证操作"),
            QrLoginStatus::RiskControl
        );
        assert_eq!(
            status_from_code(405, "操作频繁，请稍候再试"),
            QrLoginStatus::RiskControl
        );
        assert_eq!(
            status_from_code(882, "设备存在风险"),
            QrLoginStatus::RiskControl
        );
        // 其它未知码仍然算失败。
        assert_eq!(status_from_code(882, "参数错误"), QrLoginStatus::Failed);
    }

    #[test]
    fn risk_control_message_is_shown_to_the_user() {
        let json = serde_json::json!({ "message": "请完成验证操作" });
        assert_eq!(
            message_for(QrLoginStatus::RiskControl, &json),
            "请完成验证操作"
        );
        assert_eq!(
            message_for(QrLoginStatus::RiskControl, &serde_json::json!({})),
            "触发验证/风控，正在重试"
        );
    }

    #[test]
    fn prefers_the_api_message() {
        let json = serde_json::json!({ "message": "二维码已失效" });
        assert_eq!(message_for(QrLoginStatus::Expired, &json), "二维码已失效");
        let empty = serde_json::json!({});
        assert_eq!(message_for(QrLoginStatus::Waiting, &empty), "等待扫码");
    }
}
