//! 主导航侧边栏 — 紧凑宽度 + 左竖线选中态。
//!
//! 布局：
//!  ┌─ VOICEFOX 标题 ─┐
//!  │  ▶ 队列          │
//!  │    ⌕ 搜索        │
//!  │    ▤ 排行榜       │
//!  │    ≡ 歌单        │
//!  │  ·········       │
//!  │    ♥ 收藏        │
//!  │    ↶ 历史        │
//!  │    ♫ 本地音乐     │
//!  │  ⇩ 下载          │
//!  │  ·········       │
//!  │    ◉ 音源        │
//!  │    ⚙ 设置        │
//!  ├─────────────────┤
//!  │  ▌ 茶汤 - 都可唯 │
//!  │    01:58 / 05:08 │
//!  │    ● 3/6         │
//!  └─────────────────┘

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavTab {
    Main,
    Search,
    Leaderboard,
    Playlists,
    Favorites,
    History,
    LocalMusic,
    Downloads,
    Sources,
    Settings,
}

impl NavTab {
    pub const ALL: [Self; 10] = [
        Self::Main,
        Self::Search,
        Self::Leaderboard,
        Self::Playlists,
        Self::Favorites,
        Self::History,
        Self::LocalMusic,
        Self::Downloads,
        Self::Sources,
        Self::Settings,
    ];
    pub fn tab_label(self) -> &'static str {
        match self {
            Self::Main => "队列",
            Self::Search => "搜索",
            Self::Leaderboard => "排行榜",
            Self::Playlists => "歌单",
            Self::Favorites => "收藏",
            Self::History => "历史",
            Self::LocalMusic => "本地音乐",
            Self::Downloads => "下载",
            Self::Sources => "音源",
            Self::Settings => "设置",
        }
    }
    fn icon(self) -> &'static str {
        match self {
            Self::Main => "▶",
            Self::Search => "⌕",
            Self::Leaderboard => "▤",
            Self::Playlists => "≡",
            Self::Favorites => "♥",
            Self::History => "↶",
            Self::LocalMusic => "♫",
            Self::Downloads => "⇩",
            Self::Sources => "◉",
            Self::Settings => "⚙",
        }
    }
}

pub fn constraint() -> Constraint {
    Constraint::Length(18)
}

pub fn render(area: Rect, buf: &mut Buffer, active: NavTab, ctx: &crate::context::AppContext) {
    let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
    let ui = &config.ui;
    let bg_style = if let Some(style) = ui.sidebar_style {
        use lx_core::model::config::SidebarBg;
        match style {
            SidebarBg::Transparent => Style::new(),
            SidebarBg::Mantle => Style::new().bg(crate::theme::mantle(ctx)),
            SidebarBg::Surface0 => Style::new().bg(crate::theme::surface0(ctx)),
            SidebarBg::Surface1 => Style::new().bg(crate::theme::surface1(ctx)),
            SidebarBg::Base => Style::new().bg(crate::theme::base(ctx)),
        }
    } else if ui.sidebar_transparent {
        Style::new()
    } else {
        Style::new().bg(crate::theme::mantle(ctx))
    };
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::new().fg(crate::theme::border(ctx)))
        .style(bg_style);
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height < 8 || inner.width < 8 {
        return;
    }

    const PLAYING_TAB: [NavTab; 1] = [NavTab::Main];
    const EXPLORE_TABS: [NavTab; 3] = [NavTab::Search, NavTab::Leaderboard, NavTab::Playlists];
    const LIBRARY_TABS: [NavTab; 4] = [
        NavTab::Favorites,
        NavTab::History,
        NavTab::LocalMusic,
        NavTab::Downloads,
    ];
    const SYSTEM_TABS: [NavTab; 2] = [NavTab::Sources, NavTab::Settings];

    let total_tabs =
        (PLAYING_TAB.len() + EXPLORE_TABS.len() + LIBRARY_TABS.len() + SYSTEM_TABS.len()) as u16;
    let separators: u16 = 3;

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1 + total_tabs + separators),
            Constraint::Min(2),
            Constraint::Length(1),
        ])
        .split(inner);

    Paragraph::new(Line::from(vec![
        Span::styled("▶", Style::new().fg(crate::theme::accent(ctx))),
        Span::styled(" ", Style::new()),
        Span::styled(
            "VOICEFOX",
            Style::new()
                .fg(crate::theme::text(ctx))
                .add_modifier(Modifier::BOLD),
        ),
    ]))
    .style(bg_style)
    .render(layout[0], buf);

    let nav_area = layout[1];
    let mut nav_cursor = nav_area.y;
    let accent = crate::theme::accent(ctx);
    let sub = crate::theme::subtext0(ctx);
    let sep_fg = crate::theme::overlay2(ctx);
    let dim = crate::theme::overlay1(ctx);

    for (group_index, tabs) in [
        &PLAYING_TAB[..],
        &EXPLORE_TABS[..],
        &LIBRARY_TABS[..],
        &SYSTEM_TABS[..],
    ]
    .iter()
    .enumerate()
    {
        if group_index > 0 && nav_cursor < nav_area.bottom() {
            let sep_area = Rect {
                y: nav_cursor,
                height: 1,
                ..nav_area
            };
            Paragraph::new(Line::from(Span::styled(
                format!("{:·<width$}", "", width = sep_area.width as usize),
                Style::new().fg(sep_fg),
            )))
            .render(sep_area, buf);
            nav_cursor += 1;
        }

        for tab in *tabs {
            if nav_cursor >= nav_area.bottom() {
                break;
            }
            let tab_area = Rect {
                y: nav_cursor,
                height: 1,
                ..nav_area
            };
            let selected = *tab == active;

            let left_pad = if selected { "▌" } else { " " };
            let icon_style = Style::new().bg(bg_style.bg.unwrap_or_default());
            let label_style = Style::new()
                .bg(bg_style.bg.unwrap_or_default())
                .fg(if selected { accent } else { sub })
                .add_modifier(if selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                });

            let line = Line::from(vec![
                Span::styled(
                    left_pad,
                    Style::new().fg(if selected { accent } else { dim }),
                ),
                Span::styled(" ", icon_style),
                Span::styled(
                    format!("{} ", tab.icon()),
                    Style::new().fg(if selected { accent } else { sub }),
                ),
                Span::styled(tab.tab_label().to_string(), label_style),
            ]);
            Paragraph::new(line)
                .style(Style::new().bg(bg_style.bg.unwrap_or_default()))
                .render(tab_area, buf);
            nav_cursor += 1;
        }
    }

    let bottom_area = layout[2];
    render_playing_summary(bottom_area, buf, ctx, bg_style);

    let status_area = layout[3];
    render_source_status(status_area, buf, ctx, bg_style);
}

fn render_playing_summary(
    area: Rect,
    buf: &mut Buffer,
    ctx: &crate::context::AppContext,
    base_style: Style,
) {
    if area.height < 2 || area.width < 8 {
        return;
    }
    let state = *ctx.player_state.borrow();
    let is_playing = matches!(state, lx_core::model::source::PlayerState::Playing);
    let symbol = if is_playing { "▶" } else { "■" };
    let accent = crate::theme::accent(ctx);
    let sub = crate::theme::subtext1(ctx);
    let dim = crate::theme::overlay1(ctx);

    let song = ctx.current_song.read().unwrap_or_else(|e| e.into_inner());

    if let Some(s) = song.as_ref() {
        let pos = *ctx.position.borrow();
        let dur = *ctx.duration.borrow();
        let title = truncate(s, area.width as usize - 4);
        let title_w = unicode_width::UnicodeWidthStr::width(title.as_str());
        let remaining = area.width as usize - title_w - 4;
        let singer = if remaining > 1 {
            let singer = &s.singer;
            if unicode_width::UnicodeWidthStr::width(singer.as_str()) > remaining {
                let mut out = String::new();
                let mut w = 0;
                for c in singer.chars() {
                    let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
                    if w + cw > remaining.saturating_sub(1) {
                        break;
                    }
                    out.push(c);
                    w += cw;
                }
                format!("{out}…")
            } else {
                singer.to_string()
            }
        } else {
            String::new()
        };

        Paragraph::new(Line::from(vec![
            Span::styled(
                format!("▌ {symbol} "),
                Style::new().fg(accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                title.to_string(),
                Style::new()
                    .fg(crate::theme::text(ctx))
                    .add_modifier(Modifier::BOLD),
            ),
        ]))
        .style(base_style)
        .render(Rect::new(area.x, area.y, area.width, 1), buf);

        let time_str = fmt(pos, dur);
        let right = time_str;
        let right_w = unicode_width::UnicodeWidthStr::width(right.as_str());
        let right_pad = area.width.saturating_sub(right_w as u16 + 1);

        let mut row: Vec<Span> = Vec::new();
        row.push(Span::styled(format!("  {singer}"), Style::new().fg(sub)));
        if right_pad > 1 {
            row.push(Span::styled(
                format!("{:>width$}", right, width = right_pad as usize),
                Style::new().fg(dim),
            ));
        }
        Paragraph::new(Line::from(row))
            .style(base_style)
            .render(Rect::new(area.x, area.y + 1, area.width, 1), buf);
    } else {
        Paragraph::new(Line::from(Span::styled("暂无播放", Style::new().fg(dim))))
            .style(base_style)
            .render(Rect::new(area.x, area.y, area.width, 1), buf);
    }
}

fn render_source_status(
    area: Rect,
    buf: &mut Buffer,
    ctx: &crate::context::AppContext,
    base_style: Style,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let health = ctx.source_health.read().unwrap_or_else(|e| e.into_inner());
    let (online, total) = health.iter().fold((0u32, 0u32), |(ok, tot), h| {
        (ok + if h.ok { 1 } else { 0 }, tot + 1)
    });

    let dot = if online == total && total > 0 {
        "●"
    } else if online == 0 {
        "○"
    } else {
        "◐"
    };
    let fg = if online == total && total > 0 {
        crate::theme::green(ctx)
    } else if online == 0 && total > 0 {
        crate::theme::red(ctx)
    } else if total == 0 {
        crate::theme::overlay1(ctx)
    } else {
        crate::theme::yellow(ctx)
    };
    let label = if total == 0 {
        let enabled = ctx
            .config
            .read()
            .map(|c| c.source.enabled.len() as u32)
            .unwrap_or(0);
        if enabled > 0 {
            "检测中…".to_string()
        } else {
            "未启用".to_string()
        }
    } else {
        format!("{dot} {online}/{total}")
    };

    Paragraph::new(Line::from(Span::styled(
        format!("  源 {label}"),
        base_style.fg(fg),
    )))
    .render(area, buf);
}

fn fmt(pos: Duration, dur: Duration) -> String {
    if dur.is_zero() {
        return format!("{:02}:{:02}", pos.as_secs() / 60, pos.as_secs() % 60);
    }
    format!(
        "{:02}:{:02}/{:02}:{:02}",
        pos.as_secs() / 60,
        pos.as_secs() % 60,
        dur.as_secs() / 60,
        dur.as_secs() % 60
    )
}

fn truncate(song: &lx_core::model::song::SongInfo, max: usize) -> String {
    let name = &song.name;
    let width = unicode_width::UnicodeWidthStr::width(name.as_str());
    if width <= max {
        return name.clone();
    }
    if max < 2 {
        return "…".repeat(max);
    }
    let mut out = String::new();
    let mut w = 0;
    for c in name.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > max - 1 {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

pub fn hit_test(area: Rect, position: Position) -> Option<NavTab> {
    let inner = Block::default().borders(Borders::RIGHT).inner(area);
    let nav = Rect::new(
        inner.x,
        inner.y + 1,
        inner.width,
        NavTab::ALL.len() as u16 + 3,
    );
    NavTab::ALL
        .into_iter()
        .zip(tab_chunks(nav).iter())
        .find_map(|(tab, area)| area.contains(position).then_some(tab))
}

fn tab_chunks(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints(NavTab::ALL.iter().map(|_| Constraint::Length(1)))
        .split(area)
}

pub fn handle_input(key: &KeyEvent) -> Option<NavTab> {
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Char('1')) => Some(NavTab::Main),
        (KeyModifiers::NONE, KeyCode::Char('2')) => Some(NavTab::Search),
        (KeyModifiers::NONE, KeyCode::Char('3')) => Some(NavTab::Leaderboard),
        (KeyModifiers::NONE, KeyCode::Char('4')) => Some(NavTab::Playlists),
        (KeyModifiers::NONE, KeyCode::Char('5')) => Some(NavTab::Favorites),
        (KeyModifiers::NONE, KeyCode::Char('6')) => Some(NavTab::History),
        (KeyModifiers::NONE, KeyCode::Char('7')) => Some(NavTab::LocalMusic),
        (KeyModifiers::NONE, KeyCode::Char('8')) => Some(NavTab::Downloads),
        (KeyModifiers::NONE, KeyCode::Char('9')) => Some(NavTab::Sources),
        (KeyModifiers::NONE, KeyCode::Char('0')) => Some(NavTab::Settings),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{NavTab, tab_chunks};
    use ratatui::layout::Rect;
    #[test]
    fn sidebar_rows_remain_clickable() {
        let chunks = tab_chunks(Rect::new(1, 3, 20, 10));
        assert_eq!(chunks.len(), NavTab::ALL.len());
        assert!(chunks.iter().all(|chunk| chunk.height == 1));
    }
}
