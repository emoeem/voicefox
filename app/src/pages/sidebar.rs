//! 侧边栏导航

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

/// 侧边栏标签
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
}

impl NavTab {
    pub const ALL: [Self; 8] = [
        Self::Main,
        Self::Search,
        Self::Leaderboard,
        Self::Playlists,
        Self::Favorites,
        Self::History,
        Self::LocalMusic,
        Self::Settings,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Main => "1 队列",
            Self::Search => "2 搜索",
            Self::Leaderboard => "3 排行榜",
            Self::Playlists => "4 歌单",
            Self::Favorites => "5 收藏",
            Self::History => "6 历史",
            Self::LocalMusic => "7 本地",
            Self::Settings => "8 设置",
        }
    }
}

pub fn render(area: Rect, buf: &mut Buffer, active: NavTab, ctx: &crate::context::AppContext) {
    let accent = crate::theme::accent(ctx);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(crate::theme::muted(ctx)))
        .title("");
    let inner = block.inner(area);
    block.render(area, buf);

    let chunks = tab_chunks(inner);
    for (tab, tab_area) in NavTab::ALL.into_iter().zip(chunks.iter().copied()) {
        let style = if tab == active {
            Style::new()
                .fg(crate::theme::selection_fg(ctx))
                .bg(accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(crate::theme::subtext0(ctx))
        };
        Paragraph::new(Line::from(Span::styled(
            format!(" {} ", tab.label()),
            style,
        )))
        .alignment(ratatui::layout::Alignment::Center)
        .render(tab_area, buf);
    }
}

pub fn hit_test(area: Rect, position: Position) -> Option<NavTab> {
    let inner = Block::default().borders(Borders::ALL).inner(area);
    NavTab::ALL
        .into_iter()
        .zip(tab_chunks(inner).iter())
        .find_map(|(tab, area)| area.contains(position).then_some(tab))
}

fn tab_chunks(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            NavTab::ALL
                .iter()
                .map(|_| Constraint::Ratio(1, NavTab::ALL.len() as u32)),
        )
        .split(area)
}

/// 处理侧边栏全局快捷键，返回要切换到的标签页
pub fn handle_input(key: &KeyEvent) -> Option<NavTab> {
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Char('/')) => Some(NavTab::Search),
        (KeyModifiers::NONE, KeyCode::Char('1')) => Some(NavTab::Main),
        (KeyModifiers::NONE, KeyCode::Char('2')) => Some(NavTab::Search),
        (KeyModifiers::NONE, KeyCode::Char('3')) => Some(NavTab::Leaderboard),
        (KeyModifiers::NONE, KeyCode::Char('4')) => Some(NavTab::Playlists),
        (KeyModifiers::NONE, KeyCode::Char('5')) => Some(NavTab::Favorites),
        (KeyModifiers::NONE, KeyCode::Char('6')) => Some(NavTab::History),
        (KeyModifiers::NONE, KeyCode::Char('7')) => Some(NavTab::LocalMusic),
        (KeyModifiers::NONE, KeyCode::Char('8')) => Some(NavTab::Settings),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::tab_chunks;
    use ratatui::layout::Rect;

    #[test]
    fn tabs_remain_clickable_at_eighty_columns() {
        let chunks = tab_chunks(Rect::new(1, 1, 78, 1));

        assert_eq!(chunks.len(), 8);
        assert!(chunks.iter().all(|chunk| chunk.width >= 9));
        assert_eq!(chunks.iter().map(|chunk| chunk.width).sum::<u16>(), 78);
    }
}
