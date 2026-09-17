//! Shared visual chrome for content pages.
//!
//! 使用 section-title 风格，而不是每个页面都套一个带 All Borders 的 Block。

use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders};

use crate::context::AppContext;

pub fn card<'a>(ctx: &AppContext, title: impl Into<Line<'a>>) -> Block<'a> {
    Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::new().fg(crate::theme::surface1(ctx)))
        .style(Style::new().bg(crate::theme::base(ctx)))
        .title(title)
}

pub fn focused_card<'a>(ctx: &AppContext, title: impl Into<Line<'a>>) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(crate::theme::accent(ctx)))
        .style(Style::new().bg(crate::theme::base(ctx)))
        .title(title)
}
