mod matcher;
pub mod model;

use crate::model::song::SongInfo;
use crate::model::source::SourceId;
use async_trait::async_trait;
use thiserror::Error;

pub use matcher::match_songs;
pub use model::*;

#[derive(Debug, Error)]
pub enum SyncError {
    #[error("未找到同步对象: {0}")]
    CollectionNotFound(String),
    #[error("音源未登录: {0}")]
    NotLoggedIn(String),
    #[error("音源不支持同步写入: {0}")]
    WriteUnsupported(String),
    #[error("同步请求失败: {0}")]
    Provider(String),
    #[error("同步已取消")]
    Cancelled,
    #[error("同步参数无效: {0}")]
    InvalidOptions(String),
}

/// 一次远端集合拉取的结果。
///
/// `failed` 记录逐个集合拉取失败的原因。调用方**必须**据此决定能否整体替换
/// 本地缓存：只有 `is_complete()` 为真时才允许覆盖/删除旧数据，否则只能按 id
/// 增量合并，不然一次频控就会把用户已有的歌单静默清空。
#[derive(Debug, Clone, Default)]
pub struct SyncCollectionSet {
    pub playlists: Vec<SyncCollection>,
    pub favorites: Vec<SyncCollection>,
    /// 拉取失败的集合：(名称, 原因)。
    pub failed: Vec<(String, String)>,
}

impl SyncCollectionSet {
    /// 是否所有远端集合都成功拉取（可以安全地整体替换旧缓存）。
    pub fn is_complete(&self) -> bool {
        self.failed.is_empty()
    }

    /// 普通歌单与收藏集合的全集。
    pub fn into_all(self) -> Vec<SyncCollection> {
        let mut all = self.playlists;
        all.extend(self.favorites);
        all
    }
}

#[async_trait]
pub trait SyncProvider: Send + Sync {
    fn source_id(&self) -> SourceId;
    fn source_name(&self) -> &str;
    async fn list_collections(
        &self,
        kind: SyncCollectionKind,
    ) -> Result<Vec<SyncCollection>, SyncError>;
    async fn get_collection(
        &self,
        kind: SyncCollectionKind,
        id: &str,
    ) -> Result<SyncCollection, SyncError>;
    async fn create_collection(
        &self,
        kind: SyncCollectionKind,
        name: &str,
    ) -> Result<SyncCollection, SyncError>;
    async fn add_songs(
        &self,
        collection: &SyncCollection,
        songs: &[SongInfo],
    ) -> Result<usize, SyncError>;
    async fn remove_songs(
        &self,
        collection: &SyncCollection,
        songs: &[SongInfo],
    ) -> Result<usize, SyncError>;
    async fn search_song(&self, song: &SongInfo) -> Result<Vec<SongInfo>, SyncError>;
    /// 一次性取出全部远端集合（普通歌单 + 收藏），并带上逐个集合的失败信息。
    ///
    /// 默认实现退化为两次 `list_collections` 且不报告局部失败。像网易云这种
    /// 「先取一份用户歌单再按 kind 过滤」的音源应当覆写：同一份远端数据拉两遍
    /// 既慢，也会把网易云打到频控（HTTP 200 + `code=405 操作频繁`）上。
    async fn collect_all(&self) -> Result<SyncCollectionSet, SyncError> {
        Ok(SyncCollectionSet {
            playlists: self.list_collections(SyncCollectionKind::Playlist).await?,
            favorites: self.list_collections(SyncCollectionKind::Favorites).await?,
            failed: Vec::new(),
        })
    }
    async fn supports_write(&self, _kind: SyncCollectionKind) -> bool {
        true
    }
}

pub struct SyncEngine;
impl SyncEngine {
    pub fn plan(
        source: SyncCollection,
        target: SyncCollection,
        options: &SyncOptions,
    ) -> Result<SyncPlan, SyncError> {
        if source.source == target.source && source.id == target.id {
            return Err(SyncError::InvalidOptions(
                "源歌单和目标歌单不能是同一个对象".into(),
            ));
        }
        let matches = match_songs(
            &source.songs,
            &target.songs,
            options.match_duration_tolerance_ms,
            options.fuzzy_threshold,
        );
        let additions = matches
            .iter()
            .filter(|m| m.target.is_none())
            .map(|m| m.source.clone())
            .collect::<Vec<_>>();
        let removals = if matches!(options.policy, SyncPolicy::Mirror) {
            target
                .songs
                .iter()
                .filter(|candidate| {
                    !matches.iter().any(|m| {
                        m.target
                            .as_ref()
                            .is_some_and(|t| t.id == candidate.id && t.source == candidate.source)
                    })
                })
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        Ok(SyncPlan {
            source,
            target,
            matches,
            additions: additions.clone(),
            removals,
            unmatched: additions,
        })
    }

    /// 执行计划：逐批解析（search_song）并写入目标集合。
    ///
    /// `progress` 在每批写入完成后回调 `(已完成歌曲数, 计划总数)`，
    /// 供 UI 显示进度；`should_cancel` 返回 true 时中止并返回
    /// [`SyncError::Cancelled`]。测试里传两个空闭包即可。
    pub async fn execute<P: SyncProvider + ?Sized>(
        provider: &P,
        plan: &SyncPlan,
        options: &SyncOptions,
        allow_removals: bool,
        progress: &(dyn Fn(usize, usize) + Send + Sync),
        should_cancel: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<SyncReport, SyncError> {
        if !provider.supports_write(plan.target.kind).await {
            return Err(SyncError::WriteUnsupported(provider.source_name().into()));
        }
        let total = plan.additions.len();
        let mut added = 0;
        let mut done = 0usize;
        let mut failed = Vec::new();
        for chunk in plan.additions.chunks(options.batch_size.max(1)) {
            if should_cancel() {
                return Err(SyncError::Cancelled);
            }
            let mut resolved = Vec::new();
            for song in chunk {
                if should_cancel() {
                    return Err(SyncError::Cancelled);
                }
                let candidates = provider.search_song(song).await?;
                if let Some(best) = best_candidate(song, &candidates, options) {
                    resolved.push(best.clone());
                } else {
                    failed.push(SyncFailure {
                        song: song.name.clone(),
                        artist: song.singer.clone(),
                        reason: "目标音源找不到可匹配歌曲".into(),
                    });
                }
            }
            if !resolved.is_empty() {
                added += provider.add_songs(&plan.target, &resolved).await?;
            }
            done += chunk.len();
            progress(done.min(total), total);
        }
        let removed = if allow_removals
            && matches!(options.policy, SyncPolicy::Mirror)
            && !plan.removals.is_empty()
        {
            provider.remove_songs(&plan.target, &plan.removals).await?
        } else {
            0
        };
        Ok(SyncReport {
            source: plan.source.source,
            target: plan.target.source,
            collection_name: plan.target.name.clone(),
            added,
            removed,
            already_present: plan.matches.len().saturating_sub(plan.additions.len()),
            unmatched: failed.len(),
            failed,
        })
    }
}
fn best_candidate<'a>(
    source: &SongInfo,
    candidates: &'a [SongInfo],
    options: &SyncOptions,
) -> Option<&'a SongInfo> {
    let matched = match_songs(
        std::slice::from_ref(source),
        candidates,
        options.match_duration_tolerance_ms,
        options.fuzzy_threshold,
    )
    .into_iter()
    .next()?;
    let target = matched.target?;
    candidates
        .iter()
        .find(|c| c.id == target.id && c.source == target.source)
}

/// 为双向同步生成两个独立计划；执行时仍逐向确认，避免一次操作同时修改两个平台。
impl SyncEngine {
    pub fn plan_bidirectional(
        left: SyncCollection,
        right: SyncCollection,
        options: &SyncOptions,
    ) -> Result<(SyncPlan, SyncPlan), SyncError> {
        let mut forward = options.clone();
        forward.direction = SyncDirection::SourceToTarget;
        let mut backward = options.clone();
        backward.direction = SyncDirection::TargetToSource;
        Ok((
            Self::plan(left.clone(), right.clone(), &forward)?,
            Self::plan(right, left, &backward)?,
        ))
    }
}

impl SyncPlan {
    pub fn additions_count(&self) -> usize {
        self.additions.len()
    }
    pub fn removals_count(&self) -> usize {
        self.removals.len()
    }
    pub fn matched_count(&self) -> usize {
        self.matches
            .iter()
            .filter(|item| item.target.is_some())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::source::SourceId;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    fn song(id: &str, source: SourceId, name: &str, artist: &str, ms: u64) -> SongInfo {
        let mut s = SongInfo::new(id.into(), source, name.into(), artist.into());
        s.duration = Duration::from_millis(ms);
        s
    }

    /// 内存版 provider：search 返回同名歌曲（跨平台 ID 不同，靠元数据匹配），
    /// add/remove 只计数，方便断言引擎的分批与回调行为。
    struct MockProvider {
        add_calls: AtomicUsize,
        batch_size_seen: AtomicUsize,
    }
    impl MockProvider {
        fn new() -> Self {
            Self {
                add_calls: AtomicUsize::new(0),
                batch_size_seen: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl SyncProvider for MockProvider {
        fn source_id(&self) -> SourceId {
            SourceId::Wy
        }
        fn source_name(&self) -> &str {
            "mock"
        }
        async fn list_collections(
            &self,
            _kind: SyncCollectionKind,
        ) -> Result<Vec<SyncCollection>, SyncError> {
            Ok(Vec::new())
        }
        async fn get_collection(
            &self,
            kind: SyncCollectionKind,
            _id: &str,
        ) -> Result<SyncCollection, SyncError> {
            Ok(SyncCollection {
                kind,
                id: "target".into(),
                name: "target".into(),
                source: self.source_id(),
                songs: Vec::new(),
            })
        }
        async fn create_collection(
            &self,
            _kind: SyncCollectionKind,
            _name: &str,
        ) -> Result<SyncCollection, SyncError> {
            Err(SyncError::WriteUnsupported("mock".into()))
        }
        async fn add_songs(
            &self,
            _collection: &SyncCollection,
            songs: &[SongInfo],
        ) -> Result<usize, SyncError> {
            self.add_calls.fetch_add(1, Ordering::SeqCst);
            self.batch_size_seen.store(songs.len(), Ordering::SeqCst);
            Ok(songs.len())
        }
        async fn remove_songs(
            &self,
            _collection: &SyncCollection,
            _songs: &[SongInfo],
        ) -> Result<usize, SyncError> {
            Ok(0)
        }
        async fn search_song(&self, song: &SongInfo) -> Result<Vec<SongInfo>, SyncError> {
            Ok(vec![song.clone()])
        }
    }

    fn two_song_plan(batch_size: usize) -> (SyncPlan, SyncOptions) {
        let source_collection = SyncCollection {
            kind: SyncCollectionKind::Playlist,
            id: "local".into(),
            name: "local".into(),
            source: SourceId::Local,
            songs: vec![
                song("a1", SourceId::Local, "晴天", "周杰伦", 269_000),
                song("a2", SourceId::Local, "七里香", "周杰伦", 275_000),
            ],
        };
        let target_collection = SyncCollection {
            kind: SyncCollectionKind::Playlist,
            id: "target".into(),
            name: "target".into(),
            source: SourceId::Wy,
            songs: Vec::new(),
        };
        let options = SyncOptions {
            batch_size,
            ..SyncOptions::default()
        };
        (
            SyncEngine::plan(source_collection, target_collection, &options).unwrap(),
            options,
        )
    }

    #[tokio::test]
    async fn execute_reports_progress_and_batches_adds() {
        let provider = MockProvider::new();
        let (plan, options) = two_song_plan(1);
        let last = std::sync::Arc::new(std::sync::Mutex::new((0usize, 0usize)));
        let sink = std::sync::Arc::clone(&last);
        let report = SyncEngine::execute(
            &provider,
            &plan,
            &options,
            false,
            &move |done, total| {
                *sink.lock().unwrap() = (done, total);
            },
            &|| false,
        )
        .await
        .unwrap();
        // 每首一批：两次 add，最后一笔回调 done == total。
        assert_eq!(provider.add_calls.load(Ordering::SeqCst), 2);
        assert_eq!(report.added, 2);
        assert_eq!(*last.lock().unwrap(), (2, 2));
    }

    #[tokio::test]
    async fn execute_honours_cancellation() {
        let provider = MockProvider::new();
        let (plan, options) = two_song_plan(1);
        let cancelled = AtomicBool::new(true);
        let result = SyncEngine::execute(&provider, &plan, &options, false, &|_, _| {}, &|| {
            cancelled.load(Ordering::SeqCst)
        })
        .await;
        assert!(matches!(result, Err(SyncError::Cancelled)));
        assert_eq!(provider.add_calls.load(Ordering::SeqCst), 0);
    }
}
