//! 底部状态栏

use lx_core::keybinding::{Action, KeybindingConfig};
use lx_core::model::config::StatusBarItem;
use lx_core::model::source::PlayerState;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::context::AppContext;

pub fn render(
    area: Rect,
    buf: &mut Buffer,
    ctx: &AppContext,
    sort_status: Option<&'static str>,
    page_scope: &str,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let background = crate::theme::mantle(ctx);
    Block::default()
        .style(Style::new().bg(background).fg(crate::theme::text(ctx)))
        .render(area, buf);

    let state = *ctx.player_state.borrow();
    let current_song = ctx.current_song.read().unwrap_or_else(|e| e.into_inner());
    let position = *ctx.position.borrow();
    let duration = *ctx.duration.borrow();
    let audio_info = ctx.audio_info.borrow().clone();
    let volume = ctx.player.volume();
    let queue = ctx.playlist.borrow();
    let queue_index = ctx.playlist.current_index();
    let (quality, status_bar_items, keybindings) = {
        let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
        (
            config.player.quality,
            config.ui.status_bar_items.clone(),
            config.keybindings.clone(),
        )
    };

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
    let mut song = current_song.as_ref().map_or_else(
        || "voicefox".to_string(),
        |song| {
            if song.singer.trim().is_empty() {
                song.name.clone()
            } else {
                format!("{} - {}", song.name, song.singer)
            }
        },
    );
    let source = current_song
        .as_ref()
        .map(|song| {
            // 锁中毒时退回原始数据渲染，避免状态栏每帧 panic
            let js_index = *ctx
                .play_js_source_index
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            js_index
                .and_then(|index| ctx.source_manager.js_source_name(index))
                .or_else(|| {
                    ctx.source_manager
                        .get(song.source)
                        .map(|source| source.name().to_string())
                })
                .unwrap_or_else(|| song.source.as_str().to_string())
        })
        .unwrap_or_else(|| "-".to_string());
    let source_online = ctx.source_manager.has_js_source();
    let show_quality_item = status_bar_items.contains(&StatusBarItem::Quality);
    if !show_quality_item && let Some(audio_label) = audio_info.label() {
        song.push_str(&format!(" · {audio_label}"));
    }
    let queue_position = if queue.is_empty() {
        "0/0".to_string()
    } else {
        format!("{}/{}", queue_index.saturating_add(1), queue.len())
    };
    let mode = ctx.playlist.mode().label();

    let total_width = area.width as usize;
    let mut used_width = 0;
    let mut spans = Vec::new();

    // 有下载任务时，把进度放在状态栏最前面，保证切换页面时也能看到。
    if let Some(text) = download_indicator(ctx) {
        append_segment(
            &mut spans,
            &mut used_width,
            total_width,
            text,
            Style::new()
                .fg(crate::theme::teal(ctx))
                .bg(background)
                .add_modifier(Modifier::BOLD),
            ctx,
            background,
        );
    }

    for item in status_bar_items {
        let remaining = remaining_segment_width(used_width, total_width, spans.is_empty());
        let segment = match item {
            StatusBarItem::State => Some((
                format!(" {} ", state_text),
                Style::new()
                    .fg(state_color)
                    .bg(crate::theme::surface0(ctx))
                    .add_modifier(Modifier::BOLD),
            )),
            StatusBarItem::Source => {
                let source_width = remaining
                    .saturating_sub(UnicodeWidthStr::width("音源 "))
                    .min(match area.width {
                        0..=49 => 8,
                        50..=89 => 14,
                        _ => 20,
                    });
                (source_width > 0).then(|| {
                    (
                        format!("音源 {}", truncate(&source, source_width)),
                        Style::new()
                            .fg(crate::theme::peach(ctx))
                            .bg(background)
                            .add_modifier(Modifier::BOLD),
                    )
                })
            }
            StatusBarItem::Sort => sort_status.map(|sort_status| {
                (
                    format!("排序 {} (s)", sort_status),
                    Style::new()
                        .fg(crate::theme::yellow(ctx))
                        .bg(background)
                        .add_modifier(Modifier::BOLD),
                )
            }),
            StatusBarItem::Song => (remaining > 0).then(|| {
                (
                    truncate(&song, remaining.min(28)),
                    Style::new()
                        .fg(crate::theme::text(ctx))
                        .bg(background)
                        .add_modifier(Modifier::BOLD),
                )
            }),
            StatusBarItem::Time => Some((
                time.clone(),
                Style::new().fg(crate::theme::subtext1(ctx)).bg(background),
            )),
            StatusBarItem::Volume => Some((
                format!("音量 {}%", volume),
                Style::new().fg(crate::theme::sky(ctx)).bg(background),
            )),
            StatusBarItem::PlayMode => Some((
                mode.to_string(),
                Style::new().fg(crate::theme::lavender(ctx)).bg(background),
            )),
            StatusBarItem::Quality => Some((
                audio_info
                    .label()
                    .unwrap_or_else(|| quality.label().to_string()),
                Style::new().fg(crate::theme::peach(ctx)).bg(background),
            )),
            StatusBarItem::Queue => Some((
                format!("队列 {}", queue_position),
                Style::new().fg(crate::theme::teal(ctx)).bg(background),
            )),
            StatusBarItem::JsSourceState => Some((
                if source_online {
                    "自定义音源在线".to_string()
                } else {
                    "自定义音源离线".to_string()
                },
                Style::new()
                    .fg(if source_online {
                        crate::theme::green(ctx)
                    } else {
                        crate::theme::maroon(ctx)
                    })
                    .bg(background),
            )),
        };
        if let Some((text, style)) = segment {
            append_segment(
                &mut spans,
                &mut used_width,
                total_width,
                text,
                style,
                ctx,
                background,
            );
        }
    }

    let hint = page_hint(page_scope, &keybindings);
    if !hint.is_empty() {
        append_segment(
            &mut spans,
            &mut used_width,
            total_width,
            hint,
            Style::new().fg(crate::theme::overlay1(ctx)).bg(background),
            ctx,
            background,
        );
    }

    Paragraph::new(Line::from(spans))
        .style(Style::new().bg(background))
        .render(Rect::new(area.x, area.y, area.width, 1), buf);
}

fn configured_key(config: &KeybindingConfig, page: &str, action: Action, fallback: &str) -> String {
    config
        .pages
        .get(page)
        .and_then(|bindings| bindings.get(&action))
        .map(|key| key.as_str())
        .unwrap_or(fallback)
        .to_string()
}

fn page_hint(page: &str, config: &KeybindingConfig) -> String {
    match page {
        "main" => format!(
            "{} / {} 上下 · Enter 播放 · a 加队尾 · D 清空",
            configured_key(config, page, Action::ListSelectUp, "k"),
            configured_key(config, page, Action::ListSelectDown, "j")
        ),
        "search" => format!(
            "{} 搜索 · {} 播放 · {} 收藏 · {} 下载 · ←/→ 音源",
            configured_key(config, page, Action::SearchInputMode, "i"),
            configured_key(config, page, Action::ListActivate, "l"),
            configured_key(config, page, Action::ListToggleFavorite, "f"),
            configured_key(config, page, Action::ListDownload, "D"),
        ),
        "leaderboard" | "playlists" => format!(
            "{} / {} 导航 · Enter 进入/播放 · a 加队列 · {} 下载 · Esc 返回",
            configured_key(config, page, Action::ListSelectUp, "k"),
            configured_key(config, page, Action::ListSelectDown, "j"),
            configured_key(config, page, Action::ListDownload, "D"),
        ),
        "favorites" => format!(
            "{} / {} 导航 · Enter 播放 · {} 收藏 · {} 下载 · {} 排序 · / 筛选",
            configured_key(config, page, Action::ListSelectUp, "k"),
            configured_key(config, page, Action::ListSelectDown, "j"),
            configured_key(config, page, Action::ListToggleFavorite, "f"),
            configured_key(config, page, Action::ListDownload, "D"),
            configured_key(config, page, Action::ListCycleSort, "s"),
        ),
        "history" => format!(
            "{} / {} 导航 · Enter 播放 · {} 下载 · {} 排序 · / 筛选 · D 清空",
            configured_key(config, page, Action::ListSelectUp, "k"),
            configured_key(config, page, Action::ListSelectDown, "j"),
            configured_key(config, page, Action::ListDownload, "D"),
            configured_key(config, page, Action::ListCycleSort, "s"),
        ),
        "local" => format!(
            "{} / {} 导航 · Enter 播放 · {} 加队列 · {} 排序 · {} 扫描 · {} 删除 · / 筛选",
            configured_key(config, page, Action::ListSelectUp, "k"),
            configured_key(config, page, Action::ListSelectDown, "j"),
            configured_key(config, page, Action::ListAddToQueue, "a"),
            configured_key(config, page, Action::ListCycleSort, "s"),
            configured_key(config, page, Action::LocalRescan, "r"),
            configured_key(config, page, Action::LocalDelete, "d"),
        ),
        "downloads" => {
            "↑/k ↓/j 导航 · d 取消/移除 · x 清理完成 · C 清空历史 · Esc 返回".to_string()
        }
        "sources" => "←/→ 分类 · s 切换区域 · a 添加 · d 删除 · h 检测 · l 导航栏透明".to_string(),
        "settings" => "←/→ 分类 · s 切换区域 · 1-0 切换页面 · Esc 取消输入".to_string(),
        _ => String::new(),
    }
}

fn separator(ctx: &AppContext, background: ratatui::style::Color) -> Span<'static> {
    Span::styled(
        "  ·  ",
        Style::new().fg(crate::theme::overlay0(ctx)).bg(background),
    )
}

/// 状态栏上的下载进度摘要；没有进行中的任务时返回 `None`。
fn download_indicator(ctx: &AppContext) -> Option<String> {
    let tasks = ctx.downloads.snapshot();
    let active: Vec<_> = tasks.iter().filter(|task| task.state.is_active()).collect();
    if active.is_empty() {
        return None;
    }
    let ratios: Vec<f64> = active
        .iter()
        .filter_map(|task| task.progress.ratio())
        .collect();
    let percent = if ratios.is_empty() {
        None
    } else {
        let average = ratios.iter().sum::<f64>() / ratios.len() as f64;
        Some((average * 100.0).round() as u32)
    };
    Some(match percent {
        Some(percent) => format!("下载 {} 项 {percent}% (Ctrl+o)", active.len()),
        None => format!("下载 {} 项 (Ctrl+o)", active.len()),
    })
}

fn separator_width() -> usize {
    UnicodeWidthStr::width("  ·  ")
}

#[allow(clippy::too_many_arguments)]
fn append_segment<'a>(
    spans: &mut Vec<Span<'a>>,
    used_width: &mut usize,
    total_width: usize,
    text: String,
    style: Style,
    ctx: &AppContext,
    background: ratatui::style::Color,
) -> bool {
    let text_width = UnicodeWidthStr::width(text.as_str());
    let separator_width = if spans.is_empty() {
        0
    } else {
        separator_width()
    };
    let required = separator_width.saturating_add(text_width);
    if text_width == 0 || used_width.saturating_add(required) > total_width {
        return false;
    }
    if !spans.is_empty() {
        spans.push(separator(ctx, background));
    }
    spans.push(Span::styled(text, style));
    *used_width += required;
    true
}

fn remaining_segment_width(used_width: usize, total_width: usize, first: bool) -> usize {
    total_width.saturating_sub(used_width + if first { 0 } else { separator_width() })
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

fn format_duration(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs();
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}
