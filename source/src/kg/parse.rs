//! 酷狗分享链接解析。
//!
//! 链接形态对齐 music-lib `kugou`：
//! - 单曲：链接里带 `hash=`（32 位十六进制），用移动端 `getSongInfo` 取元数据；
//! - 歌单：`/yy/special/single/{id}.html` 或 `/songlist/gcid_xxx`；
//! - 专辑：`album/single/{id}.html`、`yy/album/single/{id}.html`、`albumid={id}`。

use lx_core::model::playlist::Playlist;
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::{FetchError, ParsedLink};
use regex::Regex;
use serde_json::Value;

use crate::http;
use crate::http::SendWithRetry;

use super::playlist::{MOBILE_REFERER, MOBILE_UA};

/// 链接指向的资源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KgLink {
    Song { hash: String },
    Playlist { id: String },
    Album { id: String },
}

/// 识别酷狗链接；无法识别时返回 `None`。
pub fn link_target(link: &str) -> Option<KgLink> {
    let trimmed = link.trim();
    // 单曲链接的 hash 可能出现在查询串或片段里，直接对整串做匹配。
    if let Some(hash) = capture(r"(?i)hash=([a-f0-9]{32})", trimmed) {
        return Some(KgLink::Song { hash });
    }
    if let Some(id) = capture(r"special/single/(\d+)\.html", trimmed) {
        return Some(KgLink::Playlist { id });
    }
    if let Some(id) = capture(r"songlist/(gcid_[a-zA-Z0-9]+)", trimmed) {
        return Some(KgLink::Playlist { id });
    }
    for pattern in [
        r"album/single/(\d+)\.html",
        r"yy/album/single/(\d+)\.html",
        r"album/(\d+)\.html",
        r"albumid=(\d+)",
    ] {
        if let Some(id) = capture(pattern, trimmed) {
            return Some(KgLink::Album { id });
        }
    }
    None
}

fn capture(pattern: &str, value: &str) -> Option<String> {
    let regex = Regex::new(pattern).ok()?;
    regex
        .captures(value)
        .and_then(|captures| captures.get(1))
        .map(|capture| capture.as_str().to_string())
}

pub async fn parse(link: &str) -> Result<ParsedLink, FetchError> {
    match link_target(link)
        .ok_or_else(|| FetchError::Other("无法识别酷狗链接中的歌曲/歌单/专辑".to_string()))?
    {
        KgLink::Song { hash } => Ok(ParsedLink::Song(Box::new(fetch_song(&hash).await?))),
        KgLink::Playlist { id } => {
            let (playlist, songs) = fetch_playlist(&id).await?;
            Ok(ParsedLink::Playlist {
                playlist: Box::new(playlist),
                songs,
            })
        }
        KgLink::Album { id } => Err(FetchError::Other(format!(
            "酷狗专辑链接（ID {id}）暂不支持直解，请用歌单链接或关键词搜索"
        ))),
    }
}

async fn get_json(url: &str) -> Result<Value, FetchError> {
    super::with_cookie(http::client().get(url))
        .header("User-Agent", MOBILE_UA)
        .header("Referer", MOBILE_REFERER)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))
}

/// 按 hash 取单曲元数据。
///
/// 与 music-lib 不同，这里不要求接口同时返回可播放地址：链接直解只需要
/// 歌名/歌手/时长/封面，付费曲目依旧能正常展示，播放地址仍由播放时解析。
pub async fn fetch_song(hash: &str) -> Result<SongInfo, FetchError> {
    let json = get_json(&format!(
        "http://m.kugou.com/app/i/getSongInfo.php?cmd=playInfo&hash={hash}"
    ))
    .await?;
    let name = json["songName"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    if name.is_empty() {
        return Err(FetchError::NotFound);
    }
    let mut song = SongInfo::new(hash.to_string(), SourceId::Kg, name, String::new());
    song.singer = json["author_name"].as_str().unwrap_or_default().to_string();
    song.duration = std::time::Duration::from_secs(json["timeLength"].as_u64().unwrap_or_default());
    song.cover_url = json["album_img"]
        .as_str()
        .filter(|url| !url.is_empty())
        .map(|url| url.replace("{size}", "500"));
    song.extra.insert("FileHash".to_string(), hash.to_string());
    Ok(song)
}

/// 歌单：复用歌单页解析（与热门歌单详情同一条路径），标题取自页面 `<title>`。
pub async fn fetch_playlist(id: &str) -> Result<(Playlist, Vec<SongInfo>), FetchError> {
    let (title, songs) = super::playlist::get_detail_with_title(id).await?;
    if songs.is_empty() {
        return Err(FetchError::NotFound);
    }
    let name = title.unwrap_or_else(|| format!("酷狗歌单 {id}"));
    let mut playlist = Playlist::new(id, name, SourceId::Kg);
    playlist.song_count = songs.len() as u32;
    playlist.cover_url = songs.first().and_then(|song| song.cover_url.clone());
    playlist.link = Some(format!("https://www.kugou.com/yy/special/single/{id}.html"));
    Ok((playlist, songs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_song_links_by_hash() {
        assert_eq!(
            link_target("https://www.kugou.com/song/#hash=0f1e2d3c4b5a69788796a5b4c3d2e1f0"),
            Some(KgLink::Song {
                hash: "0f1e2d3c4b5a69788796a5b4c3d2e1f0".to_string()
            })
        );
        // 大写哈希同样接受。
        assert!(matches!(
            link_target(
                "https://www.kugou.com/mixsong/abc.html?hash=0F1E2D3C4B5A69788796A5B4C3D2E1F0"
            ),
            Some(KgLink::Song { .. })
        ));
    }

    #[test]
    fn parses_playlist_and_album_links() {
        assert_eq!(
            link_target("https://www.kugou.com/yy/special/single/546903.html"),
            Some(KgLink::Playlist {
                id: "546903".to_string()
            })
        );
        assert_eq!(
            link_target("https://www.kugou.com/songlist/gcid_3z9abcd"),
            Some(KgLink::Playlist {
                id: "gcid_3z9abcd".to_string()
            })
        );
        assert_eq!(
            link_target("https://www.kugou.com/yy/album/single/12345.html"),
            Some(KgLink::Album {
                id: "12345".to_string()
            })
        );
        assert_eq!(
            link_target("https://www.kugou.com/albumid=6789"),
            Some(KgLink::Album {
                id: "6789".to_string()
            })
        );
    }

    #[test]
    fn rejects_links_without_a_recognised_id() {
        assert_eq!(link_target("https://www.kugou.com/"), None);
        assert_eq!(link_target("https://www.kugou.com/song/#hash=short"), None);
        assert_eq!(link_target("随便一段文字"), None);
    }
}
