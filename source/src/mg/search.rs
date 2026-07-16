//! mg 搜索 API
//!
//! GET https://jadeite.migu.cn/music_search/v3/search/searchAll
//! 需要签名参数 sign 和 timestamp

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{SearchError, SearchResult};
use serde_json::Value;

use super::crypto::mg_sign;
use super::super::http;

/// 将 mg formatType 映射为 Quality
fn format_to_quality(format_type: &str) -> Option<Quality> {
    match format_type {
        "PQ" => Some(Quality::Low128),
        "HQ" => Some(Quality::High320),
        "SQ" => Some(Quality::Flac),
        "ZQ" | "ZQ24" => Some(Quality::Flac24),
        _ => None,
    }
}

pub async fn search(keyword: &str, page: u32, limit: u32) -> Result<SearchResult, SearchError> {
    let encoded_keyword = urlencoding::encode(keyword);
    let (sign, timestamp) = mg_sign(keyword);

    let search_switch = r#"{"song":1,"album":0,"singer":0,"tagSong":1,"mvSong":0,"bestShow":1,"songlist":0,"lyricSong":0}"#;

    let url = format!(
        "https://jadeite.migu.cn/music_search/v3/search/searchAll?isCorrect=0&isCopyright=1&searchSwitch={}&pageSize={}&text={}&pageNo={}&sort=0&sid=USS&timestamp={}&sign={}",
        urlencoding::encode(search_switch),
        limit,
        encoded_keyword,
        page,
        timestamp,
        sign
    );

    let client = http::client();
    let resp = client
        .get(&url)
        .header("uiVersion", "A_music_3.6.1")
        .header("deviceId", "963B7AA0D21511ED807EE5846EC87D20")
        .header("channel", "0146921")
        .send()
        .await
        .map_err(|e| SearchError::Network(e.to_string()))?;

    let text = resp
        .text()
        .await
        .map_err(|e| SearchError::Network(e.to_string()))?;

    let json: Value =
        serde_json::from_str(&text).map_err(|e| SearchError::Parse(e.to_string()))?;

    // 检查错误码
    let code = json["code"].as_str().unwrap_or("");
    if code != "000000" {
        let err_msg = json["info"].as_str().unwrap_or("unknown error");
        return Err(SearchError::Api(format!("mg search error: {}", err_msg)));
    }

    let total = json["songResultData"]["totalCount"]
        .as_u64()
        .unwrap_or(0) as u32;

    // resultList 是二维数组，取所有 group 中的项目合并
    let result_list = match &json["songResultData"]["resultList"] {
        Value::Array(arr) => arr,
        _ => {
            return Ok(SearchResult {
                items: vec![],
                total: 0,
                has_more: false,
            });
        }
    };

    let mut items = Vec::new();

    for group in result_list {
        let group_items = match group {
            Value::Array(arr) => arr,
            _ => continue,
        };

        for item in group_items {
            let song_id = item["songId"].as_str().unwrap_or("").to_string();
            let copyright_id = item["copyrightId"].as_str().unwrap_or("").to_string();
            let name = item["name"].as_str().unwrap_or("").to_string();

            // 歌手名：用 、 连接
            let singer = match &item["singerList"] {
                Value::Array(singers) => {
                    let names: Vec<&str> = singers
                        .iter()
                        .filter_map(|s| s["name"].as_str())
                        .collect();
                    names.join("、")
                }
                _ => String::new(),
            };

            let album_name = item["album"].as_str().unwrap_or("").to_string();
            let album_id = item["albumId"].as_str().unwrap_or("").to_string();

            // 时长（秒）→ Duration
            let duration_secs = item["duration"].as_u64().unwrap_or(0);

            // 封面：优先 img3 > img2 > img1
            let cover_url = item["img3"]
                .as_str()
                .or_else(|| item["img2"].as_str())
                .or_else(|| item["img1"].as_str())
                .map(|s| s.to_string());

            // 歌词 URL 存入 extra
            let mut extra = HashMap::new();
            if let Some(url) = item["lrcUrl"].as_str() {
                extra.insert("lrcUrl".to_string(), url.to_string());
            }
            if let Some(url) = item["mrcurl"].as_str() {
                extra.insert("mrcurl".to_string(), url.to_string());
            }
            if let Some(url) = item["trcUrl"].as_str() {
                extra.insert("trcUrl".to_string(), url.to_string());
            }
            extra.insert("copyrightId".to_string(), copyright_id);

            // 音质列表：从 audioFormats 提取
            let mut qualities = BTreeSet::new();
            if let Value::Array(formats) = &item["audioFormats"] {
                for fmt in formats {
                    let format_type = fmt["formatType"].as_str().unwrap_or("");
                    if let Some(q) = format_to_quality(format_type) {
                        qualities.insert(q);
                    }
                }
            }

            let mut song = SongInfo::new(song_id, SourceId::Mg, name, singer);
            song.album_name = album_name;
            song.album_id = album_id;
            song.duration = Duration::from_secs(duration_secs);
            song.cover_url = cover_url;
            song.qualities = qualities;
            song.extra = extra;

            items.push(song);
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
