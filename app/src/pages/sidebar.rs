//! 主导航侧边栏：把不断增长的功能组织成稳定的键盘优先导航。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavTab {
    Main,
    Search,
    Leaderboard,
    Playlists,
    Favorites,
    History,
    Settings,
    LocalMusic,
    Downloads,
    Sources,
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
    fn label(self) -> &'static str {
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
    fn shortcut(self) -> char {
        match self {
            Self::Main => '1',
            Self::Search => '2',
            Self::Leaderboard => '3',
            Self::Playlists => '4',
            Self::Favorites => '5',
            Self::History => '6',
            Self::LocalMusic => '7',
            Self::Downloads => '8',
            Self::Sources => '9',
            Self::Settings => '0',
        }
    }
}

pub const WIDTH: u16 = 18;

pub fn render(area: Rect, buf: &mut Buffer, active: NavTab, ctx: &crate::context::AppContext) {
    let transparent = ctx
        .config
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .ui
        .sidebar_transparent;
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::new().fg(crate::theme::border(ctx)))
        .style(if transparent {
            Style::new()
        } else {
            Style::new().bg(crate::theme::mantle(ctx))
        });
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    Paragraph::new(vec![
        Line::from(Span::styled(
            "VOICEFOX",
            Style::new()
                .fg(crate::theme::text(ctx))
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "音乐终端",
            Style::new().fg(crate::theme::overlay1(ctx)),
        )),
    ])
    .block(Block::default().padding(ratatui::widgets::Padding::left(1)))
    .render(
        Rect::new(inner.x, inner.y, inner.width, 2.min(inner.height)),
        buf,
    );

    let nav = Rect::new(
        inner.x,
        inner.y + 2.min(inner.height),
        inner.width,
        inner.height.saturating_sub(4),
    );
    for (tab, tab_area) in NavTab::ALL.into_iter().zip(tab_chunks(nav).iter().copied()) {
        let selected = tab == active;
        let bg = if selected {
            Some(crate::theme::accent(ctx))
        } else if transparent {
            None
        } else {
            Some(crate::theme::mantle(ctx))
        };
        let fg = if selected {
            crate::theme::selection_fg(ctx)
        } else {
            crate::theme::subtext0(ctx)
        };
        let key_fg = if selected {
            fg
        } else {
            crate::theme::overlay1(ctx)
        };
        let item_style = bg.map_or(Style::new(), |color| Style::new().bg(color));
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {} ", tab.shortcut()), item_style.fg(key_fg)),
            Span::styled(
                format!("{} {}", tab.icon(), tab.label()),
                item_style.fg(fg).add_modifier(if selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
            ),
        ]))
        .style(item_style)
        .render(tab_area, buf);
    }
    if inner.height >= 4 {
        Paragraph::new(Line::from(Span::styled(
            "↑↓ 导航 · Enter 选择",
            Style::new().fg(crate::theme::overlay1(ctx)),
        )))
        .block(Block::default().padding(ratatui::widgets::Padding::left(1)))
        .render(
            Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1),
            buf,
        );
    }
}

pub fn hit_test(area: Rect, position: Position) -> Option<NavTab> {
    let inner = Block::default().borders(Borders::RIGHT).inner(area);
    let nav = Rect::new(
        inner.x,
        inner.y + 2.min(inner.height),
        inner.width,
        inner.height.saturating_sub(4),
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
        let chunks = tab_chunks(Rect::new(1, 3, 17, 10));
        assert_eq!(chunks.len(), NavTab::ALL.len());
        assert!(chunks.iter().all(|chunk| chunk.height == 1));
    }
}
