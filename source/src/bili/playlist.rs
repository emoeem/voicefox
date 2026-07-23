//! 哔哩哔哩用户收藏夹：登录后可浏览收藏的视频列表。

use std::time::Duration;

use lx_core::model::playlist::Playlist;
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::FetchError;

use super::BiliSource;

const FAV_FOLDER_LIST: &str =
    "https://api.bilibili.com/x/v3/fav/folder/created/list-all";
const FAV_RESOURCE_LIST: &str =
    "https://api.bilibili.com/x/v3/fav/resource/list";

pub async fn get_playlists(
    source: &BiliSource,
    _tag_id: &str,
    _page: u32,
) -> Result<Vec<Playlist>, FetchError> {
    if !source.is_logged_in() {
        return Ok(vec![]);
    }
    let mid = source
        .session()
        .user_id
        .ok_or_else(|| FetchError::Other("获取哔哩哔哩用户 ID 失败".to_string()))?;
    let json = source
        .get_json(
            FAV_FOLDER_LIST,
            &[
                ("up_mid", mid),
                ("type", "2".to_string()),
            ],
            false,
        )
        .await
        .map_err(|e| FetchError::Network(e))?;
    if json["code"].as_i64() != Some(0) {
        return Err(FetchError::Other("获取哔哩哔哩收藏夹列表失败".to_string()));
    }
    let folders: Vec<Playlist> = json["data"]["list"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|f| {
            let id = f["id"].as_u64()?.to_string();
            let name = f["title"].as_str()?.to_string();
            Some(Playlist {
                id,
                name,
                source: SourceId::Bili,
                description: None,
                cover_url: f["cover"].as_str().map(|v| v.to_string()),
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
    page: u32,
) -> Result<Vec<SongInfo>, FetchError> {
    if !source.is_logged_in() {
        return Ok(vec![]);
    }
    let json = source
        .get_json(
            FAV_RESOURCE_LIST,
            &[
                ("media_id", playlist_id.to_string()),
                ("pn", page.max(1).to_string()),
                ("ps", "20".to_string()),
                ("type", "0".to_string()),
            ],
            false,
        )
        .await
        .map_err(|e| FetchError::Network(e))?;
    if json["code"].as_i64() != Some(0) {
        return Err(FetchError::Other("获取哔哩哔哩收藏夹内容失败".to_string()));
    }
    let songs: Vec<SongInfo> = json["data"]["medias"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|item| {
            let bvid = item["bvid"].as_str()?.to_string();
            if bvid.is_empty() {
                return None;
            }
            let mut song = SongInfo::new(
                bvid.clone(),
                SourceId::Bili,
                item["title"].as_str().unwrap_or_default().to_string(),
                item["upper"]["name"].as_str().unwrap_or("哔哩哔哩用户").to_string(),
            );
            song.album_name = song.name.clone();
            song.cover_url = item["cover"].as_str().map(|v| v.to_string());
            song.duration = Duration::from_secs(item["duration"].as_u64().unwrap_or(0));
            song.extra.insert("bvid".to_string(), bvid);
            if let Some(cid) = item["cid"].as_u64() {
                song.extra.insert("cid".to_string(), cid.to_string());
            }
            Some(song)
        })
        .collect();
    Ok(songs)
}
