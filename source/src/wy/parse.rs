//! 网易云分享链接解析。
//!
//! 对齐 music-lib `netease.parseNeteaseLink`：分享链接把参数放在 `#` 之后
//! （`https://music.163.com/#/playlist?id=123`），因此候选串按
//! 「原串 → 路径+查询 → 片段」依次尝试，任一个能解析出 `song/album/playlist`
//! 加数字 ID 就算命中。

use lx_core::model::playlist::{Album, Playlist};
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::{FetchError, ParsedLink};
use serde_json::Value;

use crate::http;
use crate::http::SendWithRetry;

use super::search;

const REFERER: &str = "https://music.163.com/";

/// 链接指向的资源类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    Song,
    Playlist,
    Album,
}

/// 解析分享链接，得到资源类型与数字 ID。
pub fn link_target(link: &str) -> Option<(LinkKind, String)> {
    let mut candidates = vec![link.to_string()];
    if let Some((path, query)) = split_path_query(link) {
        let mut candidate = path;
        if let Some(query) = query {
            candidate.push('?');
            candidate.push_str(&query);
        }
        candidates.push(candidate);
    }
    if let Some(fragment) = link.split_once('#').map(|(_, fragment)| fragment) {
        let fragment = fragment.trim_start_matches('!').trim();
        if !fragment.is_empty() {
            candidates.push(fragment.to_string());
        }
    }

    candidates
        .iter()
        .find_map(|candidate| parse_candidate(candidate))
}

fn parse_candidate(candidate: &str) -> Option<(LinkKind, String)> {
    let (path, query) = split_path_query(candidate).unwrap_or((candidate.to_string(), None));
    let segments: Vec<String> = path
        .split('/')
        .map(|segment| segment.trim().to_ascii_lowercase())
        .filter(|segment| !segment.is_empty())
        .collect();

    let kind_from_segment = |segment: &str| match segment {
        "song" => Some(LinkKind::Song),
        "album" => Some(LinkKind::Album),
        "playlist" => Some(LinkKind::Playlist),
        _ => None,
    };
    let kind = segments
        .iter()
        .rev()
        .find_map(|segment| kind_from_segment(segment));

    let mut id = query
        .as_deref()
        .and_then(|query| query_value(query, "id"))
        .filter(|value| is_digits(value));
    if id.is_none()
        && let [.., prefix, last] = segments.as_slice()
        && kind_from_segment(prefix).is_some()
        && is_digits(last)
    {
        id = Some(last.clone());
    }

    match (kind, id) {
        (Some(kind), Some(id)) => Some((kind, id)),
        _ => None,
    }
}

/// 拆出路径与查询串，忽略协议、主机、端口与片段。
fn split_path_query(value: &str) -> Option<(String, Option<String>)> {
    let rest = match value.split_once("://") {
        Some((_, rest)) => rest,
        None => value,
    };
    let rest = rest.split('#').next().unwrap_or(rest);
    let rest = match rest.split_once('?') {
        Some((path, query)) => (path, Some(query.to_string())),
        None => (rest, None),
    };
    let path = match value.split_once("://") {
        // 带协议的链接要先跳过主机部分。
        Some(_) => rest.0.split_once('/').map(|(_, path)| path).unwrap_or(""),
        None => rest.0,
    };
    Some((path.to_string(), rest.1))
}

fn query_value(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == key).then(|| value.trim().to_string())
    })
}

fn is_digits(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
}

/// 解析链接并抓取对应资源。
pub async fn parse(link: &str) -> Result<ParsedLink, FetchError> {
    let (kind, id) = link_target(link)
        .ok_or_else(|| FetchError::Other("无法识别网易云链接中的歌曲/歌单/专辑 ID".to_string()))?;
    match kind {
        LinkKind::Song => {
            let song = fetch_song(&id).await?;
            Ok(ParsedLink::Song(Box::new(song)))
        }
        LinkKind::Playlist => {
            let (playlist, songs) = fetch_playlist(&id).await?;
            Ok(ParsedLink::Playlist {
                playlist: Box::new(playlist),
                songs,
            })
        }
        LinkKind::Album => {
            let (playlist, songs) = fetch_album(&id).await?;
            Ok(ParsedLink::Album {
                playlist: Box::new(playlist),
                songs,
            })
        }
    }
}

async fn get_json(url: &str) -> Result<Value, FetchError> {
    let json: Value = super::with_cookie(http::client().get(url))
        .header("Referer", REFERER)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))?;
    if json["code"].as_i64() != Some(200) {
        return Err(FetchError::Other("网易云接口返回异常".to_string()));
    }
    Ok(json)
}

/// 单曲详情：公开接口按 id 批量查，`ids` 是 JSON 数组文本。
pub async fn fetch_song(id: &str) -> Result<SongInfo, FetchError> {
    let url = format!("{REFERER}api/song/detail?ids=%5B{id}%5D");
    let json = get_json(&url).await?;
    json["songs"]
        .as_array()
        .and_then(|songs| songs.first())
        .and_then(search::parse_song)
        .ok_or_else(|| FetchError::NotFound)
}

/// 歌单元数据 + 曲目，用于链接直解与「查看歌单」。
pub async fn fetch_playlist(id: &str) -> Result<(Playlist, Vec<SongInfo>), FetchError> {
    let url = format!("{REFERER}api/v3/playlist/detail?id={id}&n=1000&s=0");
    let json = get_json(&url).await?;
    let detail = &json["playlist"];
    let name = detail["name"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    if name.is_empty() {
        return Err(FetchError::NotFound);
    }
    let songs = detail["tracks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(search::parse_song)
        .collect::<Vec<_>>();
    let mut playlist = Playlist::new(id, name, SourceId::Wy);
    playlist.cover_url = non_empty_string(&detail["coverImgUrl"]);
    playlist.description = non_empty_string(&detail["description"]);
    playlist.song_count = detail["trackCount"].as_u64().unwrap_or(songs.len() as u64) as u32;
    playlist.play_count = detail["playCount"].as_u64();
    playlist.creator = non_empty_string(&detail["creator"]["nickname"]);
    playlist.link = Some(format!("https://music.163.com/#/playlist?id={id}"));
    Ok((playlist, songs))
}

/// 专辑元数据 + 曲目。
pub async fn fetch_album(id: &str) -> Result<(Playlist, Vec<SongInfo>), FetchError> {
    let url = format!("{REFERER}api/album/{id}");
    let json = get_json(&url).await?;
    let detail = &json["album"];
    let name = detail["name"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    if name.is_empty() {
        return Err(FetchError::NotFound);
    }
    let songs = detail["songs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(search::parse_song)
        .collect::<Vec<_>>();
    let mut playlist = Playlist::new(id, name, SourceId::Wy);
    playlist.cover_url = non_empty_string(&detail["picUrl"]);
    playlist.song_count = songs.len() as u32;
    playlist.creator = non_empty_string(&detail["artist"]["name"]);
    playlist.link = Some(format!("https://music.163.com/#/album?id={id}"));
    Ok((playlist, songs))
}

/// 专辑页需要的元数据（歌手页复用 `Album` 结构）。
pub fn album_from_playlist(playlist: &Playlist) -> Album {
    Album {
        id: playlist.id.clone(),
        name: playlist.name.clone(),
        source: playlist.source,
        cover_url: playlist.cover_url.clone(),
        artist: playlist.creator.clone().unwrap_or_default(),
    }
}

fn non_empty_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_song_playlist_and_album_links() {
        assert_eq!(
            link_target("https://music.163.com/#/song?id=123456"),
            Some((LinkKind::Song, "123456".to_string()))
        );
        assert_eq!(
            link_target("https://music.163.com/#/playlist?id=456"),
            Some((LinkKind::Playlist, "456".to_string()))
        );
        assert_eq!(
            link_target("https://music.163.com/album?id=789&userid=1"),
            Some((LinkKind::Album, "789".to_string()))
        );
    }

    #[test]
    fn parses_path_style_links() {
        assert_eq!(
            link_target("https://music.163.com/song/111"),
            Some((LinkKind::Song, "111".to_string()))
        );
        assert_eq!(
            link_target("http://music.163.com/playlist/222/"),
            Some((LinkKind::Playlist, "222".to_string()))
        );
        assert_eq!(
            link_target("https://music.163.com/#/album?id=333"),
            Some((LinkKind::Album, "333".to_string()))
        );
    }

    #[test]
    fn rejects_links_without_a_known_resource() {
        assert_eq!(
            link_target("https://music.163.com/#/discover/toplist"),
            None
        );
        assert_eq!(link_target("https://music.163.com/#/song?id=abc"), None);
        assert_eq!(link_target("周杰伦 晴天"), None);
    }

    #[test]
    fn album_metadata_reuses_the_playlist_shape() {
        let mut playlist = Playlist::new("1", "叶惠美", SourceId::Wy);
        playlist.creator = Some("周杰伦".to_string());
        let album = album_from_playlist(&playlist);
        assert_eq!(album.name, "叶惠美");
        assert_eq!(album.artist, "周杰伦");
    }
}
