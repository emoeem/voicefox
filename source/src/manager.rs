//! 音源管理器：注册、调度、换源匹配
//!
//! 对标 lx-music src/renderer/utils/musicSdk/index.js

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use lx_core::model::leaderboard::LeaderboardInfo;
use lx_core::model::lyric::LyricData;
use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{FetchError, MusicSource, SearchError, SearchResult, SongUrl};

use crate::kg::KgSource;
use crate::kw::KwSource;
use crate::local::LocalSource;
use crate::mg::MgSource;
use crate::tx::TxSource;
use crate::wy::WySource;

struct JsSourceState {
    generation: u64,
    source: Option<Arc<dyn MusicSource>>,
}

/// 音源管理器
pub struct SourceManager {
    sources: HashMap<SourceId, Arc<dyn MusicSource>>,
    /// JS 自定义音源（MVP：只支持一个），用 RwLock 支持后台异步设置
    js_source: std::sync::RwLock<JsSourceState>,
    /// 本地音乐源（单独存储以便调用扫描等特有方法）
    local_source: Arc<LocalSource>,
    default: SourceId,
    enabled: HashSet<SourceId>,
}

impl SourceManager {
    pub fn new(default: SourceId, enabled: &[SourceId]) -> Self {
        let local_source = Arc::new(LocalSource::new());
        let mut manager = Self {
            sources: HashMap::new(),
            js_source: std::sync::RwLock::new(JsSourceState {
                generation: 0,
                source: None,
            }),
            local_source: Arc::clone(&local_source),
            default,
            enabled: enabled.iter().copied().collect(),
        };
        // 注册内置音源
        manager.register(Arc::new(KwSource::new()));
        manager.register(Arc::new(KgSource::new()));
        manager.register(Arc::new(MgSource::new()));
        manager.register(Arc::new(TxSource::new()));
        manager.register(Arc::new(WySource::new()));
        // 注册本地音源
        manager.register(local_source);
        manager
    }

    pub fn register(&mut self, source: Arc<dyn MusicSource>) {
        self.sources.insert(source.id(), source);
    }

    /// 开始一次 JS 音源请求。代次和当前音源受同一把锁保护，
    /// 避免旧任务在检查代次后跨过删除或新导入操作写回。
    pub fn begin_js_source_request(&self, clear_current: bool) -> u64 {
        let mut state = self.js_source.write().unwrap();
        state.generation = state.generation.wrapping_add(1);
        if clear_current {
            state.source = None;
        }
        state.generation
    }

    pub fn is_js_source_request_current(&self, generation: u64) -> bool {
        self.js_source.read().unwrap().generation == generation
    }

    pub fn set_js_source_if_current(&self, generation: u64, source: Arc<dyn MusicSource>) -> bool {
        let mut state = self.js_source.write().unwrap();
        if state.generation != generation {
            return false;
        }
        state.source = Some(source);
        true
    }

    pub fn clear_js_source_if_current(&self, generation: u64) -> bool {
        let mut state = self.js_source.write().unwrap();
        if state.generation != generation {
            return false;
        }
        state.source = None;
        true
    }

    /// 检查是否有 JS 音源
    pub fn has_js_source(&self) -> bool {
        self.js_source.read().unwrap().source.is_some()
    }

    pub fn get(&self, id: SourceId) -> Option<Arc<dyn MusicSource>> {
        self.sources.get(&id).map(Arc::clone)
    }

    /// 获取本地音乐源（可直接调用 scan 等特有方法）
    pub fn local_source(&self) -> Arc<LocalSource> {
        Arc::clone(&self.local_source)
    }

    pub fn default_source(&self) -> Arc<dyn MusicSource> {
        self.sources
            .get(&self.default)
            .map(Arc::clone)
            .expect("default source must be registered")
    }

    /// lx-music user API v3 只负责播放地址/歌词/封面，搜索仍由内置源完成。
    pub async fn search(
        &self,
        keyword: &str,
        page: u32,
        limit: u32,
    ) -> Result<SearchResult, SearchError> {
        if !self.enabled.contains(&self.default) {
            return Err(SearchError::Other(format!(
                "默认音源 {} 未启用",
                self.default.as_str()
            )));
        }
        self.default_source().search(keyword, page, limit).await
    }

    pub async fn search_scoped(
        &self,
        keyword: &str,
        page: u32,
        limit: u32,
        source: Option<SourceId>,
    ) -> Result<SearchResult, SearchError> {
        let Some(source) = source else {
            return self.search_all(keyword, page, limit).await;
        };
        if source != SourceId::Local && !self.enabled.contains(&source) {
            return Err(SearchError::Other(format!(
                "音源 {} 未启用",
                source.as_str()
            )));
        }
        let source = self
            .sources
            .get(&source)
            .map(Arc::clone)
            .ok_or_else(|| SearchError::Other("音源不可用".to_string()))?;
        source.search(keyword, page, limit).await
    }

    pub async fn search_all(
        &self,
        keyword: &str,
        page: u32,
        limit: u32,
    ) -> Result<SearchResult, SearchError> {
        let per_source_limit = (limit / 2).max(10);
        let mut tasks = tokio::task::JoinSet::new();
        for source_id in SourceId::all_online() {
            if !self.enabled.contains(source_id) {
                continue;
            }
            if let Some(source) = self.sources.get(source_id) {
                let source = Arc::clone(source);
                let keyword = keyword.to_string();
                tasks.spawn(async move {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(10),
                        source.search(&keyword, page, per_source_limit),
                    )
                    .await
                });
            }
        }

        let mut items = Vec::new();
        let mut total = 0u32;
        let mut has_more = false;
        let mut success_count = 0usize;
        let mut errors = Vec::new();
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok(Ok(result))) => {
                    success_count += 1;
                    total = total.saturating_add(result.total);
                    has_more |= result.has_more;
                    items.extend(result.items);
                }
                Ok(Ok(Err(error))) => errors.push(error.to_string()),
                Ok(Err(_)) => errors.push("请求超时".to_string()),
                Err(error) => errors.push(error.to_string()),
            }
        }

        if success_count == 0 {
            return Err(SearchError::Other(format!(
                "所有音源搜索失败: {}",
                errors.join("; ")
            )));
        }

        items.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.singer.to_lowercase().cmp(&b.singer.to_lowercase()))
                .then_with(|| a.source.as_str().cmp(b.source.as_str()))
        });
        Ok(SearchResult {
            items,
            total,
            has_more,
        })
    }

    pub async fn leaderboard(
        &self,
        source: SourceId,
        board_id: &str,
        page: u32,
        limit: u32,
    ) -> Result<SearchResult, SearchError> {
        self.online_source(source)?
            .get_leaderboard(board_id, page, limit)
            .await
    }

    pub async fn leaderboard_boards(
        &self,
        source: SourceId,
    ) -> Result<Vec<LeaderboardInfo>, SearchError> {
        self.online_source(source)?.get_leaderboard_boards().await
    }

    pub fn leaderboard_sources(&self) -> Vec<SourceId> {
        SourceId::all_online()
            .iter()
            .copied()
            .filter(|source| self.enabled.contains(source) && self.sources.contains_key(source))
            .collect()
    }

    fn online_source(&self, source: SourceId) -> Result<Arc<dyn MusicSource>, SearchError> {
        if source == SourceId::Local || !self.enabled.contains(&source) {
            return Err(SearchError::Other(format!(
                "音源 {} 未启用",
                source.as_str()
            )));
        }
        self.sources
            .get(&source)
            .map(Arc::clone)
            .ok_or_else(|| SearchError::Other("音源不可用".to_string()))
    }

    /// 获取歌曲播放地址。
    /// 在线歌曲优先使用 JS 音源，失败或未导入时回退到对应内置音源。
    pub async fn get_song_url(
        &self,
        song: &SongInfo,
        quality: Quality,
    ) -> Result<SongUrl, FetchError> {
        // 本地歌曲走本地音源
        if song.source == SourceId::Local {
            if let Some(local_src) = self.sources.get(&SourceId::Local) {
                return local_src.get_song_url(song, quality).await;
            }
            return Err(FetchError::Other("本地音源不可用".to_string()));
        }
        // 在线歌曲优先使用 JS 音源。
        let js_source = self
            .js_source
            .read()
            .unwrap()
            .source
            .as_ref()
            .map(Arc::clone);
        let js_error = if let Some(js_source) = js_source {
            match js_source.get_song_url(song, quality).await {
                Ok(result) => return Ok(result),
                Err(error) => Some(error),
            }
        } else {
            None
        };

        let source = self
            .sources
            .get(&song.source)
            .map(Arc::clone)
            .ok_or_else(|| FetchError::Other("歌曲来源不可用".to_string()))?;
        match source.get_song_url(song, quality).await {
            Ok(result) => Ok(result),
            Err(builtin_error) => {
                if let Some(js_error) = js_error {
                    Err(FetchError::Other(format!(
                        "JS 音源失败: {js_error}; 内置音源失败: {builtin_error}"
                    )))
                } else {
                    Err(builtin_error)
                }
            }
        }
    }

    /// 优先使用已导入的 lx-music JS 音源获取歌词，空结果时回退到内置搜索源。
    pub async fn get_lyric(&self, song: &SongInfo) -> Result<LyricData, FetchError> {
        let js_source = self
            .js_source
            .read()
            .unwrap()
            .source
            .as_ref()
            .map(Arc::clone);
        if let Some(js_source) = js_source
            && let Ok(data) = js_source.get_lyric(song).await
            && lyric_has_content(&data)
        {
            return Ok(data);
        }

        let source = self
            .sources
            .get(&song.source)
            .map(Arc::clone)
            .ok_or_else(|| FetchError::Other("歌曲来源不可用".to_string()))?;
        source.get_lyric(song).await
    }

    /// 优先使用搜索结果中的封面，其次请求 JS 音源，最后回退到内置搜索源。
    pub async fn get_cover_url(&self, song: &SongInfo) -> Result<String, FetchError> {
        if let Some(url) = song.cover_url.as_ref().filter(|url| !url.trim().is_empty()) {
            return Ok(url.clone());
        }
        let js_source = self
            .js_source
            .read()
            .unwrap()
            .source
            .as_ref()
            .map(Arc::clone);
        if let Some(js_source) = js_source
            && let Ok(url) = js_source.get_cover_url(song).await
            && !url.trim().is_empty()
        {
            return Ok(url);
        }

        let source = self
            .sources
            .get(&song.source)
            .map(Arc::clone)
            .ok_or_else(|| FetchError::Other("歌曲来源不可用".to_string()))?;
        source.get_cover_url(song).await
    }

    /// 跨源匹配：在其他音源中搜索同名歌曲
    /// 参考 lx-music findMusic 算法
    pub async fn find_music(&self, song: &SongInfo) -> Vec<SongInfo> {
        let exclude = song.source;
        let keyword = format!("{} {}", song.name, song.singer);

        // 1. 并行搜索所有其他源
        let mut tasks = tokio::task::JoinSet::new();
        for id in SourceId::all_online() {
            if *id == exclude || !self.enabled.contains(id) {
                continue;
            }
            if let Some(source) = self.sources.get(id) {
                let src = Arc::clone(source);
                let kw = keyword.clone();
                tasks.spawn(async move {
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(8),
                        src.search(&kw, 1, 25),
                    )
                    .await
                    {
                        Ok(Ok(result)) => Some(result.items),
                        _ => None,
                    }
                });
            }
        }

        // 2. 收集所有结果
        let mut all: Vec<SongInfo> = Vec::new();
        while let Some(task) = tasks.join_next().await {
            if let Ok(Some(items)) = task {
                all.extend(items);
            }
        }

        // 3. 预处理：计算匹配用字段
        let target_name = crate::filter::filter_str(&song.name).to_lowercase();
        let target_singer =
            crate::filter::filter_str(&crate::filter::sort_singer(&song.singer)).to_lowercase();
        let target_interval = song.duration.as_secs() as i64;

        // 4. 过滤
        all.retain(|s| {
            let f_name = crate::filter::filter_str(&s.name).to_lowercase();
            let f_singer =
                crate::filter::filter_str(&crate::filter::sort_singer(&s.singer)).to_lowercase();
            let f_interval = s.duration.as_secs() as i64;
            let f_album = crate::filter::filter_str(&s.album_name).to_lowercase();

            // 时长匹配 (允许 ±5秒)
            if target_interval > 0 && f_interval > 0 && (target_interval - f_interval).abs() >= 5 {
                return false;
            }

            // 三层匹配
            f_name == target_name && f_singer.contains(&target_singer)
                || f_singer == target_singer && f_name.contains(&target_name)
                || (!f_album.is_empty()
                    && f_album == target_name
                    && f_singer.contains(&target_singer)
                    && f_name.contains(&target_name))
        });

        // 5. 排序（按匹配度）
        all.sort_by(|a, b| {
            let a_score = match_score(a, &target_name, &target_singer, target_interval);
            let b_score = match_score(b, &target_name, &target_singer, target_interval);
            b_score.cmp(&a_score)
        });

        all
    }
}

fn lyric_has_content(data: &LyricData) -> bool {
    !data.lyric.trim().is_empty()
        || data
            .tlyric
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        || data
            .rlyric
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        || data
            .lxlyric
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
}

/// 计算匹配分数（越高越匹配）
fn match_score(s: &SongInfo, t_name: &str, t_singer: &str, t_intv: i64) -> i32 {
    let f_name = crate::filter::filter_str(&s.name).to_lowercase();
    let f_singer = crate::filter::filter_str(&crate::filter::sort_singer(&s.singer)).to_lowercase();
    let f_intv = s.duration.as_secs() as i64;

    let mut score = 0;
    if f_singer == *t_singer {
        score += 30;
    }
    if f_name == *t_name {
        score += 30;
    }
    if (f_intv - t_intv).abs() < 2 {
        score += 20;
    }
    if f_name.contains(t_name) || t_name.contains(&f_name) {
        score += 10;
    }
    score
}

#[cfg(test)]
mod tests {
    use super::{SourceManager, lyric_has_content};
    use lx_core::model::lyric::LyricData;
    use lx_core::model::source::SourceId;
    use std::sync::Arc;

    #[test]
    fn translated_lyrics_count_as_content() {
        let data = LyricData {
            tlyric: Some("[00:01.00]translation".to_string()),
            ..LyricData::default()
        };

        assert!(lyric_has_content(&data));
        assert!(!lyric_has_content(&LyricData::default()));
    }

    #[test]
    fn stale_js_source_request_cannot_commit() {
        let manager = SourceManager::new(SourceId::Kw, SourceId::all_online());
        let stale_generation = manager.begin_js_source_request(false);
        let current_generation = manager.begin_js_source_request(true);

        assert!(!manager.set_js_source_if_current(
            stale_generation,
            Arc::new(crate::local::LocalSource::new()),
        ));
        assert!(manager.set_js_source_if_current(
            current_generation,
            Arc::new(crate::local::LocalSource::new()),
        ));
        assert!(manager.has_js_source());
    }
}
