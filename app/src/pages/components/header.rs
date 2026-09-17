//! Header：顶部 1 行，VOICEFOX Logo + 页面名 + 当前播放歌曲。

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::context::AppContext;

pub fn render(area: Rect, buf: &mut Buffer, ctx: &AppContext, active_tab: &'static str) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let bg = crate::theme::surface0(ctx);
    let bottom = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::new().fg(crate::theme::surface1(ctx)).bg(bg))
        .style(Style::new().bg(bg));
    let inner = bottom.inner(area);
    bottom.render(area, buf);
    if inner.height == 0 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(20), Constraint::Min(10)])
        .split(inner);

    let left = Line::from(vec![
        Span::styled(
            "VOICEFOX",
            Style::new()
                .fg(crate::theme::accent(ctx))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::new().bg(bg)),
        Span::styled(
            active_tab,
            Style::new()
                .fg(crate::theme::text(ctx))
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    Paragraph::new(left).render(chunks[0], buf);

    let song = ctx.current_song.read().unwrap_or_else(|e| e.into_inner());
    let (title, detail) = song.as_ref().map_or_else(
        || ("—".to_string(), "暂无播放".to_string()),
        |song| (song.name.clone(), song.singer.clone()),
    );

    let mut spans: Vec<Span> = Vec::new();
    let pos = *ctx.position.borrow();
    let dur = *ctx.duration.borrow();
    let time_label = if dur.is_zero() {
        String::new()
    } else {
        format!(
            " {:02}:{:02}/{:02}:{:02}",
            pos.as_secs() / 60,
            pos.as_secs() % 60,
            dur.as_secs() / 60,
            dur.as_secs() % 60
        )
    };

    let right_text = format!("{title} — {detail}{time_label}");
    let width = chunks[1].width as usize;
    let truncated = if unicode_width::UnicodeWidthStr::width(right_text.as_str()) > width {
        let mut out = String::new();
        let mut w = 0;
        for c in right_text.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            if w + cw > width.saturating_sub(1) {
                break;
            }
            out.push(c);
            w += cw;
        }
        format!("{out}…")
    } else {
        right_text
    };

    spans.push(Span::styled(
        truncated,
        Style::new().fg(crate::theme::subtext0(ctx)).bg(bg),
    ));

    Paragraph::new(Line::from(spans))
        .alignment(ratatui::layout::Alignment::Right)
        .render(chunks[1], buf);
}
