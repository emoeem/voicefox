//! QQ音乐搜索 API
//!
//! GET https://c.y.qq.com/soso/fcgi-bin/client_search_cp

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{SearchError, SearchResult};
use serde_json::Value;

use super::super::http;

/// 从 JSON Value 提取 i64
fn json_i64(value: &Value) -> i64 {
    value.as_i64().unwrap_or(0)
}

/// 将文件大小值映射为音质
fn add_quality_if_positive(qualities: &mut BTreeSet<Quality>, size: i64, quality: Quality) {
    if size > 0 {
        qualities.insert(quality);
    }
}

pub async fn search(keyword: &str, page: u32, limit: u32) -> Result<SearchResult, SearchError> {
    let url = format!(
        "https://c.y.qq.com/soso/fcgi-bin/client_search_cp?p={page}&n={limit}&w={}&format=json&new_json=1&cr=1&aggr=1&lossless=1",
        urlencoding::encode(keyword)
    );

    let client = http::client();
    let resp = client
        .get(&url)
        .header("Referer", "https://y.qq.com/")
        .header("User-Agent", "Mozilla/5.0")
        .send()
        .await
        .map_err(|e| SearchError::Network(e.to_string()))?;

    let text = resp
        .text()
        .await
        .map_err(|e| SearchError::Network(e.to_string()))?;

    let json: Value = serde_json::from_str(&text).map_err(|e| SearchError::Parse(e.to_string()))?;

    // 检查响应
    let code = json["code"].as_i64().unwrap_or(-1);
    if code != 0 {
        return Err(SearchError::Api(format!("tx search error: code={code}")));
    }

    let data = match &json["data"]["song"] {
        Value::Null => {
            return Ok(SearchResult {
                items: vec![],
                total: 0,
                has_more: false,
            });
        }
        d => d,
    };

    let total = data["totalnum"].as_u64().unwrap_or(0) as u32;
    let item_song = match &data["list"] {
        Value::Array(items) => items,
        _ => {
            return Ok(SearchResult {
                items: vec![],
                total,
                has_more: false,
            });
        }
    };

    let mut items = Vec::with_capacity(item_song.len());

    for item in item_song {
        if let Some(song) = parse_song(item) {
            items.push(song);
        }
    }

    let has_more = (page * limit) < total;

    Ok(SearchResult {
        items,
        total,
        has_more,
    })
}

pub(crate) fn parse_song(item: &Value) -> Option<SongInfo> {
    let mid = item["mid"].as_str().unwrap_or("").to_string();
    if mid.is_empty() {
        return None;
    }

    let name = item["title"].as_str().unwrap_or("").to_string();

    // 歌手名：用 、 连接
    let singer = match &item["singer"] {
        Value::Array(singers) => {
            let names: Vec<&str> = singers.iter().filter_map(|s| s["name"].as_str()).collect();
            names.join("、")
        }
        _ => String::new(),
    };

    let album_name = item["album"]
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let album_mid = item["album"]
        .get("mid")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // 封面 URL
    let cover_url = if !album_mid.is_empty() {
        Some(format!(
            "https://y.gtimg.cn/music/photo_new/T002R500x500M000{}.jpg",
            album_mid
        ))
    } else {
        None
    };

    // 时长 (interval 单位是秒)
    let duration_secs = json_i64(&item["interval"]) as u64;
    let duration = Duration::from_secs(duration_secs);

    // 音质
    let mut qualities = BTreeSet::new();
    let file = &item["file"];
    add_quality_if_positive(
        &mut qualities,
        json_i64(&file["size_128mp3"]),
        Quality::Low128,
    );
    add_quality_if_positive(
        &mut qualities,
        json_i64(&file["size_320mp3"]),
        Quality::High320,
    );
    add_quality_if_positive(&mut qualities, json_i64(&file["size_flac"]), Quality::Flac);
    add_quality_if_positive(
        &mut qualities,
        json_i64(&file["size_hires"]),
        Quality::Flac24,
    );

    // extra
    let mut extra = HashMap::new();
    if let Some(s) = item["id"].as_i64() {
        extra.insert("songId".to_string(), s.to_string());
    }
    if let Some(s) = item["file"]["media_mid"].as_str() {
        extra.insert("strMediaMid".to_string(), s.to_string());
    }

    let mut song = SongInfo::new(mid, SourceId::Tx, name, singer);
    song.album_name = album_name;
    song.album_id = album_mid;
    song.duration = duration;
    song.cover_url = cover_url;
    song.qualities = qualities;
    song.extra = extra;

    Some(song)
}
