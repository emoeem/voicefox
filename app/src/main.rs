//! voicefox: Rust TUI 版 lx-music-desktop
//!
//! 入口：CLI 解析 → 初始化 → 启动 TUI

mod cli;
mod config;
mod context;
mod cover;
mod pages;
mod playlist;
mod storage;
mod theme;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use std::{fs::OpenOptions, path::Path};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use lx_core::events::{AppAction, Notification};
use lx_core::model::song::SongInfo;
use lx_core::model::source::Quality;
use lx_core::traits::player::PlayerEvent;
use lx_core::traits::source::SongUrl;
use ratatui::DefaultTerminal;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::Style;
use tokio::sync::mpsc;

use context::AppContext;
use pages::components;
use pages::sidebar::NavTab;

struct LeaderboardResponse {
    request_id: u64,
    board_index: usize,
    result: Result<Vec<lx_core::model::song::SongInfo>, String>,
}

#[derive(Debug, Default, Clone, Copy)]
struct UiAreas {
    tabs: Rect,
    content: Rect,
    progress: Rect,
}

#[derive(Debug, Default)]
struct ClickTracker {
    last_left_click: Option<(Instant, u16, u16)>,
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

fn main() -> anyhow::Result<()> {
    // 解析 CLI
    let cli = cli::Cli::parse();

    // 加载配置
    let (cfg, config_path) = config::loader::load(&cli.config)?;
    init_logging(&cli.log_level, &config_path);

    // 构建 tokio runtime（多线程）
    let rt = tokio::runtime::Runtime::new()?;

    // 初始化 AppContext
    let ctx = rt.block_on(AppContext::new(cfg, config_path))?;

    // 验证 mpv 是否可用
    if std::process::Command::new("mpv")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("⚠ 警告: 未找到 mpv，音频播放功能不可用");
        eprintln!("请安装 mpv: sudo apt install mpv (或 brew install mpv)");
    }

    // 启动 TUI
    let mut terminal = ratatui::init();
    let mouse_enabled = ctx.config.read().unwrap().ui.enable_mouse;
    if mouse_enabled {
        let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);
    }

    // 安装 crossterm panic hook，确保 panic 时 restore 终端
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if mouse_enabled {
            let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
        }
        ratatui::restore();
        original_hook(info);
    }));

    let result = run_app(&mut terminal, ctx, rt);
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();

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
        format!("lx_tui={level},lx_source={level},lx_player={level},lx_lyric={level}");
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .or_else(|_| tracing_subscriber::EnvFilter::try_new(default_filter))
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(writer)
        .try_init();
}

#[allow(unused_assignments)]
fn run_app(
    terminal: &mut DefaultTerminal,
    ctx: AppContext,
    rt: tokio::runtime::Runtime,
) -> anyhow::Result<()> {
    let (action_tx, mut action_rx) = mpsc::unbounded_channel::<AppAction>();
    let (leaderboard_tx, mut leaderboard_rx) = mpsc::unbounded_channel::<LeaderboardResponse>();
    let mut player_event_rx = ctx.player.take_event_receiver();

    // 搜索请求序列号（用于取消过时请求）
    let search_seq: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    let mut leaderboard_request_id: u64 = 0;

    // 导航状态
    let mut active_tab = NavTab::Main;

    // 页面状态
    let (search_source_filter, wrap_navigation, scroll_amount) = {
        let config = ctx.config.read().unwrap();
        (
            if config.ui.aggregate_search {
                None
            } else {
                Some(config.source.default)
            },
            config.ui.wrap_navigation,
            config.ui.scroll_amount,
        )
    };
    let search_page = Arc::new(std::sync::Mutex::new(pages::search::SearchPage::new(
        search_source_filter,
        wrap_navigation,
        scroll_amount,
    )));
    let settings_page = Arc::new(std::sync::Mutex::new(pages::settings::SettingsPage::new()));
    let mut main_page = pages::main_page::MainPage::new();
    let mut leaderboard = pages::leaderboard::LeaderboardPage::new();
    let mut favorites_page = pages::favorites::FavoritesPage::new();
    let mut history_selected: usize = 0;
    let mut history_scroll: usize = 0;
    let mut ui_areas = UiAreas::default();
    let mut click_tracker = ClickTracker::default();

    // 事件驱动渲染：借鉴 rmpc，只在有事件或需要渲染时才 draw()
    let mut needs_render = true;
    let max_fps = ctx.config.read().unwrap().ui.max_fps.clamp(1, 60);
    let render_interval = Duration::from_millis(1_000 / u64::from(max_fps));
    let mut last_periodic_render = Instant::now();
    let mut last_notification_cleanup = Instant::now();
    let mut mouse_capture_enabled = ctx.config.read().unwrap().ui.enable_mouse;

    // === 后台异步加载 JS 音源（不阻塞启动） ===
    let js_urls = ctx.config.read().unwrap().source.js_sources.clone();
    let default_source = ctx
        .config
        .read()
        .unwrap()
        .source
        .default
        .as_str()
        .to_string();
    spawn_js_source_loader(
        js_urls,
        default_source,
        Arc::clone(&ctx.source_manager),
        action_tx.clone(),
        &rt,
    );

    // === 初始扫描本地音乐 ===
    let local_music_paths = ctx.config.read().unwrap().local_music.paths.clone();
    let local_music_max_depth = ctx.config.read().unwrap().local_music.max_depth;
    if !local_music_paths.is_empty() && ctx.config.read().unwrap().local_music.enabled {
        let _errors = ctx.source_manager.local_source().scan(&local_music_paths, local_music_max_depth);
    }

    loop {
        let mouse_requested = ctx.config.read().unwrap().ui.enable_mouse;
        if mouse_requested != mouse_capture_enabled {
            if mouse_requested {
                let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);
            } else {
                let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
            }
            mouse_capture_enabled = mouse_requested;
        }

        // === 0. 排空异步 action ===
        while let Ok(action) = action_rx.try_recv() {
            execute_action(
                action,
                &ctx,
                &rt,
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
                    PlayerEvent::Ended => {
                        if let Some((songs, index)) = ctx.playlist.next_entry() {
                            execute_action(
                                AppAction::PlaySong { songs, index },
                                &ctx,
                                &rt,
                                &action_tx,
                                &search_page,
                                &settings_page,
                                &search_seq,
                            );
                        }
                    }
                    PlayerEvent::Error(error) => {
                        ctx.notifications
                            .write()
                            .unwrap()
                            .push_back(Notification::error(format!("播放器错误: {}", error)));
                    }
                    PlayerEvent::Buffering(_) => {}
                }
                needs_render = true;
            }
        }
        // 排行榜异步结果
        while let Ok(response) = leaderboard_rx.try_recv() {
            if response.request_id != leaderboard_request_id
                || leaderboard.selected_board != Some(response.board_index)
            {
                continue;
            }
            match response.result {
                Ok(songs) => leaderboard.update_songs(songs),
                Err(error) => {
                    leaderboard.update_error(error.clone());
                    ctx.notifications
                        .write()
                        .unwrap()
                        .push_back(Notification::error(format!("加载榜单失败: {}", error)));
                }
            }
            needs_render = true;
        }

        // === 1. 周期维护 ===
        // 这些工作必须独立于终端事件执行，否则持续按键或拖动鼠标会让
        // 搜索防抖、歌词同步和进度渲染长期得不到运行机会。
        if active_tab == NavTab::Search {
            let action = {
                let mut sp = search_page.lock().unwrap();
                sp.tick()
            };
            if let Some(action) = action {
                execute_action(
                    action,
                    &ctx,
                    &rt,
                    &action_tx,
                    &search_page,
                    &settings_page,
                    &search_seq,
                );
                needs_render = true;
            }
        }

        if last_notification_cleanup.elapsed() >= Duration::from_millis(250) {
            let mut notifs = ctx.notifications.write().unwrap();
            let previous_len = notifs.len();
            notifs.retain(|notification| !notification.is_expired(Duration::from_secs(5)));
            if notifs.len() != previous_len {
                needs_render = true;
            }
            last_notification_cleanup = Instant::now();
        }

        if last_periodic_render.elapsed() >= render_interval {
            ctx.lyric_service.update_position(*ctx.position.borrow());
            let state = *ctx.player_state.borrow();
            let input_active = active_tab == NavTab::Search
                && search_page.lock().unwrap().input_mode
                || active_tab == NavTab::Settings && settings_page.lock().unwrap().input_mode
                || active_tab == NavTab::Favorites && favorites_page.input_mode();
            let notification_active = !ctx.notifications.read().unwrap().is_empty();
            needs_render |= matches!(
                state,
                lx_core::model::source::PlayerState::Playing
                    | lx_core::model::source::PlayerState::Loading
            ) || input_active
                || notification_active;
            last_periodic_render = Instant::now();
        }

        // 在读取下一个事件前先补画上一轮状态。这样即使 key repeat 每轮都
        // 触发 continue，界面也不会被连续输入饿死。
        if needs_render {
            draw_app(
                terminal,
                &ctx,
                active_tab,
                &search_page,
                &settings_page,
                &mut main_page,
                &mut leaderboard,
                &mut favorites_page,
                &mut history_selected,
                &mut history_scroll,
                &mut ui_areas,
            )?;
            needs_render = false;
        }

        // === 2. 事件驱动：轮询终端事件 ===
        let terminal_event = if event::poll(Duration::from_millis(50)).unwrap_or(false) {
            event::read().ok()
        } else {
            None
        };
        if let Some(Event::Key(key)) = terminal_event.as_ref()
            && key.kind == KeyEventKind::Press
        {
            let key = *key;
            // 1a. 侧边栏全局快捷键 (/, 1-5) — 输入模式下跳过
            let settings_input_mode =
                active_tab == NavTab::Settings && settings_page.lock().unwrap().input_mode;
            let search_input_mode =
                active_tab == NavTab::Search && search_page.lock().unwrap().input_mode;
            let favorites_input_mode =
                active_tab == NavTab::Favorites && favorites_page.input_mode();
            let text_input_active =
                settings_input_mode || search_input_mode || favorites_input_mode;
            let favorites_filter_key =
                active_tab == NavTab::Favorites && matches!(key.code, KeyCode::Char('/'));
            if !text_input_active
                && !favorites_filter_key
                && let Some(tab) = pages::sidebar::handle_input(&key)
            {
                active_tab = tab;
                needs_render = true;
                continue;
            }

            // 1b. 全局快捷键
            match (key.modifiers, key.code) {
                (KeyModifiers::NONE, KeyCode::Char('q')) if !text_input_active => return Ok(()),
                (KeyModifiers::NONE, KeyCode::Char(' ')) if !text_input_active => {
                    ctx.player.toggle();
                    needs_render = true;
                    continue;
                }
                (KeyModifiers::NONE, KeyCode::Char('n')) if !text_input_active => {
                    if let Some((songs, index)) = ctx.playlist.next_manual_entry() {
                        execute_action(
                            AppAction::PlaySong { songs, index },
                            &ctx,
                            &rt,
                            &action_tx,
                            &search_page,
                            &settings_page,
                            &search_seq,
                        );
                    }
                    needs_render = true;
                    continue;
                }
                (KeyModifiers::SHIFT, KeyCode::Char('>')) if !text_input_active => {
                    if let Some((songs, index)) = ctx.playlist.next_manual_entry() {
                        execute_action(
                            AppAction::PlaySong { songs, index },
                            &ctx,
                            &rt,
                            &action_tx,
                            &search_page,
                            &settings_page,
                            &search_seq,
                        );
                    }
                    needs_render = true;
                    continue;
                }
                (KeyModifiers::NONE, KeyCode::Char('b')) if !text_input_active => {
                    if let Some((songs, index)) = ctx.playlist.prev_manual_entry() {
                        execute_action(
                            AppAction::PlaySong { songs, index },
                            &ctx,
                            &rt,
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
                    if let Some((songs, index)) = ctx.playlist.prev_manual_entry() {
                        execute_action(
                            AppAction::PlaySong { songs, index },
                            &ctx,
                            &rt,
                            &action_tx,
                            &search_page,
                            &settings_page,
                            &search_seq,
                        );
                    }
                    needs_render = true;
                    continue;
                }
                (KeyModifiers::NONE, KeyCode::Char('m')) if !text_input_active => {
                    let mode = ctx.playlist.cycle_mode();
                    let save_result = {
                        let mut config = ctx.config.write().unwrap();
                        config.player.play_mode = mode.as_config().to_string();
                        crate::config::loader::save(&config, &ctx.config_path)
                    };
                    let notification = match save_result {
                        Ok(()) => Notification::info(format!("播放模式: {}", mode.label())),
                        Err(error) => {
                            Notification::error(format!("播放模式已切换，但保存失败: {}", error))
                        }
                    };
                    ctx.notifications.write().unwrap().push_back(notification);
                    needs_render = true;
                    continue;
                }
                (KeyModifiers::NONE, KeyCode::Right)
                    if !text_input_active && active_tab != NavTab::Search =>
                {
                    let pos = *ctx.position.borrow();
                    ctx.player.seek(pos + Duration::from_secs(5));
                    needs_render = true;
                    continue;
                }
                (KeyModifiers::NONE, KeyCode::Left)
                    if !text_input_active && active_tab != NavTab::Search =>
                {
                    let pos = *ctx.position.borrow();
                    if pos > Duration::from_secs(5) {
                        ctx.player.seek(pos - Duration::from_secs(5));
                    } else {
                        ctx.player.seek(Duration::ZERO);
                    }
                    needs_render = true;
                    continue;
                }
                (KeyModifiers::NONE, KeyCode::Char(']')) if !text_input_active => {
                    let pos = *ctx.position.borrow();
                    ctx.player.seek(pos + Duration::from_secs(5));
                    needs_render = true;
                    continue;
                }
                (KeyModifiers::NONE, KeyCode::Char('[')) if !text_input_active => {
                    let pos = *ctx.position.borrow();
                    if pos > Duration::from_secs(5) {
                        ctx.player.seek(pos - Duration::from_secs(5));
                    } else {
                        ctx.player.seek(Duration::ZERO);
                    }
                    needs_render = true;
                    continue;
                }
                (KeyModifiers::NONE, KeyCode::Up) if active_tab == NavTab::Main => {
                    ctx.player.volume_up(5);
                    needs_render = true;
                    continue;
                }
                (KeyModifiers::NONE, KeyCode::Down) if active_tab == NavTab::Main => {
                    ctx.player.volume_down(5);
                    needs_render = true;
                    continue;
                }
                (KeyModifiers::NONE, KeyCode::Char('.')) if !text_input_active => {
                    ctx.player.volume_up(5);
                    needs_render = true;
                    continue;
                }
                (KeyModifiers::NONE, KeyCode::Char(',')) if !text_input_active => {
                    ctx.player.volume_down(5);
                    needs_render = true;
                    continue;
                }
                (KeyModifiers::NONE, KeyCode::Tab) if !text_input_active => {
                    active_tab = match active_tab {
                        NavTab::Main => NavTab::Search,
                        NavTab::Search => NavTab::Leaderboard,
                        NavTab::Leaderboard => NavTab::Favorites,
                        NavTab::Favorites => NavTab::History,
                        NavTab::History => NavTab::LocalMusic,
                        NavTab::LocalMusic => NavTab::Settings,
                        NavTab::Settings => NavTab::Main,
                    };
                    needs_render = true;
                    continue;
                }
                (KeyModifiers::SHIFT, KeyCode::BackTab) if !text_input_active => {
                    active_tab = match active_tab {
                        NavTab::Main => NavTab::Settings,
                        NavTab::Search => NavTab::Main,
                        NavTab::Leaderboard => NavTab::Search,
                        NavTab::Favorites => NavTab::Leaderboard,
                        NavTab::History => NavTab::Favorites,
                        NavTab::LocalMusic => NavTab::History,
                        NavTab::Settings => NavTab::LocalMusic,
                    };
                    needs_render = true;
                    continue;
                }
                (KeyModifiers::NONE, KeyCode::Esc)
                    if !settings_input_mode
                        && active_tab != NavTab::Main
                        && active_tab != NavTab::Search
                        && active_tab != NavTab::Favorites
                        && !(active_tab == NavTab::Leaderboard
                            && leaderboard.selected_board.is_some()) =>
                {
                    active_tab = NavTab::Main;
                    needs_render = true;
                    continue;
                }
                // Ctrl+L: 收藏/取消收藏当前歌曲
                (KeyModifiers::CONTROL, KeyCode::Char('l')) if !text_input_active => {
                    if let Some(song) = ctx.current_song.read().unwrap().as_ref() {
                        if ctx.storage.is_favorite(song) {
                            ctx.storage.remove_favorite(song);
                            let _ = action_tx.send(AppAction::ShowNotification(
                                Notification::info("已取消收藏"),
                            ));
                        } else {
                            ctx.storage.add_favorite(song);
                            let _ = action_tx.send(AppAction::ShowNotification(
                                Notification::info("已添加收藏"),
                            ));
                        }
                    }
                    needs_render = true;
                    continue;
                }
                _ => {}
            }

            // 1c. 路由到当前页面
            match active_tab {
                NavTab::Search => {
                    let action = {
                        let mut sp = search_page.lock().unwrap();
                        sp.handle_input(key)
                    };
                    if matches!(action, AppAction::GoBack) {
                        active_tab = NavTab::Main;
                        needs_render = true;
                        continue;
                    }
                    execute_action(
                        action,
                        &ctx,
                        &rt,
                        &action_tx,
                        &search_page,
                        &settings_page,
                        &search_seq,
                    );
                }
                NavTab::Main => {
                    let action = main_page.handle_input(&key, &ctx);
                    execute_action(
                        action,
                        &ctx,
                        &rt,
                        &action_tx,
                        &search_page,
                        &settings_page,
                        &search_seq,
                    );
                }
                NavTab::Leaderboard => {
                    let action = leaderboard.handle_input(&key, &ctx);
                    if let AppAction::PlaySong { songs: _, index: _ } = action.clone() {
                        execute_action(
                            action,
                            &ctx,
                            &rt,
                            &action_tx,
                            &search_page,
                            &settings_page,
                            &search_seq,
                        );
                    } else if action_is_none(&action) {
                        // 检查是否需要异步加载榜单歌曲
                        if !leaderboard.loading
                            && !leaderboard.loaded
                            && let Some(board_index) = leaderboard.selected_board
                        {
                            let (board_id, board_source) = leaderboard
                                .boards
                                .get(board_index)
                                .map(|board| (board.id.clone(), board.source))
                                .unwrap_or_else(|| {
                                    (String::new(), lx_core::model::source::SourceId::Kg)
                                });
                            leaderboard.begin_loading();
                            leaderboard_request_id = leaderboard_request_id.wrapping_add(1);
                            spawn_leaderboard_search(
                                leaderboard_request_id,
                                board_index,
                                board_id,
                                board_source,
                                Arc::clone(&ctx.source_manager),
                                leaderboard_tx.clone(),
                                &rt,
                            );
                        }
                    }
                }
                NavTab::Favorites => {
                    let action = favorites_page.handle_input(&key, &ctx);
                    if matches!(action, AppAction::GoBack) {
                        active_tab = NavTab::Main;
                        needs_render = true;
                        continue;
                    }
                    // 播放时自动切换到主页，让用户看到播放信息
                    if matches!(action, AppAction::PlaySong { .. }) {
                        active_tab = NavTab::Main;
                    }
                    execute_action(
                        action,
                        &ctx,
                        &rt,
                        &action_tx,
                        &search_page,
                        &settings_page,
                        &search_seq,
                    );
                }
                NavTab::History => {
                    let action = pages::history::handle_input(&key, &ctx, &mut history_selected);
                    // 播放时自动切换到主页，让用户看到播放信息
                    if matches!(action, AppAction::PlaySong { .. }) {
                        active_tab = NavTab::Main;
                    }
                    execute_action(
                        action,
                        &ctx,
                        &rt,
                        &action_tx,
                        &search_page,
                        &settings_page,
                        &search_seq,
                    );
                }
                NavTab::Settings => {
                    let action = {
                        let mut sp = settings_page.lock().unwrap();
                        sp.handle_input(key, &ctx)
                    };
                    if matches!(key.code, KeyCode::Char('g') | KeyCode::Char('w')) {
                        let config = ctx.config.read().unwrap();
                        search_page.lock().unwrap().set_preferences(
                            config.ui.aggregate_search,
                            config.source.default,
                            config.ui.wrap_navigation,
                            config.ui.scroll_amount,
                        );
                    }
                    execute_action(
                        action,
                        &ctx,
                        &rt,
                        &action_tx,
                        &search_page,
                        &settings_page,
                        &search_seq,
                    );
                }
                NavTab::LocalMusic => {
                    // 本地音乐：按 r 重新扫描
                    if matches!((key.modifiers, key.code), (KeyModifiers::NONE, KeyCode::Char('r'))) {
                        let paths = ctx.config.read().unwrap().local_music.paths.clone();
                        let max_depth = ctx.config.read().unwrap().local_music.max_depth;
                        let local_src = ctx.source_manager.local_source();
                        let errors = local_src.scan(&paths, max_depth);
                        if errors.is_empty() {
                            let count = local_src.all_songs().len();
                            ctx.notifications.write().unwrap().push_back(
                                Notification::info(format!("本地音乐扫描完成，共 {} 首", count)),
                            );
                        } else {
                            for err in &errors {
                                ctx.notifications.write().unwrap().push_back(
                                    Notification::error(err.clone()),
                                );
                            }
                        }
                        needs_render = true;
                    }
                }
            }
            needs_render = true;
        } else if let Some(Event::Mouse(mouse)) = terminal_event.as_ref() {
            let mouse = *mouse;
            let activate = click_tracker.is_double_click(mouse);
            let position = Position::new(mouse.column, mouse.row);
            if ui_areas.tabs.contains(position)
                && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            {
                if let Some(tab) = pages::sidebar::hit_test(ui_areas.tabs, position) {
                    active_tab = tab;
                    if tab == NavTab::Search {
                        search_page.lock().unwrap().input_mode = true;
                    }
                }
            } else if ui_areas.progress.contains(position)
                && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            {
                let duration = *ctx.duration.borrow();
                if !duration.is_zero() && ui_areas.progress.width > 0 {
                    let offset = mouse.column.saturating_sub(ui_areas.progress.x);
                    let ratio = f64::from(offset) / f64::from(ui_areas.progress.width);
                    ctx.player.seek(Duration::from_secs_f64(
                        duration.as_secs_f64() * ratio.clamp(0.0, 1.0),
                    ));
                }
            } else if ui_areas.content.contains(position) {
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
                    NavTab::Favorites => {
                        favorites_page.handle_mouse(mouse, ui_areas.content, &ctx, activate)
                    }
                    NavTab::History => pages::history::handle_mouse(
                        mouse,
                        ui_areas.content,
                        &ctx,
                        &mut history_selected,
                        history_scroll,
                        activate,
                    ),
                    NavTab::Settings => {
                        settings_page
                            .lock()
                            .unwrap()
                            .handle_mouse(mouse, ui_areas.content, &ctx)
                    }
                    NavTab::LocalMusic => AppAction::None,
                };

                if matches!(action, AppAction::PlaySong { .. })
                    && matches!(active_tab, NavTab::Favorites | NavTab::History)
                {
                    active_tab = NavTab::Main;
                }
                execute_action(
                    action,
                    &ctx,
                    &rt,
                    &action_tx,
                    &search_page,
                    &settings_page,
                    &search_seq,
                );

                if active_tab == NavTab::Leaderboard
                    && !leaderboard.loading
                    && leaderboard.selected_board.is_some()
                    && !leaderboard.loaded
                    && let Some(board_index) = leaderboard.selected_board
                {
                    let (board_id, board_source) = leaderboard
                        .boards
                        .get(board_index)
                        .map(|board| (board.id.clone(), board.source))
                        .unwrap_or_else(|| (String::new(), lx_core::model::source::SourceId::Kg));
                    leaderboard.begin_loading();
                    leaderboard_request_id = leaderboard_request_id.wrapping_add(1);
                    spawn_leaderboard_search(
                        leaderboard_request_id,
                        board_index,
                        board_id,
                        board_source,
                        Arc::clone(&ctx.source_manager),
                        leaderboard_tx.clone(),
                        &rt,
                    );
                }
            }
            needs_render = true;
        }

        // === 3. 当前事件未提前 continue 时立即渲染 ===
        if needs_render {
            draw_app(
                terminal,
                &ctx,
                active_tab,
                &search_page,
                &settings_page,
                &mut main_page,
                &mut leaderboard,
                &mut favorites_page,
                &mut history_selected,
                &mut history_scroll,
                &mut ui_areas,
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
    main_page: &mut pages::main_page::MainPage,
    leaderboard: &mut pages::leaderboard::LeaderboardPage,
    favorites_page: &mut pages::favorites::FavoritesPage,
    history_selected: &mut usize,
    history_scroll: &mut usize,
    ui_areas: &mut UiAreas,
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
        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4), // header
                Constraint::Length(3), // tabs
                Constraint::Min(3),    // tab content
                Constraint::Length(1), // progress bar
                Constraint::Length(1), // status bar
            ])
            .split(area);

        components::header::render(main_chunks[0], frame.buffer_mut(), ctx);
        pages::sidebar::render(main_chunks[1], frame.buffer_mut(), active_tab, ctx);
        let content_area = main_chunks[2];
        *ui_areas = UiAreas {
            tabs: main_chunks[1],
            content: content_area,
            progress: main_chunks[3],
        };

        match active_tab {
            NavTab::Search => {
                let mut sp = search_page.lock().unwrap();
                sp.render(content_area, frame.buffer_mut(), ctx);
            }
            NavTab::Main => {
                main_page.render(content_area, frame.buffer_mut(), ctx);
            }
            NavTab::Leaderboard => {
                leaderboard.render(content_area, frame.buffer_mut(), ctx);
            }
            NavTab::Favorites => {
                favorites_page.render(content_area, frame.buffer_mut(), ctx);
            }
            NavTab::History => {
                pages::history::render(
                    content_area,
                    frame.buffer_mut(),
                    ctx,
                    history_selected,
                    history_scroll,
                );
            }
            NavTab::Settings => {
                let mut sp = settings_page.lock().unwrap();
                sp.render(content_area, frame.buffer_mut(), ctx);
            }
            NavTab::LocalMusic => {
                // 显示本地音乐状态
                use ratatui::widgets::{Block, Borders, Paragraph, Widget};
                use ratatui::style::{Style, Color};
                use ratatui::text::{Line, Span};

                let local_src = ctx.source_manager.local_source();
                let paths = ctx.config.read().unwrap().local_music.paths.clone();
                let songs = local_src.all_songs();
                let loaded = local_src.loaded_paths();

                let mut lines = vec![
                    Line::from(Span::styled(
                        "📁 本地音乐",
                        Style::new().fg(Color::Cyan).add_modifier(ratatui::style::Modifier::BOLD),
                    )),
                    Line::from(""),
                ];

                if paths.is_empty() {
                    lines.push(Line::from(Span::styled(
                        "   未配置音乐目录，请在设置（7）中添加",
                        Style::new().fg(Color::DarkGray),
                    )));
                } else {
                    for p in &paths {
                        let mark = if loaded.iter().any(|lp| lp.to_string_lossy().as_ref() == p.as_str()) {
                            "✓"
                        } else {
                            " "
                        };
                        lines.push(Line::from(Span::styled(
                            format!("   {} {}", mark, p),
                            Style::new().fg(Color::Gray),
                        )));
                    }
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        format!("   共 {} 首歌曲", songs.len()),
                        Style::new().fg(Color::Green),
                    )));
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        "   按 r 重新扫描目录",
                        Style::new().fg(Color::DarkGray),
                    )));
                    lines.push(Line::from(Span::styled(
                        "   切换到搜索页（2）可搜索本地歌曲",
                        Style::new().fg(Color::DarkGray),
                    )));
                }

                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::new().fg(crate::theme::muted(ctx)))
                    .title("本地音乐");
                let inner = block.inner(content_area);
                block.render(content_area, frame.buffer_mut());
                Paragraph::new(lines).render(inner, frame.buffer_mut());
            }
        }

        components::progress_bar::render(main_chunks[3], frame.buffer_mut(), ctx);
        components::status_bar::render(main_chunks[4], frame.buffer_mut(), ctx);
        components::notification::render(main_chunks[4], frame.buffer_mut(), ctx);
    })?;
    Ok(())
}

#[allow(dead_code)]
fn action_is_none(action: &AppAction) -> bool {
    matches!(action, AppAction::None)
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
            let mut sp = search_page.lock().unwrap();
            sp.begin_search(&keyword, false);
            drop(sp);
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
        AppAction::SearchMore {
            keyword,
            page,
            source,
        } => {
            let mut sp = search_page.lock().unwrap();
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
        AppAction::PlaySong { songs, index } => {
            if let Some(song) = songs.get(index).cloned() {
                ctx.playlist.set_playlist(songs, index);
                let request_id = ctx.play_request_id.fetch_add(1, Ordering::SeqCst) + 1;
                let player_generation = ctx.player.prepare();
                let lyric_generation = ctx.lyric_service.prepare();
                let show_cover = ctx.config.read().unwrap().ui.show_cover;
                let cover_service = Arc::clone(&ctx.cover_service);
                if show_cover {
                    let initial_cover = song.cover_url.clone();
                    let cover = Arc::clone(&cover_service);
                    rt.spawn(async move {
                        if let Err(error) = cover.load(initial_cover).await {
                            tracing::debug!("load initial cover failed: {}", error);
                        }
                    });
                } else {
                    cover_service.clear();
                }

                // 记录播放历史
                ctx.storage.add_history(&song);

                // 乐观更新当前歌曲（即时反馈）
                *ctx.current_song.write().unwrap() = Some(song.clone());

                // 发送即时通知
                let _ = action_tx.send(AppAction::ShowNotification(Notification::info(format!(
                    "正在加载: {} - {}",
                    song.name, song.singer
                ))));

                let source_mgr = Arc::clone(&ctx.source_manager);
                let player = Arc::clone(&ctx.player);
                let lyric_service = Arc::clone(&ctx.lyric_service);
                let lyric_song = song.clone();
                let lyric_position = ctx.position.clone();
                let lyric_tx = action_tx.clone();
                rt.spawn(async move {
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
                    // 歌词请求通常会跨过数百毫秒甚至数秒，必须用完成时的
                    // 播放位置重新定位，而不是从第一行开始显示。
                    lyric_service.update_position(*lyric_position.borrow());
                    let _ = lyric_tx.send(AppAction::None);
                });
                let current_song = Arc::clone(&ctx.current_song);
                let play_request_id = Arc::clone(&ctx.play_request_id);
                let quality = ctx.config.read().unwrap().player.quality;
                let auto_toggle = ctx.config.read().unwrap().source.auto_toggle;
                let tx = action_tx.clone();

                rt.spawn(async move {
                    let resolved = tokio::time::timeout(
                        Duration::from_secs(40),
                        resolve_playable_song(
                            Arc::clone(&source_mgr),
                            song,
                            quality,
                            auto_toggle,
                            Arc::clone(&play_request_id),
                            request_id,
                        ),
                    )
                    .await;

                    if play_request_id.load(Ordering::SeqCst) != request_id {
                        return;
                    }

                    let (mut resolved_song, song_url) = match resolved {
                        Ok(Ok(Some(resolved))) => resolved,
                        Ok(Ok(None)) => return,
                        Ok(Err(error)) => {
                            player.stop();
                            let _ =
                                tx.send(AppAction::ShowNotification(Notification::error(error)));
                            return;
                        }
                        Err(_) => {
                            player.stop();
                            let _ = tx.send(AppAction::ShowNotification(Notification::error(
                                "获取播放地址超时，请稍后重试",
                            )));
                            return;
                        }
                    };

                    let url = song_url.url;
                    let player_for_start = Arc::clone(&player);
                    let request_guard = Arc::clone(&play_request_id);
                    let accepted = tokio::task::spawn_blocking(move || {
                        if request_guard.load(Ordering::SeqCst) != request_id {
                            return false;
                        }
                        player_for_start.play(&url, player_generation)
                    })
                    .await
                    .unwrap_or(false);
                    if !accepted || play_request_id.load(Ordering::SeqCst) != request_id {
                        return;
                    }

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
                    *current_song.write().unwrap() = Some(resolved_song.clone());
                    let _ = tx.send(AppAction::ShowNotification(Notification::info(format!(
                        "正在播放: {} - {}",
                        resolved_song.name, resolved_song.singer
                    ))));

                    if show_cover
                        && let Err(error) =
                            cover_service.load(resolved_song.cover_url.clone()).await
                    {
                        tracing::debug!("load cover failed: {}", error);
                    }
                });
            }
        }
        AppAction::ShowNotification(n) => {
            ctx.notifications.write().unwrap().push_back(n);
        }
        AppAction::ImportSource(url) => {
            let settings = Arc::clone(settings_page);
            let source_mgr = Arc::clone(&ctx.source_manager);
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
                match lx_source::js::loader::load_source(&url, &default_source).await {
                    Ok(source) => {
                        source_mgr.set_js_source(Arc::new(source));
                        let mut sp = settings.lock().unwrap();
                        sp.selected_source = 0;
                        sp.status_msg = Some("✓ 音源已加载并启用".to_string());
                        let _ = tx.send(AppAction::SourceImported(url));
                    }
                    Err(e) => {
                        let mut sp = settings.lock().unwrap();
                        sp.status_msg = Some(format!("✗ 音源加载失败: {}", e));
                        let _ = tx.send(AppAction::ShowNotification(Notification::error(format!(
                            "JS 音源导入失败: {}",
                            e
                        ))));
                    }
                }
            });
        }
        AppAction::SourceImported(url) => {
            let save_result = {
                let mut config = ctx.config.write().unwrap();
                config.source.js_sources.retain(|item| item != &url);
                config.source.js_sources.insert(0, url);
                crate::config::loader::save(&config, &ctx.config_path)
            };
            if let Err(e) = save_result {
                let mut sp = settings_page.lock().unwrap();
                sp.status_msg = Some(format!("✗ 音源已启用，但保存配置失败: {}", e));
                ctx.notifications
                    .write()
                    .unwrap()
                    .push_back(Notification::error(format!("保存 JS 音源配置失败: {}", e)));
            } else {
                ctx.notifications
                    .write()
                    .unwrap()
                    .push_back(Notification::info("JS 音源已加载并启用"));
            }
        }
        AppAction::RemoveSource(url) => {
            let (remaining_urls, default_source) = {
                let mut config = ctx.config.write().unwrap();
                config.source.js_sources.retain(|u| u != &url);
                let remaining_urls = config.source.js_sources.clone();
                let default_source = config.source.default.as_str().to_string();
                if let Err(e) = crate::config::loader::save(&config, &ctx.config_path) {
                    tracing::warn!("保存配置失败: {}", e);
                }
                (remaining_urls, default_source)
            };
            ctx.source_manager.clear_js_source();
            spawn_js_source_loader(
                remaining_urls,
                default_source,
                Arc::clone(&ctx.source_manager),
                action_tx.clone(),
                rt,
            );
            let _ = action_tx.send(AppAction::ShowNotification(Notification::info(
                "已移除音源",
            )));
        }
        AppAction::Navigate(_) | AppAction::GoBack | AppAction::Quit | AppAction::None => {
            // 不再使用页面栈导航，忽略这些 action
        }
    }
}

async fn resolve_playable_song(
    source_manager: Arc<lx_source::manager::SourceManager>,
    song: SongInfo,
    quality: Quality,
    auto_toggle: bool,
    play_request_id: Arc<AtomicU64>,
    request_id: u64,
) -> Result<Option<(SongInfo, SongUrl)>, String> {
    let direct_error = match source_manager.get_song_url(&song, quality).await {
        Ok(url) => return Ok(Some((song, url))),
        Err(error) => error.to_string(),
    };

    if play_request_id.load(Ordering::SeqCst) != request_id {
        return Ok(None);
    }
    if !auto_toggle {
        return Err(format!("JS 音源获取播放地址失败: {}", direct_error));
    }

    let candidates = source_manager.find_music(&song).await;
    if play_request_id.load(Ordering::SeqCst) != request_id {
        return Ok(None);
    }

    for candidate in candidates.into_iter().take(5) {
        match source_manager.get_song_url(&candidate, quality).await {
            Ok(url) => return Ok(Some((candidate, url))),
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
        "JS 音源获取播放地址失败，换源后仍不可用: {}",
        direct_error
    ))
}

fn spawn_js_source_loader(
    urls: Vec<String>,
    default_source: String,
    source_manager: Arc<lx_source::manager::SourceManager>,
    tx: mpsc::UnboundedSender<AppAction>,
    rt: &tokio::runtime::Runtime,
) {
    let urls: Vec<String> = urls
        .into_iter()
        .filter(|url| !url.trim().is_empty())
        .collect();
    if urls.is_empty() {
        source_manager.clear_js_source();
        return;
    }

    rt.spawn(async move {
        let mut last_error = None;
        for url in urls {
            match lx_source::js::loader::load_source(&url, &default_source).await {
                Ok(source) => {
                    source_manager.set_js_source(Arc::new(source));
                    let _ = tx.send(AppAction::ShowNotification(Notification::info(
                        "JS 音源已就绪",
                    )));
                    return;
                }
                Err(error) => {
                    tracing::warn!("load JS source failed ({}): {}", url, error);
                    last_error = Some(error);
                }
            }
        }

        source_manager.clear_js_source();
        if let Some(error) = last_error {
            let _ = tx.send(AppAction::ShowNotification(Notification::error(format!(
                "没有可用的 JS 音源: {}",
                error
            ))));
        }
    });
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
                let mut sp = search_page.lock().unwrap();
                sp.update_results(keyword, page, append, search_result, source);
                let _ = tx.send(AppAction::None);
            }
            Ok(Err(error)) => {
                if seq.load(Ordering::SeqCst) != my_seq + 1 {
                    return;
                }
                let mut sp = search_page.lock().unwrap();
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
                let mut sp = search_page.lock().unwrap();
                sp.update_error("请求超时，请稍后重试".to_string());
                let _ = tx.send(AppAction::ShowNotification(Notification::error(
                    "搜索超时，请稍后重试".to_string(),
                )));
            }
        }
    });
}

/// 异步加载排行榜歌曲
fn spawn_leaderboard_search(
    request_id: u64,
    board_index: usize,
    board_id: String,
    source: lx_core::model::source::SourceId,
    source_manager: Arc<lx_source::manager::SourceManager>,
    leaderboard_tx: mpsc::UnboundedSender<LeaderboardResponse>,
    rt: &tokio::runtime::Runtime,
) {
    rt.spawn(async move {
        let result = tokio::time::timeout(
            Duration::from_secs(12),
            source_manager.leaderboard(source, &board_id, 1, 100),
        )
        .await;
        let result = match result {
            Ok(Ok(search_result)) => Ok(search_result.items),
            Ok(Err(error)) => Err(error.to_string()),
            Err(_) => Err("请求超时，请稍后重试".to_string()),
        };
        let _ = leaderboard_tx.send(LeaderboardResponse {
            request_id,
            board_index,
            result,
        });
    });
}
