//! 下载队列：把「一首歌」变成「一个落盘的音频文件」。
//!
//! 流程参考 MusicBot-Go 的 `downloadAndPrepareFromPlatform`：
//! 解析播放地址（失败自动换源）→ 分片下载并校验 → 校正扩展名 →
//! 写标签/封面 → 保存歌词 → 通知界面。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use lx_core::events::{AppAction, Notification};
use lx_core::model::config::{Config, WebdavConfig};
use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::SongUrl;
use lx_source::manager::SourceManager;
use tokio::sync::{Semaphore, mpsc};

use crate::download::engine::{
    DownloadEngine, DownloadError, DownloadOptions, DownloadProgress, DownloadRequest,
    ProgressSnapshot,
};
use crate::download::naming::{
    AUDIO_EXTENSIONS, detect_extension, extension_from_url, normalize_extension, render_filename,
    resolve_download_dir, sanitize_filename, unique_dest,
};
use crate::download::records::{DownloadRecord, DownloadRecords, DownloadStatus, relative_path};
use crate::download::tags::{
    DownloadMetadata, embed_tags, shrink_cover, validate_audio, write_lyric_file,
};
use crate::download::webdav;

/// 已结束任务在列表中的保留数量。
const FINISHED_TASK_LIMIT: usize = 100;
/// 封面下载大小上限（嵌入用，超过即放弃）。
const COVER_SIZE_LIMIT: u64 = 8 * 1024 * 1024;
/// 单首歌的下载尝试次数：每次失败后从下一个解析器继续，等价于参考实现的
/// 「换候选地址」循环。
const DOWNLOAD_ATTEMPTS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadTrigger {
    /// 用户主动下载（列表 `D`、`Ctrl+S`、右键菜单）。
    Manual,
    /// 播放时自动缓存：不在开始时打扰用户，完成/失败时才提示。
    AutoCache,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadState {
    Queued,
    Resolving,
    Downloading,
    Tagging,
    Done,
    Skipped,
    Cancelled,
    Failed,
}

impl DownloadState {
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Resolving | Self::Downloading | Self::Tagging
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "排队中",
            Self::Resolving => "解析地址",
            Self::Downloading => "下载中",
            Self::Tagging => "写入标签",
            Self::Done => "已完成",
            Self::Skipped => "已跳过",
            Self::Cancelled => "已取消",
            Self::Failed => "失败",
        }
    }
}

/// 下载任务。进度用原子量暴露给界面，避免锁竞争。
pub struct DownloadTask {
    pub id: u64,
    pub name: String,
    pub singer: String,
    pub source: SourceId,
    pub quality: Quality,
    pub dest: PathBuf,
    pub state: DownloadState,
    pub error: Option<String>,
    pub progress: Arc<DownloadProgress>,
    pub started_at: Instant,
    pub finished_at: Option<Instant>,
    pub bytes: u64,
    /// 音源声明的文件大小（字节）；未知时为 `None`。
    pub expected_size: Option<u64>,
    /// 歌曲时长（秒），用于把体积换算成码率。
    pub duration_secs: u64,
    /// 入队来源：自动缓存的任务在文案与通知上更安静。
    pub trigger: DownloadTrigger,
}

/// 界面每帧读取的任务快照。
#[derive(Debug, Clone)]
pub struct DownloadTaskView {
    pub id: u64,
    pub name: String,
    pub singer: String,
    pub source: SourceId,
    pub quality: Quality,
    pub dest: PathBuf,
    pub state: DownloadState,
    pub error: Option<String>,
    pub progress: ProgressSnapshot,
    pub elapsed: Duration,
    pub bytes: u64,
    pub expected_size: Option<u64>,
    /// 按体积与时长估算的码率（kbps）。
    pub bitrate_kbps: Option<u32>,
    /// 入队来源：自动缓存的任务在面板上单独标注。
    pub trigger: DownloadTrigger,
}

impl DownloadTaskView {
    /// 界面上的一行标题。
    pub fn display_name(&self) -> String {
        if self.singer.trim().is_empty() {
            self.name.clone()
        } else {
            format!("{} - {}", self.name, self.singer)
        }
    }
}

/// 从配置快照出来的下载参数，配置变更时整体替换。
#[derive(Debug, Clone, PartialEq)]
struct RuntimeSettings {
    dir: PathBuf,
    quality: Option<Quality>,
    filename_template: String,
    skip_existing: bool,
    write_tags: bool,
    embed_cover: bool,
    save_lyric: bool,
    /// WebDAV 同步：下载完成后把文件推到远端。
    webdav: WebdavConfig,
    /// 播放时自动缓存到本地。
    auto_cache_on_play: bool,
    /// 播放满该时长后才开始缓存。
    auto_cache_after_secs: Duration,
    auto_toggle: bool,
    concurrent_songs: usize,
    play_quality: Quality,
    /// 代理与超时参与引擎构建，变化时需要重建 HTTP 客户端。
    proxy_url: String,
    timeout_secs: u64,
    /// 分片、重试、校验等下载参数。
    options: DownloadOptions,
}

impl RuntimeSettings {
    fn from_config(config: &Config) -> Self {
        Self {
            dir: resolve_download_dir(&config.download.dir),
            quality: config.download.quality,
            filename_template: config.download.filename_template.clone(),
            skip_existing: config.download.skip_existing,
            write_tags: config.download.write_tags,
            embed_cover: config.download.embed_cover,
            save_lyric: config.download.save_lyric,
            webdav: config.download.webdav.clone(),
            auto_cache_on_play: config.download.auto_cache_on_play,
            auto_cache_after_secs: Duration::from_secs(config.download.auto_cache_after_secs),
            auto_toggle: config.source.auto_toggle,
            concurrent_songs: config.download.concurrent_songs.clamp(1, 8),
            play_quality: config.player.quality,
            proxy_url: config.network.proxy_url.clone(),
            timeout_secs: config.network.timeout,
            options: DownloadOptions::from_config(&config.download),
        }
    }

    /// 本次下载使用的音质：配置固定音质优先，否则跟随播放音质。
    fn quality(&self) -> Quality {
        self.quality.unwrap_or(self.play_quality)
    }
}

pub struct DownloadManager {
    /// 下载任务统一交给主线程创建的那个多线程 runtime；
    /// 主循环本身跑在 runtime 之外，不能直接用 `tokio::spawn`。
    handle: tokio::runtime::Handle,
    engine: RwLock<Arc<DownloadEngine>>,
    settings: RwLock<RuntimeSettings>,
    semaphore: RwLock<Arc<Semaphore>>,
    tasks: Mutex<Vec<DownloadTask>>,
    inflight: Mutex<HashSet<String>>,
    /// 跨会话下载记录与去重索引。
    records: DownloadRecords,
    /// 本次会话已触发过自动缓存的歌曲键，避免轮询重复入队。
    auto_cached: Mutex<HashSet<String>>,
    next_id: AtomicU64,
}

impl DownloadManager {
    pub fn new(config: &Config, handle: tokio::runtime::Handle) -> Self {
        Self::build(config, handle, DownloadRecords::load_default())
    }

    /// 使用指定记录存储的构造函数，供测试隔离数据目录。
    #[cfg(test)]
    pub(crate) fn with_records(
        config: &Config,
        handle: tokio::runtime::Handle,
        records: DownloadRecords,
    ) -> Self {
        Self::build(config, handle, records)
    }

    fn build(config: &Config, handle: tokio::runtime::Handle, records: DownloadRecords) -> Self {
        let settings = RuntimeSettings::from_config(config);
        let engine = Arc::new(build_engine(config, &settings));
        let semaphore = Arc::new(Semaphore::new(settings.concurrent_songs));
        Self {
            handle,
            engine: RwLock::new(engine),
            settings: RwLock::new(settings),
            semaphore: RwLock::new(semaphore),
            tasks: Mutex::new(Vec::new()),
            inflight: Mutex::new(HashSet::new()),
            records,
            auto_cached: Mutex::new(HashSet::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// 配置变更后重建引擎与并发上限（已排队的任务继续用旧引擎完成）。
    pub fn sync_config(&self, config: &Config) {
        let settings = RuntimeSettings::from_config(config);
        {
            let current = self
                .settings
                .read()
                .unwrap_or_else(|error| error.into_inner());
            if *current == settings {
                // 没有实际变化就不重建客户端，避免打断正在进行的下载。
                return;
            }
        }
        let engine = Arc::new(build_engine(config, &settings));
        let semaphore = Arc::new(Semaphore::new(settings.concurrent_songs));
        *self
            .settings
            .write()
            .unwrap_or_else(|error| error.into_inner()) = settings;
        *self
            .engine
            .write()
            .unwrap_or_else(|error| error.into_inner()) = engine;
        *self
            .semaphore
            .write()
            .unwrap_or_else(|error| error.into_inner()) = semaphore;
    }

    /// 当前下载目录，供界面显示。
    pub fn download_dir(&self) -> PathBuf {
        self.settings
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .dir
            .clone()
    }

    /// 返回仍然存在的已下载文件，供播放解析器执行“本地优先”。
    ///
    /// 这让下载完成后的再次播放不再重新请求网络；自动缓存任务也因此
    /// 真正参与播放路径，而不是只能作为一个独立的下载动作。
    pub fn existing_download(&self, song: &SongInfo) -> Option<PathBuf> {
        self.records.existing_download(song, &self.download_dir())
    }

    pub fn snapshot(&self) -> Vec<DownloadTaskView> {
        let tasks = self.tasks.lock().unwrap_or_else(|error| error.into_inner());
        tasks
            .iter()
            .map(|task| {
                let elapsed = task
                    .finished_at
                    .unwrap_or_else(Instant::now)
                    .saturating_duration_since(task.started_at);
                DownloadTaskView {
                    id: task.id,
                    name: task.name.clone(),
                    singer: task.singer.clone(),
                    source: task.source,
                    quality: task.quality,
                    dest: task.dest.clone(),
                    state: task.state,
                    error: task.error.clone(),
                    progress: task.progress.snapshot(),
                    elapsed,
                    bytes: task.bytes,
                    expected_size: task.expected_size,
                    bitrate_kbps: task
                        .expected_size
                        .and_then(|size| estimate_bitrate(size, task.duration_secs)),
                    trigger: task.trigger,
                }
            })
            .collect()
    }

    pub fn active_count(&self) -> usize {
        let tasks = self.tasks.lock().unwrap_or_else(|error| error.into_inner());
        tasks.iter().filter(|task| task.state.is_active()).count()
    }

    /// 取消任务；已完成的任务只是从列表中移除。
    pub fn cancel(&self, id: u64) {
        let mut tasks = self.tasks.lock().unwrap_or_else(|error| error.into_inner());
        let Some(task) = tasks.iter_mut().find(|task| task.id == id) else {
            return;
        };
        if task.state.is_active() {
            task.progress.cancel();
            task.state = DownloadState::Cancelled;
            task.error = None;
            task.finished_at = Some(Instant::now());
        } else {
            tasks.retain(|task| task.id != id);
        }
    }

    /// 清空已结束的任务记录，保留进行中的。
    pub fn clear_finished(&self) {
        let mut tasks = self.tasks.lock().unwrap_or_else(|error| error.into_inner());
        tasks.retain(|task| task.state.is_active());
    }

    /// 最近的下载记录（含历史会话），最新在前。
    pub fn records_recent(&self, limit: usize) -> Vec<DownloadRecord> {
        self.records.recent(limit)
    }

    /// 持久化的下载记录条数。
    pub fn record_count(&self) -> usize {
        self.records.len()
    }

    /// 清空下载记录与去重索引：清空后已下载过的歌曲可以重新下载。
    pub fn clear_records(&self) -> Result<(), String> {
        self.records.clear()
    }

    /// 把一次下载的终态写入持久化记录。
    fn record_song(
        &self,
        song: &SongInfo,
        status: DownloadStatus,
        dest: Option<&std::path::Path>,
        error: Option<String>,
    ) {
        let rel_path = dest.and_then(|path| relative_path(&self.download_dir(), path));
        self.records
            .record(DownloadRecord::new(song, status, rel_path, error));
    }

    /// 下载完成后同步到 WebDAV。
    ///
    /// 失败只发警告：文件已经在本地，同步属于附加动作，不应该让整次下载
    /// 显示为失败（与参考实现把错误放进 warning 字段一致）。
    async fn sync_to_webdav(
        &self,
        path: &std::path::Path,
        settings: &RuntimeSettings,
        notify: &mpsc::UnboundedSender<AppAction>,
    ) {
        if !webdav::is_configured(&settings.webdav) {
            return;
        }
        let engine = Arc::clone(
            &self
                .engine
                .read()
                .unwrap_or_else(|error| error.into_inner()),
        );
        match webdav::upload_file(engine.client(), &settings.webdav, path).await {
            Ok(()) => {
                let _ = notify.send(AppAction::ShowNotification(Notification::info(
                    "已同步到 WebDAV".to_string(),
                )));
            }
            Err(error) => {
                tracing::warn!("webdav upload failed for {}: {error}", path.display());
                let _ = notify.send(AppAction::ShowNotification(Notification::warning(format!(
                    "WebDAV 同步失败（本地文件已保存）: {error}"
                ))));
            }
        }
    }

    /// 把一个任务标记为完成/失败，并在必要时裁剪列表长度。
    fn finish(
        &self,
        id: u64,
        state: DownloadState,
        error: Option<String>,
        bytes: u64,
        dest: Option<PathBuf>,
    ) {
        let mut tasks = self.tasks.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(task) = tasks.iter_mut().find(|task| task.id == id) {
            // 已取消的任务保持取消状态，避免迟到的结果改写用户操作。
            if task.state == DownloadState::Cancelled {
                return;
            }
            task.state = state;
            task.error = error;
            task.bytes = bytes;
            if let Some(dest) = dest {
                task.dest = dest;
            }
            task.finished_at = Some(Instant::now());
        }
        trim_finished(&mut tasks);
    }

    fn update_state(&self, id: u64, state: DownloadState, dest: Option<PathBuf>) {
        let mut tasks = self.tasks.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(task) = tasks.iter_mut().find(|task| task.id == id) {
            if task.state == DownloadState::Cancelled {
                return;
            }
            task.state = state;
            if let Some(dest) = dest {
                task.dest = dest;
            }
        }
    }

    /// 记录音源声明的体积：界面据此在下载前就显示体积与估算码率。
    fn set_expected_size(&self, id: u64, size: Option<u64>) {
        let Some(size) = size.filter(|size| *size > 0) else {
            return;
        };
        let mut tasks = self.tasks.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(task) = tasks.iter_mut().find(|task| task.id == id) {
            task.expected_size = Some(size);
        }
    }

    /// 音源给的是本地文件时直接复制；不是本地路径则返回 `None`。
    ///
    /// 目前只有汽水音乐走这条路：加密音频由音源解密到缓存目录后，播放地址
    /// 就是那个本地路径。返回值 `Some(Ok(bytes))` 表示复制成功。
    async fn copy_local_source(
        &self,
        url: &str,
        dest: &std::path::Path,
    ) -> Option<Result<u64, String>> {
        if url.contains("://") {
            return None;
        }
        let source = std::path::Path::new(url);
        if !source.is_file() {
            return None;
        }
        if source == dest {
            return Some(Ok(std::fs::metadata(dest)
                .map(|meta| meta.len())
                .unwrap_or_default()));
        }
        let result = tokio::fs::copy(source, dest)
            .await
            .map_err(|error| format!("复制本地音频失败: {error}"));
        Some(result)
    }

    /// 入队一首歌。重复入队会被合并（同一音源同一 id 只下载一次）。
    pub fn enqueue(
        self: &Arc<Self>,
        song: SongInfo,
        sources: Arc<SourceManager>,
        notify: mpsc::UnboundedSender<AppAction>,
    ) -> bool {
        self.enqueue_with_trigger(song, sources, notify, DownloadTrigger::Manual)
    }

    /// 播放自动缓存：播放满配置时长后把当前歌曲存入本地。
    ///
    /// 已经下载过（去重索引命中）或本次会话已处理过的歌曲直接跳过，
    /// 避免每秒轮询都重新入队。
    pub fn maybe_auto_cache(
        self: &Arc<Self>,
        song: &SongInfo,
        position: Duration,
        sources: Arc<SourceManager>,
        notify: mpsc::UnboundedSender<AppAction>,
    ) {
        {
            let settings = self
                .settings
                .read()
                .unwrap_or_else(|error| error.into_inner());
            if !settings.auto_cache_on_play || position < settings.auto_cache_after_secs {
                return;
            }
        }
        if song.source == SourceId::Local {
            return;
        }
        // 本地已经有这个文件时不必再走一遍解析流程。
        if self
            .records
            .existing_download(song, &self.download_dir())
            .is_some()
        {
            return;
        }
        let key = DownloadRecords::song_key(&song.singer, &song.name);
        {
            let mut scheduled = self
                .auto_cached
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if !scheduled.insert(key.clone()) {
                return;
            }
        }
        let queued =
            self.enqueue_with_trigger(song.clone(), sources, notify, DownloadTrigger::AutoCache);
        if !queued {
            // 入队被拒（例如用户正好手动下载了同一首）：撤销标记，
            // 下一轮再判断，避免这一整次会话都不再缓存它。
            let mut scheduled = self
                .auto_cached
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            scheduled.remove(&key);
        }
    }

    fn enqueue_with_trigger(
        self: &Arc<Self>,
        song: SongInfo,
        sources: Arc<SourceManager>,
        notify: mpsc::UnboundedSender<AppAction>,
        trigger: DownloadTrigger,
    ) -> bool {
        // 自动缓存是后台行为：开始下载、已在队列中这类提示会变成每首歌一条
        // 的噪音，只在完成/失败时汇报。
        let quiet = trigger == DownloadTrigger::AutoCache;
        if song.source == SourceId::Local && song.file_path.is_some() {
            if !quiet {
                let _ = notify.send(AppAction::ShowNotification(Notification::info(
                    "本地文件无需下载".to_string(),
                )));
            }
            return false;
        }

        let dedup_key = format!("{}:{}", song.source.as_str(), song.id);
        {
            let mut inflight = self
                .inflight
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if !inflight.insert(dedup_key.clone()) {
                if !quiet {
                    let _ = notify.send(AppAction::ShowNotification(Notification::warning(
                        "这首歌已经在下载队列中".to_string(),
                    )));
                }
                return false;
            }
        }

        let settings = self
            .settings
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let quality = settings.quality();
        let stem = render_filename(&settings.filename_template, &song, quality);
        let guessed_extension = guess_extension(quality);
        let dest = unique_dest(&settings.dir, &stem, guessed_extension);
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let progress = DownloadProgress::new();
        let task = DownloadTask {
            id,
            name: song.name.clone(),
            singer: song.singer.clone(),
            source: song.source,
            quality,
            dest: dest.clone(),
            state: DownloadState::Queued,
            error: None,
            progress: Arc::clone(&progress),
            started_at: Instant::now(),
            finished_at: None,
            bytes: 0,
            expected_size: None,
            duration_secs: song.duration.as_secs(),
            trigger,
        };
        {
            let mut tasks = self.tasks.lock().unwrap_or_else(|error| error.into_inner());
            tasks.push(task);
        }
        if !quiet {
            let _ = notify.send(AppAction::ShowNotification(Notification::info(format!(
                "开始下载: {}",
                song.name
            ))));
        }

        let manager = Arc::clone(self);
        let semaphore = Arc::clone(
            &self
                .semaphore
                .read()
                .unwrap_or_else(|error| error.into_inner()),
        );
        self.handle.spawn(async move {
            let _permit = semaphore.acquire_owned().await;
            manager
                .run_task(
                    id, song, dedup_key, quality, stem, settings, sources, notify, trigger,
                )
                .await;
        });
        true
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_task(
        self: Arc<Self>,
        id: u64,
        song: SongInfo,
        dedup_key: String,
        quality: Quality,
        stem: String,
        settings: RuntimeSettings,
        sources: Arc<SourceManager>,
        notify: mpsc::UnboundedSender<AppAction>,
        trigger: DownloadTrigger,
    ) {
        let result = self
            .prepare_and_download(id, &song, quality, &stem, &settings, &sources)
            .await;
        self.inflight
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&dedup_key);

        match result {
            Ok(DownloadOutcome::Done { dest, bytes }) => {
                self.finish(id, DownloadState::Done, None, bytes, Some(dest.clone()));
                self.record_song(&song, DownloadStatus::Done, Some(&dest), None);
                tracing::info!(
                    "download finished: {} -> {} ({} bytes)",
                    song.name,
                    dest.display(),
                    bytes
                );
                // 带上完整路径：用户最关心的就是「下到哪去了」。
                let message = if trigger == DownloadTrigger::AutoCache {
                    format!("已缓存到本地 → {}", dest.display())
                } else {
                    format!("下载完成 → {}", dest.display())
                };
                let _ = notify.send(AppAction::ShowNotification(Notification::success(message)));
                // 同步放在完成提示之后：上传可能耗时，不应该拖慢「下载完成」的反馈。
                self.sync_to_webdav(&dest, &settings, &notify).await;
            }
            Ok(DownloadOutcome::Skipped { dest }) => {
                tracing::info!("download skipped, file already exists: {}", dest.display());
                self.finish(id, DownloadState::Skipped, None, 0, Some(dest.clone()));
                // 记录跳过结果：这样按文件名命中的旧文件也会补进去重索引。
                self.record_song(&song, DownloadStatus::Skipped, Some(&dest), None);
            }
            Err(TaskError::Cancelled) => {
                tracing::info!("download cancelled: {}", song.name);
                self.finish(id, DownloadState::Cancelled, None, 0, None);
            }
            Err(TaskError::Failed(error)) => {
                tracing::warn!("download failed: {}: {error}", song.name);
                self.finish(id, DownloadState::Failed, Some(error.clone()), 0, None);
                self.record_song(&song, DownloadStatus::Failed, None, Some(error.clone()));
                let _ = notify.send(AppAction::ShowNotification(Notification::error(format!(
                    "下载失败 {}: {error}",
                    song.name
                ))));
            }
        }
    }

    async fn prepare_and_download(
        &self,
        id: u64,
        song: &SongInfo,
        quality: Quality,
        stem: &str,
        settings: &RuntimeSettings,
        sources: &Arc<SourceManager>,
    ) -> Result<DownloadOutcome, TaskError> {
        let engine = Arc::clone(
            &self
                .engine
                .read()
                .unwrap_or_else(|error| error.into_inner()),
        );

        if settings.skip_existing {
            // 先查跨会话去重索引（按歌手+歌名），再退回按当前模板渲染的文件名，
            // 这样换过文件名模板或重启过程序都不会重复下载同一首歌。
            let existing = self
                .records
                .existing_download(song, &settings.dir)
                .or_else(|| existing_download(&settings.dir, stem));
            if let Some(existing) = existing {
                return Ok(DownloadOutcome::Skipped { dest: existing });
            }
        }

        self.update_state(id, DownloadState::Resolving, None);
        let progress = self.task_progress(id)?;

        // 与参考实现一致：地址解析失败或下载中断时，换下一个解析器/音源继续，
        // 而不是直接判定整首歌下载失败。
        let mut js_start_index = 0usize;
        // 一旦某个 URL 已经实际返回“非音频内容”（例如网易云外链的 404 HTML），
        // 后续重试不能再走同一个内置音源；必须转入跨源匹配。
        let mut skip_builtin_fallback = false;
        let mut last_error = None;
        let mut downloaded = None;
        for attempt in 0..DOWNLOAD_ATTEMPTS {
            // 解析失败不必立刻放弃：下一轮从后面的 JS 音源继续试；
            // 如果上一轮已经拿到非音频响应，则禁止重复使用同一个内置回退地址。
            let Ok((resolved_song, url, js_index)) = resolve_download_url(
                sources,
                song,
                quality,
                settings.auto_toggle,
                js_start_index,
                skip_builtin_fallback,
            )
            .await
            else {
                tracing::debug!(
                    "download resolve attempt {}/{} failed for {} (js index {})",
                    attempt + 1,
                    DOWNLOAD_ATTEMPTS,
                    song.name,
                    js_start_index
                );
                last_error = Some("获取播放地址失败".to_string());
                js_start_index = js_start_index.saturating_add(1);
                continue;
            };

            let extension_hint = extension_from_url(&url.url)
                .unwrap_or_else(|| guess_extension(quality).to_string());
            let dest = unique_dest(&settings.dir, stem, &extension_hint);
            self.update_state(id, DownloadState::Downloading, Some(dest.clone()));
            self.set_expected_size(id, url.size);
            // 下次重试用后面那个 JS 音源，避免反复拿到同一个失效链接。
            js_start_index = js_index.map_or(js_start_index, |index| index + 1);

            // 音源可能直接给出本地文件（汽水的加密音频要先解密到缓存目录）：
            // 这种情况复制文件即可，不需要也不应该再发一次 HTTP 请求。
            if let Some(copied) = self.copy_local_source(&url.url, &dest).await {
                match copied {
                    Ok(bytes) => {
                        let final_dest =
                            maybe_fix_extension(&dest, stem, &settings.dir).unwrap_or(dest.clone());
                        if let Err(reason) = validate_audio(&final_dest, song.duration) {
                            tracing::warn!("rejecting copied audio: {reason}");
                            let _ = std::fs::remove_file(&final_dest);
                            let is_non_audio = reason.contains("不是音频文件");
                            last_error = Some(reason);
                            if is_non_audio {
                                skip_builtin_fallback = true;
                            }
                            continue;
                        }
                        downloaded = Some((resolved_song, url, final_dest, bytes));
                        break;
                    }
                    Err(error) => {
                        tracing::debug!("copy local source failed: {error}");
                        last_error = Some(error);
                        continue;
                    }
                }
            }

            let request = DownloadRequest {
                url: url.url.clone(),
                headers: url.headers.clone(),
                dest: dest.clone(),
                expected_size: url.size,
                size_is_advisory: url.size_is_advisory,
                md5: url.md5.clone(),
                // 音源注入的备用 CDN 与严格分片要求一并交给下载引擎。
                candidate_urls: url.candidate_urls.clone(),
                max_chunk_size: url.max_chunk_size,
            };
            match engine.download(&request, &progress).await {
                Ok(bytes) => {
                    // 落盘后还要确认这确实是一首完整的音频：参考实现的
                    // VerifyFullAudio 用 ffprobe 比对时长，这里用文件头 + lofty。
                    let final_dest =
                        maybe_fix_extension(&dest, stem, &settings.dir).unwrap_or(dest.clone());
                    if let Err(reason) = validate_audio(&final_dest, song.duration) {
                        tracing::warn!("rejecting downloaded audio: {reason}");
                        let _ = std::fs::remove_file(&final_dest);
                        let is_non_audio = reason.contains("不是音频文件");
                        last_error = Some(reason);
                        if is_non_audio {
                            skip_builtin_fallback = true;
                        }
                        continue;
                    }
                    downloaded = Some((resolved_song, url, final_dest, bytes));
                    break;
                }
                Err(error) => {
                    if matches!(error, DownloadError::Cancelled) {
                        return Err(TaskError::Cancelled);
                    }
                    tracing::debug!("download attempt {} failed: {error}", attempt + 1);
                    last_error = Some(error.to_string());
                    // 从下一个 JS 音源继续解析，等价于候选地址/解析器回退。
                    js_start_index = js_start_index.saturating_add(1);
                }
            }
        }

        let Some((resolved_song, url, final_dest, bytes)) = downloaded else {
            return Err(TaskError::Failed(
                last_error.unwrap_or_else(|| "下载失败".to_string()),
            ));
        };

        self.update_state(id, DownloadState::Tagging, Some(final_dest.clone()));

        let lyric = if settings.save_lyric {
            fetch_lyric(sources, &resolved_song).await
        } else {
            None
        };

        let metadata = self
            .build_metadata(&engine, &resolved_song, &url, settings, lyric.clone())
            .await;
        let audio_path = final_dest.clone();
        let write_tags = settings.write_tags;
        let write_lyric_file_flag = settings.save_lyric;
        let metadata_for_task = metadata.clone();
        let tagging = tokio::task::spawn_blocking(move || {
            if write_tags && let Err(error) = embed_tags(&audio_path, &metadata_for_task) {
                tracing::warn!("embed tags failed: {error}");
            }
            if write_lyric_file_flag
                && let Some(lyric) = metadata_for_task.lyric.as_deref()
                && let Err(error) = write_lyric_file(&audio_path, lyric, None)
            {
                tracing::warn!("write lyric file failed: {error}");
            }
        })
        .await;
        if tagging.is_err() {
            tracing::warn!("tagging task panicked for {}", final_dest.display());
        }

        Ok(DownloadOutcome::Done {
            dest: final_dest,
            bytes,
        })
    }

    async fn build_metadata(
        &self,
        engine: &DownloadEngine,
        song: &SongInfo,
        url: &SongUrl,
        settings: &RuntimeSettings,
        lyric: Option<String>,
    ) -> DownloadMetadata {
        let mut metadata = DownloadMetadata::new(&song.name, &song.singer, &song.album_name);
        metadata.lyric = lyric;

        if settings.embed_cover {
            let cover_url = url
                .cover_url
                .clone()
                .or_else(|| song.cover_url.clone())
                .filter(|url| !url.trim().is_empty());
            if let Some(cover_url) = cover_url {
                match engine
                    .fetch_bytes(&cover_url, &url.headers, COVER_SIZE_LIMIT)
                    .await
                {
                    // 过大的封面先压缩再嵌入，避免把几十 MB 原图塞进音频标签。
                    Ok(bytes) => metadata.cover = Some(shrink_cover(bytes)),
                    Err(error) => tracing::debug!("cover download skipped: {error}"),
                }
            }
        }
        metadata
    }

    fn task_progress(&self, id: u64) -> Result<Arc<DownloadProgress>, TaskError> {
        let tasks = self.tasks.lock().unwrap_or_else(|error| error.into_inner());
        tasks
            .iter()
            .find(|task| task.id == id)
            .map(|task| Arc::clone(&task.progress))
            .ok_or_else(|| TaskError::Failed("下载任务已被移除".to_string()))
    }
}

enum DownloadOutcome {
    Done { dest: PathBuf, bytes: u64 },
    Skipped { dest: PathBuf },
}

enum TaskError {
    Cancelled,
    Failed(String),
}

impl TaskError {
    fn from_message(error: String) -> Self {
        TaskError::Failed(error)
    }
}

/// 解析可下载的播放地址；开启自动换源时逐个尝试其它音源的同曲匹配。
async fn resolve_download_url(
    sources: &Arc<SourceManager>,
    song: &SongInfo,
    quality: Quality,
    auto_toggle: bool,
    js_start_index: usize,
    skip_builtin_fallback: bool,
) -> Result<(SongInfo, SongUrl, Option<usize>), TaskError> {
    if !skip_builtin_fallback {
        match tokio::time::timeout(
            Duration::from_secs(60),
            sources.get_song_url_from_js_index(song, quality, js_start_index),
        )
        .await
        {
            Ok(Ok((url, index))) => return Ok((song.clone(), url, index)),
            Ok(Err(error)) if !auto_toggle => {
                return Err(TaskError::from_message(error.to_string()));
            }
            Ok(Err(error)) => tracing::debug!("download url failed for {}: {error}", song.name),
            Err(_) => tracing::debug!("download url timed out for {}", song.name),
        }
    } else if !auto_toggle {
        return Err(TaskError::from_message(
            "下载地址返回的不是音频文件".to_string(),
        ));
    }

    // 如果已经确认当前地址返回的不是音频，不能再次调用同一个内置回退源。
    // 这正是网易云外链返回 404 HTML 时的典型场景：解析“成功”但实际资源不可用。
    // 此时直接使用跨源搜索得到的其它平台歌曲，并调用其内置音源，避免再次绕回
    // 同一个失效 JS/内置 URL。
    for candidate in sources.find_music(song).await {
        let Some(source) = sources.get(candidate.source) else {
            continue;
        };
        match tokio::time::timeout(
            Duration::from_secs(60),
            source.get_song_url(&candidate, quality),
        )
        .await
        {
            Ok(Ok(url)) => return Ok((candidate, url, None)),
            Ok(Err(error)) => tracing::debug!(
                "download toggle source {} failed: {error}",
                candidate.source.as_str()
            ),
            Err(_) => tracing::debug!(
                "download toggle source {} timed out",
                candidate.source.as_str()
            ),
        }
    }
    Err(TaskError::from_message("获取播放地址失败".to_string()))
}

async fn fetch_lyric(sources: &Arc<SourceManager>, song: &SongInfo) -> Option<String> {
    match tokio::time::timeout(
        Duration::from_secs(20),
        sources.get_lyric_with_fallback(song),
    )
    .await
    {
        Ok(Ok(lyric)) => crate::download::tags::standard_lyric(&lyric),
        _ => None,
    }
}

fn build_engine(config: &Config, settings: &RuntimeSettings) -> DownloadEngine {
    DownloadEngine::new(
        &config.network.proxy_url,
        config.network.timeout,
        settings.options.clone(),
    )
}

/// 未探测到真实格式时，按音质给出保守的扩展名。
fn guess_extension(quality: Quality) -> &'static str {
    match quality {
        Quality::Flac | Quality::Flac24 => "flac",
        Quality::High320 | Quality::Low128 => "mp3",
    }
}

/// 按体积与时长估算码率（kbps）。
///
/// 音源给的多是「文件字节数」，而用户关心的是「这是不是 320K / 无损」，
/// 换算一次就能在列表里直接显示。时长未知或体积过小时返回 `None`。
fn estimate_bitrate(size_bytes: u64, duration_secs: u64) -> Option<u32> {
    if duration_secs == 0 || size_bytes == 0 {
        return None;
    }
    let kbps = size_bytes
        .saturating_mul(8)
        .checked_div(duration_secs)?
        .checked_div(1000)?;
    (kbps > 0).then_some(kbps.min(u32::MAX as u64) as u32)
}

/// 按文件头魔数校正扩展名，和 MusicBot-Go 的 `normalizeExtractedAudioPath` 等价。
fn maybe_fix_extension(
    dest: &std::path::Path,
    stem: &str,
    dir: &std::path::Path,
) -> Option<PathBuf> {
    let mut head = [0u8; 16];
    let read = std::fs::File::open(dest)
        .and_then(|mut file| {
            use std::io::Read;
            file.read(&mut head)
        })
        .ok()?;
    let detected = detect_extension(&head[..read])?;
    let detected = normalize_extension(detected);
    let current = dest
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if detected == current {
        return Some(dest.to_path_buf());
    }
    let mut target = dir.join(format!("{}.{}", sanitize_filename(stem), detected));
    if target.exists() {
        target = unique_dest(dir, stem, &detected);
    }
    match std::fs::rename(dest, &target) {
        Ok(()) => {
            tracing::debug!(
                "download format corrected: {} → {}",
                dest.display(),
                target.display()
            );
            Some(target)
        }
        Err(error) => {
            tracing::warn!("rename downloaded file failed: {error}");
            Some(dest.to_path_buf())
        }
    }
}

/// 目录里是否已有同名音频（任一支持的扩展名）。
fn existing_download(dir: &std::path::Path, stem: &str) -> Option<PathBuf> {
    let sanitized = sanitize_filename(stem);
    for extension in AUDIO_EXTENSIONS {
        let candidate = dir.join(format!("{sanitized}.{extension}"));
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// 只保留进行中的任务和最近若干条历史记录。
fn trim_finished(tasks: &mut Vec<DownloadTask>) {
    let finished = tasks.iter().filter(|task| !task.state.is_active()).count();
    if finished <= FINISHED_TASK_LIMIT {
        return;
    }
    let mut to_remove = finished - FINISHED_TASK_LIMIT;
    tasks.retain(|task| {
        if to_remove > 0 && !task.state.is_active() {
            to_remove -= 1;
            return false;
        }
        true
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::engine::DownloadOptions;
    use crate::download::test_support::{
        RangeMode, fake_audio, spawn_test_server, temp_dir, tiny_png,
    };
    use lx_core::model::lyric::LyricData;
    use lx_core::traits::source::{FetchError, MusicSource, SearchError, SearchResult};

    fn test_config() -> Config {
        Config::default()
    }

    #[test]
    fn quality_prefers_configured_value() {
        let mut config = test_config();
        config.player.quality = Quality::High320;
        config.download.quality = Some(Quality::Flac);
        let settings = RuntimeSettings::from_config(&config);
        assert_eq!(settings.quality(), Quality::Flac);

        config.download.quality = None;
        let settings = RuntimeSettings::from_config(&config);
        assert_eq!(settings.quality(), Quality::High320);
    }

    #[tokio::test]
    async fn local_files_are_never_downloaded() {
        let manager = Arc::new(DownloadManager::new(
            &test_config(),
            tokio::runtime::Handle::current(),
        ));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut song = SongInfo::new(
            "1".to_string(),
            SourceId::Local,
            "本地歌曲".to_string(),
            "歌手".to_string(),
        );
        song.file_path = Some(PathBuf::from("/tmp/local.flac"));

        let sources = Arc::new(SourceManager::new(SourceId::Kw, &[]));
        assert!(!manager.enqueue(song, sources, tx));
        assert!(matches!(rx.try_recv(), Ok(AppAction::ShowNotification(_))));
    }

    /// 跨会话去重：即使文件名模板换了、程序重启过，同一首歌也不再重复下载。
    #[tokio::test]
    async fn previously_downloaded_song_is_skipped_without_resolving_url() {
        let dir = temp_dir("dedup-skip");
        let mut config = test_config();
        config.download.dir = dir.display().to_string();
        config.download.skip_existing = true;
        // 模板与记录里的文件名不同：只有按「歌手 - 歌名」的去重索引才会命中。
        config.download.filename_template = "{name}".to_string();

        let song = SongInfo::new(
            "1".to_string(),
            SourceId::Kw,
            "晴天".to_string(),
            "周杰伦".to_string(),
        );
        let file = dir.join("周杰伦 - 晴天.flac");
        std::fs::write(&file, b"audio").unwrap();

        let records = DownloadRecords::at_path(dir.join("downloads.json"));
        records.record(DownloadRecord::new(
            &song,
            DownloadStatus::Done,
            Some("周杰伦 - 晴天.flac".to_string()),
            None,
        ));

        let manager = Arc::new(DownloadManager::with_records(
            &config,
            tokio::runtime::Handle::current(),
            records,
        ));
        let (tx, _rx) = mpsc::unbounded_channel();
        let sources = Arc::new(SourceManager::new(SourceId::Kw, &[]));
        assert!(manager.enqueue(song, sources, tx));

        let mut view = manager.snapshot();
        for _ in 0..200 {
            view = manager.snapshot();
            if view.iter().any(|task| task.state == DownloadState::Skipped) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(view.len(), 1, "应当只产生一个任务");
        assert_eq!(
            view[0].state,
            DownloadState::Skipped,
            "命中记录后应当直接跳过，不会因为没有网络而失败"
        );
        assert_eq!(view[0].dest, file);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn guess_extension_follows_quality() {
        assert_eq!(guess_extension(Quality::Flac24), "flac");
        assert_eq!(guess_extension(Quality::Low128), "mp3");
    }

    #[test]
    fn finished_tasks_are_trimmed() {
        fn task(id: u64, state: DownloadState) -> DownloadTask {
            DownloadTask {
                id,
                name: format!("song {id}"),
                singer: String::new(),
                source: SourceId::Kw,
                quality: Quality::Flac,
                dest: PathBuf::from(format!("/tmp/{id}.flac")),
                state,
                error: None,
                progress: DownloadProgress::new(),
                started_at: Instant::now(),
                finished_at: None,
                bytes: 0,
                expected_size: None,
                duration_secs: 0,
                trigger: DownloadTrigger::Manual,
            }
        }

        let mut tasks: Vec<DownloadTask> = (0..FINISHED_TASK_LIMIT as u64 + 5)
            .map(|id| task(id, DownloadState::Done))
            .collect();
        tasks.push(task(9999, DownloadState::Downloading));

        trim_finished(&mut tasks);

        assert_eq!(tasks.len(), FINISHED_TASK_LIMIT + 1);
        assert!(tasks.iter().any(|task| task.id == 9999));
    }

    #[test]
    fn download_options_are_clamped_from_config() {
        let mut config = test_config();
        config.download.concurrency = 99;
        config.download.max_retries = 42;
        let options = DownloadOptions::from_config(&config.download);
        assert_eq!(options.concurrency, 16);
        assert_eq!(options.max_retries, 10);
    }

    #[test]
    fn task_view_name_includes_singer() {
        let view = DownloadTaskView {
            id: 1,
            name: "晴天".to_string(),
            singer: "周杰伦".to_string(),
            source: SourceId::Kw,
            quality: Quality::Flac,
            dest: PathBuf::from("/tmp/a.flac"),
            state: DownloadState::Done,
            error: None,
            progress: ProgressSnapshot {
                downloaded: 10,
                total: 20,
                cancelled: false,
            },
            elapsed: Duration::from_secs(1),
            bytes: 20,
            expected_size: Some(20),
            bitrate_kbps: None,
            trigger: DownloadTrigger::Manual,
        };
        assert_eq!(view.display_name(), "晴天 - 周杰伦");
        assert_eq!(view.progress.ratio(), Some(0.5));
    }

    #[test]
    fn bitrate_estimate_uses_size_and_duration() {
        // 4 MB / 100 秒 ≈ 335 kbps。
        assert_eq!(estimate_bitrate(4 * 1024 * 1024, 100), Some(335));
        // 时长未知或体积为 0 时不做估算，避免显示误导性的数字。
        assert_eq!(estimate_bitrate(4 * 1024 * 1024, 0), None);
        assert_eq!(estimate_bitrate(0, 100), None);
        // 极小的文件不应算出 0 kbps。
        assert_eq!(estimate_bitrate(1, 3600), None);
    }

    /// 播放自动缓存：达到阈值才入队，并且同一首歌每会话只入队一次。
    #[tokio::test]
    async fn auto_cache_respects_threshold_and_enqueues_once() {
        let dir = temp_dir("auto-cache");
        let mut config = test_config();
        config.download.dir = dir.display().to_string();
        config.download.auto_cache_on_play = true;
        config.download.auto_cache_after_secs = 10;

        let records = DownloadRecords::at_path(dir.join("downloads.json"));
        let manager = Arc::new(DownloadManager::with_records(
            &config,
            tokio::runtime::Handle::current(),
            records,
        ));
        let (tx, _rx) = mpsc::unbounded_channel();
        let sources = Arc::new(SourceManager::new(SourceId::Kw, &[]));
        let song = SongInfo::new(
            "1".to_string(),
            SourceId::Kw,
            "晴天".to_string(),
            "周杰伦".to_string(),
        );

        manager.maybe_auto_cache(
            &song,
            Duration::from_secs(5),
            Arc::clone(&sources),
            tx.clone(),
        );
        assert!(manager.snapshot().is_empty(), "未达阈值不应入队");

        manager.maybe_auto_cache(
            &song,
            Duration::from_secs(12),
            Arc::clone(&sources),
            tx.clone(),
        );
        assert_eq!(manager.snapshot().len(), 1);

        manager.maybe_auto_cache(&song, Duration::from_secs(30), sources, tx);
        assert_eq!(manager.snapshot().len(), 1, "同一首歌不应重复入队");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn auto_cache_skips_songs_that_are_already_downloaded() {
        let dir = temp_dir("auto-cache-hit");
        let mut config = test_config();
        config.download.dir = dir.display().to_string();
        config.download.auto_cache_on_play = true;
        config.download.auto_cache_after_secs = 0;
        // 即便用户关掉了「跳过已下载」，自动缓存也不该重复下同一首歌。
        config.download.skip_existing = false;

        let song = SongInfo::new(
            "1".to_string(),
            SourceId::Kw,
            "晴天".to_string(),
            "周杰伦".to_string(),
        );
        std::fs::write(dir.join("周杰伦 - 晴天.flac"), b"audio").unwrap();
        let records = DownloadRecords::at_path(dir.join("downloads.json"));
        records.record(DownloadRecord::new(
            &song,
            DownloadStatus::Done,
            Some("周杰伦 - 晴天.flac".to_string()),
            None,
        ));

        let manager = Arc::new(DownloadManager::with_records(
            &config,
            tokio::runtime::Handle::current(),
            records,
        ));
        let (tx, _rx) = mpsc::unbounded_channel();
        let sources = Arc::new(SourceManager::new(SourceId::Kw, &[]));
        manager.maybe_auto_cache(&song, Duration::from_secs(60), sources, tx);

        assert!(manager.snapshot().is_empty(), "已缓存的歌曲不应再次入队");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn auto_cache_is_off_by_default() {
        let dir = temp_dir("auto-cache-off");
        let mut config = test_config();
        config.download.dir = dir.display().to_string();

        let records = DownloadRecords::at_path(dir.join("downloads.json"));
        let manager = Arc::new(DownloadManager::with_records(
            &config,
            tokio::runtime::Handle::current(),
            records,
        ));
        let (tx, _rx) = mpsc::unbounded_channel();
        let sources = Arc::new(SourceManager::new(SourceId::Kw, &[]));
        let song = SongInfo::new(
            "1".to_string(),
            SourceId::Kw,
            "晴天".to_string(),
            "周杰伦".to_string(),
        );

        manager.maybe_auto_cache(&song, Duration::from_secs(600), sources, tx);
        assert!(manager.snapshot().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 假音源：把播放地址指向本地测试服务端，其余能力返回固定数据。
    struct FakeSource {
        audio_url: String,
        cover_url: String,
    }

    #[lx_core::async_trait::async_trait]
    impl MusicSource for FakeSource {
        fn id(&self) -> SourceId {
            SourceId::Kw
        }

        fn name(&self) -> &str {
            "fake"
        }

        async fn search(
            &self,
            _keyword: &str,
            _page: u32,
            _limit: u32,
        ) -> Result<SearchResult, SearchError> {
            Ok(SearchResult {
                items: Vec::new(),
                total: 0,
                has_more: false,
            })
        }

        async fn get_song_url(
            &self,
            _song: &SongInfo,
            quality: Quality,
        ) -> Result<SongUrl, FetchError> {
            Ok(SongUrl {
                url: self.audio_url.clone(),
                quality,
                duration: Duration::from_secs(240),
                cover_url: Some(self.cover_url.clone()),
                qualities: vec![quality],
                headers: Vec::new(),
                size: None,
                size_is_advisory: false,
                md5: None,
                candidate_urls: vec![],
                max_chunk_size: 0,
            })
        }

        async fn get_lyric(&self, _song: &SongInfo) -> Result<LyricData, FetchError> {
            Ok(LyricData {
                lyric: "[00:01.00]第一行\n[00:05.50]第二行\n".to_string(),
                ..LyricData::default()
            })
        }

        async fn get_cover_url(&self, _song: &SongInfo) -> Result<String, FetchError> {
            Ok(self.cover_url.clone())
        }

        fn supported_qualities(&self) -> Vec<Quality> {
            vec![Quality::Flac, Quality::High320]
        }
    }

    async fn wait_for_finish(
        manager: &Arc<DownloadManager>,
        timeout: Duration,
    ) -> Vec<DownloadTaskView> {
        let deadline = Instant::now() + timeout;
        loop {
            let tasks = manager.snapshot();
            if tasks.iter().all(|task| !task.state.is_active()) {
                return tasks;
            }
            if Instant::now() >= deadline {
                panic!("下载任务超时未结束: {tasks:?}");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn downloads_a_song_end_to_end() {
        let audio = fake_audio(1024 * 1024);
        let Some(server) =
            spawn_test_server(audio.clone(), Some(tiny_png()), RangeMode::Supported).await
        else {
            // 沙箱禁止监听端口时跳过。
            return;
        };
        let dir = temp_dir("manager");
        let mut config = Config::default();
        config.download.dir = dir.to_string_lossy().to_string();
        config.download.skip_existing = true;
        config.download.concurrent_songs = 1;

        let manager = Arc::new(DownloadManager::new(
            &config,
            tokio::runtime::Handle::current(),
        ));
        let mut sources = SourceManager::new(SourceId::Kw, &[SourceId::Kw]);
        sources.register(Arc::new(FakeSource {
            audio_url: server.audio_url.clone(),
            cover_url: server.cover_url.clone(),
        }));
        let sources = Arc::new(sources);

        let song = SongInfo::new(
            "42".to_string(),
            SourceId::Kw,
            "晴天".to_string(),
            "周杰伦".to_string(),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();

        assert!(manager.enqueue(song.clone(), Arc::clone(&sources), tx.clone()));
        // 同一首歌重复入队会被 inflight 表拦截（同步插入，必然命中）。
        assert!(!manager.enqueue(song.clone(), Arc::clone(&sources), tx.clone()));
        assert!(rx.try_recv().is_ok(), "入队应当给出即时通知");

        let tasks = wait_for_finish(&manager, Duration::from_secs(20)).await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].state, DownloadState::Done, "{:?}", tasks[0].error);
        let dest = tasks[0].dest.clone();
        assert_eq!(dest.file_name().unwrap(), "周杰伦 - 晴天.flac");
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            audio,
            "落盘内容必须逐字节一致"
        );
        assert!(
            dest.with_extension("lrc").exists(),
            "歌词文件应当与音频同目录"
        );
        assert!(manager.active_count() == 0);

        // 再次入队：文件已存在，按配置跳过。
        assert!(manager.enqueue(song, Arc::clone(&sources), tx));
        let tasks = wait_for_finish(&manager, Duration::from_secs(20)).await;
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[1].state, DownloadState::Skipped);
        assert_eq!(tasks[1].dest, dest);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 主循环跑在 runtime 之外，`enqueue` 必须能在同步上下文里安全地派发任务。
    /// 这里刻意用 `#[test]` + 手动 runtime 复现 `run_app` 的调用环境：
    /// 曾经这里直接用 `tokio::spawn`，一入队就 panic（no reactor running）。
    #[test]
    fn enqueue_from_a_synchronous_context_runs_the_download() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let audio = fake_audio(512 * 1024);
        let Some(server) =
            rt.block_on(spawn_test_server(audio.clone(), None, RangeMode::Supported))
        else {
            // 沙箱禁止监听端口时跳过。
            return;
        };
        let dir = temp_dir("sync-enqueue");
        let mut config = Config::default();
        config.download.dir = dir.to_string_lossy().to_string();

        let manager = Arc::new(DownloadManager::new(&config, rt.handle().clone()));
        let mut sources = SourceManager::new(SourceId::Kw, &[SourceId::Kw]);
        sources.register(Arc::new(FakeSource {
            audio_url: server.audio_url.clone(),
            cover_url: server.cover_url.clone(),
        }));
        let sources = Arc::new(sources);
        let (tx, _rx) = mpsc::unbounded_channel();
        let song = SongInfo::new(
            "sync".to_string(),
            SourceId::Kw,
            "同步入队".to_string(),
            "测试".to_string(),
        );

        // 关键：不在 runtime 上下文里调用，和主循环一致。
        assert!(manager.enqueue(song, Arc::clone(&sources), tx));
        let tasks = rt.block_on(wait_for_finish(&manager, Duration::from_secs(20)));

        assert_eq!(tasks[0].state, DownloadState::Done, "{:?}", tasks[0].error);
        assert_eq!(std::fs::read(&tasks[0].dest).unwrap(), audio);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
