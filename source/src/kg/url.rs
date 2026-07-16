//! kg 播放 URL 获取
//!
//! 流程:
//! 1. POST http://gateway.kugou.com/v3/album_audio/audio 获取歌曲详情
//! 2. 从响应中提取 play_url

use std::time::{SystemTime, UNIX_EPOCH};

use lx_core::model::song::SongInfo;
use lx_core::model::source::Quality;
use lx_core::traits::source::{FetchError, SongUrl};

use super::super::http;

/// 根据音质选择对应 hash（优先高音质）
fn select_hash(song: &SongInfo, _quality: Quality) -> Option<String> {
    // 优先 SQFileHash > HQFileHash > FileHash
    song.extra
        .get("SQFileHash")
        .or_else(|| song.extra.get("HQFileHash"))
        .or_else(|| song.extra.get("FileHash"))
        .cloned()
}

pub async fn get_song_url(song: &SongInfo, quality: Quality) -> Result<SongUrl, FetchError> {
    let client = http::client();

    let hash = select_hash(song, quality).ok_or(FetchError::NotFound)?;

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

    let resp = client
        .post("http://gateway.kugou.com/v3/album_audio/audio")
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
        .send()
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

    let data = match &json["data"] {
        serde_json::Value::Array(arr) if !arr.is_empty() => &arr[0],
        _ => return Err(FetchError::NotFound),
    };

    let play_url = data["play_url"].as_str().unwrap_or("").to_string();

    if play_url.is_empty() {
        return Err(FetchError::NotFound);
    }

    let qualities: Vec<Quality> = song.qualities.iter().copied().collect();

    Ok(SongUrl {
        url: play_url,
        quality,
        duration: song.duration,
        cover_url: None,
        qualities,
    })
}
