//! 搜索页面

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lx_core::events::{AppAction, InsertPosition};
use lx_core::keybinding::{Action, KeybindingResolver};
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
    (Some(SourceId::Bili), "哔哩哔哩 bili"),
    (Some(SourceId::Local), "本地 local"),
];

/// Deferred multi-part selection. It owns the original result list so confirming a part can
/// replace only the selected video while preserving the surrounding playback queue.
struct BiliPartPicker {
    video_title: String,
    songs: Vec<SongInfo>,
    replacement_index: usize,
    parts: Vec<SongInfo>,
    selected: usize,
    scroll_offset: usize,
}

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
    artist_filter: Option<String>,
    album_filter: Option<String>,
    base_keyword: String,
    pub variant_indices: Vec<usize>,
    pub variant_selected: usize,
    /// 变体弹窗的滚动偏移：选中项超出可视行数时跟随滚动
    variant_scroll_offset: usize,
    /// 变体弹窗最近一次渲染的可视行数，供键盘导航做可见性调整
    variant_viewport: usize,
    search_scopes: Vec<(Option<SourceId>, &'static str)>,
    part_picker: Option<BiliPartPicker>,
    bili_parts_loading: Option<u64>,
    next_bili_parts_request_id: u64,
    wrap_navigation: bool,
    scroll_amount: usize,
}

impl SearchPage {
    pub fn new(
        source_filter: Option<SourceId>,
        wrap_navigation: bool,
        scroll_amount: usize,
        enabled_sources: &[SourceId],
    ) -> Self {
        let search_scopes = enabled_search_scopes(enabled_sources);
        let source_filter = source_filter.filter(|source| {
            search_scopes
                .iter()
                .any(|(candidate, _)| *candidate == Some(*source))
        });
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
            input_mode: false,
            source_filter,
            result_source_filter: None,
            artist_filter: None,
            album_filter: None,
            base_keyword: String::new(),
            variant_indices: Vec::new(),
            variant_selected: 0,
            variant_scroll_offset: 0,
            variant_viewport: 1,
            search_scopes,
            part_picker: None,
            bili_parts_loading: None,
            next_bili_parts_request_id: 0,
            wrap_navigation,
            scroll_amount: scroll_amount.max(1),
        }
    }

    pub fn handle_input(&mut self, key: KeyEvent, resolver: &KeybindingResolver) -> AppAction {
        if self.part_picker.is_some() {
            return self.handle_bili_part_input(key);
        }
        if self.bili_parts_loading.is_some() {
            if matches!(
                (key.modifiers, key.code),
                (KeyModifiers::NONE, KeyCode::Esc)
            ) {
                self.bili_parts_loading = None;
            }
            return AppAction::None;
        }

        // The variant picker is a modal overlay.  Route every key to it before
        // handling search input or page-level bindings so Esc/v can close the
        // overlay instead of bubbling up as page navigation.
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
                        self.prepare_plain_search(&keyword);
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

        // Brackets are source selectors on the search page.  Keep them out
        // of input mode so users can still type them in a query.
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Char('[')) => return self.cycle_source(-1),
            (KeyModifiers::NONE, KeyCode::Char(']')) => return self.cycle_source(1),
            _ => {}
        }

        if let Some(action) = resolver.resolve("search", &key) {
            match action {
                Action::SearchInputMode => {
                    self.input_mode = true;
                }
                Action::SearchStart => {
                    let keyword = self.input.trim().to_string();
                    if keyword.is_empty() {
                        return AppAction::None;
                    }
                    if self.is_searching && self.last_searched_input == keyword {
                        return AppAction::None;
                    }
                    if self.result_keyword == keyword && !self.results.is_empty() {
                        return self.activate_selected_result();
                    }
                    self.last_input_time = std::time::Instant::now();
                    self.prepare_plain_search(&keyword);
                    self.last_searched_input = keyword.clone();
                    return AppAction::Search {
                        keyword,
                        source: self.source_filter,
                    };
                }
                Action::SearchToggleAggregate => {
                    self.open_variants();
                }
                Action::ListAddToQueue => {
                    if let Some(song) = self.results.get(self.selected).cloned() {
                        return AppAction::AddToQueue {
                            song: Box::new(song),
                            position: InsertPosition::End,
                        };
                    }
                }
                Action::ListAddToQueueNext => {
                    if let Some(song) = self.results.get(self.selected).cloned() {
                        return AppAction::AddToQueue {
                            song: Box::new(song),
                            position: InsertPosition::Next,
                        };
                    }
                }
                Action::ListActivate => {
                    if !self.results.is_empty() {
                        return self.activate_selected_result();
                    }
                }
                Action::ListToggleFavorite => {
                    if let Some(song) = self.results.get(self.selected).cloned() {
                        return AppAction::ToggleFavoriteSong(Box::new(song));
                    }
                }
                Action::ListSelectUp => {
                    if !self.results.is_empty() {
                        if self.selected > 0 {
                            self.selected -= 1;
                        } else if self.wrap_navigation {
                            self.selected = self.results.len().saturating_sub(1);
                        }
                    }
                }
                Action::ListSelectDown => {
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
                Action::ListSelectFirst => {
                    self.selected = 0;
                }
                Action::ListSelectLast => {
                    self.selected = self.results.len().saturating_sub(1);
                }
                Action::ListPageUp => {
                    if !self.results.is_empty() {
                        self.selected = self.selected.saturating_sub(10);
                    }
                }
                Action::ListPageDown => {
                    if !self.results.is_empty() {
                        self.selected =
                            (self.selected + 10).min(self.results.len().saturating_sub(1));
                        if self.selected + 1 == self.results.len() && self.can_load_more() {
                            return AppAction::SearchMore {
                                keyword: self.result_keyword.clone(),
                                page: self.current_page + 1,
                                source: self.source_filter,
                            };
                        }
                    }
                }
                Action::SearchCycleSourcePrev => {
                    return self.cycle_source(-1);
                }
                Action::SearchCycleSourceNext => {
                    return self.cycle_source(1);
                }
                Action::ListDownload => {
                    if let Some(song) = self.results.get(self.selected).cloned() {
                        return AppAction::DownloadSong(Box::new(song));
                    }
                }
                Action::ListGoBack => {
                    // Search has no nested page to leave.  Esc is consumed by
                    // the input/variant overlays above; while idle it is a
                    // no-op so the current tab remains visible.
                }
                _ => {}
            }
            return AppAction::None;
        }

        match (key.modifiers, key.code) {
            // Keep Esc local to search.  Modal states (input, variants and
            // Bilibili parts) have already consumed it above.
            (KeyModifiers::NONE, KeyCode::Esc) => {}
            (KeyModifiers::NONE, KeyCode::Char('i')) | (KeyModifiers::NONE, KeyCode::Char('/')) => {
                self.input_mode = true;
            }
            (KeyModifiers::NONE, KeyCode::Char('v')) => {
                self.open_variants();
            }
            // 从当前结果快速追搜元数据，避免手动重新输入长歌手名或专辑名。
            (KeyModifiers::NONE, KeyCode::Char('@')) => {
                if let Some((singer, song_source)) = self
                    .results
                    .get(self.selected)
                    .map(|song| (song.singer.clone(), song.source))
                {
                    let source = (song_source == SourceId::Bili && self.source_filter.is_none())
                        .then_some(SourceId::Bili)
                        .or(self.source_filter);
                    return self.search_metadata(&singer, source, true);
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('#')) => {
                if let Some(album) = self
                    .results
                    .get(self.selected)
                    .map(|song| song.album_name.clone())
                {
                    return self.search_metadata(&album, self.source_filter, false);
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('f')) => {
                if let Some(song) = self.results.get(self.selected).cloned() {
                    return AppAction::ToggleFavoriteSong(Box::new(song));
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('a')) => {
                if let Some(song) = self.results.get(self.selected).cloned() {
                    return AppAction::AddToQueue {
                        song: Box::new(song),
                        position: InsertPosition::End,
                    };
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('A'))
            | (KeyModifiers::SHIFT, KeyCode::Char('A')) => {
                if let Some(song) = self.results.get(self.selected).cloned() {
                    return AppAction::AddToQueue {
                        song: Box::new(song),
                        position: InsertPosition::Next,
                    };
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('l')) => {
                if !self.results.is_empty() {
                    return self.activate_selected_result();
                }
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
                    return self.activate_selected_result();
                }
                self.last_input_time = std::time::Instant::now();
                self.prepare_plain_search(&keyword);
                self.last_searched_input = keyword.clone();
                return AppAction::Search {
                    keyword,
                    source: self.source_filter,
                };
            }
            (KeyModifiers::NONE, KeyCode::Up) => {
                if !self.results.is_empty() {
                    if self.selected > 0 {
                        self.selected -= 1;
                    } else if self.wrap_navigation {
                        self.selected = self.results.len().saturating_sub(1);
                    }
                }
            }
            (KeyModifiers::NONE, KeyCode::Down) => {
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

    fn search_metadata(
        &mut self,
        value: &str,
        source: Option<SourceId>,
        artist: bool,
    ) -> AppAction {
        let keyword = value.trim().to_string();
        if keyword.is_empty() {
            return AppAction::None;
        }
        let current = if artist {
            self.artist_filter.as_deref()
        } else {
            self.album_filter.as_deref()
        };
        if current == Some(keyword.as_str()) {
            if artist {
                self.artist_filter = None;
            } else {
                self.album_filter = None;
            }
            let restore = self.base_keyword.trim().to_string();
            self.input = restore.clone();
            self.input_mode = false;
            if restore.is_empty() {
                return AppAction::None;
            }
            self.last_searched_input = restore.clone();
            return AppAction::Search {
                keyword: restore,
                source: self.source_filter,
            };
        }
        if artist {
            self.artist_filter = Some(keyword.clone());
        } else {
            self.album_filter = Some(keyword.clone());
        }
        self.input = keyword.clone();
        self.input_mode = false;
        self.last_input_time = std::time::Instant::now();
        self.last_searched_input = keyword.clone();
        AppAction::Search { keyword, source }
    }

    fn prepare_plain_search(&mut self, keyword: &str) {
        self.base_keyword = keyword.to_string();
        self.artist_filter = None;
        self.album_filter = None;
    }

    fn can_load_more(&self) -> bool {
        self.has_more
            && !self.is_searching
            && self.input.trim() == self.result_keyword
            && self.source_filter == self.result_source_filter
            && self.current_page > 0
    }

    fn activate_selected_result(&mut self) -> AppAction {
        let songs = self.results.clone();
        let index = self.selected;
        let Some(song) = songs.get(index) else {
            return AppAction::None;
        };
        if needs_bili_part_selection(song) {
            self.next_bili_parts_request_id += 1;
            let request_id = self.next_bili_parts_request_id;
            self.bili_parts_loading = Some(request_id);
            return AppAction::ResolveBiliParts {
                songs,
                index,
                request_id,
            };
        }
        AppAction::PlaySong { songs, index }
    }

    fn cycle_source(&mut self, direction: isize) -> AppAction {
        let current = self
            .search_scopes
            .iter()
            .position(|(scope, _)| *scope == self.source_filter)
            .unwrap_or(0);
        let next = (current as isize + direction)
            .rem_euclid(self.search_scopes.len().max(1) as isize) as usize;
        self.select_source(next)
    }

    fn select_source(&mut self, index: usize) -> AppAction {
        let Some((source, _)) = self.search_scopes.get(index).copied() else {
            return AppAction::None;
        };
        if self.source_filter == source && self.result_source_filter == source {
            return AppAction::None;
        }
        self.source_filter = source;
        self.error_message = None;
        self.close_variants();

        self.close_bili_part_overlay();
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
        enabled_sources: &[SourceId],
    ) {
        self.wrap_navigation = wrap_navigation;
        self.scroll_amount = scroll_amount.max(1);
        self.search_scopes = enabled_search_scopes(enabled_sources);
        self.source_filter = if aggregate_search {
            None
        } else if enabled_sources.contains(&default_source) {
            Some(default_source)
        } else {
            enabled_sources.first().copied()
        };
        if !self
            .search_scopes
            .iter()
            .any(|(scope, _)| *scope == self.source_filter)
        {
            self.source_filter = None;
        }
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
        self.close_bili_part_overlay();
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
            let mut seen: std::collections::HashSet<(String, _)> = self
                .results
                .iter()
                .map(|item| (item.id.clone(), item.source))
                .collect();
            for song in result.items {
                if seen.insert((song.id.clone(), song.source)) {
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
        self.close_bili_part_overlay();
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
        let input_block = if self.input_mode {
            super::components::chrome::focused_card(
                ctx,
                format!(" 搜索  ·  {}  ·  {} ", scope, mode),
            )
        } else {
            super::components::chrome::card(ctx, format!(" 搜索  ·  {}  ·  {} ", scope, mode))
        };

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
            let filters = match (&self.artist_filter, &self.album_filter) {
                (Some(artist), Some(album)) => format!(" · 歌手:{} · 专辑:{}", artist, album),
                (Some(artist), None) => format!(" · 歌手:{}", artist),
                (None, Some(album)) => format!(" · 专辑:{}", album),
                (None, None) => String::new(),
            };
            format!(
                "搜索结果 {}/{}{}{} · v 音源 · @ 歌手(再按删除) · # 专辑(再按删除)",
                self.results.len(),
                self.total,
                loading_more,
                filters
            )
        };
        let result_block = super::components::chrome::card(ctx, result_title);

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

        let selected_style = Style::new()
            .bg(accent)
            .fg(crate::theme::selection_fg(ctx))
            .add_modifier(Modifier::BOLD);
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
        self.render_bili_part_overlay(area, buf, ctx);
    }

    fn render_source_tabs(&self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let selected = self
            .search_scopes
            .iter()
            .position(|(scope, _)| *scope == self.source_filter)
            .unwrap_or(0);
        for (index, tab_area) in source_tab_areas(area, self.search_scopes.len())
            .iter()
            .copied()
            .enumerate()
        {
            let label = if area.width >= 66 {
                self.search_scopes[index].1
            } else {
                self.search_scopes[index]
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
        if self.part_picker.is_some() {
            return self.handle_bili_part_mouse(event, area, activate);
        }
        if self.bili_parts_loading.is_some() {
            return AppAction::None;
        }
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
            && let Some(index) = source_tab_areas(chunks[1], self.search_scopes.len())
                .iter()
                .position(|tab| tab.contains((event.column, event.row).into()))
        {
            self.input_mode = false;
            return self.select_source(index);
        }

        // 结果列表区域：滚轮只在光标落在该区域内时移动选中，
        // 避免光标悬停在输入框 / 音源 tab 上时列表跟着滚动（对齐 playlists.rs）。
        let results_area = chunks[2];
        match event.kind {
            MouseEventKind::ScrollUp if results_area.contains((event.column, event.row).into()) => {
                self.selected = self.selected.saturating_sub(self.scroll_amount);
            }
            MouseEventKind::ScrollDown
                if results_area.contains((event.column, event.row).into()) =>
            {
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
                            return self.activate_selected_result();
                        }
                    }
                }
            }
            _ => {}
        }
        AppAction::None
    }

    pub fn context_song_at(
        &mut self,
        event: MouseEvent,
        area: Rect,
    ) -> Option<(Vec<SongInfo>, usize)> {
        if self.part_picker.is_some()
            || self.bili_parts_loading.is_some()
            || !self.variant_indices.is_empty()
        {
            return None;
        }
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(area);
        let inner = Block::default().borders(Borders::ALL).inner(chunks[2]);
        let list_y = inner.y.saturating_add(1);
        if event.row < list_y || event.row >= inner.bottom() {
            return None;
        }
        let index = self.scroll_offset + event.row.saturating_sub(list_y) as usize;
        if index >= self.results.len() {
            return None;
        }
        self.input_mode = false;
        self.selected = index;
        Some((self.results.clone(), index))
    }

    fn open_variants(&mut self) {
        self.variant_indices = matching_variant_indices(&self.results, self.selected);
        self.variant_selected = self
            .variant_indices
            .iter()
            .position(|index| *index == self.selected)
            .unwrap_or(0);
        self.variant_scroll_offset = 0;
        self.ensure_variant_visible();
    }

    fn close_variants(&mut self) {
        self.variant_indices.clear();
        self.variant_selected = 0;
        self.variant_scroll_offset = 0;
    }

    fn close_bili_part_overlay(&mut self) {
        self.part_picker = None;
        self.bili_parts_loading = None;
    }

    /// Applies a response only when it belongs to the active request. Single-part videos bypass
    /// the picker; multi-part videos retain the original list until the user confirms a part.
    pub fn complete_bili_part_request(
        &mut self,
        request_id: u64,
        songs: Vec<SongInfo>,
        index: usize,
        parts: Vec<SongInfo>,
    ) -> Option<AppAction> {
        if self.bili_parts_loading != Some(request_id) {
            return None;
        }
        self.bili_parts_loading = None;
        if parts.len() == 1 {
            let mut songs = songs;
            songs.splice(index..=index, parts);
            return Some(AppAction::PlaySong { songs, index });
        }
        let video_title = songs
            .get(index)
            .map(|song| song.name.clone())
            .unwrap_or_else(|| "哔哩哔哩视频".to_string());
        self.part_picker = Some(BiliPartPicker {
            video_title,
            songs,
            replacement_index: index,
            parts,
            selected: 0,
            scroll_offset: 0,
        });
        None
    }

    pub fn fail_bili_part_request(&mut self, request_id: u64) -> bool {
        if self.bili_parts_loading != Some(request_id) {
            return false;
        }
        self.bili_parts_loading = None;
        true
    }

    fn handle_bili_part_input(&mut self, key: KeyEvent) -> AppAction {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc) => self.part_picker = None,
            (KeyModifiers::NONE, KeyCode::Up | KeyCode::Char('k')) => {
                if let Some(picker) = self.part_picker.as_mut() {
                    picker.selected = picker.selected.saturating_sub(1);
                }
            }
            (KeyModifiers::NONE, KeyCode::Down | KeyCode::Char('j')) => {
                if let Some(picker) = self.part_picker.as_mut() {
                    picker.selected =
                        (picker.selected + 1).min(picker.parts.len().saturating_sub(1));
                }
            }
            (KeyModifiers::NONE, KeyCode::Home) | (KeyModifiers::NONE, KeyCode::Char('g')) => {
                if let Some(picker) = self.part_picker.as_mut() {
                    picker.selected = 0;
                }
            }
            (KeyModifiers::NONE, KeyCode::End)
            | (KeyModifiers::NONE, KeyCode::Char('G'))
            | (KeyModifiers::SHIFT, KeyCode::Char('G')) => {
                if let Some(picker) = self.part_picker.as_mut() {
                    picker.selected = picker.parts.len().saturating_sub(1);
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('a')) => {
                if let Some(song) = self
                    .part_picker
                    .as_ref()
                    .and_then(|picker| picker.parts.get(picker.selected))
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
                if let Some(song) = self
                    .part_picker
                    .as_ref()
                    .and_then(|picker| picker.parts.get(picker.selected))
                    .cloned()
                {
                    return AppAction::AddToQueue {
                        song: Box::new(song),
                        position: InsertPosition::Next,
                    };
                }
            }
            (KeyModifiers::NONE, KeyCode::Enter) | (KeyModifiers::NONE, KeyCode::Char('l')) => {
                return self.confirm_bili_part();
            }
            _ => {}
        }
        AppAction::None
    }

    /// Replace the selected video with all of its parts and begin at the part chosen in the popup.
    fn confirm_bili_part(&mut self) -> AppAction {
        let Some(picker) = self.part_picker.take() else {
            return AppAction::None;
        };
        let selected = picker.selected.min(picker.parts.len().saturating_sub(1));
        let mut songs = picker.songs;
        songs.splice(
            picker.replacement_index..=picker.replacement_index,
            picker.parts,
        );
        AppAction::PlaySong {
            songs,
            index: picker.replacement_index + selected,
        }
    }

    fn handle_bili_part_mouse(
        &mut self,
        event: MouseEvent,
        area: Rect,
        activate: bool,
    ) -> AppAction {
        let popup = bili_part_popup(
            area,
            self.part_picker
                .as_ref()
                .map_or(0, |picker| picker.parts.len()),
        );
        match event.kind {
            MouseEventKind::ScrollUp => {
                if let Some(picker) = self.part_picker.as_mut() {
                    picker.selected = picker.selected.saturating_sub(1);
                }
            }
            MouseEventKind::ScrollDown => {
                if let Some(picker) = self.part_picker.as_mut() {
                    picker.selected =
                        (picker.selected + 1).min(picker.parts.len().saturating_sub(1));
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if !popup.contains((event.column, event.row).into()) {
                    self.part_picker = None;
                    return AppAction::None;
                }
                let inner = Block::default().borders(Borders::ALL).inner(popup);
                let list_y = inner.y.saturating_add(1);
                let footer_y = inner.bottom().saturating_sub(1);
                let mut confirm = false;
                if event.row >= list_y
                    && event.row < footer_y
                    && let Some(picker) = self.part_picker.as_mut()
                {
                    let index = picker.scroll_offset + event.row.saturating_sub(list_y) as usize;
                    if index < picker.parts.len() {
                        picker.selected = index;
                        confirm = activate;
                    }
                }
                if confirm {
                    return self.confirm_bili_part();
                }
            }
            _ => {}
        }
        AppAction::None
    }

    fn handle_variant_input(&mut self, key: KeyEvent) -> AppAction {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc) | (KeyModifiers::NONE, KeyCode::Char('v')) => {
                self.close_variants();
            }
            (KeyModifiers::NONE, KeyCode::Up) | (KeyModifiers::NONE, KeyCode::Left) => {
                if self.variant_selected > 0 {
                    self.variant_selected -= 1;
                } else if self.wrap_navigation {
                    self.variant_selected = self.variant_indices.len().saturating_sub(1);
                }
                self.ensure_variant_visible();
            }
            (KeyModifiers::NONE, KeyCode::Down) | (KeyModifiers::NONE, KeyCode::Right) => {
                if self.variant_selected + 1 < self.variant_indices.len() {
                    self.variant_selected += 1;
                } else if self.wrap_navigation {
                    self.variant_selected = 0;
                }
                self.ensure_variant_visible();
            }
            (KeyModifiers::NONE, KeyCode::Home) | (KeyModifiers::NONE, KeyCode::Char('g')) => {
                self.variant_selected = 0;
                self.ensure_variant_visible();
            }
            (KeyModifiers::NONE, KeyCode::End)
            | (KeyModifiers::NONE, KeyCode::Char('G'))
            | (KeyModifiers::SHIFT, KeyCode::Char('G')) => {
                self.variant_selected = self.variant_indices.len().saturating_sub(1);
                self.ensure_variant_visible();
            }
            (KeyModifiers::NONE, KeyCode::Char('a')) => {
                if let Some(index) = self.variant_indices.get(self.variant_selected).copied()
                    && let Some(song) = self.results.get(index).cloned()
                {
                    return AppAction::AddToQueue {
                        song: Box::new(song),
                        position: InsertPosition::End,
                    };
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('A'))
            | (KeyModifiers::SHIFT, KeyCode::Char('A')) => {
                if let Some(index) = self.variant_indices.get(self.variant_selected).copied()
                    && let Some(song) = self.results.get(index).cloned()
                {
                    return AppAction::AddToQueue {
                        song: Box::new(song),
                        position: InsertPosition::Next,
                    };
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('f')) => {
                if let Some(song) = self
                    .variant_indices
                    .get(self.variant_selected)
                    .and_then(|index| self.results.get(*index))
                    .cloned()
                {
                    return AppAction::ToggleFavoriteSong(Box::new(song));
                }
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
                    // 弹窗可滚动时行号要加上滚动偏移才是真正的变体下标
                    let index =
                        self.variant_scroll_offset + event.row.saturating_sub(list_y) as usize;
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

    fn render_variant_picker(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
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

        let visible_rows = inner.height.saturating_sub(1) as usize;
        if visible_rows == 0 {
            return;
        }
        // 记录可视行数，供键盘导航的可见性调整使用
        self.variant_viewport = visible_rows;
        self.ensure_variant_visible();
        let end = (self.variant_scroll_offset + visible_rows).min(self.variant_indices.len());
        for (row, result_index) in self.variant_indices[self.variant_scroll_offset..end]
            .iter()
            .copied()
            .enumerate()
        {
            let Some(song) = self.results.get(result_index) else {
                continue;
            };
            let style = if self.variant_scroll_offset + row == self.variant_selected {
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

    /// 让变体选中项保持在弹窗可视窗口内（渲染与键盘导航共用）。
    fn ensure_variant_visible(&mut self) {
        let visible_rows = self.variant_viewport.max(1);
        if self.variant_selected < self.variant_scroll_offset {
            self.variant_scroll_offset = self.variant_selected;
        } else if self.variant_selected >= self.variant_scroll_offset + visible_rows {
            self.variant_scroll_offset = self.variant_selected + 1 - visible_rows;
        }
        self.variant_scroll_offset = self
            .variant_scroll_offset
            .min(self.variant_indices.len().saturating_sub(visible_rows));
    }
    fn render_bili_part_overlay(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        if self.bili_parts_loading.is_some() {
            let popup = bili_part_popup(area, 0);
            Clear.render(popup, buf);
            Paragraph::new("正在读取分 P…  Esc 取消")
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::new().fg(crate::theme::accent(ctx)))
                        .title(" 选择分 P "),
                )
                .alignment(ratatui::layout::Alignment::Center)
                .render(popup, buf);
            return;
        }

        let Some(picker) = self.part_picker.as_mut() else {
            return;
        };
        let popup = bili_part_popup(area, picker.parts.len());
        if popup.width == 0 || popup.height == 0 {
            return;
        }
        Clear.render(popup, buf);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(crate::theme::accent(ctx)))
            .title(format!(" 选择分 P · {} ", picker.video_title));
        let inner = block.inner(popup);
        block.render(popup, buf);
        let visible_rows = inner.height.saturating_sub(2) as usize;
        if visible_rows == 0 {
            return;
        }
        if picker.selected < picker.scroll_offset {
            picker.scroll_offset = picker.selected;
        } else if picker.selected >= picker.scroll_offset + visible_rows {
            picker.scroll_offset = picker.selected + 1 - visible_rows;
        }
        picker.scroll_offset = picker
            .scroll_offset
            .min(picker.parts.len().saturating_sub(visible_rows));

        Paragraph::new(Line::from(Span::styled(
            " P      时长       标题",
            Style::new()
                .fg(crate::theme::muted(ctx))
                .add_modifier(Modifier::BOLD),
        )))
        .render(Rect::new(inner.x, inner.y, inner.width, 1), buf);

        let end = (picker.scroll_offset + visible_rows).min(picker.parts.len());
        for (row, part_index) in (picker.scroll_offset..end).enumerate() {
            let part = &picker.parts[part_index];
            let style = if part_index == picker.selected {
                Style::new()
                    .fg(crate::theme::selection_fg(ctx))
                    .bg(crate::theme::accent(ctx))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(crate::theme::text(ctx))
            };
            Paragraph::new(Line::from(Span::styled(bili_part_row(part), style))).render(
                Rect::new(inner.x, inner.y + 1 + row as u16, inner.width, 1),
                buf,
            );
        }
        Paragraph::new(" ↑↓/j k 选择 · Enter 播放 · Esc 取消")
            .style(Style::new().fg(crate::theme::muted(ctx)))
            .render(
                Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
                buf,
            );
    }
}

fn source_tab_areas(area: Rect, count: usize) -> std::rc::Rc<[Rect]> {
    if count == 0 {
        return std::rc::Rc::from([]);
    }
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(std::iter::repeat_n(
            Constraint::Ratio(1, count as u32),
            count,
        ))
        .split(area)
}

fn enabled_search_scopes(enabled_sources: &[SourceId]) -> Vec<(Option<SourceId>, &'static str)> {
    SEARCH_SCOPES
        .iter()
        .copied()
        .filter(|(source, _)| {
            source.is_none()
                || *source == Some(SourceId::Local)
                || source.is_some_and(|source| enabled_sources.contains(&source))
        })
        .collect()
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

fn bili_part_popup(area: Rect, count: usize) -> Rect {
    let width = area.width.saturating_sub(4).min(92);
    let height = (count as u16 + 4)
        .min(area.height.saturating_sub(2))
        .max(5.min(area.height));
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn needs_bili_part_selection(song: &SongInfo) -> bool {
    song.source == SourceId::Bili && !song.extra.contains_key("page")
}

fn bili_part_row(song: &SongInfo) -> String {
    let page = song.extra.get("page").map(String::as_str).unwrap_or("?");
    let title = song
        .extra
        .get("bili_part_title")
        .filter(|title| !title.is_empty())
        .map(String::as_str)
        .unwrap_or(&song.name);
    let duration = song.duration.as_secs();
    format!(
        " P{page:<4} {:02}:{:02}      {title}",
        duration / 60,
        duration % 60
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
    use lx_core::keybinding::KeybindingResolver;
    use lx_core::model::song::SongInfo;
    use lx_core::model::source::SourceId;

    fn song(id: &str, source: SourceId, name: &str, singer: &str) -> SongInfo {
        SongInfo::new(id.to_string(), source, name.to_string(), singer.to_string())
    }

    #[test]
    fn right_arrow_cycles_search_scope() {
        let mut page = SearchPage::new(None, true, 3, SourceId::all_online());
        page.input_mode = false;
        let resolver =
            KeybindingResolver::from_config(&lx_core::keybinding::KeybindingConfig::default());

        let action =
            page.handle_input(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), &resolver);

        assert_eq!(page.source_filter, Some(SourceId::Kw));
        assert!(matches!(action, AppAction::None));
    }

    #[test]
    fn brackets_cycle_search_scope_outside_input_mode() {
        let mut page = SearchPage::new(None, true, 3, SourceId::all_online());
        page.input_mode = false;
        let resolver =
            KeybindingResolver::from_config(&lx_core::keybinding::KeybindingConfig::default());

        page.handle_input(
            KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE),
            &resolver,
        );
        assert_eq!(page.source_filter, Some(SourceId::Kw));

        page.handle_input(
            KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE),
            &resolver,
        );
        assert_eq!(page.source_filter, None);
    }

    #[test]
    fn variant_picker_closes_with_escape_or_second_v() {
        let mut page = SearchPage::new(None, true, 3, SourceId::all_online());
        page.input_mode = false;
        page.results = vec![
            song("kw-1", SourceId::Kw, "晴天", "周杰伦"),
            song("kg-1", SourceId::Kg, "晴天", "周杰伦"),
        ];
        let resolver =
            KeybindingResolver::from_config(&lx_core::keybinding::KeybindingConfig::default());

        page.handle_input(
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
            &resolver,
        );
        assert_eq!(page.variant_indices, vec![0, 1]);
        page.handle_input(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &resolver);
        assert!(page.variant_indices.is_empty());

        page.handle_input(
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
            &resolver,
        );
        page.handle_input(
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
            &resolver,
        );
        assert!(page.variant_indices.is_empty());
    }

    #[test]
    fn idle_escape_stays_on_search_page() {
        let mut page = SearchPage::new(None, true, 3, SourceId::all_online());
        page.input_mode = false;
        let resolver =
            KeybindingResolver::from_config(&lx_core::keybinding::KeybindingConfig::default());

        let action = page.handle_input(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &resolver);

        assert!(matches!(action, AppAction::None));
    }

    #[test]
    fn l_plays_the_selected_search_result() {
        let mut page = SearchPage::new(None, true, 3, SourceId::all_online());
        page.input_mode = false;
        page.results = vec![song("kw-1", SourceId::Kw, "晴天", "周杰伦")];
        let resolver =
            KeybindingResolver::from_config(&lx_core::keybinding::KeybindingConfig::default());

        let action = page.handle_input(
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
            &resolver,
        );

        assert!(matches!(action, AppAction::PlaySong { index: 0, .. }));
    }

    #[test]
    fn at_and_hash_follow_up_searches_use_selected_metadata() {
        let mut page = SearchPage::new(None, true, 3, SourceId::all_online());
        page.input_mode = false;
        let mut selected = song("kw-1", SourceId::Kw, "晴天", "周杰伦");
        selected.album_name = "叶惠美".to_string();
        page.results = vec![selected];
        let resolver =
            KeybindingResolver::from_config(&lx_core::keybinding::KeybindingConfig::default());

        let action = page.handle_input(
            KeyEvent::new(KeyCode::Char('@'), KeyModifiers::NONE),
            &resolver,
        );
        assert!(matches!(action, AppAction::Search { ref keyword, .. } if keyword == "周杰伦"));

        let action = page.handle_input(
            KeyEvent::new(KeyCode::Char('#'), KeyModifiers::NONE),
            &resolver,
        );
        assert!(matches!(action, AppAction::Search { ref keyword, .. } if keyword == "叶惠美"));
    }

    #[test]
    fn selecting_bili_video_opens_part_picker_and_plays_selected_part() {
        let mut page = SearchPage::new(None, true, 3, SourceId::all_online());
        let mut video = song("BV1xx411c7mD", SourceId::Bili, "测试视频", "UP主");
        video
            .extra
            .insert("bvid".to_string(), "BV1xx411c7mD".to_string());
        page.results = vec![video];
        let resolver =
            KeybindingResolver::from_config(&lx_core::keybinding::KeybindingConfig::default());

        let (songs, index, request_id) = match page.handle_input(
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
            &resolver,
        ) {
            AppAction::ResolveBiliParts {
                songs,
                index,
                request_id,
            } => (songs, index, request_id),
            action => panic!("expected Bili part resolution, got {action:?}"),
        };

        let mut first = song(
            "BV1xx411c7mD-p1",
            SourceId::Bili,
            "测试视频 · P1 第一段",
            "UP主",
        );
        first.extra.insert("page".to_string(), "1".to_string());
        first
            .extra
            .insert("bili_part_title".to_string(), "第一段".to_string());
        let mut second = song(
            "BV1xx411c7mD-p2",
            SourceId::Bili,
            "测试视频 · P2 第二段",
            "UP主",
        );
        second.extra.insert("page".to_string(), "2".to_string());
        second
            .extra
            .insert("bili_part_title".to_string(), "第二段".to_string());

        assert!(
            page.complete_bili_part_request(request_id, songs, index, vec![first, second])
                .is_none()
        );
        assert!(matches!(
            page.handle_bili_part_input(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            AppAction::None
        ));

        let action = page.handle_bili_part_input(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let AppAction::PlaySong { songs, index } = action else {
            panic!("expected selected part playback");
        };
        assert_eq!(index, 1);
        assert_eq!(songs[index].extra["bili_part_title"], "第二段");
    }

    #[test]
    fn single_bili_part_starts_without_opening_picker() {
        let mut page = SearchPage::new(None, true, 3, SourceId::all_online());
        page.bili_parts_loading = Some(7);
        let mut part = song("BV1xx411c7mD", SourceId::Bili, "测试视频", "UP主");
        part.extra.insert("page".to_string(), "1".to_string());

        let action = page
            .complete_bili_part_request(
                7,
                vec![song("video", SourceId::Bili, "测试视频", "UP主")],
                0,
                vec![part],
            )
            .expect("single part should start playback");

        let AppAction::PlaySong { songs, index } = action else {
            panic!("expected single part playback");
        };
        assert_eq!(index, 0);
        assert_eq!(songs.len(), 1);
        assert_eq!(songs[0].extra["page"], "1");
        assert!(page.part_picker.is_none());
    }

    #[test]
    fn source_tab_selects_single_source_and_starts_search() {
        let mut page = SearchPage::new(None, true, 3, SourceId::all_online());
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
    fn disabled_sources_are_omitted_from_scope_navigation() {
        let mut page = SearchPage::new(None, true, 3, &[SourceId::Kw, SourceId::Wy]);

        page.cycle_source(1);
        assert_eq!(page.source_filter, Some(SourceId::Kw));
        page.cycle_source(1);
        assert_eq!(page.source_filter, Some(SourceId::Wy));
        page.cycle_source(1);
        assert_eq!(page.source_filter, Some(SourceId::Local));
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
