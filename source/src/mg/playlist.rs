use std::collections::HashSet;

use lx_core::model::playlist::Playlist;
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::FetchError;
use serde_json::Value;

use crate::http;

const USER_AGENT: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 13_2_3 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/13.0.3 Mobile/15E148 Safari/604.1";

pub async fn get_list(page: u32) -> Result<Vec<Playlist>, FetchError> {
    let url = format!(
        "https://app.c.nf.migu.cn/pc/bmw/page-data/playlist-square-recommend/v1.0?templateVersion=2&pageNo={page}"
    );
    let json: Value = request(url).await?;
    if json["code"].as_str() != Some("000000") {
        return Err(FetchError::Other("咪咕热门歌单请求失败".to_string()));
    }
    let mut playlists = Vec::new();
    let mut seen = HashSet::new();
    collect_playlists(&json["data"]["contents"], &mut seen, &mut playlists);
    Ok(playlists)
}

pub async fn get_detail(id: &str, page: u32) -> Result<Vec<SongInfo>, FetchError> {
    const PAGE_SIZE: u32 = 50;
    let mut current_page = page.max(1);
    let mut songs = Vec::new();
    loop {
        let url = format!(
            "https://app.c.nf.migu.cn/MIGUM3.0/resource/playlist/song/v2.0?pageNo={current_page}&pageSize={PAGE_SIZE}&playlistId={id}"
        );
        let json: Value = request(url).await?;
        if json["code"].as_str() != Some("000000") {
            return Err(FetchError::Other("咪咕歌单详情请求失败".to_string()));
        }
        let items = json["data"]["songList"]
            .as_array()
            .ok_or_else(|| FetchError::Parse("咪咕歌单歌曲列表为空".to_string()))?;
        songs.extend(items.iter().filter_map(super::song::parse_song));

        let total = value_u64(&json["data"]["totalCount"]).unwrap_or(items.len() as u64);
        if items.is_empty() || u64::from(current_page) * u64::from(PAGE_SIZE) >= total {
            break;
        }
        current_page = current_page.saturating_add(1);
    }
    Ok(songs)
}

async fn request(url: String) -> Result<Value, FetchError> {
    http::client()
        .get(url)
        .header("Referer", "https://m.music.migu.cn/")
        .header("channel", "0146921")
        .header("User-Agent", USER_AGENT)
        .send()
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))
}

fn collect_playlists(value: &Value, seen: &mut HashSet<String>, playlists: &mut Vec<Playlist>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_playlists(item, seen, playlists);
            }
        }
        Value::Object(map) => {
            if map.get("resType").and_then(Value::as_str) == Some("2021") {
                let id = map.get("resId").map(value_string).unwrap_or_default();
                let name = map
                    .get("txt")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if !id.is_empty() && !name.is_empty() && seen.insert(id.clone()) {
                    playlists.push(Playlist {
                        id,
                        name,
                        source: SourceId::Mg,
                        cover_url: map
                            .get("img")
                            .and_then(Value::as_str)
                            .filter(|value| !value.is_empty())
                            .map(str::to_string),
                        song_count: 0,
                        description: map
                            .get("txt2")
                            .and_then(Value::as_str)
                            .filter(|value| !value.is_empty())
                            .map(str::to_string),
                        play_count: None,
                    });
                }
            }
            for child in map.values() {
                if child.is_array() || child.is_object() {
                    collect_playlists(child, seen, playlists);
                }
            }
        }
        _ => {}
    }
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
