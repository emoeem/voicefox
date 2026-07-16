//! 排行榜页面

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lx_core::events::AppAction;
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::context::AppContext;

pub struct BoardInfo {
    pub name: String,
    pub id: String,
    pub source: SourceId,
}

pub struct LeaderboardPage {
    pub boards: Vec<BoardInfo>,
    pub songs: Vec<SongInfo>,
    pub selected: usize,
    pub selected_board: Option<usize>,
    pub loaded: bool,
    pub loading: bool,
    pub scroll_offset: usize,
    pub error_message: Option<String>,
}

impl LeaderboardPage {
    pub fn new() -> Self {
        Self {
            boards: vec![
                BoardInfo {
                    name: "TOP500".into(),
                    id: "8888".into(),
                    source: SourceId::Kg,
                },
                BoardInfo {
                    name: "飙升榜".into(),
                    id: "6666".into(),
                    source: SourceId::Kg,
                },
                BoardInfo {
                    name: "抖音热歌榜".into(),
                    id: "52144".into(),
                    source: SourceId::Kg,
                },
                BoardInfo {
                    name: "电音榜".into(),
                    id: "33160".into(),
                    source: SourceId::Kg,
                },
                BoardInfo {
                    name: "粤语金曲榜".into(),
                    id: "33165".into(),
                    source: SourceId::Kg,
                },
                BoardInfo {
                    name: "欧美金曲榜".into(),
                    id: "33166".into(),
                    source: SourceId::Kg,
                },
            ],
            songs: vec![],
            selected: 0,
            selected_board: None,
            loaded: false,
            loading: false,
            scroll_offset: 0,
            error_message: None,
        }
    }

    pub fn handle_input(&mut self, key: &KeyEvent, ctx: &AppContext) -> AppAction {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Up) | (KeyModifiers::NONE, KeyCode::Char('k')) => {
                if self.selected_board.is_some() && !self.songs.is_empty() {
                    if self.selected > 0 {
                        self.selected -= 1;
                    } else if ctx.config.read().unwrap().ui.wrap_navigation {
                        self.selected = self.songs.len().saturating_sub(1);
                    }
                } else if !self.boards.is_empty() {
                    if self.selected > 0 {
                        self.selected -= 1;
                    } else if ctx.config.read().unwrap().ui.wrap_navigation {
                        self.selected = self.boards.len().saturating_sub(1);
                    }
                }
            }
            (KeyModifiers::NONE, KeyCode::Down) | (KeyModifiers::NONE, KeyCode::Char('j')) => {
                if self.selected_board.is_some() && !self.songs.is_empty() {
                    if self.selected + 1 < self.songs.len() {
                        self.selected += 1;
                    } else if ctx.config.read().unwrap().ui.wrap_navigation {
                        self.selected = 0;
                    }
                } else if !self.boards.is_empty() {
                    if self.selected + 1 < self.boards.len() {
                        self.selected += 1;
                    } else if ctx.config.read().unwrap().ui.wrap_navigation {
                        self.selected = 0;
                    }
                }
            }
            (KeyModifiers::NONE, KeyCode::Home) | (KeyModifiers::NONE, KeyCode::Char('g')) => {
                self.selected = 0;
            }
            (KeyModifiers::NONE, KeyCode::End)
            | (KeyModifiers::NONE, KeyCode::Char('G'))
            | (KeyModifiers::SHIFT, KeyCode::Char('G')) => {
                self.selected = if self.selected_board.is_some() {
                    self.songs.len().saturating_sub(1)
                } else {
                    self.boards.len().saturating_sub(1)
                };
            }
            (KeyModifiers::CONTROL, KeyCode::Char('u')) | (KeyModifiers::NONE, KeyCode::PageUp) => {
                self.selected = self.selected.saturating_sub(10);
            }
            (KeyModifiers::CONTROL, KeyCode::Char('d'))
            | (KeyModifiers::NONE, KeyCode::PageDown) => {
                let len = if self.selected_board.is_some() {
                    self.songs.len()
                } else {
                    self.boards.len()
                };
                self.selected = (self.selected + 10).min(len.saturating_sub(1));
            }
            (KeyModifiers::NONE, KeyCode::Enter) | (KeyModifiers::NONE, KeyCode::Char('\r')) => {
                if self.selected_board.is_some() && !self.songs.is_empty() {
                    let songs = self.songs.clone();
                    let index = self.selected;
                    return AppAction::PlaySong { songs, index };
                } else if self.selected < self.boards.len() {
                    // 进入榜单，不设 loading=true（由 main.rs 检测后触发加载）
                    self.selected_board = Some(self.selected);
                    self.songs.clear();
                    self.selected = 0;
                    self.loaded = false;
                    self.loading = false;
                    self.error_message = None;
                    return AppAction::None;
                }
            }
            (KeyModifiers::NONE, KeyCode::Right) if self.selected_board.is_none() => {
                if self.selected < self.boards.len() {
                    self.selected_board = Some(self.selected);
                    self.songs.clear();
                    self.selected = 0;
                    self.loaded = false;
                    self.loading = false;
                    self.error_message = None;
                }
            }
            (KeyModifiers::NONE, KeyCode::Left) if self.selected_board.is_some() => {
                self.selected_board = None;
                self.songs.clear();
                self.selected = 0;
                self.loaded = false;
                self.loading = false;
                self.error_message = None;
            }
            (KeyModifiers::NONE, KeyCode::Esc) if self.selected_board.is_some() => {
                self.selected_board = None;
                self.songs.clear();
                self.selected = 0;
                self.loaded = false;
                self.loading = false;
                self.error_message = None;
            }
            _ => {}
        }
        AppAction::None
    }

    pub fn update_songs(&mut self, songs: Vec<SongInfo>) {
        self.songs = songs;
        self.loaded = true;
        self.loading = false;
        self.error_message = None;
        self.selected = 0;
        self.scroll_offset = 0;
    }

    pub fn begin_loading(&mut self) {
        self.loading = true;
        self.loaded = false;
        self.error_message = None;
    }

    pub fn update_error(&mut self, message: String) {
        self.songs.clear();
        self.loading = false;
        self.loaded = false;
        self.error_message = Some(message);
        self.selected = 0;
        self.scroll_offset = 0;
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        let accent = crate::theme::accent(ctx);
        let muted = crate::theme::muted(ctx);
        let compact = area.width < 82;
        let chunks = if compact {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length((self.boards.len() as u16 + 2).min(9)),
                    Constraint::Min(0),
                ])
                .split(area)
        } else {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(20), Constraint::Min(0)])
                .split(area)
        };

        // 左侧榜单列表
        let board_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(crate::theme::border(ctx)))
            .title("排行榜");

        let board_inner = board_block.inner(chunks[0]);
        board_block.render(chunks[0], buf);

        let selected_style = Style::new()
            .bg(accent)
            .fg(crate::theme::selection_fg(ctx))
            .add_modifier(Modifier::BOLD);
        let normal_style = Style::new().fg(crate::theme::text(ctx));

        let visible_height = board_inner.height as usize;
        let total = self.boards.len();
        let scroll = if self.selected >= visible_height {
            self.selected.saturating_sub(visible_height - 1)
        } else {
            0
        };

        let end = (scroll + visible_height).min(total);
        for i in scroll..end {
            let row = i - scroll;
            let board = &self.boards[i];
            let text = format!("  {:<4} {}", board.source.as_str(), board.name);
            let style = if i == self.selected && self.selected_board.is_none() {
                selected_style
            } else if self.selected_board == Some(i) {
                Style::new().fg(accent).add_modifier(Modifier::BOLD)
            } else {
                normal_style
            };
            let line_area = Rect::new(
                board_inner.x,
                board_inner.y + row as u16,
                board_inner.width,
                1,
            );
            Paragraph::new(Line::from(Span::styled(text, style))).render(line_area, buf);
        }

        // 右侧歌曲列表
        let song_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(crate::theme::border(ctx)))
            .title(if let Some(idx) = self.selected_board {
                self.boards
                    .get(idx)
                    .map(|b| format!("{} - 歌曲列表", b.name))
                    .unwrap_or_else(|| "歌曲列表".into())
            } else {
                "歌曲列表".into()
            });

        let song_inner = song_block.inner(chunks[1]);
        song_block.render(chunks[1], buf);

        if let Some(error) = &self.error_message {
            Paragraph::new(format!("加载失败: {}", error))
                .style(Style::new().fg(crate::theme::red(ctx)))
                .render(song_inner, buf);
        } else if self.loading {
            Paragraph::new("加载中...")
                .style(Style::new().fg(muted))
                .render(song_inner, buf);
        } else if self.selected_board.is_none() {
            Paragraph::new("选择一个榜单")
                .style(Style::new().fg(muted))
                .render(song_inner, buf);
        } else if self.songs.is_empty() && self.loaded {
            Paragraph::new("该榜单暂无歌曲")
                .style(Style::new().fg(muted))
                .render(song_inner, buf);
        } else if !self.songs.is_empty() {
            let selected_style = Style::new()
                .bg(accent)
                .fg(crate::theme::selection_fg(ctx))
                .add_modifier(Modifier::BOLD);
            let normal_style = Style::new().fg(crate::theme::text(ctx));

            if song_inner.height == 0 {
                return;
            }
            Paragraph::new(Line::from(Span::styled(
                super::components::song_table::header(song_inner.width),
                Style::new().fg(muted).add_modifier(Modifier::BOLD),
            )))
            .render(
                Rect::new(song_inner.x, song_inner.y, song_inner.width, 1),
                buf,
            );
            let list_area = Rect::new(
                song_inner.x,
                song_inner.y.saturating_add(1),
                song_inner.width,
                song_inner.height.saturating_sub(1),
            );
            let visible_height = list_area.height as usize;
            if visible_height == 0 {
                return;
            }
            let total = self.songs.len();

            // 自动调整 scroll
            if self.selected >= self.scroll_offset + visible_height {
                self.scroll_offset = self.selected.saturating_sub(visible_height - 1);
            } else if self.selected < self.scroll_offset {
                self.scroll_offset = self.selected;
            }
            self.scroll_offset = self.scroll_offset.min(total.saturating_sub(visible_height));

            let end = (self.scroll_offset + visible_height).min(total);
            for i in self.scroll_offset..end {
                let row = i - self.scroll_offset;
                if row as u16 >= list_area.height {
                    break;
                }
                let song = &self.songs[i];
                let text = super::components::song_table::row(song, i, list_area.width);
                let line_area =
                    Rect::new(list_area.x, list_area.y + row as u16, list_area.width, 1);
                let style = if i == self.selected {
                    selected_style
                } else {
                    normal_style
                };
                Paragraph::new(Line::from(Span::styled(text, style))).render(line_area, buf);
            }
        }
    }

    pub fn handle_mouse(
        &mut self,
        event: MouseEvent,
        area: Rect,
        activate: bool,
        ctx: &AppContext,
    ) -> AppAction {
        let chunks = leaderboard_chunks(area, self.boards.len());
        let scroll_amount = ctx.config.read().unwrap().ui.scroll_amount.max(1);
        match event.kind {
            MouseEventKind::ScrollUp => {
                self.selected = self.selected.saturating_sub(scroll_amount);
            }
            MouseEventKind::ScrollDown => {
                let len = if self.selected_board.is_some() {
                    self.songs.len()
                } else {
                    self.boards.len()
                };
                self.selected = (self.selected + scroll_amount).min(len.saturating_sub(1));
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let board_inner = Block::default().borders(Borders::ALL).inner(chunks[0]);
                if board_inner.contains((event.column, event.row).into()) {
                    let index = event.row.saturating_sub(board_inner.y) as usize;
                    if index < self.boards.len() {
                        if activate {
                            self.selected_board = Some(index);
                            self.songs.clear();
                            self.selected = 0;
                            self.loaded = false;
                            self.loading = false;
                            self.error_message = None;
                        } else if self.selected_board.is_none() {
                            self.selected = index;
                        }
                    }
                    return AppAction::None;
                }

                if self.selected_board.is_some() {
                    let song_inner = Block::default().borders(Borders::ALL).inner(chunks[1]);
                    let list_y = song_inner.y.saturating_add(1);
                    if event.row >= list_y && event.row < song_inner.bottom() {
                        let index = self.scroll_offset + event.row.saturating_sub(list_y) as usize;
                        if index < self.songs.len() {
                            self.selected = index;
                            if activate {
                                return AppAction::PlaySong {
                                    songs: self.songs.clone(),
                                    index,
                                };
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        AppAction::None
    }
}

fn leaderboard_chunks(area: Rect, board_count: usize) -> std::rc::Rc<[Rect]> {
    if area.width < 82 {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length((board_count as u16 + 2).min(9)),
                Constraint::Min(0),
            ])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(20), Constraint::Min(0)])
            .split(area)
    }
}
