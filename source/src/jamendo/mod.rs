//! Jamendo（CC 授权音乐平台）音源。
//!
//! Jamendo 的 Web 接口不要求 API Key，但要求一个 `x-jam-call` 签名头：
//! `$sha1(路径 + 随机数)*随机数~`，再带上固定的 `x-jam-version`。
//! 搜索接口直接返回 JSON 数组（不是常见的 `{data:[...]}` 包装）。
//!
//! 音频地址分 `download`（可下载）与 `stream`（流式）两组，各带
//! flac / mp33 / mp32 / mp3 / ogg 等档位，按 music-lib 的顺序挑最优。

use async_trait::async_trait;
use sha1::Digest;

use lx_core::model::lyric::LyricData;
use lx_core::model::playlist::{Album, Playlist};
use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{
    FetchError, MusicSource, ParsedLink, SearchError, SearchResult, SongUrl, SourceCapabilities,
};
use serde_json::Value;

use crate::http;
use crate::http::SendWithRetry;

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";
const REFERER: &str = "https://www.jamendo.com/search?q=musicdl";
const X_JAM_VERSION: &str = "4gvfvv";
const BASE: &str = "https://www.jamendo.com";

#[derive(Debug)]
enum ApiError {
    Network(String),
    Parse(String),
    NotFound(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Network(message) => write!(formatter, "网络错误: {message}"),
            ApiError::Parse(message) => write!(formatter, "解析失败: {message}"),
            ApiError::NotFound(message) => write!(formatter, "{message}"),
        }
    }
}

impl ApiError {
    fn into_fetch(self) -> FetchError {
        match self {
            ApiError::Network(message) => FetchError::Network(message),
            ApiError::Parse(message) => FetchError::Parse(message),
            ApiError::NotFound(message) => FetchError::Other(message),
        }
    }

    fn into_search(self) -> SearchError {
        match self {
            ApiError::Network(message) => SearchError::Network(message),
            ApiError::Parse(message) => SearchError::Parse(message),
            ApiError::NotFound(message) => SearchError::Other(message),
        }
    }
}

/// `x-jam-call` 签名：`$sha1(路径 + 随机数)*随机数~`。
pub(super) fn x_jam_call(path: &str, nonce: f64) -> String {
    let nonce = format!("{nonce}");
    let digest = sha1::Sha1::digest(format!("{path}{nonce}").as_bytes());
    format!("${}*{}~", hex::encode(digest), nonce)
}

fn random_nonce() -> f64 {
    use rand::Rng;
    rand::thread_rng().gen_range(0.1..1.0)
}

async fn api_get(path: &str, params: &[(&str, &str)]) -> Result<Value, ApiError> {
    let query = params
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                urlencoding::encode(key),
                urlencoding::encode(value)
            )
        })
        .collect::<Vec<_>>()
        .join("&");
    let url = if query.is_empty() {
        format!("{BASE}{path}")
    } else {
        format!("{BASE}{path}?{query}")
    };
    let text = http::client()
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Referer", REFERER)
        .header("x-jam-call", x_jam_call(path, random_nonce()))
        .header("x-jam-version", X_JAM_VERSION)
        .header("x-requested-with", "XMLHttpRequest")
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| ApiError::Network(error.to_string()))?
        .text()
        .await
        .map_err(|error| ApiError::Network(error.to_string()))?;
    serde_json::from_str(&text).map_err(|error| ApiError::Parse(error.to_string()))
}

fn value_string(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_u64().map(|value| value.to_string()))
        .or_else(|| value.as_i64().map(|value| value.to_string()))
        .unwrap_or_default()
}

fn non_zero_id(value: &Value) -> Option<String> {
    let id = value_string(value);
    (!id.is_empty() && id != "0").then_some(id)
}

fn cover_of(item: &Value) -> Option<String> {
    item["cover"]["big"]["size300"]
        .as_str()
        .or_else(|| item["image"].as_str())
        .filter(|url| !url.is_empty())
        .map(str::to_string)
}

/// 从 `download` / `stream` 两组地址里挑最优档位。
fn pick_stream(item: &Value) -> Option<(String, &'static str, Quality)> {
    let stream = if item["download"]
        .as_object()
        .is_some_and(|map| !map.is_empty())
    {
        &item["download"]
    } else {
        &item["stream"]
    };
    for (key, extension, quality) in [
        ("flac", "flac", Quality::Flac),
        ("mp33", "mp3", Quality::High320),
        ("mp32", "mp3", Quality::High320),
        ("mp3", "mp3", Quality::Low128),
        ("ogg", "ogg", Quality::Low128),
    ] {
        if let Some(url) = stream[key].as_str().filter(|url| !url.is_empty()) {
            return Some((url.to_string(), extension, quality));
        }
    }
    None
}

fn parse_track(item: &Value) -> Option<SongInfo> {
    let id = non_zero_id(&item["id"])?;
    let name = item["name"].as_str().unwrap_or_default().trim().to_string();
    if name.is_empty() {
        return None;
    }
    let singer = item["artist"]["name"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let mut song = SongInfo::new(id.clone(), SourceId::Jamendo, name, singer);
    song.album_name = item["album"]["name"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    song.album_id = non_zero_id(&item["albumId"])
        .or_else(|| non_zero_id(&item["album"]["id"]))
        .unwrap_or_default();
    song.duration = std::time::Duration::from_secs(item["duration"].as_u64().unwrap_or_default());
    song.cover_url = cover_of(item);
    if let Some((_, _, quality)) = pick_stream(item) {
        song.qualities.insert(quality);
    }
    song.extra.insert("track_id".to_string(), id);
    Some(song)
}

fn parse_playlist(item: &Value) -> Option<Playlist> {
    let id = non_zero_id(&item["id"])?;
    let name = item["name"].as_str().unwrap_or_default().trim().to_string();
    if name.is_empty() {
        return None;
    }
    let mut playlist = Playlist::new(id.clone(), name, SourceId::Jamendo);
    playlist.creator = item["user_name"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    playlist.cover_url = cover_of(item);
    playlist.description = description_of(item);
    playlist.link = Some(format!("https://www.jamendo.com/playlist/{id}"));
    playlist.extra.insert("playlist_id".to_string(), id);
    Some(playlist)
}

/// 简介可能是字符串，也可能是按语言分的对象（专辑就是后者）。
fn description_of(item: &Value) -> Option<String> {
    if let Some(value) = item["description"].as_str() {
        let value = value.trim();
        return (!value.is_empty() && value != "0").then(|| value.to_string());
    }
    item["description"]
        .as_object()
        .and_then(|map| map.values().find_map(|value| value.as_str()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// 搜索：Jamendo 按 `type` 区分 track / album / playlist。
async fn search_by_type(keyword: &str, search_type: &str) -> Result<Value, ApiError> {
    api_get(
        "/api/search",
        &[
            ("query", keyword),
            ("type", search_type),
            ("limit", "20"),
            ("identities", "www"),
        ],
    )
    .await
}

/// 按 ID 取曲目（支持批量，专辑/歌单详情靠它把曲目补齐）。
async fn fetch_tracks(ids: &[String]) -> Result<Value, ApiError> {
    if ids.is_empty() {
        return Ok(Value::Array(Vec::new()));
    }
    let query = ids
        .iter()
        .map(|id| format!("id={}", urlencoding::encode(id)))
        .collect::<Vec<_>>()
        .join("&");
    let text = http::client()
        .get(format!("{BASE}/api/tracks?{query}"))
        .header("User-Agent", USER_AGENT)
        .header("Referer", REFERER)
        .header("x-jam-call", x_jam_call("/api/tracks", random_nonce()))
        .header("x-jam-version", X_JAM_VERSION)
        .header("x-requested-with", "XMLHttpRequest")
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| ApiError::Network(error.to_string()))?
        .text()
        .await
        .map_err(|error| ApiError::Network(error.to_string()))?;
    serde_json::from_str(&text).map_err(|error| ApiError::Parse(error.to_string()))
}

async fn fetch_tracks_by_ids(ids: &[String]) -> Result<Vec<SongInfo>, ApiError> {
    let json = fetch_tracks(ids).await?;
    Ok(json
        .as_array()
        .map(|items| items.iter().filter_map(parse_track).collect::<Vec<_>>())
        .unwrap_or_default())
}

/// 专辑 / 歌单详情：先取容器拿曲目 ID，再批量取曲目。
async fn fetch_container_tracks(
    path: &str,
    id: &str,
    kind: &str,
) -> Result<(Playlist, Vec<SongInfo>), ApiError> {
    let json = api_get(path, &[("id", id)]).await?;
    let item = json
        .as_array()
        .and_then(|items| items.first())
        .ok_or_else(|| ApiError::NotFound(format!("Jamendo {kind}不存在")))?;
    let name = item["name"].as_str().unwrap_or_default().trim().to_string();
    if name.is_empty() {
        return Err(ApiError::NotFound(format!("Jamendo {kind}信息为空")));
    }
    let ids = item["tracks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|track| non_zero_id(&track["id"]))
        .collect::<Vec<_>>();
    let songs = fetch_tracks_by_ids(&ids).await?;

    let mut playlist = Playlist::new(id, name, SourceId::Jamendo);
    playlist.cover_url = cover_of(item);
    playlist.song_count = songs.len() as u32;
    playlist.description = description_of(item);
    playlist.creator = item["user_name"]
        .as_str()
        .or_else(|| item["artist"]["name"].as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| songs.first().map(|song| song.singer.clone()));
    playlist.link = Some(format!("https://www.jamendo.com/{kind}/{id}"));
    Ok((playlist, songs))
}

/// 链接识别：`/track/{id}`、`/album/{id}`、`/playlist/{id}`。
#[derive(Debug, Clone, PartialEq, Eq)]
enum JamendoLink {
    Track(String),
    Album(String),
    Playlist(String),
}

fn link_target(link: &str) -> Option<JamendoLink> {
    let trimmed = link.trim();
    let kinds = [
        ("track", JamendoLink::Track as fn(String) -> JamendoLink),
        ("album", JamendoLink::Album),
        ("playlist", JamendoLink::Playlist),
    ];
    for (segment, build) in kinds {
        let marker = format!("jamendo.com/{segment}/");
        if let Some(index) = trimmed.find(&marker) {
            let id = trimmed[index + marker.len()..]
                .split(['/', '?', '#'])
                .next()
                .unwrap_or_default()
                .trim();
            if !id.is_empty() && id.chars().all(|character| character.is_ascii_digit()) {
                return Some(build(id.to_string()));
            }
        }
    }
    None
}

pub struct JamendoSource;

impl JamendoSource {
    pub fn new() -> Self {
        Self
    }
}

impl Default for JamendoSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MusicSource for JamendoSource {
    fn id(&self) -> SourceId {
        SourceId::Jamendo
    }

    fn name(&self) -> &str {
        "Jamendo"
    }

    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities {
            playlists: true,
            playlist_search: true,
            album: true,
            link_parse: true,
            ..Default::default()
        }
    }

    async fn search(
        &self,
        keyword: &str,
        _page: u32,
        _limit: u32,
    ) -> Result<SearchResult, SearchError> {
        let json = search_by_type(keyword, "track")
            .await
            .map_err(ApiError::into_search)?;
        let items = json
            .as_array()
            .map(|items| items.iter().filter_map(parse_track).collect::<Vec<_>>())
            .unwrap_or_default();
        Ok(SearchResult {
            total: items.len() as u32,
            has_more: false,
            items,
        })
    }

    async fn get_song_url(
        &self,
        song: &SongInfo,
        _quality: Quality,
    ) -> Result<SongUrl, FetchError> {
        let id = song
            .extra
            .get("track_id")
            .cloned()
            .unwrap_or_else(|| song.id.clone());
        let json = fetch_tracks(std::slice::from_ref(&id))
            .await
            .map_err(ApiError::into_fetch)?;
        let item = json
            .as_array()
            .and_then(|items| items.first())
            .ok_or(FetchError::NotFound)?;
        // Jamendo 给的是可直接下载的文件，无损优先；请求低档时同样拿最优。
        let (url, _, achieved) = pick_stream(item)
            .ok_or_else(|| FetchError::Other("Jamendo 未返回可用地址".to_string()))?;
        Ok(SongUrl {
            url,
            quality: achieved,
            duration: song.duration,
            cover_url: song.cover_url.clone(),
            qualities: vec![achieved],
            headers: Vec::new(),
            size: None,
            size_is_advisory: true,
            md5: None,
            candidate_urls: Vec::new(),
            max_chunk_size: 0,
        })
    }

    async fn get_lyric(&self, _song: &SongInfo) -> Result<LyricData, FetchError> {
        // Jamendo 是 CC 曲库，接口不提供歌词。
        Err(FetchError::NotFound)
    }

    async fn get_cover_url(&self, song: &SongInfo) -> Result<String, FetchError> {
        Ok(song.cover_url.clone().unwrap_or_default())
    }

    fn supported_qualities(&self) -> Vec<Quality> {
        vec![Quality::Low128, Quality::High320, Quality::Flac]
    }

    async fn search_playlists(
        &self,
        keyword: &str,
        _page: u32,
    ) -> Result<Vec<Playlist>, SearchError> {
        let json = search_by_type(keyword, "playlist")
            .await
            .map_err(ApiError::into_search)?;
        Ok(json
            .as_array()
            .map(|items| items.iter().filter_map(parse_playlist).collect::<Vec<_>>())
            .unwrap_or_default())
    }

    async fn get_playlist_detail(&self, id: &str, _page: u32) -> Result<Vec<SongInfo>, FetchError> {
        let (_, songs) = fetch_container_tracks("/api/playlists", id, "playlist")
            .await
            .map_err(ApiError::into_fetch)?;
        Ok(songs)
    }

    async fn get_album_songs(
        &self,
        album: &Album,
        _page: u32,
        _limit: u32,
    ) -> Result<SearchResult, SearchError> {
        let (_, songs) = fetch_container_tracks("/api/albums", &album.id, "album")
            .await
            .map_err(ApiError::into_search)?;
        Ok(SearchResult {
            total: songs.len() as u32,
            has_more: false,
            items: songs,
        })
    }

    async fn parse_link(&self, link: &str) -> Result<ParsedLink, FetchError> {
        match link_target(link)
            .ok_or_else(|| FetchError::Other("无法识别 Jamendo 链接".to_string()))?
        {
            JamendoLink::Track(id) => {
                let json = fetch_tracks(std::slice::from_ref(&id))
                    .await
                    .map_err(ApiError::into_fetch)?;
                let song = json
                    .as_array()
                    .and_then(|items| items.first())
                    .and_then(parse_track)
                    .ok_or(FetchError::NotFound)?;
                Ok(ParsedLink::Song(Box::new(song)))
            }
            JamendoLink::Album(id) => {
                let (playlist, songs) = fetch_container_tracks("/api/albums", &id, "album")
                    .await
                    .map_err(ApiError::into_fetch)?;
                Ok(ParsedLink::Album {
                    playlist: Box::new(playlist),
                    songs,
                })
            }
            JamendoLink::Playlist(id) => {
                let (playlist, songs) = fetch_container_tracks("/api/playlists", &id, "playlist")
                    .await
                    .map_err(ApiError::into_fetch)?;
                Ok(ParsedLink::Playlist {
                    playlist: Box::new(playlist),
                    songs,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x_jam_call_matches_the_documented_shape() {
        let header = x_jam_call("/api/search", 0.5);
        assert!(header.starts_with('$'));
        assert!(header.ends_with("~"));
        assert!(header.contains("*0.5"));
        // 同样的 (path, nonce) 必须得到同样的签名。
        assert_eq!(header, x_jam_call("/api/search", 0.5));
        let expected = format!(
            "${}*0.5~",
            hex::encode(sha1::Sha1::digest(b"/api/search0.5"))
        );
        assert_eq!(header, expected);
    }

    #[test]
    fn picks_the_best_available_stream() {
        let flac = serde_json::json!({
            "download": { "flac": "https://cdn/a.flac", "mp3": "https://cdn/a.mp3" }
        });
        let (url, extension, quality) = pick_stream(&flac).unwrap();
        assert_eq!(url, "https://cdn/a.flac");
        assert_eq!(extension, "flac");
        assert_eq!(quality, Quality::Flac);

        // download 为空时退回 stream。
        let streaming = serde_json::json!({
            "download": {},
            "stream": { "mp32": "https://cdn/b.mp3", "ogg": "https://cdn/b.ogg" }
        });
        let (url, extension, _) = pick_stream(&streaming).unwrap();
        assert_eq!(url, "https://cdn/b.mp3");
        assert_eq!(extension, "mp3");

        assert!(pick_stream(&serde_json::json!({})).is_none());
    }

    #[test]
    fn parses_tracks_and_playlists() {
        let track = serde_json::json!({
            "id": 100,
            "name": "Track",
            "duration": 200,
            "album": { "id": 7, "name": "Album" },
            "artist": { "name": "Artist" },
            "cover": { "big": { "size300": "https://cdn/c.jpg" } },
            "download": { "flac": "https://cdn/a.flac" }
        });
        let song = parse_track(&track).unwrap();
        assert_eq!(song.id, "100");
        assert_eq!(song.singer, "Artist");
        assert_eq!(song.album_name, "Album");
        assert_eq!(song.duration.as_secs(), 200);
        assert!(song.qualities.contains(&Quality::Flac));

        let playlist = serde_json::json!({
            "id": 5,
            "name": "Mix",
            "user_name": "someone",
            "image": "https://cdn/p.jpg"
        });
        let playlist = parse_playlist(&playlist).unwrap();
        assert_eq!(playlist.creator.as_deref(), Some("someone"));
        assert_eq!(
            playlist.link.as_deref(),
            Some("https://www.jamendo.com/playlist/5")
        );
    }

    #[test]
    fn album_description_falls_back_to_any_language() {
        let item = serde_json::json!({
            "description": { "fr": "Bonjour", "en": "Hello" }
        });
        let description = description_of(&item).unwrap();
        assert!(description == "Hello" || description == "Bonjour");
        assert_eq!(
            description_of(&serde_json::json!({ "description": "" })),
            None
        );
    }

    #[test]
    fn recognises_links() {
        assert_eq!(
            link_target("https://www.jamendo.com/track/123"),
            Some(JamendoLink::Track("123".to_string()))
        );
        assert_eq!(
            link_target("https://www.jamendo.com/album/456?lang=en"),
            Some(JamendoLink::Album("456".to_string()))
        );
        assert_eq!(
            link_target("https://www.jamendo.com/playlist/789"),
            Some(JamendoLink::Playlist("789".to_string()))
        );
        assert_eq!(link_target("https://www.jamendo.com/"), None);
        assert_eq!(link_target("周杰伦"), None);
    }
}
