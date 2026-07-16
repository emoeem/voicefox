//! 全局应用状态

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use lx_core::events::Notification;
use lx_core::model::config::Config;
use lx_core::model::song::SongInfo;
use lx_core::model::source::PlayerState;
use lx_core::traits::player::Player;

use crate::cover::CoverService;
use crate::playlist::manager::PlaylistManager;
use crate::storage::Storage;
use lx_lyric::service::LyricService;
use lx_source::manager::SourceManager;

/// 全局共享状态
pub struct AppContext {
    // --- 播放器 ---
    pub player: Arc<dyn Player>,
    pub player_state: tokio::sync::watch::Receiver<PlayerState>,
    pub position: tokio::sync::watch::Receiver<std::time::Duration>,
    pub duration: tokio::sync::watch::Receiver<std::time::Duration>,

    // --- 音源 ---
    pub source_manager: Arc<SourceManager>,

    // --- 歌词 ---
    pub lyric_service: Arc<LyricService>,
    pub cover_service: Arc<CoverService>,

    // --- 播放列表 ---
    pub playlist: Arc<PlaylistManager>,
    pub current_song: Arc<std::sync::RwLock<Option<SongInfo>>>,
    pub play_request_id: Arc<AtomicU64>,

    // --- 配置 ---
    pub config: std::sync::RwLock<Config>,
    /// 配置文件路径
    pub config_path: PathBuf,

    // --- 通知 ---
    pub notifications: std::sync::RwLock<VecDeque<Notification>>,

    // --- 存储 ---
    pub storage: Storage,
}

impl AppContext {
    pub async fn new(config: Config, config_path: PathBuf) -> anyhow::Result<Self> {
        let player: Arc<dyn Player> = Arc::new(lx_player::engine::MpvEngine::new());
        player.set_volume(config.player.volume);

        // 创建音源管理器（JS 音源在 TUI 启动后异步加载）
        let source_manager = Arc::new(SourceManager::new(config.source.default));

        let lyric_service = Arc::new(LyricService::new(Arc::new(
            lx_lyric::fetcher::SourceLyricFetcher::new(source_manager.clone()),
        )));
        lyric_service.set_translation_enabled(config.lyric.show_translation);
        lyric_service.set_yrc_enabled(config.lyric.show_yrc);
        lyric_service.set_offset_ms(config.lyric.offset);
        let cover_service = Arc::new(CoverService::new());
        let play_mode = crate::playlist::mode::PlayMode::from_config(&config.player.play_mode);
        let playlist = Arc::new(PlaylistManager::new(play_mode));

        let player_state = player.state_watcher();
        let position = player.position_watcher();
        let duration = player.duration_watcher();

        Ok(Self {
            player,
            player_state,
            position,
            duration,
            source_manager,
            lyric_service,
            cover_service,
            playlist,
            current_song: Arc::new(std::sync::RwLock::new(None)),
            play_request_id: Arc::new(AtomicU64::new(0)),
            config: std::sync::RwLock::new(config),
            config_path,
            notifications: std::sync::RwLock::new(VecDeque::new()),
            storage: Storage::new(),
        })
    }
}
