//! rmpc 风格播放队列页面。

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lx_core::events::{AppAction, Notification};
use lx_core::keybinding::{Action, KeybindingResolver};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};

use crate::context::AppContext;
use crate::cover::{CoverGeometry, CoverRenderer, CoverState};
use crate::pages::components::context_menu::MenuHitSource;
use crate::pages::components::hit_test::{PANEL_BORDERS, panel_inner};
use crate::pages::components::splitter::{
    DividerHit, GUTTER, SplitAxis, Splitter, clamp_extent, clamp_ratio, divider_line, ratio_within,
    split_with_gutter,
};

/// “D 清空整个队列”的确认窗口：首次按下武装，窗口内再按一次才执行。
const CLEAR_QUEUE_CONFIRM_WINDOW: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueueEditCommand {
    MoveUp,
    MoveDown,
    RemoveSelected,
    Clear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResizeTarget {
    WideColumns,
    WideCoverLyrics,
    NarrowQueueLyrics,
}

const DEFAULT_WIDE_COLUMNS_RATIO: f32 = 0.36;
const DEFAULT_WIDE_COVER_RATIO: f32 = 0.52;
const DEFAULT_NARROW_QUEUE_RATIO: f32 = 0.62;
/// 页面在 `ui.pane_ratios` 里的 key。
const MP_PAGE_KEY: &str = "queue";

fn queue_edit_command(key: &KeyEvent) -> Option<QueueEditCommand> {
    match (key.modifiers, key.code) {
        (KeyModifiers::SHIFT, KeyCode::Up)
        | (KeyModifiers::SHIFT, KeyCode::Char('k' | 'K'))
        | (KeyModifiers::NONE, KeyCode::Char('K')) => Some(QueueEditCommand::MoveUp),
        (KeyModifiers::SHIFT, KeyCode::Down)
        | (KeyModifiers::SHIFT, KeyCode::Char('j' | 'J'))
        | (KeyModifiers::NONE, KeyCode::Char('J')) => Some(QueueEditCommand::MoveDown),
        (KeyModifiers::NONE, KeyCode::Char('d') | KeyCode::Delete) => {
            Some(QueueEditCommand::RemoveSelected)
        }
        (KeyModifiers::SHIFT, KeyCode::Char('d' | 'D'))
        | (KeyModifiers::NONE, KeyCode::Char('D')) => Some(QueueEditCommand::Clear),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy)]
struct MainLayout {
    wide: bool,
    left: Rect,
    queue: Rect,
    cover: Rect,
    lyric: Rect,
    cover_geometry: Option<CoverGeometry>,
}

/// 当前布局下可拖拽的分割线。
///
/// 方向由 `layout.wide` 明确决定：宽布局只有"左栏|队列"竖线和"封面|歌词"横线，
/// 窄布局只有"队列|歌词"横线。**不要**把横竖判定串行无条件求值。
///
/// 坐标一律来自 [`divider_line`]（= 第一块的 `right()`/`bottom()`），也就是
/// `split_with_gutter` 留在两块之间的那 1 格 gutter：渲染 [`MainPage::render`]
/// 与命中 [`MainPage::resize_target_at`] 都走这里，两边不可能不同步。
fn dividers(layout: &MainLayout) -> Vec<(ResizeTarget, DividerHit)> {
    let mut out = Vec::new();
    if layout.wide {
        if layout.left.width > 0 && layout.queue.width > 0 {
            out.push((
                ResizeTarget::WideColumns,
                DividerHit::new(
                    SplitAxis::Vertical,
                    divider_line(layout.left, SplitAxis::Vertical),
                    (layout.left.y, layout.left.bottom()),
                ),
            ));
        }
        if layout.cover.height > 0 && layout.lyric.height > 0 {
            out.push((
                ResizeTarget::WideCoverLyrics,
                DividerHit::new(
                    SplitAxis::Horizontal,
                    divider_line(layout.cover, SplitAxis::Horizontal),
                    (layout.left.x, layout.left.right()),
                ),
            ));
        }
    } else if layout.queue.height > 0 && layout.lyric.height > 0 {
        out.push((
            ResizeTarget::NarrowQueueLyrics,
            DividerHit::new(
                SplitAxis::Horizontal,
                divider_line(layout.queue, SplitAxis::Horizontal),
                (layout.queue.x, layout.queue.right()),
            ),
        ));
    }
    out
}

/// 窄布局的两个面板：队列在上、歌词在下，中间留 1 行 gutter 给横分割线。
///
/// 公式（`usable = area.height - GUTTER` 是两块面板能用的总行数）：
///
/// - `queue.height = clamp_extent(round(area.height * ratio), min_queue, max_queue)`
///   —— 比例仍然按**整个内容区高度**算，与拖拽时的
///   [`ratio_within`] 口径一致，指针落在哪一行分隔线就跟到哪一行；
/// - `min_queue = min(7, usable - 1)`（窗口再矮也给歌词留 1 行）、
///   `min_lyric = min(5, usable - min_queue)`、`max_queue = usable - min_lyric`；
/// - `lyric.height = usable - queue.height`，分隔线占 `queue.bottom()` 那一行。
///
/// 渲染与命中（`queue_index_at` / `table_header_rect`）都从 `compute_layout`
/// 拿这两个矩形，所以 gutter 一定不属于任何面板的内容区。
fn narrow_panes(area: Rect, ratio: f32) -> (Rect, Rect) {
    let usable = area.height.saturating_sub(GUTTER);
    let min_queue = 7.min(usable.saturating_sub(1));
    let min_lyric = 5.min(usable.saturating_sub(min_queue));
    let max_queue = usable.saturating_sub(min_lyric);
    let min_queue = min_queue.min(max_queue);
    let desired = ((area.height as f32) * ratio).round() as u16;
    split_with_gutter(
        area,
        SplitAxis::Horizontal,
        clamp_extent(desired, min_queue, max_queue),
    )
}

pub struct MainPage {
    selected: usize,
    scroll: usize,
    dragging: Option<usize>,
    cover: CoverRenderer,
    /// 队列内快速过滤；过滤只改变可见集合，不修改真实播放队列。
    queue_filter: String,
    queue_filter_active: bool,
    lyric_fullscreen: bool,
    /// 宽屏：左侧封面/歌词列占整个内容区的比例。
    wide_columns_ratio: f32,
    /// 宽屏：左侧封面占左栏高度的比例。
    wide_cover_ratio: f32,
    /// 窄屏：上方队列占整个内容区高度的比例。
    narrow_queue_ratio: f32,
    /// 分割条拖拽状态机（预览 / 提交 / 取消统一实现在 components::splitter）。
    splitter: Splitter<ResizeTarget>,
    /// 首次绘制前保持旧版封面高度策略；之后由用户拖拽接管。
    layout_initialized: bool,
    /// “D 清空整个队列”的武装时刻，确认窗口外或 Esc 后解除
    clear_armed: Option<Instant>,
    column_resize: Option<super::components::song_table::ColumnResizeState>,
    queue_columns: Vec<lx_core::model::config::TableColumnConfig>,
}

impl MainPage {
    pub fn new(cover: CoverRenderer) -> Self {
        Self {
            selected: 0,
            scroll: 0,
            dragging: None,
            cover,
            queue_filter: String::new(),
            queue_filter_active: false,
            lyric_fullscreen: false,
            wide_columns_ratio: DEFAULT_WIDE_COLUMNS_RATIO,
            wide_cover_ratio: DEFAULT_WIDE_COVER_RATIO,
            narrow_queue_ratio: DEFAULT_NARROW_QUEUE_RATIO,
            splitter: Splitter::default(),
            layout_initialized: false,
            clear_armed: None,
            column_resize: None,
            queue_columns: Vec::new(),
        }
    }

    /// 释放已解码的封面
    pub fn release_cover_image(&mut self) {
        self.cover.sync(None);
    }

    /// 收取封面后台线程返回的解码与编码结果，返回是否需要重绘
    pub fn poll_cover(&mut self) -> bool {
        self.cover.poll()
    }

    /// 终端尺寸变化后重新读取单元格的像素尺寸
    pub fn refresh_cover_font_size(&mut self) -> bool {
        self.cover.refresh_font_size()
    }

    /// 强制把封面重新传输给终端，用于终端已丢弃此前图片的场合
    pub fn force_cover_reload(&mut self) {
        self.cover.force_reload();
    }

    fn filtered_indices(&self, songs: &[lx_core::model::song::SongInfo]) -> Vec<usize> {
        let query = self.queue_filter.trim().to_lowercase();
        if query.is_empty() {
            return (0..songs.len()).collect();
        }
        songs
            .iter()
            .enumerate()
            .filter_map(|(i, song)| {
                let haystack =
                    format!("{} {} {}", song.name, song.singer, song.album_name).to_lowercase();
                haystack.contains(&query).then_some(i)
            })
            .collect()
    }

    pub fn handle_input(
        &mut self,
        key: &KeyEvent,
        ctx: &AppContext,
        resolver: &KeybindingResolver,
    ) -> AppAction {
        if self.splitter.is_dragging() && key.code == KeyCode::Esc {
            // Esc 取消本次视觉预览，不污染已提交布局。
            self.splitter.cancel();
            return AppAction::None;
        }

        let len = {
            let songs = ctx.playlist.borrow();
            let current = ctx.playlist.current_index();
            if self.selected >= songs.len() {
                self.selected = current.min(songs.len().saturating_sub(1));
            }
            songs.len()
        };

        if self.selected >= len {
            self.selected = len.saturating_sub(1);
        }

        if !self.queue_filter_active
            && key.modifiers == KeyModifiers::NONE
            && matches!(key.code, KeyCode::Char('v') | KeyCode::Char('V'))
        {
            self.lyric_fullscreen = !self.lyric_fullscreen;
            return AppAction::None;
        }

        if self.queue_filter_active {
            match key.code {
                KeyCode::Esc => {
                    self.queue_filter_active = false;
                    self.queue_filter.clear();
                    self.selected = 0;
                    self.scroll = 0;
                    return AppAction::None;
                }
                KeyCode::Enter => {
                    self.queue_filter_active = false;
                    return AppAction::None;
                }
                KeyCode::Backspace => {
                    self.queue_filter.pop();
                    self.selected = self
                        .filtered_indices(&ctx.playlist.borrow())
                        .first()
                        .copied()
                        .unwrap_or(0);
                    self.scroll = 0;
                    return AppAction::None;
                }
                KeyCode::Char(ch)
                    if key.modifiers == KeyModifiers::NONE
                        || key.modifiers == KeyModifiers::SHIFT =>
                {
                    self.queue_filter.push(ch);
                    self.selected = self
                        .filtered_indices(&ctx.playlist.borrow())
                        .first()
                        .copied()
                        .unwrap_or(0);
                    self.scroll = 0;
                    return AppAction::None;
                }
                _ => {}
            }
        } else if key.modifiers == KeyModifiers::NONE && key.code == KeyCode::Char('/') {
            self.queue_filter_active = true;
            self.scroll = 0;
            return AppAction::None;
        }

        if let Some(command) = queue_edit_command(key) {
            return match command {
                QueueEditCommand::MoveUp => {
                    if self.selected > 0 {
                        ctx.playlist.move_item(self.selected, self.selected - 1);
                        self.selected -= 1;
                    }
                    AppAction::None
                }
                QueueEditCommand::MoveDown => {
                    if self.selected + 1 < len {
                        ctx.playlist.move_item(self.selected, self.selected + 1);
                        self.selected += 1;
                    }
                    AppAction::None
                }
                QueueEditCommand::RemoveSelected => self.remove_at(self.selected, ctx),
                QueueEditCommand::Clear => {
                    // 二次确认：首次按下只武装并提示，确认窗口内再按一次才清空
                    let now = Instant::now();
                    let confirmed = matches!(
                        self.clear_armed,
                        Some(armed_at) if now.duration_since(armed_at) <= CLEAR_QUEUE_CONFIRM_WINDOW
                    );
                    if !confirmed {
                        self.clear_armed = Some(now);
                        return AppAction::ShowNotification(Notification::warning(
                            "再按一次 D 确认清空队列，Esc 取消",
                        ));
                    }
                    self.clear_armed = None;
                    self.clear_queue(ctx);
                    AppAction::None
                }
            };
        }

        if matches!(
            (key.modifiers, key.code),
            (KeyModifiers::NONE, KeyCode::Esc)
        ) {
            // Esc 取消“清空整个队列”的武装状态（其余行为不变）
            self.clear_armed = None;
        }

        if let Some(action) = resolver.resolve_page("main", key) {
            match action {
                Action::ListSelectUp => {
                    let songs = ctx.playlist.borrow();
                    let visible = self.filtered_indices(&songs);
                    if !visible.is_empty() {
                        let pos = visible
                            .iter()
                            .position(|&i| i == self.selected)
                            .unwrap_or(0);
                        self.selected = if pos == 0 {
                            if ctx
                                .config
                                .read()
                                .unwrap_or_else(|e| e.into_inner())
                                .ui
                                .wrap_navigation
                            {
                                *visible.last().unwrap()
                            } else {
                                visible[0]
                            }
                        } else {
                            visible[pos - 1]
                        };
                    }
                    return AppAction::None;
                }
                Action::ListSelectDown => {
                    let songs = ctx.playlist.borrow();
                    let visible = self.filtered_indices(&songs);
                    if !visible.is_empty() {
                        let pos = visible
                            .iter()
                            .position(|&i| i == self.selected)
                            .unwrap_or(0);
                        self.selected = if pos + 1 < visible.len() {
                            visible[pos + 1]
                        } else if ctx
                            .config
                            .read()
                            .unwrap_or_else(|e| e.into_inner())
                            .ui
                            .wrap_navigation
                        {
                            visible[0]
                        } else {
                            *visible.last().unwrap()
                        };
                    }
                    return AppAction::None;
                }
                Action::ListSelectFirst => {
                    self.selected = 0;
                    return AppAction::None;
                }
                Action::ListSelectLast => {
                    self.selected = len.saturating_sub(1);
                    return AppAction::None;
                }
                Action::ListPageUp => {
                    self.selected = self.selected.saturating_sub(ctx.page_step());
                    return AppAction::None;
                }
                Action::ListPageDown => {
                    self.selected = (self.selected + ctx.page_step()).min(len.saturating_sub(1));
                    return AppAction::None;
                }
                Action::ListActivate => {
                    if self.selected < len {
                        let (songs, _) = ctx.playlist.snapshot();
                        return AppAction::PlaySong {
                            songs,
                            index: self.selected,
                        };
                    }
                    return AppAction::None;
                }
                Action::ListToggleFavorite => {
                    let song = ctx.playlist.borrow().get(self.selected).cloned();
                    if let Some(song) = song {
                        return AppAction::ToggleFavoriteSong(Box::new(song));
                    }
                    return AppAction::None;
                }
                Action::ListDownload => {
                    if let Some(song) = ctx.playlist.borrow().get(self.selected).cloned() {
                        return AppAction::DownloadSong(Box::new(song));
                    }
                    return AppAction::None;
                }
                _ => {}
            }
        }

        match (key.modifiers, key.code) {
            // 裸 Up/Down 在主页被 main.rs 的音量快捷键拦截（见 KEYBINDINGS.md），
            // 这里不再重复处理；列表移动走 Action::ListSelectUp/Down 绑定。
            (KeyModifiers::NONE, KeyCode::Home) | (KeyModifiers::NONE, KeyCode::Char('g')) => {
                self.selected = 0;
            }
            (KeyModifiers::NONE, KeyCode::End)
            | (KeyModifiers::NONE, KeyCode::Char('G'))
            | (KeyModifiers::SHIFT, KeyCode::Char('G')) => {
                self.selected = len.saturating_sub(1);
            }
            (KeyModifiers::CONTROL, KeyCode::Char('u')) | (KeyModifiers::NONE, KeyCode::PageUp) => {
                self.selected = self.selected.saturating_sub(ctx.page_step());
            }
            (KeyModifiers::CONTROL, KeyCode::Char('d'))
            | (KeyModifiers::NONE, KeyCode::PageDown) => {
                self.selected = (self.selected + ctx.page_step()).min(len.saturating_sub(1));
            }
            _ if super::is_song_activation_key(key) && self.selected < len => {
                let (songs, _) = ctx.playlist.snapshot();
                return AppAction::PlaySong {
                    songs,
                    index: self.selected,
                };
            }
            (KeyModifiers::NONE, KeyCode::Char('f')) => {
                let song = ctx.playlist.borrow().get(self.selected).cloned();
                if let Some(song) = song {
                    return AppAction::ToggleFavoriteSong(Box::new(song));
                }
            }
            _ => {}
        }
        AppAction::None
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        if self.lyric_fullscreen {
            super::components::lyric::render(area, buf, ctx);
            return;
        }
        let layout = self.compute_layout(area, ctx);
        if layout.wide {
            if let Some(geometry) = layout.cover_geometry
                && layout.cover.height > 0
            {
                self.render_cover(layout.cover, buf, ctx, geometry);
            }
            super::components::lyric::render(layout.lyric, buf, ctx);
            self.render_queue(layout.queue, buf, ctx);
        } else {
            self.render_queue(layout.queue, buf, ctx);
            super::components::lyric::render(layout.lyric, buf, ctx);
        }
        self.render_resize_dividers(&layout, buf, ctx);
    }

    fn compute_layout(&mut self, area: Rect, ctx: &AppContext) -> MainLayout {
        if area.width >= 72 {
            let columns_ratio = self
                .splitter
                .effective(&ResizeTarget::WideColumns, self.wide_columns_ratio);
            let left_width = ((area.width as f32) * columns_ratio).round() as u16;
            let left_width = left_width.clamp(24, area.width.saturating_sub(24).max(24));
            // 左右两栏之间留 1 列 gutter：竖分割线占 left.right()，不压任何一栏的
            // 最后一列（左栏的右边框、右栏的左边框）。
            let (left, queue) = split_with_gutter(area, SplitAxis::Vertical, left_width);
            let geometry = ctx
                .config
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .ui
                .show_cover
                .then(|| {
                    CoverGeometry::from_font_size(
                        self.cover.font_size(),
                        ctx.cover_service.image_aspect(),
                    )
                });
            // 封面与歌词之间也要留 1 行 gutter，歌词的最小高度不能被它挤掉。
            let max_cover = left
                .height
                .saturating_sub(GUTTER + super::components::lyric::MIN_HEIGHT);
            if !self.layout_initialized {
                if let Some(geometry) = geometry {
                    let old_cover = geometry.box_height(left.width, max_cover);
                    if left.height > 0 {
                        self.wide_cover_ratio =
                            clamp_ratio(old_cover as f32 / left.height as f32, 0.20, 0.85);
                    }
                }
                self.layout_initialized = true;
            }
            let cover_ratio = self
                .splitter
                .effective(&ResizeTarget::WideCoverLyrics, self.wide_cover_ratio);
            let cover_height = if geometry.is_some() {
                ((left.height as f32) * cover_ratio)
                    .round()
                    .clamp(0.0, f32::from(max_cover)) as u16
            } else {
                0
            };
            // 没有封面时不要白白吃掉一行 gutter：整栏都留给歌词。
            let (cover, lyric) = if cover_height == 0 {
                (
                    Rect::new(left.x, left.y, left.width, 0),
                    Rect::new(left.x, left.y, left.width, left.height),
                )
            } else {
                split_with_gutter(left, SplitAxis::Horizontal, cover_height)
            };
            MainLayout {
                wide: true,
                left,
                queue,
                cover,
                lyric,
                cover_geometry: geometry,
            }
        } else {
            let queue_ratio = self
                .splitter
                .effective(&ResizeTarget::NarrowQueueLyrics, self.narrow_queue_ratio);
            let (queue, lyric) = narrow_panes(area, queue_ratio);
            MainLayout {
                wide: false,
                left: Rect::default(),
                queue,
                cover: Rect::default(),
                lyric,
                cover_geometry: None,
            }
        }
    }

    fn render_resize_dividers(&self, layout: &MainLayout, buf: &mut Buffer, ctx: &AppContext) {
        let base = Style::new().fg(crate::theme::accent(ctx));
        let style = if self.splitter.is_dragging() {
            base.bold()
        } else {
            base
        };
        // 绘制坐标与命中共用 `dividers()`，不会再出现"看得到却抓不住"。
        for (_, hit) in dividers(layout) {
            match hit.axis {
                SplitAxis::Vertical => {
                    for y in hit.span.0..hit.span.1 {
                        buf.set_string(hit.divider, y, "│", style);
                    }
                }
                SplitAxis::Horizontal => {
                    for x in hit.span.0..hit.span.1 {
                        buf.set_string(x, hit.divider, "─", style);
                    }
                }
            }
        }
    }

    /// 清空队列并做完整收尾（键位 `Shift+D` 与底栏菜单共用）。
    ///
    /// 只有显式清空才丢弃已保存的播放会话；否则「队列为空」不再触发删除，
    /// 下次启动会把这次清掉的队列又恢复回来。
    pub fn clear_queue(&mut self, ctx: &AppContext) {
        ctx.playlist.clear();
        if let Err(error) = ctx.forget_playback_session() {
            tracing::warn!("清空已保存的播放会话失败: {error}");
        }
        ctx.stop_player();
        ctx.cover_service.clear();
        ctx.lyric_service.clear();
        *ctx.current_song.write().unwrap_or_else(|e| e.into_inner()) = None;
        self.selected = 0;
        self.scroll = 0;
    }

    /// 把选中项定位到队列里的某一首（底栏「队列」段左键用）。
    ///
    /// `len` 由调用方从播放列表取（页面本身不持有队列，队列在 `ctx.playlist`）。
    pub fn select_current(&mut self, index: usize, len: usize) {
        self.selected = index.min(len.saturating_sub(1));
    }

    /// 歌曲表头所在的一行（供 main.rs 判定"表头右键 → 列菜单"）。
    pub fn table_header_rect(&mut self, area: Rect, ctx: &AppContext) -> Option<Rect> {
        let layout = self.compute_layout(area, ctx);
        let inner = panel_inner(layout.queue);
        Some(Rect::new(inner.x, inner.y, inner.width, 1))
    }

    /// 自动列宽的测量样本：当前队列，**无副作用**。
    pub fn autofit_samples(&self, ctx: &AppContext) -> Vec<lx_core::model::song::SongInfo> {
        ctx.playlist.snapshot().0
    }

    /// 兜底取消所有进行中的拖拽会话（分割条 + 列宽）。
    ///
    /// 鼠标只在 `ui_areas.content` 内派发，所以在列表外松开左键时页面收不到
    /// `Up`；切页与终端 resize 也会留下残留状态。残留会让整个页面的鼠标
    /// 事件被拖拽分支吞掉，表现为"这一页突然点不动了"。
    pub fn abort_drag_sessions(&mut self) {
        self.splitter.cancel();
        self.column_resize = None;
    }

    pub fn handle_mouse(
        &mut self,
        event: MouseEvent,
        area: Rect,
        ctx: &AppContext,
        activate: bool,
    ) -> AppAction {
        let layout = self.compute_layout(area, ctx);
        if let Some(target) = self.splitter.dragging().copied() {
            match event.kind {
                // 终端在鼠标按住拖动时并不总是发 `Drag`：有的实现会用 `Moved` 连续
                // 更新，若这里只处理 `Drag`，分割线会停留在旧位置，给人“卡住”的错觉。
                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved => {
                    self.update_resize_preview(target, event, area, &layout);
                    return AppAction::None;
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    return match self.commit_resize() {
                        Some((ratio_key, ratio)) => AppAction::CommitPaneRatio {
                            page_key: MP_PAGE_KEY.to_string(),
                            ratio_key: ratio_key.to_string(),
                            ratio,
                        },
                        None => AppAction::None,
                    };
                }
                // Resize 会话期间其它鼠标事件不能穿透到队列，避免拖动时误触。
                _ => return AppAction::None,
            }
        } else if matches!(event.kind, MouseEventKind::Down(MouseButton::Left))
            && let Some(target) = self.resize_target_at(event, &layout)
        {
            let committed = self.committed_ratio(target);
            self.splitter.begin(target, committed);
            return AppAction::None;
        }

        let queue_inner = panel_inner(layout.queue);
        let header_row = queue_inner.y;
        let table_width = queue_inner.width;
        let queue_header = Rect::new(queue_inner.x, header_row, table_width, 1);
        match super::components::song_table::handle_column_resize(
            &mut self.column_resize,
            &mut self.queue_columns,
            event,
            Some(queue_header),
            queue_inner,
        ) {
            super::components::song_table::ColumnResizeOutcome::Updated => {
                return AppAction::None;
            }
            super::components::song_table::ColumnResizeOutcome::Finished => {
                return AppAction::CommitColumnResize {
                    page_key: "queue".to_string(),
                    columns: self.queue_columns.clone(),
                };
            }
            super::components::song_table::ColumnResizeOutcome::NotHandled => {}
        }

        let scroll_amount = ctx
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .ui
            .scroll_amount
            .max(1);
        let mut play_songs = None;
        let mut drag_target = None;
        let pointer = Position::new(event.column, event.row);
        // 先记录拖拽起点：Up 事件会清掉 self.dragging，落点应用阶段仍需用到它。
        let drag_source = self.dragging;
        {
            // 只读阶段：从队列快照中取出本次事件需要的少量信息。
            let songs = ctx.playlist.borrow();
            let current = ctx.playlist.current_index();
            match event.kind {
                // 滚轮只在光标位于队列面板内时才改选中：以前光标停在封面/歌词上
                // 滚轮，队列选中也会跟着跑。
                MouseEventKind::ScrollUp if layout.queue.contains(pointer) => {
                    self.dragging = None;
                    self.selected = self.selected.saturating_sub(scroll_amount);
                }
                MouseEventKind::ScrollDown if layout.queue.contains(pointer) => {
                    self.dragging = None;
                    self.selected =
                        (self.selected + scroll_amount).min(songs.len().saturating_sub(1));
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    let index = if self.queue_filter.is_empty() {
                        queue_index_at(event, layout.queue, self.scroll, songs.len())
                    } else {
                        queue_index_at_filtered(
                            event,
                            layout.queue,
                            self.scroll,
                            &self.filtered_indices(&songs),
                        )
                    };
                    if let Some(index) = index {
                        self.selected = index;
                        self.dragging = Some(index);
                        if activate {
                            play_songs = Some(songs.to_vec());
                        }
                    } else {
                        self.dragging = None;
                        self.selected = current.min(songs.len().saturating_sub(1));
                    }
                }
                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved => {
                    if let Some(from) = self.dragging {
                        drag_target = if self.queue_filter.is_empty() {
                            queue_index_at(event, layout.queue, self.scroll, songs.len())
                        } else {
                            queue_index_at_filtered(
                                event,
                                layout.queue,
                                self.scroll,
                                &self.filtered_indices(&songs),
                            )
                        }
                        .filter(|&target| target != from);
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    self.dragging = None;
                }
                _ => {}
            }
        }
        // 借用已释放，写操作不会与读锁互相等待。
        if let (Some(from), Some(target)) = (drag_source, drag_target) {
            ctx.playlist.move_item(from, target);
            self.selected = target;
            self.dragging = Some(target);
        }
        if let Some(songs) = play_songs {
            return AppAction::PlaySong {
                songs,
                index: self.selected,
            };
        }
        AppAction::None
    }

    pub fn context_song_at(
        &mut self,
        source: MenuHitSource,
        area: Rect,
        ctx: &AppContext,
    ) -> Option<(Vec<lx_core::model::song::SongInfo>, usize)> {
        let songs = ctx.playlist.borrow();
        let layout = self.compute_layout(area, ctx);
        // 必须和左键走同一套"过滤 / 未过滤"分流：过滤生效时 `self.scroll` 是
        // 过滤视图的偏移，若这里仍按未过滤列表换算，右键菜单会指向另一首歌。
        // 鼠标与键盘入口共用这一份解析。
        let index = source.resolve_index(
            |event| {
                if self.queue_filter.is_empty() {
                    queue_index_at(event, layout.queue, self.scroll, songs.len())
                } else {
                    queue_index_at_filtered(
                        event,
                        layout.queue,
                        self.scroll,
                        &self.filtered_indices(&songs),
                    )
                }
            },
            self.selected,
            songs.len(),
        )?;
        self.selected = index;
        self.dragging = None;
        Some((songs.to_vec(), index))
    }

    pub fn remove_at(&mut self, index: usize, ctx: &AppContext) -> AppAction {
        let songs = ctx.playlist.borrow();
        let current = ctx.playlist.current_index();
        if index >= songs.len() {
            return AppAction::None;
        }
        let removing_current = index == current;
        drop(songs);
        ctx.playlist.remove(index);
        let remaining = ctx.playlist.borrow();
        let next = ctx.playlist.current_index();
        self.selected = index.min(remaining.len().saturating_sub(1));
        self.scroll = self.scroll.min(remaining.len().saturating_sub(1));
        if !removing_current {
            return AppAction::None;
        }
        if remaining.is_empty() {
            ctx.stop_player();
            ctx.cover_service.clear();
            ctx.lyric_service.clear();
            *ctx.current_song.write().unwrap_or_else(|e| e.into_inner()) = None;
            AppAction::None
        } else {
            AppAction::PlaySong {
                songs: remaining.to_vec(),
                index: next,
            }
        }
    }

    fn resize_target_at(&self, event: MouseEvent, layout: &MainLayout) -> Option<ResizeTarget> {
        dividers(layout)
            .into_iter()
            .find(|(_, hit)| hit.matches(event.column, event.row))
            .map(|(target, _)| target)
    }

    fn committed_ratio(&self, target: ResizeTarget) -> f32 {
        match target {
            ResizeTarget::WideColumns => self.wide_columns_ratio,
            ResizeTarget::WideCoverLyrics => self.wide_cover_ratio,
            ResizeTarget::NarrowQueueLyrics => self.narrow_queue_ratio,
        }
    }

    fn clamp_resize_ratio(target: ResizeTarget, ratio: f32) -> f32 {
        match target {
            ResizeTarget::WideColumns => clamp_ratio(ratio, 0.22, 0.78),
            ResizeTarget::WideCoverLyrics => clamp_ratio(ratio, 0.15, 0.85),
            ResizeTarget::NarrowQueueLyrics => clamp_ratio(ratio, 0.20, 0.80),
        }
    }

    fn update_resize_preview(
        &mut self,
        target: ResizeTarget,
        event: MouseEvent,
        area: Rect,
        layout: &MainLayout,
    ) {
        let raw_ratio = match target {
            ResizeTarget::WideColumns if area.width > 0 => {
                ratio_within(area.x, area.width, event.column)
            }
            ResizeTarget::WideCoverLyrics if layout.left.height > 0 => {
                ratio_within(layout.left.y, layout.left.height, event.row)
            }
            ResizeTarget::NarrowQueueLyrics if area.height > 0 => {
                ratio_within(area.y, area.height, event.row)
            }
            _ => return,
        };
        self.splitter
            .drag(Self::clamp_resize_ratio(target, raw_ratio));
    }

    /// 鼠标抬起：提交比例并返回需要持久化的 `(ratio_key, ratio)`。
    fn commit_resize(&mut self) -> Option<(&'static str, f32)> {
        let (target, ratio) = self.splitter.commit()?;
        let key = match target {
            ResizeTarget::WideColumns => {
                self.wide_columns_ratio = ratio;
                "wide_columns"
            }
            ResizeTarget::WideCoverLyrics => {
                self.wide_cover_ratio = ratio;
                "wide_cover"
            }
            ResizeTarget::NarrowQueueLyrics => {
                self.narrow_queue_ratio = ratio;
                "narrow_queue"
            }
        };
        Some((key, ratio))
    }

    /// 从 Config 恢复用户拖拽过的比例（页面构造后调用一次）。
    pub fn apply_pane_ratios(&mut self, ratios: &std::collections::HashMap<String, f32>) {
        if let Some(value) = ratios.get("wide_columns").copied() {
            self.wide_columns_ratio = Self::clamp_resize_ratio(ResizeTarget::WideColumns, value);
        }
        if let Some(value) = ratios.get("wide_cover").copied() {
            self.wide_cover_ratio = Self::clamp_resize_ratio(ResizeTarget::WideCoverLyrics, value);
        }
        if let Some(value) = ratios.get("narrow_queue").copied() {
            self.narrow_queue_ratio =
                Self::clamp_resize_ratio(ResizeTarget::NarrowQueueLyrics, value);
        }
    }

    pub fn pane_page_key(&self) -> &'static str {
        MP_PAGE_KEY
    }

    /// 本次实际生效的封面协议（启动时注入设置页，供 `Shift+P` 循环判断）。
    pub fn cover_protocol(&self) -> ratatui_image::picker::ProtocolType {
        self.cover.protocol_type()
    }

    /// 把三处分栏比例恢复成内置默认值（封面/歌词分栏下次布局重新自适应）。
    ///
    /// 与 `apply_pane_ratios` 对称：调用方同时要删掉持久化的 `pane_ratios`
    /// 条目，否则下次启动又会被旧值覆盖回来。
    pub fn reset_pane_ratios(&mut self) {
        self.wide_columns_ratio = DEFAULT_WIDE_COLUMNS_RATIO;
        self.wide_cover_ratio = DEFAULT_WIDE_COVER_RATIO;
        self.narrow_queue_ratio = DEFAULT_NARROW_QUEUE_RATIO;
        // 让封面高度重新按图片宽高比自适应一次
        self.layout_initialized = false;
        self.splitter.cancel();
    }

    fn render_queue(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        let accent = crate::theme::accent(ctx);
        // 借用队列快照，每帧渲染不再复制整张播放列表。
        let songs = ctx.playlist.borrow();
        let current = ctx.playlist.current_index();
        let block = Block::default()
            .borders(PANEL_BORDERS)
            .border_style(Style::new().fg(crate::theme::border(ctx)))
            .title(
                if self.queue_filter_active || !self.queue_filter.is_empty() {
                    format!(" 队列 · {} 歌曲 · /{} ", songs.len(), self.queue_filter)
                } else {
                    format!(" 队列 · {} 歌曲 · /筛选 ", songs.len())
                },
            );
        let inner = block.inner(area);
        // 拖拽期间以页面状态为准，否则拖拽结果会被每帧重载覆盖。
        if self.column_resize.is_none() {
            let cfg = ctx.config.read().unwrap_or_else(|e| e.into_inner());
            self.queue_columns = super::components::song_table::load_columns_for_page(
                &cfg.ui.table_columns,
                "queue",
                inner.width,
            );
        }
        block.render(area, buf);
        if self.queue_filter_active {
            // 筛选串画在边框标题里，插入点跟着标题文本走（去掉左右边框各一列）。
            // 这是输入法候选框的定位依据，见 ui_cursor 的说明。
            crate::ui_cursor::request_after(
                Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), 1),
                &format!(" 队列 · {} 歌曲 · /", songs.len()),
                &self.queue_filter,
            );
        }
        if songs.is_empty() {
            let hint = if self.queue_filter_active || !self.queue_filter.is_empty() {
                " 队列为空\n 筛选中没有匹配的歌曲，按 Esc 清除筛选词"
            } else {
                " 队列为空\n 在 搜索(2) · 排行榜(3) · 歌单(4) 页按 Enter 播放，歌曲会自动加入队列"
            };
            Paragraph::new(hint)
                .style(Style::new().fg(crate::theme::muted(ctx)))
                .render(inner, buf);
            return;
        }
        self.selected = self.selected.min(songs.len().saturating_sub(1));
        if inner.height == 0 {
            return;
        }

        super::components::song_table::header_paragraph(
            inner.width,
            &self.queue_columns,
            super::components::song_table::TablePalette::from_theme(ctx),
        )
        .render(Rect::new(inner.x, inner.y, inner.width, 1), buf);
        let list = Rect::new(
            inner.x,
            inner.y.saturating_add(1),
            inner.width,
            inner.height.saturating_sub(1),
        );
        let visible = list.height as usize;
        if visible == 0 {
            return;
        }
        if self.selected >= self.scroll + visible {
            self.scroll = self.selected.saturating_sub(visible - 1);
        } else if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        let filtered = self.filtered_indices(&songs);
        self.scroll = self.scroll.min(filtered.len().saturating_sub(visible));

        for (row, index) in filtered
            .iter()
            .copied()
            .skip(self.scroll)
            .take(visible)
            .enumerate()
        {
            let mut style = if index == current {
                Style::new().fg(accent).add_modifier(Modifier::BOLD)
            } else {
                Style::new()
            };
            if index == self.selected {
                style = Style::new()
                    .fg(crate::theme::selection_fg(ctx))
                    .bg(accent)
                    .add_modifier(Modifier::BOLD);
            }
            super::components::song_table::row_paragraph(
                &songs[index],
                index,
                list.width,
                &self.queue_columns,
                super::components::song_table::TablePalette::from_theme(ctx),
            )
            .style(style)
            .render(Rect::new(list.x, list.y + row as u16, list.width, 1), buf);
        }
    }
}

fn queue_index_at(event: MouseEvent, area: Rect, scroll: usize, len: usize) -> Option<usize> {
    crate::pages::components::hit_test::row_at(
        area,
        Position::new(event.column, event.row),
        scroll,
        len,
        1,
    )
}

fn queue_index_at_filtered(
    event: MouseEvent,
    area: Rect,
    scroll: usize,
    indices: &[usize],
) -> Option<usize> {
    let pos = crate::pages::components::hit_test::row_at(
        area,
        Position::new(event.column, event.row),
        scroll,
        indices.len(),
        1,
    )?;
    indices.get(pos).copied()
}

impl MainPage {
    /// 绘制封面框，无法绘制封面时退回文字占位
    fn render_cover(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        ctx: &AppContext,
        geometry: CoverGeometry,
    ) {
        let block = Block::default()
            .borders(PANEL_BORDERS)
            .border_style(Style::new().fg(crate::theme::border(ctx)))
            .title(" 封面 ");
        let inner = block.inner(area);
        block.render(area, buf);

        self.cover.sync(ctx.cover_service.image_path().as_deref());
        if self.cover.render(geometry.image_rect(inner), buf) {
            return;
        }
        render_cover_text(inner, buf, ctx);
    }
}

/// 封面框在「画不出图」时的文字状态。
///
/// 这些文案是排查封面问题的唯一线索：以前 `Empty` / `Ready`（协议不支持）
/// 会渲染成空行，用户只看到一个空框，既不知道是没拿到地址、还是拿到了但
/// 终端画不出来。现在每种状态都给出可区分的文字。
fn cover_status_hint(state: &CoverState) -> &'static str {
    match state {
        CoverState::Loading => "封面加载中...",
        CoverState::Unavailable(_) => "封面不可用",
        // current_song 非 None 但无封面地址（音源没返回，或被判定为残缺地址）
        CoverState::Empty => "音源未返回封面地址",
        // 地址已就绪，但当前终端协议画不出来（例如被识别成 halfblocks）
        CoverState::Ready => "封面无法显示（终端协议不支持）",
    }
}

fn render_cover_text(inner: Rect, buf: &mut Buffer, ctx: &AppContext) {
    let cover_state = ctx.cover_service.state();
    let hint = cover_status_hint(&cover_state);
    let song = ctx.current_song.read().unwrap_or_else(|e| e.into_inner());
    let lines = song.as_ref().map_or_else(
        || {
            vec![
                Line::from(""),
                Line::from(Span::styled(
                    "等待播放",
                    Style::new().fg(crate::theme::muted(ctx)),
                )),
            ]
        },
        |song| {
            vec![
                Line::from(""),
                Line::from(Span::styled(
                    &song.name,
                    Style::new()
                        .fg(crate::theme::text(ctx))
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    &song.singer,
                    Style::new().fg(crate::theme::muted(ctx)),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    hint,
                    Style::new().fg(crate::theme::muted(ctx)),
                )),
                match &cover_state {
                    CoverState::Unavailable(error) => Line::from(Span::styled(
                        // 按**显示宽度**截断（错误信息是中文，chars() 会超宽撑出面板）
                        crate::pages::components::text::truncate_width(error, inner.width as usize)
                            .into_owned(),
                        Style::new().fg(crate::theme::overlay0(ctx)),
                    )),
                    CoverState::Empty => Line::from(Span::styled(
                        "可在「设置 · 界面」里关闭封面显示以给歌词让位",
                        Style::new().fg(crate::theme::overlay0(ctx)),
                    )),
                    _ => Line::from(""),
                },
            ]
        },
    );
    Paragraph::new(lines)
        .alignment(ratatui::layout::Alignment::Center)
        .render(inner, buf);
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{
        MainLayout, QueueEditCommand, ResizeTarget, dividers, narrow_panes, queue_edit_command,
        queue_index_at,
    };
    use crate::pages::components::hit_test::panel_inner;
    use crate::pages::components::splitter::{GUTTER, SplitAxis, divider_line, split_with_gutter};

    #[test]
    fn queue_click_rows_align_with_rendered_rows() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        use ratatui::layout::Rect;

        let area = Rect::new(0, 0, 100, 20);
        let click = |row: u16, scroll: usize, len: usize| {
            queue_index_at(
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 50,
                    row,
                    modifiers: KeyModifiers::NONE,
                },
                area,
                scroll,
                len,
            )
        };

        assert_eq!(click(0, 0, 100), None);
        assert_eq!(click(1, 0, 100), None);
        assert_eq!(click(2, 0, 100), Some(0));
        assert_eq!(click(11, 0, 100), Some(9));
        assert_eq!(click(2, 7, 100), Some(7));
        // 下边框那一行不属于列表
        assert_eq!(click(19, 0, 100), None);
    }

    #[test]
    fn queue_reorder_shortcuts_accept_terminal_shift_variants() {
        for key in [
            KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Char('K'), KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Char('K'), KeyModifiers::NONE),
        ] {
            assert_eq!(queue_edit_command(&key), Some(QueueEditCommand::MoveUp));
        }

        for key in [
            KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Char('J'), KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Char('J'), KeyModifiers::NONE),
        ] {
            assert_eq!(queue_edit_command(&key), Some(QueueEditCommand::MoveDown));
        }
    }

    #[test]
    fn queue_delete_shortcuts_distinguish_one_song_from_the_whole_queue() {
        for key in [
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE),
        ] {
            assert_eq!(
                queue_edit_command(&key),
                Some(QueueEditCommand::RemoveSelected)
            );
        }

        for key in [
            KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Char('D'), KeyModifiers::NONE),
        ] {
            assert_eq!(queue_edit_command(&key), Some(QueueEditCommand::Clear));
        }
    }

    /// 回归（窄布局）：横分割线只占自己那一行 gutter，队列面板**最后一行内容**
    /// 既没有被它覆盖，也仍然能被鼠标点中（滚到底时最后一首歌点不到）。
    #[test]
    fn narrow_gutter_owns_a_row_and_the_last_queue_row_stays_clickable() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        use ratatui::layout::{Position, Rect};

        let area = Rect::new(0, 0, 100, 20);
        let (queue, lyric) = narrow_panes(area, 0.62);
        let gutter = divider_line(queue, SplitAxis::Horizontal);

        // 公式：两块面板 + 1 行 gutter 恰好铺满内容区
        assert_eq!(gutter, queue.bottom());
        assert_eq!(lyric.y, gutter + GUTTER);
        assert_eq!(queue.height + GUTTER + lyric.height, area.height);

        let inner = panel_inner(queue);
        let last_content_row = inner.bottom() - 1;
        // 分隔线所在的那一行不属于任何面板的内容区
        assert!(inner.bottom() < gutter, "队列内容区必须在 gutter 之上");
        assert!(
            panel_inner(lyric).y > gutter,
            "歌词内容区必须在 gutter 之下"
        );

        let visible = inner.height as usize - 1; // 列表区去掉表头
        let click = |row: u16, len: usize| {
            queue_index_at(
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 50,
                    row,
                    modifiers: KeyModifiers::NONE,
                },
                queue,
                0,
                len,
            )
        };
        // 滚到底：最后一首歌（下标 len-1）就在面板最后一行内容上，必须点得中
        assert_eq!(click(last_content_row, visible), Some(visible - 1));
        // gutter 自己那一行、以及面板的下边框行，都不属于列表
        assert_eq!(click(gutter, visible), None);
        assert_eq!(click(inner.bottom(), visible), None);

        // 拖拽命中落在 gutter 上，且容差不会吃掉最后一行内容
        let hit = crate::pages::components::splitter::DividerHit::new(
            SplitAxis::Horizontal,
            gutter,
            (queue.x, queue.right()),
        );
        assert!(hit.matches(50, gutter));
        assert!(!hit.matches(50, last_content_row));
        assert_eq!(
            crate::pages::components::hit_test::row_at(
                queue,
                Position::new(50, last_content_row),
                0,
                visible,
                1
            ),
            Some(visible - 1)
        );
    }

    /// 宽布局：竖分割线占"左栏右边的 1 列 gutter"，横分割线占封面与歌词之间的
    /// 1 行 gutter；两条线互不占位，且都不压面板最后一行/最后一列内容。
    #[test]
    fn wide_dividers_live_in_their_own_gutter_row_and_column() {
        use ratatui::layout::Rect;

        let area = Rect::new(0, 0, 120, 40);
        let (left, queue) = split_with_gutter(area, SplitAxis::Vertical, 44);
        let (cover, lyric) = split_with_gutter(left, SplitAxis::Horizontal, 20);
        let layout = MainLayout {
            wide: true,
            left,
            queue,
            cover,
            lyric,
            cover_geometry: None,
        };

        let found = dividers(&layout);
        let vertical = found
            .iter()
            .find(|(target, _)| *target == ResizeTarget::WideColumns)
            .expect("宽布局必须有左栏|队列竖线")
            .1;
        let horizontal = found
            .iter()
            .find(|(target, _)| *target == ResizeTarget::WideCoverLyrics)
            .expect("宽布局必须有封面|歌词横线")
            .1;

        // 竖线 = 左栏 right()，横线 = 封面 bottom()，两条线各自占一格
        assert_eq!(vertical.divider, left.right());
        assert_eq!(horizontal.divider, cover.bottom());
        assert_eq!(queue.x, vertical.divider + GUTTER);
        assert_eq!(lyric.y, horizontal.divider + GUTTER);

        // 容差范围不碰两侧面板的最后/最前一列内容
        assert!(vertical.matches(vertical.divider, left.y));
        assert!(!vertical.matches(panel_inner(left).right() - 1, left.y));
        assert!(!vertical.matches(panel_inner(queue).x, left.y));
        // 容差范围不碰封面最后一行内容、也不碰歌词第一行内容
        assert!(horizontal.matches(left.x, horizontal.divider));
        assert!(!horizontal.matches(left.x, panel_inner(cover).bottom() - 1));
        assert!(!horizontal.matches(left.x, panel_inner(lyric).y));
        // 横线的跨度止于竖线的 gutter 列，两条分割线不会互相擦掉
        assert_eq!(horizontal.span.1, vertical.divider);
        assert!(!horizontal.matches(vertical.divider, horizontal.divider));
    }
}
