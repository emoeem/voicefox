use lx_core::model::playlist::Playlist;
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::FetchError;
use serde_json::Value;

use crate::http;

pub async fn get_list(page: u32) -> Result<Vec<Playlist>, FetchError> {
    let offset = 30 * page.saturating_sub(1);
    let url = format!(
        "https://music.163.com/api/playlist/list?cat={}&order=hot&limit=30&offset={offset}",
        urlencoding::encode("全部")
    );
    let json: Value = http::client()
        .get(url)
        .header("Referer", "https://music.163.com/")
        .send()
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))?;
    if json["code"].as_i64() != Some(200) {
        return Err(FetchError::Other("网易云热门歌单请求失败".to_string()));
    }
    let items = json["playlists"]
        .as_array()
        .ok_or_else(|| FetchError::Parse("网易云热门歌单列表为空".to_string()))?;
    Ok(items.iter().filter_map(parse_playlist).collect())
}

pub async fn get_detail(id: &str, page: u32) -> Result<Vec<SongInfo>, FetchError> {
    let requested = page.saturating_mul(1000).max(1000);
    let url = format!("https://music.163.com/api/v3/playlist/detail?id={id}&n={requested}&s=0");
    let json: Value = http::client()
        .get(url)
        .header("Referer", "https://music.163.com/")
        .send()
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))?;
    if json["code"].as_i64() != Some(200) {
        return Err(FetchError::Other("网易云歌单详情请求失败".to_string()));
    }
    let items = json["playlist"]["tracks"]
        .as_array()
        .ok_or_else(|| FetchError::Parse("网易云歌单歌曲列表为空".to_string()))?;
    Ok(items.iter().filter_map(super::search::parse_song).collect())
}

fn parse_playlist(item: &Value) -> Option<Playlist> {
    let id = value_string(&item["id"]);
    let name = item["name"].as_str()?.trim().to_string();
    if id.is_empty() || name.is_empty() {
        return None;
    }
    Some(Playlist {
        id,
        name,
        source: SourceId::Wy,
        cover_url: non_empty_string(&item["coverImgUrl"]),
        song_count: value_u64(&item["trackCount"]).unwrap_or_default() as u32,
        description: non_empty_string(&item["description"]),
        play_count: value_u64(&item["playCount"]),
    })
}

fn non_empty_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
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
