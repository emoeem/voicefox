//! 搜索页面

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lx_core::events::AppAction;
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::SearchResult;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};

use crate::context::AppContext;

const SEARCH_SCOPES: &[(Option<SourceId>, &str)] = &[
    (None, "全部"),
    (Some(SourceId::Kw), "酷我 kw"),
    (Some(SourceId::Kg), "酷狗 kg"),
    (Some(SourceId::Tx), "QQ tx"),
    (Some(SourceId::Mg), "咪咕 mg"),
    (Some(SourceId::Wy), "网易 wy"),
];

pub struct SearchPage {
    pub input: String,
    pub results: Vec<SongInfo>,
    pub selected: usize,
    pub scroll_offset: usize,
    pub last_input_time: std::time::Instant,
    pub last_searched_input: String,
    pub is_searching: bool,
    pub result_keyword: String,
    pub total: u32,
    pub has_more: bool,
    pub error_message: Option<String>,
    pub current_page: u32,
    pub input_mode: bool,
    pub source_filter: Option<SourceId>,
    pub result_source_filter: Option<SourceId>,
    pub variant_indices: Vec<usize>,
    pub variant_selected: usize,
    wrap_navigation: bool,
    scroll_amount: usize,
}

impl SearchPage {
    pub fn new(
        source_filter: Option<SourceId>,
        wrap_navigation: bool,
        scroll_amount: usize,
    ) -> Self {
        Self {
            input: String::new(),
            results: vec![],
            selected: 0,
            scroll_offset: 0,
            last_input_time: std::time::Instant::now(),
            last_searched_input: String::new(),
            is_searching: false,
            result_keyword: String::new(),
            total: 0,
            has_more: false,
            error_message: None,
            current_page: 0,
            input_mode: true,
            source_filter,
            result_source_filter: None,
            variant_indices: Vec::new(),
            variant_selected: 0,
            wrap_navigation,
            scroll_amount: scroll_amount.max(1),
        }
    }

    pub fn handle_input(&mut self, key: KeyEvent) -> AppAction {
        if !self.variant_indices.is_empty() {
            return self.handle_variant_input(key);
        }

        if self.input_mode {
            match (key.modifiers, key.code) {
                (KeyModifiers::NONE, KeyCode::Esc) => {
                    self.input_mode = false;
                }
                (KeyModifiers::NONE, KeyCode::Enter) => {
                    let keyword = self.input.trim().to_string();
                    if !(keyword.is_empty()
                        || self.is_searching && self.last_searched_input == keyword)
                    {
                        self.last_input_time = std::time::Instant::now();
                        self.last_searched_input = keyword.clone();
                        return AppAction::Search {
                            keyword,
                            source: self.source_filter,
                        };
                    }
                }
                (_, KeyCode::Char(c))
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.input.push(c);
                    self.last_input_time = std::time::Instant::now();
                    self.error_message = None;
                }
                (_, KeyCode::Backspace)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.input.pop();
                    self.last_input_time = std::time::Instant::now();
                    self.error_message = None;
                    if self.input.trim().is_empty() {
                        self.results.clear();
                        self.result_keyword.clear();
                        self.total = 0;
                        self.has_more = false;
                        self.current_page = 0;
                        self.selected = 0;
                        self.scroll_offset = 0;
                    }
                }
                _ => {}
            }
            return AppAction::None;
        }

        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc) => {
                return AppAction::GoBack;
            }
            (KeyModifiers::NONE, KeyCode::Char('i')) | (KeyModifiers::NONE, KeyCode::Char('/')) => {
                self.input_mode = true;
            }
            (KeyModifiers::NONE, KeyCode::Char('v')) => {
                self.open_variants();
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                let keyword = self.input.trim().to_string();
                if keyword.is_empty() {
                    return AppAction::None;
                }
                if self.is_searching && self.last_searched_input == keyword {
                    return AppAction::None;
                }
                if self.result_keyword == keyword && !self.results.is_empty() {
                    // 选中歌曲 → 播放
                    let songs = self.results.clone();
                    let index = self.selected;
                    return AppAction::PlaySong { songs, index };
                }
                self.last_input_time = std::time::Instant::now();
                self.last_searched_input = keyword.clone();
                return AppAction::Search {
                    keyword,
                    source: self.source_filter,
                };
            }
            (KeyModifiers::NONE, KeyCode::Up) | (KeyModifiers::NONE, KeyCode::Char('k')) => {
                if !self.results.is_empty() {
                    if self.selected > 0 {
                        self.selected -= 1;
                    } else if self.wrap_navigation {
                        self.selected = self.results.len().saturating_sub(1);
                    }
                }
            }
            (KeyModifiers::NONE, KeyCode::Down) | (KeyModifiers::NONE, KeyCode::Char('j')) => {
                if !self.results.is_empty() {
                    if self.selected + 1 < self.results.len() {
                        self.selected += 1;
                    } else if self.can_load_more() {
                        return AppAction::SearchMore {
                            keyword: self.result_keyword.clone(),
                            page: self.current_page + 1,
                            source: self.source_filter,
                        };
                    } else if self.wrap_navigation {
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
                self.selected = self.results.len().saturating_sub(1);
            }
            (KeyModifiers::CONTROL, KeyCode::Char('u')) | (KeyModifiers::NONE, KeyCode::PageUp)
                if !self.results.is_empty() =>
            {
                self.selected = self.selected.saturating_sub(10);
            }
            (KeyModifiers::CONTROL, KeyCode::Char('d'))
            | (KeyModifiers::NONE, KeyCode::PageDown)
                if !self.results.is_empty() =>
            {
                self.selected = (self.selected + 10).min(self.results.len().saturating_sub(1));
                if self.selected + 1 == self.results.len() && self.can_load_more() {
                    return AppAction::SearchMore {
                        keyword: self.result_keyword.clone(),
                        page: self.current_page + 1,
                        source: self.source_filter,
                    };
                }
            }
            (KeyModifiers::NONE, KeyCode::Left) => {
                return self.cycle_source(-1);
            }
            (KeyModifiers::NONE, KeyCode::Right) => {
                return self.cycle_source(1);
            }
            _ => {}
        }

        AppAction::None
    }

    fn can_load_more(&self) -> bool {
        self.has_more
            && !self.is_searching
            && self.input.trim() == self.result_keyword
            && self.source_filter == self.result_source_filter
            && self.current_page > 0
    }

    fn cycle_source(&mut self, direction: isize) -> AppAction {
        let current = SEARCH_SCOPES
            .iter()
            .position(|(scope, _)| *scope == self.source_filter)
            .unwrap_or(0);
        let next = (current as isize + direction).rem_euclid(SEARCH_SCOPES.len() as isize) as usize;
        self.select_source(next)
    }

    fn select_source(&mut self, index: usize) -> AppAction {
        let Some((source, _)) = SEARCH_SCOPES.get(index).copied() else {
            return AppAction::None;
        };
        if self.source_filter == source && self.result_source_filter == source {
            return AppAction::None;
        }
        self.source_filter = source;
        self.error_message = None;
        self.close_variants();

        let keyword = self.input.trim().to_string();
        if keyword.is_empty() {
            AppAction::None
        } else {
            self.last_searched_input = keyword.clone();
            AppAction::Search {
                keyword,
                source: self.source_filter,
            }
        }
    }

    pub fn set_preferences(
        &mut self,
        aggregate_search: bool,
        default_source: SourceId,
        wrap_navigation: bool,
        scroll_amount: usize,
    ) {
        self.wrap_navigation = wrap_navigation;
        self.scroll_amount = scroll_amount.max(1);
        self.source_filter = if aggregate_search {
            None
        } else {
            Some(default_source)
        };
    }

    /// 搜索防抖 tick：用户停止输入 300ms 后自动触发搜索
    pub fn tick(&mut self) -> Option<AppAction> {
        let keyword = self.input.trim();
        if keyword.is_empty() {
            return None;
        }
        if self.last_input_time.elapsed() > std::time::Duration::from_millis(300)
            && keyword != self.last_searched_input
        {
            let keyword = keyword.to_string();
            self.last_searched_input = keyword.clone();
            Some(AppAction::Search {
                keyword,
                source: self.source_filter,
            })
        } else {
            None
        }
    }

    /// 接收异步搜索结果
    pub fn begin_search(&mut self, keyword: &str, append: bool) {
        self.is_searching = true;
        self.last_searched_input = keyword.to_string();
        self.error_message = None;
        self.close_variants();
        if !append {
            self.current_page = 0;
        }
    }

    /// 接收异步搜索结果
    pub fn update_results(
        &mut self,
        keyword: String,
        page: u32,
        append: bool,
        result: SearchResult,
        source_filter: Option<SourceId>,
    ) {
        if append {
            for song in result.items {
                if !self
                    .results
                    .iter()
                    .any(|item| item.id == song.id && item.source == song.source)
                {
                    self.results.push(song);
                }
            }
        } else {
            self.results = result.items;
            self.selected = 0;
            self.scroll_offset = 0;
        }
        self.is_searching = false;
        self.result_keyword = keyword;
        self.result_source_filter = source_filter;
        self.current_page = page;
        self.total = result.total;
        self.has_more = result.has_more;
        self.error_message = None;
    }

    /// 接收异步搜索错误
    pub fn update_error(&mut self, message: String) {
        self.is_searching = false;
        self.error_message = Some(message);
        self.close_variants();
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        let accent = crate::theme::accent(ctx);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(area);

        // 搜索输入区
        let scope = self
            .source_filter
            .map(|source| source.as_str().to_string())
            .unwrap_or_else(|| "全部音源".to_string());
        let mode = if self.input_mode { "INSERT" } else { "NORMAL" };
        let input_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(if self.input_mode {
                crate::theme::green(ctx)
            } else {
                accent
            }))
            .title(format!("搜索 · {} · {}", scope, mode));

        let cursor = if self.input_mode
            && (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
                / 500)
                .is_multiple_of(2)
        {
            "█"
        } else {
            ""
        };

        let input_line = Line::from(vec![
            Span::styled(" / ", Style::new().fg(accent)),
            Span::raw(&self.input),
            Span::styled(cursor, Style::new().fg(accent)),
        ]);

        Paragraph::new(input_line)
            .block(input_block)
            .render(chunks[0], buf);

        self.render_source_tabs(chunks[1], buf, ctx);

        // 搜索结果区
        let result_title = if self.is_searching && self.results.is_empty() {
            "搜索中".to_string()
        } else if let Some(error) = &self.error_message {
            format!("搜索失败 - {}", error)
        } else {
            let loading_more = if self.is_searching {
                " · 正在加载更多"
            } else if self.has_more {
                " · 还有更多"
            } else {
                ""
            };
            format!(
                "搜索结果 {}/{}{} · v 选择音源",
                self.results.len(),
                self.total,
                loading_more
            )
        };
        let result_block = Block::default().borders(Borders::ALL).title(result_title);
        let result_block = result_block.border_style(Style::new().fg(crate::theme::border(ctx)));

        let inner_area = result_block.inner(chunks[2]);
        result_block.render(chunks[2], buf);

        if self.results.is_empty() {
            let message = self
                .error_message
                .as_deref()
                .unwrap_or("输入关键词开始搜索");
            Paragraph::new(message)
                .style(Style::new().fg(if self.error_message.is_some() {
                    crate::theme::red(ctx)
                } else {
                    crate::theme::muted(ctx)
                }))
                .render(inner_area, buf);
            return;
        }

        if inner_area.height == 0 {
            return;
        }

        let header_area = Rect::new(inner_area.x, inner_area.y, inner_area.width, 1);
        Paragraph::new(Line::from(Span::styled(
            super::components::song_table::header(inner_area.width),
            Style::new()
                .fg(crate::theme::muted(ctx))
                .add_modifier(Modifier::BOLD),
        )))
        .render(header_area, buf);
        let list_area = Rect::new(
            inner_area.x,
            inner_area.y.saturating_add(1),
            inner_area.width,
            inner_area.height.saturating_sub(1),
        );

        let selected_style = Style::new().bg(accent).fg(crate::theme::selection_fg(ctx));
        let normal_style = Style::new().fg(crate::theme::text(ctx));

        let visible_height = list_area.height as usize;
        if visible_height == 0 {
            return;
        }
        let total = self.results.len();

        // 自动调整 scroll
        if self.selected >= self.scroll_offset + visible_height {
            self.scroll_offset = self.selected.saturating_sub(visible_height - 1);
        } else if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        }
        self.scroll_offset = self.scroll_offset.min(total.saturating_sub(visible_height));

        let end = (self.scroll_offset + visible_height).min(total);
        for i in self.scroll_offset..end {
            let row = i - self.scroll_offset;
            if row as u16 >= list_area.height {
                break;
            }

            let song = &self.results[i];
            let text = super::components::song_table::row(song, i, list_area.width);

            let line_area = Rect::new(list_area.x, list_area.y + row as u16, list_area.width, 1);
            let style = if i == self.selected {
                selected_style
            } else {
                normal_style
            };

            Paragraph::new(Line::from(Span::styled(text, style))).render(line_area, buf);
        }

        self.render_variant_picker(area, buf, ctx);
    }

    fn render_source_tabs(&self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let selected = SEARCH_SCOPES
            .iter()
            .position(|(scope, _)| *scope == self.source_filter)
            .unwrap_or(0);
        for (index, tab_area) in source_tab_areas(area).iter().copied().enumerate() {
            let label = if area.width >= 66 {
                SEARCH_SCOPES[index].1
            } else {
                SEARCH_SCOPES[index]
                    .0
                    .map(|source| source.as_str())
                    .unwrap_or("all")
            };
            let style = if index == selected {
                Style::new()
                    .fg(crate::theme::selection_fg(ctx))
                    .bg(crate::theme::accent(ctx))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(crate::theme::muted(ctx))
            };
            Paragraph::new(Line::from(Span::styled(label, style)))
                .alignment(ratatui::layout::Alignment::Center)
                .style(style)
                .render(tab_area, buf);
        }
    }

    pub fn handle_mouse(&mut self, event: MouseEvent, area: Rect, activate: bool) -> AppAction {
        if !self.variant_indices.is_empty() {
            return self.handle_variant_mouse(event, area, activate);
        }

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(area);
        if chunks[0].contains((event.column, event.row).into())
            && matches!(event.kind, MouseEventKind::Down(MouseButton::Left))
        {
            self.input_mode = true;
            return AppAction::None;
        }
        if chunks[1].contains((event.column, event.row).into())
            && matches!(event.kind, MouseEventKind::Down(MouseButton::Left))
            && let Some(index) = source_tab_areas(chunks[1])
                .iter()
                .position(|tab| tab.contains((event.column, event.row).into()))
        {
            self.input_mode = false;
            return self.select_source(index);
        }

        match event.kind {
            MouseEventKind::ScrollUp => {
                self.selected = self.selected.saturating_sub(self.scroll_amount);
            }
            MouseEventKind::ScrollDown => {
                self.selected =
                    (self.selected + self.scroll_amount).min(self.results.len().saturating_sub(1));
                if self.selected + 1 == self.results.len() && self.can_load_more() {
                    return AppAction::SearchMore {
                        keyword: self.result_keyword.clone(),
                        page: self.current_page + 1,
                        source: self.source_filter,
                    };
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let inner = Block::default().borders(Borders::ALL).inner(chunks[2]);
                let list_y = inner.y.saturating_add(1);
                if event.row >= list_y && event.row < inner.bottom() {
                    let index = self.scroll_offset + event.row.saturating_sub(list_y) as usize;
                    if index < self.results.len() {
                        self.input_mode = false;
                        self.selected = index;
                        if activate {
                            return AppAction::PlaySong {
                                songs: self.results.clone(),
                                index,
                            };
                        }
                    }
                }
            }
            _ => {}
        }
        AppAction::None
    }

    fn open_variants(&mut self) {
        self.variant_indices = matching_variant_indices(&self.results, self.selected);
        self.variant_selected = self
            .variant_indices
            .iter()
            .position(|index| *index == self.selected)
            .unwrap_or(0);
    }

    fn close_variants(&mut self) {
        self.variant_indices.clear();
        self.variant_selected = 0;
    }

    fn handle_variant_input(&mut self, key: KeyEvent) -> AppAction {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc) | (KeyModifiers::NONE, KeyCode::Char('v')) => {
                self.close_variants();
            }
            (KeyModifiers::NONE, KeyCode::Up)
            | (KeyModifiers::NONE, KeyCode::Left)
            | (KeyModifiers::NONE, KeyCode::Char('k')) => {
                if self.variant_selected > 0 {
                    self.variant_selected -= 1;
                } else if self.wrap_navigation {
                    self.variant_selected = self.variant_indices.len().saturating_sub(1);
                }
            }
            (KeyModifiers::NONE, KeyCode::Down)
            | (KeyModifiers::NONE, KeyCode::Right)
            | (KeyModifiers::NONE, KeyCode::Char('j')) => {
                if self.variant_selected + 1 < self.variant_indices.len() {
                    self.variant_selected += 1;
                } else if self.wrap_navigation {
                    self.variant_selected = 0;
                }
            }
            (KeyModifiers::NONE, KeyCode::Home) | (KeyModifiers::NONE, KeyCode::Char('g')) => {
                self.variant_selected = 0;
            }
            (KeyModifiers::NONE, KeyCode::End)
            | (KeyModifiers::NONE, KeyCode::Char('G'))
            | (KeyModifiers::SHIFT, KeyCode::Char('G')) => {
                self.variant_selected = self.variant_indices.len().saturating_sub(1);
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                if let Some(index) = self.variant_indices.get(self.variant_selected).copied() {
                    self.selected = index;
                    self.close_variants();
                    return AppAction::PlaySong {
                        songs: self.results.clone(),
                        index,
                    };
                }
            }
            _ => {}
        }
        AppAction::None
    }

    fn handle_variant_mouse(&mut self, event: MouseEvent, area: Rect, activate: bool) -> AppAction {
        match event.kind {
            MouseEventKind::ScrollUp => {
                self.variant_selected = self.variant_selected.saturating_sub(1);
            }
            MouseEventKind::ScrollDown => {
                self.variant_selected =
                    (self.variant_selected + 1).min(self.variant_indices.len().saturating_sub(1));
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let popup = variant_popup(area, self.variant_indices.len());
                if !popup.contains((event.column, event.row).into()) {
                    self.close_variants();
                    return AppAction::None;
                }
                let inner = Block::default().borders(Borders::ALL).inner(popup);
                let list_y = inner.y.saturating_add(1);
                if event.row >= list_y && event.row < inner.bottom() {
                    let index = event.row.saturating_sub(list_y) as usize;
                    if index < self.variant_indices.len() {
                        self.variant_selected = index;
                        if activate {
                            return self.handle_variant_input(KeyEvent::new(
                                KeyCode::Enter,
                                KeyModifiers::NONE,
                            ));
                        }
                    }
                }
            }
            _ => {}
        }
        AppAction::None
    }

    fn render_variant_picker(&self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        if self.variant_indices.is_empty() {
            return;
        }
        let popup = variant_popup(area, self.variant_indices.len());
        if popup.width == 0 || popup.height == 0 {
            return;
        }
        Clear.render(popup, buf);
        let title = self
            .results
            .get(self.selected)
            .map(|song| format!(" 选择音源 · {} ", song.name))
            .unwrap_or_else(|| " 选择音源 ".to_string());
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(crate::theme::accent(ctx)))
            .title(title);
        let inner = block.inner(popup);
        block.render(popup, buf);
        if inner.height == 0 {
            return;
        }

        Paragraph::new(Line::from(Span::styled(
            super::components::song_table::header(inner.width),
            Style::new()
                .fg(crate::theme::muted(ctx))
                .add_modifier(Modifier::BOLD),
        )))
        .render(Rect::new(inner.x, inner.y, inner.width, 1), buf);

        for (row, result_index) in self
            .variant_indices
            .iter()
            .copied()
            .take(inner.height.saturating_sub(1) as usize)
            .enumerate()
        {
            let Some(song) = self.results.get(result_index) else {
                continue;
            };
            let style = if row == self.variant_selected {
                Style::new()
                    .fg(crate::theme::selection_fg(ctx))
                    .bg(crate::theme::accent(ctx))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(crate::theme::text(ctx))
            };
            Paragraph::new(Line::from(Span::styled(
                super::components::song_table::row(song, result_index, inner.width),
                style,
            )))
            .render(
                Rect::new(inner.x, inner.y + 1 + row as u16, inner.width, 1),
                buf,
            );
        }
    }
}

fn source_tab_areas(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(std::iter::repeat_n(
            Constraint::Ratio(1, SEARCH_SCOPES.len() as u32),
            SEARCH_SCOPES.len(),
        ))
        .split(area)
}

fn variant_popup(area: Rect, count: usize) -> Rect {
    let width = area.width.saturating_sub(4).min(92);
    let height = (count as u16 + 3)
        .min(area.height.saturating_sub(2))
        .max(3.min(area.height));
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn matching_variant_indices(results: &[SongInfo], selected: usize) -> Vec<usize> {
    let Some(target) = results.get(selected) else {
        return Vec::new();
    };
    let mut seen_sources = std::collections::HashSet::new();
    results
        .iter()
        .enumerate()
        .filter(|(_, song)| same_track(target, song))
        .filter_map(|(index, song)| seen_sources.insert(song.source).then_some(index))
        .collect()
}

fn same_track(left: &SongInfo, right: &SongInfo) -> bool {
    let left_name = normalize(&left.name);
    let right_name = normalize(&right.name);
    if left_name.is_empty() || left_name != right_name {
        return false;
    }

    let left_singer = normalize(&left.singer);
    let right_singer = normalize(&right.singer);
    let singer_matches = left_singer.is_empty()
        || right_singer.is_empty()
        || left_singer == right_singer
        || left_singer.contains(&right_singer)
        || right_singer.contains(&left_singer);
    if !singer_matches {
        return false;
    }

    let left_duration = left.duration.as_secs() as i64;
    let right_duration = right.duration.as_secs() as i64;
    left_duration == 0 || right_duration == 0 || (left_duration - right_duration).abs() <= 8
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{SearchPage, matching_variant_indices};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use lx_core::events::AppAction;
    use lx_core::model::song::SongInfo;
    use lx_core::model::source::SourceId;

    fn song(id: &str, source: SourceId, name: &str, singer: &str) -> SongInfo {
        SongInfo::new(id.to_string(), source, name.to_string(), singer.to_string())
    }

    #[test]
    fn right_arrow_cycles_search_scope() {
        let mut page = SearchPage::new(None, true, 3);
        page.input_mode = false;

        let action = page.handle_input(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));

        assert_eq!(page.source_filter, Some(SourceId::Kw));
        assert!(matches!(action, AppAction::None));
    }

    #[test]
    fn source_tab_selects_single_source_and_starts_search() {
        let mut page = SearchPage::new(None, true, 3);
        page.input = "晴天".to_string();

        let action = page.select_source(2);

        assert_eq!(page.source_filter, Some(SourceId::Kg));
        assert!(matches!(
            action,
            AppAction::Search {
                source: Some(SourceId::Kg),
                ..
            }
        ));
    }

    #[test]
    fn groups_equivalent_tracks_by_source() {
        let results = vec![
            song("kw-1", SourceId::Kw, "晴天", "周杰伦"),
            song("kg-1", SourceId::Kg, "晴天", "周杰伦"),
            song("tx-1", SourceId::Tx, "晴天 (Live)", "周杰伦"),
            song("kw-2", SourceId::Kw, "晴天", "周杰伦"),
        ];

        assert_eq!(matching_variant_indices(&results, 0), vec![0, 1]);
    }
}
