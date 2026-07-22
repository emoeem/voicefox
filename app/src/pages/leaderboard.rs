//! 排行榜页面

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lx_core::events::{AppAction, InsertPosition};
use lx_core::model::leaderboard::LeaderboardInfo;
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::context::AppContext;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaderboardLoadRequest {
    Boards { source: SourceId },
    Songs { source: SourceId, board_id: String },
}

pub struct LeaderboardPage {
    sources: Vec<SourceId>,
    source_index: usize,
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
}

impl LeaderboardPage {
    pub fn new(sources: Vec<SourceId>) -> Self {
        Self {
            sources,
            source_index: 0,
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
            }
            _ => {}
        }
        self.selected = 0;
    }

    pub fn handle_input(&mut self, key: &KeyEvent, ctx: &AppContext) -> AppAction {
        match (key.modifiers, key.code) {
            (KeyModifiers::CONTROL, KeyCode::Left)
            | (KeyModifiers::CONTROL, KeyCode::Char('h'))
            | (KeyModifiers::NONE, KeyCode::Char('[')) => self.select_previous_source(),
            (KeyModifiers::CONTROL, KeyCode::Right)
            | (KeyModifiers::CONTROL, KeyCode::Char('l'))
            | (KeyModifiers::NONE, KeyCode::Char(']')) => self.select_next_source(),
            (KeyModifiers::NONE, KeyCode::Left) if self.selected_board.is_none() => {
                self.select_previous_source();
            }
            (KeyModifiers::NONE, KeyCode::Right) if self.selected_board.is_none() => {
                self.select_next_source();
            }
            (KeyModifiers::NONE, KeyCode::Up) | (KeyModifiers::NONE, KeyCode::Char('k')) => {
                self.move_selection_up(ctx);
            }
            (KeyModifiers::NONE, KeyCode::Down) | (KeyModifiers::NONE, KeyCode::Char('j')) => {
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
                self.selected = self.selected.saturating_sub(10);
            }
            (KeyModifiers::CONTROL, KeyCode::Char('d'))
            | (KeyModifiers::NONE, KeyCode::PageDown) => {
                self.selected = (self.selected + 10).min(self.current_list_len().saturating_sub(1));
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

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        let page = page_chunks(area, self.boards.len());
        self.render_sources(page.sources, buf, ctx);
        self.render_boards(page.boards, buf, ctx);
        self.render_songs(page.songs, buf, ctx);
    }

    pub fn handle_mouse(
        &mut self,
        event: MouseEvent,
        area: Rect,
        activate: bool,
        ctx: &AppContext,
    ) -> AppAction {
        let page = page_chunks(area, self.boards.len());
        let position = Position::new(event.column, event.row);
        let scroll_amount = ctx.config.read().unwrap().ui.scroll_amount.max(1);
        match event.kind {
            MouseEventKind::ScrollUp => {
                self.selected = self.selected.saturating_sub(scroll_amount);
            }
            MouseEventKind::ScrollDown => {
                self.selected =
                    (self.selected + scroll_amount).min(self.current_list_len().saturating_sub(1));
            }
            MouseEventKind::Down(MouseButton::Left) => {
                for (index, tab) in source_tab_rects(page.sources, self.sources.len())
                    .iter()
                    .enumerate()
                {
                    if tab.contains(position) {
                        self.select_source(index);
                        return AppAction::None;
                    }
                }

                let board_inner = Block::default().borders(Borders::ALL).inner(page.boards);
                if board_inner.contains(position) {
                    let index =
                        self.board_scroll_offset + event.row.saturating_sub(board_inner.y) as usize;
                    if index < self.boards.len() {
                        if activate {
                            self.selected = index;
                            self.enter_selected_board();
                        } else if self.selected_board.is_none() {
                            self.selected = index;
                        }
                    }
                    return AppAction::None;
                }

                if self.selected_board.is_some() {
                    let song_inner = Block::default().borders(Borders::ALL).inner(page.songs);
                    let list_y = song_inner.y.saturating_add(1);
                    if event.row >= list_y && event.row < song_inner.bottom() {
                        let index =
                            self.song_scroll_offset + event.row.saturating_sub(list_y) as usize;
                        if index < self.songs.len() {
                            self.selected = index;
                            if activate {
                                return AppAction::PlaySong {
                                    songs: self.songs.clone(),
                                    index,
                                };
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        AppAction::None
    }

    fn render_sources(&self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let accent = crate::theme::accent(ctx);
        for (index, tab) in source_tab_rects(area, self.sources.len())
            .iter()
            .enumerate()
        {
            let source = self.sources[index];
            let label = if area.width >= 48 {
                source_label(source)
            } else {
                source.as_str()
            };
            let style = if index == self.source_index {
                Style::new()
                    .bg(accent)
                    .fg(crate::theme::selection_fg(ctx))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(crate::theme::muted(ctx))
            };
            Paragraph::new(label)
                .alignment(Alignment::Center)
                .style(style)
                .render(*tab, buf);
        }
    }

    fn render_boards(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(crate::theme::border(ctx)))
            .title(format!("榜单 ({})", self.boards.len()));
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
                label = format!("{}  {}", board.name, update);
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
            .borders(Borders::ALL)
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

        Paragraph::new(Line::from(Span::styled(
            super::components::song_table::header(inner.width),
            Style::new()
                .fg(crate::theme::muted(ctx))
                .add_modifier(Modifier::BOLD),
        )))
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
            let text =
                super::components::song_table::row(&self.songs[index], index, list_area.width);
            let style = if index == self.selected {
                selected_style
            } else {
                Style::new().fg(crate::theme::text(ctx))
            };
            Paragraph::new(Line::from(Span::styled(text, style))).render(
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
        } else if ctx.config.read().unwrap().ui.wrap_navigation {
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
        } else if ctx.config.read().unwrap().ui.wrap_navigation {
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
}

struct PageChunks {
    sources: Rect,
    boards: Rect,
    songs: Rect,
}

fn page_chunks(area: Rect, board_count: usize) -> PageChunks {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);
    let content = if area.width < 82 {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length((board_count as u16 + 2).clamp(5, 11)),
                Constraint::Min(0),
            ])
            .split(vertical[1])
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(30), Constraint::Min(0)])
            .split(vertical[1])
    };
    PageChunks {
        sources: vertical[0],
        boards: content[0],
        songs: content[1],
    }
}

fn source_tab_rects(area: Rect, count: usize) -> std::rc::Rc<[Rect]> {
    if count == 0 {
        return std::rc::Rc::from([]);
    }
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![Constraint::Ratio(1, count as u32); count])
        .split(area)
}

fn ensure_visible(selected: usize, visible: usize, total: usize, offset: &mut usize) {
    if visible == 0 || total == 0 {
        *offset = 0;
        return;
    }
    if selected >= offset.saturating_add(visible) {
        *offset = selected.saturating_sub(visible - 1);
    } else if selected < *offset {
        *offset = selected;
    }
    *offset = (*offset).min(total.saturating_sub(visible));
}

fn source_name(source: SourceId) -> &'static str {
    match source {
        SourceId::Kw => "酷我",
        SourceId::Kg => "酷狗",
        SourceId::Tx => "QQ",
        SourceId::Wy => "网易云",
        SourceId::Mg => "咪咕",
        SourceId::Local => "本地",
    }
}

fn source_label(source: SourceId) -> &'static str {
    match source {
        SourceId::Kw => "酷我 kw",
        SourceId::Kg => "酷狗 kg",
        SourceId::Tx => "QQ tx",
        SourceId::Wy => "网易 wy",
        SourceId::Mg => "咪咕 mg",
        SourceId::Local => "本地 local",
    }
}

fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    if max <= 1 {
        return "…".chars().take(max).collect();
    }
    let mut result = value.chars().take(max - 1).collect::<String>();
    result.push('…');
    result
}

#[cfg(test)]
mod tests {
    use super::{LeaderboardLoadRequest, LeaderboardPage};
    use lx_core::model::leaderboard::LeaderboardInfo;
    use lx_core::model::song::SongInfo;
    use lx_core::model::source::SourceId;

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
}
