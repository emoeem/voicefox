//! kg 搜索 API
//!
//! GET https://songsearch.kugou.com/song_search_v2
//! 参数: keyword, page, pagesize, platform=WebFilter, filter=2

use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Duration;

use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{SearchError, SearchResult};
use serde_json::Value;

use super::super::http;

/// 将文件大小值映射为音质（>0 则添加对应 Quality）
fn add_quality_if_positive(qualities: &mut BTreeSet<Quality>, size: i64, quality: Quality) {
    if size > 0 {
        qualities.insert(quality);
    }
}

/// 从 JSON Value 提取 i64（兼容字符串和数字类型）
fn json_i64(value: &Value) -> i64 {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        .unwrap_or(0)
}

fn value_string(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_i64().map(|value| value.to_string()))
        .or_else(|| value.as_u64().map(|value| value.to_string()))
        .unwrap_or_default()
}

fn parse_song(item: &Value) -> Option<SongInfo> {
    let song_id = value_string(&item["Audioid"]);
    if song_id.is_empty() {
        return None;
    }
    let name = item["SongName"].as_str().unwrap_or("").to_string();

    let singer = match &item["Singers"] {
        Value::Array(singers) => singers
            .iter()
            .filter_map(|singer| singer["name"].as_str())
            .collect::<Vec<_>>()
            .join("、"),
        _ => String::new(),
    };

    let album_name = item["AlbumName"].as_str().unwrap_or("").to_string();
    let album_id = value_string(&item["AlbumID"]);
    let duration_secs = json_i64(&item["Duration"]).max(json_i64(&item["_interval"])) as u64;

    let mut qualities = BTreeSet::new();
    add_quality_if_positive(&mut qualities, json_i64(&item["FileSize"]), Quality::Low128);
    add_quality_if_positive(
        &mut qualities,
        json_i64(&item["HQFileSize"]),
        Quality::High320,
    );
    add_quality_if_positive(&mut qualities, json_i64(&item["SQFileSize"]), Quality::Flac);
    add_quality_if_positive(
        &mut qualities,
        json_i64(&item["ResFileSize"]),
        Quality::Flac24,
    );

    let mut extra = HashMap::new();
    for (source_key, extra_key) in [
        ("FileHash", "FileHash"),
        ("HQFileHash", "HQFileHash"),
        ("SQFileHash", "SQFileHash"),
        ("ResFileHash", "ResFileHash"),
    ] {
        if let Some(hash) = item[source_key].as_str().filter(|hash| !hash.is_empty()) {
            extra.insert(extra_key.to_string(), hash.to_string());
        }
    }

    let mut song = SongInfo::new(song_id, SourceId::Kg, name, singer);
    song.album_name = album_name;
    song.album_id = album_id;
    song.duration = Duration::from_secs(duration_secs);
    song.qualities = qualities;
    song.extra = extra;
    Some(song)
}

pub async fn search(keyword: &str, page: u32, limit: u32) -> Result<SearchResult, SearchError> {
    let encoded_keyword = urlencoding::encode(keyword);
    let url = format!(
        "https://songsearch.kugou.com/song_search_v2?keyword={}&page={}&pagesize={}&userid=0&platform=WebFilter&filter=2&iscorrection=1&privilege_filter=0&area_code=1",
        encoded_keyword, page, limit
    );

    let client = http::client();
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| SearchError::Network(e.to_string()))?;

    let text = resp
        .text()
        .await
        .map_err(|e| SearchError::Network(e.to_string()))?;

    let json: Value = serde_json::from_str(&text).map_err(|e| SearchError::Parse(e.to_string()))?;

    // 检查错误码
    let error_code = json["error_code"].as_i64().unwrap_or(-1);
    if error_code != 0 {
        let err_msg = json["error"].as_str().unwrap_or("unknown error");
        return Err(SearchError::Api(format!("kg search error: {}", err_msg)));
    }

    let total = json["data"]["total"].as_u64().unwrap_or(0) as u32;

    let lists = match &json["data"]["lists"] {
        Value::Array(arr) => arr,
        _ => {
            return Ok(SearchResult {
                items: vec![],
                total: 0,
                has_more: false,
            });
        }
    };

    let mut items = Vec::with_capacity(lists.len());
    let mut seen = HashSet::new();
    for item in lists {
        for candidate in std::iter::once(item).chain(item["Grp"].as_array().into_iter().flatten()) {
            let key = format!(
                "{}:{}",
                value_string(&candidate["Audioid"]),
                candidate["FileHash"].as_str().unwrap_or_default()
            );
            if key == ":" || !seen.insert(key) {
                continue;
            }
            if let Some(song) = parse_song(candidate) {
                items.push(song);
            }
        }
    }

    // 判断是否还有更多结果
    let has_more = (page * limit) < total;

    Ok(SearchResult {
        items,
        total,
        has_more,
    })
}
