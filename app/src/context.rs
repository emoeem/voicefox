//! 全局应用状态

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lx_core::events::Notification;
use lx_core::model::config::Config;
use lx_core::model::song::SongInfo;
use lx_core::model::source::PlayerState;
use lx_core::traits::player::{
    AbLoop, AudioInfo, ChannelMode, EqualizerBand, Player, ReplayGainMode,
};

use crate::cover::CoverService;
use crate::download::DownloadManager;
use crate::fmt::format_duration;
use crate::notification::DesktopNotifier;
use crate::playlist::manager::PlaylistManager;
use crate::storage::{SavedPlayerState, Storage};
use lx_core::model::source::SourceHealth;
use lx_lyric::service::LyricService;
use lx_source::bili::BiliSource;
use lx_source::manager::SourceManager;

/// 全局共享状态
pub struct AppContext {
    // --- 播放器 ---
    pub player: Arc<dyn Player>,
    pub player_state: tokio::sync::watch::Receiver<PlayerState>,
    pub position: tokio::sync::watch::Receiver<std::time::Duration>,
    pub lyric_position: tokio::sync::watch::Receiver<std::time::Duration>,
    pub duration: tokio::sync::watch::Receiver<std::time::Duration>,
    pub audio_info: tokio::sync::watch::Receiver<AudioInfo>,

    // --- 音源 ---
    pub source_manager: Arc<SourceManager>,
    pub source_health: std::sync::RwLock<Vec<SourceHealth>>,
    pub source_health_checking: std::sync::atomic::AtomicBool,
    pub bili_source: Arc<BiliSource>,

    // --- 歌词 ---
    pub lyric_service: Arc<LyricService>,
    pub cover_service: Arc<CoverService>,

    // --- 播放列表 ---
    pub playlist: Arc<PlaylistManager>,
    /// 音乐下载队列（后台任务，界面只读取快照）。
    pub downloads: Arc<DownloadManager>,
    pub current_song: Arc<std::sync::RwLock<Option<SongInfo>>>,
    pub play_request_id: Arc<AtomicU64>,
    pub active_player_generation: Arc<AtomicU64>,
    /// A-B 循环尚未配对的 A 点，供快捷键和右键菜单共享。
    pub pending_ab_loop_start: std::sync::Mutex<Option<Duration>>,
    pub play_attempted_sources: Arc<std::sync::Mutex<HashSet<lx_core::model::source::SourceId>>>,
    pub play_js_source_index: Arc<std::sync::Mutex<Option<usize>>>,
    /// JS 音源最近一次加载的结果（成功几个、哪些失败、为什么）。
    pub js_source_status: Arc<std::sync::Mutex<JsSourceStatus>>,
    /// 底栏排队等待主循环执行的命令。
    pub status_bar_commands: Arc<std::sync::Mutex<Vec<StatusBarCommand>>>,
    /// 静音前的音量，用于"静音 / 恢复"。
    pub mute_restore_volume: Arc<std::sync::atomic::AtomicU32>,
    pub local_scan_request_id: Arc<AtomicU64>,
    /// 配置延迟落盘：音量/播放控制等高频修改先标记，主循环稍后统一写盘
    config_dirty_since: Arc<std::sync::Mutex<Option<std::time::Instant>>>,
    /// 打开中的歌手/专辑详情浮层；None 表示未打开。
    pub details_page: Arc<std::sync::Mutex<Option<crate::pages::details::DetailsPage>>>,
    /// 当前进度所属的连续时间线，跳转会递增
    position_epoch: AtomicU64,

    // --- 配置 ---
    pub config: std::sync::RwLock<Config>,
    /// 配置文件路径
    pub config_path: PathBuf,

    // --- 通知 ---
    pub notifications: std::sync::RwLock<VecDeque<Notification>>,
    desktop_notifier: DesktopNotifier,

    // --- 存储 ---
    pub storage: Arc<Storage>,

    // --- 睡眠定时器 ---
    /// 倒计时到点后淡出并暂停；由主循环周期 poll 推进，状态栏与菜单只读。
    pub sleep_timer: Arc<crate::sleep_timer::SleepTimerState>,
}

/// 单个 JS 音源的加载失败记录。
///
/// 三样都留着：`name` 让用户一眼认出是哪个音源（复用 loader 的命名逻辑，
/// 与加载成功时 `JsSource` 的名字同源），`origin` 是配置里的原始 URL / 本地路径
/// （用户可以直接照着改配置），`reason` 是真实失败原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsSourceFailure {
    pub name: String,
    pub origin: String,
    pub reason: String,
}

/// 底栏请求的一次性命令。
///
/// 这类操作需要主循环的本地状态（`active_tab`、下载面板实例），没法走
/// `execute_action`，因此排队后由主循环取走执行 —— 与项目既有的
/// "共享状态放在 ctx、由主循环消费"写法保持一致。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusBarCommand {
    /// 开关下载面板（等价 Ctrl+O）。
    ToggleDownloadsPanel,
    /// 跳到队列页并定位到当前播放的那首。
    JumpToQueue,
    /// 清空队列（复用队列页那套"显式清空"的完整收尾）。
    ClearQueue,
    /// 打开某个底栏段的菜单（「更多」里点回具体段时用）。
    OpenStatusBarMenu(crate::pages::components::status_bar::StatusBarSlot),
    /// 跳到「歌单 · 我的歌单」并定位到指定远端歌单（远程歌单窗口的回车动作）。
    OpenAccountPlaylist(String),
    /// 把某页面的分栏比例恢复成内置默认（配置项已删，这里复位内存里的那一份）。
    ResetPaneLayout(String),
}

/// JS 音源的整体加载状态，供状态栏与音源菜单展示。
///
/// 之前这类信息只写 tracing，用户完全看不到"3 个里坏了 1 个、坏的是哪个"，
/// 而实测加载失败是偶发的（同脚本同环境一次 3 个、一次 2 个）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JsSourceStatus {
    /// 配置里配了几个（本次加载的总数）。
    pub total: usize,
    /// 实际加载成功几个。
    pub loaded: usize,
    /// 每个失败音源的名称 / 来源 / 原因。
    pub failures: Vec<JsSourceFailure>,
}

impl JsSourceStatus {
    /// 状态栏上的摘要，例如 `● 自定义音源 3/3`。
    pub fn summary(&self) -> String {
        if self.total == 0 {
            return "自定义音源 未配置".to_string();
        }
        let mark = if self.is_healthy() { "●" } else { "▲" };
        format!("{mark} 自定义音源 {}/{}", self.loaded, self.total)
    }

    /// 全部加载成功才算健康（没配置也算健康，状态栏不应报红）。
    pub fn is_healthy(&self) -> bool {
        self.loaded == self.total
    }
}

impl AppContext {
    pub async fn new(config: Config, config_path: PathBuf) -> anyhow::Result<Self> {
        if !config.player.engine.eq_ignore_ascii_case("mpv") {
            anyhow::bail!("不支持的播放器引擎: {}", config.player.engine);
        }
        lx_source::configure_network(&config.network.proxy_url, config.network.timeout);
        let player: Arc<dyn Player> = Arc::new(lx_player::engine::MpvEngine::new()?);
        player.set_volume(config.player.volume);
        player.set_playback_speed(config.player.playback_speed);
        player.set_audio_output_device(&config.player.audio_device);
        player.set_replaygain_mode(parse_replaygain_mode(&config.player.replaygain_mode));
        player.set_replaygain_preamp(config.player.replaygain_preamp);
        player.set_replaygain_clip(config.player.replaygain_clip);
        player.set_channel_mode(parse_channel_mode(&config.player.channel_mode));
        player.set_balance(config.player.balance);
        player.set_equalizer_bands(&config.player.equalizer_bands);

        // 创建音源管理器（JS 音源在 TUI 启动后异步加载）
        let source_manager = Arc::new(SourceManager::new(
            config.source.default,
            &config.source.enabled,
        ));
        source_manager.set_js_url_first(config.source.js_first);
        let bili_source = source_manager.bili_source();

        let lyric_service = Arc::new(LyricService::new(Arc::new(
            lx_lyric::fetcher::SourceLyricFetcher::new(source_manager.clone()),
        )));
        lyric_service.set_translation_enabled(config.lyric.show_translation);
        lyric_service.set_yrc_enabled(config.lyric.show_yrc);
        lyric_service.set_offset_ms(config.lyric.offset);
        let cover_service = Arc::new(CoverService::new(
            &config.network.proxy_url,
            config.network.timeout,
        ));
        let play_mode = crate::playlist::mode::PlayMode::from_config(&config.player.play_mode);
        let playlist = Arc::new(PlaylistManager::new(play_mode));
        // AppContext 在 runtime 内创建，这里取到的 handle 供下载任务跨线程使用。
        let downloads = Arc::new(DownloadManager::new(
            &config,
            tokio::runtime::Handle::current(),
        ));
        let storage = Arc::new(Storage::new());
        if let Some(state) = storage.load_shuffle_state()
            && !playlist.restore_shuffle(state)
        {
            tracing::warn!("shuffle state version mismatch, starting from an empty state");
        }
        storage.trim_history(config.player.history_limit);

        let player_state = player.state_watcher();
        let position = player.position_watcher();
        let lyric_position = player.audible_position_watcher();
        let duration = player.duration_watcher();
        let audio_info = player.audio_info_watcher();

        Ok(Self {
            player,
            player_state,
            position,
            lyric_position,
            duration,
            audio_info,
            source_manager,
            source_health: std::sync::RwLock::new(Vec::new()),
            source_health_checking: std::sync::atomic::AtomicBool::new(false),
            bili_source,
            lyric_service,
            cover_service,
            playlist,
            downloads,
            current_song: Arc::new(std::sync::RwLock::new(None)),
            play_request_id: Arc::new(AtomicU64::new(0)),
            active_player_generation: Arc::new(AtomicU64::new(0)),
            pending_ab_loop_start: std::sync::Mutex::new(None),
            play_attempted_sources: Arc::new(std::sync::Mutex::new(HashSet::new())),
            play_js_source_index: Arc::new(std::sync::Mutex::new(None)),
            js_source_status: Arc::new(std::sync::Mutex::new(JsSourceStatus::default())),
            status_bar_commands: Arc::new(std::sync::Mutex::new(Vec::new())),
            mute_restore_volume: Arc::new(std::sync::atomic::AtomicU32::new(80)),
            local_scan_request_id: Arc::new(AtomicU64::new(0)),
            config_dirty_since: Arc::new(std::sync::Mutex::new(None)),
            details_page: Arc::new(std::sync::Mutex::new(None)),
            position_epoch: AtomicU64::new(0),
            config: std::sync::RwLock::new(config),
            config_path,
            notifications: std::sync::RwLock::new(VecDeque::new()),
            desktop_notifier: DesktopNotifier::new(),
            storage,
            sleep_timer: Arc::new(crate::sleep_timer::SleepTimerState::new()),
        })
    }

    /// 跳转到指定进度。
    ///
    /// MPRIS 规范要求任何与正常播放 不一致的进度变化都发出 Seeked，因此将该函数作为同一入口。
    pub fn seek(&self, position: Duration) {
        self.player.seek(position);
        self.position_epoch.fetch_add(1, Ordering::Relaxed);
    }

    pub fn stop_player(&self) {
        self.play_request_id.fetch_add(1, Ordering::SeqCst);
        self.active_player_generation.fetch_add(1, Ordering::SeqCst);
        self.player.stop();
    }

    /// 当前进度纪元，变化即代表期间发生过跳转。
    pub fn position_epoch(&self) -> u64 {
        self.position_epoch.load(Ordering::Relaxed)
    }

    pub fn notify(&self, notification: Notification) {
        let (in_app, desktop) = {
            let config = self.config.read().unwrap_or_else(|e| e.into_inner());
            (config.notification.in_app, config.notification.enable)
        };
        if in_app && notification.in_app {
            let mut notifications = self
                .notifications
                .write()
                .unwrap_or_else(|e| e.into_inner());
            notifications.push_back(notification.clone());
            while notifications.len() > 8 {
                notifications.pop_front();
            }
        }
        if desktop && notification.desktop {
            self.desktop_notifier.send(notification);
        }
    }

    pub fn dismiss_notification(&self) -> bool {
        self.notifications
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .pop_back()
            .is_some()
    }

    /// 排队一个底栏命令（菜单动作里没有循环本地状态时用）。
    pub fn queue_status_bar_command(&self, command: StatusBarCommand) {
        self.status_bar_commands
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(command);
    }

    /// 取走待执行的底栏命令。
    pub fn take_status_bar_commands(&self) -> Vec<StatusBarCommand> {
        std::mem::take(
            &mut *self
                .status_bar_commands
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        )
    }

    /// 读取 JS 音源加载状态（状态栏每帧都会问，克隆一份即可）。
    pub fn js_source_status(&self) -> JsSourceStatus {
        self.js_source_status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn notification_timeout(&self) -> Duration {
        let seconds = self
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .notification
            .in_app_timeout
            .clamp(1, 60);
        Duration::from_secs(seconds)
    }

    /// PgUp/PgDn 键盘翻页步长（`ui.page_step`）。
    ///
    /// 滚轮继续用 `ui.scroll_amount`（高频小步），翻页是低频大步，两者
    /// 语义不同。夹到 1..=100，写坏配置也不至于一步跳穿列表或卡死。
    pub fn page_step(&self) -> usize {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .ui
            .page_step
            .clamp(1, 100)
    }

    pub fn persist_playback_session(&self) -> Result<(), String> {
        // 随机播放状态是跨队列的全局状态，不受"记住播放状态"开关约束
        if let Some((revision, state)) = self.playlist.shuffle_state_for_save() {
            match self.storage.save_shuffle_state(&state) {
                Ok(()) => self.playlist.mark_shuffle_saved(revision),
                Err(error) => tracing::warn!("save shuffle state failed: {error}"),
            }
        }
        if !self
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .player
            .remember_playback_state
        {
            return self.storage.clear_playback_session();
        }
        let (playlist, current_index) = self.playlist.snapshot_arc();
        if playlist.is_empty() {
            // 队列为空 ≠ 用户想丢弃上一次的播放会话：更常见的情况是本次启动还没
            // 恢复（会话文件缺失或解析失败），或者用户压根没开始播放。以前这里
            // 直接把会话文件删掉，于是「打开 → 没播放 → 退出」会把上一次保存的
            // 播放状态一并抹掉，表现就是"播放状态从来没保存过"。
            // 真正要丢弃请走显式路径：`forget_playback_session`。
            return Ok(());
        }
        let state = match *self.player_state.borrow() {
            PlayerState::Playing | PlayerState::Loading => SavedPlayerState::Playing,
            PlayerState::Paused => SavedPlayerState::Paused,
            PlayerState::Idle | PlayerState::Stopped => SavedPlayerState::Stopped,
        };
        self.storage.save_playback_session_borrowed(
            &playlist,
            current_index,
            *self.position.borrow(),
            state,
        )
    }

    /// 用户显式丢弃已保存的播放会话（清空队列时调用）。
    ///
    /// 与 [`persist_playback_session`](Self::persist_playback_session) 的区别：
    /// 后者只在队列非空时覆盖会话，队列为空时保持原样，不会误删。
    pub fn forget_playback_session(&self) -> Result<(), String> {
        self.storage.clear_playback_session()
    }

    pub fn cycle_playback_speed(&self) -> String {
        let speed = {
            let mut config = self.config.write().unwrap_or_else(|e| e.into_inner());
            config.player.playback_speed = next_playback_speed(config.player.playback_speed);
            config.player.playback_speed
        };
        self.player.set_playback_speed(speed);
        self.persist_control_update(format!("播放速度: {speed:.2}x"))
    }

    pub fn set_audio_output_device(&self, device: &str) -> String {
        let device = if device.trim().is_empty() {
            "auto"
        } else {
            device.trim()
        };
        self.player.set_audio_output_device(device);
        self.config
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .player
            .audio_device = device.to_string();
        self.persist_control_update(format!("音频设备: {device}"))
    }

    pub fn cycle_replaygain_mode(&self) -> String {
        let mode = {
            let mut config = self.config.write().unwrap_or_else(|e| e.into_inner());
            config.player.replaygain_mode = match config.player.replaygain_mode.as_str() {
                "off" => "track",
                "track" => "album",
                _ => "off",
            }
            .to_string();
            config.player.replaygain_mode.clone()
        };
        self.player
            .set_replaygain_mode(parse_replaygain_mode(&mode));
        self.persist_control_update(format!("ReplayGain: {mode}"))
    }

    pub fn cycle_replaygain_preamp(&self) -> String {
        let preamp = {
            let mut config = self.config.write().unwrap_or_else(|e| e.into_inner());
            config.player.replaygain_preamp =
                next_replaygain_preamp(config.player.replaygain_preamp);
            config.player.replaygain_preamp
        };
        self.player.set_replaygain_preamp(preamp);
        self.persist_control_update(format!("ReplayGain 预放大: {preamp:+.1} dB"))
    }

    pub fn cycle_channel_mode(&self) -> String {
        let mode = {
            let mut config = self.config.write().unwrap_or_else(|e| e.into_inner());
            config.player.channel_mode = match config.player.channel_mode.as_str() {
                "auto" => "stereo",
                "stereo" => "mono",
                "mono" => "left",
                "left" => "right",
                _ => "auto",
            }
            .to_string();
            config.player.channel_mode.clone()
        };
        self.player.set_channel_mode(parse_channel_mode(&mode));
        self.persist_control_update(format!("声道模式: {mode}"))
    }

    pub fn cycle_balance(&self) -> String {
        let balance = {
            let mut config = self.config.write().unwrap_or_else(|e| e.into_inner());
            config.player.balance = next_balance(config.player.balance);
            config.player.balance
        };
        self.player.set_balance(balance);
        self.persist_control_update(format!("左右平衡: {balance:+.2}"))
    }

    pub fn toggle_replaygain_clip(&self) -> String {
        let enabled = {
            let mut config = self.config.write().unwrap_or_else(|e| e.into_inner());
            config.player.replaygain_clip = !config.player.replaygain_clip;
            config.player.replaygain_clip
        };
        self.player.set_replaygain_clip(enabled);
        self.persist_control_update(format!(
            "ReplayGain 削波保护: {}",
            if enabled { "开启" } else { "关闭" }
        ))
    }

    pub fn cycle_equalizer_preset(&self) -> String {
        let bands = {
            let mut config = self.config.write().unwrap_or_else(|e| e.into_inner());
            config.player.equalizer_bands = next_equalizer_preset(&config.player.equalizer_bands);
            config.player.equalizer_bands.clone()
        };
        self.player.set_equalizer_bands(&bands);
        self.persist_control_update(format!("均衡器: {}", equalizer_label(&bands)))
    }

    pub fn fade_in_now(&self) -> String {
        let duration = self
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .player
            .fade_in_ms
            .max(250);
        self.player.fade_in(Duration::from_millis(duration));
        format!("已开始淡入（{}）", fade_duration_label(duration))
    }

    pub fn fade_out_now(&self) -> String {
        let duration = self
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .player
            .fade_out_ms
            .max(250);
        self.player.fade_out(Duration::from_millis(duration));
        format!("已开始淡出（{}）", fade_duration_label(duration))
    }

    pub fn set_ab_loop_start_now(&self) -> String {
        let start = *self.position.borrow();
        if start >= *self.duration.borrow() {
            return "无法设置 A 点：当前没有可循环的播放位置".to_string();
        }
        *self
            .pending_ab_loop_start
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(start);
        format!("A 点: {}", format_duration(start))
    }

    pub fn set_ab_loop_end_now(&self) -> String {
        let end = *self.position.borrow();
        let mut pending = self
            .pending_ab_loop_start
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(start) = *pending else {
            return "请先设置 A 点".to_string();
        };
        let Some(points) = AbLoop::new(start, end) else {
            return "B 点必须晚于 A 点".to_string();
        };
        self.player.set_ab_loop(Some(points));
        *pending = None;
        format!(
            "A-B 循环: {} - {}",
            format_duration(points.start),
            format_duration(points.end)
        )
    }

    pub fn clear_ab_loop(&self) -> String {
        *self
            .pending_ab_loop_start
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        self.player.clear_ab_loop();
        "已清除 A-B 循环".to_string()
    }

    fn persist_control_update(&self, message: String) -> String {
        // 播放控制键可能被连按，这里只标记脏状态，由主循环合并写盘
        self.mark_config_dirty();
        message
    }

    /// 标记配置有待落盘（首次标记时开始计时）
    pub fn mark_config_dirty(&self) {
        let mut since = self
            .config_dirty_since
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if since.is_none() {
            *since = Some(std::time::Instant::now());
        }
    }

    /// 距首次修改超过防抖窗口后统一写盘一次；返回是否实际写入。
    pub fn flush_dirty_config(&self) -> bool {
        const DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(2);
        let due = {
            let mut since = self
                .config_dirty_since
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            match *since {
                Some(t) if t.elapsed() >= DEBOUNCE => {
                    *since = None;
                    true
                }
                _ => false,
            }
        };
        if !due {
            return false;
        }
        if let Err(error) = crate::config::loader::save(
            &self.config.read().unwrap_or_else(|e| e.into_inner()),
            &self.config_path,
        ) {
            tracing::warn!("save config failed: {error}");
        }
        true
    }
}

/// 均衡器预设的**唯一来源**：`(取值标识, 显示标签, 频段)`。
///
/// 设置页的「均衡器」菜单、[`equalizer_label`] 与 `next_equalizer_preset`
/// 全部从这张表读：取值、标签、频段不可能再出现"同源重复声明"。
/// 顺序与 `next_equalizer_preset` 的循环一致。
pub const EQUALIZER_PRESETS: [(&str, &str, [EqualizerBand; 6]); 2] = [
    (
        "bass",
        "低音增强",
        [
            EqualizerBand::new(60.0, 5.0),
            EqualizerBand::new(150.0, 3.0),
            EqualizerBand::new(400.0, 0.0),
            EqualizerBand::new(1_000.0, -1.0),
            EqualizerBand::new(4_000.0, 0.0),
            EqualizerBand::new(12_000.0, 1.0),
        ],
    ),
    (
        "vocal",
        "人声",
        [
            EqualizerBand::new(60.0, -2.0),
            EqualizerBand::new(150.0, -1.0),
            EqualizerBand::new(400.0, 1.0),
            EqualizerBand::new(1_000.0, 3.0),
            EqualizerBand::new(4_000.0, 2.0),
            EqualizerBand::new(12_000.0, -1.0),
        ],
    ),
];

/// 「均衡器」菜单的取值顺序：关闭 + 各预设（`off` 不是预设，没有频段）。
///
/// 直接从 [`EQUALIZER_PRESETS`] 派生，增加一个预设只需改那一张表。
pub const EQUALIZER_CHOICE_VALUES: [&str; 3] =
    ["off", EQUALIZER_PRESETS[0].0, EQUALIZER_PRESETS[1].0];

/// 均衡器预设的取值标识 → 频段（`off` = 关闭，返回空频段）。
///
/// 生产（设置页菜单）与测试共用这一个入口，取值与频段永远同源。
pub fn equalizer_preset_bands(value: &str) -> Option<Vec<EqualizerBand>> {
    if value == "off" {
        return Some(Vec::new());
    }
    EQUALIZER_PRESETS
        .iter()
        .find(|(id, _, _)| *id == value)
        .map(|(_, _, bands)| bands.to_vec())
}

pub(crate) fn equalizer_label(bands: &[EqualizerBand]) -> &'static str {
    if bands.is_empty() {
        return "关闭";
    }
    EQUALIZER_PRESETS
        .iter()
        .find(|(_, _, preset)| bands == preset.as_slice())
        .map(|(_, label, _)| *label)
        .unwrap_or("自定义")
}

fn next_equalizer_preset(bands: &[EqualizerBand]) -> Vec<EqualizerBand> {
    if bands.is_empty() {
        return EQUALIZER_PRESETS[0].2.to_vec();
    }
    if bands == EQUALIZER_PRESETS[0].2.as_slice() {
        return EQUALIZER_PRESETS[1].2.to_vec();
    }
    Vec::new()
}

fn next_playback_speed(speed: f64) -> f64 {
    const VALUES: &[f64] = &[0.5, 0.75, 1.0, 1.25, 1.5, 2.0];
    let index = VALUES
        .iter()
        .position(|value| (*value - speed).abs() < f64::EPSILON)
        .unwrap_or(2);
    VALUES[(index + 1) % VALUES.len()]
}

fn next_replaygain_preamp(value: f64) -> f64 {
    const VALUES: &[f64] = &[-6.0, 0.0, 6.0];
    let index = VALUES
        .iter()
        .position(|candidate| (*candidate - value).abs() < f64::EPSILON)
        .unwrap_or(1);
    VALUES[(index + 1) % VALUES.len()]
}

fn next_balance(value: f64) -> f64 {
    const VALUES: &[f64] = &[-1.0, -0.5, 0.0, 0.5, 1.0];
    let index = VALUES
        .iter()
        .position(|candidate| (*candidate - value).abs() < f64::EPSILON)
        .unwrap_or(2);
    VALUES[(index + 1) % VALUES.len()]
}

fn fade_duration_label(value: u64) -> String {
    if value.is_multiple_of(1_000) {
        format!("{} 秒", value / 1_000)
    } else {
        format!("{value} ms")
    }
}

fn parse_replaygain_mode(value: &str) -> ReplayGainMode {
    match value.trim().to_ascii_lowercase().as_str() {
        "track" => ReplayGainMode::Track,
        "album" => ReplayGainMode::Album,
        _ => ReplayGainMode::Off,
    }
}

fn parse_channel_mode(value: &str) -> ChannelMode {
    match value.trim().to_ascii_lowercase().as_str() {
        "stereo" => ChannelMode::Stereo,
        "mono" => ChannelMode::Mono,
        "left" => ChannelMode::Left,
        "right" => ChannelMode::Right,
        _ => ChannelMode::Auto,
    }
}
