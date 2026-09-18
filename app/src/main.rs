//! voicefox: Rust TUI 版 lx-music-desktop
//!
//! 入口：CLI 解析 → 初始化 → 启动 TUI

mod cli;
mod config;
mod context;
mod cover;
mod data_cache;
mod download;
#[cfg(target_os = "linux")]
mod mpris;
mod notification;
mod pages;
mod playlist;
mod storage;

mod theme;
mod tmux;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use std::{
    fs::OpenOptions,
    path::{Path, PathBuf},
};

use anyhow::Context;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use lx_core::events::{AppAction, InsertPosition, Notification};
use lx_core::keybinding::{Action, KeybindingResolver};
use lx_core::model::leaderboard::LeaderboardInfo;
use lx_core::model::playlist::Playlist;
use lx_core::model::song::SongInfo;
use lx_core::model::source::{PlayerState, Quality, SourceId};
use lx_core::traits::player::PlayerEvent;
use lx_core::traits::source::SongUrl;
use ratatui::DefaultTerminal;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::Style;
use tokio::sync::mpsc;

use context::AppContext;
use data_cache::DataCache;
use pages::components;
use pages::components::context_menu::{
    MenuOutcome, PlaybackMenuAction, PlaybackMenuState, SongContextMenu, SongContextMenuOptions,
    SongMenuAction, SongMenuKind,
};
use pages::sidebar::NavTab;
use pages::sort::{SortMode, SortState, SortTarget, SortedListCache};
use storage::SavedPlayerState;

enum LeaderboardResponse {
    Boards {
        request_id: u64,
        source: SourceId,
        result: Result<Vec<LeaderboardInfo>, String>,
    },
    Songs {
        request_id: u64,
        source: SourceId,
        board_id: String,
        result: Result<Vec<SongInfo>, String>,
    },
}

enum PlaylistResponse {
    List {
        request_id: u64,
        source: SourceId,
        page: u32,
        append: bool,
        result: Result<Vec<Playlist>, String>,
    },
    Search {
        request_id: u64,
        source: SourceId,
        keyword: String,
        page: u32,
        append: bool,
        result: Result<Vec<Playlist>, String>,
    },
    Songs {
        request_id: u64,
        source: SourceId,
        playlist_id: String,
        result: Result<Vec<SongInfo>, String>,
    },
    Categories {
        request_id: u64,
        source: SourceId,
        result: Result<Vec<lx_core::model::playlist::PlaylistCategory>, String>,
    },
    User {
        request_id: u64,
        source: SourceId,
        page: u32,
        append: bool,
        result: Result<Vec<Playlist>, String>,
    },
}

#[derive(Debug, Default, Clone, Copy)]
struct UiAreas {
    tabs: Rect,
    content: Rect,
    player_progress: Rect,
    player_controls: Rect,
    notification: Rect,
}

#[derive(Debug, Default)]
struct ClickTracker {
    last_left_click: Option<(Instant, u16, u16)>,
}

#[derive(Debug, Clone)]
struct LocalDeleteConfirmation {
    name: String,
    path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeleteConfirmationAction {
    Confirm,
    Cancel,
    Ignore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalDiagnosticsKind {
    Corrupt,
    Missing,
    Duplicates,
}

impl ClickTracker {
    fn is_double_click(&mut self, event: MouseEvent) -> bool {
        if !matches!(event.kind, MouseEventKind::Down(MouseButton::Left)) {
            return false;
        }
        let doubled = self.last_left_click.is_some_and(|(time, x, y)| {
            x == event.column && y == event.row && time.elapsed() < Duration::from_millis(500)
        });
        self.last_left_click = if doubled {
            None
        } else {
            Some((Instant::now(), event.column, event.row))
        };
        doubled
    }
}

fn delete_confirmation_action(key: &crossterm::event::KeyEvent) -> DeleteConfirmationAction {
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char('y' | 'Y')) => {
            DeleteConfirmationAction::Confirm
        }
        (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char('n' | 'N'))
        | (KeyModifiers::NONE, KeyCode::Esc) => DeleteConfirmationAction::Cancel,
        _ => DeleteConfirmationAction::Ignore,
    }
}

fn nav_page_scope(tab: NavTab) -> &'static str {
    match tab {
        NavTab::Main => "main",
        NavTab::Search => "search",
        NavTab::Leaderboard => "leaderboard",
        NavTab::Playlists => "playlists",
        NavTab::Favorites => "favorites",
        NavTab::History => "history",
        NavTab::LocalMusic => "local",
        NavTab::Downloads => "downloads",
        NavTab::Sources => "sources",
        NavTab::Settings => "settings",
    }
}

fn playback_menu_state(ctx: &AppContext) -> PlaybackMenuState {
    let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
    let ab_loop = ctx
        .player
        .ab_loop()
        .map(|points| {
            format!(
                "{} - {}",
                format_clock(points.start),
                format_clock(points.end)
            )
        })
        .unwrap_or_else(|| "未设置".to_string());
    PlaybackMenuState {
        speed: config.player.playback_speed,
        audio_device: config.player.audio_device.clone(),
        replaygain_mode: config.player.replaygain_mode.clone(),
        replaygain_preamp: config.player.replaygain_preamp,
        replaygain_clip: config.player.replaygain_clip,
        equalizer: context::equalizer_label(&config.player.equalizer_bands).to_string(),
        channel_mode: config.player.channel_mode.clone(),
        balance: config.player.balance,
        ab_loop,
    }
}

fn format_clock(value: Duration) -> String {
    let total = value.as_secs();
    format!("{:02}:{:02}", total / 60, total % 60)
}

fn should_go_to_main(
    active_tab: NavTab,
    page_input_active: bool,
    playlist_open: bool,
    leaderboard_open: bool,
) -> bool {
    !page_input_active
        && active_tab != NavTab::Main
        && active_tab != NavTab::Search
        && active_tab != NavTab::Favorites
        && !(active_tab == NavTab::Playlists && playlist_open)
        && !(active_tab == NavTab::Leaderboard && leaderboard_open)
}

#[allow(clippy::too_many_arguments)]
fn execute_song_menu_action(
    action: SongMenuAction,
    menu: &SongContextMenu,
    main_page: &mut pages::main_page::MainPage,
    ctx: &AppContext,
    rt: &tokio::runtime::Runtime,
    action_tx: &mpsc::UnboundedSender<AppAction>,
    search_page: &Arc<std::sync::Mutex<pages::search::SearchPage>>,
    settings_page: &Arc<std::sync::Mutex<pages::settings::SettingsPage>>,
    search_seq: &Arc<AtomicU64>,
    favorites_page: &mut pages::favorites::FavoritesPage,
    playlists_page: &mut pages::playlists::PlaylistsPage,
    history_state: &mut SortState,
    local_state: &mut SortState,
    confirm_delete: &mut Option<LocalDeleteConfirmation>,
) {
    let app_action = match action {
        SongMenuAction::Play => AppAction::PlaySong {
            songs: menu.songs().to_vec(),
            index: menu.index(),
        },
        SongMenuAction::Download => AppAction::DownloadSong(Box::new(menu.song().clone())),
        SongMenuAction::PlayNext => AppAction::AddToQueue {
            song: Box::new(menu.song().clone()),
            position: InsertPosition::Next,
        },
        SongMenuAction::AddToQueue => AppAction::AddToQueue {
            song: Box::new(menu.song().clone()),
            position: InsertPosition::End,
        },
        SongMenuAction::OpenPlaybackControls => AppAction::None,
        SongMenuAction::OpenCustomPlaylists => {
            ctx.notify(Notification::info("请选择一个自建歌单"));
            AppAction::None
        }
        SongMenuAction::AddToCustomPlaylist(playlist_id) => {
            let song = menu.song();
            match ctx.storage.add_song_to_custom_playlist(&playlist_id, song) {
                Ok(true) => {
                    playlists_page.apply_custom_song_addition(&playlist_id, song);
                    ctx.notify(Notification::success("已加入自建歌单"));
                }
                Ok(false) => ctx.notify(Notification::info("歌曲已经在这个歌单中")),
                Err(error) => ctx.notify(Notification::error(error)),
            }
            AppAction::None
        }
        SongMenuAction::NoCustomPlaylists => {
            ctx.notify(Notification::info("暂无自建歌单，请先进入歌单页按 c 创建"));
            AppAction::None
        }
        SongMenuAction::ToggleFavorite => {
            let song = menu.song();
            let message = if ctx.storage.is_favorite(song) {
                ctx.storage.remove_favorite(song);
                "已取消收藏"
            } else {
                ctx.storage.add_favorite(song);
                "已添加收藏"
            };
            ctx.notify(Notification::success(message));
            AppAction::None
        }
        SongMenuAction::Playback(control) => {
            let message = match control {
                PlaybackMenuAction::CycleSpeed => ctx.cycle_playback_speed(),
                PlaybackMenuAction::UseDefaultAudioDevice => ctx.set_audio_output_device("auto"),
                PlaybackMenuAction::CycleReplayGainMode => ctx.cycle_replaygain_mode(),
                PlaybackMenuAction::CycleReplayGainPreamp => ctx.cycle_replaygain_preamp(),
                PlaybackMenuAction::ToggleReplayGainClip => ctx.toggle_replaygain_clip(),
                PlaybackMenuAction::CycleEqualizer => ctx.cycle_equalizer_preset(),
                PlaybackMenuAction::CycleChannelMode => ctx.cycle_channel_mode(),
                PlaybackMenuAction::CycleBalance => ctx.cycle_balance(),
                PlaybackMenuAction::FadeIn => ctx.fade_in_now(),
                PlaybackMenuAction::FadeOut => ctx.fade_out_now(),
                PlaybackMenuAction::SetAbLoopStart => ctx.set_ab_loop_start_now(),
                PlaybackMenuAction::SetAbLoopEnd => ctx.set_ab_loop_end_now(),
                PlaybackMenuAction::ClearAbLoop => ctx.clear_ab_loop(),
            };
            ctx.notify(Notification::info(message));
            AppAction::None
        }
        SongMenuAction::CycleSort(target) => {
            let mode = match target {
                SortTarget::Favorites => favorites_page.cycle_sort(),
                SortTarget::History => history_state.cycle(),
                SortTarget::Local => local_state.cycle(),
            };
            ctx.notify(Notification::info(format!(
                "排序方式: {}",
                mode.label(target)
            )));
            AppAction::None
        }
        SongMenuAction::RemoveFromQueue => {
            // 菜单打开期间队列可能已变化（自动切歌/插入/删除）：
            // 执行前校验目标位置仍是同一首歌，防止用过期下标误删。
            let expected = menu.song();
            let unchanged = ctx
                .playlist
                .borrow()
                .get(menu.index())
                .is_some_and(|current| {
                    current.id == expected.id && current.source == expected.source
                });
            if !unchanged {
                ctx.notify(Notification::warning(
                    "队列已变化，请重新右键选择要移除的歌曲",
                ));
                AppAction::None
            } else {
                let action = main_page.remove_at(menu.index(), ctx);
                ctx.notify(Notification::success(format!(
                    "已从队列移除: {}",
                    menu.song().name
                )));
                action
            }
        }
        SongMenuAction::RemoveFromHistory => {
            history_state.selected = menu.index().min(menu.songs().len().saturating_sub(2));
            AppAction::RemoveHistory(Box::new(menu.song().clone()))
        }
        SongMenuAction::ClearHistory => {
            history_state.reset_position();
            AppAction::ClearHistory
        }
        SongMenuAction::DeleteLocal => {
            if let Some(path) = &menu.song().file_path {
                *confirm_delete = Some(LocalDeleteConfirmation {
                    name: menu.song().name.clone(),
                    path: path.clone(),
                });
            } else {
                ctx.notify(Notification::error("无法删除：没有本地文件路径"));
            }
            AppAction::None
        }
        SongMenuAction::RemoveFromCustomPlaylist(playlist_id) => {
            let song = menu.song();
            match ctx
                .storage
                .remove_song_from_custom_playlist(&playlist_id, song)
            {
                Ok(true) => {
                    playlists_page.apply_custom_song_removal(&playlist_id, song);
                    ctx.notify(Notification::success("已从自建歌单移除"));
                }
                Ok(false) => ctx.notify(Notification::info("歌曲已经不在这个歌单中")),
                Err(error) => ctx.notify(Notification::error(error)),
            }
            AppAction::None
        }
        SongMenuAction::ViewArtist => match menu.songs().get(menu.index()) {
            Some(song) if !song.singer.trim().is_empty() => {
                AppAction::ShowArtistDetails(Box::new(song.clone()))
            }
            _ => AppAction::None,
        },
        SongMenuAction::ViewAlbum => match menu.songs().get(menu.index()) {
            Some(song) if !song.album_name.trim().is_empty() => {
                AppAction::ShowAlbumDetails(Box::new(lx_core::model::playlist::Album {
                    id: song.album_id.clone(),
                    name: song.album_name.clone(),
                    source: song.source,
                    cover_url: song.cover_url.clone(),
                    artist: song.singer.clone(),
                }))
            }
            _ => AppAction::None,
        },
    };
    execute_action(
        app_action,
        ctx,
        rt,
        action_tx,
        search_page,
        settings_page,
        search_seq,
    );
}

fn main() -> anyhow::Result<()> {
    tmux::prepare_ratatui_image_environment();

    // 解析 CLI
    let cli = cli::Cli::parse();

    if cli.check_libmpv {
        let player = lx_player::engine::MpvEngine::new()
            .context("libmpv 运行时自检失败，请确认程序与动态库来自同一个发布包")?;
        drop(player);
        println!("libmpv runtime check passed");
        return Ok(());
    }

    if let Some(path) = cli.export_data.as_deref() {
        storage::Storage::new()
            .export_data(path)
            .map_err(anyhow::Error::msg)?;
        println!("数据已导出到 {}", path.display());
        return Ok(());
    }

    if let Some(path) = cli.import_data.as_deref() {
        let backup = storage::Storage::new()
            .import_data(path)
            .map_err(anyhow::Error::msg)?;
        println!("数据导入完成；原数据已备份到 {}", backup.display());
        return Ok(());
    }

    if let Some(path) = cli.import_playlist.as_deref() {
        let report = storage::Storage::new()
            .import_external_playlist(path)
            .map_err(anyhow::Error::msg)?;
        println!(
            "歌单导入完成: {}（导入 {} 首，跳过 {} 首）",
            report.playlist_name, report.imported, report.skipped
        );
        return Ok(());
    }

    // 加载配置
    let (cfg, config_path) = config::loader::load(&cli.config)?;
    init_logging(&cli.log_level, &config_path);
    tracing::info!(
        "voicefox starting on {} ({})",
        std::env::consts::OS,
        std::env::consts::ARCH
    );

    // 构建 tokio runtime（多线程）
    let rt = tokio::runtime::Runtime::new()?;

    // 初始化 AppContext
    let ctx = rt.block_on(AppContext::new(cfg, config_path))?;

    // 启动 TUI
    let mut terminal = ratatui::init();
    let mouse_enabled = ctx
        .config
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .ui
        .enable_mouse;
    if mouse_enabled {
        let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);
    }

    // 安装 crossterm panic hook，确保 panic 时 restore 终端。
    // 仅主线程 panic 时才恢复终端：后台任务（tokio worker、事件线程等）
    // panic 时进程仍会继续运行主循环，提前 restore 会让后续 draw 把
    // 转义序列写进裸 shell，彻底打花终端。
    //
    // 注意：不能用“线程名是否为 None”来判断主线程 —— Rust 从 1.62 起
    // 主线程名就是 Some("main")，那样判断会把主线程 panic 当成后台 panic，
    // 终端留在鼠标上报模式，shell 里会不断冒出 `^[[<…M` 之类的转义序列。
    let main_thread_id = std::thread::current().id();
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("fatal panic: {info}");
        if std::thread::current().id() == main_thread_id {
            if mouse_enabled {
                let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
            }
            ratatui::restore();
        }
        original_hook(info);
    }));

    let result = run_app(&mut terminal, ctx, &rt);
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    rt.shutdown_timeout(Duration::from_secs(1));

    result
}

fn init_logging(level: &str, config_path: &Path) {
    use tracing_subscriber::fmt::writer::BoxMakeWriter;

    let log_path = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("voicefox.log");
    let writer = match OpenOptions::new().create(true).append(true).open(log_path) {
        Ok(file) => BoxMakeWriter::new(file),
        Err(_) => BoxMakeWriter::new(std::io::sink),
    };
    let default_filter =
        format!("voicefox={level},lx_source={level},lx_player={level},lx_lyric={level}");
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .or_else(|_| tracing_subscriber::EnvFilter::try_new(default_filter))
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(writer)
        .try_init();
}

fn configure_background_command(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

fn open_external_url(url: &str) {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "start", "", url]);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg(url);
        command
    };
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(url);
        command
    };

    configure_background_command(&mut command);
    if let Err(error) = command.spawn() {
        tracing::warn!("failed to open notification URL: {error}");
    }
}

/// 两次自动重传封面之间的最小间隔，client-attached hook 可能连续触发，需要防抖
const COVER_REDRAW_THROTTLE: Duration = Duration::from_secs(2);

#[allow(unused_assignments)]
fn run_app(
    terminal: &mut DefaultTerminal,
    ctx: AppContext,
    rt: &tokio::runtime::Runtime,
) -> anyhow::Result<()> {
    let (action_tx, mut action_rx) = mpsc::unbounded_channel::<AppAction>();
    let (leaderboard_tx, mut leaderboard_rx) = mpsc::unbounded_channel::<LeaderboardResponse>();
    let (playlist_tx, mut playlist_rx) = mpsc::unbounded_channel::<PlaylistResponse>();
    let mut player_event_rx = ctx.player.take_event_receiver();
    #[cfg(target_os = "linux")]
    let (mpris_handle, mut mpris_command_rx) = if ctx
        .config
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .integration
        .mpris
    {
        match rt.block_on(mpris::start()) {
            Ok((handle, receiver)) => (Some(handle), Some(receiver)),
            Err(error) => {
                tracing::warn!("MPRIS unavailable: {error}");
                ctx.notify(Notification::warning(format!("MPRIS 启动失败: {error}")).tui_only());
                (None, None)
            }
        }
    } else {
        (None, None)
    };

    // 搜索请求序列号（用于取消过时请求）
    let search_seq: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    let mut leaderboard_request_id: u64 = 0;
    let mut playlist_request_id: u64 = 0;

    // 键位解析器（从配置加载自定义键位）
    let keybindings = ctx
        .config
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .keybindings
        .clone();
    let kb_resolver = KeybindingResolver::from_config(&keybindings);

    // 导航状态
    let mut active_tab = NavTab::Main;
    let mut observed_active_tab = active_tab;

    // 页面状态
    let (search_source_filter, wrap_navigation, scroll_amount, enabled_sources) = {
        let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
        (
            if config.ui.aggregate_search {
                None
            } else {
                Some(config.source.default)
            },
            config.ui.wrap_navigation,
            config.ui.scroll_amount,
            config.source.enabled.clone(),
        )
    };
    let search_page = Arc::new(std::sync::Mutex::new(pages::search::SearchPage::new(
        search_source_filter,
        wrap_navigation,
        scroll_amount,
        &enabled_sources,
    )));
    let settings_page = Arc::new(std::sync::Mutex::new(pages::settings::SettingsPage::new()));
    let sources_page = Arc::new(std::sync::Mutex::new(pages::sources::SourcesPage::new()));
    let (cover_protocol, mut cover_enabled) = {
        let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
        (config.ui.cover_protocol.clone(), config.ui.show_cover)
    };
    let mut main_page = pages::main_page::MainPage::new(cover::CoverRenderer::detect(
        &cover_protocol,
        cover_enabled,
    ));
    let mut leaderboard =
        pages::leaderboard::LeaderboardPage::new(ctx.source_manager.leaderboard_sources());
    let mut playlists = pages::playlists::PlaylistsPage::new(ctx.source_manager.playlist_sources());
    let mut favorites_page = pages::favorites::FavoritesPage::new();
    let mut history_state = SortState::new(SortMode::Newest);
    let mut local_state = SortState::new(SortMode::TitleAsc);
    let mut data_cache = DataCache::default();
    let mut local_filter = components::list_filter::ListFilter::new();
    let mut history_filter = components::list_filter::ListFilter::new();
    let mut confirm_delete: Option<LocalDeleteConfirmation> = None;
    let mut local_diagnostics: Option<LocalDiagnosticsKind> = None;
    let mut song_menu: Option<SongContextMenu> = None;
    let mut ui_areas = UiAreas::default();
    let mut click_tracker = ClickTracker::default();
    let mut qr_login_page: Option<Arc<std::sync::Mutex<pages::qr_login::QrLoginPage>>> = None;
    // 快捷键说明浮层（? / \ 开关）
    let mut help_page: Option<pages::help::HelpPage> = None;
    // 下载面板浮层（Ctrl+o 开关）
    let mut downloads_panel = pages::downloads::DownloadsPanel::new();
    let mut qr_poll_deadline: Instant = Instant::now();
    let mut qr_generate_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut qr_poll_task: Option<tokio::task::JoinHandle<()>> = None;

    // 事件驱动渲染：借鉴 rmpc，只在有事件或需要渲染时才 draw()
    let mut needs_render = true;
    // 播放器状态由播放线程改写，没有事件通知，只能靠比对上一轮的值发现变化
    let mut last_player_state = *ctx.player_state.borrow();
    let max_fps = ctx
        .config
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .ui
        .max_fps
        .clamp(1, 60);
    let render_interval = Duration::from_millis(1_000 / u64::from(max_fps));
    let mut last_periodic_render = Instant::now();
    let mut last_notification_cleanup = Instant::now();
    let mut last_playback_session_save = Instant::now();
    let mut last_auto_cache_check = Instant::now();
    let mut last_local_watch_generation = ctx.source_manager.local_source().watch_generation();
    let mut faded_generation = 0_u64;
    let mut mouse_capture_enabled = ctx
        .config
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .ui
        .enable_mouse;
    // 安装 tmux 的 client-attached hook，析构时自动卸载
    let attach_watcher = tmux::AttachWatcher::install();
    let mut last_cover_redraw = Instant::now() - COVER_REDRAW_THROTTLE;
    #[cfg(target_os = "linux")]
    let mut last_mpris_snapshot: Option<mpris::MprisSnapshot> = None;
    #[cfg(target_os = "linux")]
    let mut last_mpris_update = Instant::now() - Duration::from_secs(1);
    #[cfg(target_os = "linux")]
    let mut last_position_epoch = ctx.position_epoch();

    // === 后台异步加载 JS 音源（不阻塞启动） ===
    let js_urls = ctx
        .config
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .source
        .js_sources
        .clone();
    let default_source = ctx
        .config
        .read()
        .unwrap()
        .source
        .default
        .as_str()
        .to_string();
    let js_source_generation = ctx.source_manager.begin_js_source_request(false);
    spawn_js_source_loader(
        js_urls,
        default_source,
        Arc::clone(&ctx.source_manager),
        js_source_generation,
        action_tx.clone(),
        rt,
    );

    if ctx
        .config
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .player
        .remember_playback_state
        && let Some(session) = ctx.storage.load_playback_session()
    {
        let (start_playback, paused) = playback_restore_flags(session.state);
        execute_action(
            AppAction::RestorePlayback {
                songs: session.playlist,
                index: session.current_index,
                position: session.position,
                start_playback,
                paused,
            },
            &ctx,
            rt,
            &action_tx,
            &search_page,
            &settings_page,
            &search_seq,
        );
    }

    // === 初始扫描本地音乐 ===
    let local_music_paths = ctx
        .config
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .local_music
        .paths
        .clone();
    let local_music_max_depth = ctx
        .config
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .local_music
        .max_depth;
    if !local_music_paths.is_empty()
        && ctx
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .local_music
            .enabled
    {
        execute_action(
            AppAction::ScanLocalMusic {
                paths: local_music_paths,
                max_depth: local_music_max_depth,
                force: false,
            },
            &ctx,
            rt,
            &action_tx,
            &search_page,
            &settings_page,
            &search_seq,
        );
    }

    rt.spawn(cover::sweep_temp_files());

    if ctx.bili_source.is_logged_in() {
        let bili_source = Arc::clone(&ctx.bili_source);
        let tx = action_tx.clone();
        rt.spawn(async move {
            match tokio::time::timeout(Duration::from_secs(8), bili_source.login_status()).await {
                Ok(Ok(Some(_))) => {}
                Ok(Ok(None)) => {
                    let _ = tx.send(AppAction::ShowNotification(Notification::warning(
                        "哔哩哔哩登录已失效，请重新扫码",
                    )));
                }
                Ok(Err(error)) => tracing::warn!("validate Bilibili session failed: {error}"),
                Err(_) => tracing::warn!("validate Bilibili session timed out"),
            }
            let _ = tx.send(AppAction::None);
        });
    }

    loop {
        if let Some(watcher) = attach_watcher.as_ref()
            && should_retransmit_cover(last_cover_redraw.elapsed())
            && watcher.take_attached()
        {
            tracing::debug!("client attached, retransmitting cover");
            last_cover_redraw = Instant::now();
            retransmit_cover(terminal, &mut main_page)?;
            needs_render = true;
        }

        #[cfg(target_os = "linux")]
        if let Some(receiver) = mpris_command_rx.as_mut() {
            while let Ok(command) = receiver.try_recv() {
                if execute_mpris_command(
                    command,
                    &ctx,
                    rt,
                    &action_tx,
                    &search_page,
                    &settings_page,
                    &search_seq,
                ) {
                    tracing::info!("quit requested through MPRIS");
                    if let Err(error) = ctx.persist_playback_session() {
                        tracing::warn!("save playback session failed: {error}");
                    }
                    ctx.stop_player();
                    return Ok(());
                }
                needs_render = true;
            }
        }

        let mouse_requested = ctx
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .ui
            .enable_mouse;
        if mouse_requested != mouse_capture_enabled {
            if mouse_requested {
                let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);
            } else {
                let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
            }
            mouse_capture_enabled = mouse_requested;
        }

        let cover_requested = ctx
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .ui
            .show_cover;
        if cover_requested != cover_enabled {
            if cover_requested {
                let cover_url = ctx
                    .current_song
                    .read()
                    .unwrap()
                    .as_ref()
                    .and_then(|song| song.cover_url.clone());
                let cover_service = Arc::clone(&ctx.cover_service);
                let wake_tx = action_tx.clone();
                rt.spawn(async move {
                    if let Err(error) = cover_service.load(cover_url).await {
                        tracing::debug!("load cover after enabling failed: {error}");
                    }
                    let _ = wake_tx.send(AppAction::None);
                });
            } else {
                main_page.release_cover_image();
            }
            cover_enabled = cover_requested;
            needs_render = true;
        }

        if observed_active_tab != active_tab {
            let previous_tab = observed_active_tab;
            observed_active_tab = active_tab;
            let local_source = ctx.source_manager.local_source();
            let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
            if should_scan_local_music_on_entry(
                previous_tab,
                active_tab,
                config.local_music.enabled,
                !config.local_music.paths.is_empty(),
                local_source.song_count() == 0,
                local_source.is_scanning(),
            ) {
                let action = AppAction::ScanLocalMusic {
                    paths: config.local_music.paths.clone(),
                    max_depth: config.local_music.max_depth,
                    force: false,
                };
                drop(config);
                execute_action(
                    action,
                    &ctx,
                    rt,
                    &action_tx,
                    &search_page,
                    &settings_page,
                    &search_seq,
                );
                local_state.reset_position();
                needs_render = true;
            }
        }

        // === 0. 排空异步 action ===
        while let Ok(action) = action_rx.try_recv() {
            // 拦截哔哩哔哩登录相关 action
            match &action {
                AppAction::QrLogin(source) => {
                    if let Some(task) = qr_generate_task.take() {
                        task.abort();
                    }
                    if let Some(task) = qr_poll_task.take() {
                        task.abort();
                    }
                    let page = Arc::new(std::sync::Mutex::new(pages::qr_login::QrLoginPage::new(
                        *source,
                        source.display_name().to_string(),
                    )));
                    let page_clone = Arc::clone(&page);
                    let wake_tx = action_tx.clone();
                    let manager = Arc::clone(&ctx.source_manager);
                    let source_id = *source;
                    qr_generate_task = Some(rt.spawn(async move {
                        let result = manager.create_qr_login(source_id).await;
                        let mut page = page_clone.lock().unwrap_or_else(|e| e.into_inner());
                        match result {
                            Ok(session) => page.set_qr(session),
                            Err(error) => page.set_error(format!("生成二维码失败: {error}")),
                        }
                        let _ = wake_tx.send(AppAction::None);
                    }));
                    qr_login_page = Some(page);
                    qr_poll_deadline = Instant::now();
                    needs_render = true;
                    continue;
                }
                AppAction::QrLoginSuccess(source) => {
                    if let Some(task) = qr_generate_task.take() {
                        task.abort();
                    }
                    if let Some(task) = qr_poll_task.take() {
                        task.abort();
                    }
                    qr_login_page = None;
                    let label = source.display_name();
                    let message = if ctx.source_manager.is_logged_in(*source) {
                        format!("{label}登录成功")
                    } else {
                        format!("{label}登录状态未确认，请重新打开设置查看")
                    };
                    ctx.notify(Notification::success(message));
                    needs_render = true;
                    continue;
                }
                AppAction::QrLogout(source) => {
                    if let Some(task) = qr_generate_task.take() {
                        task.abort();
                    }
                    if let Some(task) = qr_poll_task.take() {
                        task.abort();
                    }
                    qr_login_page = None;
                    let label = source.display_name();
                    let notification = match ctx.source_manager.logout(*source) {
                        Ok(()) => Notification::success(format!("已退出{label}登录")),
                        Err(error) => Notification::error(error),
                    };
                    ctx.notify(notification);
                    needs_render = true;
                    continue;
                }
                _ => {}
            }
            execute_action(
                action,
                &ctx,
                rt,
                &action_tx,
                &search_page,
                &settings_page,
                &search_seq,
            );
            needs_render = true;
        }
        if let Some(rx) = player_event_rx.as_mut() {
            while let Ok(event) = rx.try_recv() {
                match event {
                    PlayerEvent::Playing { generation }
                        if generation == ctx.active_player_generation.load(Ordering::SeqCst) =>
                    {
                        ctx.playlist.mark_playback_started();
                    }
                    PlayerEvent::Ended { generation }
                        if generation == ctx.active_player_generation.load(Ordering::SeqCst) =>
                    {
                        if let Some((songs, index)) = ctx.playlist.next_entry_arc() {
                            let _ = action_tx.send(AppAction::PlayFromQueue { songs, index });
                        }
                    }
                    PlayerEvent::Error {
                        generation,
                        message: error,
                    } if generation == ctx.active_player_generation.load(Ordering::SeqCst) => {
                        let retry_song = ctx
                            .current_song
                            .read()
                            .unwrap_or_else(|e| e.into_inner())
                            .clone();
                        let auto_toggle = ctx
                            .config
                            .read()
                            .unwrap_or_else(|e| e.into_inner())
                            .source
                            .auto_toggle;
                        if auto_toggle
                            && retry_song
                                .as_ref()
                                .is_some_and(|song| song.source != SourceId::Local)
                        {
                            tracing::warn!("current source playback failed: {}", error);
                            if let Some(song) = retry_song {
                                let _ = action_tx.send(AppAction::ShowNotification(
                                    Notification::warning("当前音源播放失败，正在尝试其他音源"),
                                ));
                                let _ = action_tx.send(AppAction::RetrySong {
                                    song: Box::new(song),
                                });
                            }
                        } else {
                            let _ = action_tx.send(AppAction::PlaybackFailed {
                                request_id: ctx.play_request_id.load(Ordering::SeqCst),
                                error: format!("播放器错误: {error}"),
                            });
                        }
                    }
                    PlayerEvent::Buffering {
                        generation,
                        percent,
                    } if generation == ctx.active_player_generation.load(Ordering::SeqCst) => {
                        tracing::trace!("libmpv buffering: {:.0}%", percent * 100.0);
                    }
                    stale_event => {
                        tracing::debug!("ignoring stale player event: {stale_event:?}");
                    }
                }
                needs_render = true;
            }
        }
        // 排行榜异步结果
        while let Ok(response) = leaderboard_rx.try_recv() {
            match response {
                LeaderboardResponse::Boards {
                    request_id,
                    source,
                    result,
                } if request_id == leaderboard_request_id
                    && leaderboard.current_source() == Some(source) =>
                {
                    match result {
                        Ok(boards) => leaderboard.update_boards(source, boards),
                        Err(error) => {
                            let request =
                                pages::leaderboard::LeaderboardLoadRequest::Boards { source };
                            leaderboard.update_error(&request, error.clone());
                            ctx.notify(Notification::error(format!("加载榜单目录失败: {error}")));
                        }
                    }
                    needs_render = true;
                }
                LeaderboardResponse::Songs {
                    request_id,
                    source,
                    board_id,
                    result,
                } if request_id == leaderboard_request_id
                    && leaderboard.current_source() == Some(source)
                    && leaderboard.current_board().map(|board| board.id.as_str())
                        == Some(board_id.as_str()) =>
                {
                    match result {
                        Ok(songs) => leaderboard.update_songs(source, &board_id, songs),
                        Err(error) => {
                            let request = pages::leaderboard::LeaderboardLoadRequest::Songs {
                                source,
                                board_id,
                            };
                            leaderboard.update_error(&request, error.clone());
                            ctx.notify(Notification::error(format!("加载榜单歌曲失败: {error}")));
                        }
                    }
                    needs_render = true;
                }
                _ => {}
            }
        }
        while let Ok(response) = playlist_rx.try_recv() {
            match response {
                PlaylistResponse::List {
                    request_id,
                    source,
                    page,
                    append,
                    result,
                } if request_id == playlist_request_id
                    && playlists.current_source() == Some(source) =>
                {
                    match result {
                        Ok(items) => playlists.update_playlists(source, page, append, items),
                        Err(error) => {
                            let category = playlists.current_category();
                            let request = pages::playlists::PlaylistLoadRequest::List {
                                source,
                                category,
                                page,
                                append,
                            };
                            playlists.update_error(&request, error.clone());
                            ctx.notify(Notification::error(format!("加载热门歌单失败: {error}")));
                        }
                    }
                    needs_render = true;
                }
                PlaylistResponse::Search {
                    request_id,
                    source,
                    keyword,
                    page,
                    append,
                    result,
                } if request_id == playlist_request_id
                    && playlists.current_source() == Some(source)
                    && playlists.search_keyword() == Some(keyword.as_str()) =>
                {
                    match result {
                        Ok(items) => playlists.update_playlists(source, page, append, items),
                        Err(error) => {
                            let request = pages::playlists::PlaylistLoadRequest::Search {
                                source,
                                keyword,
                                page,
                                append,
                            };
                            playlists.update_error(&request, error.clone());
                            ctx.notify(Notification::info(error));
                        }
                    }
                    needs_render = true;
                }
                PlaylistResponse::Songs {
                    request_id,
                    source,
                    playlist_id,
                    result,
                } if request_id == playlist_request_id
                    && playlists
                        .current_playlist()
                        .map(|playlist| (playlist.source, playlist.id.as_str()))
                        == Some((source, playlist_id.as_str())) =>
                {
                    match result {
                        Ok(songs) => playlists.update_songs(source, &playlist_id, songs),
                        Err(error) => {
                            let request = pages::playlists::PlaylistLoadRequest::Songs {
                                source,
                                playlist_id,
                            };
                            playlists.update_error(&request, error.clone());
                            ctx.notify(Notification::error(format!("加载歌单歌曲失败: {error}")));
                        }
                    }
                    needs_render = true;
                }
                PlaylistResponse::Categories {
                    request_id,
                    source,
                    result,
                } if request_id == playlist_request_id => {
                    playlists.apply_categories(source, result);
                    needs_render = true;
                }
                PlaylistResponse::User {
                    request_id,
                    source,
                    page,
                    append,
                    result,
                } if request_id == playlist_request_id
                    && playlists.current_source() == Some(source) =>
                {
                    match result {
                        Ok(items) => playlists.update_playlists(source, page, append, items),
                        Err(error) => {
                            let request = pages::playlists::PlaylistLoadRequest::User {
                                source,
                                page,
                                append,
                            };
                            playlists.update_error(&request, error.clone());
                            ctx.notify(Notification::info(error));
                        }
                    }
                    needs_render = true;
                }
                _ => {}
            }
        }
        if active_tab == NavTab::Leaderboard {
            maybe_spawn_leaderboard_load(
                &mut leaderboard,
                &mut leaderboard_request_id,
                Arc::clone(&ctx.source_manager),
                leaderboard_tx.clone(),
                rt,
            );
        }
        if active_tab == NavTab::Playlists {
            playlists.sync_saved_playlists(&ctx);
            maybe_spawn_playlist_load(
                &mut playlists,
                &mut playlist_request_id,
                Arc::clone(&ctx.source_manager),
                playlist_tx.clone(),
                rt,
            );
        }

        // === 1. 周期维护 ===
        // 这些工作必须独立于终端事件执行，否则持续按键或拖动鼠标会让
        // 搜索防抖、歌词同步和进度渲染长期得不到运行机会。
        {
            // 防抖计时在切页后也要继续走，否则输入后 300ms 内切走标签
            // 会永远丢掉这次搜索
            let action = {
                let mut sp = search_page.lock().unwrap_or_else(|e| e.into_inner());
                sp.tick()
            };
            if let Some(action) = action {
                execute_action(
                    action,
                    &ctx,
                    rt,
                    &action_tx,
                    &search_page,
                    &settings_page,
                    &search_seq,
                );
                needs_render = true;
            }
        }

        if qr_generate_task
            .as_ref()
            .is_some_and(tokio::task::JoinHandle::is_finished)
        {
            qr_generate_task.take();
        }
        if qr_poll_task
            .as_ref()
            .is_some_and(tokio::task::JoinHandle::is_finished)
        {
            qr_poll_task.take();
        }

        if let Some(ref page) = qr_login_page
            && qr_poll_task.is_none()
            && qr_poll_deadline.elapsed() >= Duration::from_secs(2)
        {
            let params = {
                let mut page = page.lock().unwrap_or_else(|e| e.into_inner());
                if page.should_poll() {
                    page.begin_poll()
                } else {
                    None
                }
            };
            if let Some((source, key)) = params {
                let page = Arc::clone(page);
                let wake_tx = action_tx.clone();
                let manager = Arc::clone(&ctx.source_manager);
                qr_poll_task = Some(rt.spawn(async move {
                    let result = manager
                        .check_qr_login(source, &key)
                        .await
                        .map_err(|error| error.to_string());
                    let mut page = page.lock().unwrap_or_else(|e| e.into_inner());
                    page.apply_check_result(result);
                    let success = page.succeeded().is_some();
                    drop(page);
                    // 登录成功后交给主循环收尾（关闭页面、发通知）。
                    let _ = wake_tx.send(if success {
                        AppAction::QrLoginSuccess(source)
                    } else {
                        AppAction::None
                    });
                }));
                qr_poll_deadline = Instant::now();
                needs_render = true;
            }
        }

        if last_notification_cleanup.elapsed() >= Duration::from_millis(250) {
            let notifications_enabled = ctx
                .config
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .notification
                .in_app;
            let lifetime = ctx.notification_timeout();
            let mut notifs = ctx.notifications.write().unwrap_or_else(|e| e.into_inner());
            let previous_len = notifs.len();
            if notifications_enabled {
                notifs.retain(|notification| !notification.is_expired(lifetime));
            } else {
                notifs.clear();
            }
            if notifs.len() != previous_len {
                needs_render = true;
            }
            last_notification_cleanup = Instant::now();
        }
        if last_playback_session_save.elapsed() >= Duration::from_secs(5) {
            if let Err(error) = ctx.persist_playback_session() {
                tracing::warn!("save playback session failed: {error}");
            }
            last_playback_session_save = Instant::now();
        }
        // 播放自动缓存：按秒检查播放进度，达到阈值后只入队一次。
        if last_auto_cache_check.elapsed() >= Duration::from_secs(1) {
            last_auto_cache_check = Instant::now();
            let song = ctx
                .current_song
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            if let Some(song) = song {
                let position = *ctx.position.borrow();
                ctx.downloads.maybe_auto_cache(
                    &song,
                    position,
                    Arc::clone(&ctx.source_manager),
                    action_tx.clone(),
                );
            }
        }

        // 本地目录监听器在后台完成增量扫描后递增代次；让 TUI 及时显示新增、删除
        // 或标签修改，而不必等用户手动切换页面。
        let local_watch_generation = ctx.source_manager.local_source().watch_generation();
        if local_watch_generation != last_local_watch_generation {
            last_local_watch_generation = local_watch_generation;
            needs_render = true;
        }

        // borrow 很便宜，所以不受 render_interval 门控
        let state = *ctx.player_state.borrow();
        if state != last_player_state {
            last_player_state = state;
            needs_render = true;
        }
        // Start the configured fade-out near the end of a track.  The engine
        // keeps the logical volume unchanged, so the next track can fade back
        // in without losing the user's volume preference.
        let active_generation = ctx.active_player_generation.load(Ordering::Acquire);
        let (fade_out_ms, position, duration) = {
            let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
            (
                config.player.fade_out_ms,
                *ctx.position.borrow(),
                *ctx.duration.borrow(),
            )
        };
        if active_generation != faded_generation {
            faded_generation = 0;
        }
        if state == PlayerState::Playing
            && fade_out_ms > 0
            && !duration.is_zero()
            && position < duration
            && position >= duration.saturating_sub(Duration::from_millis(fade_out_ms))
            && faded_generation != active_generation
        {
            ctx.player.fade_out(Duration::from_millis(fade_out_ms));
            faded_generation = active_generation;
        }
        #[cfg(target_os = "linux")]
        if let Some(handle) = mpris_handle.as_ref() {
            // 例行更新按 250ms 限频，但跳转要立刻放行
            let position_epoch = ctx.position_epoch();
            if position_epoch != last_position_epoch
                || last_mpris_update.elapsed() >= Duration::from_millis(250)
            {
                last_position_epoch = position_epoch;
                let snapshot = current_mpris_snapshot(&ctx);
                if last_mpris_snapshot.as_ref() != Some(&snapshot) {
                    handle.update(snapshot.clone());
                    last_mpris_snapshot = Some(snapshot);
                }
                last_mpris_update = Instant::now();
            }
        }

        if last_periodic_render.elapsed() >= render_interval {
            ctx.lyric_service
                .update_position(*ctx.lyric_position.borrow());
            let input_active = active_tab == NavTab::Search
                && search_page
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .input_mode
                || active_tab == NavTab::Settings
                    && settings_page
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .any_input_active()
                || active_tab == NavTab::Favorites && favorites_page.input_mode()
                || active_tab == NavTab::Playlists && playlists.input_active();
            let notification_active = !ctx
                .notifications
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty();
            needs_render |= matches!(
                state,
                lx_core::model::source::PlayerState::Playing
                    | lx_core::model::source::PlayerState::Loading
            ) || input_active
                || notification_active
                || qr_login_page.is_some();
            last_periodic_render = Instant::now();
        }

        // 高频配置修改（音量/播放控制）合并落盘
        ctx.flush_dirty_config();

        // 歌手详情页滚动接近末尾时自动追加下一页
        let spawn_more = {
            let guard = ctx.details_page.lock().unwrap_or_else(|e| e.into_inner());
            guard.as_ref().and_then(|page| {
                if page.wants_more_songs() {
                    page.artist_target()
                        .map(|artist| (artist.clone(), page.songs_page() + 1))
                } else {
                    None
                }
            })
        };
        if let Some((artist, next_page)) = spawn_more {
            if let Some(page) = ctx
                .details_page
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_mut()
            {
                page.mark_loading_more();
            }
            let manager = Arc::clone(&ctx.source_manager);
            let details = Arc::clone(&ctx.details_page);
            rt.spawn(async move {
                let albums = manager
                    .artist_albums(&artist, next_page, 100)
                    .await
                    .unwrap_or_default();
                let songs = tokio::time::timeout(
                    Duration::from_secs(15),
                    manager.artist_songs(&artist, next_page, 100),
                )
                .await;
                let mut guard = details.lock().unwrap_or_else(|e| e.into_inner());
                let Some(page) = guard.as_mut() else {
                    return;
                };
                // 用户可能已切换到其他歌手/专辑，追加前校验目标
                if page
                    .artist_target()
                    .map(|a| a.name != artist.name)
                    .unwrap_or(true)
                {
                    return;
                }
                match songs {
                    Ok(Ok(result)) => page.append_page(albums, result.items, result.has_more),
                    Ok(Err(error)) => {
                        page.set_no_more_songs();
                        tracing::warn!("加载歌手歌曲下一页失败: {error}");
                    }
                    Err(_) => {
                        page.set_no_more_songs();
                        tracing::warn!("加载歌手歌曲下一页超时");
                    }
                }
            });
        }

        // 封面的解码与编码在后台线程进行，完成后才有内容可以绘制
        needs_render |= main_page.poll_cover();

        // 在读取下一个事件前先补画上一轮状态。这样即使 key repeat 每轮都
        // 触发 continue，界面也不会被连续输入饿死。
        if needs_render {
            draw_app(
                terminal,
                &ctx,
                active_tab,
                &search_page,
                &settings_page,
                &sources_page,
                &mut main_page,
                &mut leaderboard,
                &mut playlists,
                &mut favorites_page,
                &mut history_state,
                &mut local_state,
                &mut data_cache.local,
                &mut data_cache.history,
                &mut data_cache.favorites,
                &history_filter,
                &local_filter,
                &mut ui_areas,
                &confirm_delete,
                &local_diagnostics,
                &song_menu,
                &qr_login_page,
                &mut help_page,
                &mut downloads_panel,
            )?;
            needs_render = false;
        }

        // === 2. 事件驱动：轮询终端事件 ===
        // 轮询超时不能长于一帧：50ms 会让实际刷新率钳在 ~20fps，
        // max_fps 配置形同虚设。
        let terminal_event =
            if event::poll(render_interval.min(Duration::from_millis(50))).unwrap_or(false) {
                event::read().ok()
            } else {
                None
            };
        if let Some(Event::Key(key)) = terminal_event.as_ref()
            && key.kind == KeyEventKind::Press
        {
            let key = *key;
            // 1a. 侧边栏全局快捷键（1-0）—— 输入模式下跳过
            let settings_input_mode = active_tab == NavTab::Settings
                && settings_page
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .any_input_active();
            let search_input_mode = active_tab == NavTab::Search
                && search_page
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .input_mode;
            let favorites_input_mode =
                active_tab == NavTab::Favorites && favorites_page.input_mode();
            let playlists_input_mode = active_tab == NavTab::Playlists && playlists.input_active();
            let local_input_mode = active_tab == NavTab::LocalMusic && local_filter.is_active();
            let history_input_mode = active_tab == NavTab::History && history_filter.is_active();
            let text_input_active = settings_input_mode
                || search_input_mode
                || favorites_input_mode
                || playlists_input_mode
                || local_input_mode
                || history_input_mode;

            if let Some(ref page) = qr_login_page {
                let action = page
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .handle_input(key, &kb_resolver);
                match action {
                    AppAction::QrLoginSuccess(source) => {
                        let _ = action_tx.send(AppAction::QrLoginSuccess(source));
                    }
                    AppAction::GoBack => {
                        if let Some(task) = qr_generate_task.take() {
                            task.abort();
                        }
                        if let Some(task) = qr_poll_task.take() {
                            task.abort();
                        }
                        qr_login_page = None;
                    }
                    _ => {}
                }
                needs_render = true;
                continue;
            }

            if confirm_delete.is_some() {
                match delete_confirmation_action(&key) {
                    DeleteConfirmationAction::Confirm => {
                        let confirmation = confirm_delete.take().unwrap();
                        match std::fs::remove_file(&confirmation.path) {
                            Ok(()) => {
                                let local_source = ctx.source_manager.local_source();
                                local_source.remove_by_path(&confirmation.path);
                                let custom_playlist_cleanup = ctx
                                    .storage
                                    .remove_local_path_from_custom_playlists(&confirmation.path);
                                if custom_playlist_cleanup.is_ok() {
                                    let summaries = ctx.storage.custom_playlist_summaries();
                                    playlists
                                        .apply_local_file_removal(&confirmation.path, &summaries);
                                }
                                let remaining = local_source.all_songs().len();
                                local_state.selected =
                                    local_state.selected.min(remaining.saturating_sub(1));
                                local_state.scroll =
                                    local_state.scroll.min(remaining.saturating_sub(1));

                                ctx.notify(Notification::success(format!(
                                    "已删除本地文件: {}",
                                    confirmation.name
                                )));
                                if let Err(error) = custom_playlist_cleanup {
                                    ctx.notify(Notification::warning(format!(
                                        "文件已删除，但清理自建歌单失败: {error}"
                                    )));
                                }
                                let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
                                let paths = config.local_music.paths.clone();
                                let max_depth = config.local_music.max_depth;
                                drop(config);
                                execute_action(
                                    AppAction::ScanLocalMusic {
                                        paths,
                                        max_depth,
                                        force: true,
                                    },
                                    &ctx,
                                    rt,
                                    &action_tx,
                                    &search_page,
                                    &settings_page,
                                    &search_seq,
                                );
                            }
                            Err(error) => {
                                ctx.notify(Notification::error(format!(
                                    "删除本地文件失败: {}",
                                    error
                                )));
                            }
                        }
                    }
                    DeleteConfirmationAction::Cancel => {
                        confirm_delete = None;
                    }
                    DeleteConfirmationAction::Ignore => {}
                }
                needs_render = true;
                continue;
            }

            if let Some(menu) = song_menu.as_mut() {
                let outcome = menu.handle_key(
                    &key,
                    &kb_resolver,
                    nav_page_scope(active_tab),
                    ui_areas.content,
                );
                match outcome {
                    MenuOutcome::None => {}
                    MenuOutcome::Close => {
                        song_menu = None;
                    }
                    MenuOutcome::Action(action) => {
                        let menu = song_menu.take().unwrap();
                        execute_song_menu_action(
                            action,
                            &menu,
                            &mut main_page,
                            &ctx,
                            rt,
                            &action_tx,
                            &search_page,
                            &settings_page,
                            &search_seq,
                            &mut favorites_page,
                            &mut playlists,
                            &mut history_state,
                            &mut local_state,
                            &mut confirm_delete,
                        );
                    }
                }
                needs_render = true;
                continue;
            }

            // 快捷键说明浮层：? / \ 开关；打开时独占按键
            if help_page.is_some() {
                let keep = help_page
                    .as_mut()
                    .expect("help page checked above")
                    .handle_input(&key);
                if !keep {
                    help_page = None;
                }
                needs_render = true;
                continue;
            }
            if !text_input_active
                && matches!(
                    (key.modifiers, key.code),
                    (KeyModifiers::NONE, KeyCode::Char('?') | KeyCode::Char('\\'))
                )
            {
                help_page = Some(pages::help::HelpPage::from_config(
                    &ctx.config
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .keybindings,
                ));
                needs_render = true;
                continue;
            }

            // 下载面板：打开时独占按键（Esc / Ctrl+o 关闭，c 取消，x 清理）
            if downloads_panel.is_open() {
                let tasks = ctx.downloads.snapshot();
                if downloads_panel.handle_key(&key, &ctx, &tasks)
                    == pages::downloads::PanelOutcome::Close
                {
                    downloads_panel.close();
                    // 关闭浮层后整屏重画，清掉面板压过的封面/边框残留。
                    terminal.clear()?;
                }
                needs_render = true;
                continue;
            }

            // 歌手/专辑详情浮层：打开时独占按键
            if ctx
                .details_page
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some()
            {
                let action = ctx
                    .details_page
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_mut()
                    .expect("details page checked above")
                    .handle_input(&key, &ctx, &kb_resolver);
                match action {
                    AppAction::GoBack => {
                        *ctx.details_page.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    }
                    AppAction::None => {}
                    action => execute_action(
                        action,
                        &ctx,
                        rt,
                        &action_tx,
                        &search_page,
                        &settings_page,
                        &search_seq,
                    ),
                }
                needs_render = true;
                continue;
            }

            // 本地库诊断浮层：打开时独占 Esc/i，且不被侧边栏切页关闭
            if let Some(kind) = local_diagnostics {
                match (key.modifiers, key.code) {
                    (KeyModifiers::NONE, KeyCode::Esc) => {
                        local_diagnostics = None;
                    }
                    (KeyModifiers::NONE, KeyCode::Char('i' | 'I')) => {
                        local_diagnostics = Some(match kind {
                            LocalDiagnosticsKind::Corrupt => LocalDiagnosticsKind::Missing,
                            LocalDiagnosticsKind::Missing => LocalDiagnosticsKind::Duplicates,
                            LocalDiagnosticsKind::Duplicates => LocalDiagnosticsKind::Corrupt,
                        });
                    }
                    _ => {
                        // 浮层打开期间吞掉其他按键，防止误触底层页面
                    }
                }
                needs_render = true;
                continue;
            }

            // 设置页独占自己的选项键；数字键 1-0 则始终留给侧边栏。
            // 先判断页面归属，再分发全局快捷键。
            let settings_owns_key = active_tab == NavTab::Settings
                && !text_input_active
                && settings_page
                    .lock()
                    .unwrap()
                    .consumes_key(&key, &kb_resolver);
            let sources_owns_key = active_tab == NavTab::Sources
                && !text_input_active
                && sources_page
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .consumes_key(&key);

            if !text_input_active
                && !settings_owns_key
                && !sources_owns_key
                && let Some(tab) = pages::sidebar::handle_input(&key)
            {
                active_tab = tab;
                needs_render = true;
                continue;
            }

            // 1b. 全局快捷键（查表模式）；页面专属动作在下方处理
            // 设置页的选项键覆盖了大半个字母表，与全局键位必然冲突，交由页面独占
            if !settings_owns_key
                && !sources_owns_key
                && let Some(action) = kb_resolver.resolve_global(&key)
            {
                match action {
                    Action::GlobalQuit if !text_input_active => {
                        tracing::info!("quit requested");
                        if let Err(error) = ctx.persist_playback_session() {
                            tracing::warn!("save playback session failed: {error}");
                        }
                        ctx.stop_player();
                        return Ok(());
                    }
                    Action::GlobalPlayPause if !text_input_active => {
                        toggle_or_start_current(
                            &ctx,
                            rt,
                            &action_tx,
                            &search_page,
                            &settings_page,
                            &search_seq,
                        );
                        needs_render = true;
                        continue;
                    }
                    Action::GlobalNextTrack if !text_input_active => {
                        if let Some((songs, index)) = ctx.playlist.next_manual_entry_arc() {
                            execute_action(
                                AppAction::PlayFromQueue { songs, index },
                                &ctx,
                                rt,
                                &action_tx,
                                &search_page,
                                &settings_page,
                                &search_seq,
                            );
                        }
                        needs_render = true;
                        continue;
                    }
                    Action::GlobalPrevTrack if !text_input_active => {
                        if let Some((songs, index)) = ctx.playlist.prev_manual_entry_arc() {
                            execute_action(
                                AppAction::PlayFromQueue { songs, index },
                                &ctx,
                                rt,
                                &action_tx,
                                &search_page,
                                &settings_page,
                                &search_seq,
                            );
                        }
                        needs_render = true;
                        continue;
                    }
                    Action::GlobalCycleMode if !text_input_active => {
                        let mode = ctx.playlist.cycle_mode();
                        let save_result = {
                            let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                            config.player.play_mode = mode.as_config().to_string();
                            crate::config::loader::save(&config, &ctx.config_path)
                        };
                        let notification = match save_result {
                            Ok(()) => Notification::success(format!("播放模式: {}", mode.label())),
                            Err(error) => Notification::error(format!(
                                "播放模式已切换，但保存失败: {}",
                                error
                            )),
                        };
                        ctx.notify(notification);
                        needs_render = true;
                        continue;
                    }
                    Action::GlobalSeekForward
                        if !text_input_active && active_tab == NavTab::Main =>
                    {
                        let pos = *ctx.position.borrow();
                        ctx.seek(pos + Duration::from_secs(5));
                        needs_render = true;
                        continue;
                    }
                    Action::GlobalSeekBackward
                        if !text_input_active && active_tab == NavTab::Main =>
                    {
                        let pos = *ctx.position.borrow();
                        if pos > Duration::from_secs(5) {
                            ctx.seek(pos - Duration::from_secs(5));
                        } else {
                            ctx.seek(Duration::ZERO);
                        }
                        needs_render = true;
                        continue;
                    }
                    Action::GlobalVolumeUp if !text_input_active => {
                        persist_volume(&ctx, ctx.player.volume().saturating_add(5));
                        needs_render = true;
                        continue;
                    }
                    Action::GlobalVolumeDown if !text_input_active => {
                        persist_volume(&ctx, ctx.player.volume().saturating_sub(5));
                        needs_render = true;
                        continue;
                    }
                    Action::GlobalNextTab if !text_input_active => {
                        active_tab = match active_tab {
                            NavTab::Main => NavTab::Search,
                            NavTab::Search => NavTab::Leaderboard,
                            NavTab::Leaderboard => NavTab::Playlists,
                            NavTab::Playlists => NavTab::Favorites,
                            NavTab::Favorites => NavTab::History,
                            NavTab::History => NavTab::LocalMusic,
                            NavTab::LocalMusic => NavTab::Downloads,
                            NavTab::Downloads => NavTab::Sources,
                            NavTab::Sources => NavTab::Settings,
                            NavTab::Settings => NavTab::Main,
                        };
                        needs_render = true;
                        continue;
                    }
                    Action::GlobalPrevTab if !text_input_active => {
                        active_tab = match active_tab {
                            NavTab::Main => NavTab::Settings,
                            NavTab::Search => NavTab::Main,
                            NavTab::Leaderboard => NavTab::Search,
                            NavTab::Playlists => NavTab::Leaderboard,
                            NavTab::Favorites => NavTab::Playlists,
                            NavTab::History => NavTab::Favorites,
                            NavTab::LocalMusic => NavTab::History,
                            NavTab::Downloads => NavTab::LocalMusic,
                            NavTab::Sources => NavTab::Downloads,
                            NavTab::Settings => NavTab::Sources,
                        };
                        needs_render = true;
                        continue;
                    }
                    Action::GlobalGoToMain
                        if !matches!(
                            (key.modifiers, key.code),
                            (KeyModifiers::NONE, KeyCode::Esc)
                        ) && should_go_to_main(
                            active_tab,
                            text_input_active,
                            playlists.selected_playlist.is_some(),
                            leaderboard.selected_board.is_some(),
                        ) =>
                    {
                        active_tab = NavTab::Main;
                        needs_render = true;
                        continue;
                    }
                    Action::GlobalToggleFavorite if !text_input_active => {
                        if let Some(song) = ctx
                            .current_song
                            .read()
                            .unwrap_or_else(|e| e.into_inner())
                            .as_ref()
                        {
                            if ctx.storage.is_favorite(song) {
                                ctx.storage.remove_favorite(song);
                                let _ = action_tx.send(AppAction::ShowNotification(
                                    Notification::success("已取消收藏"),
                                ));
                            } else {
                                ctx.storage.add_favorite(song);
                                let _ = action_tx.send(AppAction::ShowNotification(
                                    Notification::success("已添加收藏"),
                                ));
                            }
                        }
                        needs_render = true;
                        continue;
                    }
                    Action::GlobalRedraw if !text_input_active => {
                        last_cover_redraw = Instant::now();
                        retransmit_cover(terminal, &mut main_page)?;
                        needs_render = true;
                        continue;
                    }
                    Action::GlobalDownloadCurrent if !text_input_active => {
                        let song = ctx
                            .current_song
                            .read()
                            .unwrap_or_else(|e| e.into_inner())
                            .clone();
                        match song {
                            Some(song) => execute_action(
                                AppAction::DownloadSong(Box::new(song)),
                                &ctx,
                                rt,
                                &action_tx,
                                &search_page,
                                &settings_page,
                                &search_seq,
                            ),
                            None => ctx.notify(Notification::info("当前没有正在播放的歌曲")),
                        }
                        needs_render = true;
                        continue;
                    }
                    Action::GlobalDownloadsPanel if !text_input_active => {
                        let count = ctx.downloads.snapshot().len();
                        downloads_panel.toggle(count);
                        // 浮层覆盖在内容区之上，且可能压住图形协议绘制的封面。
                        // 开关时整屏重画一次，避免关闭后残留边框或旧内容。
                        terminal.clear()?;
                        needs_render = true;
                        continue;
                    }
                    _ => {}
                }
            }

            // Fallback：不在自定义配置中的全局键位别名（保持向后兼容）
            match (key.modifiers, key.code) {
                (KeyModifiers::SHIFT, KeyCode::Char('>')) if !text_input_active => {
                    if let Some((songs, index)) = ctx.playlist.next_manual_entry_arc() {
                        execute_action(
                            AppAction::PlayFromQueue { songs, index },
                            &ctx,
                            rt,
                            &action_tx,
                            &search_page,
                            &settings_page,
                            &search_seq,
                        );
                    }
                    needs_render = true;
                    continue;
                }
                (KeyModifiers::SHIFT, KeyCode::Char('<')) if !text_input_active => {
                    if let Some((songs, index)) = ctx.playlist.prev_manual_entry_arc() {
                        execute_action(
                            AppAction::PlayFromQueue { songs, index },
                            &ctx,
                            rt,
                            &action_tx,
                            &search_page,
                            &settings_page,
                            &search_seq,
                        );
                    }
                    needs_render = true;
                    continue;
                }
                (KeyModifiers::NONE, KeyCode::Right)
                    if !text_input_active && active_tab == NavTab::Main =>
                {
                    let pos = *ctx.position.borrow();
                    ctx.seek(pos + Duration::from_secs(5));
                    needs_render = true;
                    continue;
                }
                (KeyModifiers::NONE, KeyCode::Left)
                    if !text_input_active && active_tab == NavTab::Main =>
                {
                    let pos = *ctx.position.borrow();
                    if pos > Duration::from_secs(5) {
                        ctx.seek(pos - Duration::from_secs(5));
                    } else {
                        ctx.seek(Duration::ZERO);
                    }
                    needs_render = true;
                    continue;
                }
                (KeyModifiers::NONE, KeyCode::Up) if active_tab == NavTab::Main => {
                    persist_volume(&ctx, ctx.player.volume().saturating_add(5));
                    needs_render = true;
                    continue;
                }
                (KeyModifiers::NONE, KeyCode::Down) if active_tab == NavTab::Main => {
                    persist_volume(&ctx, ctx.player.volume().saturating_sub(5));
                    needs_render = true;
                    continue;
                }
                _ => {}
            }

            // 1c. 路由到当前页面
            match active_tab {
                NavTab::Search => {
                    let action = {
                        let mut sp = search_page.lock().unwrap_or_else(|e| e.into_inner());
                        sp.handle_input(key, &kb_resolver)
                    };
                    if matches!(action, AppAction::GoBack) {
                        active_tab = NavTab::Main;
                        needs_render = true;
                        continue;
                    }
                    execute_action(
                        action,
                        &ctx,
                        rt,
                        &action_tx,
                        &search_page,
                        &settings_page,
                        &search_seq,
                    );
                }
                NavTab::Main => {
                    let action = main_page.handle_input(&key, &ctx, &kb_resolver);
                    execute_action(
                        action,
                        &ctx,
                        rt,
                        &action_tx,
                        &search_page,
                        &settings_page,
                        &search_seq,
                    );
                }
                NavTab::Leaderboard => {
                    let action = leaderboard.handle_input(&key, &ctx, &kb_resolver);
                    execute_action(
                        action,
                        &ctx,
                        rt,
                        &action_tx,
                        &search_page,
                        &settings_page,
                        &search_seq,
                    );
                }
                NavTab::Playlists => {
                    let action = playlists.handle_input(&key, &ctx, &kb_resolver);
                    if matches!(action, AppAction::GoBack) {
                        active_tab = NavTab::Main;
                        needs_render = true;
                        continue;
                    }
                    execute_action(
                        action,
                        &ctx,
                        rt,
                        &action_tx,
                        &search_page,
                        &settings_page,
                        &search_seq,
                    );
                }
                NavTab::Favorites => {
                    let action = favorites_page.handle_input(
                        &key,
                        &ctx,
                        &kb_resolver,
                        &mut data_cache.favorites,
                    );
                    if matches!(action, AppAction::GoBack) {
                        active_tab = NavTab::Main;
                        needs_render = true;
                        continue;
                    }
                    execute_action(
                        action,
                        &ctx,
                        rt,
                        &action_tx,
                        &search_page,
                        &settings_page,
                        &search_seq,
                    );
                }
                NavTab::History => {
                    if history_filter.handle_input(&key) {
                        if !history_filter.is_active() {
                            history_state.reset_position();
                        }
                        needs_render = true;
                        continue;
                    }

                    if let Some(Action::HistoryFilter) = kb_resolver.resolve_page("history", &key) {
                        history_filter.activate();
                        needs_render = true;
                        continue;
                    }

                    let action = pages::history::handle_input(
                        &key,
                        &ctx,
                        &mut history_state,
                        history_filter.query(),
                        &kb_resolver,
                        &mut data_cache.history,
                    );
                    // 保持在历史页面，不强制切换到主页
                    execute_action(
                        action,
                        &ctx,
                        rt,
                        &action_tx,
                        &search_page,
                        &settings_page,
                        &search_seq,
                    );
                }
                NavTab::Downloads => {
                    let tasks = ctx.downloads.snapshot();
                    if matches!(
                        downloads_panel.handle_key(&key, &ctx, &tasks),
                        pages::downloads::PanelOutcome::Close
                    ) {
                        active_tab = NavTab::Main;
                    }
                    needs_render = true;
                    continue;
                }
                NavTab::Sources => {
                    let action = {
                        let mut sp = sources_page.lock().unwrap_or_else(|e| e.into_inner());
                        sp.handle_key(key, &ctx)
                    };
                    if matches!(action, AppAction::GoBack) {
                        active_tab = NavTab::Main;
                        needs_render = true;
                        continue;
                    }
                    if matches!(
                        action,
                        AppAction::QrLogin(_)
                            | AppAction::QrLogout(_)
                            | AppAction::QrLoginSuccess(_)
                    ) {
                        let _ = action_tx.send(action);
                    } else {
                        execute_action(
                            action,
                            &ctx,
                            rt,
                            &action_tx,
                            &search_page,
                            &settings_page,
                            &search_seq,
                        );
                    }
                }
                NavTab::Settings => {
                    let action = {
                        let mut sp = settings_page.lock().unwrap_or_else(|e| e.into_inner());
                        sp.handle_input(key, &ctx, &kb_resolver)
                    };
                    if matches!(
                        action,
                        AppAction::QrLogin(_)
                            | AppAction::QrLogout(_)
                            | AppAction::QrLoginSuccess(_)
                    ) {
                        let _ = action_tx.send(action);
                    } else {
                        if matches!(key.code, KeyCode::Char('g' | 'w' | 'z' | 'K')) {
                            let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
                            search_page
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .set_preferences(
                                    config.ui.aggregate_search,
                                    config.source.default,
                                    config.ui.wrap_navigation,
                                    config.ui.scroll_amount,
                                    &config.source.enabled,
                                );
                        }
                        execute_action(
                            action,
                            &ctx,
                            rt,
                            &action_tx,
                            &search_page,
                            &settings_page,
                            &search_seq,
                        );
                    }
                }
                NavTab::LocalMusic => {
                    // 1. 过滤输入模式优先消耗按键
                    if local_filter.handle_input(&key) {
                        if !local_filter.is_active() {
                            local_state.reset_position();
                        }
                        needs_render = true;
                        continue;
                    }

                    // 2. 计算排序+过滤后的歌曲视图（下标映射，不深拷贝歌曲）
                    let all_songs = pages::local_music::sorted_local_songs(
                        &ctx,
                        &local_state,
                        &mut data_cache.local,
                    );
                    let songs =
                        pages::local_music::LocalSongView::build(all_songs, local_filter.query());

                    if let Some(action) = kb_resolver.resolve_page("local", &key) {
                        match action {
                            Action::ListCycleSort => {
                                let mode = local_state.cycle();
                                ctx.notify(Notification::info(format!(
                                    "本地排序: {}",
                                    mode.label(SortTarget::Local)
                                )));
                            }
                            Action::LocalRescan => {
                                let paths = ctx
                                    .config
                                    .read()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .local_music
                                    .paths
                                    .clone();
                                let max_depth = ctx
                                    .config
                                    .read()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .local_music
                                    .max_depth;
                                execute_action(
                                    AppAction::ScanLocalMusic {
                                        paths,
                                        max_depth,
                                        force: true,
                                    },
                                    &ctx,
                                    rt,
                                    &action_tx,
                                    &search_page,
                                    &settings_page,
                                    &search_seq,
                                );
                                local_state.reset_position();
                                local_filter.reset();
                            }
                            Action::LocalFilter => {
                                local_filter.activate();
                            }
                            Action::ListSelectUp => {
                                local_state.selected = previous_list_index(
                                    local_state.selected,
                                    songs.len(),
                                    ctx.config
                                        .read()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .ui
                                        .wrap_navigation,
                                );
                            }
                            Action::ListSelectDown => {
                                local_state.selected = next_list_index(
                                    local_state.selected,
                                    songs.len(),
                                    ctx.config
                                        .read()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .ui
                                        .wrap_navigation,
                                );
                            }
                            Action::ListSelectFirst => {
                                local_state.selected = 0;
                            }
                            Action::ListSelectLast => {
                                local_state.selected = songs.len().saturating_sub(1);
                            }
                            Action::ListPageUp => {
                                local_state.selected = local_state.selected.saturating_sub(10);
                            }
                            Action::ListPageDown => {
                                local_state.selected =
                                    (local_state.selected + 10).min(songs.len().saturating_sub(1));
                            }
                            Action::ListAddToQueue => {
                                if let Some(song) = songs.get(local_state.selected).cloned() {
                                    execute_action(
                                        AppAction::AddToQueue {
                                            song: Box::new(song),
                                            position: InsertPosition::End,
                                        },
                                        &ctx,
                                        rt,
                                        &action_tx,
                                        &search_page,
                                        &settings_page,
                                        &search_seq,
                                    );
                                }
                            }
                            Action::ListAddToQueueNext => {
                                if let Some(song) = songs.get(local_state.selected).cloned() {
                                    execute_action(
                                        AppAction::AddToQueue {
                                            song: Box::new(song),
                                            position: InsertPosition::Next,
                                        },
                                        &ctx,
                                        rt,
                                        &action_tx,
                                        &search_page,
                                        &settings_page,
                                        &search_seq,
                                    );
                                }
                            }
                            Action::ListToggleFavorite => {
                                if let Some(song) = songs.get(local_state.selected).cloned() {
                                    execute_action(
                                        AppAction::ToggleFavoriteSong(Box::new(song)),
                                        &ctx,
                                        rt,
                                        &action_tx,
                                        &search_page,
                                        &settings_page,
                                        &search_seq,
                                    );
                                }
                            }
                            Action::LocalDelete => {
                                if let Some(song) = songs.get(local_state.selected) {
                                    if let Some(path) = &song.file_path {
                                        confirm_delete = Some(LocalDeleteConfirmation {
                                            name: song.name.clone(),
                                            path: path.clone(),
                                        });
                                    } else {
                                        ctx.notify(Notification::error(
                                            "无法删除：没有本地文件路径",
                                        ));
                                    }
                                }
                            }
                            Action::ListActivate
                                if !songs.is_empty() && local_state.selected < songs.len() =>
                            {
                                execute_action(
                                    AppAction::PlaySong {
                                        songs: songs.to_queue(),
                                        index: local_state.selected,
                                    },
                                    &ctx,
                                    rt,
                                    &action_tx,
                                    &search_page,
                                    &settings_page,
                                    &search_seq,
                                );
                            }
                            _ => {}
                        }
                    } else {
                        match (key.modifiers, key.code) {
                            (KeyModifiers::NONE, KeyCode::Char('i' | 'I')) => {
                                let source = ctx.source_manager.local_source();
                                local_diagnostics = Some(if !source.corrupt_files().is_empty() {
                                    LocalDiagnosticsKind::Corrupt
                                } else if !source.missing_files().is_empty() {
                                    LocalDiagnosticsKind::Missing
                                } else {
                                    LocalDiagnosticsKind::Duplicates
                                });
                                needs_render = true;
                            }
                            (KeyModifiers::NONE, KeyCode::Char('s')) => {
                                let mode = local_state.cycle();
                                ctx.notify(Notification::info(format!(
                                    "本地排序: {}",
                                    mode.label(SortTarget::Local)
                                )));
                            }
                            (KeyModifiers::NONE, KeyCode::Char('r')) => {
                                let paths = ctx
                                    .config
                                    .read()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .local_music
                                    .paths
                                    .clone();
                                let max_depth = ctx
                                    .config
                                    .read()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .local_music
                                    .max_depth;
                                execute_action(
                                    AppAction::ScanLocalMusic {
                                        paths,
                                        max_depth,
                                        force: true,
                                    },
                                    &ctx,
                                    rt,
                                    &action_tx,
                                    &search_page,
                                    &settings_page,
                                    &search_seq,
                                );
                                local_state.reset_position();
                                local_filter.reset();
                            }
                            (KeyModifiers::NONE, KeyCode::Up) => {
                                local_state.selected = previous_list_index(
                                    local_state.selected,
                                    songs.len(),
                                    ctx.config
                                        .read()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .ui
                                        .wrap_navigation,
                                );
                            }
                            (KeyModifiers::NONE, KeyCode::Down) => {
                                local_state.selected = next_list_index(
                                    local_state.selected,
                                    songs.len(),
                                    ctx.config
                                        .read()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .ui
                                        .wrap_navigation,
                                );
                            }
                            (KeyModifiers::NONE, KeyCode::Home)
                            | (KeyModifiers::NONE, KeyCode::Char('g')) => {
                                local_state.selected = 0;
                            }
                            (KeyModifiers::NONE, KeyCode::End)
                            | (KeyModifiers::NONE, KeyCode::Char('G'))
                            | (KeyModifiers::SHIFT, KeyCode::Char('G')) => {
                                local_state.selected = songs.len().saturating_sub(1);
                            }
                            (KeyModifiers::CONTROL, KeyCode::Char('u'))
                            | (KeyModifiers::NONE, KeyCode::PageUp) => {
                                local_state.selected = local_state.selected.saturating_sub(10);
                            }
                            (KeyModifiers::CONTROL, KeyCode::Char('d'))
                            | (KeyModifiers::NONE, KeyCode::PageDown) => {
                                local_state.selected =
                                    (local_state.selected + 10).min(songs.len().saturating_sub(1));
                            }
                            _ if pages::is_song_activation_key(&key) => {
                                if !songs.is_empty() && local_state.selected < songs.len() {
                                    execute_action(
                                        AppAction::PlaySong {
                                            songs: songs.to_queue(),
                                            index: local_state.selected,
                                        },
                                        &ctx,
                                        rt,
                                        &action_tx,
                                        &search_page,
                                        &settings_page,
                                        &search_seq,
                                    );
                                }
                            }
                            (KeyModifiers::NONE, KeyCode::Char('a')) => {
                                if let Some(song) = songs.get(local_state.selected).cloned() {
                                    execute_action(
                                        AppAction::AddToQueue {
                                            song: Box::new(song),
                                            position: InsertPosition::End,
                                        },
                                        &ctx,
                                        rt,
                                        &action_tx,
                                        &search_page,
                                        &settings_page,
                                        &search_seq,
                                    );
                                }
                            }
                            (KeyModifiers::NONE, KeyCode::Char('A'))
                            | (KeyModifiers::SHIFT, KeyCode::Char('A')) => {
                                if let Some(song) = songs.get(local_state.selected).cloned() {
                                    execute_action(
                                        AppAction::AddToQueue {
                                            song: Box::new(song),
                                            position: InsertPosition::Next,
                                        },
                                        &ctx,
                                        rt,
                                        &action_tx,
                                        &search_page,
                                        &settings_page,
                                        &search_seq,
                                    );
                                }
                            }
                            (KeyModifiers::NONE, KeyCode::Char('f')) => {
                                if let Some(song) = songs.get(local_state.selected).cloned() {
                                    execute_action(
                                        AppAction::ToggleFavoriteSong(Box::new(song)),
                                        &ctx,
                                        rt,
                                        &action_tx,
                                        &search_page,
                                        &settings_page,
                                        &search_seq,
                                    );
                                }
                            }
                            (KeyModifiers::NONE, KeyCode::Char('d'))
                            | (KeyModifiers::NONE, KeyCode::Delete) => {
                                if let Some(song) = songs.get(local_state.selected) {
                                    if let Some(path) = &song.file_path {
                                        confirm_delete = Some(LocalDeleteConfirmation {
                                            name: song.name.clone(),
                                            path: path.clone(),
                                        });
                                    } else {
                                        ctx.notify(Notification::error(
                                            "无法删除：没有本地文件路径",
                                        ));
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            needs_render = true;
        } else if let Some(Event::Mouse(mouse)) = terminal_event.as_ref() {
            if confirm_delete.is_some() || downloads_panel.is_open() {
                needs_render = true;
                continue;
            }
            let mouse = *mouse;

            if let Some(menu) = song_menu.as_mut() {
                let outcome = menu.handle_mouse(mouse, ui_areas.content);
                match outcome {
                    MenuOutcome::None => {}
                    MenuOutcome::Close => {
                        song_menu = None;
                    }
                    MenuOutcome::Action(action) => {
                        let menu = song_menu.take().unwrap();
                        execute_song_menu_action(
                            action,
                            &menu,
                            &mut main_page,
                            &ctx,
                            rt,
                            &action_tx,
                            &search_page,
                            &settings_page,
                            &search_seq,
                            &mut favorites_page,
                            &mut playlists,
                            &mut history_state,
                            &mut local_state,
                            &mut confirm_delete,
                        );
                    }
                }
                needs_render = true;
                continue;
            }

            let activate = click_tracker.is_double_click(mouse);
            if let Some(help) = help_page.as_mut() {
                // 点击浮层外或滚动都会进入处理；返回 false 表示关闭
                let keep = help.handle_mouse(mouse);
                let outside = !keep && mouse.kind == MouseEventKind::Down(MouseButton::Left);
                if outside {
                    help_page = None;
                }
                needs_render = true;
                continue;
            }
            if let Some(page) = ctx
                .details_page
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_mut()
            {
                let action = page.handle_mouse(mouse, ui_areas.content, &ctx, activate);
                if let AppAction::GoBack = action {
                    *ctx.details_page.lock().unwrap_or_else(|e| e.into_inner()) = None;
                }
                needs_render = true;
                continue;
            }

            if active_tab == NavTab::Playlists && playlists.input_active() {
                needs_render = true;
                continue;
            }

            let position = Position::new(mouse.column, mouse.row);
            if ui_areas.notification.contains(position)
                && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            {
                if let Some(url) = components::notification::action_url_at(
                    ui_areas.notification,
                    mouse.column,
                    mouse.row,
                    &ctx,
                ) {
                    open_external_url(&url);
                }
                ctx.dismiss_notification();
            } else if ui_areas.tabs.contains(position)
                && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            {
                if let Some(tab) = pages::sidebar::hit_test(ui_areas.tabs, position) {
                    active_tab = tab;
                    if tab == NavTab::Search {
                        search_page
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .input_mode = true;
                    }
                }
            } else if ui_areas.player_progress.contains(position)
                && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            {
                let duration = *ctx.duration.borrow();
                if let Some(position) = components::player_controls::seek_position(
                    ui_areas.player_progress,
                    mouse.column,
                    duration,
                ) {
                    ctx.seek(position);
                }
            } else if ui_areas.player_controls.contains(position)
                && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            {
                use components::player_controls::ControlHit;
                match components::player_controls::control_at(
                    ui_areas.player_controls,
                    mouse.column,
                ) {
                    Some(ControlHit::Mode) => {
                        let mode = ctx.playlist.cycle_mode();
                        let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                        config.player.play_mode = mode.as_config().to_string();
                        let _ = crate::config::loader::save(&config, &ctx.config_path);
                    }
                    Some(ControlHit::Previous) => {
                        if let Some((songs, index)) = ctx.playlist.prev_manual_entry_arc() {
                            execute_action(
                                AppAction::PlayFromQueue { songs, index },
                                &ctx,
                                rt,
                                &action_tx,
                                &search_page,
                                &settings_page,
                                &search_seq,
                            );
                        }
                    }
                    Some(ControlHit::PlayPause) => {
                        toggle_or_start_current(
                            &ctx,
                            rt,
                            &action_tx,
                            &search_page,
                            &settings_page,
                            &search_seq,
                        );
                    }
                    Some(ControlHit::Next) => {
                        if let Some((songs, index)) = ctx.playlist.next_manual_entry_arc() {
                            execute_action(
                                AppAction::PlayFromQueue { songs, index },
                                &ctx,
                                rt,
                                &action_tx,
                                &search_page,
                                &settings_page,
                                &search_seq,
                            );
                        }
                    }
                    Some(ControlHit::Volume) => {
                        if let Some(relative) = components::player_controls::volume_offset(
                            ui_areas.player_controls,
                            mouse.column,
                        ) {
                            let volume_width =
                                components::player_controls::volume_width(ui_areas.player_controls);
                            let volume = if volume_width <= 1 {
                                100
                            } else {
                                (u32::from(relative).saturating_mul(100)
                                    / u32::from(volume_width - 1))
                                .min(100)
                            };
                            persist_volume(&ctx, volume);
                        }
                    }
                    None => {}
                }
            } else if ui_areas.content.contains(position) {
                if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Right)) {
                    let target = match active_tab {
                        NavTab::Main => main_page
                            .context_song_at(mouse, ui_areas.content, &ctx)
                            .map(|target| (target, SongMenuKind::Queue, None)),
                        NavTab::Search => search_page
                            .lock()
                            .unwrap()
                            .context_song_at(mouse, ui_areas.content)
                            .map(|target| (target, SongMenuKind::Standard, None)),
                        NavTab::Leaderboard => leaderboard
                            .context_song_at(mouse, ui_areas.content)
                            .map(|target| (target, SongMenuKind::Standard, None)),
                        NavTab::Playlists => playlists
                            .context_song_at(mouse, ui_areas.content)
                            .map(|target| (target, SongMenuKind::Standard, None)),
                        NavTab::Favorites => favorites_page
                            .context_song_at(
                                mouse,
                                ui_areas.content,
                                &ctx,
                                &mut data_cache.favorites,
                            )
                            .map(|target| {
                                (
                                    target,
                                    SongMenuKind::Standard,
                                    Some((SortTarget::Favorites, favorites_page.sort_mode())),
                                )
                            }),
                        NavTab::History => pages::history::context_song_at(
                            mouse,
                            ui_areas.content,
                            &ctx,
                            &mut history_state,
                            history_filter.query(),
                            &mut data_cache.history,
                        )
                        .map(|target| {
                            (
                                target,
                                SongMenuKind::History,
                                Some((SortTarget::History, history_state.mode)),
                            )
                        }),
                        NavTab::Downloads | NavTab::Sources | NavTab::Settings => None,
                        NavTab::LocalMusic => pages::local_music::context_song_at(
                            mouse,
                            ui_areas.content,
                            &ctx,
                            &mut local_state,
                            &mut data_cache.local,
                            local_filter.is_active() || !local_filter.query().is_empty(),
                            local_filter.query(),
                        )
                        .map(|target| {
                            (
                                target,
                                SongMenuKind::Local,
                                Some((SortTarget::Local, local_state.mode)),
                            )
                        }),
                    };
                    if let Some(((songs, index), kind, sort)) = target {
                        let is_favorite = ctx.storage.is_favorite(&songs[index]);
                        let custom_playlists = ctx.storage.custom_playlist_choices();
                        let current_custom_playlist = (active_tab == NavTab::Playlists)
                            .then(|| playlists.current_custom_playlist_id())
                            .flatten();
                        song_menu = SongContextMenu::new(
                            position,
                            songs,
                            index,
                            kind,
                            is_favorite,
                            SongContextMenuOptions {
                                sort,
                                custom_playlists,
                                current_custom_playlist,
                                playback: Some(playback_menu_state(&ctx)),
                            },
                        );
                        needs_render = true;
                        continue;
                    }
                }

                let action = match active_tab {
                    NavTab::Main => main_page.handle_mouse(mouse, ui_areas.content, &ctx, activate),
                    NavTab::Search => {
                        search_page
                            .lock()
                            .unwrap()
                            .handle_mouse(mouse, ui_areas.content, activate)
                    }
                    NavTab::Leaderboard => {
                        leaderboard.handle_mouse(mouse, ui_areas.content, activate, &ctx)
                    }
                    NavTab::Playlists => {
                        playlists.handle_mouse(mouse, ui_areas.content, activate, &ctx)
                    }
                    NavTab::Favorites => favorites_page.handle_mouse(
                        mouse,
                        ui_areas.content,
                        &ctx,
                        &mut data_cache.favorites,
                        activate,
                    ),
                    NavTab::History => pages::history::handle_mouse(
                        mouse,
                        ui_areas.content,
                        &ctx,
                        &mut history_state,
                        history_filter.query(),
                        &mut data_cache.history,
                        activate,
                    ),
                    NavTab::Sources => sources_page
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .handle_mouse(mouse, ui_areas.content, &ctx),
                    NavTab::Settings => settings_page
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .handle_mouse(mouse, ui_areas.content, &ctx, &kb_resolver),
                    NavTab::Downloads => {
                        let tasks = ctx.downloads.snapshot();
                        downloads_panel.handle_mouse(mouse, ui_areas.content, &tasks);
                        AppAction::None
                    }
                    NavTab::LocalMusic => pages::local_music::handle_mouse(
                        mouse,
                        ui_areas.content,
                        &ctx,
                        &mut local_state,
                        &mut data_cache.local,
                        local_filter.is_active() || !local_filter.query().is_empty(),
                        local_filter.query(),
                        activate,
                    ),
                };

                execute_action(
                    action,
                    &ctx,
                    rt,
                    &action_tx,
                    &search_page,
                    &settings_page,
                    &search_seq,
                );
            }
            needs_render = true;
        } else if matches!(terminal_event, Some(Event::Resize(_, _))) {
            main_page.refresh_cover_font_size();
            needs_render = true;
        }

        if active_tab == NavTab::Leaderboard {
            maybe_spawn_leaderboard_load(
                &mut leaderboard,
                &mut leaderboard_request_id,
                Arc::clone(&ctx.source_manager),
                leaderboard_tx.clone(),
                rt,
            );
        }
        if active_tab == NavTab::Playlists {
            playlists.sync_saved_playlists(&ctx);
            maybe_spawn_playlist_load(
                &mut playlists,
                &mut playlist_request_id,
                Arc::clone(&ctx.source_manager),
                playlist_tx.clone(),
                rt,
            );
        }

        // === 3. 当前事件未提前 continue 时立即渲染 ===
        if needs_render {
            draw_app(
                terminal,
                &ctx,
                active_tab,
                &search_page,
                &settings_page,
                &sources_page,
                &mut main_page,
                &mut leaderboard,
                &mut playlists,
                &mut favorites_page,
                &mut history_state,
                &mut local_state,
                &mut data_cache.local,
                &mut data_cache.history,
                &mut data_cache.favorites,
                &history_filter,
                &local_filter,
                &mut ui_areas,
                &confirm_delete,
                &local_diagnostics,
                &song_menu,
                &qr_login_page,
                &mut help_page,
                &mut downloads_panel,
            )?;
            needs_render = false;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_app(
    terminal: &mut DefaultTerminal,
    ctx: &AppContext,
    active_tab: NavTab,
    search_page: &Arc<std::sync::Mutex<pages::search::SearchPage>>,
    settings_page: &Arc<std::sync::Mutex<pages::settings::SettingsPage>>,
    sources_page: &Arc<std::sync::Mutex<pages::sources::SourcesPage>>,
    main_page: &mut pages::main_page::MainPage,
    leaderboard: &mut pages::leaderboard::LeaderboardPage,
    playlists: &mut pages::playlists::PlaylistsPage,
    favorites_page: &mut pages::favorites::FavoritesPage,
    history_state: &mut SortState,
    local_state: &mut SortState,
    data_cache_local: &mut SortedListCache,
    data_cache_history: &mut SortedListCache,
    data_cache_favorites: &mut SortedListCache,
    history_filter: &components::list_filter::ListFilter,
    local_filter: &components::list_filter::ListFilter,
    ui_areas: &mut UiAreas,
    confirm_delete: &Option<LocalDeleteConfirmation>,
    local_diagnostics: &Option<LocalDiagnosticsKind>,
    song_menu: &Option<SongContextMenu>,
    qr_login_page: &Option<Arc<std::sync::Mutex<pages::qr_login::QrLoginPage>>>,
    help_page: &mut Option<pages::help::HelpPage>,
    downloads_panel: &mut pages::downloads::DownloadsPanel,
) -> anyhow::Result<()> {
    terminal.draw(|frame| {
        let area = frame.area();
        frame.render_widget(
            ratatui::widgets::Block::default().style(
                Style::new()
                    .bg(crate::theme::base(ctx))
                    .fg(crate::theme::text(ctx)),
            ),
            area,
        );
        // TUI 2.0：固定导航 + 内容工作区 + 播放/状态底栏。
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([pages::sidebar::constraint(), Constraint::Min(30)])
            .split(area);
        let main_area = body[1];
        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // 页面标题 + 底部分隔线
                Constraint::Min(3),    // page content
                Constraint::Length(5), // 播放器：歌曲 / 进度 / 控制 + 顶部分隔线
            ])
            .split(main_area);

        let tab_label = active_tab.tab_label();
        pages::sidebar::render(body[0], frame.buffer_mut(), active_tab, ctx);
        components::header::render(main_chunks[0], frame.buffer_mut(), ctx, tab_label);
        let content_area = main_chunks[1];
        let controls_area = main_chunks[2];
        let tasks = ctx.downloads.snapshot();

        components::player_controls::render(controls_area, frame.buffer_mut(), ctx);

        let player_rows = components::player_controls::areas(controls_area);
        *ui_areas = UiAreas {
            tabs: body[0],
            content: content_area,
            player_progress: player_rows.progress,
            player_controls: player_rows.controls,
            notification: Rect::default(),
        };

        match active_tab {
            NavTab::Search => {
                let mut sp = search_page.lock().unwrap_or_else(|e| e.into_inner());
                sp.render(content_area, frame.buffer_mut(), ctx);
            }
            NavTab::Main => {
                main_page.render(content_area, frame.buffer_mut(), ctx);
            }
            NavTab::Leaderboard => {
                leaderboard.render(content_area, frame.buffer_mut(), ctx);
            }
            NavTab::Playlists => {
                playlists.render(content_area, frame.buffer_mut(), ctx);
            }
            NavTab::Favorites => {
                favorites_page.render(content_area, frame.buffer_mut(), ctx, data_cache_favorites);
            }
            NavTab::History => {
                pages::history::render(
                    content_area,
                    frame.buffer_mut(),
                    ctx,
                    history_state,
                    history_filter,
                    data_cache_history,
                );
            }
            NavTab::Sources => {
                let mut sp = sources_page.lock().unwrap_or_else(|e| e.into_inner());
                sp.render(content_area, frame.buffer_mut(), ctx);
            }
            NavTab::Settings => {
                let mut sp = settings_page.lock().unwrap_or_else(|e| e.into_inner());
                sp.render(content_area, frame.buffer_mut(), ctx);
            }
            NavTab::Downloads => {
                downloads_panel.render_page(content_area, frame.buffer_mut(), ctx, &tasks);
            }
            NavTab::LocalMusic => {
                use ratatui::style::{Color, Style};
                use ratatui::text::{Line, Span};
                use ratatui::widgets::{Block, Borders, Paragraph, Widget};

                'local_content: {
                    let local_src = ctx.source_manager.local_source();
                    let paths = ctx
                        .config
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .local_music
                        .paths
                        .clone();
                    let all_songs =
                        pages::local_music::sorted_local_songs(ctx, local_state, data_cache_local);
                    let is_scanning = local_src.is_scanning();
                    let scan_stats = local_src.scan_stats();
                    let missing_count = local_src.missing_count();

                    let songs =
                        pages::local_music::LocalSongView::build(all_songs, local_filter.query());

                    local_state.selected = local_state.selected.min(songs.len().saturating_sub(1));

                    let filter_suffix = if local_filter.query().is_empty() {
                        String::new()
                    } else {
                        format!(" · 过滤 '{}' ({} 匹配)", local_filter.query(), songs.len())
                    };

                    let diagnostics = if scan_stats.failed > 0 || missing_count > 0 {
                        format!(" · 损坏 {} · 缺失 {}", scan_stats.failed, missing_count)
                    } else {
                        String::new()
                    };
                    let block = crate::pages::components::chrome::card(
                        ctx,
                        if is_scanning {
                            format!(
                                " 本地音乐 · {} 首 · ⏳ 扫描中 · 排序 {} · s 切换{}{} ",
                                songs.len(),
                                local_state.mode.label(SortTarget::Local),
                                filter_suffix,
                                diagnostics
                            )
                        } else {
                            format!(
                                " 本地音乐 · {} 首 · 排序 {} · s 切换{}{} ",
                                songs.len(),
                                local_state.mode.label(SortTarget::Local),
                                filter_suffix,
                                diagnostics
                            )
                        },
                    );
                    let inner = block.inner(content_area);
                    block.render(content_area, frame.buffer_mut());

                    if local_filter.is_active() || !local_filter.query().is_empty() {
                        local_filter.render(
                            Rect::new(inner.x, inner.y, inner.width, 1),
                            frame.buffer_mut(),
                            ctx,
                        );
                    }

                    if inner.height < 2 {
                        break 'local_content;
                    }

                    if paths.is_empty() {
                        Paragraph::new(Line::from(
                            " 未配置音乐目录，请在设置（0）→ 数据与本地库中添加",
                        ))
                        .style(Style::new().fg(Color::DarkGray))
                        .render(inner, frame.buffer_mut());
                        break 'local_content;
                    }

                    if songs.is_empty() && is_scanning {
                        Paragraph::new(Line::from(" 正在扫描本地音乐，请稍候..."))
                            .style(Style::new().fg(Color::DarkGray))
                            .render(inner, frame.buffer_mut());
                        break 'local_content;
                    }

                    if songs.is_empty() {
                        Paragraph::new(Line::from(" 目录下未找到音频文件，按 r 重新扫描"))
                            .style(Style::new().fg(Color::DarkGray))
                            .render(inner, frame.buffer_mut());
                        break 'local_content;
                    }

                    let filter_visible =
                        local_filter.is_active() || !local_filter.query().is_empty();
                    let content_y = if filter_visible { inner.y + 1 } else { inner.y };
                    let content_height = if filter_visible {
                        inner.height.saturating_sub(1)
                    } else {
                        inner.height
                    };

                    if content_height < 3 {
                        break 'local_content;
                    }

                    let visible_height = (content_height.saturating_sub(2)) as usize;
                    let sel = local_state.selected;
                    let mut sc = local_state.scroll;

                    if sel >= sc + visible_height {
                        sc = sel.saturating_sub(visible_height.saturating_sub(1));
                    } else if sel < sc {
                        sc = sel;
                    }
                    sc = sc.min(songs.len().saturating_sub(visible_height));
                    local_state.scroll = sc;

                    let header = pages::components::song_table::header(inner.width);
                    Paragraph::new(Line::from(Span::styled(
                        header,
                        Style::new()
                            .fg(crate::theme::text(ctx))
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    )))
                    .render(
                        Rect::new(inner.x, content_y, inner.width, 1),
                        frame.buffer_mut(),
                    );

                    let end = (sc + visible_height).min(songs.len());
                    for row in sc..end {
                        let Some(song) = songs.get(row) else {
                            break;
                        };
                        let i = row;
                        let text = pages::components::song_table::row(song, i, inner.width);
                        // 数据行紧跟在列头下方：过滤条可见时整体下移一行，
                        // 否则 row 0 会把列头覆盖掉。
                        let line_area =
                            Rect::new(inner.x, content_y + 1 + (row - sc) as u16, inner.width, 1);
                        let style = if i == sel {
                            Style::new()
                                .bg(crate::theme::accent(ctx))
                                .fg(crate::theme::selection_fg(ctx))
                        } else {
                            Style::new().fg(crate::theme::text(ctx))
                        };
                        Paragraph::new(Line::from(Span::styled(text, style)))
                            .render(line_area, frame.buffer_mut());
                    }

                    if let Some(confirmation) = confirm_delete {
                        use ratatui::widgets::{Clear, Wrap};
                        let dialog_w = inner.width.saturating_sub(2).min(72);
                        let dialog_h = inner.height.min(7);
                        let dialog_x = inner.x + (inner.width.saturating_sub(dialog_w)) / 2;
                        let dialog_y = inner.y + (inner.height.saturating_sub(dialog_h)) / 2;
                        let dialog_area = Rect::new(dialog_x, dialog_y, dialog_w, dialog_h);
                        Clear.render(dialog_area, frame.buffer_mut());
                        let block = Block::default()
                            .borders(Borders::ALL)
                            .border_style(Style::new().fg(crate::theme::rosewater(ctx)))
                            .title("确认删除本地文件");
                        let inner_dialog = block.inner(dialog_area);
                        block.render(dialog_area, frame.buffer_mut());
                        Paragraph::new(vec![
                            Line::from(Span::styled(
                                format!("删除「{}」？", confirmation.name),
                                Style::new().fg(crate::theme::rosewater(ctx)),
                            )),
                            Line::from(Span::styled(
                                confirmation.path.display().to_string(),
                                Style::new().fg(crate::theme::muted(ctx)),
                            )),
                            Line::from(""),
                            Line::from(Span::styled(
                                "y 确认删除    n / Esc 取消",
                                Style::new().fg(crate::theme::text(ctx)),
                            )),
                        ])
                        .wrap(Wrap { trim: false })
                        .render(inner_dialog, frame.buffer_mut());
                    }
                }
            }
        }

        if let Some(kind) = local_diagnostics {
            render_local_diagnostics(content_area, frame.buffer_mut(), ctx, *kind);
        }

        if let Some(page) = qr_login_page {
            use ratatui::widgets::{Clear, Widget};

            let overlay_area = calculate_bili_login_area(area);
            Clear.render(overlay_area, frame.buffer_mut());
            ratatui::widgets::Block::default()
                .style(
                    Style::new()
                        .bg(crate::theme::base(ctx))
                        .fg(crate::theme::text(ctx)),
                )
                .render(overlay_area, frame.buffer_mut());
            let p = page.lock().unwrap_or_else(|e| e.into_inner());
            p.render(overlay_area, frame.buffer_mut(), ctx);
        }

        ui_areas.notification = components::notification::area(area, ctx).unwrap_or_default();
        components::notification::render(area, frame.buffer_mut(), ctx);
        if let Some(menu) = song_menu {
            menu.render(content_area, frame.buffer_mut(), ctx);
        }
        if let Some(page) = ctx
            .details_page
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            use ratatui::widgets::{Clear, Widget};
            Clear.render(area, frame.buffer_mut());
            page.render(area, frame.buffer_mut(), ctx);
        }
        if let Some(help) = help_page.as_mut() {
            use ratatui::widgets::{Clear, Widget};
            Clear.render(area, frame.buffer_mut());
            help.render(area, frame.buffer_mut(), ctx);
        }
        if downloads_panel.is_open() {
            let tasks = ctx.downloads.snapshot();
            downloads_panel.render(area, frame.buffer_mut(), ctx, &tasks);
        }
    })?;
    Ok(())
}

fn render_local_diagnostics(
    area: Rect,
    buf: &mut ratatui::buffer::Buffer,
    ctx: &AppContext,
    kind: LocalDiagnosticsKind,
) {
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};

    let source = ctx.source_manager.local_source();
    let mut lines = Vec::new();
    let title = match kind {
        LocalDiagnosticsKind::Corrupt => "损坏文件",
        LocalDiagnosticsKind::Missing => "缺失文件",
        LocalDiagnosticsKind::Duplicates => "重复歌曲",
    };
    match kind {
        LocalDiagnosticsKind::Corrupt => {
            for failure in source.corrupt_files() {
                lines.push(Line::from(vec![
                    Span::styled(
                        failure.path.display().to_string(),
                        Style::new().fg(crate::theme::text(ctx)),
                    ),
                    Span::raw("  "),
                    Span::styled(failure.error, Style::new().fg(crate::theme::muted(ctx))),
                ]));
            }
        }
        LocalDiagnosticsKind::Missing => {
            for missing in source.missing_files() {
                lines.push(Line::from(vec![
                    Span::styled(
                        missing.path.display().to_string(),
                        Style::new().fg(crate::theme::text(ctx)),
                    ),
                    Span::raw("  "),
                    Span::styled(missing.song.name, Style::new().fg(crate::theme::muted(ctx))),
                ]));
            }
        }
        LocalDiagnosticsKind::Duplicates => {
            for group in source.duplicate_groups() {
                let names = group
                    .songs
                    .iter()
                    .map(|song| {
                        song.file_path
                            .as_ref()
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|| song.id.clone())
                    })
                    .collect::<Vec<_>>()
                    .join("  <->  ");
                lines.push(Line::from(Span::styled(
                    names,
                    Style::new().fg(crate::theme::text(ctx)),
                )));
            }
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "没有诊断项",
            Style::new().fg(crate::theme::muted(ctx)),
        )));
    }
    let width = area.width.saturating_sub(4).min(110);
    let height = area.height.saturating_sub(4).max(5);
    let overlay = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    Clear.render(overlay, buf);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(crate::theme::rosewater(ctx)))
        .title(format!("本地库诊断 · {title} · i 切换，Esc 关闭"));
    let inner = block.inner(overlay);
    block.render(overlay, buf);
    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .render(inner, buf);
}

fn calculate_bili_login_area(area: Rect) -> Rect {
    let qr_width = 66u16;
    let qr_height = 40u16;
    let w = qr_width.min(area.width.saturating_sub(4));
    let h = qr_height.min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

#[cfg(target_os = "linux")]
fn current_mpris_snapshot(ctx: &AppContext) -> mpris::MprisSnapshot {
    let song = ctx.current_song.read().unwrap_or_else(|e| e.into_inner());
    mpris::MprisSnapshot::new(
        *ctx.player_state.borrow(),
        song.as_ref(),
        *ctx.position.borrow(),
        *ctx.duration.borrow(),
        ctx.player.volume(),
        ctx.playlist.mode(),
        ctx.playlist.len(),
        ctx.position_epoch(),
    )
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
fn execute_mpris_command(
    command: mpris::MprisCommand,
    ctx: &AppContext,
    rt: &tokio::runtime::Runtime,
    action_tx: &mpsc::UnboundedSender<AppAction>,
    search_page: &Arc<std::sync::Mutex<pages::search::SearchPage>>,
    settings_page: &Arc<std::sync::Mutex<pages::settings::SettingsPage>>,
    search_seq: &Arc<AtomicU64>,
) -> bool {
    use mpris::MprisCommand;

    let play_entry = |entry: Option<(Arc<Vec<SongInfo>>, usize)>| {
        if let Some((songs, index)) = entry {
            execute_action(
                AppAction::PlayFromQueue { songs, index },
                ctx,
                rt,
                action_tx,
                search_page,
                settings_page,
                search_seq,
            );
        }
    };

    match command {
        MprisCommand::Quit => return true,
        MprisCommand::Play => {
            resume_or_start_current(ctx, rt, action_tx, search_page, settings_page, search_seq)
        }
        MprisCommand::Pause => ctx.player.pause(),
        MprisCommand::Toggle => {
            toggle_or_start_current(ctx, rt, action_tx, search_page, settings_page, search_seq)
        }
        MprisCommand::Stop => ctx.stop_player(),
        MprisCommand::Next => play_entry(ctx.playlist.next_manual_entry_arc()),
        MprisCommand::Previous => play_entry(ctx.playlist.prev_manual_entry_arc()),
        MprisCommand::SeekBy(offset) => {
            let current = ctx.position.borrow().as_micros();
            let target = if offset >= 0 {
                current.saturating_add(offset as u128)
            } else {
                current.saturating_sub(offset.unsigned_abs() as u128)
            };
            ctx.seek(Duration::from_micros(target.min(u64::MAX as u128) as u64));
        }
        MprisCommand::SetPosition(position) => ctx.seek(position),
        MprisCommand::SetVolume(volume) => {
            persist_volume(ctx, (volume.clamp(0.0, 1.0) * 100.0).round() as u32);
        }
    }
    false
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
fn resume_or_start_current(
    ctx: &AppContext,
    rt: &tokio::runtime::Runtime,
    action_tx: &mpsc::UnboundedSender<AppAction>,
    search_page: &Arc<std::sync::Mutex<pages::search::SearchPage>>,
    settings_page: &Arc<std::sync::Mutex<pages::settings::SettingsPage>>,
    search_seq: &Arc<AtomicU64>,
) {
    if *ctx.player_state.borrow() == PlayerState::Paused {
        ctx.player.resume();
    } else if matches!(
        *ctx.player_state.borrow(),
        PlayerState::Idle | PlayerState::Stopped
    ) {
        start_current_queue_entry(ctx, rt, action_tx, search_page, settings_page, search_seq);
    }
}

#[allow(clippy::too_many_arguments)]
fn toggle_or_start_current(
    ctx: &AppContext,
    rt: &tokio::runtime::Runtime,
    action_tx: &mpsc::UnboundedSender<AppAction>,
    search_page: &Arc<std::sync::Mutex<pages::search::SearchPage>>,
    settings_page: &Arc<std::sync::Mutex<pages::settings::SettingsPage>>,
    search_seq: &Arc<AtomicU64>,
) {
    // Copy the state out of the watch channel first: pause()/resume() send
    // a new state into the same watch channel, and a live watch::Ref holds
    // the RwLock read guard that the send needs as a write lock.
    let state = *ctx.player_state.borrow();
    match state {
        PlayerState::Playing | PlayerState::Loading => ctx.player.pause(),
        PlayerState::Paused => ctx.player.resume(),
        PlayerState::Idle | PlayerState::Stopped => {
            start_current_queue_entry(ctx, rt, action_tx, search_page, settings_page, search_seq);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn start_current_queue_entry(
    ctx: &AppContext,
    rt: &tokio::runtime::Runtime,
    action_tx: &mpsc::UnboundedSender<AppAction>,
    search_page: &Arc<std::sync::Mutex<pages::search::SearchPage>>,
    settings_page: &Arc<std::sync::Mutex<pages::settings::SettingsPage>>,
    search_seq: &Arc<AtomicU64>,
) {
    let (songs, index) = ctx.playlist.snapshot_arc();
    if songs.get(index).is_some() {
        execute_action(
            AppAction::PlayFromQueue { songs, index },
            ctx,
            rt,
            action_tx,
            search_page,
            settings_page,
            search_seq,
        );
    }
}

fn persist_volume(ctx: &AppContext, volume: u32) {
    let volume = volume.clamp(0, 100);
    ctx.player.set_volume(volume);
    {
        let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
        if config.player.volume == volume {
            return;
        }
        config.player.volume = volume;
    }
    ctx.mark_config_dirty();
}

/// 执行一个 AppAction（简化版，不再处理 Navigate/GoBack）
fn execute_action(
    action: AppAction,
    ctx: &AppContext,
    rt: &tokio::runtime::Runtime,
    action_tx: &mpsc::UnboundedSender<AppAction>,
    search_page: &Arc<std::sync::Mutex<pages::search::SearchPage>>,
    settings_page: &Arc<std::sync::Mutex<pages::settings::SettingsPage>>,
    search_seq: &Arc<AtomicU64>,
) {
    match action {
        AppAction::Search { keyword, source } => {
            let mut sp = search_page.lock().unwrap_or_else(|e| e.into_inner());
            sp.begin_search(&keyword, false);
            drop(sp);
            if lx_core::model::source::SourceId::detect_from_link(&keyword).is_some() {
                // 粘贴分享链接时直接解析，不把整条 URL 当成关键词去搜。
                spawn_link_parse(
                    keyword,
                    Arc::clone(search_page),
                    Arc::clone(&ctx.source_manager),
                    action_tx.clone(),
                    rt,
                    search_seq.clone(),
                );
            } else {
                let sp_clone = Arc::clone(search_page);
                spawn_search(
                    keyword,
                    1,
                    false,
                    source,
                    sp_clone,
                    Arc::clone(&ctx.source_manager),
                    action_tx.clone(),
                    rt,
                    search_seq.clone(),
                );
            }
        }
        AppAction::SearchMore {
            keyword,
            page,
            source,
        } => {
            let mut sp = search_page.lock().unwrap_or_else(|e| e.into_inner());
            if sp.is_searching
                || sp.result_keyword != keyword
                || sp.source_filter != source
                || page != sp.current_page + 1
            {
                return;
            }
            sp.begin_search(&keyword, true);
            drop(sp);
            spawn_search(
                keyword,
                page,
                true,
                source,
                Arc::clone(search_page),
                Arc::clone(&ctx.source_manager),
                action_tx.clone(),
                rt,
                search_seq.clone(),
            );
        }
        AppAction::ResolveBiliParts {
            songs,
            index,
            request_id,
        } => {
            let Some(song) = songs.get(index).cloned() else {
                return;
            };
            let bili_source = Arc::clone(&ctx.bili_source);
            let search_page = Arc::clone(search_page);
            let tx = action_tx.clone();
            rt.spawn(async move {
                match tokio::time::timeout(Duration::from_secs(15), bili_source.video_parts(&song))
                    .await
                {
                    Ok(Ok(parts)) if !parts.is_empty() => {
                        if let Some(action) = search_page
                            .lock()
                            .unwrap()
                            .complete_bili_part_request(request_id, songs, index, parts)
                        {
                            let _ = tx.send(action);
                        }
                    }
                    Ok(Ok(_)) => {
                        if search_page
                            .lock()
                            .unwrap()
                            .fail_bili_part_request(request_id)
                        {
                            let _ = tx.send(AppAction::ShowNotification(
                                Notification::warning("未找到可播放的分 P").tui_only(),
                            ));
                        }
                    }
                    Ok(Err(error)) => {
                        if search_page
                            .lock()
                            .unwrap()
                            .fail_bili_part_request(request_id)
                        {
                            let _ = tx.send(AppAction::ShowNotification(
                                Notification::warning(format!("分 P 解析失败: {error}")).tui_only(),
                            ));
                        }
                    }
                    Err(_) => {
                        if search_page
                            .lock()
                            .unwrap()
                            .fail_bili_part_request(request_id)
                        {
                            let _ = tx.send(AppAction::ShowNotification(
                                Notification::warning("分 P 解析超时").tui_only(),
                            ));
                        }
                    }
                }
            });
        }
        AppAction::PlaySong { songs, index } => {
            begin_song_from_list(songs, index, false, ctx, rt, action_tx);
        }
        AppAction::PlaySongAfterFailure { songs, index } => {
            begin_song_from_list(songs, index, true, ctx, rt, action_tx);
        }
        AppAction::PlayFromQueue { songs, index } => {
            begin_song_from_arc(songs, index, false, ctx, rt, action_tx);
        }
        AppAction::PlayFromQueueAfterFailure { songs, index } => {
            begin_song_from_arc(songs, index, true, ctx, rt, action_tx);
        }
        AppAction::RestorePlayback {
            songs,
            index,
            position,
            start_playback,
            paused,
        } => {
            if let Some(song) = songs.get(index).cloned() {
                ctx.playlist.set_playlist(songs, index);
                ctx.play_attempted_sources
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clear();
                *ctx.play_js_source_index
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = None;
                if start_playback {
                    start_song_playback(
                        song,
                        false,
                        Some((position, paused)),
                        true,
                        ctx,
                        rt,
                        action_tx,
                    );
                } else {
                    ctx.stop_player();
                    *ctx.current_song.write().unwrap_or_else(|e| e.into_inner()) = Some(song);
                }
            }
        }
        AppAction::AddToQueue { song, position } => {
            let song = *song;
            let was_empty = ctx.playlist.borrow().is_empty();
            let inserted = ctx.playlist.insert(song.clone(), position);
            // 空队列时插入即是开始播放，否则用户点了“下一首播放”却静默无反应
            if was_empty {
                let (songs, index) = ctx.playlist.snapshot();
                if let Some(current) = songs.get(index).cloned() {
                    begin_song_from_arc(
                        std::sync::Arc::new(songs),
                        index,
                        false,
                        ctx,
                        rt,
                        action_tx,
                    );
                    ctx.notify(Notification::success(format!(
                        "开始播放: {} - {}",
                        current.name, current.singer
                    )));
                    return;
                }
            }
            let message = match (position, inserted) {
                (InsertPosition::Next, 0) | (InsertPosition::End, _) => {
                    format!("已加入队列: {} - {}", song.name, song.singer)
                }
                (InsertPosition::Next, _) => {
                    format!("下一首播放: {} - {}", song.name, song.singer)
                }
            };
            ctx.notify(Notification::success(message));
        }
        AppAction::ToggleFavoriteSong(song) => {
            let song = *song;
            let message = if ctx.storage.is_favorite(&song) {
                ctx.storage.remove_favorite(&song);
                "已取消收藏"
            } else {
                ctx.storage.add_favorite(&song);
                "已添加收藏"
            };
            ctx.notify(Notification::success(message));
        }
        AppAction::DownloadSong(song) => {
            // 下载在后台任务里完成，主循环只负责入队并给出即时反馈。
            let config = ctx.config.read().unwrap_or_else(|e| e.into_inner()).clone();
            ctx.downloads.sync_config(&config);
            drop(config);
            ctx.downloads
                .enqueue(*song, Arc::clone(&ctx.source_manager), action_tx.clone());
        }
        AppAction::RetrySong { song } => {
            start_song_playback(*song, false, None, false, ctx, rt, action_tx);
        }
        AppAction::PlaybackFailed { request_id, error } => {
            if ctx.play_request_id.load(Ordering::SeqCst) != request_id {
                return;
            }
            ctx.stop_player();
            if let Some((songs, index)) = ctx.playlist.next_after_failure_arc() {
                let failed = ctx
                    .current_song
                    .read()
                    .unwrap()
                    .as_ref()
                    .map(|song| format!("{} - {}", song.name, song.singer))
                    .unwrap_or_else(|| "当前歌曲".to_string());
                ctx.notify(Notification::warning(format!("{error}；已跳过 {failed}")).tui_only());
                begin_song_from_arc(songs, index, true, ctx, rt, action_tx);
            } else {
                ctx.notify(Notification::error(format!(
                    "{error}；队列中没有更多可播放歌曲"
                )));
            }
        }
        AppAction::ShowNotification(n) => {
            ctx.notify(n);
        }
        AppAction::ImportSource(url) => {
            tracing::info!("importing JS source: {url}");
            let source_mgr = Arc::clone(&ctx.source_manager);
            let generation = source_mgr.begin_js_source_request(false);
            let default_source = ctx
                .config
                .read()
                .unwrap()
                .source
                .default
                .as_str()
                .to_string();
            let tx = action_tx.clone();

            rt.spawn(async move {
                match lx_source::js::loader::load_source_approving_update(&url, &default_source)
                    .await
                {
                    Ok(_) => {
                        if !source_mgr.is_js_source_request_current(generation) {
                            return;
                        }
                        let _ = tx.send(AppAction::SourceImported { url, generation });
                    }
                    Err(e) => {
                        let _ = tx.send(AppAction::SourceImportFailed {
                            error: e,
                            generation,
                        });
                    }
                }
            });
        }
        AppAction::SourceImported { url, generation } => {
            if !ctx.source_manager.is_js_source_request_current(generation) {
                return;
            }
            tracing::info!("JS source imported: {url}");
            let mut sp = settings_page.lock().unwrap_or_else(|e| e.into_inner());
            sp.selected_source = 0;
            sp.status_msg = Some("✓ 音源已加载并启用".to_string());
            drop(sp);
            let save_result = {
                let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                config.source.js_sources.retain(|item| item != &url);
                config.source.js_sources.insert(0, url);
                crate::config::loader::save(&config, &ctx.config_path)
            };
            if let Err(e) = save_result {
                let mut sp = settings_page.lock().unwrap_or_else(|e| e.into_inner());
                sp.status_msg = Some(format!("✗ 音源已启用，但保存配置失败: {}", e));
                ctx.notify(Notification::error(format!("保存 JS 音源配置失败: {}", e)));
            } else {
                let (urls, default_source) = {
                    let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
                    (
                        config.source.js_sources.clone(),
                        config.source.default.as_str().to_string(),
                    )
                };
                let generation = ctx.source_manager.begin_js_source_request(true);
                spawn_js_source_loader(
                    urls,
                    default_source,
                    Arc::clone(&ctx.source_manager),
                    generation,
                    action_tx.clone(),
                    rt,
                );
                ctx.notify(Notification::success("JS 音源配置已更新，正在加载全部脚本"));
            }
        }
        AppAction::SourceImportFailed { error, generation } => {
            if !ctx.source_manager.is_js_source_request_current(generation) {
                return;
            }
            tracing::warn!("JS source import failed: {error}");
            let mut sp = settings_page.lock().unwrap_or_else(|e| e.into_inner());
            sp.status_msg = Some(format!("✗ 音源加载失败: {}", error));
            ctx.notify(Notification::error(format!("JS 音源导入失败: {}", error)));
        }
        AppAction::CheckSourceHealth => {
            if ctx
                .source_health_checking
                .swap(true, std::sync::atomic::Ordering::AcqRel)
            {
                return;
            }
            let source_manager = Arc::clone(&ctx.source_manager);
            let tx = action_tx.clone();
            rt.spawn(async move {
                let results = source_manager.health_check().await;
                let _ = tx.send(AppAction::SourceHealthChecked { results });
            });
        }
        AppAction::SourceHealthChecked { results } => {
            ctx.source_health_checking
                .store(false, std::sync::atomic::Ordering::Release);
            let healthy = results.iter().filter(|result| result.ok).count();
            let total = results.len();
            let failures = results
                .iter()
                .filter(|result| !result.ok)
                .map(|result| format!("{}: {}", result.name, result.detail))
                .collect::<Vec<_>>();
            *ctx.source_health.write().unwrap_or_else(|e| e.into_inner()) = results;
            settings_page
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .status_msg = Some(if failures.is_empty() {
                format!("音源检测完成：{healthy}/{total} 可用")
            } else {
                format!(
                    "音源检测完成：{healthy}/{total} 可用；失败：{}",
                    failures.join("；")
                )
            });
            ctx.notify(
                Notification::info(format!("音源检测完成：{healthy}/{total} 可用")).tui_only(),
            );
        }
        AppAction::RemoveSource(url) => {
            tracing::info!("removing JS source: {url}");
            let generation = ctx.source_manager.begin_js_source_request(true);
            let (remaining_urls, default_source) = {
                let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                config.source.js_sources.retain(|u| u != &url);
                let remaining_urls = config.source.js_sources.clone();
                let default_source = config.source.default.as_str().to_string();
                if let Err(e) = crate::config::loader::save(&config, &ctx.config_path) {
                    tracing::warn!("保存配置失败: {}", e);
                }
                (remaining_urls, default_source)
            };
            spawn_js_source_loader(
                remaining_urls,
                default_source,
                Arc::clone(&ctx.source_manager),
                generation,
                action_tx.clone(),
                rt,
            );
            let _ = action_tx.send(AppAction::ShowNotification(Notification::success(
                "已移除音源",
            )));
        }
        AppAction::RemoveHistory(song) => {
            if ctx.storage.remove_history(&song) {
                ctx.notify(Notification::success(format!(
                    "已删除历史记录: {}",
                    song.name
                )));
            }
        }
        AppAction::ClearHistory => {
            if ctx.storage.clear_history() {
                ctx.notify(Notification::success("播放历史已清空"));
            } else {
                ctx.notify(Notification::info("播放历史已经是空的"));
            }
        }
        AppAction::ScanLocalMusic {
            paths,
            max_depth,
            force,
        } => {
            let generation = next_generation(&ctx.local_scan_request_id);
            let request_seq = Arc::clone(&ctx.local_scan_request_id);
            let local_source = ctx.source_manager.local_source();
            let watcher_source = Arc::clone(&local_source);
            let source_generation = local_source.begin_scan();
            let settings = Arc::clone(settings_page);
            let tx = action_tx.clone();
            rt.spawn(async move {
                let watcher_paths = paths.clone();
                let result = tokio::task::spawn_blocking(move || {
                    let errors = local_source.scan_for_generation(
                        &paths,
                        max_depth,
                        source_generation,
                        force,
                    );
                    let count = local_source.all_songs().len();
                    (errors, count)
                })
                .await;
                if request_seq.load(Ordering::SeqCst) != generation {
                    return;
                }
                if let Err(error) = watcher_source.start_watcher(
                    watcher_paths,
                    max_depth,
                    std::time::Duration::from_secs(2),
                ) {
                    tracing::warn!("启动本地音乐监听失败: {error}");
                }
                let (errors, count) = match result {
                    Ok(result) => result,
                    Err(error) => (vec![format!("本地音乐扫描任务失败: {error}")], 0),
                };
                let mut settings = settings.lock().unwrap_or_else(|e| e.into_inner());
                if errors.is_empty() {
                    settings.status_msg = Some(format!("本地音乐扫描完成，共 {} 首", count));
                    let _ = tx.send(AppAction::ShowNotification(Notification::success(format!(
                        "本地音乐扫描完成，共 {} 首",
                        count
                    ))));
                } else {
                    settings.status_msg = Some(format!("扫描错误: {}", errors.join("; ")));
                    for error in errors {
                        let _ = tx.send(AppAction::ShowNotification(Notification::error(error)));
                    }
                }
            });
        }
        AppAction::ImportExternalPlaylist(path) => {
            // 歌单解析与写盘都放到后台线程，完成后用通知汇报结果，
            // 避免在 TUI 主循环里同步解析大歌单并反复写盘。
            let storage = Arc::clone(&ctx.storage);
            let tx = action_tx.clone();
            rt.spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    storage.import_external_playlist(std::path::Path::new(&path))
                })
                .await;
                let notification = match result {
                    Ok(Ok(report)) => Notification::success(format!(
                        "已导入歌单 {}：{} 首，跳过 {} 首",
                        report.playlist_name, report.imported, report.skipped
                    )),
                    Ok(Err(error)) => Notification::error(format!("歌单导入失败: {error}")),
                    Err(error) => Notification::error(format!("歌单导入任务失败: {error}")),
                };
                let _ = tx.send(AppAction::ShowNotification(notification));
            });
        }
        AppAction::ShowArtistDetails(song) => {
            let artist = lx_core::model::playlist::Artist {
                id: String::new(),
                name: song.singer.trim().to_string(),
                source: song.source,
                cover_url: song.cover_url.clone(),
            };
            *ctx.details_page.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(pages::details::DetailsPage::artist(artist.clone()));
            let manager = Arc::clone(&ctx.source_manager);
            let details = Arc::clone(&ctx.details_page);
            rt.spawn(async move {
                let albums = tokio::time::timeout(
                    Duration::from_secs(15),
                    manager.artist_albums(&artist, 0, 100),
                )
                .await;
                let songs = manager.artist_songs(&artist, 0, 100).await;
                let mut guard = details.lock().unwrap_or_else(|e| e.into_inner());
                let Some(page) = guard.as_mut() else {
                    return;
                };
                match (albums, songs) {
                    (Ok(Ok(albums)), Ok(songs)) => {
                        page.set_artist_page(albums, songs.items, songs.has_more);
                    }
                    (Ok(Ok(_)), Err(_)) | (Err(_), Ok(_)) => {
                        page.update_error("加载歌手详情超时".to_string());
                    }
                    (Ok(Err(error)), _) | (Err(_), Err(error)) => {
                        page.update_error(format!("加载歌手详情失败: {error}"));
                    }
                }
            });
        }
        AppAction::ShowAlbumDetails(album) => {
            *ctx.details_page.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(pages::details::DetailsPage::album(*album.clone()));
            let manager = Arc::clone(&ctx.source_manager);
            let details = Arc::clone(&ctx.details_page);
            rt.spawn(async move {
                let result = tokio::time::timeout(
                    Duration::from_secs(15),
                    manager.album_songs(&album, 0, 200),
                )
                .await;
                let mut guard = details.lock().unwrap_or_else(|e| e.into_inner());
                let Some(page) = guard.as_mut() else {
                    return;
                };
                match result {
                    Ok(Ok(result)) => page.update_album(Ok(result.items)),
                    Ok(Err(error)) => page.update_album(Err(error.to_string())),
                    Err(_) => page.update_album(Err("加载专辑曲目超时".to_string())),
                }
            });
        }
        AppAction::Navigate(_)
        | AppAction::GoBack
        | AppAction::Quit
        | AppAction::None
        | AppAction::QrLogin(_)
        | AppAction::QrLogout(_)
        | AppAction::QrLoginSuccess(_) => {
            // handled elsewhere or ignored
        }
    }
}

fn begin_song_from_list(
    songs: Vec<SongInfo>,
    index: usize,
    after_failure: bool,
    ctx: &AppContext,
    rt: &tokio::runtime::Runtime,
    action_tx: &mpsc::UnboundedSender<AppAction>,
) {
    let Some(song) = songs.get(index).cloned() else {
        tracing::debug!(
            index,
            song_count = songs.len(),
            "playback list index out of bounds"
        );
        return;
    };
    tracing::debug!(
        index,
        song_count = songs.len(),
        song_id = %song.id,
        song_name = %song.name,
        after_failure,
        "begin playback from list"
    );
    if should_expand_bili_parts(&song) {
        let request_id = next_play_request(ctx);
        let _ = prepare_player(ctx);
        if !set_current_song_if_current(
            &ctx.current_song,
            &ctx.play_request_id,
            request_id,
            song.clone(),
        ) {
            return;
        }
        ctx.notify(Notification::info(format!("正在解析分 P: {}", song.name)).tui_only());
        let bili_source = Arc::clone(&ctx.bili_source);
        let play_request_id = Arc::clone(&ctx.play_request_id);
        let tx = action_tx.clone();
        rt.spawn(async move {
            let result =
                tokio::time::timeout(Duration::from_secs(15), bili_source.video_parts(&song)).await;
            if play_request_id.load(Ordering::SeqCst) != request_id {
                return;
            }

            let mut songs = songs;
            let next_index = match result {
                Ok(Ok(parts)) if !parts.is_empty() => {
                    let part_count = parts.len();
                    songs.splice(index..=index, parts);
                    let _ = tx.send(AppAction::ShowNotification(
                        Notification::success(format!("已展开 {} 个分 P", part_count)).tui_only(),
                    ));
                    index
                }
                Ok(Ok(_)) => {
                    mark_bili_parts_checked(&mut songs[index]);
                    index
                }
                Ok(Err(error)) => {
                    mark_bili_parts_checked(&mut songs[index]);
                    let _ = tx.send(AppAction::ShowNotification(Notification::warning(format!(
                        "分 P 解析失败，将播放默认分 P: {error}"
                    ))));
                    index
                }
                Err(_) => {
                    mark_bili_parts_checked(&mut songs[index]);
                    let _ = tx.send(AppAction::ShowNotification(Notification::warning(
                        "分 P 解析超时，将播放默认分 P",
                    )));
                    index
                }
            };
            let action = if after_failure {
                AppAction::PlaySongAfterFailure {
                    songs,
                    index: next_index,
                }
            } else {
                AppAction::PlaySong {
                    songs,
                    index: next_index,
                }
            };
            let _ = tx.send(action);
        });
        return;
    }

    if after_failure {
        ctx.playlist.set_playlist_after_failure(songs, index);
    } else {
        ctx.playlist.set_playlist(songs, index);
    }
    // 已尝试音源集合与 JS 音源索引由 start_song_playback 在递增请求代次后清空
    start_song_playback(song, true, None, true, ctx, rt, action_tx);
}

/// 从当前队列继续播放：歌曲列表以 `Arc` 共享，不深拷贝整张队列。
///
/// 自动切歌（播放结束 / 播放失败跳过 / MPRIS 与快捷键切歌）都走此路径。
/// B 站分 P 歌曲仍需展开成普通列表，罕见情况下回退到 Vec 流程。
fn begin_song_from_arc(
    songs: Arc<Vec<SongInfo>>,
    index: usize,
    after_failure: bool,
    ctx: &AppContext,
    rt: &tokio::runtime::Runtime,
    action_tx: &mpsc::UnboundedSender<AppAction>,
) {
    let Some(song) = songs.get(index).cloned() else {
        return;
    };
    if should_expand_bili_parts(&song) {
        begin_song_from_list(songs.to_vec(), index, after_failure, ctx, rt, action_tx);
        return;
    }
    if after_failure {
        ctx.playlist.set_playlist_arc_after_failure(songs, index);
    } else {
        ctx.playlist.set_playlist_arc(songs, index);
    }
    // 已尝试音源集合与 JS 音源索引由 start_song_playback 在递增请求代次后清空
    start_song_playback(song, true, None, true, ctx, rt, action_tx);
}

fn start_song_playback(
    song: SongInfo,
    add_history: bool,
    restored_state: Option<(Duration, bool)>,
    reset_source_state: bool,
    ctx: &AppContext,
    rt: &tokio::runtime::Runtime,
    action_tx: &mpsc::UnboundedSender<AppAction>,
) {
    let request_id = next_play_request(ctx);
    // 新歌请求必须在递增请求代次之后清空“已尝试音源”与 JS 音源索引：
    // mark_source_attempted 持锁校验代次，过期任务要么看到新代次而放弃
    // 写入，要么写入发生在清空之前而被清掉，不会污染新请求。
    // 同曲重试（RetrySong）传 false，保留重试进度以免反复尝试同一失效源。
    if reset_source_state {
        ctx.play_attempted_sources
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        *ctx.play_js_source_index
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }
    let player_generation = prepare_player(ctx);
    let lyric_generation = ctx.lyric_service.prepare();
    if !set_current_song_if_current(
        &ctx.current_song,
        &ctx.play_request_id,
        request_id,
        song.clone(),
    ) {
        return;
    }
    let (show_cover, album_cover_notification, track_change_notification) = {
        let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
        (
            config.ui.show_cover,
            config.notification.album_cover,
            config.notification.track_change,
        )
    };
    let cover_service = Arc::clone(&ctx.cover_service);
    // 队列里的 SongInfo 通常已经带了封面地址，先用它加载一次，不必等播放地址解析完。
    let initial_cover = song.cover_url.clone();
    if show_cover {
        cover_service.clear();
        let initial_cover = initial_cover.clone();
        let cover = Arc::clone(&cover_service);
        let request_guard = Arc::clone(&ctx.play_request_id);
        let wake_tx = action_tx.clone();
        rt.spawn(async move {
            if request_guard.load(Ordering::SeqCst) != request_id {
                return;
            }
            if let Err(error) = cover.load(initial_cover).await {
                tracing::debug!("load initial cover failed: {}", error);
            }
            if request_guard.load(Ordering::SeqCst) != request_id {
                return;
            }
            // 封面加载不会产生任何事件，暂停时必须主动唤醒渲染循环
            let _ = wake_tx.send(AppAction::None);
        });
    } else {
        cover_service.clear();
    }

    if add_history {
        let limit = ctx
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .player
            .history_limit;
        ctx.storage.add_history(&song, limit);
    }
    if add_history {
        let _ = action_tx.send(AppAction::ShowNotification(
            Notification::info(format!("正在加载: {} - {}", song.name, song.singer)).tui_only(),
        ));
    }

    let source_mgr = Arc::clone(&ctx.source_manager);
    let player = Arc::clone(&ctx.player);
    let lyric_service = Arc::clone(&ctx.lyric_service);
    let lyric_position = ctx.lyric_position.clone();
    let lyric_tx = action_tx.clone();

    let current_song = Arc::clone(&ctx.current_song);
    let play_request_id = Arc::clone(&ctx.play_request_id);
    let attempted_sources = Arc::clone(&ctx.play_attempted_sources);
    let js_source_index = Arc::clone(&ctx.play_js_source_index);
    let (quality, auto_toggle, fade_in_ms) = {
        let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
        (
            config.player.quality,
            config.source.auto_toggle,
            config.player.fade_in_ms,
        )
    };
    let tx = action_tx.clone();

    let downloaded_song = ctx.downloads.existing_download(&song);
    rt.spawn(async move {
        // go-musicfox 的 TrackManager 同样采用“已下载 → 缓存 → 网络”的解析顺序。
        // Voicefox 当前把自动缓存落在下载目录，因此已下载/自动缓存文件统一走这里，
        // 避免再次请求失效或受限的远程播放地址。
        let resolved = if let Some(path) = downloaded_song {
            Ok(Ok(Some((
                song.clone(),
                SongUrl {
                    url: path.to_string_lossy().into_owned(),
                    quality,
                    duration: song.duration,
                    ..SongUrl::default()
                },
            ))))
        } else {
            tokio::time::timeout(
                Duration::from_secs(40),
                resolve_playable_song(
                    Arc::clone(&source_mgr),
                    song,
                    quality,
                    auto_toggle,
                    PlaybackResolveRequest {
                        play_request_id: Arc::clone(&play_request_id),
                        attempted_sources: Arc::clone(&attempted_sources),
                        js_source_index: Arc::clone(&js_source_index),
                        request_id,
                    },
                ),
            )
            .await
        };

        if play_request_id.load(Ordering::SeqCst) != request_id {
            return;
        }

        let (mut resolved_song, song_url) = match resolved {
            Ok(Ok(Some(resolved))) => resolved,
            Ok(Ok(None)) => return,
            Ok(Err(error)) => {
                let _ = tx.send(AppAction::PlaybackFailed { request_id, error });
                return;
            }
            Err(_) => {
                let _ = tx.send(AppAction::PlaybackFailed {
                    request_id,
                    error: "获取播放地址超时，请稍后重试".to_string(),
                });
                return;
            }
        };

        let url = song_url.url;
        let headers = song_url.headers;
        // libmpv 可能在 loadfile 返回后立刻报错，先保存实际匹配到的歌曲，
        // 让错误处理继续重试正确的候选音源。
        if !set_current_song_if_current(
            &current_song,
            &play_request_id,
            request_id,
            resolved_song.clone(),
        ) {
            return;
        }
        let player_for_start = Arc::clone(&player);
        let request_guard = Arc::clone(&play_request_id);
        let accepted = tokio::task::spawn_blocking(move || {
            if request_guard.load(Ordering::SeqCst) != request_id {
                return false;
            }
            player_for_start.play_with_headers(&url, player_generation, &headers)
        })
        .await
        .unwrap_or(false);
        if !accepted || play_request_id.load(Ordering::SeqCst) != request_id {
            return;
        }
        let cue_start = resolved_song
            .extra
            .get("cue_start_ms")
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_millis);
        if let Some((position, paused)) = restored_state {
            player.seek(position);
            if paused {
                player.pause();
            }
        } else if let Some(position) = cue_start {
            player.seek(position);
        }
        if fade_in_ms > 0 && restored_state.is_none_or(|(_, paused)| !paused) {
            player.fade_in(Duration::from_millis(fade_in_ms));
        }

        // 自动换源可能匹配到另一个版本，歌词必须跟随最终交给 libmpv 的歌曲。
        let lyric_song = resolved_song.clone();
        tokio::spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(15),
                lyric_service.load(&lyric_song, lyric_generation),
            )
            .await;
            match result {
                Err(error) => tracing::warn!("load lyric timeout: {}", error),
                Ok(Err(error)) => tracing::warn!("load lyric failed: {}", error),
                Ok(Ok(())) => {}
            }
            lyric_service.update_position(*lyric_position.borrow());
            let _ = lyric_tx.send(AppAction::None);
        });

        if resolved_song.cover_url.is_none() {
            resolved_song.cover_url = song_url.cover_url.clone();
        }
        if resolved_song.cover_url.is_none()
            && let Ok(Ok(url)) = tokio::time::timeout(
                Duration::from_secs(10),
                source_mgr.get_cover_url(&resolved_song),
            )
            .await
        {
            resolved_song.cover_url = Some(url);
        }
        if !set_current_song_if_current(
            &current_song,
            &play_request_id,
            request_id,
            resolved_song.clone(),
        ) {
            return;
        }
        let playing_message = format!(
            "{} - {} [{}]",
            resolved_song.name,
            resolved_song.singer,
            resolved_song.source.as_str()
        );
        let playing_title = format!("正在播放: {}", resolved_song.name);
        let _ = tx.send(AppAction::ShowNotification(
            Notification::info(playing_message.clone())
                .with_title(playing_title.clone())
                .tui_only(),
        ));

        // - 解析结果无封面时不加载
        // - 解析后的封面地址跟队列里的相同时不再重复加载
        if show_cover
            && resolved_song.cover_url.is_some()
            && resolved_song.cover_url != initial_cover
        {
            if play_request_id.load(Ordering::SeqCst) != request_id {
                return;
            }
            if let Err(error) = cover_service.load(resolved_song.cover_url.clone()).await {
                tracing::debug!("load cover failed: {}", error);
            }
            if play_request_id.load(Ordering::SeqCst) != request_id {
                return;
            }
            let _ = tx.send(AppAction::None);
        }

        let notification_icon = if album_cover_notification {
            match cover_service
                .cache_path(resolved_song.cover_url.clone())
                .await
            {
                Ok(path) => path,
                Err(error) => {
                    tracing::debug!("cache notification cover failed: {error}");
                    None
                }
            }
        } else {
            None
        };
        if play_request_id.load(Ordering::SeqCst) != request_id {
            return;
        }
        if track_change_notification {
            let mut notification = Notification::info(playing_message)
                .with_title(playing_title)
                .replacing_previous()
                .desktop_only();
            if let Some(icon) = notification_icon {
                notification = notification.with_icon(icon);
            }
            let _ = tx.send(AppAction::ShowNotification(notification));
        }
    });
}

fn next_play_request(ctx: &AppContext) -> u64 {
    let _song_guard = ctx.current_song.write().unwrap_or_else(|e| e.into_inner());
    ctx.play_request_id.fetch_add(1, Ordering::SeqCst) + 1
}

fn set_current_song_if_current(
    current_song: &std::sync::RwLock<Option<SongInfo>>,
    play_request_id: &AtomicU64,
    request_id: u64,
    song: SongInfo,
) -> bool {
    let mut current = current_song.write().unwrap_or_else(|e| e.into_inner());
    if play_request_id.load(Ordering::SeqCst) != request_id {
        return false;
    }
    *current = Some(song);
    true
}

fn should_expand_bili_parts(song: &SongInfo) -> bool {
    song.source == SourceId::Bili
        && !song.extra.contains_key("page")
        && !song.extra.contains_key("bili_parts_checked")
}

fn mark_bili_parts_checked(song: &mut SongInfo) {
    song.extra
        .insert("bili_parts_checked".to_string(), "true".to_string());
}

struct PlaybackResolveRequest {
    play_request_id: Arc<AtomicU64>,
    attempted_sources: Arc<std::sync::Mutex<std::collections::HashSet<SourceId>>>,
    js_source_index: Arc<std::sync::Mutex<Option<usize>>>,
    request_id: u64,
}

async fn resolve_playable_song(
    source_manager: Arc<lx_source::manager::SourceManager>,
    song: SongInfo,
    quality: Quality,
    auto_toggle: bool,
    request: PlaybackResolveRequest,
) -> Result<Option<(SongInfo, SongUrl)>, String> {
    let PlaybackResolveRequest {
        play_request_id,
        attempted_sources,
        js_source_index,
        request_id,
    } = request;
    let next_js_source_index = js_source_index
        .lock()
        .unwrap()
        .and_then(|index| index.checked_add(1));
    let retrying_next_js_source = next_js_source_index.is_some();
    if play_request_id.load(Ordering::SeqCst) != request_id {
        return Ok(None);
    }
    let direct_error = if retrying_next_js_source
        || mark_source_attempted(
            &attempted_sources,
            &play_request_id,
            request_id,
            song.source,
        ) {
        match resolve_song_url(
            Arc::clone(&source_manager),
            &song,
            quality,
            next_js_source_index.unwrap_or(0),
        )
        .await
        {
            Ok((url, resolved_js_source_index)) => {
                if play_request_id.load(Ordering::SeqCst) != request_id {
                    return Ok(None);
                }
                *js_source_index.lock().unwrap_or_else(|e| e.into_inner()) =
                    resolved_js_source_index;
                return Ok(Some((song, url)));
            }
            Err(error) => error,
        }
    } else {
        format!("音源 {} 已尝试", song.source.as_str())
    };

    if play_request_id.load(Ordering::SeqCst) != request_id {
        return Ok(None);
    }
    if !auto_toggle {
        return Err(format!("获取播放地址失败: {}", direct_error));
    }

    let candidates = source_manager.find_music(&song).await;
    if play_request_id.load(Ordering::SeqCst) != request_id {
        return Ok(None);
    }

    for candidate in candidates {
        if !mark_source_attempted(
            &attempted_sources,
            &play_request_id,
            request_id,
            candidate.source,
        ) {
            continue;
        }
        match resolve_song_url(Arc::clone(&source_manager), &candidate, quality, 0).await {
            Ok((url, resolved_js_source_index)) => {
                if play_request_id.load(Ordering::SeqCst) != request_id {
                    return Ok(None);
                }
                *js_source_index.lock().unwrap_or_else(|e| e.into_inner()) =
                    resolved_js_source_index;
                return Ok(Some((candidate, url)));
            }
            Err(error) => {
                tracing::debug!(
                    "toggle source failed for {} [{}]: {}",
                    candidate.name,
                    candidate.source.as_str(),
                    error
                );
            }
        }
        if play_request_id.load(Ordering::SeqCst) != request_id {
            return Ok(None);
        }
    }

    Err(format!(
        "获取播放地址失败，换源后仍不可用: {}",
        direct_error
    ))
}

async fn resolve_song_url(
    source_manager: Arc<lx_source::manager::SourceManager>,
    song: &SongInfo,
    quality: Quality,
    js_start_index: usize,
) -> Result<(SongUrl, Option<usize>), String> {
    source_manager
        .get_song_url_from_js_index(song, quality, js_start_index)
        .await
        .map_err(|error| error.to_string())
}

/// 标记音源已尝试。持锁校验请求代次：换歌后旧任务不得把过期源写进
/// 新请求的集合（否则新歌会“未试先败”）。对过期任务返回 true，让它
/// 跳过无谓的解析并在随后的代次校验处终止。
fn mark_source_attempted(
    attempted_sources: &std::sync::Mutex<std::collections::HashSet<SourceId>>,
    play_request_id: &AtomicU64,
    request_id: u64,
    source: SourceId,
) -> bool {
    let mut attempted = attempted_sources.lock().unwrap_or_else(|e| e.into_inner());
    if play_request_id.load(Ordering::SeqCst) != request_id {
        return true;
    }
    attempted.insert(source)
}

fn prepare_player(ctx: &AppContext) -> u64 {
    let generation = ctx.player.prepare();
    ctx.active_player_generation
        .store(generation, Ordering::SeqCst);
    generation
}

fn spawn_js_source_loader(
    urls: Vec<String>,
    default_source: String,
    source_manager: Arc<lx_source::manager::SourceManager>,
    generation: u64,
    tx: mpsc::UnboundedSender<AppAction>,
    rt: &tokio::runtime::Runtime,
) {
    let urls: Vec<String> = urls
        .into_iter()
        .filter(|url| !url.trim().is_empty())
        .collect();
    if urls.is_empty() {
        source_manager.clear_js_source_if_current(generation);
        return;
    }

    rt.spawn(async move {
        let total = urls.len();
        let mut loaded =
            Vec::<(String, Arc<dyn lx_core::traits::source::MusicSource>)>::with_capacity(total);
        let mut errors = Vec::new();
        for url in urls {
            match lx_source::js::loader::load_source(&url, &default_source).await {
                Ok(source) => {
                    loaded.push((url.clone(), Arc::new(source)));
                }
                Err(error) => {
                    if !source_manager.is_js_source_request_current(generation) {
                        return;
                    }
                    tracing::warn!("load JS source failed ({}): {}", url, error);
                    errors.push(format!("{url}: {error}"));
                }
            }
        }

        let loaded_count = loaded.len();
        if !source_manager.set_named_js_sources_if_current(generation, loaded) {
            return;
        }
        if loaded_count == 0 {
            let _ = tx.send(AppAction::ShowNotification(Notification::error(format!(
                "没有可用的 JS 音源: {}",
                errors.join("; ")
            ))));
        } else if errors.is_empty() {
            let _ = tx.send(AppAction::ShowNotification(Notification::success(format!(
                "{} 个 JS 音源已就绪",
                loaded_count
            ))));
        } else {
            let _ = tx.send(AppAction::ShowNotification(Notification::warning(format!(
                "已加载 {loaded_count}/{total} 个 JS 音源"
            ))));
        }
    });
}

fn next_generation(sequence: &AtomicU64) -> u64 {
    sequence.fetch_add(1, Ordering::SeqCst) + 1
}

fn playback_restore_flags(state: SavedPlayerState) -> (bool, bool) {
    match state {
        SavedPlayerState::Playing => (true, false),
        SavedPlayerState::Paused => (true, true),
        SavedPlayerState::Stopped => (false, false),
    }
}

fn should_scan_local_music_on_entry(
    previous_tab: NavTab,
    active_tab: NavTab,
    enabled: bool,
    has_paths: bool,
    songs_empty: bool,
    is_scanning: bool,
) -> bool {
    previous_tab != NavTab::LocalMusic
        && active_tab == NavTab::LocalMusic
        && enabled
        && has_paths
        && songs_empty
        && !is_scanning
}

fn should_retransmit_cover(since_last_redraw: Duration) -> bool {
    since_last_redraw >= COVER_REDRAW_THROTTLE
}

/// 把封面重新传输给终端，并强制下一帧全量重绘
fn retransmit_cover(
    terminal: &mut DefaultTerminal,
    main_page: &mut pages::main_page::MainPage,
) -> anyhow::Result<()> {
    if !main_page.refresh_cover_font_size() {
        main_page.force_cover_reload();
    }
    // detach 期间照常渲染，缓冲区内容不变，不清屏则不会重发任何序列
    //
    // 此处不能用 Terminal::clear，它会先发 ESC[6n 读回光标位置。ratatui-image
    // 的启动探测把 ESC[5n 包在 tmux passthrough 里发给外层终端，外层终端不应答时，
    // 它读 stdin 的线程会一直留存，抢走后续所有终端应答
    let size = terminal.size()?;
    terminal.resize(Rect::new(0, 0, size.width, size.height))?;
    Ok(())
}

fn previous_list_index(selected: usize, len: usize, wrap: bool) -> usize {
    match (selected, len, wrap) {
        (_, 0, _) => 0,
        (0, len, true) => len - 1,
        _ => selected.saturating_sub(1).min(len - 1),
    }
}

fn next_list_index(selected: usize, len: usize, wrap: bool) -> usize {
    match len {
        0 => 0,
        _ if selected + 1 < len => selected + 1,
        _ if wrap => 0,
        _ => len - 1,
    }
}

/// 异步搜索（直接 async，不用 spawn_blocking——reqwest 是真正 async 的）
#[allow(clippy::too_many_arguments)]
fn spawn_search(
    keyword: String,
    page: u32,
    append: bool,
    source: Option<lx_core::model::source::SourceId>,
    search_page: Arc<std::sync::Mutex<pages::search::SearchPage>>,
    source_manager: Arc<lx_source::manager::SourceManager>,
    tx: mpsc::UnboundedSender<AppAction>,
    rt: &tokio::runtime::Runtime,
    seq: Arc<AtomicU64>,
) {
    let my_seq = seq.fetch_add(1, Ordering::SeqCst);
    rt.spawn(async move {
        let result = tokio::time::timeout(
            Duration::from_secs(12),
            source_manager.search_scoped(&keyword, page, 30, source),
        )
        .await;
        match result {
            Ok(Ok(search_result)) => {
                if seq.load(Ordering::SeqCst) != my_seq + 1 {
                    return;
                }
                let mut sp = search_page.lock().unwrap_or_else(|e| e.into_inner());
                sp.update_results(keyword, page, append, search_result, source);
                let _ = tx.send(AppAction::None);
            }
            Ok(Err(error)) => {
                if seq.load(Ordering::SeqCst) != my_seq + 1 {
                    return;
                }
                let mut sp = search_page.lock().unwrap_or_else(|e| e.into_inner());
                sp.update_error(error.to_string());
                let _ = tx.send(AppAction::ShowNotification(Notification::error(format!(
                    "搜索失败: {}",
                    error
                ))));
            }
            Err(_) => {
                if seq.load(Ordering::SeqCst) != my_seq + 1 {
                    return;
                }
                let mut sp = search_page.lock().unwrap_or_else(|e| e.into_inner());
                sp.update_error("请求超时，请稍后重试".to_string());
                let _ = tx.send(AppAction::ShowNotification(Notification::error(
                    "搜索超时，请稍后重试".to_string(),
                )));
            }
        }
    });
}

/// 解析分享链接并把结果填进搜索结果页。
///
/// 与 `spawn_search` 共用同一个请求代次：粘贴链接和输入关键词互相取消，
/// 不会出现旧请求覆盖新结果的竞态。
fn spawn_link_parse(
    link: String,
    search_page: Arc<std::sync::Mutex<pages::search::SearchPage>>,
    source_manager: Arc<lx_source::manager::SourceManager>,
    tx: mpsc::UnboundedSender<AppAction>,
    rt: &tokio::runtime::Runtime,
    seq: Arc<AtomicU64>,
) {
    let my_seq = seq.fetch_add(1, Ordering::SeqCst);
    rt.spawn(async move {
        let result =
            tokio::time::timeout(Duration::from_secs(20), source_manager.parse_link(&link)).await;
        match result {
            Ok(Ok((source_id, parsed))) => {
                if seq.load(Ordering::SeqCst) != my_seq + 1 {
                    return;
                }
                let (items, note) = match parsed {
                    lx_core::traits::source::ParsedLink::Song(song) => (vec![*song], None),
                    lx_core::traits::source::ParsedLink::Playlist { playlist, songs } => {
                        let note = format!("已解析歌单「{}」共 {} 首", playlist.name, songs.len());
                        (songs, Some(note))
                    }
                    lx_core::traits::source::ParsedLink::Album { playlist, songs } => {
                        let note = format!("已解析专辑「{}」共 {} 首", playlist.name, songs.len());
                        (songs, Some(note))
                    }
                };
                let mut sp = search_page.lock().unwrap_or_else(|e| e.into_inner());
                sp.update_results(
                    link.clone(),
                    1,
                    false,
                    lx_core::traits::source::SearchResult {
                        total: items.len() as u32,
                        has_more: false,
                        items,
                    },
                    Some(source_id),
                );
                drop(sp);
                if let Some(note) = note {
                    let _ = tx.send(AppAction::ShowNotification(Notification::success(note)));
                }
                let _ = tx.send(AppAction::None);
            }
            Ok(Err(error)) => {
                if seq.load(Ordering::SeqCst) != my_seq + 1 {
                    return;
                }
                let mut sp = search_page.lock().unwrap_or_else(|e| e.into_inner());
                sp.update_error(error.clone());
                let _ = tx.send(AppAction::ShowNotification(Notification::error(error)));
            }
            Err(_) => {
                if seq.load(Ordering::SeqCst) != my_seq + 1 {
                    return;
                }
                let mut sp = search_page.lock().unwrap_or_else(|e| e.into_inner());
                sp.update_error("链接解析超时，请稍后重试".to_string());
                let _ = tx.send(AppAction::ShowNotification(Notification::error(
                    "链接解析超时，请稍后重试".to_string(),
                )));
            }
        }
    });
}

fn maybe_spawn_leaderboard_load(
    leaderboard: &mut pages::leaderboard::LeaderboardPage,
    request_id: &mut u64,
    source_manager: Arc<lx_source::manager::SourceManager>,
    leaderboard_tx: mpsc::UnboundedSender<LeaderboardResponse>,
    rt: &tokio::runtime::Runtime,
) {
    let Some(request) = leaderboard.next_load_request() else {
        return;
    };
    leaderboard.begin_loading(&request);
    *request_id = request_id.wrapping_add(1);
    spawn_leaderboard_request(*request_id, request, source_manager, leaderboard_tx, rt);
}

/// 异步加载排行榜目录或歌曲。
fn spawn_leaderboard_request(
    request_id: u64,
    request: pages::leaderboard::LeaderboardLoadRequest,
    source_manager: Arc<lx_source::manager::SourceManager>,
    leaderboard_tx: mpsc::UnboundedSender<LeaderboardResponse>,
    rt: &tokio::runtime::Runtime,
) {
    rt.spawn(async move {
        let response = match request {
            pages::leaderboard::LeaderboardLoadRequest::Boards { source } => {
                let result = tokio::time::timeout(
                    Duration::from_secs(12),
                    source_manager.leaderboard_boards(source),
                )
                .await;
                LeaderboardResponse::Boards {
                    request_id,
                    source,
                    result: match result {
                        Ok(Ok(boards)) => Ok(boards),
                        Ok(Err(error)) => Err(error.to_string()),
                        Err(_) => Err("请求超时，请稍后重试".to_string()),
                    },
                }
            }
            pages::leaderboard::LeaderboardLoadRequest::Songs { source, board_id } => {
                let result = tokio::time::timeout(
                    Duration::from_secs(12),
                    source_manager.leaderboard(source, &board_id, 1, 300),
                )
                .await;
                LeaderboardResponse::Songs {
                    request_id,
                    source,
                    board_id,
                    result: match result {
                        Ok(Ok(search_result)) => Ok(search_result.items),
                        Ok(Err(error)) => Err(error.to_string()),
                        Err(_) => Err("请求超时，请稍后重试".to_string()),
                    },
                }
            }
        };
        let _ = leaderboard_tx.send(response);
    });
}

fn maybe_spawn_playlist_load(
    playlists: &mut pages::playlists::PlaylistsPage,
    request_id: &mut u64,
    source_manager: Arc<lx_source::manager::SourceManager>,
    playlist_tx: mpsc::UnboundedSender<PlaylistResponse>,
    rt: &tokio::runtime::Runtime,
) {
    let Some(request) = playlists.next_load_request() else {
        return;
    };
    playlists.begin_loading(&request);
    *request_id = request_id.wrapping_add(1);
    spawn_playlist_request(*request_id, request, source_manager, playlist_tx, rt);
}

fn spawn_playlist_request(
    request_id: u64,
    request: pages::playlists::PlaylistLoadRequest,
    source_manager: Arc<lx_source::manager::SourceManager>,
    playlist_tx: mpsc::UnboundedSender<PlaylistResponse>,
    rt: &tokio::runtime::Runtime,
) {
    rt.spawn(async move {
        let response = match request {
            pages::playlists::PlaylistLoadRequest::List {
                source,
                category,
                page,
                append,
            } => {
                let result = tokio::time::timeout(
                    Duration::from_secs(12),
                    source_manager.playlists(source, &category, page),
                )
                .await;
                PlaylistResponse::List {
                    request_id,
                    source,
                    page,
                    append,
                    result: match result {
                        Ok(Ok(playlists)) => Ok(playlists),
                        Ok(Err(error)) => Err(error.to_string()),
                        Err(_) => Err("请求超时，请稍后重试".to_string()),
                    },
                }
            }
            pages::playlists::PlaylistLoadRequest::Search {
                source,
                keyword,
                page,
                append,
            } => {
                let result = tokio::time::timeout(
                    Duration::from_secs(15),
                    source_manager.search_playlists(source, &keyword, page),
                )
                .await;
                PlaylistResponse::Search {
                    request_id,
                    source,
                    keyword,
                    page,
                    append,
                    result: match result {
                        Ok(Ok(items)) => Ok(items),
                        Ok(Err(error)) => Err(error.to_string()),
                        Err(_) => Err("歌单搜索超时，已回退热门歌单".to_string()),
                    },
                }
            }
            pages::playlists::PlaylistLoadRequest::Songs {
                source,
                playlist_id,
            } => {
                let timeout = if source == SourceId::Bili {
                    Duration::from_secs(45)
                } else {
                    Duration::from_secs(15)
                };
                let result = tokio::time::timeout(
                    timeout,
                    source_manager.playlist_detail(source, &playlist_id, 1),
                )
                .await;
                PlaylistResponse::Songs {
                    request_id,
                    source,
                    playlist_id,
                    result: match result {
                        Ok(Ok(songs)) => Ok(songs),
                        Ok(Err(error)) => Err(error.to_string()),
                        Err(_) => Err("请求超时，请稍后重试".to_string()),
                    },
                }
            }
            pages::playlists::PlaylistLoadRequest::Categories { source } => {
                let result = tokio::time::timeout(
                    Duration::from_secs(12),
                    source_manager.playlist_categories(source),
                )
                .await;
                PlaylistResponse::Categories {
                    request_id,
                    source,
                    result: match result {
                        Ok(Ok(categories)) => Ok(categories),
                        Ok(Err(error)) => Err(error.to_string()),
                        Err(_) => Err("请求超时，请稍后重试".to_string()),
                    },
                }
            }
            pages::playlists::PlaylistLoadRequest::User {
                source,
                page,
                append,
            } => {
                let result = tokio::time::timeout(
                    Duration::from_secs(12),
                    source_manager.user_playlists(source, page, 30),
                )
                .await;
                PlaylistResponse::User {
                    request_id,
                    source,
                    page,
                    append,
                    result: match result {
                        Ok(Ok(playlists)) => Ok(playlists),
                        Ok(Err(error)) => Err(error.to_string()),
                        Err(_) => Err("请求超时，请稍后重试".to_string()),
                    },
                }
            }
        };
        let _ = playlist_tx.send(response);
    });
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use lx_core::model::song::SongInfo;
    use lx_core::model::source::SourceId;

    use super::{
        DeleteConfirmationAction, delete_confirmation_action, next_list_index,
        playback_restore_flags, previous_list_index, should_expand_bili_parts, should_go_to_main,
        should_scan_local_music_on_entry,
    };
    use crate::pages::sidebar::NavTab;
    use crate::storage::SavedPlayerState;

    #[test]
    fn local_list_navigation_wraps_at_both_ends() {
        assert_eq!(previous_list_index(0, 4, true), 3);
        assert_eq!(next_list_index(3, 4, true), 0);
        assert_eq!(previous_list_index(0, 4, false), 0);
        assert_eq!(next_list_index(3, 4, false), 3);
    }

    #[test]
    fn stopped_sessions_restore_the_queue_without_loading_libmpv_media() {
        assert_eq!(
            playback_restore_flags(SavedPlayerState::Playing),
            (true, false)
        );
        assert_eq!(
            playback_restore_flags(SavedPlayerState::Paused),
            (true, true)
        );
        assert_eq!(
            playback_restore_flags(SavedPlayerState::Stopped),
            (false, false)
        );
    }

    #[test]
    fn local_delete_confirmation_accepts_terminal_shift_variants() {
        for key in [
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('Y'), KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Char('Y'), KeyModifiers::NONE),
        ] {
            assert_eq!(
                delete_confirmation_action(&key),
                DeleteConfirmationAction::Confirm
            );
        }

        for key in [
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        ] {
            assert_eq!(
                delete_confirmation_action(&key),
                DeleteConfirmationAction::Cancel
            );
        }
    }

    #[test]
    fn playlist_overlays_and_open_lists_receive_escape_before_global_navigation() {
        assert!(!should_go_to_main(NavTab::Playlists, true, false, false));
        assert!(!should_go_to_main(NavTab::Playlists, false, true, false));
        assert!(should_go_to_main(NavTab::Playlists, false, false, false));
    }

    #[test]
    fn entering_empty_local_music_page_starts_scan() {
        assert!(should_scan_local_music_on_entry(
            NavTab::History,
            NavTab::LocalMusic,
            true,
            true,
            true,
            false,
        ));
    }

    #[test]
    fn local_music_entry_does_not_start_invalid_or_duplicate_scan() {
        for (enabled, has_paths, songs_empty, is_scanning) in [
            (false, true, true, false),
            (true, false, true, false),
            (true, true, false, false),
            (true, true, true, true),
        ] {
            assert!(!should_scan_local_music_on_entry(
                NavTab::History,
                NavTab::LocalMusic,
                enabled,
                has_paths,
                songs_empty,
                is_scanning,
            ));
        }

        assert!(!should_scan_local_music_on_entry(
            NavTab::LocalMusic,
            NavTab::LocalMusic,
            true,
            true,
            true,
            false,
        ));
    }

    #[test]
    fn only_unresolved_bili_items_need_part_expansion() {
        let mut song = SongInfo::new(
            "BV1xx411c7mD".to_string(),
            SourceId::Bili,
            "测试视频".to_string(),
            "UP主".to_string(),
        );
        assert!(should_expand_bili_parts(&song));

        song.extra.insert("page".to_string(), "2".to_string());
        assert!(!should_expand_bili_parts(&song));

        let online_song = SongInfo::new(
            "1".to_string(),
            SourceId::Kw,
            "歌曲".to_string(),
            "歌手".to_string(),
        );
        assert!(!should_expand_bili_parts(&online_song));
    }
}
