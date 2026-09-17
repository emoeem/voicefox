//! Shared visual chrome for content pages.

use ratatui::style::Style;
use ratatui::widgets::{Block, Borders};

use crate::context::AppContext;

/// Standard content card used by the modern TUI pages.
///
/// Keeping borders/backgrounds here prevents each page from slowly drifting
/// into a different visual language as features are added.
pub fn card<'a>(ctx: &AppContext, title: impl Into<ratatui::text::Line<'a>>) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(crate::theme::surface1(ctx)))
        .style(Style::new().bg(crate::theme::base(ctx)))
        .title(title)
}

/// Strongly focused card for the active panel/input.
pub fn focused_card<'a>(ctx: &AppContext, title: impl Into<ratatui::text::Line<'a>>) -> Block<'a> {
    card(ctx, title).border_style(Style::new().fg(crate::theme::accent(ctx)))
}
