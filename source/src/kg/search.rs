//! kg 搜索 API
//!
//! GET https://songsearch.kugou.com/song_search_v2
//! 参数: keyword, page, pagesize, platform=WebFilter, filter=2

use std::collections::BTreeSet;
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

/// 从 JSON Value 提取 i64（兼容数字类型）
fn json_i64(value: &Value) -> i64 {
    value.as_i64().unwrap_or(0)
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

    for item in lists {
        let song_id = item["Audioid"].as_str().unwrap_or("").to_string();
        let name = item["SongName"].as_str().unwrap_or("").to_string();

        // 歌手名：用 、 连接
        let singer = match &item["Singers"] {
            Value::Array(singers) => {
                let names: Vec<&str> = singers.iter().filter_map(|s| s["name"].as_str()).collect();
                names.join("、")
            }
            _ => String::new(),
        };

        let album_name = item["AlbumName"].as_str().unwrap_or("").to_string();
        let album_id = item["AlbumID"].as_str().unwrap_or("").to_string();

        // 时长（秒）→ Duration
        let duration_secs = json_i64(&item["Duration"]) as u64;
        let interval = json_i64(&item["_interval"]) as u64;

        // 音质列表——通过文件大小判断
        let mut qualities = BTreeSet::new();
        let file_size = json_i64(&item["FileSize"]);
        let hq_file_size = json_i64(&item["HQFileSize"]);
        let sq_file_size = json_i64(&item["SQFileSize"]);
        let res_file_size = json_i64(&item["ResFileSize"]);

        add_quality_if_positive(&mut qualities, file_size, Quality::Low128);
        add_quality_if_positive(&mut qualities, hq_file_size, Quality::High320);
        add_quality_if_positive(&mut qualities, sq_file_size, Quality::Flac);
        add_quality_if_positive(&mut qualities, res_file_size, Quality::Flac24);

        // extra: 存储各音质 hash
        use std::collections::HashMap;
        let mut extra = HashMap::new();
        if let Some(h) = item["FileHash"].as_str() {
            extra.insert("FileHash".to_string(), h.to_string());
        }
        if let Some(h) = item["HQFileHash"].as_str() {
            extra.insert("HQFileHash".to_string(), h.to_string());
        }
        if let Some(h) = item["SQFileHash"].as_str() {
            extra.insert("SQFileHash".to_string(), h.to_string());
        }
        if let Some(h) = item["ResFileHash"].as_str() {
            extra.insert("ResFileHash".to_string(), h.to_string());
        }

        let mut song = SongInfo::new(song_id, SourceId::Kg, name, singer);
        song.album_name = album_name;
        song.album_id = album_id;
        song.duration = Duration::from_secs(duration_secs.max(interval));
        song.qualities = qualities;
        song.extra = extra;

        items.push(song);
    }

    // 判断是否还有更多结果
    let has_more = (page * limit) < total;

    Ok(SearchResult {
        items,
        total,
        has_more,
    })
}
