//! 底部状态栏

use lx_core::model::source::{PlayerState, Quality};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::context::AppContext;

pub fn render(area: Rect, buf: &mut Buffer, ctx: &AppContext) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let background = crate::theme::mantle(ctx);
    Block::default()
        .style(Style::new().bg(background).fg(crate::theme::text(ctx)))
        .render(area, buf);

    let state = *ctx.player_state.borrow();
    let current_song = ctx.current_song.read().unwrap();
    let position = *ctx.position.borrow();
    let duration = *ctx.duration.borrow();
    let volume = ctx.player.volume();
    let (queue, queue_index) = ctx.playlist.snapshot();
    let quality = ctx.config.read().unwrap().player.quality;

    let (state_text, state_color) = match state {
        PlayerState::Playing => ("播放", crate::theme::green(ctx)),
        PlayerState::Paused => ("暂停", crate::theme::yellow(ctx)),
        PlayerState::Loading => ("缓冲", crate::theme::sapphire(ctx)),
        PlayerState::Stopped => ("停止", crate::theme::overlay1(ctx)),
        PlayerState::Idle => ("空闲", crate::theme::overlay1(ctx)),
    };
    let time = if duration.is_zero() {
        format_duration(position)
    } else {
        format!(
            "{}/{}",
            format_duration(position),
            format_duration(duration)
        )
    };
    let song = current_song.as_ref().map_or_else(
        || "voicefox".to_string(),
        |song| {
            let value = if song.singer.trim().is_empty() {
                song.name.clone()
            } else {
                format!("{} - {}", song.name, song.singer)
            };
            truncate(&value, song_width(area.width))
        },
    );
    let source = current_song
        .as_ref()
        .map(|song| song.source.as_str())
        .unwrap_or("-");
    let source_online = ctx.source_manager.has_js_source();
    let queue_position = if queue.is_empty() {
        "0/0".to_string()
    } else {
        format!("{}/{}", queue_index.saturating_add(1), queue.len())
    };
    let mode = ctx.playlist.mode().label();

    let mut spans = vec![
        Span::styled(
            format!(" {} ", state_text),
            Style::new()
                .fg(state_color)
                .bg(crate::theme::surface0(ctx))
                .add_modifier(Modifier::BOLD),
        ),
        separator(ctx, background),
        Span::styled(
            song,
            Style::new()
                .fg(crate::theme::text(ctx))
                .bg(background)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    if area.width >= 60 {
        spans.extend([
            separator(ctx, background),
            Span::styled(
                time,
                Style::new().fg(crate::theme::subtext1(ctx)).bg(background),
            ),
        ]);
    }
    if area.width >= 90 {
        spans.extend([
            separator(ctx, background),
            Span::styled(
                format!("音量 {}%", volume),
                Style::new().fg(crate::theme::sky(ctx)).bg(background),
            ),
            separator(ctx, background),
            Span::styled(
                mode,
                Style::new().fg(crate::theme::lavender(ctx)).bg(background),
            ),
        ]);
    }
    if area.width >= 122 {
        spans.extend([
            separator(ctx, background),
            Span::styled(
                format!("{} {}", source, quality_label(quality)),
                Style::new().fg(crate::theme::peach(ctx)).bg(background),
            ),
            separator(ctx, background),
            Span::styled(
                format!("队列 {}", queue_position),
                Style::new().fg(crate::theme::teal(ctx)).bg(background),
            ),
        ]);
    }
    if area.width >= 140 {
        spans.extend([
            separator(ctx, background),
            Span::styled(
                if source_online {
                    "自定义音源在线"
                } else {
                    "自定义音源离线"
                },
                Style::new()
                    .fg(if source_online {
                        crate::theme::green(ctx)
                    } else {
                        crate::theme::maroon(ctx)
                    })
                    .bg(background),
            ),
        ]);
    }

    Paragraph::new(Line::from(spans))
        .style(Style::new().bg(background))
        .render(Rect::new(area.x, area.y, area.width, 1), buf);
}

fn separator(ctx: &AppContext, background: ratatui::style::Color) -> Span<'static> {
    Span::styled(
        "  ·  ",
        Style::new().fg(crate::theme::overlay0(ctx)).bg(background),
    )
}

fn song_width(width: u16) -> usize {
    match width {
        0..=59 => width.saturating_sub(16) as usize,
        _ => 28,
    }
}

fn truncate(value: &str, width: usize) -> String {
    if UnicodeWidthStr::width(value) <= width {
        return value.to_string();
    }
    if width <= 1 {
        return "…".chars().take(width).collect();
    }
    let mut result = String::new();
    let mut rendered = 0;
    for character in value.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if rendered + character_width > width - 1 {
            break;
        }
        result.push(character);
        rendered += character_width;
    }
    result.push('…');
    result
}

fn quality_label(quality: Quality) -> &'static str {
    match quality {
        Quality::Low128 => "128K",
        Quality::High320 => "320K",
        Quality::Flac => "FLAC",
        Quality::Flac24 => "Hi-Res",
    }
}

fn format_duration(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs();
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}
