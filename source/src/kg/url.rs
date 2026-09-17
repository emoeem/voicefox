//! kg 播放 URL 获取
//!
//! 流程:
//! 1. POST http://gateway.kugou.com/v3/album_audio/audio 获取歌曲详情
//! 2. 从响应中提取 play_url

use crate::http::SendWithRetry;
use std::time::{SystemTime, UNIX_EPOCH};

use md5::Digest;

use lx_core::model::song::SongInfo;
use lx_core::model::source::Quality;
use lx_core::traits::source::{FetchError, SongUrl};

use super::super::http;

/// 根据请求音质选择对应的 hash 字段；请求字段缺失时按
/// SQFileHash > HQFileHash > FileHash 回退，并返回实际选中的音质，
/// 保证 SongUrl.quality 与真实下发的流一致。
fn select_hash(song: &SongInfo, quality: Quality) -> Option<(String, Quality)> {
    let field = match quality {
        Quality::Low128 => "FileHash",
        Quality::High320 => "HQFileHash",
        Quality::Flac => "SQFileHash",
        Quality::Flac24 => "ResFileHash",
    };
    if let Some(hash) = song.extra.get(field).filter(|hash| !hash.is_empty()) {
        return Some((hash.clone(), quality));
    }
    // 回退链（沿用旧逻辑）：SQ > HQ > File
    const FALLBACK: [(&str, Quality); 3] = [
        ("SQFileHash", Quality::Flac),
        ("HQFileHash", Quality::High320),
        ("FileHash", Quality::Low128),
    ];
    FALLBACK.iter().find_map(|(key, fallback_quality)| {
        song.extra
            .get(*key)
            .filter(|hash| !hash.is_empty())
            .map(|hash| (hash.clone(), *fallback_quality))
    })
}

pub async fn get_song_url(song: &SongInfo, quality: Quality) -> Result<SongUrl, FetchError> {
    let client = http::client();

    let (hash, actual_quality) = select_hash(song, quality).ok_or(FetchError::NotFound)?;

    // 酷狗现在**不给未登录用户返回任何播放地址**：免登录的移动端
    // `getSongInfo` 与 trackercdn 都只回空 URL（实测连免费儿歌也是），
    // 必须带 token / userid / 设备 MID 走 v5 接口。
    if let Ok(url) = fetch_url_v5(song, &hash, actual_quality).await {
        return Ok(url);
    }
    if !super::session::is_logged_in() {
        return Err(FetchError::Other(
            "酷狗播放需要登录：请在设置页扫码登录后重试".to_string(),
        ));
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| FetchError::Other(e.to_string()))?
        .as_millis();

    // 构造请求体
    let body = serde_json::json!({
        "area_code": "1",
        "data": [{"hash": hash}],
        "key": "OIlwieks28dk2k092lksi2UIkp",
        "appid": 1005,
        "clientver": 11451,
        "mid": "1",
        "dfid": "-",
        "clienttime": now
    });

    let resp = super::with_cookie(client.post("http://gateway.kugou.com/v3/album_audio/audio"))
        .header("KG-THash", "13a3164")
        .header("KG-RC", "1")
        .header("KG-Fake", "0")
        .header("KG-RF", "00869891")
        .header(
            "User-Agent",
            "Android712-AndroidPhone-11451-376-0-FeeCacheUpdate-wifi",
        )
        .header("x-router", "kmr.service.kugou.com")
        .json(&body)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    if !resp.status().is_success() {
        return Err(FetchError::NotFound);
    }

    let text = resp
        .text()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| FetchError::Parse(e.to_string()))?;

    // `data` 是**嵌套数组**：`[[{...}]]`。早期接口返回 `[{...}]`，
    // 两种形态都要兼容，否则会一路走到「play_url 为空」的 NotFound。
    let data = match &json["data"] {
        serde_json::Value::Array(arr) => {
            let first = arr.first().ok_or(FetchError::NotFound)?;
            match first {
                serde_json::Value::Array(inner) => inner.first().ok_or(FetchError::NotFound)?,
                other => other,
            }
        }
        _ => return Err(FetchError::NotFound),
    };

    let play_url = data["play_url"].as_str().unwrap_or("").to_string();

    if play_url.is_empty() {
        return Err(FetchError::NotFound);
    }

    let size_key = match actual_quality {
        Quality::Low128 => "FileSize",
        Quality::High320 => "HQFileSize",
        Quality::Flac => "SQFileSize",
        Quality::Flac24 => "ResFileSize",
    };
    let size = song
        .extra
        .get(size_key)
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&s| s > 0);

    let qualities: Vec<Quality> = song.qualities.iter().copied().collect();

    Ok(SongUrl {
        url: play_url,
        quality: actual_quality,
        duration: song.duration,
        cover_url: None,
        qualities,
        headers: vec![],
        size,
        size_is_advisory: false,
        md5: None,
        candidate_urls: vec![],
        max_chunk_size: 0,
    })
}

/// 登录态下的取址：对齐 music-lib 的 `fetchURLV5`。
///
/// 需要会话里的 `token` / `userid` / `KUGOU_API_MID`，签名规则是
/// `md5(hash + LITE_KEY + appid + mid + userid)`，再对全部参数做一层
/// 「两端夹密钥」的签名（与登录接口同一套 `signKugouAndroidParams`）。
async fn fetch_url_v5(
    song: &SongInfo,
    hash: &str,
    quality: Quality,
) -> Result<SongUrl, FetchError> {
    /// lite 客户端参数。
    const LITE_APP_ID: &str = "3116";
    const LITE_VER: &str = "11440";
    const LITE_KEY: &str = "185672dd44712f60bb1736df5a377e82";
    const LITE_SIGN: &str = "LnT6xpN3khm36zse0QzvmgTZ3waWdRSA";

    let session = super::session::snapshot();
    let token = session.cookie("token").ok_or(FetchError::NotFound)?;
    let user_id = session.user_id.clone().ok_or(FetchError::NotFound)?;
    let mid = session
        .cookie("KUGOU_API_MID")
        .ok_or(FetchError::NotFound)?;
    let dfid = session
        .cookie("dfid")
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "{:x}",
                md5::Md5::digest(
                    std::time::SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or_default()
                        .to_string()
                        .as_bytes()
                )
            )
            .to_uppercase()
        });
    let clienttime = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
        .to_string();
    let album_audio_id = song
        .extra
        .get("album_audio_id")
        .or_else(|| song.extra.get("audio_id"))
        .cloned()
        .unwrap_or_else(|| "0".to_string());
    let album_id = song
        .extra
        .get("album_id")
        .cloned()
        .unwrap_or_else(|| "0".to_string());
    let quality_flag = match quality {
        Quality::Flac24 => "super",
        Quality::Flac => "flac",
        Quality::High320 => "320",
        Quality::Low128 => "128",
    };
    let access_key = format!(
        "{:x}",
        md5::Md5::digest(format!("{hash}{LITE_KEY}{LITE_APP_ID}{mid}{user_id}").as_bytes())
    );

    let params: Vec<(&str, String)> = vec![
        ("dfid", dfid.clone()),
        ("mid", mid.to_string()),
        ("uuid", "-".to_string()),
        ("appid", LITE_APP_ID.to_string()),
        ("clientver", LITE_VER.to_string()),
        ("clienttime", clienttime.clone()),
        ("token", token.to_string()),
        ("userid", user_id),
        ("album_id", album_id),
        ("area_code", "1".to_string()),
        ("hash", hash.to_string()),
        ("ssa_flag", "is_fromtrack".to_string()),
        ("version", "11436".to_string()),
        ("page_id", "967177915".to_string()),
        ("quality", quality_flag.to_string()),
        ("album_audio_id", album_audio_id),
        ("behavior", "play".to_string()),
        ("pid", "411".to_string()),
        ("cmd", "26".to_string()),
        ("pidversion", "3001".to_string()),
        ("IsFreePart", "0".to_string()),
        ("ppage_id", "356753938,823673182,967485191".to_string()),
        ("cdnBackup", "1".to_string()),
        ("kcard", "0".to_string()),
        ("module", String::new()),
        ("key", access_key),
    ];
    // 参数按 key 升序拼 `k=v`，两端各接一次密钥后取 MD5。
    let mut sorted = params.clone();
    sorted.sort_by(|left, right| left.0.cmp(right.0));
    let joined = sorted
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("");
    let signature = format!(
        "{:x}",
        md5::Md5::digest(format!("{LITE_SIGN}{joined}{LITE_SIGN}").as_bytes())
    );
    let query = params
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                urlencoding::encode(key),
                urlencoding::encode(value)
            )
        })
        .chain(std::iter::once(format!("signature={signature}")))
        .collect::<Vec<_>>()
        .join("&");
    let url = format!("https://gateway.kugou.com/v5/url?{query}");

    let json: serde_json::Value = super::with_cookie(
        http::client()
            .get(url)
            .header(
                "User-Agent",
                "Android15-1070-11083-46-0-DiscoveryDRADProtocol-wifi",
            )
            .header("x-router", "trackercdn.kugou.com")
            .header("dfid", dfid)
            .header("clienttime", clienttime)
            .header("mid", mid)
            .header("kg-rc", "1")
            .header("kg-thash", "5d816a0"),
    )
    .send_with_retry(crate::http::RETRY_ATTEMPTS)
    .await
    .map_err(|error| FetchError::Network(error.to_string()))?
    .json()
    .await
    .map_err(|error| FetchError::Parse(error.to_string()))?;

    let url = pick_response_url(&json).ok_or(FetchError::NotFound)?;
    let size = json["fileSize"]
        .as_u64()
        .or_else(|| json["data"]["fileSize"].as_u64())
        .filter(|size| *size > 0);
    Ok(SongUrl {
        url,
        quality,
        duration: song.duration,
        cover_url: song.cover_url.clone(),
        qualities: song.qualities.iter().copied().collect(),
        headers: Vec::new(),
        size,
        size_is_advisory: true,
        md5: None,
        candidate_urls: Vec::new(),
        max_chunk_size: 0,
    })
}

/// 从 v5 响应里挑播放地址：顶层、`data` 下、以及按档位分组的 `flac/high/320/...`。
fn pick_response_url(json: &serde_json::Value) -> Option<String> {
    fn text(value: &serde_json::Value) -> Option<String> {
        let raw = value.as_str()?.trim();
        (!raw.is_empty()).then(|| raw.replace("\\/", "/"))
    }
    if let Some(url) = text(&json["url"]).or_else(|| text(&json["backup_url"])) {
        return Some(url);
    }
    let data = &json["data"];
    if let Some(url) = text(&data["url"]).or_else(|| text(&data["backup_url"])) {
        return Some(url);
    }
    for key in ["flac", "high", "320", "128", "super"] {
        if let Some(url) = text(&data[key]["url"]).or_else(|| text(&data[key]["backup_url"])) {
            return Some(url);
        }
    }
    None
}
