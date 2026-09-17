//! 千千音乐专辑：搜索、专辑信息与曲目。

use lx_core::model::playlist::Playlist;
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::{FetchError, SearchError};
use serde_json::Value;

use super::crypto::{APP_ID, signed_query};
use super::song::{
    FetchErrorKind, TYPE_ALBUM, field, join_artists, parse_song, search, signed_get, value_string,
    value_u64,
};

/// 专辑 AssetCode：字母数字组合，直接作为 ID 使用。
fn normalize_asset_code(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    trimmed
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect()
}

fn parse_album(item: &Value) -> Option<Playlist> {
    let id = normalize_asset_code(&value_string(&item["albumAssetCode"]));
    let name = item["title"].as_str()?.trim().to_string();
    if id.is_empty() || name.is_empty() {
        return None;
    }
    let mut album = Playlist::new(id.clone(), name, SourceId::Qianqian);
    album.cover_url = item["pic"]
        .as_str()
        .filter(|url| !url.is_empty())
        .map(str::to_string);
    album.creator = Some(join_artists(&item["artist"])).filter(|value| !value.trim().is_empty());
    album.description = item["introduce"]
        .as_str()
        .or_else(|| item["desc"].as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    album.song_count = item["trackList"]
        .as_array()
        .map(|items| items.len() as u32)
        .unwrap_or_default();
    album.link = Some(format!("https://music.91q.com/album/{id}"));
    album.extra.insert("album_id".to_string(), id);
    Some(album)
}

pub async fn search_albums(keyword: &str, page: u32) -> Result<Vec<Playlist>, SearchError> {
    let json = search(keyword, TYPE_ALBUM, page, 30)
        .await
        .map_err(FetchErrorKind::into_search)?;
    Ok(json["data"]["typeAlbum"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(parse_album)
        .collect())
}

/// 专辑信息 + 曲目：`/v1/album/info`。
pub async fn get_album_with_meta(id: &str) -> Result<(Playlist, Vec<SongInfo>), FetchError> {
    let album_code = resolve_asset_code(id).await?;
    let query = signed_query(&[("albumAssetCode", album_code.as_str()), ("appid", APP_ID)]);
    let json = signed_get(&format!("https://music.91q.com/v1/album/info?{query}"))
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
        .unwrap_or_else(|| format!("千千专辑 {album_code}"));
    let mut album = Playlist::new(album_code.clone(), name, SourceId::Qianqian);
    album.song_count = songs.len() as u32;
    album.cover_url = data["pic"]
        .as_str()
        .filter(|url| !url.is_empty())
        .map(str::to_string);
    album.creator = Some(join_artists(&data["artist"])).filter(|value| !value.trim().is_empty());
    album.description = data["introduce"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    album.link = Some(format!("https://music.91q.com/album/{album_code}"));
    Ok((album, songs))
}

/// 数字 albumid → albumAssetCode；已经是 AssetCode 时直接返回。
async fn resolve_asset_code(id: &str) -> Result<String, FetchError> {
    let normalized = normalize_asset_code(id);
    if normalized.is_empty() {
        return Err(FetchError::Other("千千专辑 ID 为空".to_string()));
    }
    // 纯数字的是 albumid，需要换一次；AssetCode 含字母。
    if !normalized
        .chars()
        .all(|character| character.is_ascii_digit())
    {
        return Ok(normalized);
    }
    let query = signed_query(&[("albumid", normalized.as_str()), ("appid", APP_ID)]);
    let json = signed_get(&format!(
        "https://music.91q.com/v1/album/albumid2psid?{query}"
    ))
    .await
    .map_err(FetchErrorKind::into_fetch)?;
    let code = field(&json["data"], "albumAssetCode")
        .map(value_string)
        .map(|value| normalize_asset_code(&value))
        .filter(|value| !value.is_empty())
        .ok_or(FetchError::NotFound)?;
    Ok(code)
}

/// 专辑曲目（供 trait 使用）。
pub async fn get_album_songs(id: &str) -> Result<Vec<SongInfo>, FetchError> {
    get_album_with_meta(id).await.map(|(_, songs)| songs)
}

/// 专辑年份等元数据会放进 extra，这里统一提取发行日期。
pub fn release_date(item: &Value) -> Option<String> {
    value_u64(&item["releaseDate"])
        .map(|value| value.to_string())
        .or_else(|| item["releaseDate"].as_str().map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_searched_albums() {
        let item = serde_json::json!({
            "albumAssetCode": "A100",
            "title": "叶惠美",
            "pic": "https://cdn/a.jpg",
            "introduce": "周杰伦第四张专辑",
            "artist": [{ "name": "周杰伦" }],
            "trackList": [{ "TSID": "T1" }, { "TSID": "T2" }]
        });
        let album = parse_album(&item).unwrap();
        assert_eq!(album.id, "A100");
        assert_eq!(album.creator.as_deref(), Some("周杰伦"));
        assert_eq!(album.song_count, 2);
    }

    #[test]
    fn asset_code_is_sanitized() {
        assert_eq!(normalize_asset_code(" A-100 "), "A100");
        assert_eq!(normalize_asset_code(""), "");
        // 纯数字会被识别为 albumid，需要换码。
        let numeric = normalize_asset_code("123456");
        assert!(numeric.chars().all(|character| character.is_ascii_digit()));
    }

    #[test]
    fn rejects_albums_without_code_or_title() {
        assert!(parse_album(&serde_json::json!({ "title": "无码" })).is_none());
        assert!(parse_album(&serde_json::json!({ "albumAssetCode": "A1" })).is_none());
    }
}
