//! QQ音乐搜索 API
//!
//! POST https://u.y.qq.com/cgi-bin/musics.fcg?sign={zzcSign}

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{SearchError, SearchResult};
use serde_json::Value;

use super::super::http;
use super::crypto;

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
    // 生成随机 searchid
    let searchid: u64 = rand::random();

    let body = serde_json::json!({
        "comm": {
            "ct": "11",
            "cv": "14090508",
            "v": "14090508",
            "tmeAppID": "qqmusic",
            "OpenUDID": "0",
            "tmeLoginType": "0"
        },
        "req": {
            "module": "music.search.SearchCgiService",
            "method": "DoSearchForQQMusicMobile",
            "param": {
                "search_type": 0,
                "searchid": searchid,
                "query": keyword,
                "page_num": page,
                "num_per_page": limit,
                "grp": 1
            }
        }
    });

    let body_str =
        serde_json::to_string(&body).map_err(|e| SearchError::Parse(e.to_string()))?;
    let sign = crypto::zzc_sign(&body_str);

    let url = format!(
        "https://u.y.qq.com/cgi-bin/musics.fcg?sign={}",
        sign
    );

    let client = http::client();
    let resp = client
        .post(&url)
        .header("User-Agent", "QQMusic 14090508(android 12)")
        .header("Content-Type", "application/json")
        .body(body_str)
        .send()
        .await
        .map_err(|e| SearchError::Network(e.to_string()))?;

    let text = resp
        .text()
        .await
        .map_err(|e| SearchError::Network(e.to_string()))?;

    let json: Value =
        serde_json::from_str(&text).map_err(|e| SearchError::Parse(e.to_string()))?;

    // 检查响应
    let req_code = json["req"]["code"].as_i64().unwrap_or(-1);
    if req_code != 0 {
        return Err(SearchError::Api(format!(
            "tx search error: req.code={}",
            req_code
        )));
    }

    let data = match &json["req"]["data"] {
        Value::Null => {
            return Ok(SearchResult {
                items: vec![],
                total: 0,
                has_more: false,
            });
        }
        d => d,
    };

    let total = data["meta"]["estimate_sum"]
        .as_u64()
        .unwrap_or(0) as u32;

    let item_song = match &data["body"]["item_song"] {
        Value::Array(arr) => arr,
        _ => {
            // 尝试 item 字段 (另一种格式)
            match &data["body"]["item"] {
                Value::Array(arr) => arr,
                _ => {
                    return Ok(SearchResult {
                        items: vec![],
                        total: 0,
                        has_more: false,
                    });
                }
            }
        }
    };

    let mut items = Vec::with_capacity(item_song.len());

    for item in item_song {
        let mid = item["mid"].as_str().unwrap_or("").to_string();
        if mid.is_empty() {
            continue;
        }

        let name = item["title"].as_str().unwrap_or("").to_string();

        // 歌手名：用 、 连接
        let singer = match &item["singer"] {
            Value::Array(singers) => {
                let names: Vec<&str> = singers
                    .iter()
                    .filter_map(|s| s["name"].as_str())
                    .collect();
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
        add_quality_if_positive(&mut qualities, json_i64(&file["size_128mp3"]), Quality::Low128);
        add_quality_if_positive(&mut qualities, json_i64(&file["size_320mp3"]), Quality::High320);
        add_quality_if_positive(&mut qualities, json_i64(&file["size_flac"]), Quality::Flac);
        add_quality_if_positive(&mut qualities, json_i64(&file["size_hires"]), Quality::Flac24);

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

        items.push(song);
    }

    let has_more = (page * limit) < total;

    Ok(SearchResult {
        items,
        total,
        has_more,
    })
}
