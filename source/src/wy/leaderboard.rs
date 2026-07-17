use lx_core::model::leaderboard::LeaderboardInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::{SearchError, SearchResult};
use serde_json::Value;

use crate::http;

pub async fn get_boards() -> Result<Vec<LeaderboardInfo>, SearchError> {
    let json: Value = http::client()
        .get("https://music.163.com/api/toplist")
        .send()
        .await
        .map_err(|error| SearchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| SearchError::Parse(error.to_string()))?;
    if json["code"].as_i64().unwrap_or(-1) != 200 {
        return Err(SearchError::Api("网易云榜单目录请求失败".to_string()));
    }
    let raw_boards = json["list"]
        .as_array()
        .ok_or_else(|| SearchError::Parse("网易云榜单目录为空".to_string()))?;
    let boards = raw_boards
        .iter()
        .filter_map(|board| {
            let id = value_string(&board["id"]);
            let name = board["name"].as_str()?.trim().to_string();
            if id.is_empty() || name.is_empty() {
                return None;
            }
            let mut info = LeaderboardInfo::new(id, name, SourceId::Wy);
            info.update = board["updateFrequency"]
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            Some(info)
        })
        .collect::<Vec<_>>();
    if boards.is_empty() {
        return Err(SearchError::Parse("网易云未返回可用榜单".to_string()));
    }
    Ok(boards)
}

pub async fn get_list(board_id: &str, page: u32, limit: u32) -> Result<SearchResult, SearchError> {
    let requested = page.saturating_mul(limit).max(limit);
    let url =
        format!("https://music.163.com/api/v3/playlist/detail?id={board_id}&n={requested}&s=0");
    let json: Value = http::client()
        .get(url)
        .send()
        .await
        .map_err(|error| SearchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| SearchError::Parse(error.to_string()))?;
    if json["code"].as_i64().unwrap_or(-1) != 200 {
        return Err(SearchError::Api("网易云榜单请求失败".to_string()));
    }
    let total = json["playlist"]["trackCount"].as_u64().unwrap_or_default() as u32;
    let raw_items = json["playlist"]["tracks"]
        .as_array()
        .ok_or_else(|| SearchError::Parse("网易云榜单歌曲列表为空".to_string()))?;
    let offset = page.saturating_sub(1).saturating_mul(limit) as usize;
    let items = raw_items
        .iter()
        .skip(offset)
        .take(limit as usize)
        .filter_map(super::search::parse_song)
        .collect::<Vec<_>>();
    Ok(SearchResult {
        items,
        total,
        has_more: page.saturating_mul(limit) < total,
    })
}

fn value_string(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_u64().map(|value| value.to_string()))
        .unwrap_or_default()
}
