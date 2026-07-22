//! 收藏页面

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lx_core::events::{AppAction, InsertPosition};
use lx_core::model::song::SongInfo;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::context::AppContext;

pub struct FavoritesPage {
    selected: usize,
    scroll: usize,
    query: String,
    search_mode: bool,
    viewport_height: usize,
}

impl FavoritesPage {
    pub fn new() -> Self {
        Self {
            selected: 0,
            scroll: 0,
            query: String::new(),
            search_mode: false,
            viewport_height: 1,
        }
    }

    pub fn input_mode(&self) -> bool {
        self.search_mode
    }

    pub fn handle_input(&mut self, key: &KeyEvent, ctx: &AppContext) -> AppAction {
        if self.search_mode {
            match (key.modifiers, key.code) {
                (KeyModifiers::NONE, KeyCode::Esc) => {
                    self.search_mode = false;
                    self.query.clear();
                    self.selected = 0;
                    self.scroll = 0;
                }
                (KeyModifiers::NONE, KeyCode::Enter) => {
                    self.search_mode = false;
                }
                (_, KeyCode::Backspace)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.query.pop();
                    self.selected = 0;
                    self.scroll = 0;
                }
                (_, KeyCode::Char(character))
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.query.push(character);
                    self.selected = 0;
                    self.scroll = 0;
                }
                _ => {}
            }
            return AppAction::None;
        }

        let favorites = ctx.storage.load_favorites();
        let filtered = self.filtered_indices(&favorites);
        self.clamp_selection(filtered.len());
        let half_page = (self.viewport_height / 2).max(1);

        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Char('/')) => {
                self.search_mode = true;
            }
            (KeyModifiers::NONE, KeyCode::Esc) => {
                if self.query.is_empty() {
                    return AppAction::GoBack;
                }
                self.query.clear();
                self.selected = 0;
                self.scroll = 0;
            }
            (KeyModifiers::NONE, KeyCode::Up) | (KeyModifiers::NONE, KeyCode::Char('k')) => {
                if !filtered.is_empty() {
                    if self.selected > 0 {
                        self.selected -= 1;
                    } else if ctx.config.read().unwrap().ui.wrap_navigation {
                        self.selected = filtered.len().saturating_sub(1);
                    }
                }
            }
            (KeyModifiers::NONE, KeyCode::Down) | (KeyModifiers::NONE, KeyCode::Char('j')) => {
                if !filtered.is_empty() {
                    if self.selected + 1 < filtered.len() {
                        self.selected += 1;
                    } else if ctx.config.read().unwrap().ui.wrap_navigation {
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
                        .collect();
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
            (KeyModifiers::NONE, KeyCode::Char('d'))
            | (KeyModifiers::NONE, KeyCode::Delete)
            | (KeyModifiers::CONTROL, KeyCode::Char('l')) => {
                if let Some(original_index) = filtered.get(self.selected).copied()
                    && let Some(song) = favorites.get(original_index)
                    && ctx.storage.remove_favorite(song)
                {
                    let remaining = ctx.storage.load_favorites();
                    let remaining_len = self.filtered_indices(&remaining).len();
                    self.clamp_selection(remaining_len);
                    return AppAction::ShowNotification(lx_core::events::Notification::info(
                        "已取消收藏",
                    ));
                }
            }
            _ => {}
        }
        AppAction::None
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        let favorites = ctx.storage.load_favorites();
        let filtered = self.filtered_indices(&favorites);
        self.clamp_selection(filtered.len());

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(crate::theme::border(ctx)))
            .title(format!(
                " 收藏 {}/{} · / 筛选 ",
                filtered.len(),
                favorites.len()
            ));
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height == 0 {
            return;
        }

        let show_search = self.search_mode || !self.query.is_empty();
        let mut cursor_y = inner.y;
        if show_search {
            let mode = if self.search_mode { "INSERT" } else { "FILTER" };
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(" / {} ", mode),
                    Style::new()
                        .fg(crate::theme::selection_fg(ctx))
                        .bg(crate::theme::mauve(ctx))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {} ", self.query),
                    Style::new()
                        .fg(crate::theme::text(ctx))
                        .bg(crate::theme::surface0(ctx)),
                ),
            ]))
            .style(Style::new().bg(crate::theme::surface0(ctx)))
            .render(Rect::new(inner.x, cursor_y, inner.width, 1), buf);
            cursor_y = cursor_y.saturating_add(1);
        }

        if cursor_y >= inner.bottom() {
            return;
        }
        Paragraph::new(Line::from(Span::styled(
            super::components::song_table::header(inner.width),
            Style::new()
                .fg(crate::theme::subtext0(ctx))
                .add_modifier(Modifier::BOLD),
        )))
        .render(Rect::new(inner.x, cursor_y, inner.width, 1), buf);
        cursor_y = cursor_y.saturating_add(1);

        let list = Rect::new(
            inner.x,
            cursor_y,
            inner.width,
            inner.bottom().saturating_sub(cursor_y),
        );
        self.viewport_height = list.height.max(1) as usize;

        if favorites.is_empty() {
            Paragraph::new("暂无收藏，在播放时按 Ctrl+L 添加")
                .style(Style::new().fg(crate::theme::muted(ctx)))
                .render(list, buf);
            return;
        }
        if filtered.is_empty() {
            Paragraph::new(format!("没有匹配“{}”的歌曲", self.query))
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
            Paragraph::new(Line::from(Span::styled(
                super::components::song_table::row(song, filtered_index, list.width),
                style,
            )))
            .render(Rect::new(list.x, list.y + row as u16, list.width, 1), buf);
        }
    }

    pub fn handle_mouse(
        &mut self,
        event: MouseEvent,
        area: Rect,
        ctx: &AppContext,
        activate: bool,
    ) -> AppAction {
        let favorites = ctx.storage.load_favorites();
        let filtered = self.filtered_indices(&favorites);
        let scroll_amount = ctx.config.read().unwrap().ui.scroll_amount.max(1);
        match event.kind {
            MouseEventKind::ScrollUp => {
                self.selected = self.selected.saturating_sub(scroll_amount);
            }
            MouseEventKind::ScrollDown => {
                self.selected =
                    (self.selected + scroll_amount).min(filtered.len().saturating_sub(1));
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let inner = Block::default().borders(Borders::ALL).inner(area);
                let search_height = u16::from(self.search_mode || !self.query.is_empty());
                if search_height == 1 && event.row == inner.y {
                    self.search_mode = true;
                    return AppAction::None;
                }
                let list_y = inner.y.saturating_add(search_height).saturating_add(1);
                if event.row >= list_y && event.row < inner.bottom() {
                    let selected = self.scroll + event.row.saturating_sub(list_y) as usize;
                    if selected < filtered.len() {
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
            }
            _ => {}
        }
        AppAction::None
    }

    fn filtered_indices(&self, favorites: &[SongInfo]) -> Vec<usize> {
        let query = self.query.trim().to_lowercase();
        favorites
            .iter()
            .enumerate()
            .filter_map(|(index, song)| {
                let matches = query.is_empty()
                    || song.name.to_lowercase().contains(&query)
                    || song.singer.to_lowercase().contains(&query)
                    || song.album_name.to_lowercase().contains(&query)
                    || song.source.as_str().contains(&query);
                matches.then_some(index)
            })
            .collect()
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
}

#[cfg(test)]
mod tests {
    use super::FavoritesPage;
    use lx_core::model::song::SongInfo;
    use lx_core::model::source::SourceId;

    #[test]
    fn filters_title_artist_album_and_source() {
        let mut page = FavoritesPage::new();
        let mut song = SongInfo::new("1".into(), SourceId::Kw, "晴天".into(), "周杰伦".into());
        song.album_name = "叶惠美".into();
        let songs = vec![song];

        for query in ["晴天", "周杰伦", "叶惠美", "kw"] {
            page.query = query.into();
            assert_eq!(page.filtered_indices(&songs), vec![0]);
        }
    }
}
