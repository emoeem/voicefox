//! 通知 toast 组件

use lx_core::events::NotificationLevel;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::context::AppContext;

pub fn area(screen: Rect, ctx: &AppContext) -> Option<Rect> {
    if !ctx
        .config
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .notification
        .in_app
    {
        return None;
    }
    let notifs = ctx.notifications.read().unwrap_or_else(|e| e.into_inner());
    let notification = notifs.back()?;
    if screen.width < 12 || screen.height < 4 {
        return None;
    }

    let available_width = screen.width.saturating_sub(2);
    let title_width = notification
        .title
        .as_deref()
        .map_or(0, UnicodeWidthStr::width);
    let action_width = notification
        .action_label
        .as_deref()
        .map_or(0, UnicodeWidthStr::width)
        .saturating_add(4);
    let desired_width = UnicodeWidthStr::width(notification.message.as_str())
        .max(title_width)
        .max(action_width)
        .saturating_add(4)
        .clamp(28, 72) as u16;
    let width = desired_width.min(available_width);
    let content_width = width.saturating_sub(2).max(1) as usize;
    let message_width = UnicodeWidthStr::width(notification.message.as_str()).max(1);
    let title_lines = notification.title.as_deref().map_or(0, |title| {
        UnicodeWidthStr::width(title).div_ceil(content_width) as u16
    });
    let action_lines = u16::from(notification.action_url.is_some());
    // 消息不再固定截为最多 4 行：行数按屏幕可用高度动态收缩，
    // 保证标题与 action 行永远落在弹窗可视区内 —— 否则超长消息会把
    // action 行挤出弹窗，而 action_url_at 仍会在最后一行命中不可见按钮。
    let frame_rows = screen.height.saturating_sub(1);
    let chrome_rows = 2u16
        .saturating_add(title_lines)
        .saturating_add(action_lines);
    let max_message_lines = frame_rows.saturating_sub(chrome_rows).max(1);
    let lines = (message_width.div_ceil(content_width) as u16)
        .clamp(1, max_message_lines)
        .saturating_add(title_lines)
        .saturating_add(action_lines);
    let height = lines.saturating_add(2).min(screen.height.saturating_sub(1));
    let x = screen.right().saturating_sub(width).saturating_sub(1);
    let y = screen
        .bottom()
        .saturating_sub(height)
        .saturating_sub(2)
        .max(screen.y);
    Some(Rect::new(x, y, width, height))
}

pub fn action_url_at(
    notification_area: Rect,
    column: u16,
    row: u16,
    ctx: &AppContext,
) -> Option<String> {
    let notification = ctx
        .notifications
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .back()
        .cloned()?;
    let url = notification.action_url?;
    let label = notification.action_label?;
    let inner = Block::default()
        .borders(Borders::ALL)
        .inner(notification_area);
    if inner.height == 0 || row != inner.bottom().saturating_sub(1) {
        return None;
    }
    let start = inner.x;
    let end = start
        .saturating_add(UnicodeWidthStr::width(label.as_str()) as u16)
        .saturating_add(4)
        .min(inner.right());
    (column >= start && column < end).then_some(url)
}

pub fn render(screen: Rect, buf: &mut Buffer, ctx: &AppContext) {
    let Some(area) = area(screen, ctx) else {
        return;
    };
    let lifetime = ctx.notification_timeout();
    let notification = ctx
        .notifications
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .back()
        .cloned();
    let Some(notification) = notification else {
        return;
    };
    let (label, level_color) = match notification.level {
        NotificationLevel::Info => ("信息", crate::theme::blue(ctx)),
        NotificationLevel::Success => ("成功", crate::theme::green(ctx)),
        NotificationLevel::Warn => ("警告", crate::theme::yellow(ctx)),
        NotificationLevel::Error => ("错误", crate::theme::red(ctx)),
    };
    let faded = notification.age() >= lifetime.mul_f32(0.75);
    let text_color = if faded {
        crate::theme::muted(ctx)
    } else {
        crate::theme::text(ctx)
    };
    let style = Style::new().bg(crate::theme::surface0(ctx)).fg(text_color);

    Clear.render(area, buf);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(level_color))
        .style(style)
        .title(format!(" {} · {} ", label, notification.timestamp()));
    let inner = block.inner(area);
    block.render(area, buf);
    let mut lines = Vec::new();
    if let Some(title) = notification.title.as_ref() {
        lines.push(Line::from(Span::styled(
            title.as_str(),
            Style::new().fg(level_color),
        )));
    }
    lines.push(Line::from(notification.message.as_str()));
    if let (Some(label), Some(_)) = (
        notification.action_label.as_ref(),
        notification.action_url.as_ref(),
    ) {
        lines.push(Line::from(Span::styled(
            format!("[ {label} ]"),
            Style::new().fg(level_color),
        )));
    }
    Paragraph::new(lines)
        .style(style)
        .wrap(Wrap { trim: false })
        .render(inner, buf);
}
