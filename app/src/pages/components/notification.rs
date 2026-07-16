//! 通知 toast 组件

use lx_core::events::NotificationLevel;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};

use crate::context::AppContext;

pub fn render(area: Rect, buf: &mut Buffer, ctx: &AppContext) {
    let notifs = ctx.notifications.read().unwrap();
    let latest = notifs.back();

    if let Some(notification) = latest {
        let level_color = match notification.level {
            NotificationLevel::Error => crate::theme::red(ctx),
            NotificationLevel::Warn => crate::theme::yellow(ctx),
            NotificationLevel::Info => crate::theme::accent(ctx),
        };
        let age = notification.age();
        let style = if age < std::time::Duration::from_secs(3) {
            Style::new()
                .bg(level_color)
                .fg(crate::theme::selection_fg(ctx))
        } else if age < std::time::Duration::from_secs(4) {
            Style::new()
                .bg(crate::theme::surface2(ctx))
                .fg(crate::theme::text(ctx))
        } else {
            Style::new()
                .bg(crate::theme::mantle(ctx))
                .fg(crate::theme::muted(ctx))
        };
        let line = Line::from(Span::styled(
            format!(" [{}] {}", notification.timestamp(), notification.message),
            style,
        ));

        if area.height > 0 {
            Block::default().style(style).render(area, buf);
            Paragraph::new(line).render(Rect::new(area.x, area.y, area.width, 1), buf);
        }
    }
}
