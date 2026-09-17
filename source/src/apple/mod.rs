//! Apple Music 音源。
//!
//! 关键限制先说清楚：**Apple Music 只能拿到 30 秒试听片段**，完整曲目是
//! DRM 加密的，需要 gamdl 一类的解密工具（music-lib 也是这么标注的）。
//! 因此这里把试听地址作为可播放地址返回，并在能力声明里不提供无损，
//! 避免让用户以为能下到整首。

use async_trait::async_trait;

mod api;

use lx_core::model::lyric::LyricData;
use lx_core::model::playlist::{Album, Playlist};
use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{
    FetchError, MusicSource, ParsedLink, SearchError, SearchResult, SongUrl, SourceCapabilities,
};

pub struct AppleSource;

/// 试听片段只有 30 秒，界面提示语里带上这一条，避免用户以为下载失败。
pub const PREVIEW_HINT: &str = "Apple Music 仅提供 30 秒试听片段";

impl AppleSource {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AppleSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MusicSource for AppleSource {
    fn id(&self) -> SourceId {
        SourceId::Apple
    }

    fn name(&self) -> &str {
        "Apple Music"
    }

    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities {
            playlists: true,
            playlist_search: true,
            playlist_categories: true,
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
        let items = api::search(keyword)
            .await
            .map_err(api::ApiError::into_search)?;
        Ok(SearchResult {
            total: items.len() as u32,
            has_more: false,
            items,
        })
    }

    /// 只返回试听片段；没有试听地址时给出明确原因。
    async fn get_song_url(
        &self,
        song: &SongInfo,
        _quality: Quality,
    ) -> Result<SongUrl, FetchError> {
        let detail = api::fetch_song(&song.id)
            .await
            .map_err(api::ApiError::into_fetch)?;
        let url = detail.url.clone().ok_or_else(|| {
            FetchError::Other("Apple Music 仅提供 30 秒试听，该曲目没有试听片段".to_string())
        })?;
        Ok(SongUrl {
            url,
            quality: Quality::Low128,
            duration: detail.song.duration,
            cover_url: detail.song.cover_url.clone(),
            qualities: vec![Quality::Low128],
            headers: Vec::new(),
            size: None,
            size_is_advisory: true,
            md5: None,
            candidate_urls: Vec::new(),
            max_chunk_size: 0,
        })
    }

    async fn get_lyric(&self, song: &SongInfo) -> Result<LyricData, FetchError> {
        let lyric = api::fetch_lyric(&song.id)
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

    /// 只声明试听档位：完整曲目需要 DRM 解密，不在能力范围内。
    fn supported_qualities(&self) -> Vec<Quality> {
        vec![Quality::Low128]
    }

    async fn search_playlists(
        &self,
        keyword: &str,
        _page: u32,
    ) -> Result<Vec<Playlist>, SearchError> {
        api::search_playlists(keyword)
            .await
            .map_err(api::ApiError::into_search)
    }

    async fn get_playlist_categories(
        &self,
    ) -> Result<Vec<lx_core::model::playlist::PlaylistCategory>, FetchError> {
        Ok(api::curator_categories())
    }

    async fn get_playlists(&self, tag_id: &str, page: u32) -> Result<Vec<Playlist>, FetchError> {
        if tag_id.trim().is_empty() {
            return Err(FetchError::Other("Apple Music 需要先选择分类".to_string()));
        }
        api::category_playlists(tag_id, page, 20)
            .await
            .map_err(api::ApiError::into_fetch)
    }

    async fn get_playlist_detail(&self, id: &str, _page: u32) -> Result<Vec<SongInfo>, FetchError> {
        let (_, songs) = api::fetch_playlist(id)
            .await
            .map_err(api::ApiError::into_fetch)?;
        Ok(songs)
    }

    async fn get_album_songs(
        &self,
        album: &Album,
        _page: u32,
        _limit: u32,
    ) -> Result<SearchResult, SearchError> {
        let (_, songs) = api::fetch_album(&album.id)
            .await
            .map_err(api::ApiError::into_search)?;
        Ok(SearchResult {
            total: songs.len() as u32,
            has_more: false,
            items: songs,
        })
    }

    async fn parse_link(&self, link: &str) -> Result<ParsedLink, FetchError> {
        match api::link_target(link)
            .ok_or_else(|| FetchError::Other("无法识别 Apple Music 链接".to_string()))?
        {
            api::AppleLink::Song(id) => {
                let detail = api::fetch_song(&id)
                    .await
                    .map_err(api::ApiError::into_fetch)?;
                Ok(ParsedLink::Song(Box::new(detail.song)))
            }
            api::AppleLink::Album(id) => {
                let (playlist, songs) = api::fetch_album(&id)
                    .await
                    .map_err(api::ApiError::into_fetch)?;
                Ok(ParsedLink::Album {
                    playlist: Box::new(playlist),
                    songs,
                })
            }
            api::AppleLink::Playlist(id) => {
                let (playlist, songs) = api::fetch_playlist(&id)
                    .await
                    .map_err(api::ApiError::into_fetch)?;
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
    fn source_declares_preview_only_quality() {
        let source = AppleSource::new();
        assert_eq!(source.supported_qualities(), vec![Quality::Low128]);
        assert_eq!(source.id(), SourceId::Apple);
    }
}
