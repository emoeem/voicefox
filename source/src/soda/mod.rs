//! 汽水音乐（qishui / luna）音源。
//!
//! 与其它音源最大的不同：汽水给受保护曲目返回的是 **CENC 加密的 MP4**，
//! 必须先用 `playAuth` 解出密钥、再按样本做 AES-CTR 解密才能播放。
//! 因此播放地址不是「一个可直接播放的 URL」，而是「解密后落到本地缓存的文件」，
//! 由 [`play::SodaStream`] + [`crypto::decrypt_audio`] + 本地缓存三步完成。
//! 下载链路也适配了本地文件（下载管理器会直接复制而不是再发 HTTP 请求）。

pub mod api;
pub mod crypto;
pub mod play;

use async_trait::async_trait;

use lx_core::model::lyric::LyricData;
use lx_core::model::playlist::Playlist;
use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{
    FetchError, MusicSource, ParsedLink, SearchError, SearchResult, SongUrl, SourceCapabilities,
};

use crate::http;
use crate::http::SendWithRetry;

/// 解密后的音频缓存目录：`<cache>/voicefox/soda`。
///
/// 加密曲目每次播放都要解密，落盘缓存可以避免反复下载与解密；缓存按
/// 「曲目 ID + 扩展名」命名，超过 [`CACHE_MAX_AGE`] 的旧文件在写入时顺手清掉。
fn cache_dir() -> std::path::PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("voicefox")
        .join("soda")
}

const CACHE_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);

fn prune_cache(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if now
            .duration_since(modified)
            .is_ok_and(|age| age > CACHE_MAX_AGE)
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn extension_for(stream: &play::SodaStream) -> &'static str {
    if stream.format.eq_ignore_ascii_case("flac") {
        "flac"
    } else if stream.format.eq_ignore_ascii_case("mp3") {
        "mp3"
    } else {
        "m4a"
    }
}

/// 把加密音频下载并解密到缓存目录，返回本地路径。
async fn materialize(
    track_id: &str,
    stream: &play::SodaStream,
) -> Result<std::path::PathBuf, FetchError> {
    let dir = cache_dir();
    let path = dir.join(format!("{track_id}.{}", extension_for(stream)));
    if path.is_file() {
        return Ok(path);
    }
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|error| FetchError::Other(format!("创建汽水缓存目录失败: {error}")))?;
    let encrypted = http::client()
        .get(&stream.url)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .bytes()
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?;
    let auth = stream
        .play_auth
        .clone()
        .ok_or_else(|| FetchError::Other("汽水加密音频缺少 playAuth".to_string()))?;
    let decrypted = crypto::decrypt_audio(&encrypted, &auth)
        .map_err(|error| FetchError::Other(format!("汽水解密失败: {error}")))?;
    tokio::fs::write(&path, &decrypted)
        .await
        .map_err(|error| FetchError::Other(format!("写入汽水缓存失败: {error}")))?;
    prune_cache(&dir);
    Ok(path)
}

pub struct SodaSource;

impl SodaSource {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SodaSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MusicSource for SodaSource {
    fn id(&self) -> SourceId {
        SourceId::Soda
    }

    fn name(&self) -> &str {
        "汽水音乐"
    }

    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities {
            playlists: true,
            playlist_search: true,
            link_parse: true,
            ..Default::default()
        }
    }

    async fn search(
        &self,
        keyword: &str,
        page: u32,
        limit: u32,
    ) -> Result<SearchResult, SearchError> {
        let json = api::search("track", keyword, page, limit)
            .await
            .map_err(api::ApiError::into_search)?;
        let mut items = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for group in json["result_groups"].as_array().into_iter().flatten() {
            for entry in group["data"].as_array().into_iter().flatten() {
                if let Some(song) = api::parse_track(&entry["entity"]["track"])
                    && seen.insert(song.id.clone())
                {
                    items.push(song);
                }
            }
        }
        Ok(SearchResult {
            total: items.len() as u32,
            has_more: !items.is_empty(),
            items,
        })
    }

    async fn get_song_url(&self, song: &SongInfo, quality: Quality) -> Result<SongUrl, FetchError> {
        let track_id = song
            .extra
            .get("track_id")
            .cloned()
            .unwrap_or_else(|| song.id.clone());
        let response = api::fetch_seo_track_value(&track_id)
            .await
            .map_err(api::ApiError::into_fetch)?;
        let stream = play::pick_stream(&response, quality).map_err(api::ApiError::into_fetch)?;
        let achieved = stream.quality();

        // 明文流直接给 URL；加密流解密到本地缓存后给本地路径。
        let url = if stream.is_encrypted() {
            let path = materialize(&track_id, &stream).await?;
            path.to_string_lossy().into_owned()
        } else {
            stream.url.clone()
        };
        Ok(SongUrl {
            url,
            quality: achieved,
            duration: song.duration,
            cover_url: song.cover_url.clone(),
            qualities: vec![achieved],
            headers: Vec::new(),
            size: stream.size,
            size_is_advisory: true,
            md5: None,
            candidate_urls: Vec::new(),
            max_chunk_size: 0,
        })
    }

    async fn get_lyric(&self, song: &SongInfo) -> Result<LyricData, FetchError> {
        let track_id = song
            .extra
            .get("track_id")
            .cloned()
            .unwrap_or_else(|| song.id.clone());
        let detail = api::fetch_seo_track(&track_id)
            .await
            .map_err(api::ApiError::into_fetch)?;
        let lyric = detail.lyric.ok_or(FetchError::NotFound)?;
        Ok(LyricData {
            lyric,
            ..LyricData::default()
        })
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
        page: u32,
    ) -> Result<Vec<Playlist>, SearchError> {
        let json = api::search("playlist", keyword, page, 30)
            .await
            .map_err(api::ApiError::into_search)?;
        let mut playlists = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for group in json["result_groups"].as_array().into_iter().flatten() {
            for entry in group["data"].as_array().into_iter().flatten() {
                let item = &entry["entity"]["playlist"];
                let id = api::value_string(&item["id"]);
                let name = item["title"]
                    .as_str()
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if id.is_empty() || name.is_empty() || !seen.insert(id.clone()) {
                    continue;
                }
                let mut playlist = Playlist::new(id.clone(), name, SourceId::Soda);
                playlist.cover_url = item["url_cover"]["urls"]
                    .as_array()
                    .and_then(|urls| urls.first())
                    .and_then(|url| url.as_str())
                    .filter(|url| !url.is_empty())
                    .map(str::to_string);
                playlist.song_count = item["count_tracks"].as_u64().unwrap_or_default() as u32;
                playlist.play_count = item["play_count"].as_u64();
                playlist.creator = item["owner"]["nickname"]
                    .as_str()
                    .or_else(|| item["owner"]["public_name"].as_str())
                    .map(str::to_string);
                playlist.link = Some(format!(
                    "https://music.douyin.com/qishui/share/playlist?id={id}"
                ));
                playlist.extra.insert("playlist_id".to_string(), id);
                playlists.push(playlist);
            }
        }
        Ok(playlists)
    }

    async fn get_playlist_detail(&self, id: &str, _page: u32) -> Result<Vec<SongInfo>, FetchError> {
        api::fetch_playlist_songs(id)
            .await
            .map_err(api::ApiError::into_fetch)
    }

    async fn parse_link(&self, link: &str) -> Result<ParsedLink, FetchError> {
        match api::link_target(link)
            .ok_or_else(|| FetchError::Other("无法识别汽水音乐链接".to_string()))?
        {
            api::SodaLink::Track(id) => {
                let detail = api::fetch_seo_track(&id)
                    .await
                    .map_err(api::ApiError::into_fetch)?;
                let song = api::parse_track(&detail.track)
                    .ok_or_else(|| FetchError::Other("汽水曲目信息为空".to_string()))?;
                Ok(ParsedLink::Song(Box::new(song)))
            }
            api::SodaLink::Playlist(id) => {
                let songs = api::fetch_playlist_songs(&id)
                    .await
                    .map_err(api::ApiError::into_fetch)?;
                let mut playlist =
                    Playlist::new(id.clone(), format!("汽水歌单 {id}"), SourceId::Soda);
                playlist.song_count = songs.len() as u32;
                playlist.cover_url = songs.first().and_then(|song| song.cover_url.clone());
                playlist.link = Some(format!(
                    "https://music.douyin.com/qishui/share/playlist?id={id}"
                ));
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
    fn cache_extension_follows_the_stream_format() {
        let mut stream = play::SodaStream {
            format: "flac".to_string(),
            ..Default::default()
        };
        assert_eq!(extension_for(&stream), "flac");
        stream.format = "mp3".to_string();
        assert_eq!(extension_for(&stream), "mp3");
        stream.format = String::new();
        assert_eq!(extension_for(&stream), "m4a");
    }
}
