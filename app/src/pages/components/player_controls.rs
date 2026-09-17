//! PlayerBar：底部 3 行播放控制条。
//!  row0: 歌名 - 艺人  (大字居中)
//!  row1: 进度条 + 时间戳
//!  row2: 控制按钮 | 音量 | 来源·音质

use lx_core::model::source::PlayerState;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};
use std::time::Duration;

use crate::context::AppContext;

pub fn render(area: Rect, buf: &mut Buffer, ctx: &AppContext) {
    if area.height < 3 || area.width == 0 {
        return;
    }
    let bg = crate::theme::base(ctx);
    let border = crate::theme::surface1(ctx);
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::new().fg(border).bg(bg))
        .style(Style::new().bg(bg));
    let inner = block.inner(area);
    block.render(area, buf);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);

    render_song(rows[0], buf, ctx, bg);
    render_progress(rows[1], buf, ctx, bg);
    render_controls(rows[2], buf, ctx, bg);
}

fn render_song(area: Rect, buf: &mut Buffer, ctx: &AppContext, bg: ratatui::style::Color) {
    if area.width == 0 {
        return;
    }
    let song = ctx.current_song.read().unwrap_or_else(|e| e.into_inner());
    let accent = crate::theme::accent(ctx);
    let text = crate::theme::text(ctx);
    let sub = crate::theme::subtext0(ctx);

    let (title, singer) = song.as_ref().map_or_else(
        || ("—".to_string(), "暂无播放".to_string()),
        |s| (s.name.clone(), s.singer.clone()),
    );

    let mut row: Vec<Span<'static>> = Vec::new();
    row.push(Span::styled(
        truncate(
            &title,
            (area.width as usize).saturating_sub(singer.len() + 4),
        ),
        Style::new().fg(text).bg(bg).add_modifier(Modifier::BOLD),
    ));
    if !singer.is_empty() && area.width as usize > title.chars().count() + 3 {
        row.push(Span::styled(" — ", Style::new().fg(accent).bg(bg)));
        row.push(Span::styled(singer, Style::new().fg(sub).bg(bg)));
    }
    Paragraph::new(Line::from(row)).render(area, buf);
}

fn render_progress(area: Rect, buf: &mut Buffer, ctx: &AppContext, bg: ratatui::style::Color) {
    if area.width == 0 {
        return;
    }
    let position = *ctx.position.borrow();
    let duration = *ctx.duration.borrow();
    let accent = crate::theme::accent(ctx);
    let dim = crate::theme::surface1(ctx);
    let sub = crate::theme::subtext1(ctx);

    if duration.is_zero() {
        return;
    }

    let label_w = 6u16;
    let gap = 1u16;
    let bar_max = area.width.saturating_sub(label_w * 2 + gap * 2);
    let filled = ((position.as_secs_f64() / duration.as_secs_f64()).clamp(0.0, 1.0)
        * bar_max as f64) as usize;
    let empty = bar_max as usize - filled;

    let mut row: Vec<Span<'static>> = Vec::new();
    row.push(Span::styled(
        format!("{:>5}", fmt(position)),
        Style::new().fg(sub).bg(bg),
    ));
    row.push(Span::styled(" ", Style::new().bg(bg)));
    row.push(Span::styled(
        "━".repeat(filled),
        Style::new().fg(accent).bg(bg),
    ));
    row.push(Span::styled(
        "●",
        Style::new().fg(accent).bg(bg).add_modifier(Modifier::BOLD),
    ));
    row.push(Span::styled("─".repeat(empty), Style::new().fg(dim).bg(bg)));
    row.push(Span::styled(" ", Style::new().bg(bg)));
    row.push(Span::styled(
        format!("{:<5}", fmt(duration)),
        Style::new().fg(sub).bg(bg),
    ));
    Paragraph::new(Line::from(row)).render(area, buf);
}

fn render_controls(area: Rect, buf: &mut Buffer, ctx: &AppContext, bg: ratatui::style::Color) {
    if area.width == 0 {
        return;
    }
    let state = *ctx.player_state.borrow();
    let volume = ctx.player.volume();
    let sub = crate::theme::subtext0(ctx);
    let dim = crate::theme::overlay1(ctx);
    let accent = crate::theme::accent(ctx);
    let green = crate::theme::green(ctx);
    let yellow = crate::theme::yellow(ctx);
    let blue = crate::theme::blue(ctx);

    let play_icon = match state {
        PlayerState::Playing => "❚❚",
        PlayerState::Paused | PlayerState::Loading => "▶",
        PlayerState::Stopped | PlayerState::Idle => "▶",
    };
    let play_color = match state {
        PlayerState::Playing => yellow,
        PlayerState::Loading => blue,
        _ => green,
    };

    // 来源 + 音质（从 audio_info 取）
    let source_quality = {
        let audio = ctx.audio_info.borrow();
        let mut src = String::new();
        let mut q = String::new();
        {
            let song = ctx.current_song.read().unwrap_or_else(|e| e.into_inner());
            if let Some(s) = song.as_ref() {
                src = s.source.display_name().to_string();
            }
        }
        if let Some(bv) = audio.bitrate_kbps {
            q = format!("{}K", bv);
        } else if let Some(cf) = &audio.codec {
            q = cf.to_uppercase();
        }
        if q.is_empty() {
            src
        } else {
            format!("{src} · {q}")
        }
    };

    let mut row: Vec<Span<'static>> = Vec::new();

    let mode = ctx.playlist.mode();
    let mode_glyph = match mode {
        crate::playlist::mode::PlayMode::Random => "🔀",
        crate::playlist::mode::PlayMode::SingleLoop => "🔂",
        crate::playlist::mode::PlayMode::ListLoop => "🔁",
        crate::playlist::mode::PlayMode::List | crate::playlist::mode::PlayMode::None => "→",
    };
    let mode_color = match mode {
        crate::playlist::mode::PlayMode::Random
        | crate::playlist::mode::PlayMode::SingleLoop
        | crate::playlist::mode::PlayMode::ListLoop => accent,
        _ => sub,
    };
    row.push(Span::styled(
        format!(" {mode_glyph} "),
        Style::new().fg(mode_color).bg(bg),
    ));

    row.push(Span::styled("⏮ ", Style::new().fg(sub).bg(bg)));
    row.push(Span::styled(
        format!("{play_icon:>2} "),
        Style::new()
            .fg(play_color)
            .bg(bg)
            .add_modifier(Modifier::BOLD),
    ));
    row.push(Span::styled(" ⏭ ", Style::new().fg(sub).bg(bg)));
    row.push(Span::styled(" ", Style::new().bg(bg)));

    // 音量
    row.push(Span::styled(
        format!("🔊 {volume:>3}%"),
        Style::new().fg(crate::theme::sky(ctx)).bg(bg),
    ));

    // 计算剩余空间
    let used = row
        .iter()
        .map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref()))
        .sum::<usize>()
        + 2;
    let remaining = (area.width as usize).saturating_sub(used);

    if remaining >= 8 {
        // 右侧：来源·音质
        let src_q_display = if source_quality.chars().count() > remaining {
            let mut w = 0;
            let mut cut = source_quality.len();
            for (i, c) in source_quality.char_indices() {
                w += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
                if w >= remaining.saturating_sub(1) {
                    cut = i;
                    break;
                }
            }
            format!("{}…", &source_quality[..cut])
        } else {
            source_quality
        };
        row.push(Span::styled(
            format!("{:>width$}", src_q_display, width = remaining),
            Style::new().fg(accent).bg(bg).add_modifier(Modifier::BOLD),
        ));
    }

    Paragraph::new(Line::from(row)).render(area, buf);
    let _ = dim;
}

fn fmt(d: Duration) -> String {
    let t = d.as_secs();
    format!("{:02}:{:02}", t / 60, t % 60)
}

fn truncate(s: &str, max: usize) -> String {
    let width = unicode_width::UnicodeWidthStr::width(s);
    if width <= max {
        return s.to_string();
    }
    if max < 2 {
        return "…".repeat(max);
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > max - 1 {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

pub fn seek_position(area: Rect, column: u16, duration: Duration) -> Option<Duration> {
    if duration.is_zero() || area.width == 0 {
        return None;
    }
    let row1 = area.y.saturating_add(1);
    if area.height < 2 || column < area.x || column >= area.x + area.width {
        return None;
    }
    let label_width = 6u16;
    let gap = 1u16;
    let bar_max = area.width.saturating_sub(label_width * 2 + gap * 2);
    let bar_x = area.x + label_width + gap;
    let bar_width = bar_max.max(1);
    let offset = column
        .saturating_sub(bar_x)
        .min(bar_width.saturating_sub(1));
    let ratio = f64::from(offset) / f64::from(bar_width.saturating_sub(1).max(1));
    let _ = row1;
    Some(Duration::from_secs_f64(
        duration.as_secs_f64() * ratio.clamp(0.0, 1.0),
    ))
}
