//! 哔哩哔哩二维码登录页面：终端内半块字符 QR 码展示与扫码轮询。

use std::sync::Arc;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lx_core::events::AppAction;
use lx_source::bili::{BiliQrPoll, BiliQrStatus, BiliSource};
use qrcode::{Color, QrCode};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::prelude::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

/// 将 URL 渲染为终端半块字符 QR 码字符串列表（每行一个字符串）。
pub fn render_qr_terminal(url: &str, scale: u32) -> Vec<String> {
    let Some(code) = QrCode::new(url.as_bytes()).ok() else {
        return vec!["(QR 码生成失败)".to_string()];
    };
    // 确保宽度是偶数（半块字符每格对应两行像素）
    let width = code.width();
    // 将模块矩阵转为二维布尔数组，白色 padding 边框
    let padded_w = width + 2;
    let padded_h = width + 2;
    let mut modules = vec![vec![false; padded_w]; padded_h];
    for r in 0..width {
        for c in 0..width {
            modules[r + 1][c + 1] = code[(c, r)] == Color::Dark;
        }
    }
    // 半块字符：每次处理两行像素
    let mut lines = Vec::new();
    for row_pair in (0..padded_h).step_by(2) {
        let mut line = String::new();
        for col in 0..padded_w {
            let upper = modules[row_pair][col];
            let lower = modules
                .get(row_pair + 1)
                .map(|row| row[col])
                .unwrap_or(false);
            let ch = match (upper, lower) {
                (true, true) => ' ',        // 上下都是黑色模块 → 空（终端背景色 = 深色）
                (false, false) => '\u{2588}', // 上下都是白色 → 全块（终端前景色 = 浅色）
                (true, false) => '\u{2584}',  // 上黑下白 → 下半块
                (false, true) => '\u{2580}',  // 上白下黑 → 上半块
            };
            line.push(ch);
        }
        lines.push(line);
    }
    // 缩放（逐行重复）
    if scale > 1 {
        let mut scaled = Vec::new();
        for line in lines {
            let wide_line = line
                .chars()
                .flat_map(|ch| std::iter::repeat(ch).take(scale as usize))
                .collect::<String>();
            for _ in 0..scale {
                scaled.push(wide_line.clone());
            }
        }
        return scaled;
    }
    lines
}

/// 登录页面状态
pub enum BiliLoginState {
    Generating,
    Waiting {
        key: String,
        qr_lines: Vec<String>,
        started: Instant,
        expires_in: u64,
    },
    Scanned {
        key: String,
        qr_lines: Vec<String>,
        started: Instant,
    },
    Success {
        user_name: String,
    },
    Expired,
    Error(String),
}

pub struct BiliLoginPage {
    pub state: BiliLoginState,
    pub(crate) source: Arc<BiliSource>,
}

impl BiliLoginPage {
    pub fn new(source: Arc<BiliSource>) -> Self {
        Self {
            state: BiliLoginState::Generating,
            source,
        }
    }

    pub fn set_waiting(&mut self, key: String, qr_lines: Vec<String>) {
        self.state = BiliLoginState::Waiting {
            key,
            qr_lines,
            started: Instant::now(),
            expires_in: 180,
        };
    }

    pub fn set_error(&mut self, msg: String) {
        self.state = BiliLoginState::Error(msg);
    }

    pub fn is_done(&self) -> bool {
        matches!(self.state, BiliLoginState::Success { .. })
            || matches!(self.state, BiliLoginState::Error(_))
    }

    pub fn is_expired(&self) -> bool {
        matches!(self.state, BiliLoginState::Expired)
    }

    pub fn should_poll(&self) -> bool {
        !self.is_done() && !self.is_expired()
            && matches!(
                self.state,
                BiliLoginState::Waiting { .. } | BiliLoginState::Scanned { .. }
            )
    }

    /// 获取轮询参数（source + key）
    pub fn poll_params(&self) -> Option<(Arc<BiliSource>, String)> {
        match &self.state {
            BiliLoginState::Waiting { key, .. } => {
                Some((Arc::clone(&self.source), key.clone()))
            }
            BiliLoginState::Scanned { key, .. } => {
                Some((Arc::clone(&self.source), key.clone()))
            }
            _ => None,
        }
    }

    /// 应用轮询结果
    pub(crate) fn apply_check_result(&mut self, result: Result<BiliQrPoll, String>) {
        let old_state =
            std::mem::replace(&mut self.state, BiliLoginState::Error("unknown".to_string()));
        self.state = match old_state {
            BiliLoginState::Waiting {
                key,
                qr_lines,
                started,
                ..
            } => {
                let elapsed = started.elapsed().as_secs();
                if elapsed > 170 {
                    BiliLoginState::Expired
                } else {
                    match result {
                        Ok(poll) => match poll.status {
                            BiliQrStatus::Waiting => BiliLoginState::Waiting {
                                key,
                                qr_lines,
                                started,
                                expires_in: 180 - elapsed,
                            },
                            BiliQrStatus::Scanned => BiliLoginState::Scanned {
                                key,
                                qr_lines,
                                started,
                            },
                            BiliQrStatus::Success => BiliLoginState::Success {
                                user_name: poll
                                    .user
                                    .as_ref()
                                    .map(|user| user.name.clone())
                                    .unwrap_or_default(),
                            },
                            BiliQrStatus::Expired => BiliLoginState::Expired,
                        },
                        Err(error) => BiliLoginState::Error(format!("轮询失败: {error}")),
                    }
                }
            }
            BiliLoginState::Scanned {
                key,
                qr_lines,
                started,
            } => {
                match result {
                    Ok(poll) => match poll.status {
                        BiliQrStatus::Scanned => BiliLoginState::Scanned {
                            key,
                            qr_lines,
                            started,
                        },
                        BiliQrStatus::Success => BiliLoginState::Success {
                            user_name: poll
                                .user
                                .as_ref()
                                .map(|user| user.name.clone())
                                .unwrap_or_default(),
                        },
                        BiliQrStatus::Expired => BiliLoginState::Expired,
                        BiliQrStatus::Waiting => BiliLoginState::Scanned {
                            key,
                            qr_lines,
                            started,
                        },
                    },
                    Err(error) => BiliLoginState::Error(format!("轮询失败: {error}")),
                }
            }
            other => other,
        };
    }

    pub fn handle_input(&mut self, key: KeyEvent) -> AppAction {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc | KeyCode::Backspace)
            | (KeyModifiers::NONE, KeyCode::Char('q')) => {
                if self.is_done() {
                    AppAction::BiliLoginSuccess
                } else {
                    AppAction::GoBack
                }
            }
            _ => AppAction::None,
        }
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let bili_pink = ratatui::style::Color::Rgb(0xFB, 0x72, 0x99);
        let green = ratatui::style::Color::Rgb(0x00, 0xD0, 0x8A);
        let yellow = ratatui::style::Color::Rgb(0xF5, 0xC2, 0xE7);
        let red = ratatui::style::Color::Rgb(0xF3, 0x8B, 0xA8);
        let muted = ratatui::style::Color::Rgb(0x93, 0x96, 0xB7);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(bili_pink))
            .title(" 哔哩哔哩扫码登录 · Esc/q 关闭 · v 查看图片 ");
        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height < 6 || inner.width < 10 {
            Paragraph::new("窗口太小，请调整终端尺寸").style(Style::new().fg(red)).render(inner, buf);
            return;
        }

        // 底部提示行
        let tips = Line::from(Span::styled(
            " Esc/Backspace/q 返回    v 打开二维码图片",
            Style::new().fg(muted),
        ));
        let tips_y = inner.bottom().saturating_sub(1);
        if tips_y > inner.y {
            Paragraph::new(tips).render(Rect::new(inner.x, tips_y, inner.width, 1), buf);
        }

        let body_area = Rect::new(
            inner.x,
            inner.y,
            inner.width,
            inner.height.saturating_sub(1),
        );

        match &self.state {
            BiliLoginState::Generating => {
                let text = vec![
                    Line::from(Span::styled(" █ 正在生成二维码...", Style::new().fg(yellow))),
                ];
                render_centered(body_area, buf, &text);
            }
            BiliLoginState::Waiting {
                qr_lines,
                expires_in,
                ..
            } => {
                render_qr_body(body_area, buf, qr_lines, false);
                let status = format!(" 请使用哔哩哔哩客户端扫码 ({expires_in}s 后过期)");
                render_status(body_area, buf, &status, yellow);
            }
            BiliLoginState::Scanned { qr_lines, .. } => {
                render_qr_body(body_area, buf, qr_lines, false);
                let status = " 已扫码，请在手机上确认登录";
                render_status(body_area, buf, status, green);
            }
            BiliLoginState::Success { user_name } => {
                let text = vec![
                    Line::from(Span::styled(
                        format!(" ✓ 登录成功! 欢迎, {user_name}"),
                        Style::new().fg(green),
                    )),
                ];
                render_centered(body_area, buf, &text);
            }
            BiliLoginState::Expired => {
                // 没有 qr_lines 的情况下显示纯文本
                Paragraph::new("二维码已过期，按 Esc 返回设置重新生成")
                    .style(Style::new().fg(red))
                    .render(
                        Rect::new(body_area.x, body_area.y + body_area.height / 2, body_area.width, 2),
                        buf,
                    );
            }
            BiliLoginState::Error(msg) => {
                Paragraph::new(format!(" 错误: {msg}"))
                    .style(Style::new().fg(red))
                    .render(
                        Rect::new(body_area.x, body_area.y + body_area.height / 2, body_area.width, 2),
                        buf,
                    );
            }
        }
    }
}

fn render_qr_body(area: Rect, buf: &mut Buffer, qr_lines: &[String], _expired: bool) {
    let qr_height = qr_lines.len() as u16;
    let qr_width = qr_lines.first().map(|l| l.chars().count() as u16).unwrap_or(0);
    let reserved_for_status = if area.height >= qr_height.saturating_add(3) { 3u16 } else { 0 };

    let start_y = area.y.saturating_add(
        (area.height.saturating_sub(reserved_for_status))
            .saturating_sub(qr_height)
            / 2,
    );

    let white_style = Style::new().fg(ratatui::style::Color::White);
    for (row, line) in qr_lines.iter().enumerate() {
        let y = start_y.saturating_add(row as u16);
        if y >= area.bottom() {
            break;
        }
        let x = area.x.saturating_add(area.width.saturating_sub(qr_width) / 2);
        let w = qr_width.min(area.width.saturating_sub(
            area.width.saturating_sub(qr_width) / 2,
        ));
        Paragraph::new(Line::from(Span::styled(line.clone(), white_style)))
            .render(Rect::new(x, y, w, 1), buf);
    }
}

fn render_status(area: Rect, buf: &mut Buffer, text: &str, color: ratatui::style::Color) {
    let status_y = area.bottom().saturating_sub(2);
    if status_y >= area.y && status_y < area.bottom() {
        Paragraph::new(Line::from(Span::styled(text, Style::new().fg(color))))
            .render(Rect::new(area.x, status_y, area.width, 1), buf);
    }
}

fn render_centered(area: Rect, buf: &mut Buffer, lines: &[Line]) {
    let height = lines.len() as u16;
    let start_y = area.y.saturating_add(area.height.saturating_sub(height) / 2);
    Paragraph::new(lines.to_vec()).render(
        Rect::new(area.x, start_y, area.width, height.min(area.height)),
        buf,
    );
}

#[cfg(test)]
mod tests {
    use super::render_qr_terminal;

    #[test]
    fn qr_renders_half_blocks() {
        let lines = render_qr_terminal("https://example.com", 1);
        assert!(!lines.is_empty());
        // 验证每行只包含我们使用的半块字符
        for line in &lines {
            assert!(line.chars().all(|ch: char| matches!(
                ch,
                ' ' | '\u{2588}' | '\u{2584}' | '\u{2580}'
            )));
        }
    }
}
