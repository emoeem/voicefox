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

        sections.push(Section {
            title: "侧边栏导航（固定）".to_string(),
            entries: vec![
                ("1".to_string(), "队列"),
                ("2".to_string(), "搜索"),
                ("3".to_string(), "排行榜"),
                ("4".to_string(), "歌单"),
                ("5".to_string(), "收藏"),
                ("6".to_string(), "历史"),
                ("7".to_string(), "本地音乐"),
                ("8".to_string(), "下载"),
                ("9".to_string(), "音源"),
                ("0".to_string(), "设置"),
                ("Tab".to_string(), "下一个标签页"),
                ("Shift+Tab".to_string(), "上一个标签页"),
                ("? / \\".to_string(), "打开 / 关闭快捷键说明"),
            ],
        });

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

        let downloads = vec![
            ("j / ↓".to_string(), "选择下一项"),
            ("k / ↑".to_string(), "选择上一项"),
            ("g / Home".to_string(), "跳到第一项"),
            ("G / End".to_string(), "跳到最后一项"),
            ("Ctrl+d / PageDown".to_string(), "向下翻页"),
            ("Ctrl+u / PageUp".to_string(), "向上翻页"),
            ("c / d / Delete".to_string(), "取消任务 / 移除记录"),
            ("x".to_string(), "清理已完成记录"),
            ("C".to_string(), "清空下载历史"),
            ("Esc / q".to_string(), "关闭下载浮层"),
        ];
        sections.push(Section {
            title: "下载页 / 下载面板".to_string(),
            entries: downloads,
        });

        // 音源页复用设置页的实际处理器，因此这里明确展示它真正响应的键，
        // 不虚构一个独立的 `sources` keybinding scope。
        sections.push(Section {
            title: "音源页（复用设置键位）".to_string(),
            entries: vec![
                ("← / →".to_string(), "切换设置分类"),
                ("s".to_string(), "切换管理区域"),
                ("a".to_string(), "添加 JS 音源"),
                ("d".to_string(), "删除选中的 JS 音源（需再次确认）"),
                ("h".to_string(), "检测音源健康状态"),
                ("y / K".to_string(), "选择 / 切换内置音源"),
                ("b".to_string(), "打开扫码登录选择器"),
                ("l".to_string(), "切换导航栏透明背景"),
            ],
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
            | (KeyModifiers::NONE, KeyCode::Char('\\') | KeyCode::Char('?')) => return false,
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
                "快捷键说明 · {}/{} 行 · j/k 滚动 · ? / \\ 关闭",
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
        assert_eq!(help.sections.len(), 4 + PAGE_ORDER.len());
        // 全局区包含退出动作
        assert!(
            help.sections[1]
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
            help.sections[1]
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
