//! PlayerBar：统一 Geometry 驱动的播放器底栏。
//!  row0: 当前歌曲 / 艺术家 / 专辑 / 音源 / 音质
//!  row1: 播放进度 + 时间
//!  row2: 播放模式 / 上一首 / 播放 / 下一首 / 音量

use lx_core::model::source::PlayerState;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};
use std::time::Duration;

use crate::context::AppContext;

#[derive(Debug, Clone, Copy)]
pub struct PlayerAreas {
    pub song: Rect,
    pub progress: Rect,
    pub controls: Rect,
    pub meta: Rect,
}

pub fn areas(area: Rect) -> PlayerAreas {
    let inner = Block::default().borders(Borders::TOP).inner(area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);
    PlayerAreas {
        song: rows[0],
        progress: rows[1],
        controls: rows[2],
        meta: rows[3],
    }
}

pub fn render(area: Rect, buf: &mut Buffer, ctx: &AppContext) {
    if area.height < 4 || area.width == 0 {
        return;
    }
    let bg = crate::theme::base(ctx);
    let border = crate::theme::surface1(ctx);
    Block::default()
        .borders(Borders::TOP)
        .border_style(Style::new().fg(border).bg(bg))
        .style(Style::new().bg(bg))
        .render(area, buf);
    let a = areas(area);
    render_song(a.song, buf, ctx, bg);
    render_progress(a.progress, buf, ctx, bg);
    render_controls(a.controls, buf, ctx, bg);
    render_meta(a.meta, buf, ctx, bg);
}

fn render_meta(area: Rect, buf: &mut Buffer, ctx: &AppContext, bg: ratatui::style::Color) {
    if area.width == 0 {
        return;
    }
    let queue = ctx.playlist.borrow();
    let current = ctx.playlist.current_index();
    let mode = ctx.playlist.mode().label();
    let state = match *ctx.player_state.borrow() {
        PlayerState::Playing => "播放中",
        PlayerState::Loading => "缓冲",
        PlayerState::Paused => "暂停",
        _ => "待机",
    };
    let favorite = ctx
        .current_song
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|song| ctx.storage.is_favorite(song));
    let text = format!(
        " 队列 {}/{}  ·  {mode}  ·  {state}  ·  {}  ·  {}",
        if queue.is_empty() { 0 } else { current + 1 },
        queue.len(),
        if favorite { "已收藏" } else { "未收藏" },
        if ctx.downloads.snapshot().is_empty() {
            "无下载任务"
        } else {
            "下载中"
        }
    );
    Paragraph::new(Line::from(Span::styled(
        truncate(&text, area.width as usize),
        Style::new().fg(crate::theme::overlay1(ctx)).bg(bg),
    )))
    .render(area, buf);
}

fn render_song(area: Rect, buf: &mut Buffer, ctx: &AppContext, bg: ratatui::style::Color) {
    if area.width == 0 {
        return;
    }
    let song = ctx.current_song.read().unwrap_or_else(|e| e.into_inner());
    let text = crate::theme::text(ctx);
    let sub = crate::theme::subtext0(ctx);
    let accent = crate::theme::accent(ctx);
    let muted = crate::theme::overlay1(ctx);
    let (title, singer, album, source, quality) = song.as_ref().map_or_else(
        || {
            (
                "暂无播放".to_string(),
                "".to_string(),
                "".to_string(),
                "-".to_string(),
                "-".to_string(),
            )
        },
        |s| {
            (
                s.name.clone(),
                s.singer.clone(),
                s.album_name.clone(),
                s.source.display_name().to_string(),
                s.quality_label(),
            )
        },
    );
    let state = match *ctx.player_state.borrow() {
        PlayerState::Playing => "播放中",
        PlayerState::Loading => "缓冲中",
        PlayerState::Paused => "已暂停",
        _ => "待机",
    };
    let mut spans = vec![Span::styled(
        truncate(&title, (area.width as usize).saturating_sub(8)),
        Style::new().fg(text).bg(bg).add_modifier(Modifier::BOLD),
    )];
    let mut add = |label: String, color| {
        if unicode_width::UnicodeWidthStr::width(
            spans
                .iter()
                .map(|x| x.content.as_ref())
                .collect::<String>()
                .as_str(),
        ) < area.width as usize
        {
            spans.push(Span::styled(label, Style::new().fg(color).bg(bg)));
        }
    };
    if !singer.is_empty() {
        add(format!("  ·  {singer}"), sub);
    }
    if !album.is_empty() {
        add(format!("  ·  {album}"), muted);
    }
    add(format!("  ·  {source} · {quality}"), accent);
    add(format!("  [{state}]"), sub);
    let mut line = Line::from(spans);
    let width = unicode_width::UnicodeWidthStr::width(
        line.spans
            .iter()
            .map(|x| x.content.as_ref())
            .collect::<String>()
            .as_str(),
    );
    if width < area.width as usize {
        line.spans.push(Span::styled(
            " ".repeat(area.width as usize - width),
            Style::new().bg(bg),
        ));
    }
    Paragraph::new(line).render(area, buf);
}

fn render_progress(area: Rect, buf: &mut Buffer, ctx: &AppContext, bg: ratatui::style::Color) {
    if area.width < 12 {
        return;
    }
    let position = *ctx.position.borrow();
    let duration = *ctx.duration.borrow();
    let accent = crate::theme::accent(ctx);
    let dim = crate::theme::surface1(ctx);
    let sub = crate::theme::subtext1(ctx);
    let label_w = 5u16;
    let bar_width = area.width.saturating_sub(label_w * 2 + 2).max(1);
    let ratio = if duration.is_zero() {
        0.0
    } else {
        (position.as_secs_f64() / duration.as_secs_f64()).clamp(0.0, 1.0)
    };
    let marker = (ratio * f64::from(bar_width.saturating_sub(1))) as usize;
    let empty = usize::from(bar_width).saturating_sub(marker + 1);
    let line = Line::from(vec![
        Span::styled(
            format!("{:>5} ", fmt(position)),
            Style::new().fg(sub).bg(bg),
        ),
        Span::styled("━".repeat(marker), Style::new().fg(accent).bg(bg)),
        Span::styled(
            "●",
            Style::new().fg(accent).bg(bg).add_modifier(Modifier::BOLD),
        ),
        Span::styled("─".repeat(empty), Style::new().fg(dim).bg(bg)),
        Span::styled(
            format!(" {:<5}", fmt(duration)),
            Style::new().fg(sub).bg(bg),
        ),
    ]);
    Paragraph::new(line).render(area, buf);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlHit {
    Mode,
    Previous,
    PlayPause,
    Next,
    Volume,
}

#[derive(Debug, Clone, Copy)]
struct ControlSpec {
    hit: ControlHit,
    width: u16,
}

fn control_specs() -> [ControlSpec; 5] {
    [
        ControlSpec {
            hit: ControlHit::Mode,
            width: 8,
        },
        ControlSpec {
            hit: ControlHit::Previous,
            width: 8,
        },
        ControlSpec {
            hit: ControlHit::PlayPause,
            width: 8,
        },
        ControlSpec {
            hit: ControlHit::Next,
            width: 8,
        },
        ControlSpec {
            hit: ControlHit::Volume,
            width: 13,
        },
    ]
}

fn control_rects(area: Rect) -> [(ControlHit, Rect); 5] {
    let specs = control_specs();
    let mut x = area.x;
    let mut result = [(ControlHit::Mode, Rect::default()); 5];
    for (i, spec) in specs.into_iter().enumerate() {
        let width = spec.width.min(area.right().saturating_sub(x));
        result[i] = (spec.hit, Rect::new(x, area.y, width, 1));
        x = x.saturating_add(width);
    }
    result
}

pub fn control_at(area: Rect, column: u16) -> Option<ControlHit> {
    control_rects(area)
        .into_iter()
        .find_map(|(hit, rect)| rect.contains((column, area.y).into()).then_some(hit))
}

pub fn volume_width(area: Rect) -> u16 {
    control_rects(area)
        .into_iter()
        .find_map(|(hit, rect)| (hit == ControlHit::Volume).then_some(rect.width))
        .unwrap_or(0)
}

pub fn volume_offset(area: Rect, column: u16) -> Option<u16> {
    control_rects(area).into_iter().find_map(|(hit, rect)| {
        (hit == ControlHit::Volume && rect.contains((column, area.y).into()))
            .then_some(column.saturating_sub(rect.x))
    })
}

fn render_controls(area: Rect, buf: &mut Buffer, ctx: &AppContext, bg: ratatui::style::Color) {
    if area.width == 0 {
        return;
    }
    let state = *ctx.player_state.borrow();
    let mode = ctx.playlist.mode();
    let volume = ctx.player.volume();
    let sub = crate::theme::subtext0(ctx);
    let accent = crate::theme::accent(ctx);
    let play_color = match state {
        PlayerState::Playing => crate::theme::yellow(ctx),
        PlayerState::Loading => crate::theme::blue(ctx),
        _ => crate::theme::green(ctx),
    };
    let mode_label = match mode {
        crate::playlist::mode::PlayMode::Random => "随机",
        crate::playlist::mode::PlayMode::SingleLoop => "单曲",
        crate::playlist::mode::PlayMode::ListLoop => "列表",
        _ => "顺序",
    };
    let play_label = if matches!(state, PlayerState::Playing) {
        "暂停"
    } else {
        "播放"
    };
    let labels = [
        format!(" {mode_label} "),
        "  <<  ".to_string(),
        format!("  {play_label}  "),
        "  >>  ".to_string(),
        format!("  音量 {:>3}%  ", volume),
    ];
    let rects = control_rects(area);
    let mut spans = Vec::new();
    for ((_, rect), label) in rects.into_iter().zip(labels) {
        let width = rect.width as usize;
        let text = if unicode_width::UnicodeWidthStr::width(label.as_str()) > width {
            truncate(&label, width)
        } else {
            format!("{label:<width$}")
        };
        let color = if label.contains(play_label) {
            play_color
        } else if label.contains(mode_label) {
            accent
        } else {
            sub
        };
        spans.push(Span::styled(text, Style::new().fg(color).bg(bg)));
    }
    let used: usize = spans
        .iter()
        .map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref()))
        .sum();
    if used < area.width as usize {
        spans.push(Span::styled(
            " ".repeat(area.width as usize - used),
            Style::new().bg(bg),
        ));
    }
    Paragraph::new(Line::from(spans)).render(area, buf);
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
    if duration.is_zero() || area.width == 0 || column < area.x || column >= area.right() {
        return None;
    }
    let label_width = 5u16;
    let bar_width = area.width.saturating_sub(label_width * 2 + 2).max(1);
    let bar_x = area.x + label_width + 1;
    let offset = column
        .saturating_sub(bar_x)
        .min(bar_width.saturating_sub(1));
    let ratio = f64::from(offset) / f64::from(bar_width.saturating_sub(1).max(1));
    Some(Duration::from_secs_f64(
        duration.as_secs_f64() * ratio.clamp(0.0, 1.0),
    ))
}
