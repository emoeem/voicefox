use std::collections::HashSet;

use lx_core::model::leaderboard::LeaderboardInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::{SearchError, SearchResult};
use serde_json::Value;

use crate::http;

const USER_AGENT: &str = "Mozilla/5.0 (Linux; Android 5.1.1; Nexus 6 Build/LYZ28E) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/59.0.3071.115 Mobile Safari/537.36";

pub async fn get_boards() -> Result<Vec<LeaderboardInfo>, SearchError> {
    let json: Value = http::client()
        .get("https://app.c.nf.migu.cn/pc/bmw/rank/rank-index/v1.0")
        .header("Referer", "https://app.c.nf.migu.cn/")
        .header("channel", "0146921")
        .header("User-Agent", USER_AGENT)
        .send()
        .await
        .map_err(|error| SearchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| SearchError::Parse(error.to_string()))?;
    if json["code"].as_str() != Some("000000") {
        return Err(SearchError::Api(
            json["info"]
                .as_str()
                .unwrap_or("咪咕榜单目录请求失败")
                .to_string(),
        ));
    }

    let mut boards = Vec::new();
    let mut seen = HashSet::new();
    collect_boards(&json["data"]["contents"], None, &mut seen, &mut boards);
    if boards.is_empty() {
        return Err(SearchError::Parse("咪咕未返回可用榜单".to_string()));
    }
    Ok(boards)
}

pub async fn get_list(board_id: &str, page: u32, limit: u32) -> Result<SearchResult, SearchError> {
    let url = format!(
        "https://app.c.nf.migu.cn/MIGUM2.0/v1.0/content/querycontentbyId.do?columnId={board_id}&needAll=0"
    );
    let json: Value = http::client()
        .get(url)
        .header("Referer", "https://app.c.nf.migu.cn/")
        .header("channel", "0146921")
        .header("User-Agent", USER_AGENT)
        .send()
        .await
        .map_err(|error| SearchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| SearchError::Parse(error.to_string()))?;
    if json["code"].as_str() != Some("000000") {
        return Err(SearchError::Api(
            json["info"]
                .as_str()
                .unwrap_or("咪咕榜单请求失败")
                .to_string(),
        ));
    }

    let raw_items = json["columnInfo"]["contents"]
        .as_array()
        .ok_or_else(|| SearchError::Parse("咪咕榜单歌曲列表为空".to_string()))?;
    let total = json["columnInfo"]["contentsCount"]
        .as_u64()
        .unwrap_or(raw_items.len() as u64) as u32;
    let offset = page.saturating_sub(1).saturating_mul(limit) as usize;
    let items = raw_items
        .iter()
        .skip(offset)
        .take(limit as usize)
        .filter_map(|item| super::song::parse_song(&item["objectInfo"]))
        .collect::<Vec<_>>();
    Ok(SearchResult {
        items,
        total,
        has_more: page.saturating_mul(limit) < total,
    })
}

fn collect_boards(
    value: &Value,
    inherited_update: Option<&str>,
    seen: &mut HashSet<String>,
    boards: &mut Vec<LeaderboardInfo>,
) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_boards(item, inherited_update, seen, boards);
            }
        }
        Value::Object(map) => {
            let own_update = map
                .get("desc")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .or(inherited_update);
            if let (Some(id), Some(name)) = (
                map.get("rankId").and_then(Value::as_str),
                map.get("rankName").and_then(Value::as_str),
            ) && seen.insert(id.to_string())
            {
                let mut info = LeaderboardInfo::new(id.to_string(), name.to_string(), SourceId::Mg);
                info.update = own_update.map(str::to_string);
                boards.push(info);
            }
            for child in map.values() {
                if child.is_array() || child.is_object() {
                    collect_boards(child, own_update, seen, boards);
                }
            }
        }
        _ => {}
    }
}
