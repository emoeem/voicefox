//! 千千音乐歌单：搜索、分类目录、分类歌单、详情与元数据。

use lx_core::model::playlist::{Playlist, PlaylistCategory};
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::{FetchError, SearchError};
use serde_json::Value;

use super::crypto::{APP_ID, signed_query};
use super::song::{
    FetchErrorKind, TYPE_PLAYLIST, field, parse_song, search, signed_get, value_string, value_u64,
};

/// 搜索结果里的歌单条目。
fn parse_playlist(item: &Value) -> Option<Playlist> {
    let id = field(item, "id").map(value_string)?;
    let name = item["title"].as_str()?.trim().to_string();
    if id.is_empty() || name.is_empty() {
        return None;
    }
    let mut playlist = Playlist::new(id.clone(), name, SourceId::Qianqian);
    playlist.cover_url = item["pic"]
        .as_str()
        .filter(|url| !url.is_empty())
        .map(str::to_string);
    playlist.song_count = value_u64(&item["trackCount"]).unwrap_or_default() as u32;
    playlist.play_count = value_u64(&item["playCount"]);
    playlist.description = item["desc"]
        .as_str()
        .or_else(|| item["tag"].as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    playlist.link = Some(format!("https://music.91q.com/songlist/{id}"));
    playlist.extra.insert("playlist_id".to_string(), id);
    Some(playlist)
}

pub async fn search_playlists(keyword: &str, page: u32) -> Result<Vec<Playlist>, SearchError> {
    let json = search(keyword, TYPE_PLAYLIST, page, 30)
        .await
        .map_err(FetchErrorKind::into_search)?;
    Ok(json["data"]["typeSonglist"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(parse_playlist)
        .collect())
}

/// 歌单分类目录：`/v1/tracklist/category`。
pub async fn get_categories() -> Result<Vec<PlaylistCategory>, FetchError> {
    let query = signed_query(&[("appid", APP_ID)]);
    let json = signed_get(&format!(
        "https://music.91q.com/v1/tracklist/category?{query}"
    ))
    .await
    .map_err(FetchErrorKind::into_fetch)?;
    if json["state"] != true && json["errno"].as_i64() != Some(22000) {
        return Err(FetchError::Other("千千音乐歌单分类请求失败".to_string()));
    }
    let mut categories = vec![PlaylistCategory {
        id: String::new(),
        name: "全部".to_string(),
        source: SourceId::Qianqian,
        group: Some("全部".to_string()),
        count: 0,
        hot: true,
        extra: Default::default(),
    }];
    for group in json["data"].as_array().into_iter().flatten() {
        let group_name = group["categoryName"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string();
        for item in group["subCate"].as_array().into_iter().flatten() {
            let id = value_string(&item["id"]);
            let name = item["categoryName"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_string();
            if id.is_empty() || name.is_empty() {
                continue;
            }
            let mut category = PlaylistCategory::new(id, name, SourceId::Qianqian);
            category.group = (!group_name.is_empty()).then(|| group_name.clone());
            category.count = value_u64(&item["count"]).unwrap_or_default() as u32;
            categories.push(category);
        }
    }
    Ok(categories)
}

/// 分类歌单：`/v1/tracklist/list`。
pub async fn get_category_list(category: &str, page: u32) -> Result<Vec<Playlist>, FetchError> {
    let page_no = page.max(1).to_string();
    let mut params = vec![
        ("appid", APP_ID),
        ("pageNo", page_no.as_str()),
        ("pageSize", "30"),
    ];
    if !category.trim().is_empty() {
        params.push(("subCateId", category.trim()));
    }
    let query = signed_query(&params);
    let json = signed_get(&format!("https://music.91q.com/v1/tracklist/list?{query}"))
        .await
        .map_err(FetchErrorKind::into_fetch)?;
    if json["state"] != true && json["errno"].as_i64() != Some(22000) {
        return Err(FetchError::Other("千千音乐分类歌单请求失败".to_string()));
    }
    Ok(json["data"]["result"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let mut playlist = parse_playlist(item)?;
            if let Some(tags) = item["tagList"].as_array()
                && playlist.description.is_none()
            {
                let joined = tags
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("、");
                playlist.description = (!joined.is_empty()).then_some(joined);
            }
            playlist
                .extra
                .insert("category_id".to_string(), category.to_string());
            Some(playlist)
        })
        .collect())
}

/// 歌单详情 + 元数据：`/v1/tracklist/info`。
pub async fn get_detail_with_meta(id: &str) -> Result<(Playlist, Vec<SongInfo>), FetchError> {
    let query = signed_query(&[("id", id), ("appid", APP_ID), ("type", "0")]);
    let json = signed_get(&format!("https://music.91q.com/v1/tracklist/info?{query}"))
        .await
        .map_err(FetchErrorKind::into_fetch)?;
    let data = &json["data"];
    let songs = data["trackList"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(parse_song)
        .collect::<Vec<_>>();
    if songs.is_empty() {
        return Err(FetchError::NotFound);
    }
    let name = data["title"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("千千歌单 {id}"));
    let mut playlist = Playlist::new(id, name, SourceId::Qianqian);
    playlist.song_count = value_u64(&data["trackCount"]).unwrap_or(songs.len() as u64) as u32;
    playlist.cover_url = data["pic"]
        .as_str()
        .filter(|url| !url.is_empty())
        .map(str::to_string);
    playlist.description = data["desc"]
        .as_str()
        .or_else(|| data["description"].as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    playlist.creator = ["creator", "author", "userName", "nickName", "ownerName"]
        .iter()
        .find_map(|key| data[*key].as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    playlist.link = Some(format!("https://music.91q.com/songlist/{id}"));
    Ok((playlist, songs))
}

/// 歌单曲目（供 trait 使用）。
pub async fn get_detail(id: &str) -> Result<Vec<SongInfo>, FetchError> {
    get_detail_with_meta(id).await.map(|(_, songs)| songs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_searched_playlist() {
        let item = serde_json::json!({
            "id": 12345,
            "title": "华语流行",
            "pic": "https://cdn/p.jpg",
            "trackCount": 42,
            "tag": "流行"
        });
        let playlist = parse_playlist(&item).unwrap();
        assert_eq!(playlist.id, "12345");
        assert_eq!(playlist.name, "华语流行");
        assert_eq!(playlist.song_count, 42);
        assert_eq!(playlist.description.as_deref(), Some("流行"));
        assert_eq!(
            playlist.link.as_deref(),
            Some("https://music.91q.com/songlist/12345")
        );
    }

    #[test]
    fn rejects_entries_without_id_or_title() {
        assert!(parse_playlist(&serde_json::json!({ "title": "无 ID" })).is_none());
        assert!(parse_playlist(&serde_json::json!({ "id": "1" })).is_none());
    }
}
