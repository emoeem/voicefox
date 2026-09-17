//! 酷狗扫码登录。
//!
//! 流程对齐 music-lib 的 `kugou` lite 登录：
//! 1. 生成一组设备 cookie（GUID / MID / MAC / DEV）；
//! 2. `v2/qrcode` 申请二维码 key，二维码内容指向酷狗 H5 登录页；
//! 3. `v2/get_userinfo_qrcode` 轮询，成功后拿到 `token` 与 `userid`。
//!
//! 与 music-lib 的差异：这里不做 RSA 设备注册。那一步只影响多设备风控，
//! 失败在 music-lib 里也只是记一条 `register_error` 继续，而它会引入
//! RSA + 设备指纹一整套依赖，收益不成比例。

use std::collections::BTreeMap;

use lx_core::model::login::{QrLoginResult, QrLoginSession, QrLoginStatus};
use lx_core::model::source::SourceId;
use lx_core::traits::source::FetchError;
use md5::Digest;
use serde_json::Value;

use crate::http;
use crate::http::SendWithRetry;

use super::session;

/// 签名密钥（与 music-lib 保持一致）。
const SIGN_KEY: &str = "NVPh5oo715z5DIWAeQlhMDsWXXQV4hwt";
/// lite 客户端 appid / 版本号。
const LITE_APP_ID: &str = "3116";
const LITE_VER: &str = "11440";
const USER_AGENT: &str = "Android15-1070-11083-46-0-DiscoveryDRADProtocol-wifi";

const QR_CREATE_API: &str = "https://login-user.kugou.com/v2/qrcode";
const QR_CHECK_API: &str = "https://login-user.kugou.com/v2/get_userinfo_qrcode";

/// 生成一组设备 cookie，每次请求都带上，保证同一次登录内设备标识一致。
fn device_cookies() -> BTreeMap<String, String> {
    let guid = random_guid();
    let mut cookies = BTreeMap::new();
    cookies.insert("KUGOU_API_GUID".to_string(), guid.clone());
    cookies.insert("KUGOU_API_MID".to_string(), mid_from_guid(&guid));
    cookies.insert("KUGOU_API_MAC".to_string(), random_string(12));
    cookies.insert("KUGOU_API_DEV".to_string(), random_string(16));
    cookies
}

/// 32 位十六进制 GUID，按 UUID v4 的格式带连字符。
fn random_guid() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let bytes: [u8; 16] = rng.r#gen();
    let hex = hex::encode(bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// MID = GUID 的 MD5 转十进制（大整数）。
fn mid_from_guid(guid: &str) -> String {
    let digest = md5::Md5::digest(guid.as_bytes());
    // 逐字节做十进制大整数转换，避免引入大数库。
    let mut digits: Vec<u8> = vec![0];
    for byte in digest {
        let mut carry = u32::from(byte);
        for digit in digits.iter_mut() {
            let value = u32::from(*digit) * 256 + carry;
            *digit = (value % 10) as u8;
            carry = value / 10;
        }
        while carry > 0 {
            digits.push((carry % 10) as u8);
            carry /= 10;
        }
    }
    let mut value = digits
        .iter()
        .rev()
        .map(|digit| char::from(b'0' + *digit))
        .collect::<String>();
    while value.starts_with('0') && value.len() > 1 {
        value.remove(0);
    }
    if value.is_empty() {
        "0".to_string()
    } else {
        value
    }
}

fn random_string(length: usize) -> String {
    use rand::Rng;
    const CHARS: &[u8] = b"1234567890ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut rng = rand::thread_rng();
    (0..length)
        .map(|_| char::from(CHARS[rng.gen_range(0..CHARS.len())]))
        .collect()
}

/// 酷狗接口签名：参数按 key 升序拼 `k=v`，两端各接一次密钥后取 MD5。
fn sign(params: &BTreeMap<String, String>) -> String {
    let joined = params
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("");
    format!(
        "{:x}",
        md5::Md5::digest(format!("{SIGN_KEY}{joined}{SIGN_KEY}").as_bytes())
    )
}

/// 带设备信息的签名 GET（登录接口专用）。
async fn login_get(
    api: &str,
    params: &[(&str, &str)],
    device: &BTreeMap<String, String>,
) -> Result<Value, FetchError> {
    let clienttime = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
        .to_string();
    let mid = device
        .get("KUGOU_API_MID")
        .cloned()
        .unwrap_or_else(|| "-".to_string());
    let mut all: BTreeMap<String, String> = BTreeMap::new();
    all.insert("appid".to_string(), LITE_APP_ID.to_string());
    all.insert("clientver".to_string(), LITE_VER.to_string());
    all.insert("clienttime".to_string(), clienttime.clone());
    all.insert("dfid".to_string(), "-".to_string());
    all.insert("mid".to_string(), mid.clone());
    all.insert("uuid".to_string(), "-".to_string());
    for (key, value) in params {
        all.insert((*key).to_string(), (*value).to_string());
    }
    let signature = sign(&all);
    let mut query = all
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                urlencoding::encode(key),
                urlencoding::encode(value)
            )
        })
        .collect::<Vec<_>>();
    query.push(format!("signature={signature}"));
    let url = format!("{api}?{}", query.join("&"));

    let cookie = device
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("; ");
    http::client()
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("dfid", "-")
        .header("clienttime", clienttime)
        .header("mid", mid)
        .header("Cookie", cookie)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))
}

pub async fn create() -> Result<QrLoginSession, FetchError> {
    let device = device_cookies();
    let qr_text =
        format!("https://h5.kugou.com/apps/loginQRCode/html/index.html?appid={LITE_APP_ID}&");
    let json = login_get(
        QR_CREATE_API,
        &[
            ("appid", "1001"),
            ("type", "1"),
            ("plat", "4"),
            ("qrcode_txt", qr_text.as_str()),
            ("srcappid", "2919"),
        ],
        &device,
    )
    .await?;
    let key = json["data"]["qrcode"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| FetchError::Other("酷狗二维码生成失败".to_string()))?;
    Ok(QrLoginSession {
        source: SourceId::Kg,
        key: key.to_string(),
        url: format!("https://h5.kugou.com/apps/loginQRCode/html/index.html?qrcode={key}"),
        image_png: None,
        expires_in: 300,
    })
}

pub async fn check(key: &str) -> Result<QrLoginResult, FetchError> {
    let key = key.trim();
    if key.is_empty() {
        return Err(FetchError::Other("酷狗二维码 key 为空".to_string()));
    }
    let device = device_cookies();
    let json = login_get(
        QR_CHECK_API,
        &[
            ("plat", "4"),
            ("appid", LITE_APP_ID),
            ("srcappid", "2919"),
            ("qrcode", key),
        ],
        &device,
    )
    .await?;
    let status_code = json["data"]["status"].as_i64().unwrap_or_default();
    let status = status_from_code(status_code);

    let mut result = QrLoginResult::new(status, message_for(status, status_code, &json));
    if status == QrLoginStatus::Success {
        let token = json["data"]["token"].as_str().unwrap_or_default().trim();
        let user_id = json["data"]["userid"]
            .as_u64()
            .map(|value| value.to_string())
            .or_else(|| json["data"]["userid"].as_str().map(str::to_string))
            .unwrap_or_default();
        if token.is_empty() || user_id.is_empty() || user_id == "0" {
            result.status = QrLoginStatus::Failed;
            result.message = "酷狗登录成功但未返回 token 或 userid".to_string();
            return Ok(result);
        }
        session::save_login(&device, token, &user_id).map_err(FetchError::Other)?;
        let mut cookies = device;
        cookies.insert("token".to_string(), token.to_string());
        cookies.insert("userid".to_string(), user_id.clone());
        result.cookies = cookies;
        result.user_name = Some(user_id);
    }
    Ok(result)
}

fn status_from_code(code: i64) -> QrLoginStatus {
    match code {
        4 => QrLoginStatus::Success,
        2 | 3 => QrLoginStatus::Scanned,
        -1 | 5 | 6 => QrLoginStatus::Expired,
        0 | 1 => QrLoginStatus::Waiting,
        _ => QrLoginStatus::Failed,
    }
}

fn message_for(status: QrLoginStatus, code: i64, json: &Value) -> String {
    if let Some(error) = json["error"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return error.to_string();
    }
    match status {
        QrLoginStatus::Waiting => "等待扫码".to_string(),
        QrLoginStatus::Scanned => "已扫码，请在手机上确认".to_string(),
        QrLoginStatus::Success => "登录成功".to_string(),
        QrLoginStatus::Expired => "二维码已过期".to_string(),
        QrLoginStatus::Failed => format!("登录失败（status={code}）"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_kugou_qr_status_codes() {
        assert_eq!(status_from_code(4), QrLoginStatus::Success);
        assert_eq!(status_from_code(2), QrLoginStatus::Scanned);
        assert_eq!(status_from_code(3), QrLoginStatus::Scanned);
        assert_eq!(status_from_code(0), QrLoginStatus::Waiting);
        assert_eq!(status_from_code(1), QrLoginStatus::Waiting);
        assert_eq!(status_from_code(5), QrLoginStatus::Expired);
        assert_eq!(status_from_code(9), QrLoginStatus::Failed);
    }

    #[test]
    fn signature_is_deterministic_and_key_wrapped() {
        let mut params = BTreeMap::new();
        params.insert("b".to_string(), "2".to_string());
        params.insert("a".to_string(), "1".to_string());
        let signature = sign(&params);
        assert_eq!(signature.len(), 32);
        // 参数按 key 升序参与签名。
        let expected = format!(
            "{:x}",
            md5::Md5::digest(format!("{SIGN_KEY}a=1b=2{SIGN_KEY}").as_bytes())
        );
        assert_eq!(signature, expected);
    }

    #[test]
    fn guid_mid_is_a_decimal_number() {
        let guid = random_guid();
        assert_eq!(guid.len(), 36);
        let mid = mid_from_guid(&guid);
        assert!(mid.chars().all(|character| character.is_ascii_digit()));
        assert!(!mid.is_empty() && mid != "0");
        // 同一个 GUID 必须得到同一个 MID。
        assert_eq!(mid, mid_from_guid(&guid));
    }

    #[test]
    fn failed_login_reports_the_api_error() {
        let json = serde_json::json!({ "error": "二维码不存在" });
        assert_eq!(message_for(QrLoginStatus::Failed, 9, &json), "二维码不存在");
        assert_eq!(
            message_for(QrLoginStatus::Waiting, 0, &json),
            "二维码不存在"
        );
    }
}
