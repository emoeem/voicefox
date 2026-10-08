//! 排行榜页面

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lx_core::events::{AppAction, InsertPosition};
use lx_core::keybinding::{Action, KeybindingResolver};
use lx_core::model::leaderboard::LeaderboardInfo;
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};

use crate::context::AppContext;
use crate::pages::components::context_menu::MenuHitSource;
use crate::pages::components::hit_test::{PANEL_BORDERS, panel_inner};
use crate::pages::components::source_selector::{SourceSelector, SourceSelectorKey};
use crate::pages::components::splitter::{
    DividerHit, GUTTER, SplitAxis, Splitter, clamp_extent, clamp_ratio, divider_line, ratio_within,
    split_with_gutter,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaderboardLoadRequest {
    Boards { source: SourceId },
    Songs { source: SourceId, board_id: String },
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum LBResizeTarget {
    WideBoards,
    NarrowBoards,
}

const LB_DEFAULT_BOARDS_RATIO_WIDE: f32 = 0.30;
const LB_DEFAULT_BOARDS_RATIO_NARROW: f32 = 0.18;
/// 宽/窄布局分界：与 playlists 保持一致（settings/main_page 另有各自的阈值）。
const LB_WIDE_MIN_WIDTH: u16 = 82;
/// 页面在 `ui.pane_ratios` 里的 key。
const LB_PAGE_KEY: &str = "leaderboard";

#[derive(Debug, Clone)]
pub struct LeaderboardPage {
    sources: Vec<SourceId>,
    source_index: usize,
    source_selector: Option<SourceSelector>,
    pub boards: Vec<LeaderboardInfo>,
    pub songs: Vec<SongInfo>,
    pub selected: usize,
    pub selected_board: Option<usize>,
    boards_loaded: bool,
    boards_loading: bool,
    songs_loaded: bool,
    songs_loading: bool,
    board_scroll_offset: usize,
    song_scroll_offset: usize,
    error_message: Option<String>,
    board_cache: HashMap<SourceId, Vec<LeaderboardInfo>>,
    song_cache: HashMap<(SourceId, String), Vec<SongInfo>>,
    boards_ratio_wide: f32,
    boards_ratio_narrow: f32,
    splitter: Splitter<LBResizeTarget>,
    column_resize: Option<super::components::song_table::ColumnResizeState>,
    song_columns: Vec<lx_core::model::config::TableColumnConfig>,
}

impl LeaderboardPage {
    pub fn new(sources: Vec<SourceId>) -> Self {
        Self {
            sources: sources.clone(),
            source_index: 0,
            source_selector: Some(SourceSelector::from_sources(&sources, false)),
            boards: Vec::new(),
            songs: Vec::new(),
            selected: 0,
            selected_board: None,
            boards_loaded: false,
            boards_loading: false,
            songs_loaded: false,
            songs_loading: false,
            board_scroll_offset: 0,
            song_scroll_offset: 0,
            error_message: None,
            board_cache: HashMap::new(),
            song_cache: HashMap::new(),
            boards_ratio_wide: LB_DEFAULT_BOARDS_RATIO_WIDE,
            boards_ratio_narrow: LB_DEFAULT_BOARDS_RATIO_NARROW,
            splitter: Splitter::default(),
            column_resize: None,
            song_columns: Vec::new(),
        }
    }

    pub fn current_source(&self) -> Option<SourceId> {
        self.sources.get(self.source_index).copied()
    }

    pub fn current_board(&self) -> Option<&LeaderboardInfo> {
        self.selected_board.and_then(|index| self.boards.get(index))
    }

    pub fn next_load_request(&self) -> Option<LeaderboardLoadRequest> {
        let source = self.current_source()?;
        if let Some(board) = self.current_board() {
            if !self.songs_loading && !self.songs_loaded {
                return Some(LeaderboardLoadRequest::Songs {
                    source,
                    board_id: board.id.clone(),
                });
            }
        } else if !self.boards_loading && !self.boards_loaded {
            return Some(LeaderboardLoadRequest::Boards { source });
        }
        None
    }

    pub fn begin_loading(&mut self, request: &LeaderboardLoadRequest) {
        self.error_message = None;
        match request {
            LeaderboardLoadRequest::Boards { .. } => {
                self.boards_loading = true;
                self.boards_loaded = false;
            }
            LeaderboardLoadRequest::Songs { .. } => {
                self.songs_loading = true;
                self.songs_loaded = false;
            }
        }
    }

    pub fn update_boards(&mut self, source: SourceId, boards: Vec<LeaderboardInfo>) {
        self.board_cache.insert(source, boards.clone());
        if self.current_source() != Some(source) || self.selected_board.is_some() {
            return;
        }
        self.boards = boards;
        self.boards_loading = false;
        self.boards_loaded = true;
        self.error_message = None;
        self.selected = self.selected.min(self.boards.len().saturating_sub(1));
        self.board_scroll_offset = 0;
    }

    pub fn update_songs(&mut self, source: SourceId, board_id: &str, songs: Vec<SongInfo>) {
        self.song_cache
            .insert((source, board_id.to_string()), songs.clone());
        if self.current_source() != Some(source)
            || self.current_board().map(|board| board.id.as_str()) != Some(board_id)
        {
            return;
        }
        self.songs = songs;
        self.songs_loading = false;
        self.songs_loaded = true;
        self.error_message = None;
        self.selected = 0;
        self.song_scroll_offset = 0;
    }

    pub fn update_error(&mut self, request: &LeaderboardLoadRequest, message: String) {
        match request {
            LeaderboardLoadRequest::Boards { source }
                if self.current_source() == Some(*source) && self.selected_board.is_none() =>
            {
                self.boards.clear();
                self.boards_loading = false;
                self.boards_loaded = true;
                self.error_message = Some(message);
                // 只有真正应用了错误的请求才能重置光标；
                // 否则已切走的过期响应会把用户正在浏览的列表拉回顶部。
                self.selected = 0;
            }
            LeaderboardLoadRequest::Songs { source, board_id }
                if self.current_source() == Some(*source)
                    && self.current_board().map(|board| board.id.as_str())
                        == Some(board_id.as_str()) =>
            {
                self.songs.clear();
                self.songs_loading = false;
                self.songs_loaded = true;
                self.error_message = Some(message);
                self.selected = 0;
            }
            _ => {}
        }
    }

    pub fn handle_input(
        &mut self,
        key: &KeyEvent,
        ctx: &AppContext,
        resolver: &KeybindingResolver,
    ) -> AppAction {
        if self.splitter.is_dragging() && key.code == KeyCode::Esc {
            // Esc 取消本次分栏拖拽预览，不污染已提交布局
            // （与 main_page 的处理保持一致）。
            self.cancel_resize();
            return AppAction::None;
        }
        if self
            .source_selector
            .as_ref()
            .is_some_and(|selector| selector.is_open())
        {
            self.handle_source_selector(key);
            return AppAction::None;
        }
        if let Some(action) = resolver.resolve_page("leaderboard", key) {
            match action {
                Action::ListSelectUp => {
                    self.move_selection_up(ctx);
                    return AppAction::None;
                }
                Action::ListSelectDown => {
                    self.move_selection_down(ctx);
                    return AppAction::None;
                }
                Action::ListSelectFirst => {
                    self.selected = 0;
                    return AppAction::None;
                }
                Action::ListSelectLast => {
                    self.selected = self.current_list_len().saturating_sub(1);
                    return AppAction::None;
                }
                Action::ListPageUp => {
                    self.selected = self.selected.saturating_sub(10);
                    return AppAction::None;
                }
                Action::ListPageDown => {
                    self.selected =
                        (self.selected + 10).min(self.current_list_len().saturating_sub(1));
                    return AppAction::None;
                }
                Action::ListActivate => {
                    if self.selected_board.is_some() && !self.songs.is_empty() {
                        return AppAction::PlaySong {
                            songs: self.songs.clone(),
                            index: self.selected,
                        };
                    }
                    self.enter_selected_board();
                    return AppAction::None;
                }
                Action::ListDownload => {
                    if self.selected_board.is_some()
                        && let Some(song) = self.songs.get(self.selected).cloned()
                    {
                        return AppAction::DownloadSong(Box::new(song));
                    }
                    return AppAction::None;
                }
                Action::ListAddToQueue => {
                    if self.selected_board.is_some()
                        && let Some(song) = self.songs.get(self.selected).cloned()
                    {
                        return AppAction::AddToQueue {
                            song: Box::new(song),
                            position: InsertPosition::End,
                        };
                    }
                    return AppAction::None;
                }
                Action::ListAddToQueueNext => {
                    if self.selected_board.is_some()
                        && let Some(song) = self.songs.get(self.selected).cloned()
                    {
                        return AppAction::AddToQueue {
                            song: Box::new(song),
                            position: InsertPosition::Next,
                        };
                    }
                    return AppAction::None;
                }
                Action::ListToggleFavorite => {
                    if self.selected_board.is_some()
                        && let Some(song) = self.songs.get(self.selected).cloned()
                    {
                        return AppAction::ToggleFavoriteSong(Box::new(song));
                    }
                    return AppAction::None;
                }
                Action::ListGoBack => {
                    if self.selected_board.is_some() {
                        self.leave_board();
                    }
                    return AppAction::None;
                }
                Action::SearchCycleSourcePrev => {
                    self.select_previous_source();
                    return AppAction::None;
                }
                Action::SearchCycleSourceNext => {
                    self.select_next_source();
                    return AppAction::None;
                }
                _ => {}
            }
        }

        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Char('p' | 'P')) => {
                self.open_source_selector();
            }
            (KeyModifiers::CONTROL, KeyCode::Left)
            | (KeyModifiers::CONTROL, KeyCode::Char('h'))
            | (KeyModifiers::NONE, KeyCode::Char('[')) => self.select_previous_source(),
            (KeyModifiers::CONTROL, KeyCode::Right) | (KeyModifiers::NONE, KeyCode::Char(']')) => {
                self.select_next_source()
            }
            (KeyModifiers::NONE, KeyCode::Left) if self.selected_board.is_none() => {
                self.select_previous_source();
            }
            (KeyModifiers::NONE, KeyCode::Right) if self.selected_board.is_none() => {
                self.select_next_source();
            }
            (KeyModifiers::NONE, KeyCode::Up) => {
                self.move_selection_up(ctx);
            }
            (KeyModifiers::NONE, KeyCode::Down) => {
                self.move_selection_down(ctx);
            }
            (KeyModifiers::NONE, KeyCode::Home) | (KeyModifiers::NONE, KeyCode::Char('g')) => {
                self.selected = 0;
            }
            (KeyModifiers::NONE, KeyCode::End)
            | (KeyModifiers::NONE, KeyCode::Char('G'))
            | (KeyModifiers::SHIFT, KeyCode::Char('G')) => {
                self.selected = self.current_list_len().saturating_sub(1);
            }
            (KeyModifiers::CONTROL, KeyCode::Char('u')) | (KeyModifiers::NONE, KeyCode::PageUp) => {
                self.selected = self.selected.saturating_sub(ctx.page_step());
            }
            (KeyModifiers::CONTROL, KeyCode::Char('d'))
            | (KeyModifiers::NONE, KeyCode::PageDown) => {
                self.selected = (self.selected + ctx.page_step())
                    .min(self.current_list_len().saturating_sub(1));
            }
            _ if super::is_song_activation_key(key) => {
                if self.selected_board.is_some() && !self.songs.is_empty() {
                    return AppAction::PlaySong {
                        songs: self.songs.clone(),
                        index: self.selected,
                    };
                }
                self.enter_selected_board();
            }
            (KeyModifiers::NONE, KeyCode::Char('a')) if self.selected_board.is_some() => {
                if let Some(song) = self.songs.get(self.selected).cloned() {
                    return AppAction::AddToQueue {
                        song: Box::new(song),
                        position: InsertPosition::End,
                    };
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('A'))
            | (KeyModifiers::SHIFT, KeyCode::Char('A'))
                if self.selected_board.is_some() =>
            {
                if let Some(song) = self.songs.get(self.selected).cloned() {
                    return AppAction::AddToQueue {
                        song: Box::new(song),
                        position: InsertPosition::Next,
                    };
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('f')) if self.selected_board.is_some() => {
                if let Some(song) = self.songs.get(self.selected).cloned() {
                    return AppAction::ToggleFavoriteSong(Box::new(song));
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('h'))
            | (KeyModifiers::NONE, KeyCode::Left)
            | (KeyModifiers::NONE, KeyCode::Esc)
                if self.selected_board.is_some() =>
            {
                self.leave_board();
            }
            (KeyModifiers::NONE, KeyCode::Char('r')) => self.refresh_current(),
            _ => {}
        }
        AppAction::None
    }

    fn compute_layout(&self, area: Rect, board_count: usize) -> PageChunks {
        let wide = area.width >= LB_WIDE_MIN_WIDTH;

        let min_boards_rows = (board_count as u16 + 2).clamp(5, 11);
        let min_boards_cols: u16 = 12;
        let min_songs_cols: u16 = 20;
        let min_songs_rows: u16 = 5;

        if !wide {
            // 上下分栏：分割线占独立 1 行 gutter，两块面板只能用 `usable` 行。
            // 比例仍按整个内容区高度算，拖拽时指针落在哪一行线就跟到哪一行。
            let usable = area.height.saturating_sub(GUTTER);
            let ratio = self
                .splitter
                .effective(&LBResizeTarget::NarrowBoards, self.boards_ratio_narrow);
            let desired = ((area.height as f32) * ratio).round() as u16;
            let max_boards = usable.saturating_sub(min_songs_rows);
            // 窗口太矮时先降低下限，别把歌曲面板挤成 0（那样分割线也没了）。
            let min_boards = min_boards_rows.min(max_boards);
            let boards_height = clamp_extent(desired, min_boards, max_boards);
            let (boards, songs) = split_with_gutter(area, SplitAxis::Horizontal, boards_height);
            PageChunks {
                boards,
                songs,
                wide: false,
            }
        } else {
            // 左右分栏：同理，中间留 1 列 gutter，歌曲面板不再少一列边框。
            let usable = area.width.saturating_sub(GUTTER);
            let ratio = self
                .splitter
                .effective(&LBResizeTarget::WideBoards, self.boards_ratio_wide);
            let desired = ((area.width as f32) * ratio).round() as u16;
            let max_boards = usable.saturating_sub(min_songs_cols);
            let min_boards = min_boards_cols.min(max_boards);
            let boards_width = clamp_extent(desired, min_boards, max_boards);
            let (boards, songs) = split_with_gutter(area, SplitAxis::Vertical, boards_width);
            PageChunks {
                boards,
                songs,
                wide: true,
            }
        }
    }

    /// 当前布局下**唯一**可拖拽的那条分割线（方向由布局决定，不靠几何猜）。
    ///
    /// 坐标来自 [`divider_line`]，也就是 `split_with_gutter` 留出的那 1 格
    /// gutter：`render_resize_dividers` 与 `resize_target_at` 共用这一份结果。
    fn divider(&self, layout: &PageChunks) -> Option<(LBResizeTarget, DividerHit)> {
        if layout.wide {
            if layout.boards.width == 0 || layout.songs.width == 0 {
                return None;
            }
            let x = divider_line(layout.boards, SplitAxis::Vertical);
            Some((
                LBResizeTarget::WideBoards,
                DividerHit::new(
                    SplitAxis::Vertical,
                    x,
                    (layout.boards.y, layout.boards.bottom()),
                ),
            ))
        } else {
            if layout.boards.height == 0 || layout.songs.height == 0 {
                return None;
            }
            let y = divider_line(layout.boards, SplitAxis::Horizontal);
            Some((
                LBResizeTarget::NarrowBoards,
                DividerHit::new(
                    SplitAxis::Horizontal,
                    y,
                    (layout.boards.x, layout.boards.right()),
                ),
            ))
        }
    }

    fn resize_target_at(&self, event: MouseEvent, layout: &PageChunks) -> Option<LBResizeTarget> {
        let (target, hit) = self.divider(layout)?;
        hit.matches(event.column, event.row).then_some(target)
    }

    fn clamp_resize_ratio(target: LBResizeTarget, ratio: f32) -> f32 {
        match target {
            LBResizeTarget::WideBoards => clamp_ratio(ratio, 0.15, 0.55),
            LBResizeTarget::NarrowBoards => clamp_ratio(ratio, 0.10, 0.40),
        }
    }

    /// 该分割线已提交的比例（用于开始拖拽时取初值）。
    fn committed_ratio(&self, target: LBResizeTarget) -> f32 {
        match target {
            LBResizeTarget::WideBoards => self.boards_ratio_wide,
            LBResizeTarget::NarrowBoards => self.boards_ratio_narrow,
        }
    }

    fn update_resize_preview(&mut self, target: LBResizeTarget, event: MouseEvent, area: Rect) {
        let raw_ratio = match target {
            LBResizeTarget::WideBoards if area.width > 0 => {
                ratio_within(area.x, area.width, event.column)
            }
            LBResizeTarget::NarrowBoards if area.height > 0 => {
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
            LBResizeTarget::WideBoards => {
                self.boards_ratio_wide = ratio;
                "boards_wide"
            }
            LBResizeTarget::NarrowBoards => {
                self.boards_ratio_narrow = ratio;
                "boards_narrow"
            }
        };
        Some((key, ratio))
    }

    fn cancel_resize(&mut self) {
        self.splitter.cancel();
    }

    /// 从 Config 恢复用户拖拽过的比例（页面构造后调用一次）。
    pub fn apply_pane_ratios(&mut self, ratios: &HashMap<String, f32>) {
        if let Some(value) = ratios.get("boards_wide").copied() {
            self.boards_ratio_wide = Self::clamp_resize_ratio(LBResizeTarget::WideBoards, value);
        }
        if let Some(value) = ratios.get("boards_narrow").copied() {
            self.boards_ratio_narrow =
                Self::clamp_resize_ratio(LBResizeTarget::NarrowBoards, value);
        }
    }

    pub fn pane_page_key(&self) -> &'static str {
        LB_PAGE_KEY
    }

    /// 把两处分栏比例恢复成内置默认值（与 `apply_pane_ratios` 对称）。
    ///
    /// 调用方同时要删掉持久化的 `pane_ratios` 条目，否则下次启动又被旧值覆盖。
    pub fn reset_pane_ratios(&mut self) {
        self.boards_ratio_wide = LB_DEFAULT_BOARDS_RATIO_WIDE;
        self.boards_ratio_narrow = LB_DEFAULT_BOARDS_RATIO_NARROW;
        self.splitter.cancel();
    }

    fn render_resize_dividers(&self, layout: &PageChunks, buf: &mut Buffer, ctx: &AppContext) {
        use ratatui::style::Style;
        let base = Style::new().fg(crate::theme::accent(ctx));
        let style = if self.splitter.is_dragging() {
            base.bold()
        } else {
            base
        };
        // 只画当前方向的那条线：以前两个方向都会画，只是恰好压在面板边框上才看不出来。
        let Some((_, hit)) = self.divider(layout) else {
            return;
        };
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

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        let shell = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(area);
        self.render_source_tabs(shell[0], buf, ctx);
        let page = self.compute_layout(shell[1], self.boards.len());
        self.render_boards(page.boards, buf, ctx);
        self.render_songs(page.songs, buf, ctx);
        self.render_resize_dividers(&page, buf, ctx);
        self.render_source_selector(area, buf, ctx);
    }

    /// 歌曲表头所在的一行（供 main.rs 判定"表头右键 → 列菜单"）。
    pub fn table_header_rect(&self, area: Rect, _ctx: &AppContext) -> Option<Rect> {
        self.selected_board?;
        let shell = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(area);
        let page = self.compute_layout(shell[1], self.boards.len());
        let inner = panel_inner(page.songs);
        (inner.height > 0).then(|| Rect::new(inner.x, inner.y, inner.width, 1))
    }

    /// 自动列宽的测量样本，**无副作用**。
    pub fn autofit_samples(&self, _ctx: &AppContext) -> Vec<SongInfo> {
        self.songs.clone()
    }

    /// 兜底取消所有进行中的拖拽会话（分割条 + 列宽）。
    pub fn abort_drag_sessions(&mut self) {
        self.splitter.cancel();
        self.column_resize = None;
    }

    pub fn handle_mouse(
        &mut self,
        event: MouseEvent,
        area: Rect,
        activate: bool,
        ctx: &AppContext,
    ) -> AppAction {
        if self
            .source_selector
            .as_ref()
            .is_some_and(|selector| selector.is_open())
        {
            let result = self
                .source_selector
                .as_mut()
                .and_then(|selector| selector.handle_mouse(event, area));
            if let Some(SourceSelectorKey::Source(source)) = result
                && let Some(index) = self
                    .sources
                    .iter()
                    .position(|candidate| *candidate == source)
            {
                self.select_source(index);
            }
            return AppAction::None;
        }
        let shell = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(area);
        if matches!(event.kind, MouseEventKind::Down(MouseButton::Left))
            && let Some(selector) = self.source_selector.as_ref()
            && let Some(SourceSelectorKey::Source(source)) =
                selector.tab_at(shell[0], (event.column, event.row).into())
            && let Some(index) = self
                .sources
                .iter()
                .position(|candidate| *candidate == source)
        {
            self.select_source(index);
            return AppAction::None;
        }
        let page = self.compute_layout(shell[1], self.boards.len());
        if let Some(target) = self.splitter.dragging().copied() {
            match event.kind {
                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved => {
                    self.update_resize_preview(target, event, shell[1]);
                    return AppAction::None;
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    return match self.commit_resize() {
                        Some((ratio_key, ratio)) => AppAction::CommitPaneRatio {
                            page_key: LB_PAGE_KEY.to_string(),
                            ratio_key: ratio_key.to_string(),
                            ratio,
                        },
                        None => AppAction::None,
                    };
                }
                _ => return AppAction::None,
            }
        } else if matches!(event.kind, MouseEventKind::Down(MouseButton::Left))
            && let Some(target) = self.resize_target_at(event, &page)
        {
            let committed = self.committed_ratio(target);
            self.splitter.begin(target, committed);
            return AppAction::None;
        }

        if self.selected_board.is_some() {
            let songs_inner = panel_inner(page.songs);
            match super::components::song_table::handle_column_resize(
                &mut self.column_resize,
                &mut self.song_columns,
                event,
                Some(Rect::new(
                    songs_inner.x,
                    songs_inner.y,
                    songs_inner.width,
                    1,
                )),
                songs_inner,
            ) {
                super::components::song_table::ColumnResizeOutcome::Updated => {
                    return AppAction::None;
                }
                super::components::song_table::ColumnResizeOutcome::Finished => {
                    return AppAction::CommitColumnResize {
                        page_key: "leaderboard".to_string(),
                        columns: self.song_columns.clone(),
                    };
                }
                super::components::song_table::ColumnResizeOutcome::NotHandled => {}
            }
        }

        let position = Position::new(event.column, event.row);
        let scroll_amount = ctx
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .ui
            .scroll_amount
            .max(1);
        match event.kind {
            // 滚轮只在光标位于榜单面板内时才改选中（以前在歌曲面板/空白处滚轮
            // 也会移动榜单选中）。
            MouseEventKind::ScrollUp if page.boards.contains(position) => {
                self.selected = self.selected.saturating_sub(scroll_amount);
            }
            MouseEventKind::ScrollDown if page.boards.contains(position) => {
                self.selected =
                    (self.selected + scroll_amount).min(self.current_list_len().saturating_sub(1));
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(index) = crate::pages::components::hit_test::row_at(
                    page.boards,
                    position,
                    self.board_scroll_offset,
                    self.boards.len(),
                    0,
                ) {
                    self.handle_board_click(index, activate);
                    return AppAction::None;
                }

                if self.selected_board.is_some()
                    && let Some(index) = crate::pages::components::hit_test::row_at(
                        page.songs,
                        position,
                        self.song_scroll_offset,
                        self.songs.len(),
                        1,
                    )
                {
                    self.selected = index;
                    if activate {
                        return AppAction::PlaySong {
                            songs: self.songs.clone(),
                            index,
                        };
                    }
                }
            }
            _ => {}
        }
        AppAction::None
    }

    pub fn context_song_at(
        &mut self,
        source: MenuHitSource,
        area: Rect,
    ) -> Option<(Vec<SongInfo>, usize)> {
        self.selected_board?;
        let shell = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(area);
        let page = self.compute_layout(shell[1], self.boards.len());
        let index = source.resolve_index(
            |event| {
                crate::pages::components::hit_test::row_at(
                    page.songs,
                    Position::new(event.column, event.row),
                    self.song_scroll_offset,
                    self.songs.len(),
                    1,
                )
            },
            self.selected,
            self.songs.len(),
        )?;
        self.selected = index;
        Some((self.songs.clone(), index))
    }

    fn render_source_tabs(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        if let Some(selector) = self.source_selector.as_ref() {
            selector.render_tabs(area, buf, ctx);
        }
    }

    fn render_boards(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        let block = Block::default()
            .borders(PANEL_BORDERS)
            .border_style(Style::new().fg(crate::theme::border(ctx)))
            .title(format!(
                "榜单 · {} · {} 个 · P 切换音源",
                self.current_source().map(source_name).unwrap_or("无"),
                self.boards.len()
            ));
        let inner = block.inner(area);
        block.render(area, buf);
        if self.sources.is_empty() {
            self.render_muted("没有启用在线音源", inner, buf, ctx);
            return;
        }
        if self.selected_board.is_none() {
            if let Some(error) = &self.error_message {
                Paragraph::new(format!("加载失败: {error}"))
                    .style(Style::new().fg(crate::theme::red(ctx)))
                    .render(inner, buf);
                return;
            }
            if self.boards_loading {
                self.render_muted("加载榜单目录...", inner, buf, ctx);
                return;
            }
            if self.boards_loaded && self.boards.is_empty() {
                self.render_muted("该音源暂无榜单", inner, buf, ctx);
                return;
            }
        }
        if inner.height == 0 || self.boards.is_empty() {
            return;
        }

        let visible_height = inner.height as usize;
        if self.selected_board.is_none() {
            ensure_visible(
                self.selected,
                visible_height,
                self.boards.len(),
                &mut self.board_scroll_offset,
            );
        }
        let selected_style = Style::new()
            .bg(crate::theme::accent(ctx))
            .fg(crate::theme::selection_fg(ctx))
            .add_modifier(Modifier::BOLD);
        for index in self.board_scroll_offset
            ..(self.board_scroll_offset + visible_height).min(self.boards.len())
        {
            let board = &self.boards[index];
            let prefix = format!("{:>2}. ", index + 1);
            let available = inner.width.saturating_sub(prefix.chars().count() as u16) as usize;
            let mut label = board.name.clone();
            if inner.width >= 34
                && let Some(update) = board.update.as_deref()
            {
                // 精确到秒的时间戳会占掉 1/3 宽度把榜单名截断，压缩成相对时间。
                label = format!("{}  {}", board.name, format_board_update(update));
            }
            let text = format!("{prefix}{}", truncate_chars(&label, available));
            let style = if self.selected_board.is_none() && index == self.selected {
                selected_style
            } else if self.selected_board == Some(index) {
                Style::new()
                    .fg(crate::theme::accent(ctx))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(crate::theme::text(ctx))
            };
            Paragraph::new(Line::from(Span::styled(text, style))).render(
                Rect::new(
                    inner.x,
                    inner.y + (index - self.board_scroll_offset) as u16,
                    inner.width,
                    1,
                ),
                buf,
            );
        }
    }

    fn render_songs(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        let title = self
            .current_board()
            .map(|board| format!("{} · {}", source_name(board.source), board.name))
            .unwrap_or_else(|| "歌曲列表".to_string());
        let block = Block::default()
            .borders(PANEL_BORDERS)
            .border_style(Style::new().fg(crate::theme::border(ctx)))
            .title(title);
        let inner = block.inner(area);
        block.render(area, buf);

        if self.selected_board.is_none() {
            self.render_muted("选择一个榜单", inner, buf, ctx);
            return;
        }
        if let Some(error) = &self.error_message {
            Paragraph::new(format!("加载失败: {error}"))
                .style(Style::new().fg(crate::theme::red(ctx)))
                .render(inner, buf);
            return;
        }
        if self.songs_loading {
            self.render_muted("加载榜单歌曲...", inner, buf, ctx);
            return;
        }
        if self.songs_loaded && self.songs.is_empty() {
            self.render_muted("该榜单暂无歌曲", inner, buf, ctx);
            return;
        }
        if self.songs.is_empty() || inner.height == 0 {
            return;
        }

        // 拖拽期间以页面状态为准，否则拖拽结果会被每帧重载覆盖。
        if self.column_resize.is_none() {
            let cfg = ctx.config.read().unwrap_or_else(|e| e.into_inner());
            self.song_columns = super::components::song_table::load_columns_for_page(
                &cfg.ui.table_columns,
                "leaderboard",
                inner.width,
            );
        }

        super::components::song_table::header_paragraph(
            inner.width,
            &self.song_columns,
            super::components::song_table::TablePalette::from_theme(ctx),
        )
        .render(Rect::new(inner.x, inner.y, inner.width, 1), buf);
        let list_area = Rect::new(
            inner.x,
            inner.y.saturating_add(1),
            inner.width,
            inner.height.saturating_sub(1),
        );
        let visible_height = list_area.height as usize;
        if visible_height == 0 {
            return;
        }
        ensure_visible(
            self.selected,
            visible_height,
            self.songs.len(),
            &mut self.song_scroll_offset,
        );
        let selected_style = Style::new()
            .bg(crate::theme::accent(ctx))
            .fg(crate::theme::selection_fg(ctx))
            .add_modifier(Modifier::BOLD);
        for index in self.song_scroll_offset
            ..(self.song_scroll_offset + visible_height).min(self.songs.len())
        {
            let row_paragraph = super::components::song_table::row_paragraph(
                &self.songs[index],
                index,
                list_area.width,
                &self.song_columns,
                super::components::song_table::TablePalette::from_theme(ctx),
            );
            let style = if index == self.selected {
                selected_style
            } else {
                Style::new().fg(crate::theme::text(ctx))
            };
            row_paragraph.style(style).render(
                Rect::new(
                    list_area.x,
                    list_area.y + (index - self.song_scroll_offset) as u16,
                    list_area.width,
                    1,
                ),
                buf,
            );
        }
    }

    fn render_muted(&self, text: &str, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        Paragraph::new(text)
            .style(Style::new().fg(crate::theme::muted(ctx)))
            .render(area, buf);
    }

    fn move_selection_up(&mut self, ctx: &AppContext) {
        let len = self.current_list_len();
        if len == 0 {
            return;
        }
        if self.selected > 0 {
            self.selected -= 1;
        } else if ctx
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .ui
            .wrap_navigation
        {
            self.selected = len - 1;
        }
    }

    fn move_selection_down(&mut self, ctx: &AppContext) {
        let len = self.current_list_len();
        if len == 0 {
            return;
        }
        if self.selected + 1 < len {
            self.selected += 1;
        } else if ctx
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .ui
            .wrap_navigation
        {
            self.selected = 0;
        }
    }

    fn current_list_len(&self) -> usize {
        if self.selected_board.is_some() {
            self.songs.len()
        } else {
            self.boards.len()
        }
    }

    fn handle_board_click(&mut self, index: usize, activate: bool) {
        if index >= self.boards.len() {
            return;
        }
        if activate {
            if self.selected_board.is_some() {
                self.leave_board();
            }
            self.selected = index;
            self.enter_selected_board();
        } else if self.selected_board.is_none() {
            self.selected = index;
        }
    }

    fn enter_selected_board(&mut self) {
        if self.selected_board.is_some() || self.selected >= self.boards.len() {
            return;
        }
        let board_index = self.selected;
        let board = &self.boards[board_index];
        let cache_key = (board.source, board.id.clone());
        self.selected_board = Some(board_index);
        self.selected = 0;
        self.song_scroll_offset = 0;
        self.error_message = None;
        self.songs_loading = false;
        if let Some(songs) = self.song_cache.get(&cache_key) {
            self.songs = songs.clone();
            self.songs_loaded = true;
        } else {
            self.songs.clear();
            self.songs_loaded = false;
        }
    }

    fn leave_board(&mut self) {
        let board_index = self.selected_board.take().unwrap_or_default();
        self.songs.clear();
        self.songs_loaded = false;
        self.songs_loading = false;
        self.error_message = None;
        self.selected = board_index.min(self.boards.len().saturating_sub(1));
        self.song_scroll_offset = 0;
    }

    fn refresh_current(&mut self) {
        let Some(source) = self.current_source() else {
            return;
        };
        self.error_message = None;
        if let Some(board) = self.current_board() {
            self.song_cache.remove(&(source, board.id.clone()));
            self.songs.clear();
            self.songs_loaded = false;
            self.songs_loading = false;
            self.selected = 0;
            self.song_scroll_offset = 0;
        } else {
            self.board_cache.remove(&source);
            self.boards.clear();
            self.boards_loaded = false;
            self.boards_loading = false;
            self.selected = 0;
            self.board_scroll_offset = 0;
        }
    }

    fn select_previous_source(&mut self) {
        if self.sources.is_empty() {
            return;
        }
        let index = if self.source_index == 0 {
            self.sources.len() - 1
        } else {
            self.source_index - 1
        };
        self.select_source(index);
    }

    fn select_next_source(&mut self) {
        if self.sources.is_empty() {
            return;
        }
        self.select_source((self.source_index + 1) % self.sources.len());
    }

    fn select_source(&mut self, index: usize) {
        if index >= self.sources.len() || index == self.source_index {
            return;
        }
        self.source_index = index;
        if let Some(selector) = self.source_selector.as_mut() {
            selector.select(index);
        }
        self.selected_board = None;
        self.songs.clear();
        self.songs_loaded = false;
        self.songs_loading = false;
        self.error_message = None;
        self.selected = 0;
        self.board_scroll_offset = 0;
        self.song_scroll_offset = 0;
        let source = self.sources[index];
        if let Some(boards) = self.board_cache.get(&source) {
            self.boards = boards.clone();
            self.boards_loaded = true;
            self.boards_loading = false;
        } else {
            self.boards.clear();
            self.boards_loaded = false;
            self.boards_loading = false;
        }
    }
    fn open_source_selector(&mut self) {
        if let Some(selector) = self.source_selector.as_mut() {
            selector.select(self.source_index);
            selector.open();
        }
    }

    fn handle_source_selector(&mut self, key: &KeyEvent) {
        let Some(selector) = self.source_selector.as_mut() else {
            return;
        };
        if let Some(SourceSelectorKey::Source(source)) = selector.handle_key(*key) {
            if let Some(index) = self
                .sources
                .iter()
                .position(|candidate| *candidate == source)
            {
                self.select_source(index);
            }
            if let Some(selector) = self.source_selector.as_mut() {
                selector.close()
            }
        } else if !selector.is_open()
            && let Some(selector) = self.source_selector.as_mut()
        {
            selector.close()
        }
    }

    fn render_source_selector(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        if let Some(selector) = self.source_selector.as_mut() {
            selector.render_popup(area, buf, ctx, "选择音源");
        }
    }
}

struct PageChunks {
    boards: Rect,
    songs: Rect,
    /// 是否宽布局（左右并排）。**必须有这个标志**：否则分割条只能靠几何猜方向，
    /// 窄屏下"每行最右一列"就会被误判成竖分割条。
    wide: bool,
}

fn ensure_visible(selected: usize, visible: usize, total: usize, offset: &mut usize) {
    // 共享实现在 components::scroll，leaderboard / playlists 保持一致行为。
    crate::pages::components::scroll::ensure_visible(selected, visible, total, offset)
}

fn source_name(source: SourceId) -> &'static str {
    source.display_name()
}

fn truncate_chars(value: &str, max: usize) -> String {
    super::components::text::truncate_width(value, max).into_owned()
}

/// 把接口返回的「YYYY-MM-DD HH:MM:SS」压缩成相对时间：
/// 今天 → `今天 HH:MM`；今年 → `MM-DD HH:MM`；更早 → `YYYY-MM-DD`。
/// 解析失败原样返回（不同音源的格式不保证一致）。
fn format_board_update(raw: &str) -> String {
    use chrono::{Datelike, Local, NaiveDateTime};
    NaiveDateTime::parse_from_str(raw.trim(), "%Y-%m-%d %H:%M:%S")
        .map(|time| {
            let today = Local::now().date_naive();
            let date = time.date();
            if date == today {
                format!("今天 {}", time.format("%H:%M"))
            } else if date.year() == today.year() {
                time.format("%m-%d %H:%M").to_string()
            } else {
                time.format("%Y-%m-%d").to_string()
            }
        })
        .unwrap_or_else(|_| raw.to_string())
}

#[cfg(test)]
mod board_update_tests {
    use super::format_board_update;

    #[test]
    fn compresses_timestamps_by_distance() {
        use chrono::{Duration, Local, NaiveDateTime};
        let fmt = |time: NaiveDateTime| time.format("%Y-%m-%d %H:%M:%S").to_string();

        // 今天 → 只剩时分。
        let today = Local::now().date_naive().and_hms_opt(9, 30, 1).unwrap();
        assert_eq!(format_board_update(&fmt(today)), "今天 09:30");

        // 今年更早 → 月-日 时分。
        let earlier_this_year = today - Duration::days(30);
        let expected = earlier_this_year.format("%m-%d %H:%M").to_string();
        assert_eq!(format_board_update(&fmt(earlier_this_year)), expected);

        // 去年 → 只留日期。
        let last_year = today - Duration::days(400);
        let expected = last_year.format("%Y-%m-%d").to_string();
        assert_eq!(format_board_update(&fmt(last_year)), expected);

        // 解析不了的原样返回（不同音源格式不保证一致）。
        assert_eq!(format_board_update("每周四更新"), "每周四更新");
    }
}

#[cfg(test)]
mod tests {
    use super::{LeaderboardLoadRequest, LeaderboardPage};
    use crate::pages::components::hit_test::{panel_inner, row_at};
    use crate::pages::components::splitter::{GUTTER, SplitAxis, divider_line};
    use lx_core::model::leaderboard::LeaderboardInfo;
    use lx_core::model::song::SongInfo;
    use lx_core::model::source::SourceId;
    use ratatui::layout::{Position, Rect};

    /// 恢复默认布局必须把两处分栏比例都写回内置默认（否则"恢复默认面板布局"
    /// 只删了配置，界面要重启才变）。
    #[test]
    fn reset_pane_ratios_restores_both_defaults() {
        let mut page = LeaderboardPage::new(vec![SourceId::Kw]);
        page.apply_pane_ratios(&std::collections::HashMap::from([
            ("boards_wide".to_string(), 0.75_f32),
            ("boards_narrow".to_string(), 0.7_f32),
        ]));
        assert_ne!(page.boards_ratio_wide, super::LB_DEFAULT_BOARDS_RATIO_WIDE);

        page.reset_pane_ratios();

        assert_eq!(page.boards_ratio_wide, super::LB_DEFAULT_BOARDS_RATIO_WIDE);
        assert_eq!(
            page.boards_ratio_narrow,
            super::LB_DEFAULT_BOARDS_RATIO_NARROW
        );
        assert!(!page.splitter.is_dragging());
    }

    #[test]
    fn caches_each_source_and_refreshes_the_current_view() {
        let mut page = LeaderboardPage::new(vec![SourceId::Kw, SourceId::Kg]);
        assert_eq!(
            page.next_load_request(),
            Some(LeaderboardLoadRequest::Boards {
                source: SourceId::Kw
            })
        );

        let board = LeaderboardInfo::new("93".to_string(), "飙升榜".to_string(), SourceId::Kw);
        page.update_boards(SourceId::Kw, vec![board.clone()]);
        page.enter_selected_board();
        assert_eq!(
            page.next_load_request(),
            Some(LeaderboardLoadRequest::Songs {
                source: SourceId::Kw,
                board_id: "93".to_string()
            })
        );

        let song = SongInfo::new(
            "1".to_string(),
            SourceId::Kw,
            "测试歌曲".to_string(),
            "测试歌手".to_string(),
        );
        page.update_songs(SourceId::Kw, "93", vec![song]);
        assert!(page.next_load_request().is_none());

        page.leave_board();
        page.select_source(1);
        assert_eq!(
            page.next_load_request(),
            Some(LeaderboardLoadRequest::Boards {
                source: SourceId::Kg
            })
        );

        page.select_source(0);
        assert_eq!(page.boards, vec![board]);
        assert!(page.next_load_request().is_none());
        page.refresh_current();
        assert_eq!(
            page.next_load_request(),
            Some(LeaderboardLoadRequest::Boards {
                source: SourceId::Kw
            })
        );
    }
    #[test]
    fn activating_another_board_does_not_require_leaving_first() {
        let mut page = LeaderboardPage::new(vec![SourceId::Kw]);
        let first = LeaderboardInfo::new("first".to_string(), "第一榜".to_string(), SourceId::Kw);
        let second = LeaderboardInfo::new("second".to_string(), "第二榜".to_string(), SourceId::Kw);
        page.update_boards(SourceId::Kw, vec![first, second]);

        page.handle_board_click(0, true);
        assert_eq!(page.selected_board, Some(0));

        page.handle_board_click(1, true);
        assert_eq!(page.selected_board, Some(1));
        assert_eq!(
            page.next_load_request(),
            Some(LeaderboardLoadRequest::Songs {
                source: SourceId::Kw,
                board_id: "second".to_string(),
            })
        );
    }

    /// 回归（窄布局）：横分割线占**自己**那一行 gutter，榜单与歌曲面板的
    /// 最后一行内容既不被它覆盖，也仍然能被鼠标点中。
    #[test]
    fn narrow_divider_row_is_a_gutter_and_the_last_song_row_stays_clickable() {
        let page = LeaderboardPage::new(Vec::new());
        let area = Rect::new(0, 0, 60, 24);
        let chunks = page.compute_layout(area, 0);
        let (_, hit) = page.divider(&chunks).expect("窄布局必须有横分割线");

        // 两块面板 + 1 行 gutter 恰好铺满内容区
        assert_eq!(
            hit.divider,
            divider_line(chunks.boards, SplitAxis::Horizontal)
        );
        assert_eq!(chunks.songs.y, hit.divider + GUTTER);
        assert_eq!(
            chunks.boards.height + GUTTER + chunks.songs.height,
            area.height
        );

        let boards_inner = panel_inner(chunks.boards);
        let songs_inner = panel_inner(chunks.songs);
        // 分隔线所在的那一行不属于任何面板的内容区
        assert!(
            boards_inner.bottom() < hit.divider,
            "榜单内容区必须在 gutter 之上"
        );
        assert!(songs_inner.y > hit.divider, "歌曲内容区必须在 gutter 之下");

        // 滚到底：榜单面板最后一行内容能命中最后一条榜单
        let visible_boards = boards_inner.height as usize;
        assert_eq!(
            row_at(
                chunks.boards,
                Position::new(boards_inner.x + 1, boards_inner.bottom() - 1),
                0,
                visible_boards,
                0,
            ),
            Some(visible_boards - 1)
        );
        // 歌曲面板最后一行内容能命中最后一首歌（表格占 1 行表头）
        let visible_songs = songs_inner.height as usize - 1;
        assert_eq!(
            row_at(
                chunks.songs,
                Position::new(songs_inner.x + 1, songs_inner.bottom() - 1),
                0,
                visible_songs,
                1,
            ),
            Some(visible_songs - 1)
        );
        // gutter 那一行不是内容区，点它不会选中列表项
        assert_eq!(
            row_at(
                chunks.boards,
                Position::new(boards_inner.x + 1, hit.divider),
                0,
                99,
                0
            ),
            None
        );
        assert_eq!(
            row_at(
                chunks.songs,
                Position::new(songs_inner.x + 1, hit.divider),
                0,
                99,
                1
            ),
            None
        );
        // 拖拽命中落在 gutter 上，±GRAB_RADIUS 的容差也不会吃掉最后一行内容
        assert!(hit.matches(boards_inner.x, hit.divider));
        assert!(!hit.matches(boards_inner.x, boards_inner.bottom() - 1));
        assert!(!hit.matches(songs_inner.x, songs_inner.y));
    }

    /// 回归（宽布局）：竖分割线占左侧面板右边的那 1 列 gutter，
    /// 歌曲面板的最后一列内容照样点得中。
    #[test]
    fn wide_divider_column_is_a_gutter_and_the_last_song_column_stays_clickable() {
        let page = LeaderboardPage::new(Vec::new());
        let area = Rect::new(0, 0, 120, 30);
        let chunks = page.compute_layout(area, 0);
        let (_, hit) = page.divider(&chunks).expect("宽布局必须有竖分割线");

        assert_eq!(
            hit.divider,
            divider_line(chunks.boards, SplitAxis::Vertical)
        );
        assert_eq!(chunks.songs.x, hit.divider + GUTTER);
        assert_eq!(
            chunks.boards.width + GUTTER + chunks.songs.width,
            area.width
        );

        let boards_inner = panel_inner(chunks.boards);
        let songs_inner = panel_inner(chunks.songs);
        assert!(
            boards_inner.right() <= hit.divider,
            "榜单内容区必须在 gutter 之左"
        );
        assert!(songs_inner.x > hit.divider, "歌曲内容区必须在 gutter 之右");

        // 歌曲面板最后一列内容（第一首歌那一行）能命中
        let visible_songs = songs_inner.height as usize - 1;
        assert_eq!(
            row_at(
                chunks.songs,
                Position::new(songs_inner.right() - 1, songs_inner.y + 1),
                0,
                visible_songs,
                1,
            ),
            Some(0)
        );
        // gutter 那一列不是内容区
        assert_eq!(
            row_at(
                chunks.songs,
                Position::new(hit.divider, songs_inner.y + 1),
                0,
                visible_songs,
                1,
            ),
            None
        );
        // 拖拽命中落在 gutter 上，容差不碰两侧面板的最后一列/最前一列
        assert!(hit.matches(hit.divider, songs_inner.y));
        assert!(!hit.matches(boards_inner.right() - 1, songs_inner.y));
        assert!(!hit.matches(songs_inner.x, songs_inner.y));
    }
}
