//! 千千音乐（91q）音源。
//!
//! 接口协议参考 music-lib 的 `qianqian` 包：所有接口都要带
//! `timestamp` 与 `sign`（见 [`crypto`]），搜索按 `type` 区分歌曲/专辑/歌单。

pub mod album;
pub mod crypto;
pub mod lyric;
pub mod parse;
pub mod playlist;
pub mod song;
pub mod url;

use async_trait::async_trait;

use lx_core::model::lyric::LyricData;
use lx_core::model::playlist::{Album, Playlist, PlaylistCategory};
use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{
    FetchError, MusicSource, ParsedLink, SearchError, SearchResult, SongUrl, SourceCapabilities,
};

pub struct QianqianSource;

impl QianqianSource {
    pub fn new() -> Self {
        Self
    }
}

impl Default for QianqianSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MusicSource for QianqianSource {
    fn id(&self) -> SourceId {
        SourceId::Qianqian
    }

    fn name(&self) -> &str {
        "千千音乐"
    }

    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities {
            playlists: true,
            playlist_search: true,
            playlist_categories: true,
            album: true,
            artist: true,
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
        song::search_songs(keyword, page, limit).await
    }

    async fn get_song_url(&self, song: &SongInfo, quality: Quality) -> Result<SongUrl, FetchError> {
        url::get_song_url(song, quality).await
    }

    async fn get_lyric(&self, song: &SongInfo) -> Result<LyricData, FetchError> {
        lyric::get_lyric(song).await
    }

    async fn get_cover_url(&self, song: &SongInfo) -> Result<String, FetchError> {
        Ok(song.cover_url.clone().unwrap_or_default())
    }

    fn supported_qualities(&self) -> Vec<Quality> {
        vec![Quality::Low128, Quality::High320, Quality::Flac]
    }

    async fn get_playlist_categories(&self) -> Result<Vec<PlaylistCategory>, FetchError> {
        playlist::get_categories().await
    }

    // `tag_id` 是分类目录里的 subCateId，空值表示全部。
    async fn get_playlists(&self, tag_id: &str, page: u32) -> Result<Vec<Playlist>, FetchError> {
        playlist::get_category_list(tag_id, page).await
    }

    async fn search_playlists(
        &self,
        keyword: &str,
        page: u32,
    ) -> Result<Vec<Playlist>, SearchError> {
        playlist::search_playlists(keyword, page).await
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
        let songs = album::get_album_songs(&album.id)
            .await
            .map_err(|error| SearchError::Other(error.to_string()))?;
        Ok(SearchResult {
            total: songs.len() as u32,
            has_more: false,
            items: songs,
        })
    }

    async fn parse_link(&self, link: &str) -> Result<ParsedLink, FetchError> {
        parse::parse(link).await
    }
}
