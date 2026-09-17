//! rmpc 风格播放队列页面。

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lx_core::events::{AppAction, Notification};
use lx_core::keybinding::{Action, KeybindingResolver};
use lx_core::model::source::PlayerState;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::context::AppContext;
use crate::cover::{CoverGeometry, CoverRenderer, CoverState};

/// “D 清空整个队列”的确认窗口：首次按下武装，窗口内再按一次才执行。
const CLEAR_QUEUE_CONFIRM_WINDOW: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueueEditCommand {
    MoveUp,
    MoveDown,
    RemoveSelected,
    Clear,
}

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

pub struct MainPage {
    selected: usize,
    scroll: usize,
    dragging: Option<usize>,
    cover: CoverRenderer,
    /// “D 清空整个队列”的武装时刻，确认窗口外或 Esc 后解除
    clear_armed: Option<Instant>,
}

impl MainPage {
    pub fn new(cover: CoverRenderer) -> Self {
        Self {
            selected: 0,
            scroll: 0,
            dragging: None,
            cover,
            clear_armed: None,
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

    pub fn handle_input(
        &mut self,
        key: &KeyEvent,
        ctx: &AppContext,
        resolver: &KeybindingResolver,
    ) -> AppAction {
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
                    ctx.playlist.clear();
                    ctx.stop_player();
                    ctx.cover_service.clear();
                    ctx.lyric_service.clear();
                    *ctx.current_song.write().unwrap_or_else(|e| e.into_inner()) = None;
                    self.selected = 0;
                    self.scroll = 0;
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
                    if len != 0 {
                        self.selected = if self.selected == 0 {
                            if ctx
                                .config
                                .read()
                                .unwrap_or_else(|e| e.into_inner())
                                .ui
                                .wrap_navigation
                            {
                                len - 1
                            } else {
                                0
                            }
                        } else {
                            self.selected - 1
                        };
                    }
                    return AppAction::None;
                }
                Action::ListSelectDown => {
                    if len != 0 {
                        self.selected = if self.selected + 1 < len {
                            self.selected + 1
                        } else if ctx
                            .config
                            .read()
                            .unwrap_or_else(|e| e.into_inner())
                            .ui
                            .wrap_navigation
                        {
                            0
                        } else {
                            self.selected
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
                    self.selected = self.selected.saturating_sub(5);
                    return AppAction::None;
                }
                Action::ListPageDown => {
                    self.selected = (self.selected + 5).min(len.saturating_sub(1));
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
                self.selected = self.selected.saturating_sub(5);
            }
            (KeyModifiers::CONTROL, KeyCode::Char('d'))
            | (KeyModifiers::NONE, KeyCode::PageDown) => {
                self.selected = (self.selected + 5).min(len.saturating_sub(1));
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
        if area.width >= 72 {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(36), Constraint::Percentage(64)])
                .split(area);
            // 封面框高度由封面比例决定，歌词占满剩余高度，但至少保住 MIN_HEIGHT。
            // 关闭封面时左栏全部用于歌词
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
            let cover_height = geometry.map_or(0, |geometry| {
                geometry.box_height(
                    columns[0].width,
                    columns[0]
                        .height
                        .saturating_sub(super::components::lyric::MIN_HEIGHT),
                )
            });
            let left = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(cover_height), Constraint::Min(0)])
                .split(columns[0]);
            if let Some(geometry) = geometry
                && cover_height > 0
            {
                self.render_cover(left[0], buf, ctx, geometry);
            }
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
        let scroll_amount = ctx
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .ui
            .scroll_amount
            .max(1);
        let mut play_songs = None;
        let mut drag_target = None;
        {
            // 只读阶段：从队列快照中取出本次事件需要的少量信息。
            let songs = ctx.playlist.borrow();
            let current = ctx.playlist.current_index();
            match event.kind {
                MouseEventKind::ScrollUp => {
                    self.dragging = None;
                    self.selected = self.selected.saturating_sub(scroll_amount);
                }
                MouseEventKind::ScrollDown => {
                    self.dragging = None;
                    self.selected =
                        (self.selected + scroll_amount).min(songs.len().saturating_sub(1));
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(index) = queue_index_at(event, area, self.scroll, songs.len()) {
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
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some(from) = self.dragging {
                        drag_target = queue_index_at(event, area, self.scroll, songs.len())
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
        if let (Some(from), Some(target)) = (self.dragging, drag_target) {
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
        event: MouseEvent,
        area: Rect,
        ctx: &AppContext,
    ) -> Option<(Vec<lx_core::model::song::SongInfo>, usize)> {
        let songs = ctx.playlist.borrow();
        let index = queue_index_at(event, area, self.scroll, songs.len())?;
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

    fn render_queue(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        let accent = crate::theme::accent(ctx);
        // 借用队列快照，每帧渲染不再复制整张播放列表。
        let songs = ctx.playlist.borrow();
        let current = ctx.playlist.current_index();
        let block = super::components::chrome::card(ctx, format!(" 队列 · {} 歌曲 ", songs.len()));
        let inner = block.inner(area);
        block.render(area, buf);
        if songs.is_empty() {
            Paragraph::new("队列为空 · 按 2 搜索歌曲并播放")
                .style(Style::new().fg(crate::theme::muted(ctx)))
                .render(inner, buf);
            return;
        }
        self.selected = self.selected.min(songs.len().saturating_sub(1));
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

        let playing = ctx.current_song.read().unwrap_or_else(|e| e.into_inner());
        let playing_source_name = playing
            .as_ref()
            .map(|s| s.source.display_name().to_string())
            .unwrap_or_default();
        let state = *ctx.player_state.borrow();
        let is_playing = matches!(state, PlayerState::Playing);

        for (row, index) in (self.scroll..songs.len().min(self.scroll + visible)).enumerate() {
            let song = &songs[index];
            let is_current = index == current;
            let is_selected = index == self.selected;

            let marker = if is_current {
                if is_playing { "▶ " } else { "♪ " }
            } else if is_selected {
                "▌ "
            } else {
                "  "
            };

            let mut spans: Vec<Span<'static>> = Vec::new();
            let marker_style = if is_current {
                Style::new()
                    .fg(crate::theme::green(ctx))
                    .add_modifier(Modifier::BOLD)
            } else if is_selected {
                Style::new().fg(accent).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(crate::theme::overlay0(ctx))
            };
            spans.push(Span::styled(marker, marker_style));

            let row_style = if is_current {
                let same_source = song.source.display_name() == playing_source_name.as_str();
                if same_source {
                    Style::new().fg(accent).add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(crate::theme::blue(ctx))
                }
            } else if is_selected {
                Style::new()
                    .fg(crate::theme::text(ctx))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(crate::theme::text(ctx))
            };

            let text =
                super::components::song_table::row(song, index, list.width.saturating_sub(2));
            spans.push(Span::styled(text, row_style));

            Paragraph::new(Line::from(spans))
                .render(Rect::new(list.x, list.y + row as u16, list.width, 1), buf);
        }
    }
}

fn queue_index_at(event: MouseEvent, area: Rect, scroll: usize, len: usize) -> Option<usize> {
    let queue_area = queue_area(area);
    let inner = Block::default().borders(Borders::ALL).inner(queue_area);
    let list_y = inner.y.saturating_add(1);
    if event.column < inner.x
        || event.column >= inner.right()
        || event.row < list_y
        || event.row >= inner.bottom()
    {
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

impl MainPage {
    /// 绘制封面框，无法绘制封面时退回文字占位
    fn render_cover(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        ctx: &AppContext,
        geometry: CoverGeometry,
    ) {
        let block = super::components::chrome::card(ctx, " 封面 ");
        let inner = block.inner(area);
        block.render(area, buf);

        self.cover.sync(ctx.cover_service.image_path().as_deref());
        if self.cover.render(geometry.image_rect(inner), buf) {
            return;
        }
        render_cover_text(inner, buf, ctx);
    }
}

fn render_cover_text(inner: Rect, buf: &mut Buffer, ctx: &AppContext) {
    let cover_state = ctx.cover_service.state();
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
                    match &cover_state {
                        CoverState::Loading => "封面加载中...",
                        CoverState::Unavailable(_) => "封面不可用",
                        // current_song 非 None 但无封面 <-> 封面被禁用
                        CoverState::Empty => "",
                        // 封面就绪但是终端无法显示
                        CoverState::Ready => "封面无法显示",
                    },
                    Style::new().fg(crate::theme::muted(ctx)),
                )),
                match &cover_state {
                    CoverState::Unavailable(error) => Line::from(Span::styled(
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

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{QueueEditCommand, queue_edit_command};

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
}
