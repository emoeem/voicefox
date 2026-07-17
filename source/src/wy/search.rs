//! 网易云音乐搜索 API
//!
//! POST https://interface.music.163.com/eapi/batch
//! 使用 eapi 加密，form-urlencoded body

use std::collections::BTreeSet;
use std::time::Duration;

use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{SearchError, SearchResult};
use serde_json::Value;

use super::super::http;
use super::crypto;

/// 将文件大小值映射为音质（>0 则添加对应 Quality）
fn add_quality_if_positive(qualities: &mut BTreeSet<Quality>, size: i64, quality: Quality) {
    if size > 0 {
        qualities.insert(quality);
    }
}

/// 从 JSON Value 提取 i64（兼容数字类型）
fn json_i64(value: &Value) -> i64 {
    value.as_i64().unwrap_or(0)
}

pub async fn search(keyword: &str, page: u32, limit: u32) -> Result<SearchResult, SearchError> {
    let url = "/api/search/song/list/page";

    let mut data = serde_json::json!({
        "keyword": keyword,
        "needCorrect": "1",
        "channel": "typing",
        "offset": limit * (page.saturating_sub(1)),
        "scene": "normal",
        "limit": limit,
    });

    if page == 1 {
        data["total"] = serde_json::Value::Bool(true);
    }

    let encrypted = crypto::eapi(url, &data);

    let client = http::client();
    let resp = client
        .post("https://interface.music.163.com/eapi/batch")
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
        )
        .header("origin", "https://music.163.com")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!("params={}", encrypted))
        .send()
        .await
        .map_err(|e| SearchError::Network(e.to_string()))?;

    let text = resp
        .text()
        .await
        .map_err(|e| SearchError::Network(e.to_string()))?;

    let json: Value = serde_json::from_str(&text).map_err(|e| SearchError::Parse(e.to_string()))?;

    // 检查响应码 (code 或 result 两种格式)
    let code = json["code"].as_i64().unwrap_or(0);
    if code != 200 {
        let msg = json["message"].as_str().unwrap_or("unknown error");
        return Err(SearchError::Api(format!(
            "wy search error (code={}): {}",
            code, msg
        )));
    }

    // 尝试 data.resources 或 result.songs 等路径
    let result_val = json.get("data").or_else(|| json.get("result"));

    let total = result_val
        .and_then(|r| r["totalCount"].as_u64())
        .unwrap_or(0) as u32;

    let resources = result_val.and_then(|r| r["resources"].as_array());

    // 获取歌曲列表（兼容 resources / result.songs 两种格式）
    let resources_arr = resources.or_else(|| result_val.and_then(|r| r["songs"].as_array()));

    let songs = match resources_arr {
        Some(arr) => arr,
        None => {
            return Ok(SearchResult {
                items: vec![],
                total,
                has_more: false,
            });
        }
    };

    let mut items = Vec::with_capacity(songs.len());

    for resource in songs {
        if let Some(song) = parse_song(resource) {
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

pub(crate) fn parse_song(resource: &Value) -> Option<SongInfo> {
    // 兼容新版 resources、旧版 content/item 以及直接歌曲对象。
    let item = resource
        .pointer("/baseInfo/simpleSongData")
        .or_else(|| resource.get("content"))
        .or_else(|| resource.get("item"))
        .unwrap_or(resource);

    let song_id = item["id"].as_i64().unwrap_or(0).to_string();
    if song_id.is_empty() || song_id == "0" {
        return None;
    }

    let name = item["name"].as_str().unwrap_or("").to_string();

    // 歌手名：用 、 连接
    let singer = match &item["ar"] {
        Value::Array(artists) => {
            let names: Vec<&str> = artists.iter().filter_map(|a| a["name"].as_str()).collect();
            names.join("、")
        }
        _ => String::new(),
    };

    let album_name = item["al"]
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let album_id = item["al"]
        .get("id")
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .map(|id| id.to_string())
        .unwrap_or_default();

    // 封面
    let cover_url = item["al"]
        .get("picUrl")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // 时长 (dt 单位是毫秒)
    let duration_ms = json_i64(&item["dt"]) as u64;
    let duration = Duration::from_millis(duration_ms);

    // 音质
    let mut qualities = BTreeSet::new();
    qualities.insert(Quality::Low128); // 默认至少 128k

    add_quality_if_positive(
        &mut qualities,
        json_i64(&item["h"]["size"]),
        Quality::High320,
    );
    add_quality_if_positive(&mut qualities, json_i64(&item["sq"]["size"]), Quality::Flac);
    add_quality_if_positive(
        &mut qualities,
        json_i64(&item["hr"]["size"]),
        Quality::Flac24,
    );

    // privilege.maxBrLevel == "hires" 也视为 Flac24
    if item["privilege"].get("maxBrLevel").and_then(|v| v.as_str()) == Some("hires") {
        qualities.insert(Quality::Flac24);
    }

    let mut song = SongInfo::new(song_id, SourceId::Wy, name, singer);
    song.album_name = album_name;
    song.album_id = album_id;
    song.duration = duration;
    song.cover_url = cover_url;
    song.qualities = qualities;

    Some(song)
}
