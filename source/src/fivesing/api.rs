//! 5sing 接口层。

use lx_core::model::playlist::Playlist;
use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use regex::Regex;
use serde_json::Value;

use crate::http;
use crate::http::SendWithRetry;

pub(super) const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";

const SEARCH_API: &str = "http://search.5sing.kugou.com/home/json";
const SONG_INFO_API: &str = "http://mobileapi.5sing.kugou.com/song/newget";
const SONG_URL_API: &str = "http://mobileapi.5sing.kugou.com/song/getSongUrl";
const PLAYLIST_INFO_API: &str = "http://mobileapi.5sing.kugou.com/song/getsonglist";

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

fn value_string(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_u64().map(|value| value.to_string()))
        .or_else(|| value.as_i64().map(|value| value.to_string()))
        .unwrap_or_default()
}

fn value_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

/// 站点会在标题里塞 `<em>` 高亮标签，转义后要去掉。
pub(super) fn clean_text(raw: &str) -> String {
    let decoded = raw
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");
    Regex::new(r"<[^>]*>")
        .map(|regex| regex.replace_all(&decoded, "").trim().to_string())
        .unwrap_or_else(|_| decoded.trim().to_string())
}

async fn get_text(url: &str) -> Result<String, ApiError> {
    http::client()
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Referer", "http://5sing.kugou.com/")
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| ApiError::Network(error.to_string()))?
        .text()
        .await
        .map_err(|error| ApiError::Network(error.to_string()))
}

async fn get_json(url: &str) -> Result<Value, ApiError> {
    let text = get_text(url).await?;
    serde_json::from_str(&text).map_err(|error| ApiError::Parse(error.to_string()))
}

/// 搜索：`kind` 取 0（歌曲）或 1（歌单）。
async fn search(keyword: &str, kind: &str) -> Result<Value, ApiError> {
    get_json(&format!(
        "{SEARCH_API}?keyword={}&sort=1&page=1&filter=0&type={kind}",
        urlencoding::encode(keyword)
    ))
    .await
}

pub(super) async fn search_songs(keyword: &str) -> Result<Vec<SongInfo>, ApiError> {
    let json = search(keyword, "0").await?;
    Ok(json["list"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = value_string(&item["songId"]);
            let kind = item["typeEname"].as_str().unwrap_or_default().to_string();
            let name = clean_text(item["songName"].as_str().unwrap_or_default());
            if id.is_empty() || kind.is_empty() || name.is_empty() {
                return None;
            }
            let singer = clean_text(item["singer"].as_str().unwrap_or_default());
            let mut song = SongInfo::new(format!("{id}|{kind}"), SourceId::Fivesing, name, singer);
            // 站点只给文件大小，按 320kbps 反推时长作为展示值。
            if let Some(size) = value_u64(&item["songSize"]).filter(|size| *size > 0) {
                song.duration = std::time::Duration::from_secs(size * 8 / 320_000);
            }
            song.extra.insert("songid".to_string(), id.clone());
            song.extra.insert("songtype".to_string(), kind.clone());
            song.cover_url = item["user"]["I"]
                .as_str()
                .filter(|url| !url.is_empty())
                .map(str::to_string);
            Some(song)
        })
        .collect())
}

pub(super) async fn search_playlists(keyword: &str) -> Result<Vec<Playlist>, ApiError> {
    let json = search(keyword, "1").await?;
    Ok(json["list"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = value_string(&item["songListId"]);
            let name = clean_text(item["title"].as_str().unwrap_or_default());
            if id.is_empty() || name.is_empty() {
                return None;
            }
            let user_id = value_string(&item["userId"]);
            let mut playlist = Playlist::new(id.clone(), name, SourceId::Fivesing);
            playlist.cover_url = item["pictureUrl"]
                .as_str()
                .filter(|url| !url.is_empty())
                .map(str::to_string);
            playlist.song_count = value_u64(&item["songCnt"]).unwrap_or_default() as u32;
            playlist.play_count = value_u64(&item["playCount"]);
            let creator = item["userName"].as_str().unwrap_or_default().trim();
            playlist.creator = if creator.is_empty() {
                (!user_id.is_empty()).then(|| format!("ID: {user_id}"))
            } else {
                Some(creator.to_string())
            };
            let description = clean_text(item["content"].as_str().unwrap_or_default());
            playlist.description =
                (!description.is_empty() && description != "0").then_some(description);
            playlist.link = if user_id.is_empty() {
                Some(format!("http://5sing.kugou.com/dj/{id}.html"))
            } else {
                Some(format!("http://5sing.kugou.com/{user_id}/dj/{id}.html"))
            };
            playlist.extra.insert("playlist_id".to_string(), id);
            Some(playlist)
        })
        .collect())
}

/// 单曲元数据（不含地址）。
pub(super) async fn fetch_song(song_id: &str, song_type: &str) -> Result<SongInfo, ApiError> {
    let json = get_json(&format!(
        "{SONG_INFO_API}?songid={song_id}&songtype={song_type}"
    ))
    .await?;
    let data = &json["data"];
    let name = data["SN"].as_str().unwrap_or_default().trim().to_string();
    if name.is_empty() {
        return Err(ApiError::Other("5sing 单曲详情为空".to_string()));
    }
    let mut song = SongInfo::new(
        format!("{song_id}|{song_type}"),
        SourceId::Fivesing,
        name,
        data["user"]["NN"].as_str().unwrap_or_default().to_string(),
    );
    song.cover_url = data["user"]["I"]
        .as_str()
        .filter(|url| !url.is_empty())
        .map(str::to_string);
    song.extra.insert("songid".to_string(), song_id.to_string());
    song.extra
        .insert("songtype".to_string(), song_type.to_string());
    Ok(song)
}

/// 歌词：`newget` 的 `dynamicWords` 字段就是 LRC 文本。
pub(super) async fn fetch_lyric(
    song_id: &str,
    song_type: &str,
) -> Result<Option<String>, ApiError> {
    let json = get_json(&format!(
        "{SONG_INFO_API}?songid={song_id}&songtype={song_type}"
    ))
    .await?;
    Ok(json["data"]["dynamicWords"]
        .as_str()
        .map(str::trim)
        .filter(|lyric| !lyric.is_empty())
        .map(str::to_string))
}

/// 一个档位的直链及其备用地址。
#[derive(Debug, Clone)]
pub(super) struct AudioLink {
    pub quality: Quality,
    pub url: String,
    pub backups: Vec<String>,
}

/// 三档地址集合。
#[derive(Debug, Default)]
pub(super) struct AudioLinks {
    pub entries: Vec<AudioLink>,
}

impl AudioLinks {
    /// 按请求档位挑一个：优先不高于请求档位的最高档，都没有就取最高档。
    pub(super) fn pick(&self, quality: Quality) -> Option<&AudioLink> {
        let mut sorted = self.entries.iter().collect::<Vec<_>>();
        sorted.sort_by(|left, right| right.quality.cmp(&left.quality));
        sorted
            .iter()
            .find(|entry| entry.quality <= quality)
            .or_else(|| sorted.first())
            .map(|entry| *entry)
    }

    pub(super) fn available_qualities(&self) -> Vec<Quality> {
        let mut qualities = self
            .entries
            .iter()
            .map(|entry| entry.quality)
            .collect::<Vec<_>>();
        qualities.sort();
        qualities.dedup();
        qualities
    }
}

pub(super) async fn fetch_audio_links(
    song_id: &str,
    song_type: &str,
) -> Result<AudioLinks, ApiError> {
    let json = get_json(&format!(
        "{SONG_URL_API}?songid={song_id}&songtype={song_type}"
    ))
    .await?;
    if json["code"].as_i64() != Some(1000) {
        return Err(ApiError::Other("5sing 播放地址请求失败".to_string()));
    }
    let data = &json["data"];
    let mut links = AudioLinks::default();
    for (url_key, backup_key, quality) in [
        ("squrl", "squrl_backup", Quality::Flac),
        ("hqurl", "hqurl_backup", Quality::High320),
        ("lqurl", "lqurl_backup", Quality::Low128),
    ] {
        let Some(url) = data[url_key]
            .as_str()
            .map(str::trim)
            .filter(|url| !url.is_empty() && *url != "0")
        else {
            continue;
        };
        let backups = data[backup_key]
            .as_str()
            .map(str::trim)
            .filter(|url| !url.is_empty() && *url != "0")
            .map(|url| vec![url.to_string()])
            .unwrap_or_default();
        links.entries.push(AudioLink {
            quality,
            url: url.to_string(),
            backups,
        });
    }
    if links.entries.is_empty() {
        return Err(ApiError::Other("5sing 未返回可用地址".to_string()));
    }
    Ok(links)
}

/// 歌单元数据 + 曲目。曲目需要解析歌单页 HTML（站点没有公开的曲目接口）。
pub(super) async fn fetch_playlist(id: &str) -> Result<(Playlist, Vec<SongInfo>), ApiError> {
    let json = get_json(&format!("{PLAYLIST_INFO_API}?id={id}&songfields=ID,user")).await?;
    // `data` 可能是对象也可能是空数组：空数组表示歌单不存在或无权限。
    let data = &json["data"];
    if !data.is_object() {
        return Err(ApiError::Other("5sing 歌单不存在或无权访问".to_string()));
    }
    let name = clean_text(data["T"].as_str().unwrap_or_default());
    let user_id = value_string(&data["user"]["ID"]);
    if name.is_empty() || user_id.is_empty() {
        return Err(ApiError::Other("5sing 歌单信息不完整".to_string()));
    }
    let mut playlist = Playlist::new(id, name, SourceId::Fivesing);
    playlist.cover_url = data["P"]
        .as_str()
        .filter(|url| !url.is_empty())
        .map(str::to_string);
    playlist.song_count = value_u64(&data["E"]).unwrap_or_default() as u32;
    playlist.play_count = value_u64(&data["H"]);
    playlist.creator = data["user"]["NN"].as_str().map(str::to_string);
    playlist.description = {
        let description = clean_text(data["C"].as_str().unwrap_or_default());
        (!description.is_empty() && description != "0").then_some(description)
    };
    playlist.link = Some(format!("http://5sing.kugou.com/{user_id}/dj/{id}.html"));
    playlist
        .extra
        .insert("user_id".to_string(), user_id.clone());

    let page = get_text(&format!("http://5sing.kugou.com/{user_id}/dj/{id}.html")).await?;
    let songs = parse_songs_from_html(&page);
    Ok((playlist, songs))
}

/// 从歌单页 HTML 里提取曲目。
///
/// 站点结构：每个 `<li class="p_rel">` 是一首，曲名在 `href="…/{type}/{id}.html"` 的
/// 锚文本里，歌手在 `class="s_soner"`（原站拼写如此）的锚文本里。
pub(super) fn parse_songs_from_html(html: &str) -> Vec<SongInfo> {
    let block_re = match Regex::new(r#"(?s)<li class="p_rel">(.*?)</li>"#) {
        Ok(regex) => regex,
        Err(_) => return Vec::new(),
    };
    let song_re =
        Regex::new(r#"href="http://5sing\.kugou\.com/(yc|fc|bz)/(\d+)\.html"[^>]*>([^<]+)</a>"#)
            .expect("valid 5sing song regex");
    let artist_re =
        Regex::new(r#"class="s_soner[^"]*".*?>([^<]+)</a>"#).expect("valid artist regex");

    let mut songs = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for block in block_re.captures_iter(html) {
        let Some(block_html) = block.get(1).map(|value| value.as_str()) else {
            continue;
        };
        let Some(song_match) = song_re.captures(block_html) else {
            continue;
        };
        let kind = song_match
            .get(1)
            .map(|value| value.as_str())
            .unwrap_or_default();
        let song_id = song_match
            .get(2)
            .map(|value| value.as_str())
            .unwrap_or_default();
        let name = clean_text(
            song_match
                .get(3)
                .map(|value| value.as_str())
                .unwrap_or_default(),
        );
        if song_id.is_empty() || name.is_empty() {
            continue;
        }
        if !seen.insert(format!("{kind}|{song_id}")) {
            continue;
        }
        let singer = artist_re
            .captures(block_html)
            .and_then(|captures| captures.get(1))
            .map(|value| clean_text(value.as_str()))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "Unknown".to_string());
        let mut song = SongInfo::new(
            format!("{song_id}|{kind}"),
            SourceId::Fivesing,
            name,
            singer,
        );
        song.extra.insert("songid".to_string(), song_id.to_string());
        song.extra.insert("songtype".to_string(), kind.to_string());
        songs.push(song);
    }
    songs
}

/// 链接指向的资源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FivesingLink {
    Song { id: String, kind: String },
    Playlist { id: String },
}

pub(super) fn link_target(link: &str) -> Option<FivesingLink> {
    let trimmed = link.trim();
    if let Some(captures) = Regex::new(r"5sing\.kugou\.com/(?:(\d+)/)?dj/([a-zA-Z0-9]+)\.html")
        .ok()?
        .captures(trimmed)
    {
        let id = captures.get(2)?.as_str().to_string();
        return Some(FivesingLink::Playlist { id });
    }
    if let Some(captures) = Regex::new(r"5sing\.kugou\.com/(yc|fc|bz)/(\d+)\.html")
        .ok()?
        .captures(trimmed)
    {
        return Some(FivesingLink::Song {
            kind: captures.get(1)?.as_str().to_string(),
            id: captures.get(2)?.as_str().to_string(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_highlight_tags_and_entities() {
        assert_eq!(clean_text("<em>晴天</em>"), "晴天");
        assert_eq!(clean_text("A&amp;B"), "A&B");
        assert_eq!(clean_text("  <b>周杰伦</b>  "), "周杰伦");
    }

    #[test]
    fn parses_songs_from_playlist_html() {
        let html = r#"
        <ul>
          <li class="p_rel">
            <a href="http://5sing.kugou.com/yc/123456.html" title="原创">晴天</a>
            <a class="s_soner" href="/1/2.html">周杰伦</a>
          </li>
          <li class="p_rel">
            <a href="http://5sing.kugou.com/fc/999.html">翻唱歌</a>
            <a class="s_soner_1" href="/3.html">某歌手</a>
          </li>
          <li class="p_rel">没有歌曲信息</li>
        </ul>"#;
        let songs = parse_songs_from_html(html);
        assert_eq!(songs.len(), 2);
        assert_eq!(songs[0].id, "123456|yc");
        assert_eq!(songs[0].singer, "周杰伦");
        assert_eq!(songs[1].singer, "某歌手");
    }

    #[test]
    fn picks_the_closest_quality_with_backups() {
        let links = AudioLinks {
            entries: vec![
                AudioLink {
                    quality: Quality::Flac,
                    url: "sq".to_string(),
                    backups: vec!["sq2".to_string()],
                },
                AudioLink {
                    quality: Quality::Low128,
                    url: "lq".to_string(),
                    backups: Vec::new(),
                },
            ],
        };
        let picked = links.pick(Quality::High320).unwrap();
        assert_eq!(picked.url, "lq", "请求 320K 时 320K 缺失，应回退到 128K");
        assert_eq!(picked.backups.len(), 0);
        assert_eq!(links.pick(Quality::Flac).unwrap().url, "sq");
        assert_eq!(
            links.available_qualities(),
            vec![Quality::Low128, Quality::Flac]
        );
    }

    #[test]
    fn recognises_links() {
        assert_eq!(
            link_target("http://5sing.kugou.com/yc/123456.html"),
            Some(FivesingLink::Song {
                id: "123456".to_string(),
                kind: "yc".to_string()
            })
        );
        assert_eq!(
            link_target("http://5sing.kugou.com/12345/dj/abc123.html"),
            Some(FivesingLink::Playlist {
                id: "abc123".to_string()
            })
        );
        assert_eq!(link_target("https://www.kugou.com/song/"), None);
    }
}
