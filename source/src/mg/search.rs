//! mg 搜索 API
//!
//! GET https://jadeite.migu.cn/music_search/v3/search/searchAll
//! 需要签名参数 sign 和 timestamp

use lx_core::traits::source::{SearchError, SearchResult};
use serde_json::Value;

use super::super::http;
use super::crypto::mg_sign;
use super::song::parse_song;

pub async fn search(keyword: &str, page: u32, limit: u32) -> Result<SearchResult, SearchError> {
    let encoded_keyword = urlencoding::encode(keyword);
    let (sign, timestamp) = mg_sign(keyword);

    let search_switch = r#"{"song":1,"album":0,"singer":0,"tagSong":1,"mvSong":0,"bestShow":1,"songlist":0,"lyricSong":0}"#;

    let url = format!(
        "https://jadeite.migu.cn/music_search/v3/search/searchAll?isCorrect=0&isCopyright=1&searchSwitch={}&pageSize={}&text={}&pageNo={}&sort=0&sid=USS",
        urlencoding::encode(search_switch),
        limit,
        encoded_keyword,
        page,
    );

    let client = http::client();
    let resp = client
        .get(&url)
        .header("uiVersion", "A_music_3.6.1")
        .header("deviceId", "963B7AA0D21511ED807EE5846EC87D20")
        .header("timestamp", timestamp)
        .header("sign", sign)
        .header("channel", "0146921")
        .header(
            "User-Agent",
            "Mozilla/5.0 (Linux; U; Android 11.0.0; zh-cn; MI 11 Build/OPR1.170623.032) AppleWebKit/534.30 (KHTML, like Gecko) Version/4.0 Mobile Safari/534.30",
        )
        .send()
        .await
        .map_err(|e| SearchError::Network(e.to_string()))?;

    let text = resp
        .text()
        .await
        .map_err(|e| SearchError::Network(e.to_string()))?;

    let json: Value = serde_json::from_str(&text).map_err(|e| SearchError::Parse(e.to_string()))?;

    // 检查错误码
    let code = json["code"].as_str().unwrap_or("");
    if code != "000000" {
        let err_msg = json["info"].as_str().unwrap_or("unknown error");
        return Err(SearchError::Api(format!("mg search error: {}", err_msg)));
    }

    let total = json["songResultData"]["totalCount"]
        .as_u64()
        .or_else(|| {
            json["songResultData"]["totalCount"]
                .as_str()
                .and_then(|value| value.parse().ok())
        })
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
    let mut seen = std::collections::HashSet::new();

    for group in result_list {
        let group_items = match group {
            Value::Array(arr) => arr,
            _ => continue,
        };

        for item in group_items {
            if let Some(song) = parse_song(item) {
                let key = format!(
                    "{}:{}",
                    song.id,
                    song.extra
                        .get("copyrightId")
                        .map(String::as_str)
                        .unwrap_or("")
                );
                if seen.insert(key) {
                    items.push(song);
                }
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
