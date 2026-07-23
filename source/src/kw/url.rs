//! kw 播放 URL 获取
//!
//! 流程:
//! 1. 调用 musicInfo API 获取封面
//! 2. 调用 url API 获取播放地址

use std::time::{SystemTime, UNIX_EPOCH};

use lx_core::model::song::SongInfo;
use lx_core::model::source::Quality;
use lx_core::traits::source::{FetchError, SongUrl};

use super::super::http;

/// 根据音质选择对应的 bitrate 参数值
fn quality_to_bitrate(quality: Quality) -> u32 {
    match quality {
        Quality::Flac24 => 4000,
        Quality::Flac => 2000,
        Quality::High320 => 320,
        Quality::Low128 => 128,
    }
}

/// 封面 fallback URL
fn fallback_cover_url(song_id: &str) -> String {
    format!(
        "http://artistpicserver.kuwo.cn/pic.web?corp=kuwo&type=rid_pic&pictype=500&size=500&rid={}",
        song_id
    )
}

/// 通过 musicInfo API 获取封面图片 URL
async fn fetch_cover_url(client: &reqwest::Client, song_id: &str) -> Option<String> {
    let url = format!("http://www.kuwo.cn/api/www/music/musicInfo?mid={}", song_id);

    let resp = client
        .get(&url)
        .header("Referer", "http://www.kuwo.cn/")
        .header("csrf", song_id)
        .header("Cookie", format!("kw_token={}", song_id))
        .send()
        .await
        .ok()?;

    let text = resp.text().await.ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;

    if json["code"].as_i64() != Some(200) {
        return None;
    }

    json["data"]["pic"].as_str().map(|s| s.to_string())
}

pub async fn get_song_url(song: &SongInfo, quality: Quality) -> Result<SongUrl, FetchError> {
    let client = http::client();
    let song_id = &song.id;

    // Step 1: 获取封面（优先用 musicInfo，失败用 fallback）
    let cover_url = fetch_cover_url(&client, song_id)
        .await
        .unwrap_or_else(|| fallback_cover_url(song_id));

    // Step 2: 构造播放 URL 请求
    let bitrate = quality_to_bitrate(quality);

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| FetchError::Other(e.to_string()))?
        .as_millis();

    // 随机 10 位 reqId
    use rand::Rng;
    let req_id: u64 = rand::thread_rng().gen_range(1_000_000_000..10_000_000_000);

    let url = format!(
        "http://www.kuwo.cn/url?format=mp3&rid={}&response=url&type=convert_url3&br={}k&from=web&t={}&reqId={}",
        song_id, bitrate, timestamp, req_id
    );

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    if !resp.status().is_success() {
        return Err(FetchError::NotFound);
    }

    let url_text = resp
        .text()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?
        .trim()
        .to_string();

    if url_text.is_empty() {
        return Err(FetchError::NotFound);
    }

    // 转换 BTreeSet → Vec
    let qualities: Vec<Quality> = song.qualities.iter().copied().collect();

    Ok(SongUrl {
        url: url_text,
        quality,
        duration: song.duration,
        cover_url: Some(cover_url),
        qualities,
        headers: vec![],
    })
}
