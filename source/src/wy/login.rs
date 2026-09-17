//! 网易云扫码登录。
//!
//! - 生成：POST `/api/login/qrcode/unikey`（表单 `type=3`）拿到 `unikey`；
//! - 轮询：POST `/api/login/qrcode/client/login`（表单 `key` + `type=3`），
//!   状态码 800 过期 / 801 等待 / 802 已扫码 / 803 成功，成功后响应体与
//!   `Set-Cookie` 里都带登录票据，两者都收下来。

use std::collections::BTreeMap;

use lx_core::model::login::{QrLoginResult, QrLoginSession, QrLoginStatus};
use lx_core::model::source::SourceId;
use lx_core::traits::source::FetchError;
use serde_json::Value;

use crate::http;
use crate::http::SendWithRetry;

use super::session;

const QR_KEY_API: &str = "https://interface.music.163.com/api/login/qrcode/unikey";
const QR_CHECK_API: &str = "https://interface.music.163.com/api/login/qrcode/client/login";
const REFERER: &str = "https://music.163.com/";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; WOW64) AppleWebKit/537.36 (KHTML, like Gecko) Safari/537.36 Chrome/91.0.4472.164 NeteaseMusicDesktop/3.0.18.203152";

pub async fn create() -> Result<QrLoginSession, FetchError> {
    let response = http::client()
        .post(QR_KEY_API)
        .header("User-Agent", USER_AGENT)
        .header("Referer", REFERER)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body("type=3")
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?;
    let json: Value = response
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))?;
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
    let response = http::client()
        .post(QR_CHECK_API)
        .header("User-Agent", USER_AGENT)
        .header("Referer", REFERER)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!("key={key}&type=3"))
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?;
    let headers = response.headers().clone();
    let json: Value = response
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))?;
    let code = json["code"].as_i64().unwrap_or_default();
    let status = status_from_code(code);

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
    result.cookies = cookies;
    Ok(result)
}

fn status_from_code(code: i64) -> QrLoginStatus {
    match code {
        800 => QrLoginStatus::Expired,
        801 => QrLoginStatus::Waiting,
        802 => QrLoginStatus::Scanned,
        803 => QrLoginStatus::Success,
        _ => QrLoginStatus::Failed,
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
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_netease_qr_codes() {
        assert_eq!(status_from_code(800), QrLoginStatus::Expired);
        assert_eq!(status_from_code(801), QrLoginStatus::Waiting);
        assert_eq!(status_from_code(802), QrLoginStatus::Scanned);
        assert_eq!(status_from_code(803), QrLoginStatus::Success);
        assert_eq!(status_from_code(404), QrLoginStatus::Failed);
    }

    #[test]
    fn prefers_the_api_message() {
        let json = serde_json::json!({ "message": "二维码已失效" });
        assert_eq!(message_for(QrLoginStatus::Expired, &json), "二维码已失效");
        let empty = serde_json::json!({});
        assert_eq!(message_for(QrLoginStatus::Waiting, &empty), "等待扫码");
    }
}
