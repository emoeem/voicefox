use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use super::geometry::{TabRect, tab_rects, truncate_width};
use crate::context::AppContext;

pub fn rects<I, S>(area: Rect, labels: I, max_width: usize) -> Vec<TabRect>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    tab_rects(area, labels, 2, max_width)
}

pub fn render(
    area: Rect,
    labels: &[String],
    selected: usize,
    max_width: usize,
    ctx: &AppContext,
    buf: &mut Buffer,
) {
    let surface = crate::theme::surface0(ctx);
    let accent = crate::theme::accent(ctx);
    let selected_fg = crate::theme::selection_fg(ctx);
    let muted = crate::theme::muted(ctx);
    let tabs = rects(area, labels.iter(), max_width);
    let mut spans = Vec::new();
    for (i, tab) in tabs.iter().enumerate() {
        let label = labels
            .get(tab.index)
            .map(|value| truncate_width(value, max_width))
            .unwrap_or_default();
        if i > 0 {
            spans.push(Span::styled("  ", Style::new().bg(surface)));
        }
        let style = if tab.index == selected {
            Style::new()
                .bg(accent)
                .fg(selected_fg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().bg(surface).fg(muted)
        };
        spans.push(Span::styled(format!(" {label} "), style));
    }
    Paragraph::new(Line::from(spans))
        .style(Style::new().bg(surface))
        .render(Rect::new(area.x, area.y, area.width, 1), buf);
}
