use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use unicode_width::UnicodeWidthStr;

use crate::AppAction;
use crate::context::AppContext;

pub struct SourcesPage {
    selected: usize,
    scroll_offset: usize,
    status_msg: Option<String>,
}

impl Default for SourcesPage {
    fn default() -> Self {
        Self::new()
    }
}

impl SourcesPage {
    pub fn new() -> Self {
        Self {
            selected: 0,
            scroll_offset: 0,
            status_msg: None,
        }
    }

    fn sources() -> &'static [lx_core::model::source::SourceId] {
        lx_core::model::source::SourceId::all_online()
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let accent = crate::theme::accent(ctx);
        let muted = crate::theme::muted(ctx);
        let green = ratatui::style::Color::Green;
        let red = ratatui::style::Color::Red;
        let yellow = ratatui::style::Color::Yellow;
        let dim = crate::theme::overlay1(ctx);
        let text = crate::theme::text(ctx);
        let overlay0 = crate::theme::overlay0(ctx);

        let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
        let enabled = &config.source.enabled;
        let default_source = config.source.default;

        let header_chunks = Layout::default()
            .direction(ratatui::layout::Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(0)])
            .split(area);

        let title_line = Line::from(vec![
            Span::styled(
                "音源管理",
                Style::new().fg(accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  ↑/↓ 选择 · Space 启用 · d 默认 · l 登录 · q 返回",
                Style::new().fg(muted),
            ),
        ]);
        Paragraph::new(title_line).render(header_chunks[0], buf);

        let content_area = header_chunks[1];
        let sources = Self::sources();
        let total = sources.len();

        if self.selected >= total {
            self.selected = total.saturating_sub(1);
        }

        let visible_rows = content_area.height as usize;
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + visible_rows {
            self.scroll_offset = self.selected + 1 - visible_rows;
        }

        let mut row_y = content_area.y;
        let end = (self.scroll_offset + visible_rows).min(total);
        for i in self.scroll_offset..end {
            let source = sources[i];
            let is_enabled = enabled.contains(&source);
            let is_default = default_source == source;
            let is_logged_in = ctx.source_manager.is_logged_in(source);
            let selected = i == self.selected;

            let mut spans: Vec<Span> = Vec::new();
            let marker = if selected { "▌ " } else { "  " };
            spans.push(Span::styled(
                marker,
                Style::new().fg(if selected { accent } else { overlay0 }),
            ));

            let checkbox = if is_enabled { "✓" } else { " " };
            spans.push(Span::styled(
                format!("[{}]", checkbox),
                Style::new().fg(if is_enabled { green } else { dim }),
            ));
            spans.push(Span::styled(" ", Style::new()));

            let name = source.display_name();
            spans.push(Span::styled(
                pad_right(name, 10),
                Style::new()
                    .fg(if selected { accent } else { text })
                    .add_modifier(Modifier::BOLD),
            ));

            spans.push(Span::styled(" ", Style::new()));

            spans.push(Span::styled(
                if is_default { "★ 默认" } else { "      " },
                Style::new().fg(if is_default { yellow } else { muted }),
            ));

            spans.push(Span::styled(" ", Style::new()));

            let login_text = if is_logged_in {
                "已登录"
            } else {
                "未登录"
            };
            spans.push(Span::styled(
                pad_right(login_text, 6),
                Style::new().fg(if is_logged_in { green } else { red }),
            ));

            spans.push(Span::styled("  ", Style::new()));

            let line = Line::from(spans);
            Paragraph::new(line)
                .render(Rect::new(content_area.x, row_y, content_area.width, 1), buf);

            row_y += 1;
        }

        if let Some(msg) = &self.status_msg
            && row_y < area.y + area.height
        {
            let footer = Line::from(vec![Span::styled(msg, Style::new().fg(accent))]);
            Paragraph::new(footer).render(Rect::new(area.x, row_y, area.width, 1), buf);
        }
    }

    pub fn consumes_key(&self, key: &KeyEvent) -> bool {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Char('1'..='9' | '0')) => false,
            (KeyModifiers::NONE, KeyCode::Esc) | (KeyModifiers::NONE, KeyCode::Tab) => false,
            (
                KeyModifiers::NONE,
                KeyCode::Char('j' | 'k' | 'g' | 'G' | 'd' | 'l' | 'L' | 'q' | ' ' | 'h' | 'K'),
            ) => true,
            (KeyModifiers::NONE, KeyCode::Up | KeyCode::Down) => true,
            _ => false,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, ctx: &AppContext) -> AppAction {
        if matches!(
            (key.modifiers, key.code),
            (KeyModifiers::NONE, KeyCode::Esc)
        ) {
            return AppAction::GoBack;
        }
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Char('j') | KeyCode::Down) => {
                let total = Self::sources().len();
                if total > 0 {
                    self.selected = (self.selected + 1) % total;
                }
                AppAction::None
            }
            (KeyModifiers::NONE, KeyCode::Char('k') | KeyCode::Up) => {
                let total = Self::sources().len();
                if total > 0 {
                    self.selected = (self.selected + total - 1) % total;
                }
                AppAction::None
            }
            (KeyModifiers::NONE, KeyCode::Char('g')) => {
                self.selected = 0;
                AppAction::None
            }
            (KeyModifiers::NONE, KeyCode::Char('G')) => {
                let total = Self::sources().len();
                self.selected = total.saturating_sub(1);
                AppAction::None
            }
            (KeyModifiers::NONE, KeyCode::Char(' ')) => {
                self.toggle_selected(ctx);
                AppAction::None
            }
            (KeyModifiers::SHIFT, KeyCode::Char('K' | 'k'))
            | (KeyModifiers::NONE, KeyCode::Char('K')) => {
                self.toggle_selected(ctx);
                AppAction::None
            }
            (KeyModifiers::NONE, KeyCode::Char('d')) => {
                self.set_as_default(ctx);
                AppAction::None
            }
            (KeyModifiers::NONE, KeyCode::Char('l')) => {
                let source = Self::sources()[self.selected.min(Self::sources().len() - 1)];
                if ctx.source_manager.is_logged_in(source) {
                    self.status_msg = Some(format!("{} 已登录", source.display_name()));
                    AppAction::None
                } else {
                    AppAction::QrLogin(source)
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('Q')) => {
                let source = Self::sources()[self.selected.min(Self::sources().len() - 1)];
                AppAction::QrLogout(source)
            }
            _ => AppAction::None,
        }
    }

    fn toggle_selected(&mut self, ctx: &AppContext) {
        let sources = Self::sources();
        if sources.is_empty() {
            return;
        }
        let source = sources[self.selected.min(sources.len() - 1)];
        let result = {
            let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
            if config.source.enabled.contains(&source) {
                if config.source.enabled.len() == 1 {
                    self.status_msg = Some("至少保留一个音源".to_string());
                    return;
                }
                config.source.enabled.retain(|s| *s != source);
                if config.source.default == source {
                    config.source.default = config.source.enabled[0];
                }
            } else {
                config.source.enabled.push(source);
                config.source.enabled.sort_by_key(|item| {
                    sources
                        .iter()
                        .position(|candidate| candidate == item)
                        .unwrap_or(usize::MAX)
                });
            }
            let default = config.source.default;
            let enabled = config.source.enabled.clone();
            let save = crate::config::loader::save(&config, &ctx.config_path);
            (default, enabled, save)
        };
        ctx.source_manager
            .update_source_preferences(result.0, &result.1);
        match result.2 {
            Ok(()) => {
                let state = if result.1.contains(&source) {
                    "已启用"
                } else {
                    "已禁用"
                };
                self.status_msg = Some(format!("{} {}", source.display_name(), state));
            }
            Err(e) => {
                self.status_msg = Some(format!("保存失败: {e}"));
            }
        }
    }

    fn set_as_default(&mut self, ctx: &AppContext) {
        let sources = Self::sources();
        if sources.is_empty() {
            return;
        }
        let source = sources[self.selected.min(sources.len() - 1)];
        let result = {
            let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
            if !config.source.enabled.contains(&source) {
                self.status_msg = Some(format!("请先启用 {} 再设为默认", source.display_name()));
                return;
            }
            config.source.default = source;
            let default = config.source.default;
            let enabled = config.source.enabled.clone();
            let save = crate::config::loader::save(&config, &ctx.config_path);
            (default, enabled, save)
        };
        ctx.source_manager
            .update_source_preferences(result.0, &result.1);
        match result.2 {
            Ok(()) => {
                self.status_msg = Some(format!("默认音源: {}", source.display_name()));
            }
            Err(e) => {
                self.status_msg = Some(format!("保存失败: {e}"));
            }
        }
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect, _ctx: &AppContext) -> AppAction {
        match mouse.kind {
            crossterm::event::MouseEventKind::ScrollDown => {
                let total = Self::sources().len();
                if total > 0 {
                    self.selected = (self.selected + 1) % total;
                }
                AppAction::None
            }
            crossterm::event::MouseEventKind::ScrollUp => {
                let total = Self::sources().len();
                if total > 0 {
                    self.selected = (self.selected + total - 1) % total;
                }
                AppAction::None
            }
            _ => {
                let rel_y = mouse.row.saturating_sub(area.y);
                let header_height = 2u16;
                if rel_y >= header_height {
                    let idx = (rel_y - header_height) as usize + self.scroll_offset;
                    let total = Self::sources().len();
                    if idx < total {
                        self.selected = idx;
                    }
                }
                AppAction::None
            }
        }
    }
}

fn pad_right(value: &str, width: usize) -> String {
    let used = UnicodeWidthStr::width(value);
    if used >= width {
        return value.to_string();
    }
    format!("{value}{}", " ".repeat(width - used))
}
