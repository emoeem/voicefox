//! 歌词显示组件
//!
//! 对标 go-musicfox internal/ui/lyric.go

use ratatui::buffer::Buffer;
use ratatui::layout::Alignment;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::context::AppContext;

/// 渲染歌词显示
/// area: 可用区域
/// 显示当前行前后各 N 行，使当前行尽量居中
pub fn render(area: Rect, buf: &mut Buffer, ctx: &AppContext) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(crate::theme::border(ctx)))
        .title(" 歌词 ");
    let inner = block.inner(area);
    block.render(area, buf);

    let state = ctx.lyric_service.current_state();

    if state.is_empty || state.lines.is_empty() {
        let text = "♫  暂无歌词";
        let y = inner.y + inner.height / 2;
        Paragraph::new(text)
            .style(Style::new().fg(crate::theme::muted(ctx)))
            .alignment(Alignment::Center)
            .render(Rect::new(inner.x, y, inner.width, 1), buf);
        return;
    }

    let current = state.current_line;
    let visible_rows = inner.height as usize;
    if visible_rows == 0 {
        return;
    }

    let current_row = visible_rows / 2;
    let translation_visible = state.translation.is_some() && current_row + 1 < visible_rows;
    let start = current.saturating_sub(current_row);
    let end = (current + visible_rows + 1).min(state.lines.len());

    for line_idx in start..end {
        let relative = line_idx as isize - current as isize;
        let row = if relative <= 0 {
            current_row as isize + relative
        } else {
            current_row as isize + relative + isize::from(translation_visible)
        };
        if row < 0 || row >= visible_rows as isize {
            continue;
        }
        let y = inner.y + row as u16;

        let line = &state.lines[line_idx];
        let distance = (line_idx as isize - current as isize).unsigned_abs();

        let (prefix, style) = if line_idx == current {
            (
                "❯ ",
                Style::new()
                    .fg(crate::theme::accent(ctx))
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            let gray = match distance {
                1 => Color::Rgb(180, 180, 180),
                2 => Color::Rgb(140, 140, 140),
                3..=4 => Color::Rgb(100, 100, 100),
                _ => Color::Rgb(70, 70, 70),
            };
            ("  ", Style::new().fg(gray))
        };

        let text = format!(
            "{}{}",
            prefix,
            truncate(&line.text, inner.width.saturating_sub(2) as usize)
        );
        Paragraph::new(Line::from(Span::styled(text, style)))
            .alignment(Alignment::Center)
            .render(Rect::new(inner.x, y, inner.width, 1), buf);
    }

    if translation_visible && let Some(ref translation) = state.translation {
        let y = inner.y + current_row as u16 + 1;
        let text = truncate(translation, inner.width as usize);
        Paragraph::new(Line::from(Span::styled(
            text,
            Style::new()
                .fg(Color::Rgb(183, 138, 82))
                .add_modifier(Modifier::ITALIC),
        )))
        .alignment(Alignment::Center)
        .render(Rect::new(inner.x, y, inner.width, 1), buf);
    }
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if UnicodeWidthStr::width(s) <= max {
        return s.to_string();
    }
    if max <= 1 {
        return "…".chars().take(max).collect();
    }
    let mut result = String::new();
    let mut width = 0;
    for ch in s.chars() {
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + char_width > max - 1 {
            break;
        }
        result.push(ch);
        width += char_width;
    }
    result.push('…');
    result
}
