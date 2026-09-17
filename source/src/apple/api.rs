//! Apple Music AMP 接口层。
//!
//! 鉴权：AMP 接口要 `Authorization: Bearer <token>`，这个 token 是网页端
//! 内置的公开 JWT——从首页找到 `index-legacy~*.js`，再从中正则提取以 `eyJh`
//! 开头的串即可，不需要账号。`media-user-token`（cookie）只有个人库才需要。

use std::sync::OnceLock;

use lx_core::model::playlist::{Playlist, PlaylistCategory};
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use regex::Regex;
use serde_json::Value;

use crate::http;
use crate::http::SendWithRetry;

pub(super) const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";
const HOME: &str = "https://music.apple.com";
const AMP: &str = "https://amp-api.music.apple.com";
/// 默认店面；分类歌单固定用中国区（策展人 ID 是 cn 的）。
const STOREFRONT: &str = "us";
const CATEGORY_STOREFRONT: &str = "cn";

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

/// 网页端内置的公开 token，进程内缓存一次。
static TOKEN: OnceLock<String> = OnceLock::new();

/// 首页里引用的脚本路径。
pub(super) fn script_path(homepage: &str) -> Option<String> {
    Regex::new(r#"/(assets/index-legacy[~\-][^/"]+\.js)"#)
        .ok()?
        .captures(homepage)
        .and_then(|captures| captures.get(1))
        .map(|capture| capture.as_str().to_string())
}

/// 从脚本内容里提取 JWT。
///
/// 页面里会有多个 JWT（不同用途），无法只靠前缀区分，因此全部收集后
/// 由 [`token`] 逐个试探，用能打通 AMP 接口的那个。
pub(super) fn extract_tokens(script: &str) -> Vec<String> {
    let Ok(regex) = Regex::new(r#"eyJ[A-Za-z0-9._-]{40,}"#) else {
        return Vec::new();
    };
    let mut tokens = regex
        .find_iter(script)
        .map(|found| found.as_str().to_string())
        .collect::<Vec<_>>();
    tokens.dedup();
    tokens
}

async fn token() -> Result<String, ApiError> {
    if let Some(token) = TOKEN.get() {
        return Ok(token.clone());
    }
    let homepage = http::client()
        .get(HOME)
        .header("User-Agent", USER_AGENT)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| ApiError::Network(error.to_string()))?
        .text()
        .await
        .map_err(|error| ApiError::Network(error.to_string()))?;
    let path = script_path(&homepage)
        .ok_or_else(|| ApiError::Other("Apple Music 首页结构变化，未找到脚本".to_string()))?;
    let script = http::client()
        .get(format!("{HOME}/{path}"))
        .header("User-Agent", USER_AGENT)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| ApiError::Network(error.to_string()))?
        .text()
        .await
        .map_err(|error| ApiError::Network(error.to_string()))?;
    let candidates = extract_tokens(&script);
    if candidates.is_empty() {
        return Err(ApiError::Other(
            "Apple Music 脚本里没有找到 token".to_string(),
        ));
    }
    // 逐个试探：用一次极小的搜索请求判断哪个 token 能被接受。
    // 注意不能用「某个具体曲目」探测——曲目不在该区时会返回 404，
    // 那是区划问题而不是 token 问题。
    let mut token = None;
    for candidate in candidates {
        let probe = http::client()
            .get(format!(
                "{AMP}/v1/catalog/{STOREFRONT}/search?term=music&types=songs&limit=1"
            ))
            .header("User-Agent", USER_AGENT)
            .header("Authorization", format!("Bearer {candidate}"))
            .header("Origin", HOME)
            .send_with_retry(1)
            .await;
        if probe.is_ok_and(|response| response.status().is_success()) {
            token = Some(candidate);
            break;
        }
    }
    let token = token.ok_or_else(|| ApiError::Other("Apple Music token 全部不可用".to_string()))?;
    let _ = TOKEN.set(token.clone());
    Ok(token)
}

async fn amp_get(uri: &str, params: &[(&str, &str)]) -> Result<Value, ApiError> {
    let token = token().await?;
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
        format!("{AMP}{uri}")
    } else {
        format!("{AMP}{uri}?{query}")
    };
    http::client()
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Authorization", format!("Bearer {token}"))
        .header("Origin", HOME)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| ApiError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| ApiError::Parse(error.to_string()))
}

/// 封面模板：把 `{w}` / `{h}` 占位替换成具体尺寸。
pub(super) fn artwork_url(template: &str, size: u32) -> Option<String> {
    let template = template.trim();
    if template.is_empty() {
        return None;
    }
    Some(
        template
            .replace("{w}", &size.to_string())
            .replace("{h}", &size.to_string()),
    )
}

fn parse_song(item: &Value) -> Option<SongInfo> {
    let id = item["id"].as_str()?.trim().to_string();
    let attributes = item.get("attributes")?;
    let name = attributes["name"].as_str()?.trim().to_string();
    if id.is_empty() || name.is_empty() {
        return None;
    }
    let mut song = SongInfo::new(
        id.clone(),
        SourceId::Apple,
        name,
        attributes["artistName"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    );
    song.album_name = attributes["albumName"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    song.duration = std::time::Duration::from_millis(
        attributes["durationInMillis"].as_u64().unwrap_or_default(),
    );
    song.cover_url = attributes["artwork"]["url"]
        .as_str()
        .and_then(|template| artwork_url(template, 600));
    song.extra.insert("song_id".to_string(), id);
    Some(song)
}

fn parse_playlist(item: &Value, kind: &str) -> Option<Playlist> {
    let id = item["id"].as_str()?.trim().to_string();
    let attributes = item.get("attributes")?;
    let name = attributes["name"].as_str()?.trim().to_string();
    if id.is_empty() || name.is_empty() {
        return None;
    }
    let mut playlist = Playlist::new(id.clone(), name, SourceId::Apple);
    playlist.cover_url = attributes["artwork"]["url"]
        .as_str()
        .and_then(|template| artwork_url(template, 600));
    playlist.description = {
        let description = attributes["description"]["standard"]
            .as_str()
            .or_else(|| attributes["description"].as_str())
            .unwrap_or_default()
            .trim();
        (!description.is_empty()).then(|| description.to_string())
    };
    playlist.creator = attributes["artistName"]
        .as_str()
        .or_else(|| attributes["curatorName"].as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    playlist.song_count = attributes["trackCount"].as_u64().unwrap_or_default() as u32;
    playlist.link = Some(format!("{HOME}/{}", link_path(kind, &id)));
    playlist.extra.insert(format!("{kind}_id"), id);
    Some(playlist)
}

fn link_path(kind: &str, id: &str) -> String {
    match kind {
        "album" => format!("{STOREFRONT}/album/{id}"),
        "playlist" => format!("{STOREFRONT}/playlist/{id}"),
        _ => format!("{STOREFRONT}/song/{id}"),
    }
}

/// 搜索结果里的歌曲。
pub(super) async fn search(keyword: &str) -> Result<Vec<SongInfo>, ApiError> {
    let json = amp_get(
        &format!("/v1/catalog/{STOREFRONT}/search"),
        &[("term", keyword), ("types", "songs"), ("limit", "30")],
    )
    .await?;
    Ok(json["results"]["songs"]["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(parse_song)
        .collect())
}

pub(super) async fn search_playlists(keyword: &str) -> Result<Vec<Playlist>, ApiError> {
    let json = amp_get(
        &format!("/v1/catalog/{STOREFRONT}/search"),
        &[("term", keyword), ("types", "playlists"), ("limit", "20")],
    )
    .await?;
    Ok(json["results"]["playlists"]["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| parse_playlist(item, "playlist"))
        .collect())
}

/// 单曲详情 + 试听地址。
pub(super) struct SongDetail {
    pub song: SongInfo,
    pub url: Option<String>,
}

pub(super) async fn fetch_song(id: &str) -> Result<SongDetail, ApiError> {
    let json = amp_get(
        &format!("/v1/catalog/{STOREFRONT}/songs/{id}"),
        &[
            ("extend", "extendedAssetUrls"),
            ("include", "lyrics,albums"),
        ],
    )
    .await?;
    let item = json["data"]
        .as_array()
        .and_then(|items| items.first())
        .ok_or_else(|| ApiError::Other("Apple Music 未找到该曲目".to_string()))?;
    let song =
        parse_song(item).ok_or_else(|| ApiError::Other("Apple Music 曲目信息为空".to_string()))?;
    let url = item["attributes"]["previews"]
        .as_array()
        .and_then(|previews| previews.first())
        .and_then(|preview| preview["url"].as_str())
        .filter(|url| !url.is_empty())
        .map(str::to_string);
    Ok(SongDetail { song, url })
}

/// 歌词：`relationships.lyrics.data[].attributes.text`。
pub(super) async fn fetch_lyric(id: &str) -> Result<Option<String>, ApiError> {
    let json = amp_get(
        &format!("/v1/catalog/{STOREFRONT}/songs/{id}"),
        &[("include", "lyrics")],
    )
    .await?;
    let lyric = json["data"]
        .as_array()
        .and_then(|items| items.first())
        .and_then(|item| item["relationships"]["lyrics"]["data"].as_array())
        .and_then(|lyrics| lyrics.first())
        .and_then(|lyric| lyric["attributes"]["text"].as_str())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    Ok(lyric)
}

/// 专辑 / 歌单曲目。
async fn fetch_container_tracks(
    kind: &str,
    id: &str,
) -> Result<(Playlist, Vec<SongInfo>), ApiError> {
    let json = amp_get(
        &format!("/v1/catalog/{STOREFRONT}/{kind}s/{id}"),
        &[("include", "tracks"), ("limit[tracks]", "100")],
    )
    .await?;
    let item = json["data"]
        .as_array()
        .and_then(|items| items.first())
        .ok_or_else(|| ApiError::Other(format!("Apple Music 未找到该 {kind}")))?;
    let playlist = parse_playlist(item, kind)
        .ok_or_else(|| ApiError::Other(format!("Apple Music {kind} 信息为空")))?;
    let songs = item["relationships"]["tracks"]["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(parse_song)
        .collect::<Vec<_>>();
    Ok((playlist, songs))
}

pub(super) async fn fetch_album(id: &str) -> Result<(Playlist, Vec<SongInfo>), ApiError> {
    fetch_container_tracks("album", id).await
}

pub(super) async fn fetch_playlist(id: &str) -> Result<(Playlist, Vec<SongInfo>), ApiError> {
    fetch_container_tracks("playlist", id).await
}

/// 策展人分类：Apple 没有公开的分类目录接口，用固定的中国区策展人 ID 表
/// （与 music-lib 一致）。
pub(super) fn curator_categories() -> Vec<PlaylistCategory> {
    const CURATORS: &[(&str, &str)] = &[
        ("1526756058", "热门"),
        ("1479949880", "C-Pop"),
        ("1019400042", "国语流行"),
        ("1019398918", "粤语流行"),
        ("1019399540", "国际流行"),
        ("1019399551", "K-Pop"),
        ("1019399547", "J-Pop"),
        ("989061415", "嘻哈/说唱"),
        ("1019400044", "R&B"),
        ("1019400046", "摇滚"),
        ("1019397973", "另类音乐"),
        ("976439535", "舞曲"),
        ("1019399544", "电子"),
        ("1019397971", "古典"),
        ("1019399535", "爵士"),
        ("1019399520", "乡村"),
        ("1019399556", "拉丁"),
        ("1019399533", "世界音乐"),
    ];
    let mut categories = vec![PlaylistCategory {
        id: String::new(),
        name: "全部".to_string(),
        source: SourceId::Apple,
        group: Some("全部".to_string()),
        count: 0,
        hot: true,
        extra: Default::default(),
    }];
    for (id, name) in CURATORS {
        let mut category = PlaylistCategory::new(*id, *name, SourceId::Apple);
        category.group = Some("Apple Music".to_string());
        categories.push(category);
    }
    categories
}

/// 分类歌单：`/v1/catalog/cn/apple-curators/{id}/playlists`。
pub(super) async fn category_playlists(
    category_id: &str,
    page: u32,
    limit: u32,
) -> Result<Vec<Playlist>, ApiError> {
    let limit = limit.clamp(1, 25);
    let offset = (page.max(1) - 1) * limit;
    let limit = limit.to_string();
    let offset = offset.to_string();
    let json = amp_get(
        &format!("/v1/catalog/{CATEGORY_STOREFRONT}/apple-curators/{category_id}/playlists"),
        &[
            ("limit", limit.as_str()),
            ("offset", offset.as_str()),
            ("l", "zh-Hans-CN"),
        ],
    )
    .await?;
    Ok(json["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| parse_playlist(item, "playlist"))
        .collect())
}

/// 链接识别：`/song/{id}`、`/album/{id}?i={song}`、`/playlist/{id}`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AppleLink {
    Song(String),
    Album(String),
    Playlist(String),
}

pub(super) fn link_target(link: &str) -> Option<AppleLink> {
    let trimmed = link.trim();
    if trimmed.is_empty() {
        return None;
    }
    // 专辑链接里的 `?i=` 指向专辑内单曲，优先按单曲处理。
    if let Some(song_id) = query_value(trimmed, "i") {
        return Some(AppleLink::Song(song_id));
    }
    let kinds = [
        ("song", AppleLink::Song as fn(String) -> AppleLink),
        ("album", AppleLink::Album),
        ("playlist", AppleLink::Playlist),
    ];
    for (segment, build) in kinds {
        let marker = format!("/{segment}/");
        if let Some(index) = trimmed.find(&marker) {
            let rest = &trimmed[index + marker.len()..];
            // 形如 /album/ye-hui-mei/1440833097：取路径最后一段。
            let id = rest
                .split(['?', '#'])
                .next()
                .unwrap_or(rest)
                .split('/')
                .rfind(|segment| !segment.is_empty())
                .unwrap_or_default()
                .trim()
                .to_string();
            if !id.is_empty() {
                return Some(build(id));
            }
        }
    }
    if !trimmed.is_empty() && trimmed.chars().all(|character| character.is_ascii_digit()) {
        return Some(AppleLink::Song(trimmed.to_string()));
    }
    None
}

fn query_value(link: &str, key: &str) -> Option<String> {
    let (_, query) = link.split_once('?')?;
    let query = query.split('#').next().unwrap_or(query);
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == key && !value.trim().is_empty()).then(|| value.trim().to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_token_from_the_bundled_script() {
        let homepage = r#"<script src="/assets/index-legacy~abc123.js"></script>"#;
        // 真实 token 是长 JWT：正则要求 eyJ 之后至少 40 个合法字符。
        let script =
            r#"window.__TOKEN__="eyJhbGciOiJFUzI1NiJ9.eyJpc3MiOiJhcHBsZSJ9.c2lnbmF0dXJl";"#;
        assert_eq!(
            script_path(homepage).as_deref(),
            Some("assets/index-legacy~abc123.js")
        );
        let tokens = extract_tokens(script);
        assert_eq!(tokens.len(), 1);
        assert!(tokens[0].starts_with("eyJhbGciOiJFUzI1NiJ9"));
        assert_eq!(script_path("<html></html>"), None);
        assert!(extract_tokens("<script>var a = 1;</script>").is_empty());
    }

    #[test]
    fn artwork_template_replaces_size_placeholders() {
        assert_eq!(
            artwork_url("https://cdn/{w}x{h}bb.jpg", 600).as_deref(),
            Some("https://cdn/600x600bb.jpg")
        );
        assert_eq!(artwork_url("   ", 600), None);
    }

    #[test]
    fn parses_song_and_playlist_resources() {
        let song = serde_json::json!({
            "id": "1440833098",
            "attributes": {
                "name": "晴天",
                "artistName": "周杰伦",
                "albumName": "叶惠美",
                "durationInMillis": 269000,
                "artwork": { "url": "https://cdn/{w}x{h}bb.jpg" }
            }
        });
        let song = parse_song(&song).unwrap();
        assert_eq!(song.id, "1440833098");
        assert_eq!(song.singer, "周杰伦");
        assert_eq!(song.duration.as_secs(), 269);
        assert_eq!(song.cover_url.as_deref(), Some("https://cdn/600x600bb.jpg"));

        let playlist = serde_json::json!({
            "id": "pl.abc",
            "attributes": {
                "name": "华语精选",
                "curatorName": "Apple Music",
                "trackCount": 50,
                "description": { "standard": "每日更新" }
            }
        });
        let playlist = parse_playlist(&playlist, "playlist").unwrap();
        assert_eq!(playlist.creator.as_deref(), Some("Apple Music"));
        assert_eq!(playlist.song_count, 50);
        assert_eq!(playlist.description.as_deref(), Some("每日更新"));
    }

    #[test]
    fn recognises_links() {
        assert_eq!(
            link_target("https://music.apple.com/us/song/1440833098"),
            Some(AppleLink::Song("1440833098".to_string()))
        );
        assert_eq!(
            link_target("https://music.apple.com/cn/album/ye-hui-mei/1440833097?i=1440833098"),
            Some(AppleLink::Song("1440833098".to_string()))
        );
        assert_eq!(
            link_target("https://music.apple.com/us/album/ye-hui-mei/1440833097"),
            Some(AppleLink::Album("1440833097".to_string()))
        );
        assert_eq!(
            link_target("https://music.apple.com/us/playlist/pl.abc123"),
            Some(AppleLink::Playlist("pl.abc123".to_string()))
        );
        assert_eq!(link_target("https://music.apple.com/"), None);
        assert_eq!(link_target("周杰伦"), None);
    }

    #[test]
    fn curator_categories_start_with_all() {
        let categories = curator_categories();
        assert_eq!(categories[0].name, "全部");
        assert!(categories.len() > 10);
        assert!(
            categories
                .iter()
                .all(|category| category.source == SourceId::Apple)
        );
    }
}
