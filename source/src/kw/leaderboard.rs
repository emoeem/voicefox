use std::collections::BTreeSet;
use std::time::Duration;

use lx_core::model::leaderboard::LeaderboardInfo;
use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{SearchError, SearchResult};
use serde_json::Value;

use crate::http;

pub async fn get_boards() -> Result<Vec<LeaderboardInfo>, SearchError> {
    let json: Value = http::client()
        .get("http://qukudata.kuwo.cn/q.k?op=query&cont=tree&node=2&pn=0&rn=1000&fmt=json&level=2")
        .send()
        .await
        .map_err(|error| SearchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| SearchError::Parse(error.to_string()))?;

    let children = json["child"]
        .as_array()
        .ok_or_else(|| SearchError::Parse("酷我榜单目录为空".to_string()))?;
    let boards = children
        .iter()
        .filter(|board| board["source"].as_str() == Some("1"))
        .filter_map(|board| {
            let id = value_string(&board["sourceid"]);
            let name = board["name"].as_str()?.trim().to_string();
            if id.is_empty() || name.is_empty() {
                return None;
            }
            let mut info = LeaderboardInfo::new(id, name, SourceId::Kw);
            info.update = board["info"]
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string);
            Some(info)
        })
        .collect::<Vec<_>>();

    if boards.is_empty() {
        return Err(SearchError::Parse("酷我未返回可用榜单".to_string()));
    }
    Ok(boards)
}

pub async fn get_list(board_id: &str, page: u32, limit: u32) -> Result<SearchResult, SearchError> {
    let url = format!(
        "http://kbangserver.kuwo.cn/ksong.s?from=pc&fmt=json&pn={}&rn={limit}&type=bang&data=content&id={board_id}&show_copyright_off=0&pcmp4=1&isbang=1",
        page.saturating_sub(1)
    );
    let json: Value = http::client()
        .get(url)
        .send()
        .await
        .map_err(|error| SearchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| SearchError::Parse(error.to_string()))?;

    let raw_items = json["musiclist"]
        .as_array()
        .ok_or_else(|| SearchError::Parse("酷我榜单歌曲列表为空".to_string()))?;
    let total = value_u32(&json["num"]).unwrap_or(raw_items.len() as u32);
    let items = raw_items.iter().filter_map(parse_song).collect();
    Ok(SearchResult {
        items,
        total,
        has_more: page.saturating_mul(limit) < total,
    })
}

fn parse_song(item: &Value) -> Option<SongInfo> {
    let id = value_string(&item["id"]);
    if id.is_empty() {
        return None;
    }
    let name = item["name"].as_str().unwrap_or_default().to_string();
    let singer = item["artist"]
        .as_str()
        .unwrap_or_default()
        .replace('&', "、");
    let mut song = SongInfo::new(id, SourceId::Kw, name, singer);
    song.album_name = item["album"].as_str().unwrap_or_default().to_string();
    song.album_id = value_string(&item["albumid"]);
    song.duration = Duration::from_secs(
        value_u64(&item["song_duration"])
            .or_else(|| value_u64(&item["duration"]))
            .unwrap_or_default(),
    );
    song.cover_url = item["pic"]
        .as_str()
        .filter(|url| !url.is_empty())
        .map(str::to_string);
    song.qualities = parse_qualities(item["formats"].as_str().unwrap_or_default());
    Some(song)
}

fn parse_qualities(formats: &str) -> BTreeSet<Quality> {
    let mut qualities = BTreeSet::new();
    for format in formats.split('|') {
        match format {
            "MP3128" | "WMA128" => {
                qualities.insert(Quality::Low128);
            }
            "MP3H" => {
                qualities.insert(Quality::High320);
            }
            "ALFLAC" => {
                qualities.insert(Quality::Flac);
            }
            "HIRFLAC" | "HIRES" => {
                qualities.insert(Quality::Flac24);
            }
            _ => {}
        }
    }
    if qualities.is_empty() {
        qualities.insert(Quality::Low128);
    }
    qualities
}

fn value_string(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_u64().map(|value| value.to_string()))
        .unwrap_or_default()
}

fn value_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn value_u32(value: &Value) -> Option<u32> {
    value_u64(value).map(|value| value as u32)
}
