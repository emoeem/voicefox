//! 酷我音乐 (kw) 音源
//!
//! API 协议参考: lx-music src/renderer/utils/musicSdk/kw/

mod crypto;
pub mod leaderboard;
pub mod lyric;
pub mod parse;
pub mod playlist;
pub mod search;
pub(crate) mod session;
pub mod url;

use async_trait::async_trait;

use lx_core::model::leaderboard::LeaderboardInfo;
use lx_core::model::lyric::LyricData;
use lx_core::model::playlist::{Album, Playlist};
use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{
    FetchError, MusicSource, SearchError, SearchResult, SongUrl, SourceCapabilities,
};

pub struct KwSource;

impl KwSource {
    pub fn new() -> Self {
        Self
    }
}

impl Default for KwSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MusicSource for KwSource {
    fn id(&self) -> SourceId {
        SourceId::Kw
    }

    fn name(&self) -> &str {
        "酷我音乐"
    }

    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities {
            playlists: true,
            playlist_search: true,
            playlist_categories: true,
            album: true,
            artist: true,
            leaderboard: true,
            link_parse: true,
            ..Default::default()
        }
    }

    async fn search(
        &self,
        keyword: &str,
        page: u32,
        limit: u32,
    ) -> Result<SearchResult, lx_core::traits::source::SearchError> {
        search::search(keyword, page, limit).await
    }

    async fn get_song_url(&self, song: &SongInfo, quality: Quality) -> Result<SongUrl, FetchError> {
        url::get_song_url(song, quality).await
    }

    async fn get_lyric(&self, song: &SongInfo) -> Result<LyricData, FetchError> {
        lyric::get_lyric(song).await
    }

    async fn get_cover_url(&self, song: &SongInfo) -> Result<String, FetchError> {
        url::resolve_cover_url(&super::http::client(), &song.id)
            .await
            .ok_or(FetchError::NotFound)
    }

    fn supported_qualities(&self) -> Vec<Quality> {
        vec![
            Quality::Low128,
            Quality::High320,
            Quality::Flac,
            Quality::Flac24,
        ]
    }

    async fn get_playlist_categories(
        &self,
    ) -> Result<Vec<lx_core::model::playlist::PlaylistCategory>, FetchError> {
        playlist::get_categories().await
    }

    // `tag_id` 是酷我的分类 ID，空值表示热门推荐。
    async fn get_playlists(&self, tag_id: &str, page: u32) -> Result<Vec<Playlist>, FetchError> {
        if tag_id.trim().is_empty() {
            return playlist::get_list(page).await;
        }
        playlist::get_category_list(tag_id, page).await
    }

    async fn search_playlists(
        &self,
        keyword: &str,
        page: u32,
    ) -> Result<Vec<Playlist>, SearchError> {
        playlist::search_playlists(keyword, page).await
    }

    async fn parse_link(
        &self,
        link: &str,
    ) -> Result<lx_core::traits::source::ParsedLink, FetchError> {
        parse::parse(link).await
    }

    async fn get_playlist_detail(&self, id: &str, page: u32) -> Result<Vec<SongInfo>, FetchError> {
        playlist::get_detail(id, page).await
    }

    async fn get_album_songs(
        &self,
        album: &Album,
        _page: u32,
        _limit: u32,
    ) -> Result<SearchResult, SearchError> {
        let songs = playlist::get_album_songs(&album.id)
            .await
            .map_err(|error| SearchError::Other(error.to_string()))?;
        Ok(SearchResult {
            total: songs.len() as u32,
            has_more: false,
            items: songs,
        })
    }

    async fn get_leaderboard_boards(&self) -> Result<Vec<LeaderboardInfo>, SearchError> {
        leaderboard::get_boards().await
    }

    async fn get_leaderboard(
        &self,
        id: &str,
        page: u32,
        limit: u32,
    ) -> Result<SearchResult, SearchError> {
        leaderboard::get_list(id, page, limit).await
    }
}
