//! 哔哩哔哩用户收藏夹：登录后可浏览收藏的视频列表。

use std::collections::HashSet;
use std::time::Duration;

use lx_core::model::playlist::Playlist;
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::FetchError;

use super::BiliSource;

const FAV_FOLDER_LIST: &str = "https://api.bilibili.com/x/v3/fav/folder/created/list-all";
const FAV_RESOURCE_LIST: &str = "https://api.bilibili.com/x/v3/fav/resource/list";

pub async fn get_playlists(
    source: &BiliSource,
    _tag_id: &str,
    _page: u32,
) -> Result<Vec<Playlist>, FetchError> {
    if !source.is_logged_in() {
        return Err(FetchError::Other("请先在设置页面登录哔哩哔哩".to_string()));
    }
    let mid = source
        .session()
        .user_id
        .ok_or_else(|| FetchError::Other("获取哔哩哔哩用户 ID 失败".to_string()))?;
    let json = source
        .get_json(
            FAV_FOLDER_LIST,
            &[("up_mid", mid), ("type", "2".to_string())],
            false,
        )
        .await
        .map_err(FetchError::Network)?;
    if json["code"].as_i64() != Some(0) {
        return Err(FetchError::Other(format!(
            "获取哔哩哔哩收藏夹列表失败: {}",
            json["message"].as_str().unwrap_or("unknown error")
        )));
    }
    let folders = json["data"]["list"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            let id = f["id"].as_u64()?.to_string();
            let name = f["title"].as_str()?.to_string();
            Some(Playlist {
                id,
                name,
                source: SourceId::Bili,
                description: None,
                cover_url: normalize_url(f["cover"].as_str()),
                song_count: f["media_count"].as_u64().unwrap_or(0) as u32,
                play_count: None,
            })
        })
        .collect();
    Ok(folders)
}

pub async fn get_playlist_detail(
    source: &BiliSource,
    playlist_id: &str,
    _page: u32,
) -> Result<Vec<SongInfo>, FetchError> {
    if !source.is_logged_in() {
        return Err(FetchError::Other("请先在设置页面登录哔哩哔哩".to_string()));
    }
    const PAGE_SIZE: u32 = 20;
    const MAX_PAGES: u32 = 100;

    let mut all_songs = Vec::new();
    let mut seen = HashSet::new();
    let mut current_page = 1u32;
    loop {
        let json = source
            .get_json(
                FAV_RESOURCE_LIST,
                &[
                    ("media_id", playlist_id.to_string()),
                    ("pn", current_page.to_string()),
                    ("ps", PAGE_SIZE.to_string()),
                    ("type", "0".to_string()),
                ],
                false,
            )
            .await
            .map_err(FetchError::Network)?;
        if json["code"].as_i64() != Some(0) {
            return Err(FetchError::Other(format!(
                "获取哔哩哔哩收藏夹内容失败: {}",
                json["message"].as_str().unwrap_or("unknown error")
            )));
        }
        let medias = json["data"]["medias"].as_array();
        let page_empty = medias.is_none_or(Vec::is_empty);
        if let Some(medias) = medias {
            for song in medias.iter().filter_map(parse_song) {
                if seen.insert(song.id.clone()) {
                    all_songs.push(song);
                }
            }
        }
        let has_more = json["data"]["has_more"].as_bool().unwrap_or(false);
        if !has_more || page_empty {
            break;
        }
        if current_page >= MAX_PAGES {
            return Err(FetchError::Other(format!(
                "哔哩哔哩收藏夹超过 {} 条，已停止加载",
                PAGE_SIZE * MAX_PAGES
            )));
        }
        current_page += 1;
    }

    Ok(all_songs)
}

fn parse_song(item: &serde_json::Value) -> Option<SongInfo> {
    let bvid = item["bvid"].as_str()?.trim();
    if bvid.is_empty() {
        return None;
    }
    let mut song = SongInfo::new(
        bvid.to_string(),
        SourceId::Bili,
        item["title"].as_str().unwrap_or_default().to_string(),
        item["upper"]["name"]
            .as_str()
            .unwrap_or("哔哩哔哩用户")
            .to_string(),
    );
    song.album_name = song.name.clone();
    song.cover_url = normalize_url(item["cover"].as_str());
    song.duration = Duration::from_secs(item["duration"].as_u64().unwrap_or(0));
    song.extra.insert("bvid".to_string(), bvid.to_string());
    if let Some(cid) = item["cid"]
        .as_str()
        .map(str::to_string)
        .or_else(|| item["cid"].as_u64().map(|value| value.to_string()))
        .filter(|value| !value.is_empty())
    {
        song.extra.insert("cid".to_string(), cid);
    }
    Some(song)
}

fn normalize_url(value: Option<&str>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty()).map(|value| {
        value
            .strip_prefix("//")
            .map_or_else(|| value.to_string(), |url| format!("https://{url}"))
    })
}
