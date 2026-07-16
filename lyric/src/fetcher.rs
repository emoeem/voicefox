use std::sync::Arc;

use async_trait::async_trait;

use lx_core::model::lyric::LyricData;
use lx_core::model::song::SongInfo;
use lx_core::traits::lyric_fetcher::{LyricFetchError, LyricFetcher};
use lx_core::traits::source::FetchError;
use lx_source::manager::SourceManager;

/// LyricFetcher 实现：委托给 source::SourceManager 获取歌词
pub struct SourceLyricFetcher {
    source_manager: Arc<SourceManager>,
}

impl SourceLyricFetcher {
    pub fn new(source_manager: Arc<SourceManager>) -> Self {
        Self { source_manager }
    }
}

#[async_trait]
impl LyricFetcher for SourceLyricFetcher {
    async fn fetch(&self, song: &SongInfo) -> Result<LyricData, LyricFetchError> {
        self.source_manager
            .get_lyric(song)
            .await
            .map_err(map_fetch_error)
    }
}

fn map_fetch_error(e: FetchError) -> LyricFetchError {
    match e {
        FetchError::Network(msg) => LyricFetchError::Network(msg),
        FetchError::NotFound => LyricFetchError::NotFound,
        FetchError::Parse(msg) => LyricFetchError::Parse(msg),
        FetchError::TooManyRequests => LyricFetchError::Other("too many requests".to_string()),
        FetchError::Other(msg) => LyricFetchError::Other(msg),
    }
}
