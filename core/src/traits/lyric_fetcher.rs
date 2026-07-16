use async_trait::async_trait;

use crate::model::lyric::LyricData;
use crate::model::song::SongInfo;

/// 歌词获取抽象
#[async_trait]
pub trait LyricFetcher: Send + Sync {
    async fn fetch(&self, song: &SongInfo) -> Result<LyricData, LyricFetchError>;
}

#[derive(Debug, thiserror::Error)]
pub enum LyricFetchError {
    #[error("network error: {0}")]
    Network(String),
    #[error("not found")]
    NotFound,
    #[error("parse error: {0}")]
    Parse(String),
    #[error("{0}")]
    Other(String),
}
