use std::time::Duration;

use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::{SearchError, SearchResult};
use regex::Regex;
use serde_json::Value;

use super::BiliSource;

const SEARCH_ENDPOINT: &str =
    "https://api.bilibili.com/x/web-interface/wbi/search/type";

pub async fn search(
    source: &BiliSource,
    keyword: &str,
    page: u32,
    limit: u32,
) -> Result<SearchResult, SearchError> {
    let json = source
        .signed_get(
            SEARCH_ENDPOINT,
            &[
                ("search_type", "video".to_string()),
                ("keyword", keyword.to_string()),
                ("page", page.max(1).to_string()),
                ("tids", "3".to_string()),
            ],
        )
        .await
        .map_err(SearchError::Network)?;
    if json["code"].as_i64() != Some(0) {
        return Err(SearchError::Api(format!(
            "哔哩哔哩搜索失败: {}",
            json["message"].as_str().unwrap_or("unknown error")
        )));
    }
    let data = &json["data"];
    let raw_items = data["result"]
        .as_array()
        .ok_or_else(|| SearchError::Parse("哔哩哔哩搜索结果为空".to_string()))?;
    let items = raw_items
        .iter()
        .filter_map(parse_song)
        .take(limit as usize)
        .collect::<Vec<_>>();
    let total = data["numResults"]
        .as_u64()
        .unwrap_or(items.len() as u64) as u32;
    Ok(SearchResult {
        has_more: page.saturating_mul(limit) < total && !items.is_empty(),
        total,
        items,
    })
}

fn parse_song(item: &Value) -> Option<SongInfo> {
    let bvid = item["bvid"].as_str()?.trim();
    if bvid.is_empty() {
        return None;
    }
    let title = strip_html(item["title"].as_str().unwrap_or_default());
    let mut song = SongInfo::new(
        bvid.to_string(),
        SourceId::Bili,
        title.clone(),
        item["author"].as_str().unwrap_or("哔哩哔哩用户").to_string(),
    );
    song.album_name = title;
    song.album_id = item["mid"].as_u64().map(|value| value.to_string()).unwrap_or_default();
    song.duration = parse_duration(item["duration"].as_str().unwrap_or_default());
    song.cover_url = item["pic"]
        .as_str()
        .filter(|value| !value.is_empty())
        .map(|value| {
            if value.starts_with("//") {
                format!("https:{value}")
            } else {
                value.to_string()
            }
        });
    song.extra.insert("bvid".to_string(), bvid.to_string());
    if let Some(cid) = item["cid"].as_u64() {
        song.extra.insert("cid".to_string(), cid.to_string());
    }
    if let Some(aid) = item["aid"].as_u64() {
        song.extra.insert("aid".to_string(), aid.to_string());
    }
    Some(song)
}

fn strip_html(value: &str) -> String {
    let value = Regex::new(r"<[^>]+>")
        .map(|regex| regex.replace_all(value, "").into_owned())
        .unwrap_or_else(|_| value.to_string());
    value
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

fn parse_duration(value: &str) -> Duration {
    let mut parts = value.split(':').rev();
    let seconds = parts.next().and_then(|value| value.parse::<u64>().ok()).unwrap_or(0);
    let minutes = parts.next().and_then(|value| value.parse::<u64>().ok()).unwrap_or(0);
    let hours = parts.next().and_then(|value| value.parse::<u64>().ok()).unwrap_or(0);
    Duration::from_secs(hours * 3600 + minutes * 60 + seconds)
}
