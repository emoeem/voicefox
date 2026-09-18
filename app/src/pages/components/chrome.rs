//! Shared visual chrome for content pages.
//!
//! 使用 section-title 风格，而不是每个页面都套一个带 All Borders 的 Block。

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders};

use crate::context::AppContext;

fn card_layout<'a>(title: impl Into<Line<'a>>) -> Block<'a> {
    Block::default().borders(Borders::BOTTOM).title(title)
}

pub fn card<'a>(ctx: &AppContext, title: impl Into<Line<'a>>) -> Block<'a> {
    card_layout(title)
        .border_style(Style::new().fg(crate::theme::surface1(ctx)))
        .style(Style::new().bg(crate::theme::base(ctx)))
}

/// 返回 `card` 的内容区域：顶部标题和底部边框各占一行。
/// 鼠标命中计算与页面渲染共用这套布局。
pub fn card_inner(area: Rect) -> Rect {
    card_layout("").inner(area)
}

pub fn focused_card<'a>(ctx: &AppContext, title: impl Into<Line<'a>>) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(crate::theme::accent(ctx)))
        .style(Style::new().bg(crate::theme::base(ctx)))
        .title(title)
}
