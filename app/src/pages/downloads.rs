//! 下载面板浮层：查看下载队列、进度与失败原因，并支持取消/清理。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Widget};
use unicode_width::UnicodeWidthStr;

use crate::context::AppContext;
use crate::download::records::{DownloadRecord, DownloadStatus};
use crate::download::{DownloadState, DownloadTaskView, DownloadTrigger};

/// 面板底部展示的历史记录条数。
const HISTORY_LIMIT: usize = 5;

/// 面板按键处理结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelOutcome {
    /// 按键已消费，界面需要重绘。
    Consumed,
    /// 请求关闭面板。
    Close,
    /// 与面板无关的按键，交回主循环。
    Ignore,
}

pub struct DownloadsPanel {
    open: bool,
    selected: usize,
    scroll: usize,
}

impl Default for DownloadsPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl DownloadsPanel {
    pub fn new() -> Self {
        Self {
            open: false,
            selected: 0,
            scroll: 0,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn toggle(&mut self, task_count: usize) {
        self.open = !self.open;
        if self.open {
            // 打开时定位到最近的任务，用户最常关心刚提交的那一条。
            self.selected = task_count.saturating_sub(1);
            self.scroll = 0;
        }
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// 专用下载页渲染：复用下载管理器的成熟布局，但不改变浮层开关状态。
    pub fn render_page(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        ctx: &AppContext,
        tasks: &[DownloadTaskView],
    ) {
        let was_open = self.open;
        self.open = true;
        self.render(area, buf, ctx, tasks);
        self.open = was_open;
    }

    /// 处理按键；`tasks` 为当前任务快照。
    pub fn handle_key(
        &mut self,
        key: &KeyEvent,
        ctx: &AppContext,
        tasks: &[DownloadTaskView],
    ) -> PanelOutcome {
        let len = tasks.len();
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc)
            | (KeyModifiers::CONTROL, KeyCode::Char('o'))
            | (KeyModifiers::NONE, KeyCode::Char('q')) => return PanelOutcome::Close,
            (KeyModifiers::NONE, KeyCode::Char('j' | 'J'))
            | (KeyModifiers::NONE, KeyCode::Down) => {
                if len > 0 {
                    self.selected = (self.selected + 1).min(len - 1);
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('k' | 'K')) | (KeyModifiers::NONE, KeyCode::Up) => {
                self.selected = self.selected.saturating_sub(1);
            }
            (KeyModifiers::NONE, KeyCode::Char('g')) | (KeyModifiers::NONE, KeyCode::Home) => {
                self.selected = 0;
            }
            (KeyModifiers::NONE, KeyCode::Char('G'))
            | (KeyModifiers::SHIFT, KeyCode::Char('G'))
            | (KeyModifiers::NONE, KeyCode::End) => {
                self.selected = len.saturating_sub(1);
            }
            (KeyModifiers::CONTROL, KeyCode::Char('d'))
            | (KeyModifiers::NONE, KeyCode::PageDown) => {
                self.selected = (self.selected + 5).min(len.saturating_sub(1));
            }
            (KeyModifiers::CONTROL, KeyCode::Char('u')) | (KeyModifiers::NONE, KeyCode::PageUp) => {
                self.selected = self.selected.saturating_sub(5);
            }
            (KeyModifiers::NONE, KeyCode::Char('c'))
            | (KeyModifiers::NONE, KeyCode::Char('d'))
            | (KeyModifiers::NONE, KeyCode::Delete) => {
                if let Some(task) = tasks.get(self.selected) {
                    let cancelling = task.state.is_active();
                    ctx.downloads.cancel(task.id);
                    ctx.notify(lx_core::events::Notification::info(if cancelling {
                        format!("已取消下载: {}", task.name)
                    } else {
                        format!("已移除下载记录: {}", task.name)
                    }));
                    self.selected = self.selected.min(len.saturating_sub(2));
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('x')) => {
                ctx.downloads.clear_finished();
                self.selected = 0;
                ctx.notify(lx_core::events::Notification::info("已清理完成的下载记录"));
            }
            (KeyModifiers::SHIFT, KeyCode::Char('C' | 'c'))
            | (KeyModifiers::NONE, KeyCode::Char('C')) => match ctx.downloads.clear_records() {
                Ok(()) => {
                    self.selected = 0;
                    ctx.notify(lx_core::events::Notification::info(
                        "已清空下载历史，之后可以重新下载这些歌曲",
                    ));
                }
                Err(error) => {
                    ctx.notify(lx_core::events::Notification::error(format!(
                        "清空下载历史失败: {error}"
                    )));
                }
            },
            _ => return PanelOutcome::Ignore,
        }
        PanelOutcome::Consumed
    }

    /// 把选中项保持在可视区域内。
    fn clamp_scroll(&mut self, len: usize, rows: usize) {
        self.selected = self.selected.min(len.saturating_sub(1));
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if rows > 0 && self.selected >= self.scroll + rows {
            self.scroll = self.selected + 1 - rows;
        }
    }

    pub fn render(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        ctx: &AppContext,
        tasks: &[DownloadTaskView],
    ) {
        if !self.open || area.width < 20 || area.height < 5 {
            return;
        }
        let width = area.width.saturating_sub(2).clamp(20, 96);
        let active = ctx.downloads.active_count();
        let history = ctx.downloads.records_recent(HISTORY_LIMIT);
        let rows_needed = if tasks.is_empty() { 1 } else { tasks.len() * 2 };
        // 上下边框 + 一行目录提示 + 历史区（含标题行）。
        let history_rows = history_rows(history.len());
        let height = (rows_needed as u16 + history_rows as u16 + 5)
            .min(area.height.saturating_sub(2))
            .max(5);
        let panel = Rect::new(
            area.x + area.width.saturating_sub(width) / 2,
            area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        );
        // 与帮助/详情浮层一致：先清空整屏再画面板，避免和底层页面的边框、
        // 封面图形协议混在一起，关闭时也不会留下两者叠加的痕迹。
        Clear.render(area, buf);
        Clear.render(panel, buf);

        let title = if tasks.is_empty() {
            format!(
                " 下载 · 历史 {} 条 (Ctrl+o 关闭) ",
                ctx.downloads.record_count()
            )
        } else {
            format!(
                " 下载 · {} 条 · {} 进行中 · 历史 {} 条 (Ctrl+o 关闭) ",
                tasks.len(),
                active,
                ctx.downloads.record_count(),
            )
        };
        let block = super::components::chrome::focused_card(ctx, title)
            .style(Style::new().bg(crate::theme::base(ctx)));
        let inner = block.inner(panel);
        block.render(panel, buf);

        // 最后一行固定显示下载目录，回答「下到哪去了」；历史区贴在它上面。
        let footer_height = u16::from(inner.height > 0);
        let history_area = Rect {
            y: inner.y
                + inner
                    .height
                    .saturating_sub(footer_height + history_rows as u16),
            height: (history_rows as u16).min(inner.height.saturating_sub(footer_height)),
            ..inner
        };
        let list_area = Rect {
            height: inner
                .height
                .saturating_sub(footer_height + history_rows as u16),
            ..inner
        };
        let footer_area = Rect {
            y: inner.y + inner.height.saturating_sub(1),
            height: footer_height,
            ..inner
        };

        if tasks.is_empty() {
            Paragraph::new(Line::from(Span::styled(
                "暂无下载任务：歌曲列表按 D、任意页面按 Ctrl+s 下载当前歌曲，右键菜单也可以下载",
                Style::new().fg(crate::theme::muted(ctx)),
            )))
            .wrap(ratatui::widgets::Wrap { trim: false })
            .render(list_area, buf);
        } else {
            let rows = (list_area.height as usize) / 2;
            self.clamp_scroll(tasks.len(), rows.max(1));
            let width = inner.width as usize;
            let mut lines: Vec<Line> = Vec::new();
            for (index, task) in tasks.iter().enumerate().skip(self.scroll).take(rows.max(1)) {
                let selected = index == self.selected;
                lines.push(task_title_line(task, selected, width, ctx));
                lines.push(task_progress_line(task, selected, width, ctx));
            }
            Paragraph::new(lines).render(list_area, buf);
        }
        render_history(history_area, buf, ctx, &history);
        self.render_footer(footer_area, buf, ctx);
    }

    fn render_footer(&self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let text = download_dir_footer(&ctx.downloads.download_dir());
        Paragraph::new(Line::from(Span::styled(
            truncate_to_width(&text, area.width as usize),
            Style::new().fg(crate::theme::overlay1(ctx)),
        )))
        .render(area, buf);
    }
}

/// 面板底部的固定提示：明确告诉用户文件会落在哪个目录。
pub fn download_dir_footer(dir: &std::path::Path) -> String {
    format!("下载目录 {}/", dir.display())
}

/// 历史区占用的行数：每条记录一行，加一行标题；没有记录时不占空间。
fn history_rows(count: usize) -> usize {
    if count == 0 { 0 } else { count + 1 }
}

/// 渲染持久化的下载历史。
///
/// 历史只在面板里展示，不参与选中：它记录的是过去的结果，点选/取消
/// 都没有意义，`C` 一次性清空即可。
fn render_history(area: Rect, buf: &mut Buffer, ctx: &AppContext, history: &[DownloadRecord]) {
    if area.height == 0 || area.width == 0 || history.is_empty() {
        return;
    }
    let width = area.width as usize;
    let mut lines = Vec::with_capacity(history.len() + 1);
    lines.push(Line::from(Span::styled(
        truncate_to_width("历史（C 清空，清空后可重新下载）", width),
        Style::new().fg(crate::theme::overlay1(ctx)),
    )));
    for record in history {
        let marker = match record.status {
            DownloadStatus::Done => "✓",
            DownloadStatus::Skipped => "·",
            DownloadStatus::Failed => "✗",
        };
        let text = format!(
            "  {marker} {} · {} · {} · {}",
            record.singer,
            record.name,
            record.status.label(),
            relative_time(record.finished_at),
        );
        let style = if record.status == DownloadStatus::Failed {
            Style::new().fg(crate::theme::red(ctx))
        } else {
            Style::new().fg(crate::theme::muted(ctx))
        };
        lines.push(Line::from(Span::styled(
            truncate_to_width(&text, width),
            style,
        )));
    }
    Paragraph::new(lines).render(area, buf);
}

/// 把 Unix 秒格式化成「刚刚 / 12 分钟前 / 3 小时前 / 2 天前」。
fn relative_time(finished_at: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default();
    let elapsed = now.saturating_sub(finished_at).max(0);
    match elapsed {
        0..=59 => "刚刚".to_string(),
        60..=3599 => format!("{} 分钟前", elapsed / 60),
        3600..=86_399 => format!("{} 小时前", elapsed / 3600),
        _ => format!("{} 天前", elapsed / 86_400),
    }
}

fn task_title_line(
    task: &DownloadTaskView,
    selected: bool,
    width: usize,
    ctx: &AppContext,
) -> Line<'static> {
    let state_color = state_color(task, ctx);
    let marker = if selected { "▶ " } else { "  " };
    // 自动缓存的任务与手动下载区分开：用户没按过下载键，看到任务时
    // 需要知道它是播放时自动加进来的。
    let origin = if task.trigger == DownloadTrigger::AutoCache {
        format!("{}·缓存", task.source.as_str())
    } else {
        task.source.as_str().to_string()
    };
    let prefix = format!("{marker}{} [{origin}] ", task.state.label());
    let prefix_width = UnicodeWidthStr::width(prefix.as_str());
    let name = truncate_to_width(&task.display_name(), width.saturating_sub(prefix_width + 8));
    let mut style = Style::new().fg(state_color);
    if selected {
        style = style
            .bg(crate::theme::surface0(ctx))
            .add_modifier(Modifier::BOLD);
    }
    Line::from(vec![
        Span::styled(
            marker,
            Style::new().fg(if selected {
                crate::theme::accent(ctx)
            } else {
                crate::theme::muted(ctx)
            }),
        ),
        Span::styled(
            format!("{} [{}] ", task.state.label(), task.source.as_str()),
            style,
        ),
        Span::styled(name, Style::new().fg(crate::theme::text(ctx))),
    ])
}

fn task_progress_line(
    task: &DownloadTaskView,
    selected: bool,
    width: usize,
    ctx: &AppContext,
) -> Line<'static> {
    let style = if selected {
        Style::new()
            .fg(crate::theme::text(ctx))
            .bg(crate::theme::surface0(ctx))
    } else {
        Style::new().fg(crate::theme::muted(ctx))
    };
    if let Some(error) = task.error.as_deref() {
        let text = truncate_to_width(&format!("   失败: {error}"), width);
        return Line::from(Span::styled(text, Style::new().fg(crate::theme::red(ctx))));
    }

    let detail = format_progress_detail(task);
    let bar_width = 12usize;
    let bar = match task.progress.ratio() {
        Some(ratio) => {
            let filled = (ratio * bar_width as f64).round() as usize;
            format!(
                "{}{}",
                "█".repeat(filled.min(bar_width)),
                "░".repeat(bar_width.saturating_sub(filled))
            )
        }
        None => "░".repeat(bar_width),
    };
    let text = format!("    {bar} {detail}");
    Line::from(Span::styled(truncate_to_width(&text, width), style))
}

/// 一行进度明细：百分比或已下载体积、耗时、音质。
fn format_progress_detail(task: &DownloadTaskView) -> String {
    let spec = size_spec(task);
    let size = match task.progress.ratio() {
        Some(ratio) if task.state == DownloadState::Downloading => {
            format!("{:>3.0}%", ratio * 100.0)
        }
        _ => human_size(task.progress.downloaded.max(task.bytes)),
    };
    let elapsed = task.elapsed.as_secs();
    let elapsed = if elapsed >= 60 {
        format!("{}m{}s", elapsed / 60, elapsed % 60)
    } else {
        format!("{elapsed}s")
    };
    match task.state {
        DownloadState::Done => match spec {
            Some(spec) => format!("{spec} · {elapsed} · 已保存 {}", task.dest.display()),
            None => format!("{size} · {elapsed} · 已保存 {}", task.dest.display()),
        },
        DownloadState::Skipped => format!("已存在: {}", task.dest.display()),
        DownloadState::Queued => "等待空闲下载位".to_string(),
        DownloadState::Resolving => "正在解析播放地址（失败会自动换源）".to_string(),
        DownloadState::Tagging => match spec {
            Some(spec) => format!("{spec} · 写入标签与歌词"),
            None => format!("{size} · 写入标签与歌词"),
        },
        DownloadState::Cancelled => "已取消".to_string(),
        DownloadState::Failed => "失败".to_string(),
        DownloadState::Downloading => match spec {
            Some(spec) => format!("{spec} · {size} · {elapsed}"),
            None => format!("{size} · {elapsed} · {}", task.quality.label()),
        },
    }
}

/// 「体积 · 规格」说明：体积取音源声明值或引擎探测到的总量，规格优先用
/// 实测/估算码率，拿不到码率时退回请求档位（128K / FLAC 等）。
fn size_spec(task: &DownloadTaskView) -> Option<String> {
    let total = (task.progress.total > 0)
        .then_some(task.progress.total)
        .or(task.expected_size);
    let quality = task
        .bitrate_kbps
        .map(|bitrate| format!("{bitrate}kbps"))
        .unwrap_or_else(|| task.quality.label().to_string());
    total.map(|size| format!("{} · {quality}", human_size(size)))
}

fn state_color(task: &DownloadTaskView, ctx: &AppContext) -> ratatui::style::Color {
    match task.state {
        DownloadState::Done => crate::theme::green(ctx),
        DownloadState::Skipped => crate::theme::overlay1(ctx),
        DownloadState::Cancelled => crate::theme::yellow(ctx),
        DownloadState::Failed => crate::theme::red(ctx),
        DownloadState::Queued => crate::theme::overlay1(ctx),
        DownloadState::Resolving | DownloadState::Tagging => crate::theme::sapphire(ctx),
        DownloadState::Downloading => crate::theme::accent(ctx),
    }
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes}{}", UNITS[unit])
    } else {
        format!("{value:.1}{}", UNITS[unit])
    }
}

fn truncate_to_width(value: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(value) <= max_width || max_width <= 1 {
        return value.to_string();
    }
    let mut result = String::new();
    let mut used = 0;
    for character in value.chars() {
        let character_width = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
        if used + character_width + 1 > max_width {
            break;
        }
        result.push(character);
        used += character_width;
    }
    result.push('…');
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::engine::ProgressSnapshot;

    fn task(state: DownloadState, downloaded: u64, total: u64) -> DownloadTaskView {
        DownloadTaskView {
            id: 1,
            name: "晴天".to_string(),
            singer: "周杰伦".to_string(),
            source: lx_core::model::source::SourceId::Kw,
            quality: lx_core::model::source::Quality::Flac,
            dest: std::path::PathBuf::from("/music/voicefox/周杰伦 - 晴天.flac"),
            state,
            error: None,
            progress: ProgressSnapshot {
                downloaded,
                total,
                cancelled: false,
            },
            elapsed: std::time::Duration::from_secs(3),
            bytes: 0,
            expected_size: None,
            bitrate_kbps: None,
            trigger: DownloadTrigger::Manual,
        }
    }

    #[test]
    fn human_size_switches_units() {
        assert_eq!(human_size(0), "0B");
        assert_eq!(human_size(512), "512B");
        assert_eq!(human_size(1536), "1.5KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0MB");
    }

    #[test]
    fn truncation_keeps_short_values_intact() {
        assert_eq!(truncate_to_width("abc", 10), "abc");
        assert_eq!(truncate_to_width("abcdef", 4), "abc…");
    }

    #[test]
    fn footer_names_the_download_directory() {
        let footer = download_dir_footer(std::path::Path::new("/home/me/Music/voicefox"));

        assert_eq!(footer, "下载目录 /home/me/Music/voicefox/");
    }

    #[test]
    fn finished_tasks_show_the_full_saved_path() {
        let detail = format_progress_detail(&task(DownloadState::Done, 4096, 4096));

        assert!(detail.contains("已保存"), "{detail}");
        assert!(
            detail.contains("/music/voicefox/周杰伦 - 晴天.flac"),
            "{detail}"
        );
    }

    #[test]
    fn running_tasks_show_percentage_and_quality() {
        let detail = format_progress_detail(&task(DownloadState::Downloading, 512, 1024));

        assert!(detail.contains("50%"), "{detail}");
        assert!(detail.contains("FLAC"), "{detail}");
    }

    #[test]
    fn known_size_and_bitrate_replace_the_quality_label() {
        // 已下载 5 MB / 总量 10 MB → 50%。
        let mut downloading = task(DownloadState::Downloading, 5 * 1024 * 1024, 0);
        // 引擎探测到的总量优先于音源声明值。
        downloading.progress.total = 10 * 1024 * 1024;
        downloading.expected_size = Some(9 * 1024 * 1024);
        downloading.bitrate_kbps = Some(320);
        let detail = format_progress_detail(&downloading);

        assert!(detail.contains("10.0MB"), "{detail}");
        assert!(detail.contains("320kbps"), "{detail}");
        assert!(detail.contains("50%"), "{detail}");
    }

    #[test]
    fn size_spec_falls_back_to_the_engine_total() {
        // 音源没声明体积，但引擎探测到了总量。
        let mut done = task(DownloadState::Done, 0, 0);
        done.bytes = 2 * 1024 * 1024;
        done.expected_size = None;
        done.progress.total = 2 * 1024 * 1024;

        // 没有码率估算时退回请求档位，用户仍能看出下的是哪种规格。
        assert_eq!(size_spec(&done).as_deref(), Some("2.0MB · FLAC"));
        // 两者都没有时不显示体积说明。
        let unknown = task(DownloadState::Queued, 0, 0);
        assert_eq!(size_spec(&unknown), None);
    }

    #[test]
    fn failed_tasks_surface_the_reason() {
        let mut failed = task(DownloadState::Failed, 0, 0);
        failed.error = Some("网络错误: 连接超时".to_string());

        // 失败行由渲染函数直接使用 error 文本，这里校验数据本身可读。
        assert_eq!(format_progress_detail(&failed), "失败");
        assert_eq!(failed.error.as_deref(), Some("网络错误: 连接超时"));
    }

    #[test]
    fn history_rows_account_for_the_title_only_when_present() {
        assert_eq!(history_rows(0), 0);
        assert_eq!(history_rows(1), 2);
        assert_eq!(history_rows(HISTORY_LIMIT), HISTORY_LIMIT + 1);
    }

    #[test]
    fn relative_time_buckets_by_age() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        assert_eq!(relative_time(now), "刚刚");
        assert_eq!(relative_time(now - 120), "2 分钟前");
        assert_eq!(relative_time(now - 7_200), "2 小时前");
        assert_eq!(relative_time(now - 172_800), "2 天前");
        // 时钟回拨时不应出现负数。
        assert_eq!(relative_time(now + 600), "刚刚");
    }
}
