//! 底部状态栏

use lx_core::model::source::PlayerState;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::context::AppContext;

pub fn render(area: Rect, buf: &mut Buffer, ctx: &AppContext) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let state = *ctx.player_state.borrow();
    let current_song = ctx.current_song.read().unwrap();
    let position = *ctx.position.borrow();
    let duration = *ctx.duration.borrow();

    let vol = ctx.player.volume();

    let (state_text, state_color) = match state {
        PlayerState::Playing => ("播放中", Color::Green),
        PlayerState::Paused => ("已暂停", Color::Yellow),
        PlayerState::Loading => ("加载中", crate::theme::accent(ctx)),
        PlayerState::Stopped => ("已停止", Color::DarkGray),
        PlayerState::Idle => ("空闲", Color::DarkGray),
    };

    let song_info = if let Some(s) = current_song.as_ref() {
        format!("{} - {}  [{}]", s.name, s.singer, s.source.as_str())
    } else {
        "欢迎使用 voicefox | 按 / 搜索歌曲".to_string()
    };

    let time_str = if duration > std::time::Duration::ZERO {
        format!(
            "{} / {}",
            format_duration(position),
            format_duration(duration)
        )
    } else {
        format_duration(position)
    };

    let mode = ctx.playlist.mode().label();
    let source_state = if ctx.source_manager.has_js_source() {
        "音源在线"
    } else {
        "音源离线"
    };
    let detail = if area.width >= 100 {
        format!(
            "  {}  ·  音量 {}%  ·  {}  ·  {}  ·  {}",
            song_info, vol, time_str, mode, source_state
        )
    } else if area.width >= 68 {
        format!("  {}  ·  {}  ·  {}%  ·  {}", song_info, time_str, vol, mode)
    } else {
        format!("  {}  ·  {}%  ·  {}", time_str, vol, mode)
    };
    let background = Color::Rgb(28, 31, 36);
    let line = Line::from(vec![
        Span::styled(
            format!(" {} ", state_text),
            Style::new()
                .fg(state_color)
                .bg(background)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(detail, Style::new().fg(Color::Gray).bg(background)),
    ]);

    Paragraph::new(line)
        .style(Style::new().bg(background))
        .render(Rect::new(area.x, area.y, area.width, 1), buf);
}

fn format_duration(d: std::time::Duration) -> String {
    let total_secs = d.as_secs();
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{:02}:{:02}", mins, secs)
}
