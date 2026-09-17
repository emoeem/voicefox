//! 酷狗音乐 (kg) 音源
//!
//! API 协议参考: lx-music src/renderer/utils/musicSdk/kg/

mod crypto;
pub mod leaderboard;
pub mod login;
pub mod lyric;
pub mod parse;
pub mod playlist;
pub mod search;
pub mod session;
pub mod url;

use async_trait::async_trait;

use lx_core::model::leaderboard::LeaderboardInfo;
use lx_core::model::login::{QrLoginResult, QrLoginSession};
use lx_core::model::lyric::LyricData;
use lx_core::model::playlist::{Album, Playlist};
use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{
    FetchError, MusicSource, SearchError, SearchResult, SongUrl, SourceCapabilities,
};

pub struct KgSource;

/// 给请求带上登录 cookie；未登录时原样返回。
///
/// 酷狗的高音质（FLAC / 无损）地址需要登录态与设备 cookie。
pub(super) fn with_cookie(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    match session::cookie_header() {
        Some(cookie) => request.header("Cookie", cookie),
        None => request,
    }
}

impl KgSource {
    pub fn new() -> Self {
        Self
    }
}

impl Default for KgSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MusicSource for KgSource {
    fn id(&self) -> SourceId {
        SourceId::Kg
    }

    fn name(&self) -> &str {
        "酷狗音乐"
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
            login: true,
            qr_login: true,
            user_playlists: true,
            ..Default::default()
        }
    }

    async fn create_qr_login(&self) -> Result<QrLoginSession, FetchError> {
        login::create().await
    }

    async fn check_qr_login(&self, key: &str) -> Result<QrLoginResult, FetchError> {
        login::check(key).await
    }

    async fn get_user_playlists(&self, page: u32, limit: u32) -> Result<Vec<Playlist>, FetchError> {
        playlist::get_user_playlists(page, limit).await
    }

    fn logout(&self) -> Result<(), FetchError> {
        session::logout().map_err(FetchError::Other)
    }

    fn is_logged_in(&self) -> bool {
        session::is_logged_in()
    }

    async fn search(
        &self,
        keyword: &str,
        page: u32,
        limit: u32,
    ) -> Result<SearchResult, SearchError> {
        search::search(keyword, page, limit).await
    }

    async fn get_song_url(&self, song: &SongInfo, quality: Quality) -> Result<SongUrl, FetchError> {
        url::get_song_url(song, quality).await
    }

    async fn get_lyric(&self, song: &SongInfo) -> Result<LyricData, FetchError> {
        lyric::get_lyric(song).await
    }

    async fn get_cover_url(&self, song: &SongInfo) -> Result<String, FetchError> {
        // kg 封面需要额外 API 调用，简单返回空
        Ok(song.cover_url.clone().unwrap_or_default())
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

    // `tag_id` 是酷狗的分类 ID（`id:tagid`），空值表示热门歌单。
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

    async fn get_playlist_detail(&self, id: &str, _page: u32) -> Result<Vec<SongInfo>, FetchError> {
        playlist::get_detail(id).await
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
