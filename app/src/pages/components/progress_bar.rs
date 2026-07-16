//! 播放进度条

use crate::context::AppContext;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use std::time::Duration;

fn format_duration(d: Duration) -> String {
    let total_secs = d.as_secs();
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{:02}:{:02}", mins, secs)
}

pub fn render(area: Rect, buf: &mut Buffer, ctx: &AppContext) {
    let accent = crate::theme::accent(ctx);
    if area.height == 0 || area.width == 0 {
        return;
    }
    let position = *ctx.position.borrow();
    let duration = *ctx.duration.borrow();

    if duration == Duration::ZERO {
        return;
    }

    let ratio = (position.as_secs_f64() / duration.as_secs_f64()).clamp(0.0, 1.0);
    if area.width < 18 {
        Paragraph::new(format!(
            "{}/{}",
            format_duration(position),
            format_duration(duration)
        ))
        .style(Style::new().fg(Color::Gray))
        .render(area, buf);
        return;
    }
    let bar_width = area.width.saturating_sub(16) as usize;
    let filled = (bar_width as f64 * ratio) as usize;
    let empty = bar_width.saturating_sub(filled);

    let bar_spans = vec![
        Span::styled(
            format!(" {} ", format_duration(position)),
            Style::new().fg(Color::Gray),
        ),
        Span::styled("█".repeat(filled), Style::new().fg(accent)),
        Span::styled("░".repeat(empty), Style::new().fg(Color::DarkGray)),
        Span::styled(
            format!(" {}", format_duration(duration)),
            Style::new().fg(Color::Gray),
        ),
    ];

    Paragraph::new(Line::from(bar_spans)).render(area, buf);
}
