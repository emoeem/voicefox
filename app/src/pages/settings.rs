//! 设置页面：支持 JS 音源 URL 或本地路径导入/删除

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lx_core::events::AppAction;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};

use crate::context::AppContext;

/// 检查 JS 音源是否已缓存到本地
fn is_source_cached(url: &str) -> bool {
    lx_source::js::loader::is_source_cached(url)
}

fn shorten_source(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    let visible_chars = max_chars.saturating_sub(3);
    format!(
        "{}...",
        value.chars().take(visible_chars).collect::<String>()
    )
}

pub struct SettingsPage {
    /// 输入中的 JS 源 URL 或本地路径
    pub input_url: String,
    /// 是否在输入模式
    pub input_mode: bool,
    /// 导入状态消息
    pub status_msg: Option<String>,
    /// JS 源列表的选中索引
    pub selected_source: usize,
    /// 本地音乐路径输入
    pub local_path_input: String,
    /// 本地音乐路径输入模式
    pub local_path_mode: bool,
    /// 本地路径列表选中索引
    pub selected_local_path: usize,
    /// 当前聚焦区域: "js" 或 "local"
    pub focus: String,
}

impl SettingsPage {
    pub fn new() -> Self {
        Self {
            input_url: String::new(),
            input_mode: false,
            status_msg: None,
            selected_source: 0,
            local_path_input: String::new(),
            local_path_mode: false,
            selected_local_path: 0,
            focus: "js".to_string(),
        }
    }

    pub fn handle_input(&mut self, key: KeyEvent, ctx: &AppContext) -> AppAction {
        if self.local_path_mode {
            return self.handle_local_path_input(key, ctx);
        }
        if self.input_mode {
            match (key.modifiers, key.code) {
                (KeyModifiers::NONE, KeyCode::Esc) => {
                    self.input_mode = false;
                    self.input_url.clear();
                    return AppAction::None;
                }
                (KeyModifiers::NONE, KeyCode::Enter) => {
                    if !self.input_url.trim().is_empty() {
                        let url = self.input_url.trim().to_string();
                        self.input_mode = false;
                        self.input_url.clear();
                        self.status_msg = Some("正在添加音源...".to_string());
                        return AppAction::ImportSource(url);
                    }
                    return AppAction::None;
                }
                (KeyModifiers::NONE, KeyCode::Char(c)) => {
                    self.input_url.push(c);
                }
                (KeyModifiers::NONE, KeyCode::Backspace) => {
                    self.input_url.pop();
                }
                _ => {}
            }
        } else {
            let sources = ctx.config.read().unwrap().source.js_sources.clone();
            match (key.modifiers, key.code) {
                (KeyModifiers::NONE, KeyCode::Char('a')) => {
                    self.input_mode = true;
                    self.status_msg = None;
                }
                (KeyModifiers::NONE, KeyCode::Up) | (KeyModifiers::NONE, KeyCode::Char('k')) => {
                    if self.selected_source > 0 {
                        self.selected_source -= 1;
                    }
                }
                (KeyModifiers::NONE, KeyCode::Down) | (KeyModifiers::NONE, KeyCode::Char('j')) => {
                    if self.selected_source + 1 < sources.len() {
                        self.selected_source += 1;
                    }
                }
                (KeyModifiers::NONE, KeyCode::Char('d')) => {
                    if !sources.is_empty() && self.selected_source < sources.len() {
                        let url = sources[self.selected_source].clone();
                        self.status_msg = Some("已移除音源".to_string());
                        // 如果删除后列表为空或索引超出，重置选中
                        if self.selected_source >= sources.len().saturating_sub(1) {
                            self.selected_source = self.selected_source.saturating_sub(1);
                        }
                        return AppAction::RemoveSource(url);
                    }
                }
                (KeyModifiers::NONE, KeyCode::Char('t')) => {
                    self.update_config(ctx, |config| {
                        config.ui.enable_mouse = !config.ui.enable_mouse;
                    });
                }
                (KeyModifiers::NONE, KeyCode::Char('g')) => {
                    self.update_config(ctx, |config| {
                        config.ui.aggregate_search = !config.ui.aggregate_search;
                    });
                }
                (KeyModifiers::NONE, KeyCode::Char('w')) => {
                    self.update_config(ctx, |config| {
                        config.ui.wrap_navigation = !config.ui.wrap_navigation;
                    });
                }
                (KeyModifiers::NONE, KeyCode::Char('c')) => {
                    self.update_config(ctx, |config| {
                        config.ui.show_cover = !config.ui.show_cover;
                    });
                    if !ctx.config.read().unwrap().ui.show_cover {
                        ctx.cover_service.clear();
                    }
                }
                (KeyModifiers::NONE, KeyCode::Char('m')) => {
                    let mode = ctx.playlist.cycle_mode();
                    let result = {
                        let mut config = ctx.config.write().unwrap();
                        config.player.play_mode = mode.as_config().to_string();
                        crate::config::loader::save(&config, &ctx.config_path)
                    };
                    self.status_msg = Some(match result {
                        Ok(()) => format!("播放模式: {}", mode.label()),
                        Err(error) => format!("播放模式已切换，但保存失败: {}", error),
                    });
                }
                (KeyModifiers::NONE, KeyCode::Char('p')) => {
                    self.update_config(ctx, |config| {
                        config.theme.accent = match config.theme.accent.as_str() {
                            "#cba6f7" => "#89b4fa",
                            "#89b4fa" => "#94e2d5",
                            "#94e2d5" => "#f5c2e7",
                            "#f5c2e7" => "#fab387",
                            _ => "#cba6f7",
                        }
                        .to_string();
                    });
                }
                (KeyModifiers::NONE, KeyCode::Tab) => {
                    self.focus = if self.focus == "js" { "local" } else { "js" }.to_string();
                }
                _ if self.focus == "local" => {
                    return self.handle_local_keys(key, ctx);
                }
                _ => {}
            }
        }
        AppAction::None
    }

    /// 处理本地音乐路径输入模式
    fn handle_local_path_input(&mut self, key: KeyEvent, ctx: &AppContext) -> AppAction {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc) => {
                self.local_path_mode = false;
                self.local_path_input.clear();
                AppAction::None
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                if !self.local_path_input.trim().is_empty() {
                    let path = self.local_path_input.trim().to_string();
                    // 添加到配置
                    {
                        let mut config = ctx.config.write().unwrap();
                        if !config.local_music.paths.contains(&path) {
                            config.local_music.paths.push(path.clone());
                            config.local_music.enabled = true;
                            let _ = crate::config::loader::save(&config, &ctx.config_path);
                        }
                    }
                    // 触发扫描
                    let paths = ctx.config.read().unwrap().local_music.paths.clone();
                    let max_depth = ctx.config.read().unwrap().local_music.max_depth;
                    ctx.source_manager.local_source().scan(&paths, max_depth);
                    self.local_path_mode = false;
                    self.local_path_input.clear();
                    self.status_msg = Some("本地音乐目录已添加".to_string());
                }
                AppAction::None
            }
            (KeyModifiers::NONE, KeyCode::Char(c)) => {
                self.local_path_input.push(c);
                AppAction::None
            }
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                self.local_path_input.pop();
                AppAction::None
            }
            _ => AppAction::None,
        }
    }

    /// 处理本地音乐区域的按键（非输入模式）
    fn handle_local_keys(&mut self, key: KeyEvent, ctx: &AppContext) -> AppAction {
        let paths = ctx.config.read().unwrap().local_music.paths.clone();
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Char('a')) => {
                self.local_path_mode = true;
                self.local_path_input.clear();
                self.status_msg = None;
                AppAction::None
            }
            (KeyModifiers::NONE, KeyCode::Up) | (KeyModifiers::NONE, KeyCode::Char('k')) => {
                if self.selected_local_path > 0 {
                    self.selected_local_path -= 1;
                }
                AppAction::None
            }
            (KeyModifiers::NONE, KeyCode::Down) | (KeyModifiers::NONE, KeyCode::Char('j')) => {
                if self.selected_local_path + 1 < paths.len() {
                    self.selected_local_path += 1;
                }
                AppAction::None
            }
            (KeyModifiers::NONE, KeyCode::Char('d')) => {
                if !paths.is_empty() && self.selected_local_path < paths.len() {
                    let removed = paths[self.selected_local_path].clone();
                    {
                        let mut config = ctx.config.write().unwrap();
                        config.local_music.paths.retain(|p| p != &removed);
                        let _ = crate::config::loader::save(&config, &ctx.config_path);
                    }
                    if self.selected_local_path >= paths.len().saturating_sub(1) {
                        self.selected_local_path = self.selected_local_path.saturating_sub(1);
                    }
                    // 重新扫描
                    let remaining = ctx.config.read().unwrap().local_music.paths.clone();
                    let max_depth = ctx.config.read().unwrap().local_music.max_depth;
                    ctx.source_manager.local_source().scan(&remaining, max_depth);
                    self.status_msg = Some(format!("已移除: {}", removed));
                }
                AppAction::None
            }
            (KeyModifiers::NONE, KeyCode::Char('r')) => {
                let max_depth = ctx.config.read().unwrap().local_music.max_depth;
                let _errors = ctx.source_manager.local_source().scan(&paths, max_depth);
                let count = ctx.source_manager.local_source().all_songs().len();
                self.status_msg = Some(format!("扫描完成，共 {} 首", count));
                AppAction::None
            }
            _ => AppAction::None,
        }
    }

    fn update_config(
        &mut self,
        ctx: &AppContext,
        update: impl FnOnce(&mut lx_core::model::config::Config),
    ) {
        let result = {
            let mut config = ctx.config.write().unwrap();
            update(&mut config);
            crate::config::loader::save(&config, &ctx.config_path)
        };
        self.status_msg = Some(match result {
            Ok(()) => "设置已保存".to_string(),
            Err(error) => format!("保存设置失败: {}", error),
        });
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        let config = ctx.config.read().unwrap();
        let sources = &config.source.js_sources;
        let local_paths = &config.local_music.paths;
        let accent = crate::theme::accent(ctx);
        let muted = crate::theme::muted(ctx);
        let chunks = settings_chunks(area);

        let options_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(crate::theme::border(ctx)))
            .title(" 界面与播放 · t/g/w/c/m/p ");
        let options_inner = options_block.inner(chunks[0]);
        options_block.render(chunks[0], buf);
        let options = vec![
            setting_line("鼠标控制", config.ui.enable_mouse, "t", accent, muted),
            setting_line("聚合搜索", config.ui.aggregate_search, "g", accent, muted),
            setting_line("循环导航", config.ui.wrap_navigation, "w", accent, muted),
            setting_line("封面显示", config.ui.show_cover, "c", accent, muted),
            Line::from(vec![
                Span::styled(" [m] ", Style::new().fg(muted)),
                Span::raw("播放模式   "),
                Span::styled(ctx.playlist.mode().label(), Style::new().fg(accent)),
            ]),
            Line::from(vec![
                Span::styled(" [p] ", Style::new().fg(muted)),
                Span::raw(format!("主题强调色  {}", config.theme.accent)),
            ]),
            Line::from(format!(" 默认音源    {}", config.source.default.as_str())),
            Line::from(format!(
                " 自动换源    {}",
                enabled(config.source.auto_toggle)
            )),
            Line::from(Span::styled(
                format!(" {}", ctx.config_path.display()),
                Style::new().fg(muted),
            )),
        ];
        Paragraph::new(options).render(options_inner, buf);

        let source_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(crate::theme::border(ctx)))
            .title(" lx-music JS 音源 · a 添加 / d 删除 ");
        let source_inner = source_block.inner(chunks[1]);
        source_block.render(chunks[1], buf);
        if source_inner.height > 0 {
            let source_state = if ctx.source_manager.has_js_source() {
                (
                    "已就绪，播放/歌词/封面由 JS 音源解析",
                    crate::theme::green(ctx),
                )
            } else if sources.is_empty() {
                ("尚未导入 JS 音源", crate::theme::yellow(ctx))
            } else {
                ("加载中或加载失败", crate::theme::yellow(ctx))
            };
            Paragraph::new(Line::from(Span::styled(
                format!(" {}", source_state.0),
                Style::new().fg(source_state.1),
            )))
            .render(
                Rect::new(source_inner.x, source_inner.y, source_inner.width, 1),
                buf,
            );
        }

        let source_rows = source_inner.height.saturating_sub(2) as usize;
        if sources.is_empty() {
            if source_inner.height > 2 {
                Paragraph::new(" (无)")
                    .style(Style::new().fg(muted))
                    .render(
                        Rect::new(source_inner.x, source_inner.y + 2, source_inner.width, 1),
                        buf,
                    );
            }
        } else {
            self.selected_source = self.selected_source.min(sources.len().saturating_sub(1));
            let max_chars = source_inner.width.saturating_sub(14) as usize;
            for (row, (index, url)) in sources.iter().enumerate().take(source_rows).enumerate() {
                let cached = is_source_cached(url);
                let status = if cached { "cached" } else { "download" };
                let style = if index == self.selected_source {
                    Style::new()
                        .fg(crate::theme::selection_fg(ctx))
                        .bg(accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(crate::theme::text(ctx))
                };
                let text = format!(" {:<8} {}", status, shorten_source(url, max_chars.max(8)));
                Paragraph::new(Line::from(Span::styled(text, style))).render(
                    Rect::new(
                        source_inner.x,
                        source_inner.y + 2 + row as u16,
                        source_inner.width,
                        1,
                    ),
                    buf,
                );
            }
        }

        if let Some(ref msg) = self.status_msg
            && source_inner.height > 1
        {
            Paragraph::new(Line::from(Span::styled(
                format!(" {}", msg),
                Style::new().fg(crate::theme::yellow(ctx)),
            )))
            .render(
                Rect::new(
                    source_inner.x,
                    source_inner.bottom().saturating_sub(1),
                    source_inner.width,
                    1,
                ),
                buf,
            );
        }

        // ── 本地音乐目录列表 ──
        let local_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(if self.focus == "local" { accent } else { crate::theme::border(ctx) }))
            .title(" 本地音乐目录 · Tab 切换 / a 添加 / d 删除 / r 扫描 ");
        let local_inner = local_block.inner(chunks[2]);
        local_block.render(chunks[2], buf);

        let local_songs = ctx.source_manager.local_source().all_songs();
        let local_rows = local_inner.height.saturating_sub(2) as usize;
        if local_paths.is_empty() {
            if local_inner.height > 2 {
                Paragraph::new(Line::from(Span::styled(
                    " (无，按 a 添加音乐目录)",
                    Style::new().fg(crate::theme::yellow(ctx)),
                )))
                .render(
                    Rect::new(local_inner.x, local_inner.y + 2, local_inner.width, 1),
                    buf,
                );
            }
        } else {
            self.selected_local_path = self.selected_local_path.min(local_paths.len().saturating_sub(1));
            for (row, (index, path)) in local_paths.iter().enumerate().take(local_rows).enumerate() {
                let style = if index == self.selected_local_path {
                    Style::new()
                        .fg(crate::theme::selection_fg(ctx))
                        .bg(accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(crate::theme::text(ctx))
                };
                Paragraph::new(Line::from(Span::styled(
                    format!(" {}", path), style,
                ))).render(
                    Rect::new(local_inner.x, local_inner.y + 2 + row as u16, local_inner.width, 1),
                    buf,
                );
            }
        }
        // 底部显示歌曲数
        if local_inner.height > 1 {
            let footer_y = local_inner.bottom().saturating_sub(1);
            if footer_y > local_inner.y {
                Paragraph::new(Line::from(Span::styled(
                    format!(" 共 {} 首歌曲", local_songs.len()),
                    Style::new().fg(muted),
                ))).render(
                    Rect::new(local_inner.x, footer_y, local_inner.width, 1),
                    buf,
                );
            }
        }

        // ── 本地音乐路径输入弹窗 ──
        if self.local_path_mode {
            let width = area.width.saturating_sub(4).min(74);
            let input_area = Rect::new(
                area.x + area.width.saturating_sub(width) / 2,
                area.y + area.height.saturating_sub(3) / 2,
                width,
                3.min(area.height),
            );
            Clear.render(input_area, buf);
            let input_block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(crate::theme::green(ctx)))
                .title("输入本地音乐目录路径");
            let inner = input_block.inner(input_area);
            input_block.render(input_area, buf);
            let cursor = if (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
                / 500).is_multiple_of(2) { "█" } else { " " };
            Paragraph::new(Line::from(format!("{}{}", self.local_path_input, cursor))).render(inner, buf);
        }

        if self.input_mode {
            let width = area.width.saturating_sub(4).min(74);
            let input_area = Rect::new(
                area.x + area.width.saturating_sub(width) / 2,
                area.y + area.height.saturating_sub(3) / 2,
                width,
                3.min(area.height),
            );
            Clear.render(input_area, buf);
            let input_block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(crate::theme::green(ctx)))
                .title("输入 JS 音源 URL 或本地路径");

            let inner = input_block.inner(input_area);
            input_block.render(input_area, buf);
            let cursor = if (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
                / 500)
                .is_multiple_of(2)
            {
                "█"
            } else {
                " "
            };

            Paragraph::new(Line::from(format!("{}{}", self.input_url, cursor))).render(inner, buf);
        }
    }

    pub fn handle_mouse(&mut self, event: MouseEvent, area: Rect, ctx: &AppContext) -> AppAction {
        if self.input_mode {
            return AppAction::None;
        }
        let chunks = settings_chunks(area);
        match event.kind {
            MouseEventKind::ScrollUp => {
                self.selected_source = self.selected_source.saturating_sub(1);
            }
            MouseEventKind::ScrollDown => {
                let len = ctx.config.read().unwrap().source.js_sources.len();
                self.selected_source = (self.selected_source + 1).min(len.saturating_sub(1));
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let options_inner = Block::default().borders(Borders::ALL).inner(chunks[0]);
                if options_inner.contains((event.column, event.row).into()) {
                    let key = match event.row.saturating_sub(options_inner.y) {
                        0 => Some('t'),
                        1 => Some('g'),
                        2 => Some('w'),
                        3 => Some('c'),
                        4 => Some('m'),
                        5 => Some('p'),
                        _ => None,
                    };
                    if let Some(key) = key {
                        return self.handle_input(
                            KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
                            ctx,
                        );
                    }
                }

                let source_inner = Block::default().borders(Borders::ALL).inner(chunks[1]);
                if source_inner.contains((event.column, event.row).into())
                    && event.row >= source_inner.y.saturating_add(2)
                {
                    let index = event.row.saturating_sub(source_inner.y + 2) as usize;
                    let len = ctx.config.read().unwrap().source.js_sources.len();
                    if index < len {
                        self.selected_source = index;
                        self.focus = "js".to_string();
                    }
                }

                // 本地音乐目录列表点击
                let local_inner = Block::default().borders(Borders::ALL).inner(chunks[2]);
                if local_inner.contains((event.column, event.row).into())
                    && event.row >= local_inner.y.saturating_add(2)
                {
                    let index = event.row.saturating_sub(local_inner.y + 2) as usize;
                    let len = ctx.config.read().unwrap().local_music.paths.len();
                    if index < len {
                        self.selected_local_path = index;
                        self.focus = "local".to_string();
                    }
                }
            }
            _ => {}
        }
        AppAction::None
    }
}

fn enabled(value: bool) -> &'static str {
    if value { "开启" } else { "关闭" }
}

fn setting_line(label: &str, value: bool, key: &str, accent: Color, muted: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" [{key}] "), Style::new().fg(muted)),
        Span::raw(format!("{label:<10} ")),
        Span::styled(
            if value { "[x]" } else { "[ ]" },
            Style::new().fg(if value { accent } else { muted }),
        ),
    ])
}

fn settings_chunks(area: Rect) -> std::rc::Rc<[Rect]> {
    if area.width >= 72 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
            .split(area)
    } else {
        // 三块垂直布局
        let h = area.height;
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(12.min(h.saturating_div(3))),
                Constraint::Min(6),
                Constraint::Length(8.min(h.saturating_div(3))),
            ])
            .split(area)
    }
}

#[cfg(test)]
mod tests {
    use super::shorten_source;

    #[test]
    fn shortens_unicode_source_path_on_character_boundaries() {
        let path = "/home/user/音乐音源/这是一个很长的第三方音源脚本文件名/latest.js";
        let shortened = shorten_source(path, 24);

        assert_eq!(shortened.chars().count(), 24);
        assert!(shortened.ends_with("..."));
    }
}
