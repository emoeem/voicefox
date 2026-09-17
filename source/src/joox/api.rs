//! JOOX 接口层。
//!
//! - 搜索走 OpenJOOX v2 `search_type`；歌单曲目继续走 OpenJOOX playlist；
//! - 单曲详情（含下载地址）走 `api.joox.com/web-fcgi-bin/web_get_songinfo`，
//!   响应是 JSONP（`MusicInfoCallback(...)`），歌词同理走 `web_lyric`。
//!
//! 请求统一带 `X-Forwarded-For`：JOOX 对地区敏感，缺这个头会返回空结果
//! （music-lib 也是这么处理的）。

use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use serde_json::Value;

use crate::http;
use crate::http::SendWithRetry;

pub(super) const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";
/// 固定的出口 IP 标识，绕过地区限制。
pub(super) const X_FORWARDED_FOR: &str = "203.0.113.1";

const OPENJOOX_SEARCH: &str = "https://cache.api.joox.com/openjoox/v2/search_type";
const OPENJOOX_PLAYLIST: &str = "https://cache.api.joox.com/openjoox/v3/playlist";
const SONG_INFO_API: &str = "https://api.joox.com/web-fcgi-bin/web_get_songinfo";
const LYRIC_API: &str = "https://api.joox.com/web-fcgi-bin/web_lyric";

#[derive(Debug)]
pub(super) enum ApiError {
    Network(String),
    Parse(String),
    Other(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Network(message) => write!(formatter, "网络错误: {message}"),
            ApiError::Parse(message) => write!(formatter, "解析失败: {message}"),
            ApiError::Other(message) => write!(formatter, "{message}"),
        }
    }
}

impl ApiError {
    pub(super) fn into_fetch(self) -> lx_core::traits::source::FetchError {
        use lx_core::traits::source::FetchError;
        match self {
            ApiError::Network(message) => FetchError::Network(message),
            ApiError::Parse(message) => FetchError::Parse(message),
            ApiError::Other(message) => FetchError::Other(message),
        }
    }

    pub(super) fn into_search(self) -> lx_core::traits::source::SearchError {
        use lx_core::traits::source::SearchError;
        match self {
            ApiError::Network(message) => SearchError::Network(message),
            ApiError::Parse(message) => SearchError::Parse(message),
            ApiError::Other(message) => SearchError::Other(message),
        }
    }
}

pub(super) fn value_string(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_u64().map(|value| value.to_string()))
        .or_else(|| value.as_i64().map(|value| value.to_string()))
        .unwrap_or_default()
}

pub(super) fn value_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

/// 名称里可能带空格与特殊字符，JOOX 的 URL ID 需要还原成原样。
pub(super) fn normalize_id(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    urlencoding::decode(trimmed)
        .map(|value| value.into_owned())
        .unwrap_or_else(|_| trimmed.to_string())
}

/// 从 JSONP 外壳里取出 JSON：`Callback({...})` → `{...}`。
pub(super) fn strip_jsonp<'a>(body: &'a str, callback: &str) -> &'a str {
    match body.find(&format!("{callback}(")) {
        Some(index) => {
            let inner = &body[index + callback.len() + 1..];
            inner.trim_end().strip_suffix(')').unwrap_or(inner).trim()
        }
        None => body.trim(),
    }
}

async fn openjoox_get(api: &str, params: &[(&str, &str)]) -> Result<Value, ApiError> {
    let mut query = vec![("country", "sg"), ("lang", "zh_cn")];
    query.extend_from_slice(params);
    let url = format!(
        "{api}?{}",
        query
            .iter()
            .map(|(key, value)| format!(
                "{}={}",
                urlencoding::encode(key),
                urlencoding::encode(value)
            ))
            .collect::<Vec<_>>()
            .join("&")
    );
    http::client()
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("X-Forwarded-For", X_FORWARDED_FOR)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| ApiError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| ApiError::Parse(error.to_string()))
}

/// 搜索：返回原始 JSON，歌曲/歌单/专辑各自从 `section_list` 里取。
pub(super) async fn search(keyword: &str) -> Result<Value, ApiError> {
    openjoox_get(OPENJOOX_SEARCH, &[("key", keyword), ("type", "0")]).await
}

/// v2 `search_type` 的歌曲结果位于 `tracks`，部分版本会把单项再包一层数组。
pub(super) fn songs_from_search(json: &Value) -> Vec<SongInfo> {
    let mut songs = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for entry in json["tracks"].as_array().into_iter().flatten() {
        let info = if entry.is_array() {
            entry.as_array().and_then(|items| items.first())
        } else {
            Some(entry)
        };
        let Some(info) = info else { continue };
        let Some(song) = parse_song_info(info) else {
            continue;
        };
        if seen.insert(song.id.clone()) {
            songs.push(song);
        }
    }
    songs
}

/// 歌单曲目。
pub(super) async fn playlist_songs(id: &str) -> Result<Value, ApiError> {
    openjoox_get(OPENJOOX_PLAYLIST, &[("id", id)]).await
}

/// 从搜索结果里提取歌曲（`section_list[].item_list[].song[].song_info`）。
pub(super) fn songs_from_sections(json: &Value) -> Vec<SongInfo> {
    let mut songs = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for section in json["section_list"].as_array().into_iter().flatten() {
        for item in section["item_list"].as_array().into_iter().flatten() {
            for entry in item["song"].as_array().into_iter().flatten() {
                let info = &entry["song_info"];
                let Some(song) = parse_song_info(info) else {
                    continue;
                };
                if seen.insert(song.id.clone()) {
                    songs.push(song);
                }
            }
        }
    }
    songs
}

fn parse_song_info(info: &Value) -> Option<SongInfo> {
    let id = normalize_id(&value_string(&info["id"]));
    let name = info["name"].as_str().unwrap_or_default().trim().to_string();
    if id.is_empty() || name.is_empty() {
        return None;
    }
    let singer = info["artist_list"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|artist| artist["name"].as_str())
        .filter(|name| !name.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" / ");
    let mut song = SongInfo::new(id.clone(), SourceId::Joox, name, singer);
    song.album_name = info["album_name"].as_str().unwrap_or_default().to_string();
    song.album_id = value_string(&info["album_id"]);
    song.duration =
        std::time::Duration::from_secs(value_u64(&info["play_duration"]).unwrap_or_default());
    song.cover_url = pick_image(&info["images"]);
    song.extra.insert("songid".to_string(), id);
    Some(song)
}

/// 封面优先取 300px 那张，否则退回第一张。
fn pick_image(images: &Value) -> Option<String> {
    let list = images.as_array()?;
    list.iter()
        .find(|image| image["width"].as_u64() == Some(300))
        .or_else(|| list.first())
        .and_then(|image| image["url"].as_str())
        .filter(|url| !url.is_empty())
        .map(str::to_string)
}

/// 从歌单/搜索条目里提取歌单（`item_list[].editor_playlist`）。
pub(super) fn playlists_from_sections(json: &Value) -> Vec<lx_core::model::playlist::Playlist> {
    use lx_core::model::playlist::Playlist;
    let mut playlists = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for section in json["section_list"].as_array().into_iter().flatten() {
        for item in section["item_list"].as_array().into_iter().flatten() {
            let info = &item["editor_playlist"];
            let id = normalize_id(&value_string(&info["id"]));
            let name = info["name"].as_str().unwrap_or_default().trim().to_string();
            if id.is_empty() || name.is_empty() || !seen.insert(id.clone()) {
                continue;
            }
            let mut playlist = Playlist::new(id.clone(), name, SourceId::Joox);
            playlist.cover_url = pick_image(&info["images"]);
            playlist.song_count = value_u64(&info["song_count"]).unwrap_or_default() as u32;
            playlist.link = Some(format!("https://www.joox.com/sg/playlist/{id}"));
            playlist.extra.insert("playlist_id".to_string(), id);
            playlists.push(playlist);
        }
    }
    playlists
}

/// 单曲详情 + 可用档位。
pub(super) struct SongDetail {
    pub song: SongInfo,
    pub urls: Vec<(Quality, String)>,
}

/// `web_get_songinfo`：返回 JSONP，里面同时有元数据与各档位直链。
pub(super) async fn fetch_song_detail(song_id: &str) -> Result<SongDetail, ApiError> {
    let url = format!(
        "{SONG_INFO_API}?songid={}&country=hk&lang=zh_TW&from_type=-1&channel_id=-1&_={}",
        urlencoding::encode(song_id),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default()
    );
    let body = http::client()
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("X-Forwarded-For", X_FORWARDED_FOR)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| ApiError::Network(error.to_string()))?
        .text()
        .await
        .map_err(|error| ApiError::Network(error.to_string()))?;
    let json: Value = serde_json::from_str(strip_jsonp(&body, "MusicInfoCallback"))
        .map_err(|error| ApiError::Parse(error.to_string()))?;

    let name = json["msong"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    if name.is_empty() {
        return Err(ApiError::Other("JOOX 单曲详情为空".to_string()));
    }
    let mut song = SongInfo::new(song_id.to_string(), SourceId::Joox, name, String::new());
    song.singer = json["msinger"].as_str().unwrap_or_default().to_string();
    song.album_name = json["malbum"].as_str().unwrap_or_default().to_string();
    song.duration =
        std::time::Duration::from_secs(value_u64(&json["minterval"]).unwrap_or_default());
    song.cover_url = json["img"]
        .as_str()
        .filter(|url| !url.is_empty())
        .map(str::to_string);
    song.extra.insert("songid".to_string(), song_id.to_string());

    // 可用档位由 kbps_map 决定：值为空或 0 表示该档位不可用。
    let kbps: Value = json["kbps_map"]
        .as_str()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_else(|| json["kbps_map"].clone());
    let mut urls = Vec::new();
    for (key, quality, field) in [
        ("320", Quality::High320, "r320Url"),
        ("192", Quality::High320, "r192Url"),
        ("128", Quality::Low128, "mp3Url"),
        ("96", Quality::Low128, "m4aUrl"),
    ] {
        let available = kbps[key]
            .as_str()
            .map(|value| value != "0" && !value.is_empty())
            .or_else(|| kbps[key].as_f64().map(|value| value > 0.0))
            .unwrap_or(true);
        if !available {
            continue;
        }
        if let Some(url) = json[field].as_str().filter(|url| !url.is_empty()) {
            urls.push((quality, url.to_string()));
        }
    }
    for (quality, _) in &urls {
        song.qualities.insert(*quality);
    }
    Ok(SongDetail { song, urls })
}

/// 歌词：`web_lyric` 同样返回 JSONP。
pub(super) async fn fetch_lyric(song_id: &str) -> Result<Option<String>, ApiError> {
    let url = format!(
        "{LYRIC_API}?musicid={}&country=sg&lang=zh_cn",
        urlencoding::encode(song_id)
    );
    let body = http::client()
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("X-Forwarded-For", X_FORWARDED_FOR)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| ApiError::Network(error.to_string()))?
        .text()
        .await
        .map_err(|error| ApiError::Network(error.to_string()))?;
    let json: Value = serde_json::from_str(strip_jsonp(&body, "MusicJsonCallback"))
        .map_err(|error| ApiError::Parse(error.to_string()))?;
    Ok(json["lyric"]
        .as_str()
        .map(str::trim)
        .filter(|lyric| !lyric.is_empty())
        .map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_jsonp_wrappers() {
        assert_eq!(
            strip_jsonp("MusicInfoCallback({\"a\":1})", "MusicInfoCallback"),
            "{\"a\":1}"
        );
        assert_eq!(strip_jsonp("{\"a\":1}", "MusicInfoCallback"), "{\"a\":1}");
    }

    #[test]
    fn parses_songs_and_playlists_from_sections() {
        let json = serde_json::json!({
            "section_list": [{
                "item_list": [
                    { "type": 5, "song": [{ "song_info": {
                        "id": "S1",
                        "name": "晴天",
                        "album_name": "叶惠美",
                        "album_id": "A1",
                        "play_duration": 269,
                        "artist_list": [{ "name": "周杰伦" }],
                        "images": [{ "width": 150, "url": "small.jpg" }, { "width": 300, "url": "big.jpg" }]
                    }}]},
                    { "type": 1, "editor_playlist": { "id": "P1", "name": "华语", "images": [] } },
                    { "type": 2, "album": { "id": "A1", "name": "叶惠美", "artist_list": [{ "name": "周杰伦" }] } }
                ]
            }]
        });
        let songs = songs_from_sections(&json);
        assert_eq!(songs.len(), 1);
        assert_eq!(songs[0].id, "S1");
        assert_eq!(songs[0].singer, "周杰伦");
        assert_eq!(songs[0].cover_url.as_deref(), Some("big.jpg"));

        let playlists = playlists_from_sections(&json);
        assert_eq!(playlists.len(), 1);
        assert_eq!(playlists[0].name, "华语");
    }

    #[test]
    fn ids_are_url_decoded() {
        assert_eq!(normalize_id("abc%2Bdef"), "abc+def");
        assert_eq!(normalize_id("  plain  "), "plain");
    }
}
