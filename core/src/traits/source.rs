use async_trait::async_trait;
use std::time::Duration;

use crate::model::leaderboard::LeaderboardInfo;
use crate::model::lyric::LyricData;
use crate::model::playlist::{Playlist, Tag};
use crate::model::song::SongInfo;
use crate::model::source::{Quality, SourceId};

/// 搜索结果
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub items: Vec<SongInfo>,
    pub total: u32,
    pub has_more: bool,
}

/// 播放 URL 结果
#[derive(Debug, Clone)]
pub struct SongUrl {
    pub url: String,
    pub quality: Quality,
    pub duration: Duration,
    pub cover_url: Option<String>,
    pub qualities: Vec<Quality>,
}

/// 音源统一接口
#[async_trait]
pub trait MusicSource: Send + Sync {
    /// 音源唯一标识
    fn id(&self) -> SourceId;
    /// 音源显示名称
    fn name(&self) -> &str;

    /// 搜索歌曲
    async fn search(
        &self,
        keyword: &str,
        page: u32,
        limit: u32,
    ) -> Result<SearchResult, SearchError>;
    /// 获取播放 URL
    async fn get_song_url(&self, song: &SongInfo, quality: Quality) -> Result<SongUrl, FetchError>;
    /// 获取歌词
    async fn get_lyric(&self, song: &SongInfo) -> Result<LyricData, FetchError>;
    /// 获取封面 URL
    async fn get_cover_url(&self, song: &SongInfo) -> Result<String, FetchError>;

    /// 支持的音质列表
    fn supported_qualities(&self) -> Vec<Quality>;

    // --- 可选实现 ---
    async fn get_playlist_tags(&self) -> Result<Vec<Tag>, FetchError> {
        Ok(vec![])
    }
    async fn get_playlists(&self, _tag_id: &str, _page: u32) -> Result<Vec<Playlist>, FetchError> {
        Ok(vec![])
    }
    async fn get_playlist_detail(
        &self,
        _id: &str,
        _page: u32,
    ) -> Result<Vec<SongInfo>, FetchError> {
        Ok(vec![])
    }
    async fn get_leaderboard_boards(&self) -> Result<Vec<LeaderboardInfo>, SearchError> {
        Err(SearchError::Other("该音源不支持排行榜".to_string()))
    }
    async fn get_leaderboard(
        &self,
        _id: &str,
        _page: u32,
        _limit: u32,
    ) -> Result<SearchResult, SearchError> {
        Err(SearchError::Other("该音源不支持排行榜".to_string()))
    }
}

/// 搜索错误
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("network error: {0}")]
    Network(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("api error: {0}")]
    Api(String),
    #[error("{0}")]
    Other(String),
}

/// 获取错误
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("network error: {0}")]
    Network(String),
    #[error("not found")]
    NotFound,
    #[error("too many requests")]
    TooManyRequests,
    #[error("parse error: {0}")]
    Parse(String),
    #[error("{0}")]
    Other(String),
}
