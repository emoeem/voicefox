//! 快捷键说明浮层。
//!
//! 展示全局键位与各页面键位；数据来自用户实际生效的键位配置
//! （含自定义修改），而不是硬编码的默认值。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lx_core::keybinding::{KeybindingConfig, PAGE_ORDER};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};

use crate::context::AppContext;
use crate::theme;

/// 一个键位分组（全局或某个页面）
struct Section {
    title: String,
    /// (键位展示, 动作说明)
    entries: Vec<(String, &'static str)>,
}

pub struct HelpPage {
    sections: Vec<Section>,
    scroll: usize,
}

impl HelpPage {
    /// 从实际生效的键位配置构建浮层内容。
    pub fn from_config(config: &KeybindingConfig) -> Self {
        let mut sections = Vec::new();

        let mut global: Vec<(String, &'static str)> = config
            .global
            .iter()
            .map(|(action, key)| (key.clone(), action.label()))
            .collect();
        sort_entries(&mut global);
        sections.push(Section {
            title: "全局快捷键".to_string(),
            entries: global,
        });

        for (page, display) in PAGE_ORDER {
            let Some(bindings) = config.pages.get(page) else {
                continue;
            };
            let mut entries: Vec<(String, &'static str)> = bindings
                .iter()
                .map(|(action, key)| (key.clone(), action.label()))
                .collect();
            sort_entries(&mut entries);
            sections.push(Section {
                title: format!("{display}（{page}）"),
                entries,
            });
        }

        Self {
            sections,
            scroll: 0,
        }
    }

    pub fn handle_input(&mut self, key: &KeyEvent) -> bool {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc)
            | (KeyModifiers::NONE, KeyCode::Char('q'))
            | (KeyModifiers::NONE, KeyCode::Char('\\')) => return false,
            (KeyModifiers::NONE, KeyCode::Char('j' | 'J'))
            | (KeyModifiers::NONE, KeyCode::Down) => {
                self.scroll = self.scroll.saturating_add(1);
            }
            (KeyModifiers::NONE, KeyCode::Char('k' | 'K')) | (KeyModifiers::NONE, KeyCode::Up) => {
                self.scroll = self.scroll.saturating_sub(1);
            }
            (KeyModifiers::CONTROL, KeyCode::Char('d'))
            | (KeyModifiers::NONE, KeyCode::PageDown) => {
                self.scroll = self.scroll.saturating_add(15);
            }
            (KeyModifiers::CONTROL, KeyCode::Char('u')) | (KeyModifiers::NONE, KeyCode::PageUp) => {
                self.scroll = self.scroll.saturating_sub(15);
            }
            (KeyModifiers::NONE, KeyCode::Char('g')) | (KeyModifiers::NONE, KeyCode::Home) => {
                self.scroll = 0
            }
            (KeyModifiers::NONE, KeyCode::Char('G'))
            | (KeyModifiers::NONE, KeyCode::End)
            | (KeyModifiers::SHIFT, KeyCode::Char('G')) => self.scroll = usize::MAX,
            _ => {}
        }
        true
    }

    pub fn handle_mouse(&mut self, event: MouseEvent) -> bool {
        match event.kind {
            MouseEventKind::ScrollUp => {
                self.scroll = self.scroll.saturating_sub(3);
            }
            MouseEventKind::ScrollDown => {
                self.scroll = self.scroll.saturating_add(3);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                // 点击浮层外关闭
                return false;
            }
            _ => {}
        }
        true
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        let width = 84.min(area.width.saturating_sub(4)).max(20);
        let height = area.height.saturating_sub(2).max(5);
        let overlay = Rect::new(
            area.x + area.width.saturating_sub(width) / 2,
            area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        );

        let total_lines = self.line_count();
        let visible = overlay.height.saturating_sub(2) as usize;
        self.scroll = clamp_scroll(self.scroll, total_lines, visible);
        let start = self.scroll;

        // 先构建全文（标题 + 空行 + 条目），再按滚动位置切片
        let mut all_lines: Vec<Line> = Vec::new();
        for section in &self.sections {
            all_lines.push(Line::from(""));
            all_lines.extend(section_lines(section, ctx));
            all_lines.push(Line::from(""));
        }
        let lines: Vec<Line> = all_lines.into_iter().skip(start).take(visible).collect();

        Clear.render(overlay, buf);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(theme::rosewater(ctx)))
            .title(format!(
                "快捷键说明 · {}/{} 行 · j/k 滚动 · Esc 关闭",
                (start + visible).min(total_lines),
                total_lines
            ));
        let inner = block.inner(overlay);
        block.render(overlay, buf);
        Paragraph::new(lines).render(inner, buf);
    }

    fn line_count(&self) -> usize {
        self.sections
            .iter()
            .map(|section| section.entries.len() + 3)
            .sum::<usize>()
            + self.sections.len()
    }
}

fn sort_entries(entries: &mut [(String, &'static str)]) {
    entries.sort_by(|(key_a, label_a), (key_b, label_b)| {
        key_a
            .to_lowercase()
            .cmp(&key_b.to_lowercase())
            .then(label_a.cmp(label_b))
    });
}

fn section_lines(section: &Section, ctx: &AppContext) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(section.entries.len() + 2);
    lines.push(Line::from(Span::styled(
        section.title.clone(),
        Style::new()
            .fg(theme::accent(ctx))
            .add_modifier(Modifier::BOLD),
    )));
    for (key, label) in &section.entries {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {key:<12}"),
                Style::new().fg(theme::rosewater(ctx)),
            ),
            Span::styled((*label).to_string(), Style::new().fg(theme::text(ctx))),
        ]));
    }
    lines
}

pub fn clamp_scroll(scroll: usize, total: usize, visible: usize) -> usize {
    scroll.min(total.saturating_sub(visible))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lx_core::keybinding::{Action, KeybindingResolver};

    #[test]
    fn help_lists_global_and_all_default_pages() {
        let config = KeybindingConfig::default();
        let help = HelpPage::from_config(&config);
        assert_eq!(help.sections.len(), 1 + PAGE_ORDER.len());
        // 全局区包含退出动作
        assert!(
            help.sections[0]
                .entries
                .iter()
                .any(|(_, label)| *label == "退出应用")
        );
    }

    #[test]
    fn esc_and_q_close_the_overlay() {
        let mut help = HelpPage::from_config(&KeybindingConfig::default());
        let resolver_free = KeybindingResolver::from_config(&KeybindingConfig::default());
        let _ = resolver_free;
        assert!(!help.handle_input(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(!help.handle_input(&KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)));
        assert!(help.handle_input(&KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)));
    }

    #[test]
    fn custom_bindings_are_reflected() {
        let mut config = KeybindingConfig::default();
        config.global.insert(Action::GlobalQuit, "Q".to_string());
        let help = HelpPage::from_config(&config);
        assert!(
            help.sections[0]
                .entries
                .iter()
                .any(|(key, label)| key == "Q" && *label == "退出应用")
        );
    }

    #[test]
    fn scroll_clamps_to_content() {
        assert_eq!(clamp_scroll(100, 10, 5), 5);
        assert_eq!(clamp_scroll(2, 10, 5), 2);
    }
}
