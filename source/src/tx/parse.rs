//! QQ 音乐分享链接解析。
//!
//! - 单曲：`/songDetail/{songmid}`，或 `playsong.html?songmid=` / `songid=`
//!   （数字 ID 通过 `fcg_play_single_song` 换回 mid）；
//! - 歌单：`/playlist/{disstid}`、`/taoge/{disstid}` 或 `?id=`/`?disstid=`。

use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::{FetchError, ParsedLink};
use regex::Regex;
use serde_json::Value;

use crate::http;
use crate::http::SendWithRetry;

/// 链接指向的资源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TxLink {
    Song {
        mid: String,
    },
    /// 只有数字 ID（`songid=`）时先查详情再拿 mid。
    SongId {
        id: String,
    },
    Playlist {
        id: String,
    },
}

pub fn link_target(link: &str) -> Option<TxLink> {
    let trimmed = link.trim();
    if let Some(mid) = capture(r"songDetail/([A-Za-z0-9]+)", trimmed) {
        return Some(TxLink::Song { mid });
    }
    if let Some(mid) = query_value(trimmed, "songmid") {
        return Some(TxLink::Song { mid });
    }
    if let Some(id) = query_value(trimmed, "songid") {
        return Some(TxLink::SongId { id });
    }
    for pattern in [r"playlist/(\d+)", r"taoge/(\d+)"] {
        if let Some(id) = capture(pattern, trimmed) {
            return Some(TxLink::Playlist { id });
        }
    }
    for key in ["id", "disstid", "dissid"] {
        if let Some(id) =
            query_value(trimmed, key).filter(|value| value.chars().all(char::is_numeric))
        {
            return Some(TxLink::Playlist { id });
        }
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

/// 取查询参数；同时兼容 `?a=b` 与 `#/path?a=b` 两种写法。
fn query_value(link: &str, key: &str) -> Option<String> {
    let query = link.split_once('?').map(|(_, query)| query)?;
    let query = query.split('#').next().unwrap_or(query);
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name.eq_ignore_ascii_case(key) && !value.trim().is_empty())
            .then(|| value.trim().to_string())
    })
}

pub async fn parse(link: &str) -> Result<ParsedLink, FetchError> {
    match link_target(link)
        .ok_or_else(|| FetchError::Other("无法识别 QQ 音乐链接中的歌曲/歌单".to_string()))?
    {
        TxLink::Song { mid } => Ok(ParsedLink::Song(Box::new(fetch_song(&mid).await?))),
        TxLink::SongId { id } => {
            let mid = resolve_mid(&id).await?;
            Ok(ParsedLink::Song(Box::new(fetch_song(&mid).await?)))
        }
        TxLink::Playlist { id } => {
            let (playlist, songs) = super::playlist::get_detail_with_meta(&id).await?;
            Ok(ParsedLink::Playlist {
                playlist: Box::new(playlist),
                songs,
            })
        }
    }
}

/// 数字 songid → songmid。
async fn resolve_mid(id: &str) -> Result<String, FetchError> {
    let json = song_detail(&format!("songid={id}")).await?;
    json["data"]
        .as_array()
        .and_then(|items| items.first())
        .and_then(|item| item["mid"].as_str())
        .filter(|mid| !mid.is_empty())
        .map(str::to_string)
        .ok_or(FetchError::NotFound)
}

async fn song_detail(query: &str) -> Result<Value, FetchError> {
    let json: Value = http::client()
        .get(format!(
            "https://c.y.qq.com/v8/fcg-bin/fcg_play_single_song.fcg?{query}&format=json"
        ))
        .header("Referer", "https://y.qq.com/")
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))?;
    Ok(json)
}

/// 按 songmid 取单曲元数据。
pub async fn fetch_song(mid: &str) -> Result<SongInfo, FetchError> {
    let json = song_detail(&format!("songmid={mid}")).await?;
    let item = json["data"]
        .as_array()
        .and_then(|items| items.first())
        .ok_or(FetchError::NotFound)?;
    let name = item["name"].as_str().unwrap_or_default().trim().to_string();
    if name.is_empty() {
        return Err(FetchError::NotFound);
    }
    let singer = item["singer"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|singer| singer["name"].as_str())
        .collect::<Vec<_>>()
        .join("、");
    let mut song = SongInfo::new(mid.to_string(), SourceId::Tx, name, singer);
    song.album_name = item["album"]["name"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    song.album_id = item["album"]["mid"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    song.duration = std::time::Duration::from_secs(item["interval"].as_u64().unwrap_or_default());
    if !song.album_id.is_empty() {
        song.cover_url = Some(format!(
            "https://y.gtimg.cn/music/photo_new/T002R300x300M000{}.jpg",
            song.album_id
        ));
    }
    Ok(song)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_song_links() {
        assert_eq!(
            link_target("https://y.qq.com/n/ryqq/songDetail/001Qu4I30eVFYb"),
            Some(TxLink::Song {
                mid: "001Qu4I30eVFYb".to_string()
            })
        );
        assert_eq!(
            link_target("https://i.y.qq.com/v8/playsong.html?songmid=003aAYrm3GE0Ac"),
            Some(TxLink::Song {
                mid: "003aAYrm3GE0Ac".to_string()
            })
        );
        assert_eq!(
            link_target("https://i.y.qq.com/v8/playsong.html?songid=108671366"),
            Some(TxLink::SongId {
                id: "108671366".to_string()
            })
        );
    }

    #[test]
    fn parses_playlist_links() {
        assert_eq!(
            link_target("https://y.qq.com/n/ryqq/playlist/7010262597"),
            Some(TxLink::Playlist {
                id: "7010262597".to_string()
            })
        );
        assert_eq!(
            link_target("https://y.qq.com/n/yqq/playlist.html?id=7010262597"),
            Some(TxLink::Playlist {
                id: "7010262597".to_string()
            })
        );
        assert_eq!(
            link_target("https://y.qq.com/n/ryqq/taoge/123456"),
            Some(TxLink::Playlist {
                id: "123456".to_string()
            })
        );
    }

    #[test]
    fn rejects_links_without_a_known_resource() {
        assert_eq!(link_target("https://y.qq.com/"), None);
        assert_eq!(
            link_target("https://y.qq.com/n/ryqq/singer/001Qu4I30eVFYb"),
            None
        );
        assert_eq!(link_target("周杰伦 晴天"), None);
    }

    #[test]
    fn jsonp_wrapper_is_stripped() {
        assert_eq!(
            super::super::playlist::strip_jsonp("callback({\"code\":0})"),
            "{\"code\":0}"
        );
        assert_eq!(
            super::super::playlist::strip_jsonp("{\"code\":0}"),
            "{\"code\":0}"
        );
    }
}
