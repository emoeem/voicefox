//! 通知 toast 组件

use lx_core::events::NotificationLevel;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::context::AppContext;

pub fn render(area: Rect, buf: &mut Buffer, ctx: &AppContext) {
    let notifs = ctx.notifications.read().unwrap();
    let latest = notifs.back();

    if let Some(notification) = latest {
        let level_color = match notification.level {
            NotificationLevel::Error => Color::Red,
            NotificationLevel::Warn => Color::Yellow,
            NotificationLevel::Info => crate::theme::accent(ctx),
        };
        let age = notification.age();
        let style = if age < std::time::Duration::from_secs(3) {
            Style::new().bg(level_color).fg(Color::White)
        } else if age < std::time::Duration::from_secs(4) {
            Style::new().bg(Color::DarkGray).fg(Color::White)
        } else {
            Style::new().fg(crate::theme::muted(ctx))
        };
        let line = Line::from(Span::styled(
            format!(" [{}] {}", notification.timestamp(), notification.message),
            style,
        ));

        if area.height > 0 {
            Paragraph::new(line).render(Rect::new(area.x, area.y, area.width, 1), buf);
        }
    }
}
