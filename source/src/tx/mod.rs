//! QQ音乐 (tx) 音源
//!
//! API 协议参考: lx-music src/renderer/utils/musicSdk/tx/

mod crypto;
pub mod leaderboard;
pub mod lyric;
pub mod search;
pub mod url;

use async_trait::async_trait;

use lx_core::model::leaderboard::LeaderboardInfo;
use lx_core::model::lyric::LyricData;
use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{FetchError, MusicSource, SearchError, SearchResult, SongUrl};

pub struct TxSource;

impl TxSource {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl MusicSource for TxSource {
    fn id(&self) -> SourceId {
        SourceId::Tx
    }

    fn name(&self) -> &str {
        "QQ音乐"
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
