//! voicefox: Rust TUI 版 lx-music-desktop
//!
//! 入口：CLI 解析 → 初始化 → 启动 TUI

mod cli;
mod config;
mod context;
mod cover;
mod data_cache;
mod download;
mod fmt;
mod media_controls;
mod media_session;
#[cfg(target_os = "linux")]
mod mpris;
mod notification;
mod pages;
mod playlist;
mod remote_cache;
mod sleep_timer;
#[cfg(target_os = "windows")]
mod smtc;
mod storage;
mod sync;
mod ui_cursor;
mod visualizer;

mod theme;
mod tmux;

#[cfg(any(target_os = "linux", target_os = "windows"))]
use media_controls::{current_media_snapshot, execute_media_command, start_media_controls};
use media_controls::{persist_volume, toggle_or_start_current};

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
use lx_core::events::PageId;
use lx_core::events::{AppAction, InsertPosition, Notification};
use lx_core::keybinding::{Action, KeybindingResolver};
use lx_core::model::config::SourcePolicy;
use lx_core::model::leaderboard::LeaderboardInfo;
use lx_core::model::login::{QrLoginResult, QrLoginStatus};
use lx_core::model::playlist::Playlist;
use lx_core::model::song::SongInfo;
use lx_core::model::source::{PlayerState, Quality, SourceId};
use lx_core::traits::player::PlayerEvent;
use lx_core::traits::source::SongUrl;
use ratatui::DefaultTerminal;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::Style;
use tokio::sync::mpsc;

use context::{AppContext, JsSourceFailure, JsSourceStatus, StatusBarCommand};
use data_cache::DataCache;
use pages::components;
use pages::components::context_menu::StatusBarMenuAction;
use pages::components::context_menu::{
    ColumnMenuAction, MenuAction, MenuHitSource, MenuItem, MenuOutcome, PlaybackMenuAction,
    PlaybackMenuState, SongContextMenu, SongContextMenuOptions, SongMenuAction, SongMenuKind,
};
use pages::components::list_filter::ListFilter;
use pages::components::status_bar::StatusBarSlot;
use pages::sidebar::NavTab;
use pages::sort::{SortMode, SortState, SortTarget, SortedListCache};

use crate::fmt::format_duration;
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
}

#[derive(Debug, Default, Clone)]
struct UiAreas {
    /// 整屏区域。整屏浮层（详情页/帮助页等）的渲染与命中都以它为基准。
    screen: Rect,
    tabs: Rect,
    content: Rect,
    progress: Rect,
    notification: Rect,
    /// 底部状态栏（快速控制栏）矩形。
    status: Rect,
    /// 底栏本帧实际排布出来的可点区段（渲染与命中共用同一份几何）。
    status_hits: Vec<pages::components::status_bar::StatusBarHit>,
    /// 底栏被宽度收纳进「更多」的段（顺序同配置）。
    status_collapsed: Vec<pages::components::status_bar::StatusBarSlot>,
    /// 底栏顶边的拖拽把手矩形（渲染时写入，命中直接用它 —— 同源）。
    status_handle: Option<Rect>,
    /// 鼠标是否停在拖拽把手上（含"正在拖"），用于 hover 提示。
    status_handle_hover: bool,
    /// 底栏当前悬停的段；只在这个值变化时重绘。
    status_hover: Option<pages::components::status_bar::StatusBarSlot>,
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
        // 2px 邻域内都算同一次双击：终端/触摸板普遍存在 1-2px 手抖，
        // 严格要求同像素会让双击频繁判定失败。
        const DOUBLE_CLICK_SLOP: u16 = 2;
        let doubled = self.last_left_click.is_some_and(|(time, x, y)| {
            x.abs_diff(event.column) <= DOUBLE_CLICK_SLOP
                && y.abs_diff(event.row) <= DOUBLE_CLICK_SLOP
                && time.elapsed() < Duration::from_millis(500)
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
                format_duration(points.start),
                format_duration(points.end)
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

/// 页面在键位配置里的 scope 名（用于 `resolve_page`）。
fn page_scope_for_tab(tab: NavTab) -> Option<&'static str> {
    match tab {
        NavTab::Main => Some("main"),
        NavTab::Search => Some("search"),
        NavTab::Leaderboard => Some("leaderboard"),
        NavTab::Playlists => Some("playlists"),
        NavTab::Favorites => Some("favorites"),
        NavTab::History => Some("history"),
        NavTab::LocalMusic => Some("local"),
        NavTab::Settings => Some("settings"),
    }
}

/// 上下文菜单目标：`(歌曲列表, 下标)` + 菜单类型 + 可选的排序项。
type SongMenuTarget = (
    (Vec<SongInfo>, usize),
    SongMenuKind,
    Option<(SortTarget, SortMode)>,
);

/// 解析"当前页面上下文菜单的目标歌曲"。
///
/// 鼠标右键与键盘入口（默认 `x`）共用这一条路径 —— `source` 决定目标是
/// "鼠标点中那一行"还是"当前选中项"，其余（过滤视图映射、菜单类型、排序项）
/// 完全一致，不会出现两条路径跑偏。
#[allow(clippy::too_many_arguments)]
fn context_menu_target(
    source: MenuHitSource,
    active_tab: NavTab,
    area: Rect,
    ctx: &AppContext,
    main_page: &mut pages::main_page::MainPage,
    leaderboard: &mut pages::leaderboard::LeaderboardPage,
    playlists: &mut pages::playlists::PlaylistsPage,
    favorites_page: &mut pages::favorites::FavoritesPage,
    search_page: &Arc<std::sync::Mutex<pages::search::SearchPage>>,
    history_state: &mut SortState,
    local_state: &mut SortState,
    history_filter: &ListFilter,
    local_filter: &ListFilter,
    data_cache: &mut DataCache,
) -> Option<SongMenuTarget> {
    match active_tab {
        NavTab::Main => main_page
            .context_song_at(source, area, ctx)
            .map(|target| (target, SongMenuKind::Queue, None)),
        NavTab::Search => search_page
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .context_song_at(source, area)
            .map(|target| (target, SongMenuKind::Standard, None)),
        NavTab::Leaderboard => leaderboard
            .context_song_at(source, area)
            .map(|target| (target, SongMenuKind::Standard, None)),
        NavTab::Playlists => playlists
            .context_song_at(source, area)
            .map(|target| (target, SongMenuKind::Standard, None)),
        NavTab::Favorites => favorites_page
            .context_song_at(source, area, ctx, &mut data_cache.favorites)
            .map(|target| {
                (
                    target,
                    SongMenuKind::Standard,
                    Some((SortTarget::Favorites, favorites_page.sort_mode())),
                )
            }),
        NavTab::History => pages::history::context_song_at(
            source,
            area,
            ctx,
            history_state,
            history_filter,
            &mut data_cache.history,
        )
        .map(|target| {
            (
                target,
                SongMenuKind::History,
                Some((SortTarget::History, history_state.mode)),
            )
        }),
        NavTab::LocalMusic => pages::local_music::context_song_at(
            source,
            area,
            ctx,
            local_state,
            &mut data_cache.local,
            local_filter,
        )
        .map(|target| {
            (
                target,
                SongMenuKind::Local,
                Some((SortTarget::Local, local_state.mode)),
            )
        }),
        NavTab::Settings => None,
    }
}

/// 用已解析出的目标构造歌曲上下文菜单。
fn build_song_menu(
    target: SongMenuTarget,
    origin: Position,
    active_tab: NavTab,
    ctx: &AppContext,
    playlists: &pages::playlists::PlaylistsPage,
) -> Option<SongContextMenu> {
    let ((songs, index), kind, sort) = target;
    let is_favorite = ctx.storage.is_favorite(songs.get(index)?);
    let custom_playlists = ctx.storage.custom_playlist_choices();
    let current_custom_playlist = (active_tab == NavTab::Playlists)
        .then(|| playlists.current_custom_playlist_id())
        .flatten();
    SongContextMenu::new(
        origin,
        songs,
        index,
        kind,
        is_favorite,
        SongContextMenuOptions {
            sort,
            custom_playlists,
            current_custom_playlist,
            playback: Some(playback_menu_state(ctx)),
        },
    )
}

/// 分发上下文菜单选中的动作。
///
/// 歌曲动作走原有业务路径；列动作复用既有的 `CommitColumnResize` /
/// `ResetColumnWidths` 两个 AppAction（`ResetColumnWidths` 至此才有了生产者）。
/// 列菜单作用的页面 key（与 `table_columns` 持久化用的 key 一致）。
fn column_page_key(tab: NavTab) -> Option<&'static str> {
    match tab {
        NavTab::Main => Some("queue"),
        NavTab::Search => Some("search"),
        NavTab::Leaderboard => Some("leaderboard"),
        NavTab::Playlists => Some("playlists"),
        NavTab::Favorites => Some("favorites"),
        NavTab::History => Some("history"),
        NavTab::LocalMusic => Some("local_music"),
        NavTab::Settings => None,
    }
}

/// 当前页面的歌曲表头矩形 + 自动列宽样本（都是无副作用的只读信息）。
#[allow(clippy::too_many_arguments)]
fn table_header_and_samples(
    active_tab: NavTab,
    area: Rect,
    ctx: &AppContext,
    main_page: &mut pages::main_page::MainPage,
    leaderboard: &pages::leaderboard::LeaderboardPage,
    playlists: &pages::playlists::PlaylistsPage,
    favorites_page: &pages::favorites::FavoritesPage,
    search_page: &Arc<std::sync::Mutex<pages::search::SearchPage>>,
    history_filter: &ListFilter,
    local_filter: &ListFilter,
) -> Option<(Rect, Vec<SongInfo>)> {
    match active_tab {
        NavTab::Main => {
            let header = main_page.table_header_rect(area, ctx)?;
            Some((header, main_page.autofit_samples(ctx)))
        }
        NavTab::Search => {
            let page = search_page.lock().unwrap_or_else(|e| e.into_inner());
            let header = page.table_header_rect(area)?;
            Some((header, page.autofit_samples(ctx)))
        }
        NavTab::Leaderboard => Some((
            leaderboard.table_header_rect(area, ctx)?,
            leaderboard.autofit_samples(ctx),
        )),
        NavTab::Playlists => Some((
            playlists.table_header_rect(area, ctx)?,
            playlists.autofit_samples(ctx),
        )),
        NavTab::Favorites => Some((
            favorites_page.table_header_rect(area, ctx)?,
            favorites_page.autofit_samples(ctx),
        )),
        NavTab::History => Some((
            pages::history::table_header_rect(area, history_filter)?,
            pages::history::autofit_samples(ctx),
        )),
        NavTab::LocalMusic => Some((
            pages::local_music::table_header_rect(area, local_filter)?,
            pages::local_music::autofit_samples(ctx),
        )),
        NavTab::Settings => None,
    }
}

/// 一次"拖表头换列"的会话状态。
#[derive(Debug, Clone)]
struct ColumnReorderState {
    page_key: String,
    column_key: String,
    /// 拖拽开始时表头的**实际**矩形。列宽与边界都要按它换算——主页面宽布局
    /// 下队列只占右侧一栏，表头宽度和整块内容区宽度并不相等，拿内容区宽度
    /// 去算边界会把"拖边界改宽"误判成"拖表头换列"。
    header: Rect,
}

fn column_at_x(
    columns: &[lx_core::model::config::TableColumnConfig],
    width: u16,
    local_x: u16,
) -> Option<&lx_core::model::config::TableColumnConfig> {
    pages::components::song_table::compute_layout(columns, width)
        .into_iter()
        .find(|column| {
            local_x >= column.start_x && local_x < column.start_x.saturating_add(column.width)
        })
        .map(|layout| &columns[layout.original_index])
}

fn reorder_columns_at_x(
    columns: &[lx_core::model::config::TableColumnConfig],
    width: u16,
    column_key: &str,
    local_x: u16,
) -> Vec<lx_core::model::config::TableColumnConfig> {
    let layout = pages::components::song_table::compute_layout(columns, width);
    let Some(source_index) = layout
        .iter()
        .position(|layout| columns[layout.original_index].key == column_key)
    else {
        return columns.to_vec();
    };
    let mut target_index = layout
        .iter()
        .position(|layout| local_x < layout.start_x.saturating_add(layout.width / 2))
        .unwrap_or(layout.len());
    if target_index == source_index || target_index == source_index + 1 {
        return columns.to_vec();
    }

    let mut visible = layout
        .iter()
        .map(|layout| columns[layout.original_index].clone())
        .collect::<Vec<_>>();
    let moved = visible.remove(source_index);
    if target_index > source_index {
        target_index = target_index.saturating_sub(1);
    }
    visible.insert(target_index.min(visible.len()), moved);

    let mut visible_iter = visible.into_iter();
    let mut result = columns.to_vec();
    for column in result.iter_mut().filter(|column| column.visible) {
        if let Some(next) = visible_iter.next() {
            *column = next;
        }
    }
    result
}

/// 构造表头列设置菜单：显示/隐藏切换 + 自动列宽 + 恢复默认。
///
/// 切换项直接把"换过 visible 之后的完整列配置"放进动作里，
/// 提交端不需要回头问页面，显示隐藏与自动列宽共用同一个 AppAction。
fn build_column_menu(
    active_tab: NavTab,
    origin: Position,
    // 表头的实际宽度（不是内容区宽度）：列配置/自动列宽必须与页面渲染同口径。
    width: u16,
    ctx: &AppContext,
    samples: &[SongInfo],
) -> Option<SongContextMenu> {
    use pages::components::song_table::{
        auto_fit_columns, column_is_hideable, load_columns_for_page, toggle_column_visibility,
    };

    let page_key = column_page_key(active_tab)?;
    let columns = {
        let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
        load_columns_for_page(&config.ui.table_columns, page_key, width)
    };

    let mut items = Vec::new();
    for column in &columns {
        let mark = if column.visible { "✓" } else { "○" };
        let label = format!("{mark} {}", column.label);
        // "歌曲"列不允许隐藏，做成可见但不可选的禁用项。
        if !column_is_hideable(&column.key) {
            items.push(MenuItem::disabled(label).with_hint("必显"));
            continue;
        }
        items.push(MenuItem::new(
            label,
            ColumnMenuAction::Apply(toggle_column_visibility(&columns, &column.key)),
        ));
    }
    items.push(MenuItem::new(
        "自动调整列宽",
        ColumnMenuAction::Apply(auto_fit_columns(&columns, samples, width)),
    ));
    items.push(MenuItem::new("恢复默认列宽", ColumnMenuAction::Reset));
    items.push(MenuItem::new(
        "恢复默认面板布局",
        ColumnMenuAction::ResetLayout,
    ));
    Some(SongContextMenu::from_entries(
        origin,
        " 列设置 ",
        items,
        page_key,
    ))
}

/// 底栏左键的一次性结果。
///
/// 能直接变成 `AppAction` 的就走 `AppAction`；需要主循环本地状态
/// （标签页、下载面板）的排队成 `StatusBarCommand`。
enum StatusBarPrimary {
    Action(AppAction),
    Command(StatusBarCommand),
    /// 该段的左键就是展开它自己的菜单（音源、自定义音源、当前歌曲…）。
    OpenMenu,
    /// 纯展示段（时间、排序），点了什么也不做。
    None,
}

/// 底栏各段的左键行为 —— "快速切换"优先。
fn status_bar_primary(slot: StatusBarSlot, ctx: &AppContext) -> StatusBarPrimary {
    use lx_core::model::config::StatusBarItem;
    match slot {
        StatusBarSlot::Download => {
            StatusBarPrimary::Command(StatusBarCommand::ToggleDownloadsPanel)
        }
        // 睡眠定时器没有"快速切换"语义（设定需要精确选档），左右键都开菜单。
        StatusBarSlot::SleepTimer => StatusBarPrimary::OpenMenu,
        StatusBarSlot::More => StatusBarPrimary::OpenMenu,
        StatusBarSlot::Item(item) => match item {
            StatusBarItem::State => StatusBarPrimary::Action(AppAction::TogglePlayPause),
            StatusBarItem::Song => StatusBarPrimary::OpenMenu,
            StatusBarItem::Volume => StatusBarPrimary::OpenMenu,
            // 左键循环切换（与 `m` 同一套顺序），右键才是精确选择。
            StatusBarItem::PlayMode => {
                let next = ctx.playlist.mode().next_mode();
                StatusBarPrimary::Action(AppAction::SetPlayMode(next.as_config().to_string()))
            }
            // 音质左键循环切换；只改偏好，不打断正在播放的歌。
            StatusBarItem::Quality => {
                let current = ctx
                    .config
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .player
                    .quality;
                StatusBarPrimary::Action(AppAction::SetQuality(pages::settings::next_quality(
                    current,
                )))
            }
            StatusBarItem::Queue => StatusBarPrimary::Command(StatusBarCommand::JumpToQueue),
            StatusBarItem::Source | StatusBarItem::JsSourceState => StatusBarPrimary::OpenMenu,
            StatusBarItem::Time | StatusBarItem::Sort => StatusBarPrimary::None,
        },
    }
}

/// 底栏鼠标事件的单一分派结果。
///
/// 判定顺序写死在一处 —— **高度拖拽 > 段点击**：拖拽一旦开始，任何鼠标事件
/// 都只用来改行数，绝不会顺手触发某个段的主操作（项目里"拖拽把别的控件一起
/// 点了"那类问题就是这么来的）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusBarMouseRoute {
    /// 按在把手（顶边最右端）上：开始一次高度拖拽。
    BeginResize,
    /// 拖拽中：把行数预览成这个值（只改内存，不落盘）。
    Resize(u16),
    /// 松开左键：提交这个行数并落盘。
    CommitResize(u16),
    /// 命中某个段：左键 = 主操作，右键 = 精确选择菜单。
    Click { slot: StatusBarSlot, right: bool },
    /// 在底栏上但无事可做（段间分隔符、纯悬停…）。
    None,
}

/// 底栏鼠标分派（纯函数）。
///
/// `handle` 必须是渲染那一帧写进 `UiAreas` 的把手矩形，`hits` 也必须来自
/// 同一帧 —— 命中与渲染同源才不会"看得到却拖不动"。
fn route_status_bar_mouse(
    mouse: crossterm::event::MouseEvent,
    bottom: u16,
    handle: Option<Rect>,
    hits: &[pages::components::status_bar::StatusBarHit],
    dragging: bool,
) -> StatusBarMouseRoute {
    use crossterm::event::{MouseButton, MouseEventKind};

    // 拖拽中：优先于一切（包括"指针正好压在某个段上"）。
    if dragging {
        return match mouse.kind {
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved => {
                StatusBarMouseRoute::Resize(pages::components::status_bar::rows_for_pointer(
                    bottom, mouse.row,
                ))
            }
            MouseEventKind::Up(MouseButton::Left) => StatusBarMouseRoute::CommitResize(
                pages::components::status_bar::rows_for_pointer(bottom, mouse.row),
            ),
            _ => StatusBarMouseRoute::None,
        };
    }

    if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
        && handle.is_some_and(|handle| handle.contains(Position::new(mouse.column, mouse.row)))
    {
        return StatusBarMouseRoute::BeginResize;
    }

    let Some(slot) = pages::components::status_bar::hit_test(hits, mouse.column, mouse.row) else {
        return StatusBarMouseRoute::None;
    };
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            StatusBarMouseRoute::Click { slot, right: false }
        }
        MouseEventKind::Down(MouseButton::Right) => {
            StatusBarMouseRoute::Click { slot, right: true }
        }
        _ => StatusBarMouseRoute::None,
    }
}

/// 把底栏行数写进内存配置（拖拽预览用）；返回是否真的变了。
fn set_status_bar_rows(ctx: &AppContext, rows: u16) -> bool {
    let rows = rows.clamp(1, u16::from(lx_core::model::config::STATUS_BAR_MAX_HEIGHT));
    let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
    if config.ui.status_bar_height == rows as u8 {
        return false;
    }
    config.ui.status_bar_height = rows as u8;
    true
}

/// 松开鼠标：写内存并立刻落盘（沿用 `CommitPaneRatio` 的写法）。
fn commit_status_bar_rows(ctx: &AppContext, rows: u16) {
    set_status_bar_rows(ctx, rows);
    let save_result = {
        let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
        crate::config::loader::save(&config, &ctx.config_path)
    };
    if let Err(error) = save_result {
        ctx.notify(Notification::error(format!("保存底栏高度失败: {error}")));
    }
}

/// 睡眠定时器菜单的条目（`t` 与底栏段共用同一份构造）。
fn sleep_timer_menu_items(ctx: &AppContext) -> Vec<MenuItem> {
    let armed = ctx.sleep_timer.armed_minutes();
    let mut items = Vec::new();
    items.push(MenuItem::disabled(match ctx.sleep_timer.status_label() {
        Some(label) => format!("当前: {label}"),
        None => "当前: 未启用".to_string(),
    }));
    items.push(MenuItem::new(
        if armed.is_none() {
            "✓ 关闭"
        } else {
            "○ 关闭"
        },
        StatusBarMenuAction::SetSleepTimer(None),
    ));
    for minutes in crate::sleep_timer::PRESET_MINUTES {
        let mark = if armed == Some(minutes) { "✓" } else { "○" };
        items.push(MenuItem::new(
            format!("{mark} {minutes} 分钟"),
            StatusBarMenuAction::SetSleepTimer(Some(minutes)),
        ));
    }
    items
}

/// 睡眠定时器菜单：`t` 打开；底栏段的左键 / 右键复用同一份。
fn build_sleep_timer_menu(origin: Position, ctx: &AppContext) -> SongContextMenu {
    SongContextMenu::from_status_items(origin, " 睡眠定时器 ", sleep_timer_menu_items(ctx))
}

/// 底栏右键菜单（左键对"打开菜单"型段也复用同一份构造）。
fn build_status_bar_menu(
    slot: StatusBarSlot,
    origin: Position,
    ctx: &AppContext,
    playlists: &pages::playlists::PlaylistsPage,
    collapsed: &[StatusBarSlot],
) -> Option<SongContextMenu> {
    use lx_core::model::config::StatusBarItem;
    use pages::components::context_menu::submenu;

    let (title, items) = match slot {
        StatusBarSlot::More => (" 更多 ".to_string(), collapsed_slot_items(collapsed, ctx)),
        StatusBarSlot::SleepTimer => (" 睡眠定时器 ".to_string(), sleep_timer_menu_items(ctx)),
        StatusBarSlot::Download => (
            " 下载 ".to_string(),
            vec![{
                let mut item =
                    MenuItem::new("打开下载面板", StatusBarMenuAction::OpenDownloadsPanel);
                if let Some(hint) = config_key_hint(ctx, None, Action::GlobalDownloadsPanel) {
                    item = item.with_hint(hint);
                }
                item
            }],
        ),
        StatusBarSlot::Item(item) => match item {
            StatusBarItem::State => (
                " 播放控制 ".to_string(),
                vec![
                    {
                        let mut item =
                            MenuItem::new("播放 / 暂停", StatusBarMenuAction::TogglePlayPause);
                        if let Some(hint) = config_key_hint(ctx, None, Action::GlobalPlayPause) {
                            item = item.with_hint(hint);
                        }
                        item
                    },
                    {
                        let mut item = MenuItem::new("上一首", StatusBarMenuAction::PreviousTrack);
                        if let Some(hint) = config_key_hint(ctx, None, Action::GlobalPrevTrack) {
                            item = item.with_hint(hint);
                        }
                        item
                    },
                    {
                        let mut item = MenuItem::new("下一首", StatusBarMenuAction::NextTrack);
                        if let Some(hint) = config_key_hint(ctx, None, Action::GlobalNextTrack) {
                            item = item.with_hint(hint);
                        }
                        item
                    },
                ],
            ),
            // 当前歌曲直接复用歌曲菜单，避免两套歌曲操作。
            StatusBarItem::Song => {
                let song = ctx
                    .current_song
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()?;
                return build_song_menu(
                    ((vec![song], 0), SongMenuKind::Standard, None),
                    origin,
                    NavTab::Main,
                    ctx,
                    playlists,
                );
            }
            StatusBarItem::Volume => (
                " 音量 ".to_string(),
                vec![
                    {
                        let mut item =
                            MenuItem::new("音量 +5%", StatusBarMenuAction::VolumeDelta(5));
                        if let Some(hint) = config_key_hint(ctx, None, Action::GlobalVolumeUp) {
                            item = item.with_hint(hint);
                        }
                        item
                    },
                    {
                        let mut item =
                            MenuItem::new("音量 -5%", StatusBarMenuAction::VolumeDelta(-5));
                        if let Some(hint) = config_key_hint(ctx, None, Action::GlobalVolumeDown) {
                            item = item.with_hint(hint);
                        }
                        item
                    },
                    MenuItem::new("静音 / 恢复", StatusBarMenuAction::ToggleMute),
                    MenuItem::new("设为 50%", StatusBarMenuAction::SetVolume(50)),
                    MenuItem::new("设为 100%", StatusBarMenuAction::SetVolume(100)),
                ],
            ),
            StatusBarItem::PlayMode => {
                let current = ctx.playlist.mode();
                let mut items = Vec::new();
                for mode in [
                    crate::playlist::mode::PlayMode::ListLoop,
                    crate::playlist::mode::PlayMode::SingleLoop,
                    crate::playlist::mode::PlayMode::Random,
                    crate::playlist::mode::PlayMode::List,
                    crate::playlist::mode::PlayMode::None,
                ] {
                    let mark = if mode == current { "✓" } else { "○" };
                    items.push(MenuItem::new(
                        format!("{mark} {}", mode.label()),
                        StatusBarMenuAction::SetPlayMode(mode.as_config().to_string()),
                    ));
                }
                (" 播放模式 ".to_string(), items)
            }
            StatusBarItem::Quality => {
                let (preference, actual) = {
                    let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
                    (config.player.quality, ctx.audio_info.borrow().label())
                };
                let mut items = Vec::new();
                for quality in [
                    Quality::Low128,
                    Quality::High320,
                    Quality::Flac,
                    Quality::Flac24,
                ] {
                    let mark = if quality == preference { "✓" } else { "○" };
                    items.push(MenuItem::new(
                        format!("{mark} {}", quality.label()),
                        StatusBarMenuAction::SetQuality(quality),
                    ));
                }
                // 偏好与实际分开显示，避免"点了 FLAC 就一定有 FLAC"的误解。
                items.push(MenuItem::disabled(format!(
                    "偏好 {} · 当前 {}",
                    preference.label(),
                    actual.unwrap_or_else(|| "未知".to_string())
                )));
                items.push(MenuItem::new(
                    "用当前音质重新解析这首歌",
                    StatusBarMenuAction::ReparseCurrentSong,
                ));
                (" 音质 ".to_string(), items)
            }
            StatusBarItem::Queue => {
                let len = ctx.playlist.len();
                let index = ctx.playlist.current_index();
                (
                    " 播放队列 ".to_string(),
                    vec![
                        MenuItem::new("上一首", StatusBarMenuAction::PreviousTrack),
                        MenuItem::new("下一首", StatusBarMenuAction::NextTrack),
                        MenuItem::disabled(format!(
                            "当前 {} / {}",
                            if len == 0 { 0 } else { index + 1 },
                            len
                        )),
                        MenuItem::new("跳到队列页并定位当前歌", StatusBarMenuAction::JumpToQueue),
                        MenuItem::new("清空队列", StatusBarMenuAction::ClearQueue),
                    ],
                )
            }
            StatusBarItem::Source => {
                let (policy, platform, source_name) = {
                    let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
                    let name = current_source_label(ctx);
                    (config.source.policy, config.source.policy_platform, name)
                };
                let enabled = ctx.source_manager.enabled_sources();
                let mut policy_items = Vec::new();
                for (mode, label) in [(SourcePolicy::Prefer, "优先"), (SourcePolicy::Only, "只用")]
                {
                    let mut targets = Vec::new();
                    for id in &enabled {
                        let mark = if policy == mode && platform == Some(*id) {
                            "✓"
                        } else {
                            "○"
                        };
                        let name = ctx
                            .source_manager
                            .get(*id)
                            .map(|source| source.name().to_string())
                            .unwrap_or_else(|| id.as_str().to_string());
                        targets.push(MenuItem::new(
                            format!("{mark} {name}"),
                            StatusBarMenuAction::SetSourcePolicy {
                                policy: mode,
                                platform: Some(*id),
                            },
                        ));
                    }
                    policy_items.push(MenuItem::new(
                        label,
                        submenu(format!(" {label}指定平台 "), targets),
                    ));
                }
                (
                    " 音源 ".to_string(),
                    vec![
                        // 当前播放来源是只读信息，与下面的"策略"严格分开：
                        // 否则用户分不清"换的是这首歌"还是"以后都用它"。
                        MenuItem::disabled(format!("当前播放来源: {source_name}")),
                        MenuItem::new(
                            if policy == SourcePolicy::Auto {
                                "✓ 自动（默认）"
                            } else {
                                "○ 自动（默认）"
                            },
                            StatusBarMenuAction::SetSourcePolicy {
                                policy: SourcePolicy::Auto,
                                platform: None,
                            },
                        ),
                        policy_items.remove(0),
                        policy_items.remove(0),
                        MenuItem::new("重新加载 JS 音源", StatusBarMenuAction::ReloadJsSources),
                        MenuItem::new(
                            "打开设置·音源面板",
                            StatusBarMenuAction::OpenSettingsSources,
                        ),
                    ],
                )
            }
            StatusBarItem::JsSourceState => {
                let status = ctx.js_source_status();
                let mut items: Vec<MenuItem> = ctx
                    .source_manager
                    .js_source_names()
                    .into_iter()
                    .map(|name| MenuItem::disabled(format!("✓ {name}")))
                    .collect();
                for failure in &status.failures {
                    items.push(MenuItem::disabled(format!(
                        "✗ {} — {}",
                        failure.name, failure.reason
                    )));
                }
                if status.total == 0 {
                    items.push(MenuItem::disabled("未配置 JS 音源"));
                }
                items.push(MenuItem::new(
                    "重新加载",
                    StatusBarMenuAction::ReloadJsSources,
                ));
                items.push(MenuItem::new(
                    "打开设置·音源面板",
                    StatusBarMenuAction::OpenSettingsSources,
                ));
                (" 自定义音源 ".to_string(), items)
            }
            StatusBarItem::Time | StatusBarItem::Sort => return None,
        },
    };
    Some(SongContextMenu::from_status_items(origin, title, items))
}

/// 当前播放来源的可读名称（JS 音源名优先，否则内置平台名）。
fn current_source_label(ctx: &AppContext) -> String {
    let song = ctx
        .current_song
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let Some(song) = song else {
        return "-".to_string();
    };
    let js_index = *ctx
        .play_js_source_index
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    js_index
        .and_then(|index| ctx.source_manager.js_source_name(index))
        .or_else(|| {
            ctx.source_manager
                .get(song.source)
                .map(|source| source.name().to_string())
        })
        .unwrap_or_else(|| song.source.display_name().to_string())
}

/// 从键位配置反查动作的提示串（页面级优先、全局兜底）。
///
/// 底栏菜单、页面标题里的键位提示统一走这里：用户改键后提示跟随变化；
/// 动作没有绑定（用户删掉了）时返回 `None`，调用方整体省略提示段。
fn config_key_hint(ctx: &AppContext, page: Option<&str>, action: Action) -> Option<String> {
    ctx.config
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .keybindings
        .key_hint(page, action)
        .map(str::to_string)
}

/// 「更多」菜单：列出被宽度挤掉的可交互段，点进去仍是它们各自的菜单。
fn collapsed_slot_items(collapsed: &[StatusBarSlot], ctx: &AppContext) -> Vec<MenuItem> {
    use lx_core::model::config::StatusBarItem;
    if collapsed.is_empty() {
        return vec![MenuItem::disabled("没有更多字段")];
    }
    let mut items = Vec::new();
    for slot in collapsed {
        let label = match slot {
            StatusBarSlot::Download => {
                format!("下载 ({})", ctx.downloads.snapshot().len())
            }
            StatusBarSlot::SleepTimer => "睡眠定时器".to_string(),
            StatusBarSlot::More => continue,
            StatusBarSlot::Item(item) => match item {
                StatusBarItem::State => "播放控制".to_string(),
                StatusBarItem::Song => "当前歌曲".to_string(),
                StatusBarItem::Volume => format!("音量 {}%", ctx.player.volume()),
                StatusBarItem::PlayMode => format!("播放模式 {}", ctx.playlist.mode().label()),
                StatusBarItem::Quality => "音质".to_string(),
                StatusBarItem::Queue => "播放队列".to_string(),
                StatusBarItem::Source => format!("音源 {}", current_source_label(ctx)),
                StatusBarItem::JsSourceState => ctx.js_source_status().summary(),
                StatusBarItem::Time | StatusBarItem::Sort => continue,
            },
        };
        items.push(MenuItem::new(label, StatusBarMenuAction::OpenSlot(*slot)));
    }
    items
}

/// 执行底栏菜单动作。
///
/// 能落到 `AppAction` 的一律复用（模式、音质、策略、重载、跳设置）；
/// 音量/播放暂停/上一首下一首直接操作 `ctx`；需要循环本地状态或要开新菜单的
/// 排队成 `StatusBarCommand`，由主循环消费。
#[allow(clippy::too_many_arguments)]
fn execute_status_bar_action(
    action: StatusBarMenuAction,
    ctx: &AppContext,
    rt: &tokio::runtime::Runtime,
    action_tx: &mpsc::UnboundedSender<AppAction>,
    search_page: &Arc<std::sync::Mutex<pages::search::SearchPage>>,
    settings_page: &Arc<std::sync::Mutex<pages::settings::SettingsPage>>,
    search_seq: &Arc<AtomicU64>,
) {
    use std::sync::atomic::Ordering;

    let forward = |action: AppAction| {
        execute_action(
            action,
            ctx,
            rt,
            action_tx,
            search_page,
            settings_page,
            search_seq,
        );
    };
    match action {
        StatusBarMenuAction::TogglePlayPause => forward(AppAction::TogglePlayPause),
        StatusBarMenuAction::SetPlayMode(value) => forward(AppAction::SetPlayMode(value)),
        StatusBarMenuAction::SetQuality(quality) => forward(AppAction::SetQuality(quality)),
        StatusBarMenuAction::SetSourcePolicy { policy, platform } => {
            forward(AppAction::SetSourcePolicy { policy, platform });
        }
        StatusBarMenuAction::ReloadJsSources => forward(AppAction::ReloadJsSources),
        StatusBarMenuAction::OpenSettingsSources => forward(AppAction::Navigate(PageId::Settings)),
        StatusBarMenuAction::VolumeDelta(delta) => {
            let next = (ctx.player.volume() as i32 + delta).clamp(0, 100) as u32;
            persist_volume(ctx, next);
        }
        StatusBarMenuAction::SetVolume(volume) => persist_volume(ctx, volume.min(100)),
        StatusBarMenuAction::ToggleMute => {
            let current = ctx.player.volume();
            if current == 0 {
                let restore = ctx
                    .mute_restore_volume
                    .load(Ordering::Relaxed)
                    .clamp(1, 100);
                persist_volume(ctx, restore);
            } else {
                ctx.mute_restore_volume.store(current, Ordering::Relaxed);
                persist_volume(ctx, 0);
            }
        }
        StatusBarMenuAction::NextTrack => {
            if let Some((songs, index)) = ctx.playlist.next_manual_entry_arc() {
                forward(AppAction::PlayFromQueue { songs, index });
            }
        }
        StatusBarMenuAction::PreviousTrack => {
            if let Some((songs, index)) = ctx.playlist.prev_manual_entry_arc() {
                forward(AppAction::PlayFromQueue { songs, index });
            }
        }
        StatusBarMenuAction::ReparseCurrentSong => {
            let song = ctx
                .current_song
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Some(song) = song {
                // 重置解析状态：换音质后要重新走一遍完整换源链路。
                start_song_playback(song, false, None, true, ctx, rt, action_tx);
            }
        }
        StatusBarMenuAction::JumpToQueue => {
            ctx.queue_status_bar_command(StatusBarCommand::JumpToQueue);
        }
        StatusBarMenuAction::ClearQueue => {
            ctx.queue_status_bar_command(StatusBarCommand::ClearQueue);
        }
        StatusBarMenuAction::OpenDownloadsPanel => {
            ctx.queue_status_bar_command(StatusBarCommand::ToggleDownloadsPanel);
        }
        StatusBarMenuAction::SetSleepTimer(minutes) => {
            let message = match minutes {
                Some(mins) => ctx.sleep_timer.arm(mins),
                None => ctx.sleep_timer.cancel(),
            };
            ctx.notify(Notification::info(message));
        }
        StatusBarMenuAction::OpenSlot(slot) => {
            ctx.queue_status_bar_command(StatusBarCommand::OpenStatusBarMenu(slot));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn dispatch_menu_action(
    action: MenuAction,
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
    match action {
        MenuAction::Song(song_action) => execute_song_menu_action(
            song_action,
            menu,
            main_page,
            ctx,
            rt,
            action_tx,
            search_page,
            settings_page,
            search_seq,
            favorites_page,
            playlists_page,
            history_state,
            local_state,
            confirm_delete,
        ),
        MenuAction::Column(column_action) => {
            let Some(page_key) = menu.page_key().map(str::to_string) else {
                return;
            };
            let action = match column_action {
                ColumnMenuAction::Apply(columns) => {
                    AppAction::CommitColumnResize { page_key, columns }
                }
                ColumnMenuAction::Reset => AppAction::ResetColumnWidths { page_key },
                // 内存态复位交给主循环统一处理（那里同时持有队列 / 榜单 /
                // 歌单 / 设置四个页面，能按 page_key 正确分发）。
                ColumnMenuAction::ResetLayout => AppAction::ResetPaneLayout { page_key },
            };
            execute_action(
                action,
                ctx,
                rt,
                action_tx,
                search_page,
                settings_page,
                search_seq,
            );
        }
        MenuAction::StatusBar(status_action) => execute_status_bar_action(
            status_action,
            ctx,
            rt,
            action_tx,
            search_page,
            settings_page,
            search_seq,
        ),
        // 子菜单与返回上级由菜单自身消费，不会走到这里。
        MenuAction::Submenu { .. } | MenuAction::Back => {}
        // 设置页自己消费的菜单动作（设置页持有并处理自己的菜单，主循环无需理解其语义）。
        MenuAction::SettingChoice { .. } => {}
    }
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
        SongMenuAction::ViewArtist(artist_name) => match menu.songs().get(menu.index()) {
            Some(song) if !artist_name.trim().is_empty() => {
                let mut artist_song = song.clone();
                artist_song.singer = artist_name;
                AppAction::ShowArtistDetails(Box::new(artist_song))
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

/// 启动频谱可视化采集；失败时记录日志并给出 TUI 提示，返回 None。
/// 启动初始化（配置里默认开启）与按 w 切换共用这一条路径。
fn start_visualizer(ctx: &AppContext) -> Option<visualizer::Visualizer> {
    match visualizer::Visualizer::start() {
        Ok(handle) => Some(handle),
        Err(error) => {
            tracing::warn!("visualizer unavailable: {error:#}");
            ctx.notify(Notification::warning(format!("频谱可视化启动失败: {error:#}")).tui_only());
            None
        }
    }
}

/// 把可视化开关落盘到 `[ui] visualizer`（off / bars）。写盘失败由调用方提示。
fn persist_visualizer_mode(ctx: &AppContext, mode: &str) -> anyhow::Result<()> {
    let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
    config.ui.visualizer = mode.to_string();
    crate::config::loader::save(&config, &ctx.config_path)
}

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
    // 平台媒体控件只存在于 Linux（MPRIS）/ Windows（SMTC）：macOS 上既不启动，
    // 也不声明句柄（下面所有使用点本身也在这两个平台的 cfg 下）。
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    let (media_handle, mut media_command_rx) = start_media_controls(&ctx, rt);

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
    let (search_source_filter, wrap_navigation, scroll_amount, page_step, enabled_sources) = {
        let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
        (
            if config.ui.aggregate_search {
                None
            } else {
                Some(config.source.default)
            },
            config.ui.wrap_navigation,
            config.ui.scroll_amount,
            config.ui.page_step,
            config.source.enabled.clone(),
        )
    };
    let search_page = Arc::new(std::sync::Mutex::new(pages::search::SearchPage::new(
        search_source_filter,
        wrap_navigation,
        scroll_amount,
        page_step,
        &enabled_sources,
    )));
    let settings_page = Arc::new(std::sync::Mutex::new(pages::settings::SettingsPage::new()));
    let (cover_protocol, mut cover_enabled) = {
        let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
        (config.ui.cover_protocol.clone(), config.ui.show_cover)
    };
    let mut main_page = pages::main_page::MainPage::new(cover::CoverRenderer::detect(
        &cover_protocol,
        cover_enabled,
    ));
    // 封面协议能力只探测这一次（与 `CoverRenderer::detect` 同一口径）：设置页
    // 的 `Shift+P` 循环据它跳过本终端画不出的协议，并在行内显示已纠正的原因。
    settings_page
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .set_cover_capabilities(cover::CoverCapabilities::detect(main_page.cover_protocol()));
    let mut leaderboard =
        pages::leaderboard::LeaderboardPage::new(ctx.source_manager.leaderboard_sources());
    let mut playlists = pages::playlists::PlaylistsPage::new(ctx.source_manager.playlist_sources());
    // 恢复用户拖拽过的面板分隔比例（run_app 开头 config 已就绪）
    {
        let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
        if let Some(ratios) = config.ui.pane_ratios.get(leaderboard.pane_page_key()) {
            leaderboard.apply_pane_ratios(ratios);
        }
        let playlists_key = playlists.pane_page_key();
        if let Some(ratios) = config.ui.pane_ratios.get(playlists_key) {
            playlists.apply_pane_ratios(ratios);
        }
        if let Some(ratios) = config.ui.pane_ratios.get(main_page.pane_page_key()) {
            main_page.apply_pane_ratios(ratios);
        }
        let settings_key = settings_page
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pane_page_key();
        if let Some(ratios) = config.ui.pane_ratios.get(settings_key) {
            settings_page
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .apply_pane_ratios(ratios);
        }
    }
    let mut favorites_page =
        pages::favorites::FavoritesPage::new(ctx.source_manager.enabled_sources());
    let mut history_state = SortState::new(SortMode::Newest, "history");
    let mut local_state = SortState::new(SortMode::TitleAsc, "local_music");
    let mut data_cache = DataCache::default();
    let mut local_filter = components::list_filter::ListFilter::new();
    let mut history_filter = components::list_filter::ListFilter::new();
    let mut confirm_delete: Option<LocalDeleteConfirmation> = None;
    let mut local_diagnostics: Option<LocalDiagnosticsKind> = None;
    let mut song_menu: Option<SongContextMenu> = None;
    let mut column_reorder: Option<ColumnReorderState> = None;
    let mut ui_areas = UiAreas::default();
    let mut click_tracker = ClickTracker::default();
    let mut qr_login_page: Option<Arc<std::sync::Mutex<pages::qr_login::QrLoginPage>>> = None;
    let mut sync_overlay: Option<pages::sync_overlay::SyncOverlay> = None;
    // 快捷键说明浮层（? / F1 开关）
    let mut help_page: Option<pages::help::HelpPage> = None;
    // 下载面板浮层（Ctrl+o 开关）
    let mut downloads_panel = pages::downloads::DownloadsPanel::new();
    // 频谱可视化：Some 即开启（绘制并持续采集），None 即关闭。按 w 切换。
    let visualizer_enabled = ctx
        .config
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .ui
        .visualizer_enabled();
    let mut visualizer: Option<visualizer::Visualizer> = if visualizer_enabled {
        start_visualizer(&ctx)
    } else {
        None
    };
    // 频谱叠加层的调色板缓存：主题不变时跨帧复用渐变（见 render.rs）。
    let mut visualizer_palette = visualizer::PaletteCache::default();
    // 底栏高度拖拽会话：拖动中只改内存，松开才落盘（行数真值始终在 config 里）。
    let mut status_bar_resizing = false;
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
    let mut last_login_notice_check = Instant::now();
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
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    let mut last_media_snapshot: Option<media_session::MediaSnapshot> = None;
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    let mut last_media_update = Instant::now() - Duration::from_secs(1);
    #[cfg(any(target_os = "linux", target_os = "windows"))]
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
        Arc::clone(&ctx.js_source_status),
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

        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if let Some(receiver) = media_command_rx.as_mut() {
            while let Ok(command) = receiver.try_recv() {
                if execute_media_command(
                    command,
                    &ctx,
                    rt,
                    &action_tx,
                    &search_page,
                    &settings_page,
                    &search_seq,
                ) {
                    tracing::info!("quit requested through media controls");
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
            // 切页时上一页可能还停在拖拽会话里，先统一收尾。
            abort_all_drag_sessions(
                &mut main_page,
                &mut leaderboard,
                &mut playlists,
                &mut favorites_page,
                &search_page,
                &settings_page,
                &mut history_state,
                &mut local_state,
            );
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
            // 拦截扫码登录相关 action
            match &action {
                AppAction::QrLogin(source, kind) => {
                    if let Some(task) = qr_generate_task.take() {
                        task.abort();
                    }
                    if let Some(task) = qr_poll_task.take() {
                        task.abort();
                    }
                    let page = Arc::new(std::sync::Mutex::new(
                        pages::qr_login::QrLoginPage::with_kind(
                            *source,
                            source.display_name().to_string(),
                            *kind,
                        ),
                    ));
                    qr_generate_task = Some(spawn_qr_generate(
                        rt,
                        &ctx,
                        Arc::clone(&page),
                        action_tx.clone(),
                    ));
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
                    if *source == SourceId::Wy && ctx.source_manager.is_logged_in(*source) {
                        // 网易云登录成功后立即进入远程收藏刷新，不再要求用户猜测下一步。
                        let _ = action_tx.send(AppAction::SyncNetease);
                    }
                    needs_render = true;
                    continue;
                }
                AppAction::SyncNetease | AppAction::SyncQq => {
                    let source = if matches!(action, AppAction::SyncNetease) {
                        SourceId::Wy
                    } else {
                        SourceId::Tx
                    };
                    if !ctx.source_manager.is_logged_in(source) {
                        ctx.notify(Notification::warning(format!(
                            "请先登录{}",
                            source.display_name()
                        )));
                        needs_render = true;
                        continue;
                    }
                    let mut overlay = pages::sync_overlay::SyncOverlay::new();
                    overlay.start_for(source, Arc::clone(&ctx.storage), rt);
                    sync_overlay = Some(overlay);
                    needs_render = true;
                    continue;
                }
                AppAction::PushLocalPlaylist { playlist_id } => {
                    let local = ctx.storage.custom_playlist(playlist_id).map(|playlist| {
                        crate::sync::LocalPushCollection {
                            name: playlist.name.clone(),
                            songs: playlist.songs.clone(),
                            is_favorites: false,
                        }
                    });
                    match local {
                        Some(local) => {
                            let mut overlay = pages::sync_overlay::SyncOverlay::new();
                            overlay.start_push_for(SourceId::Wy, local, rt);
                            sync_overlay = Some(overlay);
                        }
                        None => ctx.notify(Notification::warning("歌单不存在或已被删除")),
                    }
                    needs_render = true;
                    continue;
                }
                AppAction::PushFavorites => {
                    let local = crate::sync::LocalPushCollection {
                        name: "我的收藏".to_string(),
                        songs: ctx.storage.load_favorites(),
                        is_favorites: true,
                    };
                    if local.songs.is_empty() {
                        ctx.notify(Notification::info("收藏为空，没有可推送的歌曲"));
                        needs_render = true;
                        continue;
                    }
                    let mut overlay = pages::sync_overlay::SyncOverlay::new();
                    overlay.start_push_for(SourceId::Wy, local, rt);
                    sync_overlay = Some(overlay);
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
                        if retry_song
                            .as_ref()
                            .is_some_and(|song| should_retry_with_other_source(song, auto_toggle))
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
                            let request = pages::playlists::PlaylistLoadRequest::List {
                                source,
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
            // 远程歌单刷新是异步的：先把入口列表同步到最新缓存，再决定请求
            playlists.sync_scopes();
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

        // 二维码初始生成 / 过期自动重建：页面停在 Generating 或本地计时到期时
        // （不必等服务端 800，见 `needs_regeneration` 的说明），补发生成任务。
        if let Some(ref page) = qr_login_page
            && qr_generate_task.is_none()
        {
            let regenerate = page
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .needs_regeneration();
            if regenerate {
                qr_generate_task = Some(spawn_qr_generate(
                    rt,
                    &ctx,
                    Arc::clone(page),
                    action_tx.clone(),
                ));
                needs_render = true;
            }
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
                    let result = match manager.check_qr_login(source, &key).await {
                        Ok(result) => Ok(result),
                        Err(error) => Ok(QrLoginResult::new(
                            QrLoginStatus::NetworkError,
                            format!("{}，正在重试", error),
                        )),
                    };
                    let mut page = page.lock().unwrap_or_else(|e| e.into_inner());
                    page.apply_check_result(result);
                    let success = page.succeeded().is_some();
                    drop(page);
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
        // 播放自动缓存：按秒检查进度，达到配置阈值后入队一次；不改变现有 TUI。
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

        // 网易云会话失效提醒：播放/搜索等路径上接口返回「需要登录」时，
        // 音源侧会记一次标记，这里消费并提醒一次（不重复轰炸）。
        if last_login_notice_check.elapsed() >= Duration::from_secs(1) {
            last_login_notice_check = Instant::now();
            if lx_source::wy::session::take_expired_notice() {
                ctx.notify(Notification::warning(
                    "网易云登录已失效，请在设置（8）→ 账号与扫码 重新扫码",
                ));
                needs_render = true;
            }
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
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if let Some(handle) = media_handle.as_ref() {
            // 例行更新按 250ms 限频，但跳转要立刻放行
            let position_epoch = ctx.position_epoch();
            if position_epoch != last_position_epoch
                || last_media_update.elapsed() >= Duration::from_millis(250)
            {
                last_position_epoch = position_epoch;
                let snapshot = current_media_snapshot(&ctx);
                if last_media_snapshot.as_ref() != Some(&snapshot) {
                    handle.update(snapshot.clone());
                    last_media_snapshot = Some(snapshot);
                }
                last_media_update = Instant::now();
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
                || qr_login_page.is_some()
                || sync_overlay.is_some()
                // 睡眠定时器启用时倒计时每秒都在变，暂停状态下也要保持走动。
                || ctx.sleep_timer.is_active()
                // 频谱画的是系统混音：voicefox 暂停时柱子也可能在动（其他应用出声）。
                || visualizer.is_some();
            // 睡眠定时器随周期渲染一并推进：到点淡出、淡出后暂停都发生在这里。
            if let Some(message) = ctx.sleep_timer.poll(&ctx) {
                ctx.notify(Notification::info(message));
                needs_render = true;
            }
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
                &mut sync_overlay,
                &mut help_page,
                &mut downloads_panel,
                &mut visualizer_palette,
                visualizer.as_ref(),
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
            // 1a. 侧边栏全局快捷键（1-8）—— 输入模式下跳过
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

            if let Some(ref mut sync) = sync_overlay {
                let phase = sync.state.lock().unwrap_or_else(|e| e.into_inner()).phase;
                match phase {
                    pages::sync_overlay::SyncPhase::PushPick => match (key.modifiers, key.code) {
                        (KeyModifiers::NONE, KeyCode::Esc) => {
                            sync_overlay = None;
                        }
                        (KeyModifiers::NONE, KeyCode::Up)
                        | (KeyModifiers::NONE, KeyCode::Char('k')) => {
                            sync.push_move(-1);
                        }
                        (KeyModifiers::NONE, KeyCode::Down)
                        | (KeyModifiers::NONE, KeyCode::Char('j')) => {
                            sync.push_move(1);
                        }
                        (KeyModifiers::NONE, KeyCode::Enter) => {
                            if sync.push_on_create_option() {
                                sync.push_plan_create(rt);
                            } else {
                                sync.push_plan_selected(rt);
                            }
                        }
                        _ => {}
                    },
                    pages::sync_overlay::SyncPhase::PushDiff => match (key.modifiers, key.code) {
                        (KeyModifiers::NONE, KeyCode::Esc) => {
                            // 回到选单；重新选择目标会覆盖旧计划。
                            sync.set_phase(pages::sync_overlay::SyncPhase::PushPick);
                        }
                        (KeyModifiers::NONE, KeyCode::Enter)
                        | (KeyModifiers::NONE, KeyCode::Char('s'))
                        | (KeyModifiers::NONE, KeyCode::Char('S')) => {
                            sync.push_confirm(rt);
                        }
                        _ => {}
                    },
                    _ => match key.code {
                        KeyCode::Esc
                            if matches!(
                                phase,
                                pages::sync_overlay::SyncPhase::Preparing
                                    | pages::sync_overlay::SyncPhase::Running
                            ) =>
                        {
                            sync.cancel();
                        }
                        KeyCode::Esc => {
                            sync_overlay = None;
                        }
                        KeyCode::Enter | KeyCode::Char('s') | KeyCode::Char('S')
                            if phase == pages::sync_overlay::SyncPhase::Preview =>
                        {
                            sync.confirm(Arc::clone(&ctx.storage), rt);
                        }
                        KeyCode::Char('r') | KeyCode::Char('R')
                            if matches!(
                                phase,
                                pages::sync_overlay::SyncPhase::Failed
                                    | pages::sync_overlay::SyncPhase::Cancelled
                            ) =>
                        {
                            sync.retry(Arc::clone(&ctx.storage), rt);
                        }
                        _ => {}
                    },
                }
                needs_render = true;
                continue;
            }

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
                        dispatch_menu_action(
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

            // 快捷键说明浮层：? / F1 开关；打开时独占按键
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
                    (KeyModifiers::NONE, KeyCode::Char('\\'))
                )
            {
                let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
                help_page = Some(pages::help::HelpPage::from_config(
                    &config.keybindings,
                    config.ui.page_step.clamp(1, 100),
                    config.ui.scroll_amount,
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
                    AppAction::SyncNetease | AppAction::SyncQq => {}
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

            // 设置页独占自己的选项键；数字键 1-8 则始终留给侧边栏。
            // 先判断页面归属，再分发全局快捷键。
            let settings_owns_key = active_tab == NavTab::Settings
                && !text_input_active
                && settings_page
                    .lock()
                    .unwrap()
                    .consumes_key(&key, &kb_resolver);

            if !text_input_active
                && !settings_owns_key
                && let Some(tab) = pages::sidebar::handle_input(&key)
            {
                active_tab = tab;
                needs_render = true;
                continue;
            }

            // 1b. 全局快捷键（查表模式）；页面专属动作在下方处理
            // 设置页的选项键覆盖了大半个字母表，与全局键位必然冲突，交由页面独占
            if !settings_owns_key && let Some(action) = kb_resolver.resolve_global(&key) {
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
                            NavTab::LocalMusic => NavTab::Settings,
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
                            NavTab::Settings => NavTab::LocalMusic,
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
                    // 一键把**当前页面**的面板布局恢复默认：原入口只在表头右键
                    // 菜单里（列设置 → 恢复默认面板布局），无鼠标环境按不到。
                    Action::GlobalResetLayout if !text_input_active => {
                        let page_key = match active_tab {
                            NavTab::Main => Some("queue"),
                            NavTab::Leaderboard => Some("leaderboard"),
                            NavTab::Playlists => Some("playlists"),
                            NavTab::Settings => Some("settings"),
                            // 其余页面没有可拖拽的面板比例
                            _ => None,
                        };
                        if let Some(page_key) = page_key {
                            execute_action(
                                AppAction::ResetPaneLayout {
                                    page_key: page_key.to_string(),
                                },
                                &ctx,
                                rt,
                                &action_tx,
                                &search_page,
                                &settings_page,
                                &search_seq,
                            );
                            needs_render = true;
                        }
                        continue;
                    }
                    // 睡眠定时器：`t` 打开菜单（预设档位 + 关闭），与底栏段同源。
                    Action::GlobalSleepTimer if !text_input_active => {
                        song_menu = Some(build_sleep_timer_menu(
                            Position::new(ui_areas.status.x + 2, ui_areas.status.y),
                            &ctx,
                        ));
                        needs_render = true;
                        continue;
                    }
                    // 频谱可视化：w 开关，状态落盘到 [ui] visualizer。
                    Action::GlobalVisualizer if !text_input_active => {
                        let enabling = visualizer.is_none();
                        visualizer = if enabling {
                            start_visualizer(&ctx)
                        } else {
                            None
                        };
                        if !enabling {
                            // 关闭频谱后必须让封面重新传图：开启期间每帧频谱都会
                            // 覆盖封面图片行首那格（图形协议把整行转义序列塞在
                            // 首列），kitty 会自动撤掉 placement，而协议状态仍记着
                            // "已发送"不会重发，不重传则封面区域留空。
                            retransmit_cover(terminal, &mut main_page)?;
                        }
                        if visualizer.is_some() == enabling {
                            let mode = if enabling { "bars" } else { "off" };
                            let message =
                                format!("频谱可视化: {}", if enabling { "开启" } else { "关闭" });
                            match persist_visualizer_mode(&ctx, mode) {
                                Ok(()) => ctx.notify(Notification::info(message)),
                                Err(error) => ctx.notify(Notification::warning(format!(
                                    "{message}，但保存失败: {error}"
                                ))),
                            }
                        }
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

            // 键盘打开上下文菜单（无鼠标环境下的右键替代入口）。
            // 与鼠标右键共用 `context_menu_target`，两条路径不会跑偏。
            if !text_input_active
                && let Some(scope) = page_scope_for_tab(active_tab)
                && kb_resolver.resolve_page(scope, &key) == Some(Action::ListContextMenu)
            {
                let origin = Position::new(
                    ui_areas.content.x.saturating_add(2),
                    ui_areas.content.y.saturating_add(2),
                );
                if let Some(target) = context_menu_target(
                    MenuHitSource::Selected,
                    active_tab,
                    ui_areas.content,
                    &ctx,
                    &mut main_page,
                    &mut leaderboard,
                    &mut playlists,
                    &mut favorites_page,
                    &search_page,
                    &mut history_state,
                    &mut local_state,
                    &history_filter,
                    &local_filter,
                    &mut data_cache,
                ) {
                    song_menu = build_song_menu(target, origin, active_tab, &ctx, &playlists);
                    needs_render = true;
                    continue;
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
                // 左/右方向键的 seek 在 Left/Right 空闲的页签生效（队列/历史/本地）；
                // 搜索、排行榜、歌单、收藏、设置页把这两个键用在了切音源/切分类等
                // 页面功能上，不能被全局 seek 吞掉。鼠标点进度条不受此限制。
                (KeyModifiers::NONE, KeyCode::Right)
                    if !text_input_active
                        && matches!(
                            active_tab,
                            NavTab::Main | NavTab::History | NavTab::LocalMusic
                        ) =>
                {
                    let pos = *ctx.position.borrow();
                    ctx.seek(pos + Duration::from_secs(5));
                    needs_render = true;
                    continue;
                }
                (KeyModifiers::NONE, KeyCode::Left)
                    if !text_input_active
                        && matches!(
                            active_tab,
                            NavTab::Main | NavTab::History | NavTab::LocalMusic
                        ) =>
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
                // 音量调整全页签可用（与 `.`/`,` 一致）；裸 Up/Down 仍归列表导航。
                (KeyModifiers::CONTROL, KeyCode::Up) if !text_input_active => {
                    persist_volume(&ctx, ctx.player.volume().saturating_add(5));
                    needs_render = true;
                    continue;
                }
                (KeyModifiers::CONTROL, KeyCode::Down) if !text_input_active => {
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
                NavTab::Settings => {
                    let action = {
                        let mut sp = settings_page.lock().unwrap_or_else(|e| e.into_inner());
                        sp.handle_input(key, &ctx, &kb_resolver)
                    };
                    // BiliLogin/BiliLogout 需要发到 channel 让主循环处理（生成 QR 码等）
                    if matches!(
                        action,
                        AppAction::QrLogin(..)
                            | AppAction::QrLogout(_)
                            | AppAction::QrLoginSuccess(_)
                            | AppAction::SyncNetease
                            | AppAction::SyncQq
                    ) {
                        let _ = action_tx.send(action);
                    } else {
                        // 'z' 切换滚动步长；'j' 在设置页无功能，属死键，移除
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
                                    config.ui.page_step,
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
            if confirm_delete.is_some() {
                needs_render = true;
                continue;
            }
            let mouse = *mouse;
            // 扫码登录 / 同步浮层是模态的：键盘分支早已拦住按键，鼠标分支以前漏了，
            // 于是浮层在屏上时点 tab 会切页、点进度条会 seek、点通知会开外链。
            if qr_login_page.is_some() || sync_overlay.is_some() {
                needs_render = true;
                continue;
            }
            // 页面只在 `ui_areas.content` 内收到鼠标事件；在标签栏/侧边栏/进度条上
            // 松开左键时页面拿不到 Up，这里兜底结束它的拖拽会话。
            if matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left))
                && !ui_areas
                    .content
                    .contains(Position::new(mouse.column, mouse.row))
            {
                column_reorder = None;
                abort_all_drag_sessions(
                    &mut main_page,
                    &mut leaderboard,
                    &mut playlists,
                    &mut favorites_page,
                    &search_page,
                    &settings_page,
                    &mut history_state,
                    &mut local_state,
                );
                needs_render = true;
            }
            if let Some(reorder) = column_reorder.clone() {
                match mouse.kind {
                    MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved => {
                        // 一律按"拖拽开始时表头的真实矩形"换算：主页面宽布局下
                        // 队列只占右栏，表头比整块内容区窄，用内容区宽度会算错列位置。
                        let width = reorder.header.width;
                        let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                        let current = pages::components::song_table::load_columns_for_page(
                            &config.ui.table_columns,
                            &reorder.page_key,
                            width,
                        );
                        let local_x = mouse.column.saturating_sub(reorder.header.x);
                        let next =
                            reorder_columns_at_x(&current, width, &reorder.column_key, local_x);
                        if next != current {
                            config
                                .ui
                                .table_columns
                                .insert(reorder.page_key.clone(), next);
                        }
                        needs_render = true;
                        continue;
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        let page_key = reorder.page_key.clone();
                        column_reorder = None;
                        let columns = {
                            let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
                            pages::components::song_table::load_columns_for_page(
                                &config.ui.table_columns,
                                &page_key,
                                reorder.header.width,
                            )
                        };
                        let _ = action_tx.send(AppAction::CommitColumnResize { page_key, columns });
                        needs_render = true;
                        continue;
                    }
                    _ => {
                        needs_render = true;
                        continue;
                    }
                }
            }

            if downloads_panel.is_open() {
                let tasks = ctx.downloads.snapshot();
                downloads_panel.handle_mouse(&mouse, &ctx, &tasks);
                needs_render = true;
                continue;
            }

            if let Some(menu) = song_menu.as_mut() {
                let outcome = menu.handle_mouse(mouse, ui_areas.content);
                match outcome {
                    MenuOutcome::None => {}
                    MenuOutcome::Close => {
                        song_menu = None;
                    }
                    MenuOutcome::Action(action) => {
                        let menu = song_menu.take().unwrap();
                        dispatch_menu_action(
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
                // 详情页渲染在整屏上（见 draw_app），命中必须用同一矩形。
                let action = page.handle_mouse(mouse, ui_areas.screen, &ctx, activate);
                if let AppAction::GoBack = action {
                    *ctx.details_page.lock().unwrap_or_else(|e| e.into_inner()) = None;
                }
                needs_render = true;
                continue;
            }

            let position = Position::new(mouse.column, mouse.row);
            if active_tab == NavTab::Favorites && favorites_page.remote_menu_open() {
                if let Some(action) = favorites_page.handle_remote_menu_mouse(mouse) {
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
                continue;
            }
            // 底栏高度拖拽：一旦开始，指针可以离开底栏（往上拖到屏幕中部），
            // 所以必须先于"按位置分派"的整条链处理；拖拽期间不吃滚轮、不吃段点击。
            if status_bar_resizing {
                let route = route_status_bar_mouse(
                    mouse,
                    ui_areas.status.bottom(),
                    ui_areas.status_handle,
                    &ui_areas.status_hits,
                    true,
                );
                match route {
                    StatusBarMouseRoute::Resize(rows) => {
                        set_status_bar_rows(&ctx, rows);
                    }
                    StatusBarMouseRoute::CommitResize(rows) => {
                        status_bar_resizing = false;
                        ui_areas.status_handle_hover = false;
                        commit_status_bar_rows(&ctx, rows);
                    }
                    _ => {}
                }
                needs_render = true;
                continue;
            }
            // 底栏悬停高亮：鼠标一动就整屏重画太浪费，只在悬停段变化时重绘。
            if matches!(mouse.kind, MouseEventKind::Moved) {
                let pointer = Position::new(mouse.column, mouse.row);
                let hovered = if ui_areas.status.contains(pointer) {
                    pages::components::status_bar::hit_test(
                        &ui_areas.status_hits,
                        mouse.column,
                        mouse.row,
                    )
                } else {
                    None
                };
                let handle_hovered = ui_areas
                    .status_handle
                    .is_some_and(|handle| handle.contains(pointer));
                if hovered != ui_areas.status_hover
                    || handle_hovered != ui_areas.status_handle_hover
                {
                    ui_areas.status_hover = hovered;
                    ui_areas.status_handle_hover = handle_hovered;
                    needs_render = true;
                }
                // 设置页的分界线悬停高亮：页面收不到 `Moved`（上面已经 continue），
                // 只有这一处把移动转给它；返回值 = 悬停的分界线变了。
                if active_tab == NavTab::Settings
                    && settings_page
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .update_divider_hover(pointer)
                {
                    needs_render = true;
                }
                continue;
            }
            // 底栏滚轮：悬停在底栏**任意**段上都调音量（常见播放器惯例）。
            // 用现场命中而不是 Moved 缓存的 hover——未经移动直接滚轮时
            // 缓存可能是旧值。底栏上滚轮不冒泡到列表，避免"在状态栏滚动
            // 却把上面的列表滚走"。
            if matches!(
                mouse.kind,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            ) && ui_areas.status.contains(position)
            {
                let hovered = pages::components::status_bar::hit_test(
                    &ui_areas.status_hits,
                    mouse.column,
                    mouse.row,
                );
                if hovered.is_some() {
                    let delta = if matches!(mouse.kind, MouseEventKind::ScrollUp) {
                        5
                    } else {
                        -5
                    };
                    let next = (ctx.player.volume() as i32 + delta).clamp(0, 100) as u32;
                    persist_volume(&ctx, next);
                }
                needs_render = true;
                continue;
            }
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
            } else if ui_areas.progress.contains(position)
                && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            {
                let duration = *ctx.duration.borrow();
                if let Some(position) = components::progress_bar::seek_position(
                    ui_areas.progress,
                    mouse.column,
                    duration,
                ) {
                    ctx.seek(position);
                }
            } else if ui_areas.status.contains(position)
                && matches!(
                    mouse.kind,
                    MouseEventKind::Down(MouseButton::Left)
                        | MouseEventKind::Down(MouseButton::Right)
                )
            {
                // 底栏是"快速控制栏"：左键 = 主操作，右键 = 精确选择菜单。
                // 顶边右侧的把手另有用途 —— 高度拖拽（见 route_status_bar_mouse），
                // 它占的列已经不在任何段的命中矩形里。
                match route_status_bar_mouse(
                    mouse,
                    ui_areas.status.bottom(),
                    ui_areas.status_handle,
                    &ui_areas.status_hits,
                    false,
                ) {
                    StatusBarMouseRoute::BeginResize => {
                        status_bar_resizing = true;
                        ui_areas.status_handle_hover = true;
                        needs_render = true;
                    }
                    StatusBarMouseRoute::Click { slot, right } => {
                        if right {
                            if let Some(menu) = build_status_bar_menu(
                                slot,
                                position,
                                &ctx,
                                &playlists,
                                &ui_areas.status_collapsed,
                            ) {
                                song_menu = Some(menu);
                            }
                        } else {
                            match status_bar_primary(slot, &ctx) {
                                StatusBarPrimary::Action(action) => {
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
                                StatusBarPrimary::Command(command) => {
                                    ctx.queue_status_bar_command(command);
                                }
                                StatusBarPrimary::OpenMenu => {
                                    if let Some(menu) = build_status_bar_menu(
                                        slot,
                                        position,
                                        &ctx,
                                        &playlists,
                                        &ui_areas.status_collapsed,
                                    ) {
                                        song_menu = Some(menu);
                                    }
                                }
                                StatusBarPrimary::None => {}
                            }
                        }
                        needs_render = true;
                    }
                    StatusBarMouseRoute::Resize(_)
                    | StatusBarMouseRoute::CommitResize(_)
                    | StatusBarMouseRoute::None => {}
                }
            } else if ui_areas.content.contains(position) {
                // 表头左键拖动非分隔线区域 → 调整列顺序；分隔线仍交给页面自己的
                // ColumnResizeState 处理，因此“拖边界改宽”和“拖表头换列”不冲突。
                if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
                    && let Some((header, _)) = table_header_and_samples(
                        active_tab,
                        ui_areas.content,
                        &ctx,
                        &mut main_page,
                        &leaderboard,
                        &playlists,
                        &favorites_page,
                        &search_page,
                        &history_filter,
                        &local_filter,
                    )
                    && header.y == mouse.row
                    && mouse.column >= header.x
                    && mouse.column < header.right()
                    && let Some(page_key) = column_page_key(active_tab)
                {
                    // 用表头自身的宽度与起点：整块内容区宽度在主页面上包含
                    // 左侧封面/歌词栏，会让边界判定错位，拖边界变成换列。
                    let width = header.width;
                    let columns = {
                        let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
                        pages::components::song_table::load_columns_for_page(
                            &config.ui.table_columns,
                            page_key,
                            width,
                        )
                    };
                    let local_x = mouse.column.saturating_sub(header.x);
                    let is_boundary =
                        pages::components::song_table::find_boundary(&columns, width, local_x)
                            .is_some();
                    if !is_boundary && let Some(column) = column_at_x(&columns, width, local_x) {
                        column_reorder = Some(ColumnReorderState {
                            page_key: page_key.to_string(),
                            column_key: column.key.clone(),
                            header,
                        });
                        needs_render = true;
                        continue;
                    }
                }

                // 表头右键 → 列设置菜单。与歌曲菜单共用同一个槽位与交互
                // （渲染、键盘、点外关闭都只有一份实现）。
                if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Right))
                    && let Some((header, samples)) = table_header_and_samples(
                        active_tab,
                        ui_areas.content,
                        &ctx,
                        &mut main_page,
                        &leaderboard,
                        &playlists,
                        &favorites_page,
                        &search_page,
                        &history_filter,
                        &local_filter,
                    )
                    && header.y == mouse.row
                    && mouse.column >= header.x
                    && mouse.column < header.right()
                    && let Some(menu) =
                        build_column_menu(active_tab, position, header.width, &ctx, &samples)
                {
                    song_menu = Some(menu);
                    needs_render = true;
                    continue;
                }
                if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Right)) {
                    if let Some(target) = context_menu_target(
                        MenuHitSource::Mouse(mouse),
                        active_tab,
                        ui_areas.content,
                        &ctx,
                        &mut main_page,
                        &mut leaderboard,
                        &mut playlists,
                        &mut favorites_page,
                        &search_page,
                        &mut history_state,
                        &mut local_state,
                        &history_filter,
                        &local_filter,
                        &mut data_cache,
                    ) {
                        song_menu = build_song_menu(target, position, active_tab, &ctx, &playlists);
                        needs_render = true;
                        continue;
                    }
                    if active_tab == NavTab::Favorites {
                        favorites_page.open_remote_menu(position);
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
                        &history_filter,
                        &mut data_cache.history,
                        activate,
                    ),
                    NavTab::Settings => settings_page
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .handle_mouse(mouse, ui_areas.content, &ctx, &kb_resolver),
                    NavTab::LocalMusic => pages::local_music::handle_mouse(
                        mouse,
                        ui_areas.content,
                        &ctx,
                        &mut local_state,
                        &mut data_cache.local,
                        &local_filter,
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
            // 尺寸变了，拖拽中的比例/坐标已经失效，统一收尾。
            abort_all_drag_sessions(
                &mut main_page,
                &mut leaderboard,
                &mut playlists,
                &mut favorites_page,
                &search_page,
                &settings_page,
                &mut history_state,
                &mut local_state,
            );
            // 底栏也一样：屏幕高度变了，拖拽中的行数映射已经不对了。
            status_bar_resizing = false;
            ui_areas.status_handle_hover = false;
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
            // 远程歌单刷新是异步的：先把入口列表同步到最新缓存，再决定请求
            playlists.sync_scopes();
            playlists.sync_saved_playlists(&ctx);
            maybe_spawn_playlist_load(
                &mut playlists,
                &mut playlist_request_id,
                Arc::clone(&ctx.source_manager),
                playlist_tx.clone(),
                rt,
            );
        }

        // 底栏排队的一次性命令：需要 `active_tab` / 下载面板这类循环本地状态，
        // 没法在 execute_action 里做，统一在这里消费。
        for command in ctx.take_status_bar_commands() {
            match command {
                StatusBarCommand::ToggleDownloadsPanel => {
                    let count = ctx.downloads.snapshot().len();
                    downloads_panel.toggle(count);
                    terminal.clear()?;
                    needs_render = true;
                }
                StatusBarCommand::JumpToQueue => {
                    active_tab = NavTab::Main;
                    let queue_len = ctx.playlist.len();
                    main_page.select_current(ctx.playlist.current_index(), queue_len);
                    needs_render = true;
                }
                StatusBarCommand::ClearQueue => {
                    main_page.clear_queue(&ctx);
                    needs_render = true;
                }
                StatusBarCommand::OpenAccountPlaylist(remote_id) => {
                    active_tab = NavTab::Playlists;
                    playlists.focus_account_playlist(&remote_id, &ctx);
                    needs_render = true;
                }
                StatusBarCommand::ResetPaneLayout(page_key) => {
                    // 每个页面自己的"默认比例"只有它知道，这里持有全部页面引用，
                    // 因此是唯一能一次分发到位的时机。
                    match page_key.as_str() {
                        "queue" => main_page.reset_pane_ratios(),
                        "leaderboard" => leaderboard.reset_pane_ratios(),
                        "playlists" => playlists.reset_pane_ratios(),
                        "settings" => {
                            let mut settings =
                                settings_page.lock().unwrap_or_else(|e| e.into_inner());
                            settings.reset_pane_ratios();
                        }
                        _ => {}
                    }
                    ctx.notify(Notification::success("已恢复默认面板布局"));
                    needs_render = true;
                }
                StatusBarCommand::OpenStatusBarMenu(slot) => {
                    if let Some(menu) = build_status_bar_menu(
                        slot,
                        Position::new(ui_areas.status.x + 2, ui_areas.status.y),
                        &ctx,
                        &playlists,
                        &ui_areas.status_collapsed,
                    ) {
                        song_menu = Some(menu);
                        needs_render = true;
                    }
                }
            }
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
                &mut sync_overlay,
                &mut help_page,
                &mut downloads_panel,
                &mut visualizer_palette,
                visualizer.as_ref(),
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
    sync_overlay: &mut Option<pages::sync_overlay::SyncOverlay>,
    help_page: &mut Option<pages::help::HelpPage>,
    downloads_panel: &mut pages::downloads::DownloadsPanel,
    visualizer_palette: &mut visualizer::PaletteCache,
    visualizer: Option<&visualizer::Visualizer>,
) -> anyhow::Result<()> {
    terminal.draw(|frame| {
        // 每帧重新收集文本插入点请求（见 ui_cursor 模块说明）。
        crate::ui_cursor::clear();
        let area = frame.area();
        frame.render_widget(
            ratatui::widgets::Block::default().style(
                Style::new()
                    .bg(crate::theme::base(ctx))
                    .fg(crate::theme::text(ctx)),
            ),
            area,
        );
        // 全局最小尺寸兜底：过小的窗口里固定边框（头部 4 + 标签栏 3 + 进度 1 +
        // 状态栏 ≥1）会把内容区压到只剩残行，各组件互相挤压没法看。
        // 显示提示页并清空全部命中区，防止鼠标打到上一帧的过期坐标。
        const MIN_WIDTH: u16 = 60;
        const MIN_HEIGHT: u16 = 20;
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            *ui_areas = UiAreas::default();
            let hint = format!("窗口太小，请调整终端尺寸（至少 {MIN_WIDTH} × {MIN_HEIGHT}）");
            let hint_width = hint.chars().count() as u16;
            let hint_area = Rect::new(
                area.x + area.width.saturating_sub(hint_width) / 2,
                area.y + area.height / 2,
                hint_width.min(area.width),
                1,
            );
            let hint_paragraph = ratatui::widgets::Paragraph::new(hint)
                .alignment(ratatui::layout::Alignment::Center)
                .style(Style::new().fg(crate::theme::yellow(ctx)));
            ratatui::widgets::Widget::render(hint_paragraph, hint_area, frame.buffer_mut());
            return;
        }
        let status_rows = ctx
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .ui
            .status_bar_rows()
            .max(1);
        // 顶部的层次：Now Playing 头部（无外框，3 行）→ 标签栏（背景色带，1 行）
        // → 内容 → 进度条 → 底部快速控制栏。
        // 头部与标签栏的行数（回退到原始布局：头部 4 行带框 + 标签栏 3 行）。
        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4),           // 头部
                Constraint::Length(3),           // 标签栏
                Constraint::Min(3),              // tab content
                Constraint::Length(1),           // progress bar
                Constraint::Length(status_rows), // status bar（可配 1~2 行）
            ])
            .split(area);

        components::header::render(main_chunks[0], frame.buffer_mut(), ctx);
        pages::sidebar::render(main_chunks[1], frame.buffer_mut(), active_tab, ctx);
        let content_area = main_chunks[2];
        let kept_hover = ui_areas.status_hover;
        let kept_handle_hover = ui_areas.status_handle_hover;
        *ui_areas = UiAreas {
            screen: area,
            tabs: main_chunks[1],
            content: content_area,
            progress: main_chunks[3],
            notification: Rect::default(),
            status: main_chunks[4],
            status_hits: Vec::new(),
            status_collapsed: Vec::new(),
            status_handle: None,
            status_handle_hover: kept_handle_hover,
            status_hover: kept_hover,
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
            NavTab::Settings => {
                let mut sp = settings_page.lock().unwrap_or_else(|e| e.into_inner());
                sp.render(content_area, frame.buffer_mut(), ctx);
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
                    // 排序键提示读真实键位配置；用户删掉绑定时整段省略。
                    let sort_suffix =
                        match config_key_hint(ctx, Some("local"), Action::ListCycleSort) {
                            Some(hint) => format!(" · {hint} 切换"),
                            None => String::new(),
                        };
                    let block = Block::default()
                        .borders(components::hit_test::PANEL_BORDERS)
                        .border_style(Style::new().fg(crate::theme::muted(ctx)))
                        .title(if is_scanning {
                            format!(
                                "本地音乐 ({} 首，扫描中) · 排序 {}{}{}{}",
                                songs.len(),
                                local_state.mode.label(SortTarget::Local),
                                sort_suffix,
                                filter_suffix,
                                diagnostics
                            )
                        } else {
                            format!(
                                "本地音乐 ({} 首) · 排序 {}{}{}{}",
                                songs.len(),
                                local_state.mode.label(SortTarget::Local),
                                sort_suffix,
                                filter_suffix,
                                diagnostics
                            )
                        });
                    block.render(content_area, frame.buffer_mut());
                    // 与 local_music 的鼠标命中共用同一份行账本（过滤行 → 表头 → 列表）。
                    let rows = components::hit_test::PanelRows::new(
                        content_area,
                        local_filter.is_visible(),
                        false,
                        true,
                    );
                    let inner = rows.inner;

                    if let Some(row) = rows.filter_row() {
                        local_filter.render(row, frame.buffer_mut(), ctx);
                    }

                    if inner.height < 2 {
                        break 'local_content;
                    }

                    if paths.is_empty() {
                        Paragraph::new(Line::from(format!(
                            " 未配置音乐目录，请在设置（{}）中添加",
                            NavTab::Settings.shortcut_digit()
                        )))
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
                        let rescan_hint =
                            match config_key_hint(ctx, Some("local"), Action::LocalRescan) {
                                Some(hint) => format!("，按 {hint} 重新扫描"),
                                None => String::new(),
                            };
                        Paragraph::new(Line::from(format!(" 目录下未找到音频文件{rescan_hint}")))
                            .style(Style::new().fg(Color::DarkGray))
                            .render(inner, frame.buffer_mut());
                        break 'local_content;
                    }

                    let Some(header_row) = rows.header else {
                        break 'local_content;
                    };
                    if rows.list.height < 2 {
                        break 'local_content;
                    }

                    let visible_height = rows.list.height as usize;
                    let sel = local_state.selected;
                    let mut sc = local_state.scroll;

                    if sel >= sc + visible_height {
                        sc = sel.saturating_sub(visible_height.saturating_sub(1));
                    } else if sel < sc {
                        sc = sel;
                    }
                    sc = sc.min(songs.len().saturating_sub(visible_height));
                    local_state.scroll = sc;

                    // 拖拽期间以页面状态为准：这里每帧都从 Config 重载的话，
                    // 鼠标刚算出来的宽度会在同一帧被覆盖回旧值，拖拽看起来毫无反应。
                    if local_state.column_resize.is_none() {
                        let cfg = ctx.config.read().unwrap_or_else(|e| e.into_inner());
                        local_state.columns = pages::components::song_table::load_columns_for_page(
                            &cfg.ui.table_columns,
                            local_state.page_key,
                            inner.width,
                        );
                    }

                    pages::components::song_table::header_paragraph(
                        inner.width,
                        &local_state.columns,
                        pages::components::song_table::TablePalette::from_theme(ctx),
                    )
                    .render(header_row, frame.buffer_mut());

                    let end = (sc + visible_height).min(songs.len());
                    for row in sc..end {
                        let Some(song) = songs.get(row) else {
                            break;
                        };
                        let i = row;
                        let row_paragraph = pages::components::song_table::row_paragraph(
                            song,
                            i,
                            inner.width,
                            &local_state.columns,
                            pages::components::song_table::TablePalette::from_theme(ctx),
                        );
                        // 数据行紧跟在列头下方：过滤条可见时整体下移一行，
                        // 否则 row 0 会把列头覆盖掉。
                        let line_area = Rect::new(
                            rows.list.x,
                            rows.list.y + (row - sc) as u16,
                            rows.list.width,
                            1,
                        );
                        let style = if i == sel {
                            Style::new()
                                .bg(crate::theme::accent(ctx))
                                .fg(crate::theme::selection_fg(ctx))
                        } else {
                            Style::new().fg(crate::theme::text(ctx))
                        };
                        row_paragraph
                            .style(style)
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

        // 页面里的文本输入已在渲染时登记了插入点。这里取出它并显式设置
        // `Frame::cursor_position`：不设置的话 ratatui 只隐藏光标、不移动它，
        // 光标会停在差分渲染最后一个变化的单元格上，输入法候选框就会在搜索框
        // 和状态栏之间来回跳（issue #42）。
        //
        // 任何全屏浮层都会盖住输入区，此时不能再把光标留在页面上。
        let overlay_covers_input = local_diagnostics.is_some()
            || qr_login_page.is_some()
            || sync_overlay.is_some()
            || song_menu.is_some()
            || help_page.is_some()
            || downloads_panel.is_open()
            || ctx
                .details_page
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some();
        let cursor_anchor = if overlay_covers_input {
            None
        } else {
            crate::ui_cursor::take()
        };

        if let Some(kind) = local_diagnostics {
            render_local_diagnostics(content_area, frame.buffer_mut(), ctx, *kind);
        }

        if let Some(page) = qr_login_page {
            use ratatui::widgets::{Clear, Widget};

            let overlay_area = calculate_qr_login_area(area);
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

        if let Some(sync) = sync_overlay.as_ref() {
            let overlay_area = Rect::new(
                area.x + 2,
                area.y + 2,
                area.width.saturating_sub(4),
                area.height.saturating_sub(4),
            );
            sync.render(overlay_area, frame.buffer_mut(), ctx);
        }

        // 频谱可视化：非模态叠加在内容区上（菜单/通知在其后再画，保持在上层）。
        //
        // 扫码登录、同步浮层、本地诊断这三类"画在频谱之前"的整屏提示必须让路：
        // 频谱整块盖上去会把它们糊掉，看起来就像界面卡死、点什么都没反应。
        let spectrum_blocked_by_modal =
            local_diagnostics.is_some() || qr_login_page.is_some() || sync_overlay.is_some();
        if !spectrum_blocked_by_modal
            && let Some(handle) = visualizer
            && let Some(snapshot) = handle.frame()
        {
            visualizer::render_data(
                main_chunks[2],
                frame.buffer_mut(),
                ctx,
                visualizer_palette.palette(ctx),
                &snapshot.data,
                &snapshot.peaks,
            );
        }
        components::progress_bar::render(main_chunks[3], frame.buffer_mut(), ctx);
        let sort_status = match active_tab {
            NavTab::Favorites => Some(favorites_page.sort_label()),
            NavTab::History => Some(history_state.mode.label(SortTarget::History)),
            NavTab::LocalMusic => Some(local_state.mode.label(SortTarget::Local)),
            _ => None,
        };
        // 排序段的键位提示按当前页面反查真实绑定（favorites/history/local 各自可配）。
        let sort_page = match active_tab {
            NavTab::Favorites => Some("favorites"),
            NavTab::History => Some("history"),
            NavTab::LocalMusic => Some("local"),
            _ => None,
        };
        let sort_hint =
            sort_page.and_then(|page| config_key_hint(ctx, Some(page), Action::ListCycleSort));
        let status_frame = components::status_bar::render(
            main_chunks[4],
            frame.buffer_mut(),
            ctx,
            sort_status,
            sort_hint,
            ui_areas.status_hover,
            ui_areas.status_handle_hover,
        );
        ui_areas.status_hits = status_frame.hits;
        ui_areas.status_collapsed = status_frame.collapsed;
        ui_areas.status_handle = status_frame.handle;
        // 命中用它画出来的实际矩形：布局把底栏挤矮时也不会"画 1 行、命中 3 行"。
        ui_areas.status = Rect::new(
            main_chunks[4].x,
            main_chunks[4].y,
            main_chunks[4].width,
            status_frame.rows,
        );
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

        // 有文本输入获得焦点时把终端光标钉在插入点：这既是输入框该有的光标，
        // 也是输入法候选框的定位依据。没有输入时保持 ratatui 的默认隐藏行为。
        if let Some(anchor) = cursor_anchor {
            frame.set_cursor_position(anchor);
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

fn calculate_qr_login_area(area: Rect) -> Rect {
    let qr_width = 66u16;
    let qr_height = 40u16;
    let w = qr_width.min(area.width.saturating_sub(4));
    let h = qr_height.min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

/// 发起一次二维码生成任务（初次打开扫码页与过期自动重建共用）。
fn spawn_qr_generate(
    rt: &tokio::runtime::Runtime,
    ctx: &AppContext,
    page: Arc<std::sync::Mutex<pages::qr_login::QrLoginPage>>,
    wake_tx: mpsc::UnboundedSender<AppAction>,
) -> tokio::task::JoinHandle<()> {
    let manager = Arc::clone(&ctx.source_manager);
    // 渠道也跟着页面走：过期自动重建时要重建同一条渠道，不能退回默认渠道。
    let (source_id, kind) = {
        let page = page.lock().unwrap_or_else(|e| e.into_inner());
        (page.source, page.kind)
    };
    rt.spawn(async move {
        let result = manager.create_qr_login_kind(source_id, kind).await;
        let mut page = page.lock().unwrap_or_else(|e| e.into_inner());
        match result {
            Ok(session) => page.set_qr(session),
            Err(error) => page.set_error(format!("生成二维码失败: {error}")),
        }
        let _ = wake_tx.send(AppAction::None);
    })
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
        // ── 底栏（快速控制栏）─────────────────────────────
        AppAction::TogglePlayPause => {
            toggle_or_start_current(ctx, rt, action_tx, search_page, settings_page, search_seq);
        }
        AppAction::SetPlayMode(value) => {
            let mode = crate::playlist::mode::PlayMode::from_config(&value);
            ctx.playlist.set_mode(mode);
            let save_result = {
                let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                config.player.play_mode = mode.as_config().to_string();
                crate::config::loader::save(&config, &ctx.config_path)
            };
            match save_result {
                Ok(()) => ctx.notify(Notification::success(format!("播放模式: {}", mode.label()))),
                Err(error) => ctx.notify(Notification::warning(format!(
                    "播放模式已切换，但保存失败: {error}"
                ))),
            }
        }
        AppAction::SetQuality(quality) => {
            let save_result = {
                let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                config.player.quality = quality;
                crate::config::loader::save(&config, &ctx.config_path)
            };
            // 只改偏好，不打断正在播放的歌：正在播的那首走"用当前音质重新解析"。
            match save_result {
                Ok(()) => ctx.notify(Notification::success(format!(
                    "音质偏好: {}",
                    quality.label()
                ))),
                Err(error) => ctx.notify(Notification::warning(format!(
                    "音质偏好已切换，但保存失败: {error}"
                ))),
            }
        }
        AppAction::SetSourcePolicy { policy, platform } => {
            let save_result = {
                let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                config.source.policy = policy;
                config.source.policy_platform = platform;
                crate::config::loader::save(&config, &ctx.config_path)
            };
            let label = match (policy, platform) {
                (SourcePolicy::Auto, _) => "解析策略: 自动".to_string(),
                (mode, Some(id)) => format!("解析策略: {} {}", mode.label(), id.as_str()),
                (mode, None) => format!("解析策略: {}", mode.label()),
            };
            match save_result {
                Ok(()) => ctx.notify(Notification::success(label)),
                Err(error) => ctx.notify(Notification::warning(format!(
                    "{label}，但保存失败: {error}"
                ))),
            }
            // 立即按新策略重新解析当前歌：否则用户点了"只用网易"在界面上
            // 看不出任何变化，会以为"点了没反应"。
            let song = ctx
                .current_song
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Some(song) = song {
                start_song_playback(song, false, None, true, ctx, rt, action_tx);
            }
        }
        AppAction::ReloadJsSources => {
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
                Arc::clone(&ctx.js_source_status),
            );
            ctx.notify(Notification::info("正在重新加载 JS 音源"));
        }
        AppAction::Search { keyword, source } => {
            let mut sp = search_page.lock().unwrap_or_else(|e| e.into_inner());
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
            sp.set_status("✓ 音源已加载并启用");
            drop(sp);
            let save_result = {
                let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                config.source.js_sources.retain(|item| item != &url);
                config.source.js_sources.insert(0, url);
                crate::config::loader::save(&config, &ctx.config_path)
            };
            if let Err(e) = save_result {
                let mut sp = settings_page.lock().unwrap_or_else(|e| e.into_inner());
                sp.set_status(format!("✗ 音源已启用，但保存配置失败: {e}"));
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
                    Arc::clone(&ctx.js_source_status),
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
            sp.set_status(format!("✗ 音源加载失败: {error}"));
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
                .set_status(if failures.is_empty() {
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
                Arc::clone(&ctx.js_source_status),
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
                    settings.set_status(format!("本地音乐扫描完成，共 {count} 首"));
                    let _ = tx.send(AppAction::ShowNotification(Notification::success(format!(
                        "本地音乐扫描完成，共 {} 首",
                        count
                    ))));
                } else {
                    settings.set_status(format!("扫描错误: {}", errors.join("; ")));
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
        AppAction::CommitColumnResize { page_key, columns } => {
            let save_result = {
                let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                config.ui.table_columns.insert(page_key.clone(), columns);
                crate::config::loader::save(&config, &ctx.config_path)
            };
            if let Err(e) = save_result {
                ctx.notify(Notification::error(format!("保存列宽配置失败: {}", e)));
            }
        }
        // 键盘菜单入口在 run_app 里就地处理（菜单状态是那里的局部变量），
        // 走到这里说明没有可作用的页面，忽略即可。
        AppAction::OpenContextMenu => {}
        // 远程歌单窗口的回车：切页与选中是循环本地状态，排给主循环执行。
        AppAction::OpenAccountPlaylist(remote_id) => {
            ctx.queue_status_bar_command(StatusBarCommand::OpenAccountPlaylist(remote_id));
        }
        AppAction::CommitPaneRatio {
            page_key,
            ratio_key,
            ratio,
        } => {
            let save_result = {
                let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                config
                    .ui
                    .pane_ratios
                    .entry(page_key)
                    .or_default()
                    .insert(ratio_key, ratio);
                crate::config::loader::save(&config, &ctx.config_path)
            };
            if let Err(e) = save_result {
                ctx.notify(Notification::error(format!("保存面板布局失败: {}", e)));
            }
        }
        AppAction::ResetColumnWidths { page_key } => {
            let save_result = {
                let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                config.ui.table_columns.remove(&page_key);
                crate::config::loader::save(&config, &ctx.config_path)
            };
            if let Err(e) = save_result {
                ctx.notify(Notification::error(format!("恢复默认列宽失败: {}", e)));
            }
        }
        AppAction::ResetPaneLayout { page_key } => {
            // 删掉持久化比例；内存里的那一份由主循环按页面复位
            // （否则用户要重启才看得到变化）。
            let save_result = {
                let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                config.ui.pane_ratios.remove(&page_key);
                crate::config::loader::save(&config, &ctx.config_path)
            };
            ctx.queue_status_bar_command(StatusBarCommand::ResetPaneLayout(page_key));
            if let Err(e) = save_result {
                ctx.notify(Notification::error(format!("恢复默认布局失败: {}", e)));
            }
        }
        AppAction::Navigate(_)
        | AppAction::GoBack
        | AppAction::Quit
        | AppAction::None
        | AppAction::QrLogin(..)
        | AppAction::QrLogout(_)
        | AppAction::QrLoginSuccess(_)
        | AppAction::SyncNetease
        | AppAction::SyncQq
        // 推送要开 sync_overlay（主循环本地状态），由主循环动作泵处理。
        | AppAction::PushLocalPlaylist { .. }
        | AppAction::PushFavorites => {
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

/// 播放器报错后，是否值得换源重试这首歌。
///
/// 只有**真本地文件**不该重试：文件本身有问题，换到任何在线音源都没有意义。
///
/// 不能写成 `song.source != SourceId::Local`：JS 音源搜到的歌同样标记为
/// `SourceId::Local`（靠 `extra["source"]` 记录真实平台），那样会把它们和真本地
/// 文件一起排除掉 —— 表现就是"JS 音源搜到的歌播放失败后直接跳下一首，不换源"。
/// 项目里已有 `is_local_file_song()` 承担这份区分，这里直接复用，保证与播放地址
/// 解析路径（`get_song_url_inner`）是同一份判定。
fn should_retry_with_other_source(song: &SongInfo, auto_toggle: bool) -> bool {
    auto_toggle && !lx_source::manager::is_local_file_song(song)
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
    let (quality, auto_toggle, fade_in_ms, policy, policy_platform) = {
        let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
        (
            config.player.quality,
            config.source.auto_toggle,
            config.player.fade_in_ms,
            config.source.policy,
            config.source.policy_platform,
        )
    };
    let tx = action_tx.clone();

    rt.spawn(async move {
        let resolved = tokio::time::timeout(
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
                    policy,
                    policy_platform,
                },
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

        // 封面地址统一走 `resolve_cover_url`：残缺地址（只剩域名之类）会被
        // 挡下并跨源重找一次，找不到合格地址时保留原值，由封面面板显示原因。
        let from_song_url = song_url.cover_url.clone();
        resolved_song.cover_url = voicefox_runtime::resolve_cover_url(
            &source_mgr,
            &resolved_song,
            from_song_url,
            voicefox_runtime::is_usable_remote_url,
            Duration::from_secs(10),
        )
        .await;
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
            resolved_song.source.display_name()
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
    /// 解析策略（`auto` 时与历史行为完全一致）。
    policy: SourcePolicy,
    /// 策略作用的平台。
    policy_platform: Option<SourceId>,
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
        policy,
        policy_platform,
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
        format!("音源 {} 已尝试", song.source.display_name())
    };

    if play_request_id.load(Ordering::SeqCst) != request_id {
        return Ok(None);
    }
    if !auto_toggle {
        return Err(format!("获取播放地址失败: {}", direct_error));
    }

    // `only X`：解析集合收窄为 {X} —— 直接去 X 上找同曲，找不到就失败，
    // 不去别的平台兜底。`prefer X` 只调整下面的候选顺序，来源集合不变。
    let candidates = match only_platform(&song, policy, policy_platform) {
        Some(platform) => source_manager.find_music_on(&song, &[platform]).await,
        None => {
            let mut candidates = source_manager.find_music(&song).await;
            order_fallback_candidates(&mut candidates, policy, policy_platform);
            candidates
        }
    };
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

/// `prefer`：把指定平台的候选排到最前，其余保持原顺序（稳定排序）。
///
/// 只改"先试谁"，不改来源集合、不改匹配规则 —— `only` 的收窄在调用处完成。
fn order_fallback_candidates(
    candidates: &mut [lx_core::model::song::SongInfo],
    policy: SourcePolicy,
    platform: Option<SourceId>,
) {
    if policy != SourcePolicy::Prefer {
        return;
    }
    let Some(platform) = platform else {
        return;
    };
    candidates.sort_by_key(|candidate| u8::from(candidate.source != platform));
}

/// `only X` 是否适用（歌曲本身已经来自 X 时无需收窄）。
fn only_platform(
    song: &lx_core::model::song::SongInfo,
    policy: SourcePolicy,
    platform: Option<SourceId>,
) -> Option<SourceId> {
    match (policy, platform) {
        (SourcePolicy::Only, Some(platform)) if song.source != platform => Some(platform),
        _ => None,
    }
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

type LoadedJsSources = Vec<(String, Arc<dyn lx_core::traits::source::MusicSource>)>;

/// 逐个加载 JS 音源，返回（成功的音源，失败记录）。
///
/// - 单个失败**只记录、不中断**，其它音源照常加载（原有行为，保持不变）；
/// - 成功列表保持配置顺序 —— 这个顺序就是播放时的尝试顺序，即优先级；
/// - 失败记录同样保持配置顺序，便于和配置逐行对照；
/// - 期间若出现更新的加载请求（代次变化），返回 `None` 表示本次结果作废。
async fn load_js_sources(
    urls: Vec<String>,
    default_source: &str,
    source_manager: &lx_source::manager::SourceManager,
    generation: u64,
) -> Option<(LoadedJsSources, Vec<JsSourceFailure>)> {
    let mut loaded: LoadedJsSources = Vec::with_capacity(urls.len());
    let mut failures = Vec::new();
    for url in urls {
        match lx_source::js::loader::load_source(&url, default_source).await {
            Ok(source) => loaded.push((url, Arc::new(source))),
            Err(reason) => {
                if !source_manager.is_js_source_request_current(generation) {
                    return None;
                }
                // 日志保留：通知会随超时消失，日志是事后排查的依据。
                tracing::warn!("load JS source failed ({url}): {reason}");
                let name = lx_source::js::loader::source_display_name(
                    &lx_source::js::loader::cached_source_path(&url),
                    &url,
                );
                failures.push(JsSourceFailure {
                    name,
                    origin: url,
                    reason,
                });
            }
        }
    }
    Some((loaded, failures))
}

/// 把失败记录拼成一行给用户看的文本：`名称（来源）：原因`。
///
/// 用 `；` 连接而不是换行 —— 应用内通知正文是单行 `Line`，换行符不会被拆开，
/// 折行交给 `Paragraph` 自己处理。
fn describe_js_failures(failures: &[JsSourceFailure]) -> String {
    failures
        .iter()
        .map(|failure| format!("{}（{}）：{}", failure.name, failure.origin, failure.reason))
        .collect::<Vec<_>>()
        .join("；")
}

/// 组装 JS 音源加载结果的应用内通知。
///
/// 部分失败时**必须点名**是哪些音源、为什么失败，只报数量用户无从下手。
fn js_source_load_notification(
    loaded_count: usize,
    total: usize,
    failures: &[JsSourceFailure],
) -> Notification {
    if loaded_count == 0 {
        return Notification::error(format!(
            "JS 音源全部加载失败（共 {total} 个）：{}",
            describe_js_failures(failures)
        ));
    }
    if failures.is_empty() {
        return Notification::success(format!("{loaded_count} 个 JS 音源已就绪"));
    }
    Notification::warning(format!(
        "已加载 {loaded_count}/{total} 个 JS 音源，失败：{}",
        describe_js_failures(failures)
    ))
}

fn spawn_js_source_loader(
    urls: Vec<String>,
    default_source: String,
    source_manager: Arc<lx_source::manager::SourceManager>,
    generation: u64,
    tx: mpsc::UnboundedSender<AppAction>,
    rt: &tokio::runtime::Runtime,
    js_status: Arc<std::sync::Mutex<JsSourceStatus>>,
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
        let Some((loaded, failures)) =
            load_js_sources(urls, &default_source, &source_manager, generation).await
        else {
            return;
        };

        let loaded_count = loaded.len();
        if !source_manager.set_named_js_sources_if_current(generation, loaded) {
            return;
        }
        // 留存逐项状态：状态栏与音源菜单都要显示"3 个里坏了哪个、为什么"，
        // 只发一条会消失的通知是不够的。
        *js_status.lock().unwrap_or_else(|e| e.into_inner()) = JsSourceStatus {
            total,
            loaded: loaded_count,
            failures: failures.clone(),
        };
        let _ = tx.send(AppAction::ShowNotification(js_source_load_notification(
            loaded_count,
            total,
            &failures,
        )));
    });
}

/// 兜底清理所有页面进行中的拖拽会话。
///
/// 触发场景：鼠标在内容区之外松开（页面根本收不到 `Up`）、键盘切页、终端 resize。
/// 缺少这一步时残留的拖拽状态会让该页后续鼠标事件被拖拽分支的
/// `_ => return AppAction::None` 永久吞掉，表现为"这一页突然点不动了"。
#[allow(clippy::too_many_arguments)]
fn abort_all_drag_sessions(
    main_page: &mut pages::main_page::MainPage,
    leaderboard: &mut pages::leaderboard::LeaderboardPage,
    playlists: &mut pages::playlists::PlaylistsPage,
    favorites_page: &mut pages::favorites::FavoritesPage,
    search_page: &Arc<std::sync::Mutex<pages::search::SearchPage>>,
    settings_page: &Arc<std::sync::Mutex<pages::settings::SettingsPage>>,
    history_state: &mut SortState,
    local_state: &mut SortState,
) {
    main_page.abort_drag_sessions();
    leaderboard.abort_drag_sessions();
    playlists.abort_drag_sessions();
    favorites_page.abort_drag_sessions();
    search_page
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .abort_drag_sessions();
    settings_page
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .abort_drag_sessions();
    history_state.cancel_column_resize();
    local_state.cancel_column_resize();
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
                page,
                append,
            } => {
                // 账号歌单（「我的歌单」入口）是远程缓存的只读镜像：列表全在
                // 内存里，既不翻页也不发请求。
                //
                // 音源入口（网易云）仍然走音源自己的歌单接口：登录之后用户
                // 既需要账号歌单，也需要音源提供的推荐歌单，两者是两套数据，
                // 以前用缓存顶掉音源列表会让推荐歌单彻底不可见。
                let account = if source == SourceId::Wy {
                    crate::remote_cache::with_netease(|collections| {
                        collections
                            .iter()
                            .filter(|c| c.kind == lx_core::sync::SyncCollectionKind::Playlist)
                            .map(|c| lx_core::model::playlist::Playlist {
                                id: format!("netease-account:{}", c.id),
                                name: c.name.clone(),
                                source: SourceId::Wy,
                                cover_url: c.songs.first().and_then(|s| s.cover_url.clone()),
                                song_count: c.songs.len() as u32,
                                description: None,
                                play_count: None,
                                creator: None,
                                link: None,
                                extra: Default::default(),
                            })
                            .collect::<Vec<_>>()
                    })
                } else {
                    Vec::new()
                };
                let result = tokio::time::timeout(
                    Duration::from_secs(12),
                    source_manager.playlists(source, "", page),
                )
                .await
                .map(|r| r.map_err(|e| e.to_string()))
                .unwrap_or_else(|_| Err("请求超时，请稍后重试".to_string()));
                // 音源歌单接口失败但本地有账号歌单时，至少把账号歌单给出去，
                // 不让页面停在空列表 + 报错态。
                let result = match result {
                    Ok(items) => Ok(items),
                    Err(error) if !account.is_empty() && page <= 1 => {
                        tracing::debug!(
                            "playlist list failed, falling back to account cache: {error}"
                        );
                        Ok(account)
                    }
                    Err(error) => Err(error),
                };
                PlaylistResponse::List {
                    request_id,
                    source,
                    page,
                    append,
                    result,
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
                let result = if source == SourceId::Wy {
                    // 「我的歌单」入口的 id 带虚拟前缀，取缓存前要还原成远端 id
                    let cached = crate::remote_cache::playlist(
                        playlist_id
                            .strip_prefix("netease-account:")
                            .unwrap_or(&playlist_id),
                    );
                    if let Some(collection) = cached {
                        Ok(collection.songs)
                    } else if playlist_id.starts_with("netease-account:") {
                        // 账号歌单只存在于缓存里，缓存没有就是真没有
                        Err("该歌单不在本地缓存中，请先在设置里刷新远程歌单".to_string())
                    } else {
                        tokio::time::timeout(
                            timeout,
                            source_manager.playlist_detail(source, &playlist_id, 1),
                        )
                        .await
                        .map(|r| r.map_err(|e| e.to_string()))
                        .unwrap_or_else(|_| Err("请求超时，请稍后重试".to_string()))
                    }
                } else {
                    tokio::time::timeout(
                        timeout,
                        source_manager.playlist_detail(source, &playlist_id, 1),
                    )
                    .await
                    .map(|r| r.map_err(|e| e.to_string()))
                    .unwrap_or_else(|_| Err("请求超时，请稍后重试".to_string()))
                };
                PlaylistResponse::Songs {
                    request_id,
                    source,
                    playlist_id,
                    result,
                }
            }
        };
        let _ = playlist_tx.send(response);
    });
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
    use lx_core::model::song::SongInfo;
    use lx_core::model::source::SourceId;

    use super::JsSourceStatus;
    use super::{
        ClickTracker, DeleteConfirmationAction, JsSourceFailure, delete_confirmation_action,
        describe_js_failures, js_source_load_notification, load_js_sources, next_list_index,
        only_platform, order_fallback_candidates, playback_restore_flags, previous_list_index,
        should_expand_bili_parts, should_go_to_main, should_retry_with_other_source,
        should_scan_local_music_on_entry,
    };
    use crate::pages::sidebar::NavTab;
    use crate::storage::SavedPlayerState;
    use lx_core::events::NotificationLevel;
    use lx_core::model::config::SourcePolicy;

    #[test]
    fn double_click_tolerates_a_two_pixel_jitter() {
        use crossterm::event::MouseButton;

        let mut tracker = ClickTracker::default();
        let click = |column: u16, row: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        // 2px 以内的手抖仍算同一次双击。
        assert!(!tracker.is_double_click(click(10, 10)));
        assert!(tracker.is_double_click(click(12, 10)));
        // 双击完成后重新计数：第三次点击不是双击。
        assert!(!tracker.is_double_click(click(12, 10)));
        // 超出邻域（>2px）是两次独立单击。
        assert!(!tracker.is_double_click(click(18, 10)));
        assert!(!tracker.is_double_click(click(18, 14)));
    }

    /// 列边界命中必须按"表头实际宽度"换算，不能用整块内容区宽度：主页面
    /// 宽布局下队列只占右侧一栏，两者宽度不同，用错就会把"拖边界改宽"误判
    /// 成"拖表头换列"，表现就是相邻列互相卡顿。
    #[test]
    fn column_boundary_uses_the_header_width_not_the_content_width() {
        use crate::pages::components::song_table::{
            compute_layout, default_columns, find_boundary,
        };
        use ratatui::layout::Rect;

        let header = Rect::new(40, 6, 72, 1);
        let content_width = 150u16;
        let columns = default_columns(header.width);

        let layout = compute_layout(&columns, header.width);
        // 第 0、1 列之间那条分隔线在表头内的局部 x。
        let boundary = layout[0].start_x + layout[0].width;

        assert!(
            find_boundary(&columns, header.width, boundary).is_some(),
            "按表头宽度应当命中分隔线"
        );
        assert!(
            find_boundary(&columns, content_width.saturating_sub(2), boundary).is_none(),
            "内容区宽度不能拿来判定表头分隔线"
        );
    }

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

    // ── 播放器级重试判定 ──
    //
    // 旧判断是 `song.source != SourceId::Local`，它把"JS 音源搜到的歌"和
    // "真本地文件"混在一起（两者 source 都是 Local），于是 JS 搜到的歌
    // 播放失败后会被直接跳过而不是换源重试。

    #[test]
    fn only_real_local_files_skip_the_remote_retry() {
        // JS 音源搜到的歌：source=Local + extra["source"]=真实平台
        let mut js_song = SongInfo::new("1".into(), SourceId::Local, "歌".into(), "手".into());
        js_song.extra.insert("source".into(), "wy".into());
        assert!(
            should_retry_with_other_source(&js_song, true),
            "JS 音源搜到的歌必须换源重试"
        );

        // JS 搜到的歌即使后来落到本地路径，也仍然是可换源的歌
        let mut downloaded = js_song.clone();
        downloaded.file_path = Some(std::path::PathBuf::from("/music/b.flac"));
        assert!(should_retry_with_other_source(&downloaded, true));

        // 真本地文件：不该换源重试
        let mut local = SongInfo::new("2".into(), SourceId::Local, "歌".into(), "手".into());
        local.file_path = Some(std::path::PathBuf::from("/music/a.flac"));
        assert!(!should_retry_with_other_source(&local, true));

        // 没有 file_path 的中间态同样是本地文件（扫描器是后补 file_path 的）
        let bare_local = SongInfo::new("3".into(), SourceId::Local, "歌".into(), "手".into());
        assert!(!should_retry_with_other_source(&bare_local, true));

        // 在线歌曲：应当重试
        for source in [SourceId::Wy, SourceId::Kw, SourceId::Kg, SourceId::Bili] {
            let online = SongInfo::new("4".into(), source, "歌".into(), "手".into());
            assert!(
                should_retry_with_other_source(&online, true),
                "{source:?} 播放失败应当换源重试"
            );
        }

        // 关闭自动换源时谁都不重试
        assert!(!should_retry_with_other_source(&js_song, false));
    }

    // ── JS 音源加载失败的可观测性 ──

    fn js_failure(name: &str, origin: &str, reason: &str) -> JsSourceFailure {
        JsSourceFailure {
            name: name.to_string(),
            origin: origin.to_string(),
            reason: reason.to_string(),
        }
    }

    #[test]
    fn failure_text_names_the_source_its_origin_and_the_reason() {
        let failures = vec![
            js_failure(
                "huibq",
                "https://example.com/pdone/huibq/latest.js",
                "下载 JS 音源失败（HTTP 500）",
            ),
            js_failure("grass", "/home/me/grass.js", "读取本地 JS 音源失败"),
        ];
        let text = describe_js_failures(&failures);
        for expected in [
            "huibq",
            "https://example.com/pdone/huibq/latest.js",
            "HTTP 500",
            "grass",
            "/home/me/grass.js",
            "读取本地 JS 音源失败",
        ] {
            assert!(text.contains(expected), "失败文本缺少 {expected}：{text}");
        }
    }

    #[test]
    fn partial_failure_notification_points_at_the_failing_sources() {
        let failures = vec![js_failure(
            "huibq",
            "https://example.com/huibq/latest.js",
            "HTTP 500",
        )];
        let notification = js_source_load_notification(2, 3, &failures);
        assert_eq!(notification.level, NotificationLevel::Warn);
        assert!(
            notification.message.contains("2/3"),
            "{}",
            notification.message
        );
        assert!(
            notification.message.contains("huibq"),
            "{}",
            notification.message
        );
        assert!(
            notification.message.contains("HTTP 500"),
            "{}",
            notification.message
        );
    }

    #[test]
    fn total_failure_is_an_error_and_success_is_a_success() {
        let failures = vec![js_failure("a", "https://example.com/a.js", "HTTP 404")];
        let all_failed = js_source_load_notification(0, 1, &failures);
        assert_eq!(all_failed.level, NotificationLevel::Error);
        assert!(
            all_failed.message.contains("HTTP 404"),
            "{}",
            all_failed.message
        );

        let ready = js_source_load_notification(3, 3, &[]);
        assert_eq!(ready.level, NotificationLevel::Success);
    }

    /// 每个音源都要有独立的失败记录，且顺序与配置一致（脱网、确定性）。
    #[tokio::test]
    async fn every_failing_js_source_gets_its_own_record_in_config_order() {
        let urls = vec![
            "/nonexistent/voicefox-missing-a.js".to_string(),
            "/nonexistent/voicefox-missing-b.js".to_string(),
        ];
        let manager = lx_source::manager::SourceManager::new(SourceId::Kw, SourceId::all_online());
        let generation = manager.begin_js_source_request(true);
        let (loaded, failures) = load_js_sources(urls.clone(), "kw", &manager, generation)
            .await
            .expect("代次未变化，应当返回结果");

        assert!(loaded.is_empty());
        assert_eq!(failures.len(), 2, "每个音源都要有自己的记录：{failures:?}");
        assert_eq!(failures[0].origin, urls[0], "失败记录必须保持配置顺序");
        assert_eq!(failures[1].origin, urls[1]);
        for failure in &failures {
            assert!(!failure.name.is_empty(), "必须给出可读的音源名称");
            assert!(!failure.reason.is_empty(), "必须保留真实失败原因");
        }
    }

    /// 加载期间出现更新的请求时，本次结果作废，不能用旧结果覆盖新音源列表。
    #[tokio::test]
    async fn a_superseded_js_source_load_is_discarded() {
        let manager = lx_source::manager::SourceManager::new(SourceId::Kw, SourceId::all_online());
        let stale = manager.begin_js_source_request(true);
        // 模拟用户在加载途中又保存了一次配置
        manager.begin_js_source_request(true);

        let result = load_js_sources(
            vec!["/nonexistent/voicefox-missing.js".to_string()],
            "kw",
            &manager,
            stale,
        )
        .await;
        assert!(result.is_none(), "过期请求必须作废");
    }

    /// 环境是否具备加载 JS 音源的条件：node 可用 + 两个缓存目录可写。
    ///
    /// 显式探测而不去匹配错误串：这样环境不满足时明确跳过，一旦真的跑起来，
    /// 断言就是严格的（不会因为环境问题假绿）。
    fn js_source_runtime_ready() -> bool {
        if std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("跳过：node 不可用");
            return false;
        }
        // loader 写脚本缓存用 dirs::config_dir()/lx-tui/sources；
        // engine 写 wrapper.js 用 dirs::cache_dir()/voicefox/js。
        let script_cache = lx_source::js::loader::cached_source_path("voicefox-probe");
        let engine_cache = dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("voicefox")
            .join("js");
        for dir in [script_cache.parent(), Some(engine_cache.as_path())]
            .into_iter()
            .flatten()
        {
            if std::fs::create_dir_all(dir).is_err()
                || std::fs::write(dir.join(".voicefox-write-probe"), b"probe").is_err()
            {
                eprintln!("跳过：缓存目录不可写（{}）", dir.display());
                return false;
            }
        }
        true
    }

    /// 一个音源失败不影响其它音源：中间的有效音源照常加载，顺序保持。
    #[tokio::test]
    async fn one_failing_js_source_does_not_block_the_others() {
        if !js_source_runtime_ready() {
            return;
        }
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../source/tests/fixtures/user_api_v3.js"
        );

        let urls = vec![
            "/nonexistent/voicefox-missing-first.js".to_string(),
            fixture.to_string(),
            "/nonexistent/voicefox-missing-last.js".to_string(),
        ];
        let manager = lx_source::manager::SourceManager::new(SourceId::Kw, SourceId::all_online());
        let generation = manager.begin_js_source_request(true);
        let (loaded, failures) = load_js_sources(urls.clone(), "kw", &manager, generation)
            .await
            .expect("代次未变化，应当返回结果");

        assert_eq!(loaded.len(), 1, "中间的有效音源应当加载成功");
        assert_eq!(loaded[0].0, fixture, "成功列表必须保持配置顺序");
        assert_eq!(failures.len(), 2, "两侧的无效音源各有一条失败记录");
        assert_eq!(failures[0].origin, urls[0]);
        assert_eq!(failures[1].origin, urls[2]);
        assert_eq!(
            failures
                .iter()
                .filter(|failure| failure.reason.is_empty())
                .count(),
            0,
            "失败记录必须带原因"
        );
    }

    // ── 解析策略（auto / prefer / only）──

    fn fallback_candidate(source: SourceId) -> SongInfo {
        SongInfo::new("1".into(), source, "晴天".into(), "周杰伦".into())
    }

    /// `auto` 必须与历史行为完全一致：候选顺序一根手指都不动。
    #[test]
    fn auto_policy_leaves_the_candidate_order_untouched() {
        let mut candidates = vec![
            fallback_candidate(SourceId::Kw),
            fallback_candidate(SourceId::Wy),
            fallback_candidate(SourceId::Kg),
        ];
        let before: Vec<SourceId> = candidates.iter().map(|song| song.source).collect();
        order_fallback_candidates(&mut candidates, SourcePolicy::Auto, Some(SourceId::Wy));
        let after: Vec<SourceId> = candidates.iter().map(|song| song.source).collect();
        assert_eq!(before, after, "auto 不允许改变候选顺序");
    }

    /// `prefer X`：X 的候选排到最前，其余保持相对顺序（稳定排序）。
    #[test]
    fn prefer_policy_moves_the_platform_to_the_front_stably() {
        let mut candidates = vec![
            fallback_candidate(SourceId::Kw),
            fallback_candidate(SourceId::Wy),
            fallback_candidate(SourceId::Kg),
            fallback_candidate(SourceId::Wy),
        ];
        order_fallback_candidates(&mut candidates, SourcePolicy::Prefer, Some(SourceId::Wy));
        let order: Vec<SourceId> = candidates.iter().map(|song| song.source).collect();
        assert_eq!(
            order,
            vec![SourceId::Wy, SourceId::Wy, SourceId::Kw, SourceId::Kg],
            "指定平台排最前，其余保持原相对顺序"
        );
    }

    #[test]
    fn prefer_without_a_platform_is_a_no_op() {
        let mut candidates = vec![
            fallback_candidate(SourceId::Kw),
            fallback_candidate(SourceId::Wy),
        ];
        order_fallback_candidates(&mut candidates, SourcePolicy::Prefer, None);
        let order: Vec<SourceId> = candidates.iter().map(|song| song.source).collect();
        assert_eq!(order, vec![SourceId::Kw, SourceId::Wy]);
    }

    /// `only X`：只对"不是来自 X"的歌收窄来源集合。
    #[test]
    fn only_policy_applies_to_songs_from_other_platforms() {
        let song = fallback_candidate(SourceId::Kw);
        assert_eq!(
            only_platform(&song, SourcePolicy::Only, Some(SourceId::Wy)),
            Some(SourceId::Wy)
        );
        // 歌曲本来就来自 X：直接走原路径，无需收窄
        let same = fallback_candidate(SourceId::Wy);
        assert_eq!(
            only_platform(&same, SourcePolicy::Only, Some(SourceId::Wy)),
            None
        );
        // 其它策略不收窄
        assert_eq!(
            only_platform(&song, SourcePolicy::Auto, Some(SourceId::Wy)),
            None
        );
        assert_eq!(
            only_platform(&song, SourcePolicy::Prefer, Some(SourceId::Wy)),
            None
        );
        // 没指定平台时不收窄
        assert_eq!(only_platform(&song, SourcePolicy::Only, None), None);
    }

    // ── JS 音源状态摘要 ──

    #[test]
    fn js_source_status_summary_reports_partial_failures() {
        let healthy = JsSourceStatus {
            total: 3,
            loaded: 3,
            failures: Vec::new(),
        };
        assert_eq!(healthy.summary(), "● 自定义音源 3/3");
        assert!(healthy.is_healthy());

        let partial = JsSourceStatus {
            total: 3,
            loaded: 2,
            failures: vec![JsSourceFailure {
                name: "huibq".to_string(),
                origin: "https://example.com/huibq/latest.js".to_string(),
                reason: "HTTP 500".to_string(),
            }],
        };
        assert_eq!(partial.summary(), "▲ 自定义音源 2/3");
        assert!(!partial.is_healthy(), "缺一个就不该显示为健康");

        let none = JsSourceStatus::default();
        assert_eq!(none.summary(), "自定义音源 未配置");
        assert!(none.is_healthy(), "没配置不该报红");
    }

    // ── 底栏高度拖拽 ──

    /// 底栏鼠标分派：拖拽优先于段点击，且把手不会被当成段。
    #[test]
    fn status_bar_resize_drag_never_routes_to_a_segment_click() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        use lx_core::model::config::StatusBarItem;
        use ratatui::layout::Rect;

        use super::{StatusBarMouseRoute, route_status_bar_mouse};
        use crate::pages::components::status_bar::{
            StatusBarHit, StatusBarSlot, resize_handle, rows_for_pointer,
        };

        let mouse = |kind: MouseEventKind, column: u16, row: u16| MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        // 屏幕 24 行：底栏占 2 行（y=22..24），把手在顶行最右端。
        let status = Rect::new(0, 22, 80, 2);
        let handle = resize_handle(status);
        let hits = vec![
            StatusBarHit {
                slot: StatusBarSlot::Item(StatusBarItem::Volume),
                rect: Rect::new(0, 22, 10, 1),
            },
            StatusBarHit {
                slot: StatusBarSlot::Item(StatusBarItem::Queue),
                rect: Rect::new(0, 23, 10, 1),
            },
        ];

        // 未拖拽时：按在把手上 = 开始拖高度（不是点段）
        assert_eq!(
            route_status_bar_mouse(
                mouse(MouseEventKind::Down(MouseButton::Left), 79, 22),
                24,
                handle,
                &hits,
                false
            ),
            StatusBarMouseRoute::BeginResize
        );
        // 未拖拽时：把手以外的左键 / 右键仍然是段点击
        assert_eq!(
            route_status_bar_mouse(
                mouse(MouseEventKind::Down(MouseButton::Left), 3, 22),
                24,
                handle,
                &hits,
                false
            ),
            StatusBarMouseRoute::Click {
                slot: StatusBarSlot::Item(StatusBarItem::Volume),
                right: false,
            }
        );
        assert_eq!(
            route_status_bar_mouse(
                mouse(MouseEventKind::Down(MouseButton::Right), 3, 23),
                24,
                handle,
                &hits,
                false
            ),
            StatusBarMouseRoute::Click {
                slot: StatusBarSlot::Item(StatusBarItem::Queue),
                right: true,
            }
        );
        // 纯悬停（没有按键）不会开始拖拽，也不会当成点击
        assert_eq!(
            route_status_bar_mouse(
                mouse(MouseEventKind::Moved, 3, 22),
                24,
                handle,
                &hits,
                false
            ),
            StatusBarMouseRoute::None
        );
        // 段间分隔符上什么也不做
        assert_eq!(
            route_status_bar_mouse(
                mouse(MouseEventKind::Down(MouseButton::Left), 40, 22),
                24,
                handle,
                &hits,
                false
            ),
            StatusBarMouseRoute::None
        );

        // 拖拽中：即使指针正压在「音量」段上，也只能更新行数 —— 绝不能变成点击
        for kind in [
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Moved,
        ] {
            assert_eq!(
                route_status_bar_mouse(mouse(kind, 3, 21), 24, handle, &hits, true),
                StatusBarMouseRoute::Resize(3),
                "拖拽期间的段上事件必须只改高度"
            );
        }
        // 拖拽中：往上拖到 y=18 → 6 行（上限）
        assert_eq!(
            route_status_bar_mouse(
                mouse(MouseEventKind::Drag(MouseButton::Left), 3, 18),
                24,
                handle,
                &hits,
                true
            ),
            StatusBarMouseRoute::Resize(rows_for_pointer(24, 18))
        );
        // 松开左键 → 提交（写盘在事件循环里做）
        assert_eq!(
            route_status_bar_mouse(
                mouse(MouseEventKind::Up(MouseButton::Left), 3, 20),
                24,
                handle,
                &hits,
                true
            ),
            StatusBarMouseRoute::CommitResize(4)
        );
        // 拖拽中的右键不会被当成菜单
        assert_eq!(
            route_status_bar_mouse(
                mouse(MouseEventKind::Down(MouseButton::Right), 3, 22),
                24,
                handle,
                &hits,
                true
            ),
            StatusBarMouseRoute::None
        );
    }
}
