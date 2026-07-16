//! 收藏页面

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lx_core::events::AppAction;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::context::AppContext;

pub fn render(
    area: Rect,
    buf: &mut Buffer,
    ctx: &AppContext,
    selected: &mut usize,
    scroll: &mut usize,
) {
    let favs = ctx.storage.load_favorites();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(crate::theme::border(ctx)))
        .title(format!("收藏 ({} 首)", favs.len()));

    let inner = block.inner(area);
    block.render(area, buf);

    if favs.is_empty() {
        Paragraph::new(Line::from(Span::styled(
            "暂无收藏，在主页面按 Ctrl+L 收藏当前歌曲",
            Style::new().fg(crate::theme::muted(ctx)),
        )))
        .render(inner, buf);
        return;
    }

    // 确保 selected 不越界
    if *selected >= favs.len() {
        *selected = 0;
    }

    let selected_style = Style::new().bg(crate::theme::accent(ctx)).fg(Color::Black);
    let normal_style = Style::new().fg(crate::theme::text(ctx));

    if inner.height == 0 {
        return;
    }
    Paragraph::new(Line::from(Span::styled(
        super::components::song_table::header(inner.width),
        Style::new().fg(crate::theme::muted(ctx)),
    )))
    .render(Rect::new(inner.x, inner.y, inner.width, 1), buf);
    let list = Rect::new(
        inner.x,
        inner.y.saturating_add(1),
        inner.width,
        inner.height.saturating_sub(1),
    );
    let visible_height = list.height as usize;
    if visible_height == 0 {
        return;
    }
    let total = favs.len();

    // 自动调整 scroll
    if *selected >= *scroll + visible_height {
        *scroll = selected.saturating_sub(visible_height - 1);
    } else if *selected < *scroll {
        *scroll = *selected;
    }
    *scroll = (*scroll).min(total.saturating_sub(visible_height));

    let end = (*scroll + visible_height).min(total);
    for i in *scroll..end {
        let row = i - *scroll;
        if row as u16 >= list.height {
            break;
        }
        let song = &favs[i];
        let text = super::components::song_table::row(song, i, list.width);
        let line_area = Rect::new(list.x, list.y + row as u16, list.width, 1);
        let style = if i == *selected {
            selected_style
        } else {
            normal_style
        };
        Paragraph::new(Line::from(Span::styled(text, style))).render(line_area, buf);
    }
}

pub fn handle_input(key: &KeyEvent, ctx: &AppContext, selected: &mut usize) -> AppAction {
    let favs = ctx.storage.load_favorites();

    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Up) | (KeyModifiers::NONE, KeyCode::Char('k')) => {
            if !favs.is_empty() {
                if *selected > 0 {
                    *selected -= 1;
                } else if ctx.config.read().unwrap().ui.wrap_navigation {
                    *selected = favs.len().saturating_sub(1);
                }
            }
        }
        (KeyModifiers::NONE, KeyCode::Down) | (KeyModifiers::NONE, KeyCode::Char('j')) => {
            if !favs.is_empty() {
                if *selected + 1 < favs.len() {
                    *selected += 1;
                } else if ctx.config.read().unwrap().ui.wrap_navigation {
                    *selected = 0;
                }
            }
        }
        (KeyModifiers::NONE, KeyCode::Enter) | (KeyModifiers::NONE, KeyCode::Char('\r')) => {
            if !favs.is_empty() && *selected < favs.len() {
                let songs = favs.clone();
                let index = *selected;
                return AppAction::PlaySong { songs, index };
            }
        }
        (KeyModifiers::NONE, KeyCode::Char('d')) => {
            if !favs.is_empty() && *selected < favs.len() {
                let song = &favs[*selected];
                ctx.storage.remove_favorite(song);
                if *selected >= favs.len().saturating_sub(1) {
                    *selected = favs.len().saturating_sub(2);
                }
            }
        }
        _ => {}
    }
    AppAction::None
}

pub fn handle_mouse(
    event: MouseEvent,
    area: Rect,
    ctx: &AppContext,
    selected: &mut usize,
    scroll: usize,
    activate: bool,
) -> AppAction {
    let favs = ctx.storage.load_favorites();
    let scroll_amount = ctx.config.read().unwrap().ui.scroll_amount.max(1);
    match event.kind {
        MouseEventKind::ScrollUp => {
            *selected = selected.saturating_sub(scroll_amount);
        }
        MouseEventKind::ScrollDown => {
            *selected = (*selected + scroll_amount).min(favs.len().saturating_sub(1));
        }
        MouseEventKind::Down(MouseButton::Left) => {
            let inner = Block::default().borders(Borders::ALL).inner(area);
            let list_y = inner.y.saturating_add(1);
            if event.row >= list_y && event.row < inner.bottom() {
                let index = scroll + event.row.saturating_sub(list_y) as usize;
                if index < favs.len() {
                    *selected = index;
                    if activate {
                        return AppAction::PlaySong { songs: favs, index };
                    }
                }
            }
        }
        _ => {}
    }
    AppAction::None
}
