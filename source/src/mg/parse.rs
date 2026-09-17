//! 咪咕分享链接解析。
//!
//! - 单曲：`music.migu.cn/v3/music/song/{contentId}`
//! - 歌单：`playlistId=`、`musicListId=`、`(playlist|songlist)/{id}`
//! - 专辑：`music.migu.cn/v3|v5/music/album/{id}`、`albumId=`、`resourceId=`

use lx_core::model::playlist::Playlist;
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::{FetchError, ParsedLink};
use regex::Regex;
use serde_json::Value;

use crate::http;
use crate::http::SendWithRetry;

/// 链接指向的资源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MgLink {
    Song { content_id: String },
    Playlist { id: String },
    Album { id: String },
}

pub fn link_target(link: &str) -> Option<MgLink> {
    let trimmed = link.trim();
    if let Some(content_id) = capture(r"music\.migu\.cn/v3/music/song/(\d+)", trimmed) {
        return Some(MgLink::Song { content_id });
    }
    for pattern in [
        r"playlistId=(\d+)",
        r"musicListId=(\d+)",
        r"(?:playlist|songlist)/(\d+)",
    ] {
        if let Some(id) = capture(pattern, trimmed) {
            return Some(MgLink::Playlist { id });
        }
    }
    for pattern in [
        r"music\.migu\.cn/(?:v3|v5)/music/album/(\d+)",
        r"albumId=(\d+)",
        r"resourceId=(\d+)",
    ] {
        if let Some(id) = capture(pattern, trimmed) {
            return Some(MgLink::Album { id });
        }
    }
    // 裸 ID 也接受：分享里常见「复制这段内容…60054701934」这种形式。
    if !trimmed.is_empty() && !trimmed.contains('/') && trimmed.chars().all(|c| c.is_ascii_digit())
    {
        return Some(MgLink::Playlist {
            id: trimmed.to_string(),
        });
    }
    None
}

fn capture(pattern: &str, value: &str) -> Option<String> {
    Regex::new(pattern)
        .ok()?
        .captures(value)
        .and_then(|captures| captures.get(1))
        .map(|capture| capture.as_str().to_string())
}

pub async fn parse(link: &str) -> Result<ParsedLink, FetchError> {
    match link_target(link)
        .ok_or_else(|| FetchError::Other("无法识别咪咕链接中的歌曲/歌单/专辑".to_string()))?
    {
        MgLink::Song { content_id } => {
            Ok(ParsedLink::Song(Box::new(fetch_song(&content_id).await?)))
        }
        MgLink::Playlist { id } => {
            let (playlist, songs) = super::playlist::get_detail_with_meta(&id).await?;
            Ok(ParsedLink::Playlist {
                playlist: Box::new(playlist),
                songs,
            })
        }
        MgLink::Album { id } => {
            let (playlist, songs) = fetch_album(&id).await?;
            Ok(ParsedLink::Album {
                playlist: Box::new(playlist),
                songs,
            })
        }
    }
}

async fn request(url: String) -> Result<Value, FetchError> {
    http::client()
        .get(url)
        .header("Referer", "https://m.music.migu.cn/")
        .header("channel", "0146921")
        .header("User-Agent", super::playlist::USER_AGENT)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))
}

/// 按 contentId 取单曲元数据。
///
/// 只取元数据：咪咕的播放地址要额外的密钥协商，链接直解不该把它拖进来。
pub async fn fetch_song(content_id: &str) -> Result<SongInfo, FetchError> {
    let url = format!(
        "https://app.c.nf.migu.cn/MIGUM2.0/v1.0/content/resourceinfo.do?resourceType=2&resourceId={content_id}"
    );
    let json = request(url).await?;
    let item = json["resource"]
        .as_array()
        .and_then(|items| items.first())
        .ok_or(FetchError::NotFound)?;
    super::song::parse_song(item).ok_or(FetchError::NotFound)
}

/// 专辑元数据 + 曲目。
pub async fn fetch_album(id: &str) -> Result<(Playlist, Vec<SongInfo>), FetchError> {
    let url = format!(
        "https://app.c.nf.migu.cn/MIGUM2.0/v1.0/content/queryAlbumSong?albumId={id}&pageNo=1&pageSize=100"
    );
    let json = request(url).await?;
    let songs = json["data"]["songList"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(super::song::parse_song)
        .collect::<Vec<_>>();
    if songs.is_empty() {
        return Err(FetchError::NotFound);
    }
    // 专辑名放在曲目的 album 字段里（咪咕专辑接口只回歌曲列表）。
    let name = songs
        .first()
        .map(|song| song.album_name.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("咪咕专辑 {id}"));
    let artist = songs
        .first()
        .map(|song| song.singer.clone())
        .unwrap_or_default();
    let mut playlist = Playlist::new(id, name, SourceId::Mg);
    playlist.song_count =
        value_u64(&json["data"]["totalCount"]).unwrap_or(songs.len() as u64) as u32;
    playlist.creator = (!artist.is_empty()).then_some(artist);
    playlist.cover_url = songs.first().and_then(|song| song.cover_url.clone());
    playlist.link = Some(format!("https://music.migu.cn/v3/music/album/{id}"));
    Ok((playlist, songs))
}

fn value_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_song_playlist_and_album_links() {
        assert_eq!(
            link_target("https://music.migu.cn/v3/music/song/60054701934"),
            Some(MgLink::Song {
                content_id: "60054701934".to_string()
            })
        );
        assert_eq!(
            link_target("https://music.migu.cn/v3/music/playlist/123456?playlistId=123456"),
            Some(MgLink::Playlist {
                id: "123456".to_string()
            })
        );
        assert_eq!(
            link_target("https://music.migu.cn/v3/music/album/789456"),
            Some(MgLink::Album {
                id: "789456".to_string()
            })
        );
        assert_eq!(
            link_target("https://music.migu.cn/share?albumId=456789"),
            Some(MgLink::Album {
                id: "456789".to_string()
            })
        );
    }

    #[test]
    fn accepts_a_bare_numeric_share_id() {
        assert_eq!(
            link_target("123456789"),
            Some(MgLink::Playlist {
                id: "123456789".to_string()
            })
        );
    }

    #[test]
    fn rejects_links_without_a_known_resource() {
        assert_eq!(link_target("https://music.migu.cn/v3"), None);
        assert_eq!(link_target("周杰伦 晴天"), None);
    }
}
