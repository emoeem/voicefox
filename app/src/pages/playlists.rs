//! 热门歌单与歌单收藏页面

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lx_core::events::{AppAction, InsertPosition, Notification};
use lx_core::model::playlist::Playlist;
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::context::AppContext;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaylistLoadRequest {
    List {
        source: SourceId,
    },
    Songs {
        source: SourceId,
        playlist_id: String,
    },
}

pub struct PlaylistsPage {
    scopes: Vec<Option<SourceId>>,
    scope_index: usize,
    pub playlists: Vec<Playlist>,
    pub songs: Vec<SongInfo>,
    pub selected: usize,
    pub selected_playlist: Option<usize>,
    list_loaded: bool,
    list_loading: bool,
    songs_loaded: bool,
    songs_loading: bool,
    playlist_scroll_offset: usize,
    song_scroll_offset: usize,
    error_message: Option<String>,
    list_cache: HashMap<SourceId, Vec<Playlist>>,
    song_cache: HashMap<(SourceId, String), Vec<SongInfo>>,
}

impl PlaylistsPage {
    pub fn new(sources: Vec<SourceId>) -> Self {
        let scopes = std::iter::once(None)
            .chain(sources.into_iter().map(Some))
            .collect();
        Self {
            scopes,
            scope_index: 0,
            playlists: Vec::new(),
            songs: Vec::new(),
            selected: 0,
            selected_playlist: None,
            list_loaded: true,
            list_loading: false,
            songs_loaded: false,
            songs_loading: false,
            playlist_scroll_offset: 0,
            song_scroll_offset: 0,
            error_message: None,
            list_cache: HashMap::new(),
            song_cache: HashMap::new(),
        }
    }

    pub fn current_source(&self) -> Option<SourceId> {
        self.scopes.get(self.scope_index).copied().flatten()
    }

    pub fn current_playlist(&self) -> Option<&Playlist> {
        self.selected_playlist
            .and_then(|index| self.playlists.get(index))
    }

    pub fn sync_favorites(&mut self, ctx: &AppContext) {
        if self.current_source().is_some() || self.selected_playlist.is_some() {
            return;
        }
        self.playlists = ctx.storage.load_favorite_playlists();
        self.list_loaded = true;
        self.list_loading = false;
        self.selected = self.selected.min(self.playlists.len().saturating_sub(1));
    }

    pub fn next_load_request(&self) -> Option<PlaylistLoadRequest> {
        if let Some(playlist) = self.current_playlist() {
            if !self.songs_loading && !self.songs_loaded {
                return Some(PlaylistLoadRequest::Songs {
                    source: playlist.source,
                    playlist_id: playlist.id.clone(),
                });
            }
            return None;
        }
        let source = self.current_source()?;
        if !self.list_loading && !self.list_loaded {
            return Some(PlaylistLoadRequest::List { source });
        }
        None
    }

    pub fn begin_loading(&mut self, request: &PlaylistLoadRequest) {
        self.error_message = None;
        match request {
            PlaylistLoadRequest::List { .. } => {
                self.list_loading = true;
                self.list_loaded = false;
            }
            PlaylistLoadRequest::Songs { .. } => {
                self.songs_loading = true;
                self.songs_loaded = false;
            }
        }
    }

    pub fn update_playlists(&mut self, source: SourceId, playlists: Vec<Playlist>) {
        self.list_cache.insert(source, playlists.clone());
        if self.current_source() != Some(source) || self.selected_playlist.is_some() {
            return;
        }
        self.playlists = playlists;
        self.list_loading = false;
        self.list_loaded = true;
        self.error_message = None;
        self.selected = self.selected.min(self.playlists.len().saturating_sub(1));
        self.playlist_scroll_offset = 0;
    }

    pub fn update_songs(&mut self, source: SourceId, playlist_id: &str, songs: Vec<SongInfo>) {
        self.song_cache
            .insert((source, playlist_id.to_string()), songs.clone());
        if self
            .current_playlist()
            .map(|playlist| (playlist.source, playlist.id.as_str()))
            != Some((source, playlist_id))
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

    pub fn update_error(&mut self, request: &PlaylistLoadRequest, message: String) {
        match request {
            PlaylistLoadRequest::List { source }
                if self.current_source() == Some(*source) && self.selected_playlist.is_none() =>
            {
                self.playlists.clear();
                self.list_loading = false;
                self.list_loaded = true;
                self.error_message = Some(message);
            }
            PlaylistLoadRequest::Songs {
                source,
                playlist_id,
            } if self
                .current_playlist()
                .map(|playlist| (playlist.source, playlist.id.as_str()))
                == Some((*source, playlist_id.as_str())) =>
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
            | (KeyModifiers::NONE, KeyCode::Char('[')) => self.select_previous_scope(ctx),
            (KeyModifiers::CONTROL, KeyCode::Right) | (KeyModifiers::NONE, KeyCode::Char(']')) => {
                self.select_next_scope(ctx)
            }
            (KeyModifiers::NONE, KeyCode::Left) if self.selected_playlist.is_none() => {
                self.select_previous_scope(ctx);
            }
            (KeyModifiers::NONE, KeyCode::Right) if self.selected_playlist.is_none() => {
                self.select_next_scope(ctx);
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
                if self.selected_playlist.is_some() && !self.songs.is_empty() {
                    return AppAction::PlaySong {
                        songs: self.songs.clone(),
                        index: self.selected,
                    };
                }
                self.enter_selected_playlist();
            }
            (KeyModifiers::NONE, KeyCode::Char('a')) if self.selected_playlist.is_some() => {
                if let Some(song) = self.songs.get(self.selected).cloned() {
                    return AppAction::AddToQueue {
                        song: Box::new(song),
                        position: InsertPosition::End,
                    };
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('A'))
            | (KeyModifiers::SHIFT, KeyCode::Char('A'))
                if self.selected_playlist.is_some() =>
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
                if self.selected_playlist.is_some() =>
            {
                self.leave_playlist();
            }
            (KeyModifiers::NONE, KeyCode::Char('f'))
            | (KeyModifiers::CONTROL, KeyCode::Char('l')) => {
                return self.toggle_favorite(ctx);
            }
            (KeyModifiers::NONE, KeyCode::Char('r')) => self.refresh_current(ctx),
            _ => {}
        }
        AppAction::None
    }

    pub fn handle_mouse(
        &mut self,
        event: MouseEvent,
        area: Rect,
        activate: bool,
        ctx: &AppContext,
    ) -> AppAction {
        let page = page_chunks(area, self.playlists.len());
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
                for (index, tab) in scope_tab_rects(page.scopes, self.scopes.len())
                    .iter()
                    .enumerate()
                {
                    if tab.contains(position) {
                        self.select_scope(index, ctx);
                        return AppAction::None;
                    }
                }

                let playlist_inner = Block::default().borders(Borders::ALL).inner(page.playlists);
                if playlist_inner.contains(position) {
                    let index = self.playlist_scroll_offset
                        + event.row.saturating_sub(playlist_inner.y) as usize;
                    if index < self.playlists.len() {
                        if activate {
                            self.selected = index;
                            self.enter_selected_playlist();
                        } else if self.selected_playlist.is_none() {
                            self.selected = index;
                        }
                    }
                    return AppAction::None;
                }

                if self.selected_playlist.is_some() {
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
            MouseEventKind::Down(MouseButton::Right) if self.selected_playlist.is_none() => {
                let playlist_inner = Block::default().borders(Borders::ALL).inner(page.playlists);
                if playlist_inner.contains(position) {
                    let index = self.playlist_scroll_offset
                        + event.row.saturating_sub(playlist_inner.y) as usize;
                    if index < self.playlists.len() {
                        self.selected = index;
                        return self.toggle_favorite(ctx);
                    }
                }
            }
            _ => {}
        }
        AppAction::None
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        let page = page_chunks(area, self.playlists.len());
        self.render_scopes(page.scopes, buf, ctx);
        self.render_playlists(page.playlists, buf, ctx);
        self.render_songs(page.songs, buf, ctx);
    }

    fn render_scopes(&self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        for (index, tab) in scope_tab_rects(area, self.scopes.len()).iter().enumerate() {
            let label = scope_label(self.scopes[index], area.width >= 58);
            let style = if index == self.scope_index {
                Style::new()
                    .bg(crate::theme::accent(ctx))
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

    fn render_playlists(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        let title = if self.current_source().is_none() {
            format!("收藏歌单 ({})", self.playlists.len())
        } else if self.current_source() == Some(SourceId::Bili) {
            format!("我的收藏夹 ({})", self.playlists.len())
        } else {
            format!("热门歌单 ({})", self.playlists.len())
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(crate::theme::border(ctx)))
            .title(title);
        let inner = block.inner(area);
        block.render(area, buf);

        if self.selected_playlist.is_none() {
            if let Some(error) = &self.error_message {
                Paragraph::new(format!("加载失败: {error}"))
                    .style(Style::new().fg(crate::theme::red(ctx)))
                    .render(inner, buf);
                return;
            }
            if self.list_loading {
                let message = if self.current_source() == Some(SourceId::Bili) {
                    "加载哔哩哔哩收藏夹..."
                } else {
                    "加载热门歌单..."
                };
                render_muted(message, inner, buf, ctx);
                return;
            }
            if self.list_loaded && self.playlists.is_empty() {
                let message = if self.current_source().is_none() {
                    "暂无收藏歌单"
                } else if self.current_source() == Some(SourceId::Bili) {
                    "暂无收藏夹，或尚未登录哔哩哔哩"
                } else {
                    "该音源暂无热门歌单"
                };
                render_muted(message, inner, buf, ctx);
                return;
            }
        }
        if inner.height == 0 || self.playlists.is_empty() {
            return;
        }

        let visible_height = inner.height as usize;
        if self.selected_playlist.is_none() {
            ensure_visible(
                self.selected,
                visible_height,
                self.playlists.len(),
                &mut self.playlist_scroll_offset,
            );
        }
        for index in self.playlist_scroll_offset
            ..(self.playlist_scroll_offset + visible_height).min(self.playlists.len())
        {
            let playlist = &self.playlists[index];
            let favorite = ctx.storage.is_favorite_playlist(playlist);
            let prefix = if favorite { "* " } else { "  " };
            let details = if playlist.song_count > 0 {
                format!(" · {} 首", playlist.song_count)
            } else {
                String::new()
            };
            let text = truncate_chars(
                &format!("{prefix}{}{}", playlist.name, details),
                inner.width as usize,
            );
            let style = if self.selected_playlist.is_none() && index == self.selected {
                Style::new()
                    .bg(crate::theme::accent(ctx))
                    .fg(crate::theme::selection_fg(ctx))
                    .add_modifier(Modifier::BOLD)
            } else if self.selected_playlist == Some(index) {
                Style::new()
                    .fg(crate::theme::accent(ctx))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(crate::theme::text(ctx))
            };
            Paragraph::new(Line::from(Span::styled(text, style))).render(
                Rect::new(
                    inner.x,
                    inner.y + (index - self.playlist_scroll_offset) as u16,
                    inner.width,
                    1,
                ),
                buf,
            );
        }
    }

    fn render_songs(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        let title = self
            .current_playlist()
            .map(|playlist| format!("{} · {}", source_name(playlist.source), playlist.name))
            .unwrap_or_else(|| "歌曲列表".to_string());
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(crate::theme::border(ctx)))
            .title(title);
        let inner = block.inner(area);
        block.render(area, buf);

        if self.selected_playlist.is_none() {
            render_muted("选择一个歌单", inner, buf, ctx);
            return;
        }
        if let Some(error) = &self.error_message {
            Paragraph::new(format!("加载失败: {error}"))
                .style(Style::new().fg(crate::theme::red(ctx)))
                .render(inner, buf);
            return;
        }
        if self.songs_loading {
            render_muted("加载歌单歌曲...", inner, buf, ctx);
            return;
        }
        if self.songs_loaded && self.songs.is_empty() {
            render_muted("该歌单暂无歌曲", inner, buf, ctx);
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
        for index in self.song_scroll_offset
            ..(self.song_scroll_offset + visible_height).min(self.songs.len())
        {
            let text =
                super::components::song_table::row(&self.songs[index], index, list_area.width);
            let style = if index == self.selected {
                Style::new()
                    .bg(crate::theme::accent(ctx))
                    .fg(crate::theme::selection_fg(ctx))
                    .add_modifier(Modifier::BOLD)
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

    fn toggle_favorite(&mut self, ctx: &AppContext) -> AppAction {
        let Some(playlist) = self
            .current_playlist()
            .or_else(|| self.playlists.get(self.selected))
            .cloned()
        else {
            return AppAction::None;
        };
        if ctx.storage.is_favorite_playlist(&playlist) {
            ctx.storage.remove_favorite_playlist(&playlist);
            if self.current_source().is_none() {
                if self.selected_playlist.is_some() {
                    self.leave_playlist();
                }
                self.sync_favorites(ctx);
            }
            AppAction::ShowNotification(Notification::info("已取消收藏歌单"))
        } else {
            ctx.storage.add_favorite_playlist(&playlist);
            AppAction::ShowNotification(Notification::info("已收藏歌单"))
        }
    }

    fn enter_selected_playlist(&mut self) {
        if self.selected_playlist.is_some() || self.selected >= self.playlists.len() {
            return;
        }
        let playlist_index = self.selected;
        let playlist = &self.playlists[playlist_index];
        let cache_key = (playlist.source, playlist.id.clone());
        self.selected_playlist = Some(playlist_index);
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

    fn leave_playlist(&mut self) {
        let playlist_index = self.selected_playlist.take().unwrap_or_default();
        self.songs.clear();
        self.songs_loaded = false;
        self.songs_loading = false;
        self.error_message = None;
        self.selected = playlist_index.min(self.playlists.len().saturating_sub(1));
        self.song_scroll_offset = 0;
    }

    fn refresh_current(&mut self, ctx: &AppContext) {
        self.error_message = None;
        if let Some(playlist) = self.current_playlist() {
            self.song_cache
                .remove(&(playlist.source, playlist.id.clone()));
            self.songs.clear();
            self.songs_loaded = false;
            self.songs_loading = false;
            self.selected = 0;
            self.song_scroll_offset = 0;
        } else if let Some(source) = self.current_source() {
            self.list_cache.remove(&source);
            self.playlists.clear();
            self.list_loaded = false;
            self.list_loading = false;
            self.selected = 0;
            self.playlist_scroll_offset = 0;
        } else {
            self.sync_favorites(ctx);
        }
    }

    fn select_previous_scope(&mut self, ctx: &AppContext) {
        if self.scopes.is_empty() {
            return;
        }
        let index = if self.scope_index == 0 {
            self.scopes.len() - 1
        } else {
            self.scope_index - 1
        };
        self.select_scope(index, ctx);
    }

    fn select_next_scope(&mut self, ctx: &AppContext) {
        if self.scopes.is_empty() {
            return;
        }
        self.select_scope((self.scope_index + 1) % self.scopes.len(), ctx);
    }

    fn select_scope(&mut self, index: usize, ctx: &AppContext) {
        if index >= self.scopes.len() || index == self.scope_index {
            return;
        }
        self.scope_index = index;
        self.selected_playlist = None;
        self.songs.clear();
        self.songs_loaded = false;
        self.songs_loading = false;
        self.error_message = None;
        self.selected = 0;
        self.playlist_scroll_offset = 0;
        self.song_scroll_offset = 0;
        if let Some(source) = self.current_source() {
            if let Some(playlists) = self.list_cache.get(&source) {
                self.playlists = playlists.clone();
                self.list_loaded = true;
                self.list_loading = false;
            } else {
                self.playlists.clear();
                self.list_loaded = false;
                self.list_loading = false;
            }
        } else {
            self.playlists = ctx.storage.load_favorite_playlists();
            self.list_loaded = true;
            self.list_loading = false;
        }
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
        if self.selected_playlist.is_some() {
            self.songs.len()
        } else {
            self.playlists.len()
        }
    }
}

struct PageChunks {
    scopes: Rect,
    playlists: Rect,
    songs: Rect,
}

fn page_chunks(area: Rect, playlist_count: usize) -> PageChunks {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);
    let content = if area.width < 82 {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length((playlist_count as u16 + 2).clamp(5, 11)),
                Constraint::Min(0),
            ])
            .split(vertical[1])
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(34), Constraint::Min(0)])
            .split(vertical[1])
    };
    PageChunks {
        scopes: vertical[0],
        playlists: content[0],
        songs: content[1],
    }
}

fn scope_tab_rects(area: Rect, count: usize) -> std::rc::Rc<[Rect]> {
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

fn render_muted(text: &str, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
    Paragraph::new(text)
        .style(Style::new().fg(crate::theme::muted(ctx)))
        .render(area, buf);
}

fn source_name(source: SourceId) -> &'static str {
    match source {
        SourceId::Kw => "酷我",
        SourceId::Kg => "酷狗",
        SourceId::Tx => "QQ",
        SourceId::Wy => "网易云",
        SourceId::Mg => "咪咕",
        SourceId::Bili => "哔哩哔哩",
        SourceId::Local => "本地",
    }
}

fn scope_label(source: Option<SourceId>, full: bool) -> &'static str {
    match (source, full) {
        (None, _) => "已收藏",
        (Some(SourceId::Kw), true) => "酷我 kw",
        (Some(SourceId::Kg), true) => "酷狗 kg",
        (Some(SourceId::Tx), true) => "QQ tx",
        (Some(SourceId::Wy), true) => "网易 wy",
        (Some(SourceId::Mg), true) => "咪咕 mg",
        (Some(SourceId::Bili), true) => "哔哩哔哩 bili",
        (Some(SourceId::Local), true) => "本地 local",
        (Some(source), false) => source.as_str(),
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
    use super::{PlaylistLoadRequest, PlaylistsPage};
    use lx_core::model::playlist::Playlist;
    use lx_core::model::source::SourceId;

    fn playlist(id: &str, source: SourceId) -> Playlist {
        Playlist {
            id: id.to_string(),
            name: id.to_string(),
            source,
            cover_url: None,
            song_count: 0,
            description: None,
            play_count: None,
        }
    }

    #[test]
    fn source_lists_are_cached_independently() {
        let mut page = PlaylistsPage::new(vec![SourceId::Kw, SourceId::Kg]);
        page.scope_index = 1;
        page.list_loaded = false;
        assert_eq!(
            page.next_load_request(),
            Some(PlaylistLoadRequest::List {
                source: SourceId::Kw
            })
        );
        page.update_playlists(SourceId::Kw, vec![playlist("kw-1", SourceId::Kw)]);
        page.scope_index = 2;
        page.list_loaded = false;
        assert_eq!(
            page.next_load_request(),
            Some(PlaylistLoadRequest::List {
                source: SourceId::Kg
            })
        );
    }
}
