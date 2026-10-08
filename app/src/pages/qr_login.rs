//! 通用扫码登录页面。
//!
//! 原先只有哔哩哔哩有登录页，各平台的二维码协议差异被写在页面里。M0 之后
//! 音源侧统一暴露 `create_qr_login` / `check_qr_login`（返回通用的
//! `QrLoginSession` / `QrLoginResult`），页面因此只需处理一套状态机：
//! 生成二维码 → 轮询 → 成功/过期。

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lx_core::events::AppAction;
use lx_core::keybinding::KeybindingResolver;
use lx_core::model::login::{QrLoginKind, QrLoginResult, QrLoginSession, QrLoginStatus};
use lx_core::model::source::SourceId;
use qrcode::{Color, QrCode};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::theme;

/// 轮询间隔：平台侧状态变化不快，2 秒足够且不至于触发风控。
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const MAX_RETRY_INTERVAL: Duration = Duration::from_secs(15);

/// 二维码过期后自动重建的次数上限。
///
/// 自动刷新是给「人还在页面前，码先过期了」准备的；超过上限仍无进展多半是
/// 环境问题（风控/网络），继续无限重建只会静默轮询，所以停在错误态等用户
/// 手动按 R。
const MAX_AUTO_REGENERATIONS: u32 = 5;

/// 页面状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QrLoginState {
    /// 正在向音源申请二维码。
    Generating,
    /// 二维码已就绪，等待扫码。
    Waiting {
        key: String,
        qr_lines: Vec<String>,
        started: Instant,
        expires_in: u64,
    },
    /// 已扫码，等待手机确认。
    Scanned {
        key: String,
        qr_lines: Vec<String>,
        started: Instant,
        expires_in: u64,
    },
    /// 登录成功。
    Success { user: Option<String> },
    /// 临时错误：二维码继续保留，后台会自动重试。
    Retrying {
        key: String,
        qr_lines: Vec<String>,
        started: Instant,
        expires_in: u64,
        message: String,
    },
    /// 不可恢复的失败或过期。
    Error { message: String },
}

/// 通用扫码登录页。
pub struct QrLoginPage {
    pub source: SourceId,
    /// 本轮扫码走的是哪条渠道（QQ 音乐有 QQ / 微信两条）。
    pub kind: QrLoginKind,
    pub display_name: String,
    pub state: QrLoginState,
    /// 本轮轮询是否已经发出，避免主循环每个 tick 重复请求。
    polling: bool,
    /// 下一次允许轮询的时间；由统一状态机负责节流与退避。
    next_poll_at: Instant,
    /// 连续临时失败次数，用于指数退避。
    retry_count: u32,
    /// 本轮页面已自动重建二维码的次数（按 R 手动刷新会清零）。
    regenerations: u32,
}

impl QrLoginPage {
    /// 指定扫码渠道创建页面（普通音源传 `QrLoginKind::Standard`，
    /// QQ 音乐的「微信」入口传 `QrLoginKind::WeChat`）。
    pub fn with_kind(source: SourceId, display_name: String, kind: QrLoginKind) -> Self {
        Self {
            source,
            kind,
            display_name,
            state: QrLoginState::Generating,
            polling: false,
            next_poll_at: Instant::now(),
            retry_count: 0,
            regenerations: 0,
        }
    }

    /// 二维码生成完成。
    pub fn set_qr(&mut self, session: QrLoginSession) {
        // 会话自己带渠道（重建二维码时可能与页面初始渠道一致，以会话为准）。
        self.kind = session.kind;
        // 有些平台（QQ）返回的是二维码图片而不是链接，此时直接渲染图片。
        let qr_lines = match session.image_png.as_deref() {
            Some(encoded) => render_png_qr(encoded, PNG_QR_MAX_WIDTH),
            None => render_qr_terminal(&session.url, 1),
        };
        self.next_poll_at = Instant::now();
        self.retry_count = 0;
        self.state = QrLoginState::Waiting {
            key: session.key,
            qr_lines,
            started: Instant::now(),
            expires_in: session.expires_in.max(1),
        };
    }

    pub fn set_error(&mut self, message: String) {
        self.state = QrLoginState::Error { message };
    }

    fn set_retrying(&mut self, message: String) {
        let state = std::mem::replace(
            &mut self.state,
            QrLoginState::Error {
                message: String::new(),
            },
        );
        self.state = match state {
            QrLoginState::Waiting {
                key,
                qr_lines,
                started,
                expires_in,
            }
            | QrLoginState::Scanned {
                key,
                qr_lines,
                started,
                expires_in,
            }
            | QrLoginState::Retrying {
                key,
                qr_lines,
                started,
                expires_in,
                ..
            } => QrLoginState::Retrying {
                key,
                qr_lines,
                started,
                expires_in,
                message,
            },
            other => other,
        };
    }

    fn schedule_poll(&mut self, delay: Duration) {
        self.next_poll_at = Instant::now() + delay;
    }

    fn schedule_transient_retry(&mut self) {
        let delay_secs = 2u64.saturating_pow(self.retry_count.min(3));
        let delay = Duration::from_secs(delay_secs).min(MAX_RETRY_INTERVAL);
        self.retry_count = self.retry_count.saturating_add(1);
        self.schedule_poll(delay);
    }

    /// 是否还需要轮询：等待中或已扫码，且二维码尚未过期。
    pub fn should_poll(&self) -> bool {
        match &self.state {
            QrLoginState::Waiting {
                started,
                expires_in,
                ..
            }
            | QrLoginState::Scanned {
                started,
                expires_in,
                ..
            }
            | QrLoginState::Retrying {
                started,
                expires_in,
                ..
            } => started.elapsed() < Duration::from_secs(*expires_in),
            _ => false,
        }
    }

    /// 是否需要（重新）生成二维码：初始生成、或二维码已到期的等待/重试状态。
    ///
    /// 本地计时到期就判定需要重建，不必等服务端 800：最后一次轮询可能走了
    /// 网络退避，若只认服务端结论，页面会永远停在「0 秒后过期」不上不下。
    /// 有轮询在途时暂缓，让它的结果先行落地（结果若是过期会走自动重建）。
    pub fn needs_regeneration(&self) -> bool {
        if self.polling {
            return false;
        }
        match &self.state {
            QrLoginState::Generating => true,
            QrLoginState::Waiting {
                started,
                expires_in,
                ..
            }
            | QrLoginState::Scanned {
                started,
                expires_in,
                ..
            }
            | QrLoginState::Retrying {
                started,
                expires_in,
                ..
            } => started.elapsed() >= Duration::from_secs(*expires_in),
            _ => false,
        }
    }

    /// 过期/会话失效后的自动重建；超出上限就停在错误态，等用户按 R。
    fn request_regeneration(&mut self, reason: &str) {
        self.polling = false;
        if self.regenerations >= MAX_AUTO_REGENERATIONS {
            self.state = QrLoginState::Error {
                message: format!("{reason}，已自动刷新 {MAX_AUTO_REGENERATIONS} 次，按 R 重新生成"),
            };
            return;
        }
        self.regenerations += 1;
        self.state = QrLoginState::Generating;
    }

    /// 手动重新生成二维码（按 R）；清零自动刷新计数。
    fn manual_refresh(&mut self) {
        if matches!(self.state, QrLoginState::Generating) {
            return;
        }
        self.regenerations = 0;
        self.polling = false;
        self.state = QrLoginState::Generating;
    }

    /// 取出本轮要轮询的 (音源, key)。
    ///
    /// 返回 `Some` 时同时标记「轮询进行中」，直到 [`Self::apply_check_result`]
    /// 收到结果才复位——否则主循环会每个 tick 都发起一次请求。
    pub fn begin_poll(&mut self) -> Option<(SourceId, String)> {
        if self.polling || Instant::now() < self.next_poll_at {
            return None;
        }
        let key = match &self.state {
            QrLoginState::Waiting { key, .. }
            | QrLoginState::Scanned { key, .. }
            | QrLoginState::Retrying { key, .. } => key.clone(),
            _ => return None,
        };
        self.polling = true;
        Some((self.source, key))
    }

    /// 应用一次轮询结果。
    pub fn apply_check_result(&mut self, result: Result<QrLoginResult, String>) {
        self.polling = false;
        let result = match result {
            Ok(result) => result,
            Err(message) => {
                self.set_retrying(message);
                self.schedule_transient_retry();
                return;
            }
        };
        match result.status {
            QrLoginStatus::Success => {
                self.state = QrLoginState::Success {
                    user: result.user_name,
                };
            }
            // 过期/会话失效不再停死在错误页：自动重建二维码，用户重新扫即可。
            QrLoginStatus::Expired => self.request_regeneration("二维码已过期"),
            QrLoginStatus::InvalidSession => self.request_regeneration("登录会话已失效"),
            QrLoginStatus::Failed => {
                let message = if result.message.trim().is_empty() {
                    "登录失败".to_string()
                } else {
                    result.message
                };
                self.set_error(message);
            }
            QrLoginStatus::NetworkError
            | QrLoginStatus::RiskControl
            | QrLoginStatus::ServerError => {
                self.set_retrying(if result.message.trim().is_empty() {
                    "正在重试，请保持二维码页面打开…".to_string()
                } else {
                    result.message
                });
                self.schedule_transient_retry();
            }
            // 等待/已扫码：保留二维码，只切换提示文案。
            QrLoginStatus::Waiting | QrLoginStatus::Scanned => {
                let scanned = result.status == QrLoginStatus::Scanned;
                match &self.state {
                    QrLoginState::Waiting {
                        key,
                        qr_lines,
                        started,
                        expires_in,
                    }
                    | QrLoginState::Scanned {
                        key,
                        qr_lines,
                        started,
                        expires_in,
                    } => {
                        let (key, qr_lines, started, expires_in) =
                            (key.clone(), qr_lines.clone(), *started, *expires_in);
                        self.retry_count = 0;
                        self.schedule_poll(POLL_INTERVAL);
                        self.state = if scanned {
                            QrLoginState::Scanned {
                                key,
                                qr_lines,
                                started,
                                expires_in,
                            }
                        } else {
                            QrLoginState::Waiting {
                                key,
                                qr_lines,
                                started,
                                expires_in,
                            }
                        };
                    }
                    _ => {}
                }
            }
        }
    }

    /// 是否已经登录成功（主循环据此关闭页面）。
    pub fn succeeded(&self) -> Option<&Option<String>> {
        match &self.state {
            QrLoginState::Success { user } => Some(user),
            _ => None,
        }
    }

    pub fn handle_input(&mut self, key: KeyEvent, _resolver: &KeybindingResolver) -> AppAction {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc)
            | (KeyModifiers::NONE, KeyCode::Char('q'))
            | (KeyModifiers::NONE, KeyCode::Backspace) => AppAction::GoBack,
            (KeyModifiers::NONE, KeyCode::Char('r'))
            | (KeyModifiers::NONE, KeyCode::Char('R'))
            | (KeyModifiers::SHIFT, KeyCode::Char('R')) => {
                self.manual_refresh();
                AppAction::None
            }
            _ => AppAction::None,
        }
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer, ctx: &crate::context::AppContext) {
        let accent = theme::accent(ctx);
        let muted = theme::muted(ctx);
        let red = theme::red(ctx);
        let yellow = theme::yellow(ctx);
        let green = theme::green(ctx);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(accent))
            .title(format!(
                // 非默认渠道（微信）在标题里标出来，避免与 QQ 扫码页面混淆。
                " {} 扫码登录{} · Esc/q 关闭 · R 重新生成 ",
                self.display_name,
                match self.kind.badge() {
                    Some(badge) => format!("（{badge}）"),
                    None => String::new(),
                }
            ))
            .style(Style::new().bg(theme::mantle(ctx)));
        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height < 6 || inner.width < 10 {
            Paragraph::new("窗口太小，请调整终端尺寸")
                .style(Style::new().fg(red))
                .render(inner, buf);
            return;
        }

        let body = Rect {
            height: inner.height.saturating_sub(1),
            ..inner
        };
        let tips = Paragraph::new(Line::from(Span::styled(
            " Esc / q 关闭 · R 重新生成",
            Style::new().fg(muted),
        )));
        tips.render(
            Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
            buf,
        );

        match &self.state {
            QrLoginState::Generating => {
                let text = if self.regenerations > 0 {
                    "正在重新生成二维码…"
                } else {
                    "正在生成二维码…"
                };
                centered(
                    body,
                    buf,
                    vec![Line::from(Span::styled(text, Style::new().fg(yellow)))],
                )
            }
            QrLoginState::Waiting {
                qr_lines,
                started,
                expires_in,
                ..
            }
            | QrLoginState::Scanned {
                qr_lines,
                started,
                expires_in,
                ..
            }
            | QrLoginState::Retrying {
                qr_lines,
                started,
                expires_in,
                ..
            } => {
                let remaining = expires_in.saturating_sub(started.elapsed().as_secs());
                let status = match &self.state {
                    QrLoginState::Scanned { .. } => "已扫码，请在手机上确认".to_string(),
                    QrLoginState::Retrying { message, .. } => message.clone(),
                    _ => "请使用手机 App 扫码登录".to_string(),
                };
                let mut lines = Vec::new();
                if qr_fits(body, qr_lines) {
                    for line in qr_lines {
                        lines.push(Line::from(Span::styled(
                            line.clone(),
                            Style::new().fg(theme::text(ctx)),
                        )));
                    }
                } else {
                    lines.push(Line::from(Span::styled(
                        "（终端太小，无法显示二维码）",
                        Style::new().fg(red),
                    )));
                }
                lines.push(Line::from(""));
                // 本地计时到期后主循环会立即重建，但仍有 tick 间隙；
                // 显示「正在重新生成」而不是误导性的「0 秒后过期」。
                let expiry_note = if remaining == 0 {
                    "已过期，正在重新生成…".to_string()
                } else {
                    format!("{remaining} 秒后过期")
                };
                lines.push(Line::from(Span::styled(
                    format!("{status}（{expiry_note}）"),
                    Style::new().fg(green),
                )));
                centered(body, buf, lines);
            }
            QrLoginState::Success { user } => {
                let text = match user {
                    Some(user) => format!("登录成功：{user}"),
                    None => "登录成功".to_string(),
                };
                centered(
                    body,
                    buf,
                    vec![Line::from(Span::styled(text, Style::new().fg(green)))],
                );
            }
            QrLoginState::Error { message } => centered(
                body,
                buf,
                vec![Line::from(Span::styled(
                    message.clone(),
                    Style::new().fg(red),
                ))],
            ),
        }
    }
}

/// 图片二维码在终端里的最大列宽（超过就做整数倍缩小，保持锐利）。
const PNG_QR_MAX_WIDTH: usize = 64;

/// 把二维码 PNG（base64）渲染成半块字符行。
///
/// QQ 的登录码是服务端直接生成的图片，无法用 `qrcode` 重新编码，只能在终端
/// 里把像素转成字符：按整数倍缩小到目标列宽，再每两行像素合成一行半块字符。
pub fn render_png_qr(encoded: &str, max_width: usize) -> Vec<String> {
    use base64::Engine;

    let Ok(png) = base64::engine::general_purpose::STANDARD.decode(encoded.trim()) else {
        return vec!["(二维码图片解码失败)".to_string()];
    };
    let Ok(image) = image::load_from_memory(&png) else {
        return vec!["(二维码图片解析失败)".to_string()];
    };
    let gray = image.to_luma8();
    let width = gray.width() as usize;
    let height = gray.height() as usize;
    if width == 0 || height < 2 {
        return vec!["(二维码图片为空)".to_string()];
    }
    let scale = width.div_ceil(max_width.max(1)).max(1);
    let columns = width / scale;
    let rows = height / scale;
    let dark = |x: usize, y: usize| -> bool {
        let pixel = gray.get_pixel(x.min(width - 1) as u32, y.min(height - 1) as u32);
        pixel[0] < 128
    };
    let mut lines = Vec::with_capacity(rows.div_ceil(2));
    for row in (0..rows).step_by(2) {
        let mut line = String::with_capacity(columns);
        for column in 0..columns {
            let upper = dark(column * scale, row * scale);
            let lower = dark(column * scale, ((row + 1).min(rows - 1)) * scale);
            line.push(match (upper, lower) {
                (true, true) => ' ',
                (false, false) => '\u{2588}',
                (true, false) => '\u{2584}',
                (false, true) => '\u{2580}',
            });
        }
        lines.push(line);
    }
    lines
}

fn qr_fits(area: Rect, lines: &[String]) -> bool {
    lines.len() <= area.height as usize
        && lines
            .first()
            .is_none_or(|line| line.chars().count() <= area.width as usize)
}

fn centered(area: Rect, buf: &mut Buffer, lines: Vec<Line<'_>>) {
    let height = lines.len() as u16;
    let offset = area.height.saturating_sub(height) / 2;
    Paragraph::new(lines).alignment(Alignment::Center).render(
        Rect::new(area.x, area.y + offset, area.width, height.min(area.height)),
        buf,
    );
}

/// 将 URL 渲染为终端半块字符 QR 码字符串列表（每行一个字符串）。
pub fn render_qr_terminal(url: &str, scale: u32) -> Vec<String> {
    let Some(code) = QrCode::new(url.as_bytes()).ok() else {
        return vec!["(QR 码生成失败)".to_string()];
    };
    const QUIET_ZONE: usize = 4;

    let width = code.width();
    let padded = width + QUIET_ZONE * 2;
    let mut modules = vec![vec![false; padded]; padded];
    for row in 0..width {
        for col in 0..width {
            modules[row + QUIET_ZONE][col + QUIET_ZONE] = code[(col, row)] == Color::Dark;
        }
    }
    // 半块字符：每次处理两行像素，一次输出一行。
    let mut lines = Vec::new();
    for row_pair in (0..padded).step_by(2) {
        let mut line = String::new();
        for col in 0..padded {
            let upper = modules[row_pair][col];
            let lower = modules
                .get(row_pair + 1)
                .map(|row| row[col])
                .unwrap_or(false);
            line.push(match (upper, lower) {
                (true, true) => ' ',
                (false, false) => '\u{2588}',
                (true, false) => '\u{2584}',
                (false, true) => '\u{2580}',
            });
        }
        lines.push(line);
    }
    if scale > 1 {
        let mut scaled = Vec::new();
        for line in lines {
            let wide: String = line
                .chars()
                .flat_map(|character| std::iter::repeat_n(character, scale as usize))
                .collect();
            for _ in 0..scale {
                scaled.push(wide.clone());
            }
        }
        return scaled;
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_page() -> QrLoginPage {
        QrLoginPage::with_kind(
            SourceId::Wy,
            "网易云音乐".to_string(),
            QrLoginKind::Standard,
        )
    }

    #[test]
    fn renders_a_qr_code_with_quiet_zone() {
        let lines = render_qr_terminal("https://music.163.com/login?codekey=abc", 1);
        assert!(lines.len() > 10);
        // 半块字符只应包含四种字符。
        assert!(lines.iter().all(|line| {
            line.chars()
                .all(|c| matches!(c, ' ' | '\u{2588}' | '\u{2584}' | '\u{2580}'))
        }));
    }

    #[test]
    fn poll_is_throttled_until_a_result_arrives() {
        let mut page = test_page();
        page.set_qr(QrLoginSession {
            kind: QrLoginKind::Standard,
            source: SourceId::Wy,
            key: "key-1".to_string(),
            url: "https://music.163.com/login?codekey=key-1".to_string(),
            image_png: None,
            expires_in: 300,
        });
        // 二维码生成后第一次轮询可以立即发出；之后由统一调度器节流。
        assert_eq!(page.begin_poll(), Some((SourceId::Wy, "key-1".to_string())));
        page.apply_check_result(Ok(QrLoginResult::new(QrLoginStatus::Waiting, "等待扫码")));
        page.next_poll_at = Instant::now() - POLL_INTERVAL;
        assert_eq!(page.begin_poll(), Some((SourceId::Wy, "key-1".to_string())));
        // 结果未回来之前不会重复发起。
        assert_eq!(page.begin_poll(), None);
        page.apply_check_result(Ok(QrLoginResult::new(QrLoginStatus::Waiting, "等待扫码")));
        page.next_poll_at = Instant::now() - POLL_INTERVAL;
        assert!(page.begin_poll().is_some());
    }

    #[test]
    fn transient_errors_use_exponential_backoff_without_expiring_the_qr() {
        let mut page = test_page();
        page.set_qr(QrLoginSession {
            kind: QrLoginKind::Standard,
            source: SourceId::Wy,
            key: "k".to_string(),
            url: "https://music.163.com/login?codekey=k".to_string(),
            image_png: None,
            expires_in: 300,
        });
        page.next_poll_at = Instant::now() - POLL_INTERVAL;
        assert!(page.begin_poll().is_some());
        page.apply_check_result(Err("network".to_string()));
        assert!(page.next_poll_at > Instant::now());
        assert_eq!(page.retry_count, 1);
        assert!(page.should_poll());
    }

    #[test]
    fn scanned_state_keeps_the_qr_and_switches_the_hint() {
        let mut page = test_page();
        page.set_qr(QrLoginSession {
            kind: QrLoginKind::Standard,
            source: SourceId::Wy,
            key: "k".to_string(),
            url: "https://music.163.com/login?codekey=k".to_string(),
            image_png: None,
            expires_in: 300,
        });
        page.apply_check_result(Ok(QrLoginResult::new(QrLoginStatus::Scanned, "已扫码")));
        match &page.state {
            QrLoginState::Scanned { qr_lines, .. } => assert!(!qr_lines.is_empty()),
            other => panic!("应当停留在已扫码状态: {other:?}"),
        }
    }

    #[test]
    fn success_carries_the_account_name_and_stops_polling() {
        let mut page = test_page();
        page.set_qr(QrLoginSession {
            kind: QrLoginKind::Standard,
            source: SourceId::Wy,
            key: "k".to_string(),
            url: "https://music.163.com/login?codekey=k".to_string(),
            image_png: None,
            expires_in: 300,
        });
        let mut result = QrLoginResult::new(QrLoginStatus::Success, "登录成功");
        result.user_name = Some("听歌的人".to_string());
        page.apply_check_result(Ok(result));
        assert_eq!(
            page.succeeded().and_then(|user| user.clone()).as_deref(),
            Some("听歌的人")
        );
        assert!(!page.should_poll());
        assert_eq!(page.begin_poll(), None);
    }

    #[test]
    fn expired_auto_regenerates_until_the_cap_then_needs_manual_refresh() {
        let mut page = test_page();
        page.set_qr(QrLoginSession {
            kind: QrLoginKind::Standard,
            source: SourceId::Wy,
            key: "k".to_string(),
            url: "https://music.163.com/login?codekey=k".to_string(),
            image_png: None,
            expires_in: 300,
        });
        // 过期不再停死在错误页：自动回到生成态，主循环据此重建二维码。
        page.apply_check_result(Ok(QrLoginResult::new(QrLoginStatus::Expired, "过期")));
        assert!(matches!(page.state, QrLoginState::Generating));
        assert!(page.needs_regeneration());
        for _ in 1..MAX_AUTO_REGENERATIONS {
            page.apply_check_result(Ok(QrLoginResult::new(QrLoginStatus::Expired, "过期")));
        }
        assert!(matches!(page.state, QrLoginState::Generating));
        // 超出自动上限后停在错误态，但按 R 仍可重新生成。
        page.apply_check_result(Ok(QrLoginResult::new(QrLoginStatus::Expired, "过期")));
        assert!(matches!(page.state, QrLoginState::Error { .. }));
        assert!(!page.needs_regeneration());
        page.manual_refresh();
        assert!(matches!(page.state, QrLoginState::Generating));

        let mut page = test_page();
        page.set_qr(QrLoginSession {
            kind: QrLoginKind::Standard,
            source: SourceId::Wy,
            key: "k".to_string(),
            url: "https://music.163.com/login?codekey=k".to_string(),
            image_png: None,
            expires_in: 300,
        });
        page.apply_check_result(Err("网络错误".to_string()));
        assert!(matches!(page.state, QrLoginState::Retrying { .. }));
        assert!(page.should_poll());
    }

    #[test]
    fn local_expiry_requests_regeneration_even_without_a_server_verdict() {
        // 回归：最后一两次轮询走了网络退避时，本地计时先到期而服务端 800
        // 还没拿到，旧实现会让页面永远停在「0 秒后过期」。
        let mut page = test_page();
        page.set_qr(QrLoginSession {
            kind: QrLoginKind::Standard,
            source: SourceId::Wy,
            key: "k".to_string(),
            url: "https://music.163.com/login?codekey=k".to_string(),
            image_png: None,
            expires_in: 300,
        });
        match &mut page.state {
            QrLoginState::Waiting { started, .. } => {
                *started = Instant::now() - Duration::from_secs(301);
            }
            other => panic!("应当处于等待扫码状态: {other:?}"),
        }
        assert!(!page.should_poll());
        assert!(page.needs_regeneration());
    }

    #[test]
    fn r_key_requests_a_manual_refresh() {
        let mut page = test_page();
        page.set_qr(QrLoginSession {
            kind: QrLoginKind::Standard,
            source: SourceId::Wy,
            key: "k".to_string(),
            url: "https://music.163.com/login?codekey=k".to_string(),
            image_png: None,
            expires_in: 300,
        });
        let key = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE);
        page.handle_input(key, &unresolved_resolver());
        assert!(matches!(page.state, QrLoginState::Generating));
        assert!(page.needs_regeneration());

        // Shift+R 同样生效。
        let mut page = test_page();
        page.set_qr(QrLoginSession {
            kind: QrLoginKind::Standard,
            source: SourceId::Wy,
            key: "k".to_string(),
            url: "https://music.163.com/login?codekey=k".to_string(),
            image_png: None,
            expires_in: 300,
        });
        page.apply_check_result(Ok(QrLoginResult::new(QrLoginStatus::Scanned, "已扫码")));
        let key = KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT);
        page.handle_input(key, &unresolved_resolver());
        assert!(matches!(page.state, QrLoginState::Generating));
    }

    /// 测试用的键位解析器；扫码页目前不消费键位配置，任意实例均可。
    fn unresolved_resolver() -> KeybindingResolver {
        KeybindingResolver::from_config(&lx_core::keybinding::KeybindingConfig::default())
    }

    #[test]
    fn transient_statuses_keep_the_qr_alive() {
        let mut page = test_page();
        page.set_qr(QrLoginSession {
            kind: QrLoginKind::Standard,
            source: SourceId::Wy,
            key: "k".to_string(),
            url: "https://music.163.com/login?codekey=k".to_string(),
            image_png: None,
            expires_in: 300,
        });
        for status in [
            QrLoginStatus::NetworkError,
            QrLoginStatus::RiskControl,
            QrLoginStatus::ServerError,
        ] {
            page.apply_check_result(Ok(QrLoginResult::new(status, "暂时异常")));
            assert!(matches!(page.state, QrLoginState::Retrying { .. }));
            assert!(page.should_poll());
        }
    }
}
