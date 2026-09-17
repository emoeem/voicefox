//! 酷我分享链接解析。
//!
//! - 单曲：`/play_detail/{rid}`，用 `m.kuwo.cn/newh5/singles/songinfoandlrc`
//!   取歌名/歌手/封面（只要元数据，播放地址仍由播放时解析）；
//! - 歌单：`/playlist_detail/{id}`，复用歌单详情接口，连歌单名一起返回。

use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::{FetchError, ParsedLink};
use regex::Regex;
use serde_json::Value;

use crate::http;
use crate::http::SendWithRetry;

/// 链接指向的资源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KwLink {
    Song { rid: String },
    Playlist { id: String },
}

pub fn link_target(link: &str) -> Option<KwLink> {
    let trimmed = link.trim();
    if let Some(rid) = capture(r"play_detail/(\d+)", trimmed) {
        return Some(KwLink::Song { rid });
    }
    if let Some(id) = capture(r"playlist_detail/(\d+)", trimmed) {
        return Some(KwLink::Playlist { id });
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
        .ok_or_else(|| FetchError::Other("无法识别酷我链接中的歌曲/歌单".to_string()))?
    {
        KwLink::Song { rid } => Ok(ParsedLink::Song(Box::new(fetch_song(&rid).await?))),
        KwLink::Playlist { id } => {
            let (playlist, songs) = super::playlist::get_detail_with_meta(&id).await?;
            Ok(ParsedLink::Playlist {
                playlist: Box::new(playlist),
                songs,
            })
        }
    }
}

/// 按 rid 取单曲元数据。
///
/// 与 music-lib 不同，这里不要求同时拿到播放地址：付费曲目拿不到地址，
/// 但仍然应该能在列表里展示出来。
pub async fn fetch_song(rid: &str) -> Result<SongInfo, FetchError> {
    let json: Value = http::client()
        .get(format!(
            "http://m.kuwo.cn/newh5/singles/songinfoandlrc?musicId={rid}&httpsStatus=1"
        ))
        .header("Referer", "http://www.kuwo.cn/")
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))?;
    let info = &json["data"]["songinfo"];
    let name = info["songName"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    if name.is_empty() {
        return Err(FetchError::NotFound);
    }
    let artist = info["artist"].as_str().unwrap_or_default().to_string();
    let mut song = SongInfo::new(rid.to_string(), SourceId::Kw, name, artist);
    song.album_name = info["album"].as_str().unwrap_or_default().to_string();
    song.cover_url = info["pic"]
        .as_str()
        .filter(|url| !url.is_empty())
        .map(str::to_string);
    song.duration = std::time::Duration::from_secs(
        info["duration"]
            .as_str()
            .and_then(|value| value.parse().ok())
            .or_else(|| info["duration"].as_u64())
            .unwrap_or_default(),
    );
    song.extra.insert("rid".to_string(), rid.to_string());
    Ok(song)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_song_and_playlist_links() {
        assert_eq!(
            link_target("https://www.kuwo.cn/play_detail/123456"),
            Some(KwLink::Song {
                rid: "123456".to_string()
            })
        );
        assert_eq!(
            link_target("http://www.kuwo.cn/playlist_detail/1082685103"),
            Some(KwLink::Playlist {
                id: "1082685103".to_string()
            })
        );
    }

    #[test]
    fn rejects_links_without_a_known_resource() {
        assert_eq!(link_target("https://www.kuwo.cn/"), None);
        assert_eq!(link_target("https://www.kuwo.cn/singer_detail/1"), None);
        assert_eq!(link_target("周杰伦"), None);
    }
}
