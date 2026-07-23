//! mg 播放 URL 获取
//!
//! 流程:
//! 1. POST https://c.musicapp.migu.cn/MIGUM2.0/v1.0/content/resourceinfo.do → 获取 newRateFormats
//! 2. 根据 quality 选择对应 formatType 的 URL
//! 3. 封面从 albumImgs[0] 取

use lx_core::model::song::SongInfo;
use lx_core::model::source::Quality;
use lx_core::traits::source::{FetchError, SongUrl};

use super::super::http;

/// Quality → mg formatType 映射
fn quality_to_format(quality: Quality) -> &'static str {
    match quality {
        Quality::Low128 => "PQ",
        Quality::High320 => "HQ",
        Quality::Flac => "SQ",
        Quality::Flac24 => "ZQ",
    }
}

pub async fn get_song_url(song: &SongInfo, quality: Quality) -> Result<SongUrl, FetchError> {
    let client = http::client();

    let copyright_id = song.extra.get("copyrightId").ok_or(FetchError::NotFound)?;

    let url = "https://c.musicapp.migu.cn/MIGUM2.0/v1.0/content/resourceinfo.do?resourceType=2";

    let resp = client
        .post(url)
        .header("User-Agent", "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Mobile Safari/537.36")
        .form(&[("resourceId", copyright_id.as_str())])
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

    let resource = match &json["resource"] {
        serde_json::Value::Array(arr) if !arr.is_empty() => &arr[0],
        _ => return Err(FetchError::NotFound),
    };

    // 封面从 albumImgs 取
    let cover_url = resource["albumImgs"]
        .as_array()
        .and_then(|imgs| imgs.first())
        .and_then(|img| img.as_str())
        .map(|s| s.to_string());

    // 查找匹配音质的 URL
    let target_format = quality_to_format(quality);
    let formats = match &resource["newRateFormats"] {
        serde_json::Value::Array(arr) => arr,
        _ => return Err(FetchError::NotFound),
    };

    let play_url = formats
        .iter()
        .find(|f| {
            f["formatType"].as_str() == Some(target_format)
                || (quality == Quality::Flac24 && f["formatType"].as_str() == Some("ZQ24"))
        })
        .and_then(|f| f["url"].as_str())
        .map(|s| s.to_string());

    let play_url = match play_url {
        Some(url) if !url.is_empty() => url,
        _ => {
            // fallback: 取任意可用格式
            formats
                .iter()
                .find_map(|f| f["url"].as_str())
                .map(|s| s.to_string())
                .ok_or(FetchError::NotFound)?
        }
    };

    let qualities: Vec<Quality> = song.qualities.iter().copied().collect();

    Ok(SongUrl {
        url: play_url,
        quality,
        duration: song.duration,
        cover_url,
        qualities,
        headers: vec![],
    })
}
