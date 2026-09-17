//! 汽水音乐接口层。
//!
//! - 搜索走 Android 端 `/luna/search/{type}`，参数是固定的客户端指纹（无签名）；
//! - 单曲详情/歌词走 SEO 接口 `beta-luna.douyin.com/luna/h5/seo_track`，
//!   它不需要签名也不要求登录，是浏览器分享页用的那条路径；
//! - 歌单详情走 PC 端 `/luna/pc/playlist/detail`。

use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use serde_json::Value;

use crate::http;
use crate::http::SendWithRetry;

pub(super) const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";
const SEO_TRACK_API: &str = "https://beta-luna.douyin.com/luna/h5/seo_track";

/// 统一的接口错误。
#[derive(Debug)]
pub(crate) enum ApiError {
    Network(String),
    Parse(String),
    Other(String),
}

impl ApiError {
    pub(crate) fn into_fetch(self) -> lx_core::traits::source::FetchError {
        use lx_core::traits::source::FetchError;
        match self {
            ApiError::Network(message) => FetchError::Network(message),
            ApiError::Parse(message) => FetchError::Parse(message),
            ApiError::Other(message) => FetchError::Other(message),
        }
    }

    pub(crate) fn into_search(self) -> lx_core::traits::source::SearchError {
        use lx_core::traits::source::SearchError;
        match self {
            ApiError::Network(message) => SearchError::Network(message),
            ApiError::Parse(message) => SearchError::Parse(message),
            ApiError::Other(message) => SearchError::Other(message),
        }
    }
}

/// 搜索：`search_type` 取 `track` / `playlist` 等。
///
/// 用**网页端**参数（`aid=386088` + `device_platform=web`）。Android 端那套
/// 设备指纹是 music-lib 里写死的，实测已经失效——平台返回
/// `{"extra":{"empty_search":1}}`，而网页端参数能正常返回 `result_groups`。
pub(super) async fn search(
    search_type: &str,
    keyword: &str,
    page: u32,
    page_size: u32,
) -> Result<Value, ApiError> {
    let page = page.max(1);
    let page_size = page_size.max(1);
    let url = format!(
        "https://api.qishui.com/luna/search/{search_type}?q={}&cursor={}&count={page_size}&aid=386088&device_platform=web&channel=pc_web",
        urlencoding::encode(keyword),
        (page - 1) * page_size
    );
    let json: Value = http::client()
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("content-type", "application/json; charset=UTF-8")
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| ApiError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| ApiError::Parse(error.to_string()))?;
    Ok(json)
}

/// 搜索里的 `track` 对象统一转成 SongInfo。
pub(super) fn parse_track(track: &Value) -> Option<SongInfo> {
    let id = track["id"].as_str()?.trim().to_string();
    if id.is_empty() {
        return None;
    }
    let name = track["name"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    if name.is_empty() {
        return None;
    }
    let singer = track["artists"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|artist| artist["name"].as_str())
        .filter(|name| !name.trim().is_empty())
        .collect::<Vec<_>>()
        .join("、");
    let mut song = SongInfo::new(id.clone(), SourceId::Soda, name, singer);
    song.album_name = track["album"]["name"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    song.album_id = track["album"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    song.duration =
        std::time::Duration::from_secs(value_u64(&track["duration"]).unwrap_or_default() / 1000);
    song.cover_url = track["album"]["url_cover"]["urls"]
        .as_array()
        .and_then(|urls| urls.first())
        .and_then(Value::as_str)
        .or_else(|| track["album"]["url_cover"]["uri"].as_str())
        .filter(|url| !url.is_empty())
        .map(str::to_string);
    if let Some(vid) = track["vid"].as_str().filter(|value| !value.is_empty()) {
        song.extra.insert("vid".to_string(), vid.to_string());
    }
    song.extra.insert("track_id".to_string(), id);
    song.qualities = qualities_from_bit_rates(&track["bit_rates"]);
    Some(song)
}

/// 码率表 → 音质档位（汽水以码率标注，没有官方的「档位」概念）。
fn qualities_from_bit_rates(bit_rates: &Value) -> std::collections::BTreeSet<Quality> {
    use std::collections::BTreeSet;
    let mut qualities = BTreeSet::new();
    for entry in bit_rates.as_array().into_iter().flatten() {
        let bitrate = value_u64(&entry["bit_rate"]).unwrap_or_default();
        let format = entry["format"]
            .as_str()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let quality = match (bitrate, format.as_str()) {
            (_, "flac") => Quality::Flac,
            (rate, _) if rate >= 256 => Quality::High320,
            (rate, _) if rate > 0 => Quality::Low128,
            _ => continue,
        };
        qualities.insert(quality);
    }
    qualities
}

pub(super) fn value_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

pub(crate) fn value_string(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_u64().map(|value| value.to_string()))
        .or_else(|| value.as_i64().map(|value| value.to_string()))
        .unwrap_or_default()
}

/// 单曲详情（SEO 路径）：返回曲目对象与歌词内容。
pub(super) struct SeoTrack {
    pub track: Value,
    pub lyric: Option<String>,
}

pub(super) async fn fetch_seo_track(track_id: &str) -> Result<SeoTrack, ApiError> {
    let json = fetch_seo_track_value(track_id).await?;
    // SEO 接口把曲目放在 `seo_track` 里（老版本叫 `track` / `track_info`）。
    let track = ["track", "track_info", "seo_track"]
        .iter()
        .find_map(|key| json[*key].as_object().map(|_| json[*key].clone()))
        .ok_or_else(|| ApiError::Other("汽水单曲详情为空".to_string()))?;
    let lyric = json["lyric"]["content"]
        .as_str()
        .filter(|content| !content.trim().is_empty())
        .map(str::to_string);
    Ok(SeoTrack { track, lyric })
}

/// SEO 单曲接口的原始响应：播放链路要遍历整棵树找候选流。
pub(super) async fn fetch_seo_track_value(track_id: &str) -> Result<Value, ApiError> {
    let url = format!(
        "{SEO_TRACK_API}?track_id={}&device_platform=web",
        urlencoding::encode(track_id)
    );
    http::client()
        .get(url)
        .header("User-Agent", USER_AGENT)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| ApiError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| ApiError::Parse(error.to_string()))
}

/// 歌单曲目：PC 端 `/luna/pc/playlist/detail`。
///
/// 响应里曲目藏在 `media_resources[].entity.track_wrapper.track`。
pub(super) async fn fetch_playlist_songs(id: &str) -> Result<Vec<SongInfo>, ApiError> {
    let url = format!(
        "https://api.qishui.com/luna/pc/playlist/detail?playlist_id={}&cursor=0&cnt=100&aid=386088&device_platform=web&channel=pc_web",
        urlencoding::encode(id)
    );
    let json: Value = http::client()
        .get(url)
        .header("User-Agent", USER_AGENT)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| ApiError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| ApiError::Parse(error.to_string()))?;
    let mut songs = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for resource in json["media_resources"].as_array().into_iter().flatten() {
        let track = &resource["entity"]["track_wrapper"]["track"];
        if let Some(song) = parse_track(track)
            && seen.insert(song.id.clone())
        {
            songs.push(song);
        }
    }
    if songs.is_empty() {
        return Err(ApiError::Other("汽水歌单为空或接口已变更".to_string()));
    }
    Ok(songs)
}

/// 链接指向的资源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SodaLink {
    Track(String),
    Playlist(String),
}

/// 识别汽水分享链接：`/share/track?id=`、`/share/playlist?id=`。
pub(super) fn link_target(link: &str) -> Option<SodaLink> {
    let trimmed = link.trim();
    let id = trimmed
        .split_once('?')
        .map(|(_, query)| query)
        .map(|query| query.split('&').collect::<Vec<_>>())
        .into_iter()
        .flatten()
        .find_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            (name == "id" && !value.trim().is_empty()).then(|| value.trim().to_string())
        });
    let id = id?;
    if trimmed.contains("/share/playlist") {
        return Some(SodaLink::Playlist(id));
    }
    if trimmed.contains("/share/track") {
        return Some(SodaLink::Track(id));
    }
    // 纯数字/字母数字 ID 按单曲处理。
    if !trimmed.contains('/') && trimmed.len() > 5 {
        return Some(SodaLink::Track(trimmed.to_string()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_searched_track() {
        let track = serde_json::json!({
            "id": "7001",
            "name": "晴天",
            "duration": 269000,
            "vid": "v1",
            "artists": [{ "name": "周杰伦" }],
            "album": {
                "id": "a1",
                "name": "叶惠美",
                "url_cover": { "urls": ["https://cdn/cover.jpg"] }
            },
            "bit_rates": [
                { "bit_rate": 320, "format": "mp3" },
                { "bit_rate": 0, "format": "flac" }
            ]
        });
        let song = parse_track(&track).unwrap();
        assert_eq!(song.id, "7001");
        assert_eq!(song.singer, "周杰伦");
        assert_eq!(song.album_name, "叶惠美");
        assert_eq!(song.duration.as_secs(), 269);
        assert_eq!(song.extra.get("vid").map(String::as_str), Some("v1"));
        assert!(song.qualities.contains(&Quality::High320));
        assert!(song.qualities.contains(&Quality::Flac));
    }

    #[test]
    fn rejects_tracks_without_id_or_name() {
        assert!(parse_track(&serde_json::json!({ "name": "无 ID" })).is_none());
        assert!(parse_track(&serde_json::json!({ "id": "1" })).is_none());
    }
}
