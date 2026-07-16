use lx_core::model::source::PlayerState;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::context::AppContext;

pub fn render(area: Rect, buf: &mut Buffer, ctx: &AppContext) {
    let accent = crate::theme::accent(ctx);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(crate::theme::border(ctx)));
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height == 0 {
        return;
    }

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(24.min(inner.width / 3)),
            Constraint::Min(10),
            Constraint::Length(28.min(inner.width / 3)),
        ])
        .split(inner);
    let state = *ctx.player_state.borrow();
    let state_label = match state {
        PlayerState::Playing => "PLAYING",
        PlayerState::Paused => "PAUSED",
        PlayerState::Loading => "LOADING",
        PlayerState::Stopped => "STOPPED",
        PlayerState::Idle => "IDLE",
    };
    let position = *ctx.position.borrow();
    let duration = *ctx.duration.borrow();
    Paragraph::new(vec![
        Line::from(Span::styled(
            format!("[{}]", state_label),
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )),
        Line::from(format!(
            "{} / {}",
            format_duration(position),
            format_duration(duration)
        )),
    ])
    .render(columns[0], buf);

    let song = ctx.current_song.read().unwrap();
    let (title, detail) = song.as_ref().map_or_else(
        || ("暂无播放".to_string(), "使用搜索添加歌曲".to_string()),
        |song| {
            (
                song.name.clone(),
                format!(
                    "{}{}",
                    song.singer,
                    if song.album_name.trim().is_empty() {
                        String::new()
                    } else {
                        format!(" - {}", song.album_name)
                    }
                ),
            )
        },
    );
    Paragraph::new(vec![
        Line::from(Span::styled(
            title,
            Style::new()
                .fg(crate::theme::text(ctx))
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            detail,
            Style::new().fg(crate::theme::muted(ctx)),
        )),
    ])
    .alignment(Alignment::Center)
    .render(columns[1], buf);

    let source = song
        .as_ref()
        .map(|song| song.source.as_str())
        .unwrap_or("-");
    Paragraph::new(vec![
        Line::from(Span::styled(
            format!("Volume: {:>3}%", ctx.player.volume()),
            Style::new().fg(accent),
        )),
        Line::from(format!(
            "{} · {} · {}",
            ctx.playlist.mode().label(),
            source,
            if ctx.source_manager.has_js_source() {
                "SOURCE OK"
            } else {
                "SOURCE OFF"
            }
        )),
    ])
    .alignment(Alignment::Right)
    .render(columns[2], buf);
}

fn format_duration(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs();
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}
