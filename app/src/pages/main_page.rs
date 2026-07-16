//! rmpc 风格播放队列页面。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lx_core::events::AppAction;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::context::AppContext;

pub struct MainPage {
    selected: usize,
    scroll: usize,
    dragging: Option<usize>,
}

impl MainPage {
    pub fn new() -> Self {
        Self {
            selected: 0,
            scroll: 0,
            dragging: None,
        }
    }

    pub fn handle_input(&mut self, key: &KeyEvent, ctx: &AppContext) -> AppAction {
        let (songs, current) = ctx.playlist.snapshot();
        if self.selected >= songs.len() {
            self.selected = current.min(songs.len().saturating_sub(1));
        }
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Up) | (KeyModifiers::NONE, KeyCode::Char('k')) => {
                if !songs.is_empty() {
                    self.selected = if self.selected == 0 {
                        if ctx.config.read().unwrap().ui.wrap_navigation {
                            songs.len() - 1
                        } else {
                            0
                        }
                    } else {
                        self.selected - 1
                    };
                }
            }
            (KeyModifiers::NONE, KeyCode::Down) | (KeyModifiers::NONE, KeyCode::Char('j')) => {
                if !songs.is_empty() {
                    self.selected = if self.selected + 1 < songs.len() {
                        self.selected + 1
                    } else if ctx.config.read().unwrap().ui.wrap_navigation {
                        0
                    } else {
                        self.selected
                    };
                }
            }
            (KeyModifiers::NONE, KeyCode::Home) | (KeyModifiers::NONE, KeyCode::Char('g')) => {
                self.selected = 0;
            }
            (KeyModifiers::NONE, KeyCode::End)
            | (KeyModifiers::NONE, KeyCode::Char('G'))
            | (KeyModifiers::SHIFT, KeyCode::Char('G')) => {
                self.selected = songs.len().saturating_sub(1);
            }
            (KeyModifiers::CONTROL, KeyCode::Char('u')) | (KeyModifiers::NONE, KeyCode::PageUp) => {
                self.selected = self.selected.saturating_sub(5);
            }
            (KeyModifiers::CONTROL, KeyCode::Char('d'))
            | (KeyModifiers::NONE, KeyCode::PageDown) => {
                self.selected = (self.selected + 5).min(songs.len().saturating_sub(1));
            }
            (KeyModifiers::SHIFT, KeyCode::Up) | (KeyModifiers::SHIFT, KeyCode::Char('K')) => {
                if self.selected > 0 {
                    ctx.playlist.move_item(self.selected, self.selected - 1);
                    self.selected -= 1;
                }
            }
            (KeyModifiers::SHIFT, KeyCode::Down) | (KeyModifiers::SHIFT, KeyCode::Char('J')) => {
                if self.selected + 1 < songs.len() {
                    ctx.playlist.move_item(self.selected, self.selected + 1);
                    self.selected += 1;
                }
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                if self.selected < songs.len() {
                    return AppAction::PlaySong {
                        songs,
                        index: self.selected,
                    };
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('d')) => {
                let removing_current = self.selected == current;
                ctx.playlist.remove(self.selected);
                self.selected = self.selected.min(songs.len().saturating_sub(2));
                if removing_current {
                    let (remaining, next) = ctx.playlist.snapshot();
                    if remaining.is_empty() {
                        ctx.player.stop();
                        ctx.cover_service.clear();
                        ctx.lyric_service.clear();
                        *ctx.current_song.write().unwrap() = None;
                    } else {
                        return AppAction::PlaySong {
                            songs: remaining,
                            index: next,
                        };
                    }
                }
            }
            (KeyModifiers::SHIFT, KeyCode::Char('D')) => {
                ctx.playlist.clear();
                ctx.player.stop();
                ctx.cover_service.clear();
                ctx.lyric_service.clear();
                *ctx.current_song.write().unwrap() = None;
                self.selected = 0;
                self.scroll = 0;
            }
            _ => {}
        }
        AppAction::None
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        if area.width >= 72 {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(36), Constraint::Percentage(64)])
                .split(area);
            let left = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
                .split(columns[0]);
            render_cover_placeholder(left[0], buf, ctx);
            super::components::lyric::render(left[1], buf, ctx);
            self.render_queue(columns[1], buf, ctx);
        } else {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
                .split(area);
            self.render_queue(rows[0], buf, ctx);
            super::components::lyric::render(rows[1], buf, ctx);
        }
    }

    pub fn handle_mouse(
        &mut self,
        event: MouseEvent,
        area: Rect,
        ctx: &AppContext,
        activate: bool,
    ) -> AppAction {
        let (songs, current) = ctx.playlist.snapshot();
        let scroll_amount = ctx.config.read().unwrap().ui.scroll_amount.max(1);
        match event.kind {
            MouseEventKind::ScrollUp => {
                self.dragging = None;
                self.selected = self.selected.saturating_sub(scroll_amount);
            }
            MouseEventKind::ScrollDown => {
                self.dragging = None;
                self.selected = (self.selected + scroll_amount).min(songs.len().saturating_sub(1));
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(index) = queue_index_at(event, area, self.scroll, songs.len()) {
                    self.selected = index;
                    self.dragging = Some(index);
                    if activate {
                        self.dragging = None;
                        return AppAction::PlaySong { songs, index };
                    }
                } else {
                    self.dragging = None;
                    self.selected = current.min(songs.len().saturating_sub(1));
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(from) = self.dragging
                    && let Some(target) = queue_index_at(event, area, self.scroll, songs.len())
                    && from != target
                {
                    ctx.playlist.move_item(from, target);
                    self.selected = target;
                    self.dragging = Some(target);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.dragging = None;
            }
            _ => {}
        }
        AppAction::None
    }

    fn render_queue(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        let accent = crate::theme::accent(ctx);
        let (songs, current) = ctx.playlist.snapshot();
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(crate::theme::border(ctx)))
            .title(format!(" Queue · {} songs ", songs.len()));
        let inner = block.inner(area);
        block.render(area, buf);
        if songs.is_empty() {
            Paragraph::new("队列为空")
                .style(Style::new().fg(crate::theme::muted(ctx)))
                .render(inner, buf);
            return;
        }
        self.selected = self.selected.min(songs.len() - 1);
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
        self.scroll = self.scroll.min(songs.len().saturating_sub(visible));

        for (row, index) in (self.scroll..songs.len().min(self.scroll + visible)).enumerate() {
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
            Paragraph::new(Line::from(Span::styled(
                super::components::song_table::row(&songs[index], index, list.width),
                style,
            )))
            .render(Rect::new(list.x, list.y + row as u16, list.width, 1), buf);
        }
    }
}

fn queue_index_at(event: MouseEvent, area: Rect, scroll: usize, len: usize) -> Option<usize> {
    let queue_area = queue_area(area);
    let inner = Block::default().borders(Borders::ALL).inner(queue_area);
    let list_y = inner.y.saturating_add(1);
    if event.row < list_y || event.row >= inner.bottom() {
        return None;
    }
    let index = scroll + event.row.saturating_sub(list_y) as usize;
    (index < len).then_some(index)
}

fn queue_area(area: Rect) -> Rect {
    if area.width >= 72 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(36), Constraint::Percentage(64)])
            .split(area)[1]
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
            .split(area)[0]
    }
}

fn render_cover_placeholder(area: Rect, buf: &mut Buffer, ctx: &AppContext) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(crate::theme::border(ctx)))
        .title(" Cover ");
    let inner = block.inner(area);
    block.render(area, buf);
    if ctx.cover_service.has_image() {
        ctx.cover_service.render(inner, buf);
        return;
    }
    let cover_state = ctx.cover_service.state();
    let song = ctx.current_song.read().unwrap();
    let lines = song.as_ref().map_or_else(
        || {
            vec![
                Line::from(""),
                Line::from(Span::styled(
                    "NO COVER",
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
                    match &cover_state {
                        crate::cover::CoverState::Loading => "封面加载中...",
                        crate::cover::CoverState::Unavailable(_) => "封面不可用",
                        crate::cover::CoverState::Empty => "等待播放",
                        crate::cover::CoverState::Ready => "封面已就绪",
                    },
                    Style::new().fg(crate::theme::muted(ctx)),
                )),
                match &cover_state {
                    crate::cover::CoverState::Unavailable(error) => Line::from(Span::styled(
                        error.chars().take(inner.width as usize).collect::<String>(),
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
