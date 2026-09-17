//! 千千音乐分享链接解析。
//!
//! - 单曲：`music.91q.com/song/{TSID}`
//! - 歌单：`(songlist|tracklist|playlist)/{id}`、`(songlistid|tracklistid|playlistid|id)=`
//! - 专辑：`music.91q.com/album/{assetCode}`、`albumAssetCode=`、`albumid=`

use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::{FetchError, ParsedLink};
use regex::Regex;

use super::crypto::{APP_ID, signed_query};
use super::song::{FetchErrorKind, field, join_artists, signed_get, value_string, value_u64};

/// 链接指向的资源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QianqianLink {
    Song { tsid: String },
    Playlist { id: String },
    Album { id: String },
}

pub fn link_target(link: &str) -> Option<QianqianLink> {
    let trimmed = link.trim();
    if let Some(tsid) = capture(r"music\.91q\.com/song/([A-Za-z0-9]+)", trimmed) {
        return Some(QianqianLink::Song { tsid });
    }
    for pattern in ["songlist", "tracklist", "playlist"] {
        if let Some(id) = capture(
            &format!(r"music\.91q\.com/{pattern}/([A-Za-z0-9]+)"),
            trimmed,
        ) {
            return Some(QianqianLink::Playlist { id });
        }
    }
    if let Some(id) = capture(r"music\.91q\.com/album/([A-Za-z0-9]+)", trimmed) {
        return Some(QianqianLink::Album { id });
    }
    if let Some(id) = capture(r"albumAssetCode=([A-Za-z0-9]+)", trimmed) {
        return Some(QianqianLink::Album { id });
    }
    if let Some(id) = capture(r"albumid=(\d+)", trimmed) {
        return Some(QianqianLink::Album { id });
    }
    for key in ["songlistid", "tracklistid", "playlistid"] {
        if let Some(id) = capture(&format!(r"{key}=([A-Za-z0-9]+)"), trimmed) {
            return Some(QianqianLink::Playlist { id });
        }
    }
    // 裸 ID：分享文案里常见，按歌单处理（与 music-lib 一致）。
    if !trimmed.is_empty()
        && !trimmed.contains('/')
        && trimmed
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    {
        return Some(QianqianLink::Playlist {
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
        .ok_or_else(|| FetchError::Other("无法识别千千音乐链接中的歌曲/歌单/专辑".to_string()))?
    {
        QianqianLink::Song { tsid } => Ok(ParsedLink::Song(Box::new(fetch_song(&tsid).await?))),
        QianqianLink::Playlist { id } => {
            let (playlist, songs) = super::playlist::get_detail_with_meta(&id).await?;
            Ok(ParsedLink::Playlist {
                playlist: Box::new(playlist),
                songs,
            })
        }
        QianqianLink::Album { id } => {
            let (album, songs) = super::album::get_album_with_meta(&id).await?;
            Ok(ParsedLink::Album {
                playlist: Box::new(album),
                songs,
            })
        }
    }
}

/// 按 TSID 取单曲元数据（`/v1/song/info`），不要求同时拿到播放地址。
pub async fn fetch_song(tsid: &str) -> Result<SongInfo, FetchError> {
    let query = signed_query(&[("TSID", tsid), ("appid", APP_ID)]);
    let json = signed_get(&format!("https://music.91q.com/v1/song/info?{query}"))
        .await
        .map_err(FetchErrorKind::into_fetch)?;
    let item = json["data"]
        .as_array()
        .and_then(|items| items.first())
        .ok_or(FetchError::NotFound)?;
    let name = item["title"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    if name.is_empty() {
        return Err(FetchError::NotFound);
    }
    let mut song = SongInfo::new(
        tsid.to_string(),
        SourceId::Qianqian,
        name,
        join_artists(&item["artist"]),
    );
    song.album_name = item["albumTitle"].as_str().unwrap_or_default().to_string();
    song.duration =
        std::time::Duration::from_secs(value_u64(&item["duration"]).unwrap_or_default());
    song.cover_url = item["pic"]
        .as_str()
        .filter(|url| !url.is_empty())
        .map(str::to_string);
    // 详情接口偶尔把数组包在对象里，这里兼容一层。
    song.album_id = field(&item["albumAssetCode"], "")
        .map(value_string)
        .unwrap_or_default();
    song.extra.insert("tsid".to_string(), tsid.to_string());
    Ok(song)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_song_playlist_and_album_links() {
        assert_eq!(
            link_target("https://music.91q.com/song/T10045678"),
            Some(QianqianLink::Song {
                tsid: "T10045678".to_string()
            })
        );
        assert_eq!(
            link_target("https://music.91q.com/songlist/123456"),
            Some(QianqianLink::Playlist {
                id: "123456".to_string()
            })
        );
        assert_eq!(
            link_target("https://music.91q.com/album/A1234"),
            Some(QianqianLink::Album {
                id: "A1234".to_string()
            })
        );
        assert_eq!(
            link_target("https://music.91q.com/?albumAssetCode=A9876"),
            Some(QianqianLink::Album {
                id: "A9876".to_string()
            })
        );
    }

    #[test]
    fn accepts_a_bare_share_id_as_playlist() {
        assert_eq!(
            link_target("abc123"),
            Some(QianqianLink::Playlist {
                id: "abc123".to_string()
            })
        );
    }

    #[test]
    fn rejects_links_without_a_known_resource() {
        assert_eq!(link_target("https://music.91q.com/player"), None);
        assert_eq!(link_target("周杰伦 晴天"), None);
    }
}
