//! 收藏页面

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lx_core::events::{AppAction, InsertPosition};
use lx_core::keybinding::{Action, KeybindingResolver};
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};

use crate::context::AppContext;
use crate::pages::components::context_menu::MenuHitSource;
use crate::pages::components::hit_test::{PANEL_BORDERS, PanelRows};
use crate::pages::components::song_table::{self, ColumnResizeState};
use crate::pages::components::source_selector::{SourceSelector, SourceSelectorKey};
use crate::pages::sort::{SortMode, SortTarget, SortedListCache};
use lx_core::model::config::TableColumnConfig;

pub struct FavoritesPage {
    selected: usize,
    scroll: usize,
    filter: super::components::list_filter::ListFilter,
    viewport_height: usize,
    sort_mode: SortMode,
    sources: Vec<SourceId>,
    source_index: usize,
    source_selector: Option<SourceSelector>,
    remote_menu: Option<(Position, usize)>,
    /// 上一次渲染时的页面区域；右键菜单的定位与命中判定共用它，
    /// 否则「渲染时贴边内缩、点击时用原始坐标」会让点中的条目错位。
    last_area: Rect,
    column_resize: Option<ColumnResizeState>,
    columns: Vec<TableColumnConfig>,
}

/// 网易云收藏右键菜单的条目。渲染与命中判定共用这一份，
/// 避免两边各写一套行偏移（曾经因此点「登录」却触发了「刷新」）。
const REMOTE_MENU_ITEMS: [&str; 3] = ["登录 / 重新登录网易云", "刷新网易云收藏", "关闭"];
const REMOTE_MENU_WIDTH: u16 = 28;
/// 上下边框各 1 行 + 3 个条目。
const REMOTE_MENU_HEIGHT: u16 = 5;

impl FavoritesPage {
    pub fn new(sources: Vec<SourceId>) -> Self {
        let source_index = sources
            .iter()
            .position(|source| *source == SourceId::Wy)
            .map(|index| index + 1)
            .unwrap_or(0);
        Self {
            selected: 0,
            scroll: 0,
            filter: super::components::list_filter::ListFilter::new(),
            viewport_height: 1,
            sort_mode: SortMode::Newest,
            sources: sources.clone(),
            // 收藏页默认展示网易云收藏；其他音源仍可通过 P 切换。
            source_index,
            source_selector: Some(SourceSelector::from_sources(&sources, true)),
            remote_menu: None,
            last_area: Rect::default(),
            column_resize: None,
            columns: Vec::new(),
        }
    }

    pub fn input_mode(&self) -> bool {
        self.filter.is_active()
    }

    pub fn sort_mode(&self) -> SortMode {
        self.sort_mode
    }

    pub fn sort_label(&self) -> &'static str {
        self.sort_mode.label(SortTarget::Favorites)
    }

    pub fn cycle_sort(&mut self) -> SortMode {
        self.sort_mode = self.sort_mode.next();
        self.selected = 0;
        self.scroll = 0;
        self.sort_mode
    }

    /// 打开右键菜单。
    ///
    /// 这里就把菜单左上角夹进页面范围内并记下来，渲染和命中判定都直接用这个
    /// 坐标：之前渲染会贴边内缩、而点击用的是原始右键坐标，靠边一点就会错位。
    pub fn open_remote_menu(&mut self, origin: Position) {
        let area = self.last_area;
        let (x, y) = if area.width >= REMOTE_MENU_WIDTH && area.height >= REMOTE_MENU_HEIGHT {
            (
                origin.x.min(area.right().saturating_sub(REMOTE_MENU_WIDTH)),
                origin
                    .y
                    .min(area.bottom().saturating_sub(REMOTE_MENU_HEIGHT)),
            )
        } else {
            (origin.x, origin.y)
        };
        self.remote_menu = Some((Position::new(x, y), 0));
    }

    pub fn remote_menu_open(&self) -> bool {
        self.remote_menu.is_some()
    }

    pub fn handle_remote_menu_mouse(&mut self, event: MouseEvent) -> Option<AppAction> {
        let (origin, selected) = self.remote_menu?;
        let rect = Rect::new(origin.x, origin.y, REMOTE_MENU_WIDTH, REMOTE_MENU_HEIGHT);
        // 条目区 = 去掉边框后的内部区域。渲染与命中判定共用它，
        // 就不会再出现「渲染多一行 / 点击少一行」这类错位。
        let inner = Block::default().borders(Borders::ALL).inner(rect);
        match event.kind {
            MouseEventKind::Down(MouseButton::Left)
                if inner.contains(Position::new(event.column, event.row)) =>
            {
                let item = event.row.saturating_sub(inner.y) as usize;
                self.remote_menu = None;
                if item >= REMOTE_MENU_ITEMS.len() {
                    return None;
                }
                return match REMOTE_MENU_ITEMS[item] {
                    // 「关闭」：只关菜单（上面已关），不派发动作。
                    "关闭" => None,
                    _ if item == 0 => Some(AppAction::QrLogin(
                        SourceId::Wy,
                        lx_core::model::login::QrLoginKind::Standard,
                    )),
                    _ => Some(AppAction::SyncNetease),
                };
            }
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Down(MouseButton::Right) => {
                self.remote_menu = None;
            }
            MouseEventKind::ScrollUp => {
                self.remote_menu = Some((origin, selected.saturating_sub(1)))
            }
            MouseEventKind::ScrollDown => {
                self.remote_menu = Some((origin, (selected + 1).min(REMOTE_MENU_ITEMS.len() - 1)))
            }
            _ => {}
        }
        None
    }

    fn render_remote_menu(&self, buf: &mut Buffer, ctx: &AppContext) {
        let Some((origin, selected)) = self.remote_menu else {
            return;
        };
        // 坐标已在 open_remote_menu 里夹好，这里不再二次内缩。
        let rect = Rect::new(origin.x, origin.y, REMOTE_MENU_WIDTH, REMOTE_MENU_HEIGHT);
        Clear.render(rect, buf);
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(crate::theme::accent(ctx)))
            .title(" 网易云收藏 ")
            .render(rect, buf);
        for (index, label) in REMOTE_MENU_ITEMS.iter().enumerate() {
            let style = if index == selected {
                Style::new()
                    .bg(crate::theme::accent(ctx))
                    .fg(crate::theme::selection_fg(ctx))
            } else {
                Style::new().fg(crate::theme::text(ctx))
            };
            Paragraph::new(Line::from(Span::styled(format!(" {}", label), style))).render(
                Rect::new(
                    rect.x + 1,
                    rect.y + 1 + index as u16,
                    rect.width.saturating_sub(2),
                    1,
                ),
                buf,
            );
        }
    }

    pub fn handle_input(
        &mut self,
        key: &KeyEvent,
        ctx: &AppContext,
        resolver: &KeybindingResolver,
        cache: &mut SortedListCache,
    ) -> AppAction {
        if self
            .source_selector
            .as_ref()
            .is_some_and(|selector| selector.is_open())
        {
            if matches!(
                (key.modifiers, key.code),
                (KeyModifiers::NONE, KeyCode::Enter)
            ) {
                if let Some(index) = self.source_selector.as_ref().map(|p| p.selected_index()) {
                    if let Some(selector) = self.source_selector.as_mut() {
                        selector.close();
                    }
                    self.select_source(index);
                }
            } else {
                self.handle_source_selector(key);
            }
            return AppAction::None;
        }
        let query_before = self.filter.query().to_string();
        if self.filter.handle_input(key) {
            if self.filter.query() != query_before {
                self.selected = 0;
                self.scroll = 0;
            }
            return AppAction::None;
        }

        let favorites = self.sorted_favorites(ctx, cache);
        let filtered = self.filtered_song_indices(favorites);
        self.clamp_selection(filtered.len());
        let half_page = (self.viewport_height / 2).max(1);

        if let Some(action) = resolver.resolve_page("favorites", key) {
            match action {
                Action::FavoritesFilter => {
                    self.filter.activate();
                    return AppAction::None;
                }
                Action::ListCycleSort => {
                    let mode = self.cycle_sort();
                    return AppAction::ShowNotification(lx_core::events::Notification::info(
                        format!("收藏排序: {}", mode.label(SortTarget::Favorites)),
                    ));
                }
                Action::ListSelectUp => {
                    if !filtered.is_empty() {
                        if self.selected > 0 {
                            self.selected -= 1;
                        } else if ctx
                            .config
                            .read()
                            .unwrap_or_else(|e| e.into_inner())
                            .ui
                            .wrap_navigation
                        {
                            self.selected = filtered.len().saturating_sub(1);
                        }
                    }
                    return AppAction::None;
                }
                Action::ListSelectDown => {
                    if !filtered.is_empty() {
                        if self.selected + 1 < filtered.len() {
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
                    return AppAction::None;
                }
                Action::ListSelectFirst => {
                    self.selected = 0;
                    return AppAction::None;
                }
                Action::ListSelectLast => {
                    self.selected = filtered.len().saturating_sub(1);
                    return AppAction::None;
                }
                Action::ListPageUp => {
                    self.selected = self.selected.saturating_sub(half_page);
                    return AppAction::None;
                }
                Action::ListPageDown => {
                    self.selected =
                        (self.selected + half_page).min(filtered.len().saturating_sub(1));
                    return AppAction::None;
                }
                Action::ListActivate => {
                    if self.selected < filtered.len() {
                        let songs = filtered
                            .iter()
                            .filter_map(|index| favorites.get(*index).cloned())
                            .collect::<Vec<_>>();
                        self.log_keyboard_playback(&songs);
                        return AppAction::PlaySong {
                            songs,
                            index: self.selected,
                        };
                    }
                    return AppAction::None;
                }
                Action::ListDownload => {
                    if let Some(song) = filtered
                        .get(self.selected)
                        .and_then(|index| favorites.get(*index))
                        .cloned()
                    {
                        return AppAction::DownloadSong(Box::new(song));
                    }
                    return AppAction::None;
                }
                Action::ListAddToQueue => {
                    if let Some(song) = filtered
                        .get(self.selected)
                        .and_then(|index| favorites.get(*index))
                        .cloned()
                    {
                        return AppAction::AddToQueue {
                            song: Box::new(song),
                            position: InsertPosition::End,
                        };
                    }
                    return AppAction::None;
                }
                Action::ListAddToQueueNext => {
                    if let Some(song) = filtered
                        .get(self.selected)
                        .and_then(|index| favorites.get(*index))
                        .cloned()
                    {
                        return AppAction::AddToQueue {
                            song: Box::new(song),
                            position: InsertPosition::Next,
                        };
                    }
                    return AppAction::None;
                }
                Action::FavoritesRemove => {
                    if let Some(original_index) = filtered.get(self.selected).copied()
                        && let Some(song) = favorites.get(original_index)
                        && ctx.storage.remove_favorite(song)
                    {
                        let remaining = ctx.storage.load_favorites();
                        let remaining_len = self.filtered_song_indices(&remaining).len();
                        self.clamp_selection(remaining_len);
                        return AppAction::ShowNotification(lx_core::events::Notification::info(
                            "已取消收藏",
                        ));
                    }
                    return AppAction::None;
                }
                Action::ListToggleFavorite => {
                    if let Some(original_index) = filtered.get(self.selected).copied()
                        && let Some(song) = favorites.get(original_index).cloned()
                    {
                        return AppAction::ToggleFavoriteSong(Box::new(song));
                    }
                    return AppAction::None;
                }
                Action::ListGoBack => {
                    if !self.filter.query().is_empty() {
                        self.filter.reset();
                        self.selected = 0;
                        self.scroll = 0;
                    }
                    return AppAction::None;
                }
                _ => {}
            }
        }

        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Char('p' | 'P')) => self.open_source_selector(),
            (KeyModifiers::NONE, KeyCode::Char('/')) => {
                self.filter.activate();
            }
            (KeyModifiers::NONE, KeyCode::Left) => {
                if let Some(selector) = self.source_selector.as_mut()
                    && let Some(result) = selector.cycle(-1)
                {
                    match result {
                        SourceSelectorKey::All => self.select_source(0),
                        SourceSelectorKey::Source(source) => {
                            if let Some(index) = self
                                .sources
                                .iter()
                                .position(|candidate| *candidate == source)
                            {
                                self.select_source(index + 1);
                            }
                        }
                        _ => {}
                    }
                }
            }
            (KeyModifiers::NONE, KeyCode::Right) => {
                if let Some(selector) = self.source_selector.as_mut()
                    && let Some(result) = selector.cycle(1)
                {
                    match result {
                        SourceSelectorKey::All => self.select_source(0),
                        SourceSelectorKey::Source(source) => {
                            if let Some(index) = self
                                .sources
                                .iter()
                                .position(|candidate| *candidate == source)
                            {
                                self.select_source(index + 1);
                            }
                        }
                        _ => {}
                    }
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('s')) => {
                let mode = self.cycle_sort();
                return AppAction::ShowNotification(lx_core::events::Notification::info(format!(
                    "收藏排序: {}",
                    mode.label(SortTarget::Favorites)
                )));
            }
            (KeyModifiers::NONE, KeyCode::Esc) => {
                if !self.filter.query().is_empty() {
                    self.filter.reset();
                    self.selected = 0;
                    self.scroll = 0;
                }
            }
            (KeyModifiers::NONE, KeyCode::Up) => {
                if !filtered.is_empty() {
                    if self.selected > 0 {
                        self.selected -= 1;
                    } else if ctx
                        .config
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .ui
                        .wrap_navigation
                    {
                        self.selected = filtered.len().saturating_sub(1);
                    }
                }
            }
            (KeyModifiers::NONE, KeyCode::Down) => {
                if !filtered.is_empty() {
                    if self.selected + 1 < filtered.len() {
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
            }
            (KeyModifiers::NONE, KeyCode::Home) | (KeyModifiers::NONE, KeyCode::Char('g')) => {
                self.selected = 0;
            }
            (KeyModifiers::NONE, KeyCode::End)
            | (KeyModifiers::NONE, KeyCode::Char('G'))
            | (KeyModifiers::SHIFT, KeyCode::Char('G')) => {
                self.selected = filtered.len().saturating_sub(1);
            }
            (KeyModifiers::CONTROL, KeyCode::Char('u')) | (KeyModifiers::NONE, KeyCode::PageUp) => {
                self.selected = self.selected.saturating_sub(half_page);
            }
            (KeyModifiers::CONTROL, KeyCode::Char('d'))
            | (KeyModifiers::NONE, KeyCode::PageDown) => {
                self.selected = (self.selected + half_page).min(filtered.len().saturating_sub(1));
            }
            _ if super::is_song_activation_key(key) => {
                if self.selected < filtered.len() {
                    let songs = filtered
                        .iter()
                        .filter_map(|index| favorites.get(*index).cloned())
                        .collect::<Vec<_>>();
                    self.log_keyboard_playback(&songs);
                    return AppAction::PlaySong {
                        songs,
                        index: self.selected,
                    };
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('a')) => {
                if let Some(song) = filtered
                    .get(self.selected)
                    .and_then(|index| favorites.get(*index))
                    .cloned()
                {
                    return AppAction::AddToQueue {
                        song: Box::new(song),
                        position: InsertPosition::End,
                    };
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('A'))
            | (KeyModifiers::SHIFT, KeyCode::Char('A')) => {
                if let Some(song) = filtered
                    .get(self.selected)
                    .and_then(|index| favorites.get(*index))
                    .cloned()
                {
                    return AppAction::AddToQueue {
                        song: Box::new(song),
                        position: InsertPosition::Next,
                    };
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('d')) | (KeyModifiers::NONE, KeyCode::Delete) => {
                if let Some(original_index) = filtered.get(self.selected).copied()
                    && let Some(song) = favorites.get(original_index)
                    && ctx.storage.remove_favorite(song)
                {
                    let remaining = ctx.storage.load_favorites();
                    let remaining_len = self.filtered_song_indices(&remaining).len();
                    self.clamp_selection(remaining_len);
                    return AppAction::ShowNotification(lx_core::events::Notification::info(
                        "已取消收藏",
                    ));
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('f')) => {
                if let Some(original_index) = filtered.get(self.selected).copied()
                    && let Some(song) = favorites.get(original_index).cloned()
                {
                    return AppAction::ToggleFavoriteSong(Box::new(song));
                }
            }
            // 推送收藏到网易云红心（写回，追加不删除）。
            (KeyModifiers::SHIFT, KeyCode::Char('P')) => {
                return AppAction::PushFavorites;
            }
            _ => {}
        }
        AppAction::None
    }

    pub fn render(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        ctx: &AppContext,
        cache: &mut SortedListCache,
    ) {
        let favorites = self.sorted_favorites(ctx, cache);
        let filtered = self.filtered_song_indices(favorites);
        self.clamp_selection(filtered.len());
        // 记下页面区域：右键菜单的定位与命中判定都要按它夹取，两边必须同源。
        self.last_area = area;

        let block = Block::default()
            .borders(PANEL_BORDERS)
            .border_style(Style::new().fg(crate::theme::border(ctx)))
            .title(format!(
                " {}收藏 {}/{} · {} · 排序 {} · P 音源 · 右键管理网易云 · / 筛选 ",
                if self.current_source() == Some(SourceId::Wy) {
                    "网易云 "
                } else {
                    ""
                },
                filtered.len(),
                favorites.len(),
                self.current_source()
                    .map(|s| s.display_name())
                    .unwrap_or("全部音源"),
                self.sort_label()
            ));
        block.render(area, buf);
        // 行账本：过滤行 → 音源条 → 表头 → 列表。渲染与鼠标命中共用同一份。
        // 命中侧以前漏记了"音源条"这一行，导致点击恒定选中下一首。
        let rows = PanelRows::new(
            area,
            self.filter.is_active() || !self.filter.query().is_empty(),
            true,
            true,
        );
        if rows.inner.height == 0 {
            return;
        }
        if let Some(row) = rows.filter_row() {
            self.filter.render(row, buf, ctx);
        }
        if let Some(row) = rows.toolbar_row() {
            self.render_source_tabs(row, buf, ctx);
        }
        let Some(header_row) = rows.header else {
            return;
        };

        // 拖拽期间以页面状态为准，否则拖拽结果会被每帧重载覆盖。
        if self.column_resize.is_none() {
            let cfg = ctx.config.read().unwrap_or_else(|e| e.into_inner());
            self.columns = song_table::load_columns_for_page(
                &cfg.ui.table_columns,
                "favorites",
                rows.inner.width,
            );
        }

        song_table::header_paragraph(
            rows.inner.width,
            &self.columns,
            song_table::TablePalette::from_theme(ctx),
        )
        .render(header_row, buf);

        let list = rows.list;
        self.viewport_height = list.height.max(1) as usize;

        if favorites.is_empty() {
            let message = if self.current_source() == Some(SourceId::Wy) {
                // 用计数判断而不是克隆整份缓存：这是每帧都会走的渲染路径。
                if crate::remote_cache::summary_counts().0 == 0 {
                    "暂无网易云收藏。右键打开菜单：登录 / 重新登录网易云，或刷新网易云收藏。"
                } else {
                    "网易云红心歌曲为空。右键刷新网易云收藏。"
                }
            } else {
                "暂无收藏，播放时按 Ctrl+L 或列表中按 f 添加"
            };
            Paragraph::new(message)
                .style(Style::new().fg(crate::theme::muted(ctx)))
                .render(list, buf);
            self.render_remote_menu(buf, ctx);
            return;
        }
        if filtered.is_empty() {
            Paragraph::new(format!("没有匹配“{}”的歌曲", self.filter.query()))
                .style(Style::new().fg(crate::theme::overlay1(ctx)))
                .render(list, buf);
            return;
        }
        if list.height == 0 {
            return;
        }

        if self.selected >= self.scroll + self.viewport_height {
            self.scroll = self.selected.saturating_sub(self.viewport_height - 1);
        } else if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        self.scroll = self
            .scroll
            .min(filtered.len().saturating_sub(self.viewport_height));

        for (row, filtered_index) in
            (self.scroll..filtered.len().min(self.scroll + self.viewport_height)).enumerate()
        {
            let Some(song) = favorites.get(filtered[filtered_index]) else {
                continue;
            };
            let style = if filtered_index == self.selected {
                Style::new()
                    .bg(crate::theme::accent(ctx))
                    .fg(crate::theme::selection_fg(ctx))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(crate::theme::text(ctx))
            };
            song_table::row_paragraph(
                song,
                filtered_index,
                list.width,
                &self.columns,
                song_table::TablePalette::from_theme(ctx),
            )
            .style(style)
            .render(Rect::new(list.x, list.y + row as u16, list.width, 1), buf);
        }
        self.render_source_selector(area, buf, ctx);
        self.render_remote_menu(buf, ctx);
    }

    fn render_source_tabs(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        if let Some(selector) = self.source_selector.as_ref() {
            selector.render_tabs(area, buf, ctx);
        }
    }

    /// 歌曲表头所在的一行（供 main.rs 判定"表头右键 → 列菜单"）。
    pub fn table_header_rect(&self, area: Rect, _ctx: &AppContext) -> Option<Rect> {
        PanelRows::new(
            area,
            self.filter.is_active() || !self.filter.query().is_empty(),
            true,
            true,
        )
        .header
    }

    /// 自动列宽的测量样本，**无副作用**（直接读收藏库，不动排序缓存与过滤）。
    pub fn autofit_samples(&self, ctx: &AppContext) -> Vec<SongInfo> {
        ctx.storage.load_favorites()
    }

    /// 兜底取消进行中的列宽拖拽。
    pub fn abort_drag_sessions(&mut self) {
        self.column_resize = None;
    }

    pub fn handle_mouse(
        &mut self,
        event: MouseEvent,
        area: Rect,
        ctx: &AppContext,
        cache: &mut SortedListCache,
        activate: bool,
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
            match result {
                Some(SourceSelectorKey::All) => self.select_source(0),
                Some(SourceSelectorKey::Source(source)) => {
                    if let Some(index) = self
                        .sources
                        .iter()
                        .position(|candidate| *candidate == source)
                    {
                        self.select_source(index + 1);
                    }
                }
                _ => {}
            }
            return AppAction::None;
        }
        // 与渲染共用同一份行账本，避免"渲染画了三行、命中只算两行"。
        let rows = PanelRows::new(
            area,
            self.filter.is_active() || !self.filter.query().is_empty(),
            true,
            true,
        );
        let inner = rows.inner;
        match song_table::handle_column_resize(
            &mut self.column_resize,
            &mut self.columns,
            event,
            rows.header,
            inner,
        ) {
            song_table::ColumnResizeOutcome::Updated => return AppAction::None,
            song_table::ColumnResizeOutcome::Finished => {
                return AppAction::CommitColumnResize {
                    page_key: "favorites".to_string(),
                    columns: self.columns.clone(),
                };
            }
            song_table::ColumnResizeOutcome::NotHandled => {}
        }

        if matches!(event.kind, MouseEventKind::Down(MouseButton::Left))
            && let Some(tab_area) = rows.toolbar_row()
            && let Some(selector) = self.source_selector.as_ref()
            && let Some(key) = selector.tab_at(tab_area, (event.column, event.row).into())
        {
            match key {
                SourceSelectorKey::All => self.select_source(0),
                SourceSelectorKey::Source(source) => {
                    if let Some(index) = self
                        .sources
                        .iter()
                        .position(|candidate| *candidate == source)
                    {
                        self.select_source(index + 1);
                    }
                }
                _ => {}
            }
            return AppAction::None;
        }
        let favorites = self.sorted_favorites(ctx, cache);
        let filtered = self.filtered_song_indices(favorites);
        let scroll_amount = ctx
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .ui
            .scroll_amount
            .max(1);
        let position = Position::new(event.column, event.row);
        match event.kind {
            MouseEventKind::ScrollUp if rows.list.contains(position) => {
                self.selected = self.selected.saturating_sub(scroll_amount);
            }
            MouseEventKind::ScrollDown if rows.list.contains(position) => {
                self.selected =
                    (self.selected + scroll_amount).min(filtered.len().saturating_sub(1));
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if rows.filter_row().is_some_and(|row| row.y == event.row) {
                    self.filter.activate();
                    return AppAction::None;
                }
                if let Some(selected) = rows.index_at(position, self.scroll, filtered.len()) {
                    self.selected = selected;
                    if activate {
                        let songs = filtered
                            .iter()
                            .filter_map(|index| favorites.get(*index).cloned())
                            .collect();
                        return AppAction::PlaySong {
                            songs,
                            index: selected,
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
        ctx: &AppContext,
        cache: &mut SortedListCache,
    ) -> Option<(Vec<SongInfo>, usize)> {
        let favorites = self.sorted_favorites(ctx, cache);
        let filtered = self.filtered_song_indices(favorites);
        // 与渲染共用行账本：过滤行 → 音源条 → 表头 → 列表。
        let rows = PanelRows::new(
            area,
            self.filter.is_active() || !self.filter.query().is_empty(),
            true,
            true,
        );
        let index = source.resolve_index(
            |event| {
                rows.index_at(
                    Position::new(event.column, event.row),
                    self.scroll,
                    filtered.len(),
                )
            },
            self.selected,
            filtered.len(),
        )?;
        let original_index = filtered.get(index).copied()?;
        // 菜单仍按过滤视图返回 (songs, index)；但右键会退出过滤，列表回到
        // 完整视图，selected 必须映射回原始下标，否则会指向另一首歌。
        let songs = filtered
            .iter()
            .filter_map(|original| favorites.get(*original).cloned())
            .collect::<Vec<_>>();
        // deactivate() 只退出输入模式、保留 query，这里要真正关闭过滤
        self.filter.reset();
        self.selected = original_index;
        // 同步调整 scroll，让该行在完整视图中可见（与 render 的滚动逻辑一致）
        if self.selected >= self.scroll + self.viewport_height {
            self.scroll = self.selected.saturating_sub(self.viewport_height - 1);
        } else if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        self.scroll = self
            .scroll
            .min(favorites.len().saturating_sub(self.viewport_height));
        Some((songs, index))
    }

    fn filtered_song_indices(&self, favorites: &[SongInfo]) -> Vec<usize> {
        let query = self.filter.query().trim().to_lowercase();
        // favorites 已经是按当前排序方式排好序的缓存结果，这里只做过滤，
        // 不再重复排序。
        (0..favorites.len())
            .filter(|index| {
                let song = &favorites[*index];
                self.current_source()
                    .is_none_or(|source| song.source == source)
                    && (query.is_empty()
                        || song.name.to_lowercase().contains(&query)
                        || song.singer.to_lowercase().contains(&query)
                        || song.album_name.to_lowercase().contains(&query)
                        || song.source.as_str().contains(&query))
            })
            .collect()
    }

    /// 返回按当前排序方式排列的收藏列表（来自页面缓存，命中时零拷贝）。
    fn sorted_favorites<'a>(
        &self,
        ctx: &AppContext,
        cache: &'a mut SortedListCache,
    ) -> &'a [SongInfo] {
        let mode = self.sort_mode;
        if self.current_source() == Some(SourceId::Wy) {
            // 网易云收藏不是本地收藏库：直接把远程缓存作为收藏页数据源。
            let version = crate::remote_cache::generation();
            return cache.get_or_build(version, mode, SortTarget::Favorites, || {
                crate::remote_cache::favorites_songs()
            });
        }
        let version = ctx.storage.generation();
        cache.get_or_build(version, mode, SortTarget::Favorites, || {
            ctx.storage.load_favorites()
        })
    }

    fn current_source(&self) -> Option<SourceId> {
        self.source_index
            .checked_sub(1)
            .and_then(|index| self.sources.get(index).copied())
    }

    fn open_source_selector(&mut self) {
        if let Some(selector) = self.source_selector.as_mut() {
            selector.select(self.source_index);
            selector.open();
        }
    }

    fn select_source(&mut self, index: usize) {
        if index > self.sources.len() {
            return;
        }
        self.source_index = index;
        if let Some(selector) = self.source_selector.as_mut() {
            selector.select(index);
        }
        self.selected = 0;
        self.scroll = 0;
    }

    fn handle_source_selector(&mut self, key: &KeyEvent) {
        let result = self
            .source_selector
            .as_mut()
            .and_then(|selector| selector.handle_key(*key));
        match result {
            Some(SourceSelectorKey::All) => self.select_source(0),
            Some(SourceSelectorKey::Source(source)) => {
                if let Some(index) = self
                    .sources
                    .iter()
                    .position(|candidate| *candidate == source)
                {
                    self.select_source(index + 1);
                }
            }
            _ => {}
        }
    }

    fn render_source_selector(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        if let Some(selector) = self.source_selector.as_mut() {
            selector.render_popup(area, buf, ctx, "选择音源");
        }
    }

    fn clamp_selection(&mut self, len: usize) {
        if len == 0 {
            self.selected = 0;
            self.scroll = 0;
        } else {
            self.selected = self.selected.min(len - 1);
            self.scroll = self.scroll.min(self.selected);
        }
    }

    fn log_keyboard_playback(&self, songs: &[SongInfo]) {
        let selected = songs.get(self.selected);
        let order = songs
            .iter()
            .take(5)
            .map(|song| format!("{}:{}", song.id, song.name))
            .collect::<Vec<_>>();
        tracing::debug!(
            sort = ?self.sort_mode,
            selected_index = self.selected,
            selected_id = selected.map(|song| song.id.as_str()),
            selected_name = selected.map(|song| song.name.as_str()),
            ?order,
            "favorites keyboard playback"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::FavoritesPage;
    use crate::pages::sort::{SortMode, SortTarget, sorted_songs};
    use lx_core::events::AppAction;
    use lx_core::model::song::SongInfo;
    use lx_core::model::source::SourceId;
    use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::layout::{Position, Rect};

    fn click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// 右键菜单的行命中必须和渲染行一致：曾经点「登录」会触发「刷新」，
    /// 而「刷新」点了没反应（第一项被当成 0，实际从 origin.y+1 才开始）。
    #[test]
    fn remote_menu_maps_each_row_to_its_own_action() {
        let mut page = FavoritesPage::new(vec![SourceId::Wy, SourceId::Kw]);
        page.last_area = Rect::new(0, 0, 80, 24);
        page.open_remote_menu(Position::new(10, 5));

        // 第 1 行（origin.y + 1）= 登录
        let action = page.handle_remote_menu_mouse(click(12, 6));
        assert!(
            matches!(action, Some(AppAction::QrLogin(..))),
            "第 1 行应当是登录, 实际 {action:?}"
        );
        assert!(!page.remote_menu_open(), "点中条目后菜单应关闭");

        // 第 2 行 = 刷新远程歌单
        page.open_remote_menu(Position::new(10, 5));
        let action = page.handle_remote_menu_mouse(click(12, 7));
        assert!(
            matches!(action, Some(AppAction::SyncNetease)),
            "第 2 行应当是刷新, 实际 {action:?}"
        );
        assert!(!page.remote_menu_open());

        // 第 3 行 = 关闭：只关菜单，不派发动作
        page.open_remote_menu(Position::new(10, 5));
        let action = page.handle_remote_menu_mouse(click(12, 8));
        assert!(action.is_none());
        assert!(!page.remote_menu_open());

        // 边框行（origin.y）不应被当成任何条目
        page.open_remote_menu(Position::new(10, 5));
        let action = page.handle_remote_menu_mouse(click(12, 5));
        assert!(action.is_none());
        assert!(!page.remote_menu_open());
    }

    #[test]
    fn remote_menu_is_clamped_into_the_page_and_still_hits_correctly() {
        let mut page = FavoritesPage::new(vec![SourceId::Wy]);
        page.last_area = Rect::new(0, 0, 80, 24);
        // 在右下角右键：菜单必须被内缩进页面，点击命中的仍是同样的行。
        page.open_remote_menu(Position::new(79, 23));
        let (origin, _) = page.remote_menu.expect("menu open");
        assert!(
            origin.y + super::REMOTE_MENU_HEIGHT <= 24,
            "菜单不能被放到页面外: {origin:?}"
        );
        assert!(origin.x + super::REMOTE_MENU_WIDTH <= 80, "{origin:?}");

        // 按内缩后的坐标点第 2 行，仍应拿到 SyncNetease。
        let action = page.handle_remote_menu_mouse(click(origin.x + 1, origin.y + 2));
        assert!(matches!(action, Some(AppAction::SyncNetease)));
    }

    #[test]
    fn filters_title_artist_album_and_source() {
        let mut page = FavoritesPage::new(vec![SourceId::Kw, SourceId::Kg]);
        let mut song = SongInfo::new("1".into(), SourceId::Kw, "晴天".into(), "周杰伦".into());
        song.album_name = "叶惠美".into();
        let songs = vec![song];

        for query in ["晴天", "周杰伦", "叶惠美", "kw"] {
            page.filter.set_query(query);
            assert_eq!(page.filtered_song_indices(&songs), vec![0]);
        }
    }

    #[test]
    fn defaults_to_most_recent_favorite_and_cycles_sorting() {
        let mut page = FavoritesPage::new(vec![SourceId::Kw, SourceId::Kg]);
        let songs = vec![
            SongInfo::new("1".into(), SourceId::Kw, "A".into(), "X".into()),
            SongInfo::new("2".into(), SourceId::Kw, "B".into(), "Y".into()),
        ];

        // filtered_song_indices 只负责过滤；排序由 SortedListCache 提供，
        // 输入按当前排序方式排好序时顺序保持不变。
        assert_eq!(page.filtered_song_indices(&songs), vec![0, 1]);
        assert_eq!(page.cycle_sort(), SortMode::Oldest);
        assert_eq!(page.filtered_song_indices(&songs), vec![0, 1]);
    }

    #[test]
    fn source_navigation_includes_all_and_supports_left_right() {
        let mut page = FavoritesPage::new(vec![SourceId::Kw, SourceId::Kg]);
        assert_eq!(page.source_index, 0);
        page.select_source(1);
        assert_eq!(page.current_source(), Some(SourceId::Kw));
        page.select_source(2);
        assert_eq!(page.current_source(), Some(SourceId::Kg));
        page.select_source(0);
        assert_eq!(page.current_source(), None);
    }

    #[test]
    fn keyboard_selection_indices_follow_the_sorted_favorites() {
        let page = FavoritesPage::new(vec![SourceId::Kw, SourceId::Kg]);
        let stored = vec![
            SongInfo::new("1".into(), SourceId::Kw, "C".into(), "X".into()),
            SongInfo::new("2".into(), SourceId::Kw, "B".into(), "Y".into()),
            SongInfo::new("3".into(), SourceId::Kw, "A".into(), "Z".into()),
        ];
        let sorted = sorted_songs(stored, SortMode::TitleAsc, SortTarget::Favorites);
        let filtered = page.filtered_song_indices(&sorted);
        let songs = filtered
            .iter()
            .filter_map(|index| sorted.get(*index).cloned())
            .collect::<Vec<_>>();

        assert_eq!(songs[0].name, "A");
        assert_eq!(songs[2].name, "C");
    }
}
