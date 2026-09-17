//! JOOX 音源（腾讯系海外平台）。
//!
//! 已实现：搜索、播放地址、歌词、链接直解（单曲/歌单/专辑）、歌单搜索与曲目。
//! 尚未实现：歌单分类与专辑曲目——两者在 music-lib 里都要解析 `www.joox.com`
//! 的网页内嵌 JSON（还带 base64 标题），打算和汽水那条链一起补。

pub mod api;

use async_trait::async_trait;

use lx_core::model::lyric::LyricData;
use lx_core::model::playlist::Playlist;
use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{
    FetchError, MusicSource, ParsedLink, SearchError, SearchResult, SongUrl, SourceCapabilities,
};

/// JOOX 的 `song_info` 里没有「文件大小」，下载引擎只做单连接流式。
fn song_url_from(quality: Quality, url: String, song: &SongInfo) -> SongUrl {
    SongUrl {
        url,
        quality,
        duration: song.duration,
        cover_url: song.cover_url.clone(),
        qualities: vec![quality],
        headers: Vec::new(),
        size: None,
        size_is_advisory: true,
        md5: None,
        candidate_urls: Vec::new(),
        max_chunk_size: 0,
    }
}

pub struct JooxSource;

impl JooxSource {
    pub fn new() -> Self {
        Self
    }
}

impl Default for JooxSource {
    fn default() -> Self {
        Self::new()
    }
}

/// 链接识别：单曲 `/single/{id}`、歌单 `/playlist/{id}`、专辑 `/album/{id}`。
fn link_target(link: &str) -> Option<(LinkKind, String)> {
    let trimmed = link.trim();
    let patterns = [
        ("single", LinkKind::Song),
        ("playlist", LinkKind::Playlist),
        ("album", LinkKind::Album),
    ];
    for (segment, kind) in patterns {
        let marker = format!("/{segment}/");
        if let Some(index) = trimmed.find(&marker) {
            let rest = &trimmed[index + marker.len()..];
            let id = rest
                .split(['/', '?', '#'])
                .next()
                .map(api::normalize_id)
                .unwrap_or_default();
            if !id.is_empty() {
                return Some((kind, id));
            }
        }
    }
    // 裸 ID：分享文案里常见。JOOX 的 ID 是 base64 风格的长串，
    // 用长度 + 字符集双重判断，避免把「歌手 歌名」这类文本当成 ID。
    if !trimmed.contains('/')
        && trimmed.len() > 10
        && trimmed
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "+/-_=".contains(character))
    {
        return Some((LinkKind::Song, api::normalize_id(trimmed)));
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkKind {
    Song,
    Playlist,
    Album,
}

#[async_trait]
impl MusicSource for JooxSource {
    fn id(&self) -> SourceId {
        SourceId::Joox
    }

    fn name(&self) -> &str {
        "JOOX"
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
        let json = api::search(keyword)
            .await
            .map_err(api::ApiError::into_search)?;
        let items = api::songs_from_search(&json);
        Ok(SearchResult {
            total: items.len() as u32,
            has_more: false,
            items,
        })
    }

    async fn get_song_url(&self, song: &SongInfo, quality: Quality) -> Result<SongUrl, FetchError> {
        let id = song
            .extra
            .get("songid")
            .cloned()
            .unwrap_or_else(|| song.id.clone());
        let detail = api::fetch_song_detail(&id)
            .await
            .map_err(api::ApiError::into_fetch)?;
        // 档位从高到低挑第一个可用的。
        let mut candidates = detail.urls;
        candidates.sort_by_key(|right| std::cmp::Reverse(right.0));
        let (achieved, url) = candidates
            .iter()
            .find(|(candidate, _)| *candidate <= quality)
            .or_else(|| candidates.first())
            .cloned()
            .ok_or_else(|| FetchError::Other("JOOX 未返回可用地址".to_string()))?;
        Ok(song_url_from(achieved, url, &detail.song))
    }

    async fn get_lyric(&self, song: &SongInfo) -> Result<LyricData, FetchError> {
        let id = song
            .extra
            .get("songid")
            .cloned()
            .unwrap_or_else(|| song.id.clone());
        let lyric = api::fetch_lyric(&id)
            .await
            .map_err(api::ApiError::into_fetch)?
            .ok_or(FetchError::NotFound)?;
        Ok(LyricData {
            lyric,
            ..LyricData::default()
        })
    }

    async fn get_cover_url(&self, song: &SongInfo) -> Result<String, FetchError> {
        Ok(song.cover_url.clone().unwrap_or_default())
    }

    fn supported_qualities(&self) -> Vec<Quality> {
        vec![Quality::Low128, Quality::High320]
    }

    async fn search_playlists(
        &self,
        keyword: &str,
        _page: u32,
    ) -> Result<Vec<Playlist>, SearchError> {
        let json = api::search(keyword)
            .await
            .map_err(api::ApiError::into_search)?;
        Ok(api::playlists_from_sections(&json))
    }

    async fn get_playlist_detail(&self, id: &str, _page: u32) -> Result<Vec<SongInfo>, FetchError> {
        let json = api::playlist_songs(id)
            .await
            .map_err(api::ApiError::into_fetch)?;
        let songs = api::songs_from_sections(&json);
        if songs.is_empty() {
            return Err(FetchError::NotFound);
        }
        Ok(songs)
    }

    /// 专辑曲目：OpenJOOX 的专辑是特殊歌单，走同一接口，再用 `album` 信息补名字。
    async fn get_album_songs(
        &self,
        album: &lx_core::model::playlist::Album,
        _page: u32,
        _limit: u32,
    ) -> Result<SearchResult, SearchError> {
        let json = api::playlist_songs(&album.id)
            .await
            .map_err(|error| SearchError::Other(error.to_string()))?;
        let items = api::songs_from_sections(&json);
        Ok(SearchResult {
            total: items.len() as u32,
            has_more: false,
            items,
        })
    }

    async fn parse_link(&self, link: &str) -> Result<ParsedLink, FetchError> {
        let (kind, id) =
            link_target(link).ok_or_else(|| FetchError::Other("无法识别 JOOX 链接".to_string()))?;
        match kind {
            LinkKind::Song => {
                let detail = api::fetch_song_detail(&id)
                    .await
                    .map_err(api::ApiError::into_fetch)?;
                Ok(ParsedLink::Song(Box::new(detail.song)))
            }
            LinkKind::Playlist => {
                let json = api::playlist_songs(&id)
                    .await
                    .map_err(api::ApiError::into_fetch)?;
                let songs = api::songs_from_sections(&json);
                if songs.is_empty() {
                    return Err(FetchError::NotFound);
                }
                let name = json["name"]
                    .as_str()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("JOOX 歌单 {id}"));
                let mut playlist = Playlist::new(id.clone(), name, SourceId::Joox);
                playlist.song_count = songs.len() as u32;
                playlist.cover_url = songs.first().and_then(|song| song.cover_url.clone());
                playlist.link = Some(format!("https://www.joox.com/sg/playlist/{id}"));
                Ok(ParsedLink::Playlist {
                    playlist: Box::new(playlist),
                    songs,
                })
            }
            LinkKind::Album => {
                // 专辑曲目同样能从 OpenJOOX 的 playlist 接口取到（专辑是特殊歌单）。
                let json = api::playlist_songs(&id)
                    .await
                    .map_err(api::ApiError::into_fetch)?;
                let songs = api::songs_from_sections(&json);
                if songs.is_empty() {
                    return Err(FetchError::NotFound);
                }
                let name = json["name"]
                    .as_str()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("JOOX 专辑 {id}"));
                let mut album = Playlist::new(id.clone(), name, SourceId::Joox);
                album.song_count = songs.len() as u32;
                album.cover_url = songs.first().and_then(|song| song.cover_url.clone());
                album.creator = songs.first().map(|song| song.singer.clone());
                album.link = Some(format!("https://www.joox.com/hk/album/{id}"));
                Ok(ParsedLink::Album {
                    playlist: Box::new(album),
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
    fn recognises_joox_links() {
        assert_eq!(
            link_target("https://www.joox.com/hk/single/abc123"),
            Some((LinkKind::Song, "abc123".to_string()))
        );
        assert_eq!(
            link_target("https://www.joox.com/sg/playlist/pl123?lang=zh_cn"),
            Some((LinkKind::Playlist, "pl123".to_string()))
        );
        assert_eq!(
            link_target("https://www.joox.com/hk/album/al123"),
            Some((LinkKind::Album, "al123".to_string()))
        );
        assert_eq!(link_target("https://www.joox.com/"), None);
        assert_eq!(link_target("周杰伦"), None);
    }

    #[test]
    fn bare_share_ids_are_treated_as_songs() {
        assert_eq!(
            link_target("AbCdEf12345xyz"),
            Some((LinkKind::Song, "AbCdEf12345xyz".to_string()))
        );
        // 文本关键词不能被当成 ID。
        assert_eq!(link_target("周杰伦 晴天"), None);
        assert_eq!(link_target("short"), None);
    }
}
