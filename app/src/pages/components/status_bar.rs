//! 底部状态栏 —— 播放器快速控制栏
//!
//! 这里不只是"画一行字"：每个动态字段同时是一个可点击的控件。
//! 因此**渲染与命中必须共用同一份几何** —— 宽度分配、换行、丢弃、
//! 「更多」收纳都只在 `allocate()` 里算一次，`render()` 按它画，
//! `hit_test()` 按它命中。否则就会重演本项目已经修过好几次的
//! "看得见却点不准 / 差一行"那类问题。

use lx_core::model::config::{STATUS_BAR_MAX_HEIGHT, SourcePolicy, StatusBarItem};
use lx_core::model::source::PlayerState;
use lx_core::model::source::Quality;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};
use unicode_width::UnicodeWidthStr;

use crate::fmt::format_duration;
use crate::pages::components::text::truncate_width;

use crate::context::AppContext;

/// 状态栏上可命中区域的标识。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusBarSlot {
    Item(StatusBarItem),
    /// 前置的下载进度指示（点击打开下载面板）。
    Download,
    /// 前置的睡眠定时器倒计时（点击打开定时器菜单）。
    SleepTimer,
    /// 因宽度不足被收纳的可交互段入口。
    More,
}

/// 一段的几何。渲染与命中共用同一份记录。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusBarHit {
    pub slot: StatusBarSlot,
    /// 该段所占的屏幕矩形（高度恒为 1）。
    pub rect: Rect,
}

/// 一帧的底栏布局结果。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StatusBarFrame {
    /// 本帧可命中的区段。
    pub hits: Vec<StatusBarHit>,
    /// 因宽度被收纳进「更多」的段（顺序同配置）——「更多」菜单靠它列出内容。
    pub collapsed: Vec<StatusBarSlot>,
    /// 本帧实际画了几行（= `ui.status_bar_rows()` 再按可用高度夹取）。
    pub rows: u16,
    /// 顶边的拖拽把手矩形（与画出来的那一格同源；面板过窄时为 `None`）。
    pub handle: Option<Rect>,
}

/// 命中测试：`(column, row)` 落在哪一段上。
pub fn hit_test(hits: &[StatusBarHit], column: u16, row: u16) -> Option<StatusBarSlot> {
    hits.iter()
        .find(|hit| hit.rect.y == row && column >= hit.rect.x && column < hit.rect.right())
        .map(|hit| hit.slot)
}

// ───────────────────── 顶边拖拽把手（改底栏行数） ─────────────────────

/// 拖拽把手的宽度（列）。放在**顶行最右端**：右侧角落是经典的"拉伸把手"位置，
/// 而把宽度从排布里扣掉（见 [`render`]）就保证它永远不会盖住任何一个段 ——
/// 于是"按在把手上"与"点段"天然互斥，不需要靠状态机兜底。
pub const RESIZE_HANDLE_WIDTH: u16 = 2;

/// 把手字形：`⇕`（上/下双箭头）明示"这里能上下拖"。
pub const RESIZE_HANDLE_GLYPH: &str = " ⇕";

/// 顶边拖拽把手所在的矩形；面板为空 / 过窄时为 `None`。
///
/// 渲染与命中共用这一个函数，避免"画在这一格、却要按在那一格"。
pub fn resize_handle(area: Rect) -> Option<Rect> {
    (area.height > 0 && area.width > RESIZE_HANDLE_WIDTH).then(|| {
        Rect::new(
            area.right() - RESIZE_HANDLE_WIDTH,
            area.y,
            RESIZE_HANDLE_WIDTH,
            1,
        )
    })
}

/// 指针所在行 → 底栏行数（纯映射，含 1..=[`STATUS_BAR_MAX_HEIGHT`] 夹取）。
///
/// 底栏钉在屏幕底部，所以拖动它的上边缘时"指针在哪一行"就唯一决定行数：
/// `rows = 底边 - 指针行`。往上拖 = 变高，往下拖 = 变矮，越过上下限都夹回区间内。
pub fn rows_for_pointer(bottom: u16, pointer_row: u16) -> u16 {
    bottom
        .saturating_sub(pointer_row)
        .clamp(1, u16::from(STATUS_BAR_MAX_HEIGHT))
}

/// 该字段是否可交互 —— 决定放不下时是否值得收进「更多」。
///
/// 时间与排序是纯展示，挤不下就直接丢；其余字段丢一个就等于少一个入口。
pub fn is_interactive(item: StatusBarItem) -> bool {
    matches!(
        item,
        StatusBarItem::State
            | StatusBarItem::Song
            | StatusBarItem::Volume
            | StatusBarItem::PlayMode
            | StatusBarItem::Source
            | StatusBarItem::Quality
            | StatusBarItem::Queue
            | StatusBarItem::JsSourceState
    )
}

fn slot_is_interactive(slot: StatusBarSlot) -> bool {
    match slot {
        StatusBarSlot::Item(item) => is_interactive(item),
        StatusBarSlot::Download | StatusBarSlot::SleepTimer | StatusBarSlot::More => true,
    }
}

/// 收纳入口的文本。
const MORE_LABEL: &str = "⋯ 更多";

const SEGMENT_SEPARATOR: &str = "  ·  ";

// ───────────────────── 纯布局（不依赖 ctx / Buffer，可单测） ─────────────────────

/// 一个待排布的段。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    slot: StatusBarSlot,
    /// 固定前缀（如「音源 」）。截断只作用于 `text`，前缀永远保留。
    prefix: &'static str,
    /// 正文（可能被截断）。
    text: String,
    /// 可压缩到的下限（等于自然宽度表示不可压缩）。
    min_width: usize,
    /// 显示上限（超出按显示宽度截断）。
    max_width: usize,
}

impl Candidate {
    fn fixed(slot: StatusBarSlot, text: impl Into<String>) -> Self {
        let text = text.into();
        let width = UnicodeWidthStr::width(text.as_str());
        Self {
            slot,
            prefix: "",
            text,
            min_width: width,
            max_width: width,
        }
    }

    fn flexible(
        slot: StatusBarSlot,
        prefix: &'static str,
        text: impl Into<String>,
        min_width: usize,
        max_width: usize,
    ) -> Self {
        Self {
            slot,
            prefix,
            text: text.into(),
            min_width,
            max_width,
        }
    }

    fn natural_width(&self) -> usize {
        let raw = UnicodeWidthStr::width(self.prefix) + UnicodeWidthStr::width(self.text.as_str());
        raw.clamp(self.min_width.min(self.max_width), self.max_width)
    }

    /// 按分配到的宽度渲染文本：前缀保留，正文按显示宽度截断。
    fn render_text(&self, width: usize) -> String {
        let prefix_width = UnicodeWidthStr::width(self.prefix);
        if prefix_width >= width {
            return truncate(self.prefix, width);
        }
        format!(
            "{}{}",
            self.prefix,
            truncate(&self.text, width - prefix_width)
        )
    }
}

/// 一行里待绘制的一段（绝对列位置 + 精确宽度 + 文本）。
#[derive(Debug, Clone)]
struct Piece {
    row: u16,
    x: u16,
    text: String,
    style: Style,
}

/// 按绝对列位置把若干段拼成一行，段间空隙原样补回。
///
/// 单独抽出来是为了能直接断言"渲染出来的列与命中矩形一致" —— 这是最容易写错、
/// 又最难在集成测试里发现的地方（段间分隔符丢了就会出现"看得见点不准"）。
fn build_row_line(pieces: &[Piece], row: u16) -> Line<'static> {
    let background_style = pieces
        .first()
        .map(|piece| Style::new().bg(piece.style.bg.unwrap_or_default()))
        .unwrap_or_default();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut column = 0usize;
    for piece in pieces.iter().filter(|piece| piece.row == row) {
        let target = piece.x as usize;
        if target > column {
            let gap = target - column;
            let filler = if gap == separator_width() {
                SEGMENT_SEPARATOR.to_string()
            } else {
                " ".repeat(gap)
            };
            spans.push(Span::styled(filler, background_style));
            column = target;
        }
        column += UnicodeWidthStr::width(piece.text.as_str());
        spans.push(Span::styled(piece.text.clone(), piece.style));
    }
    Line::from(spans)
}

/// 排布结果中的一段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Placed {
    /// 在候选列表中的下标（排布保持原顺序，但可能有段被丢弃）。
    index: usize,
    slot: StatusBarSlot,
    x: u16,
    width: u16,
    row: u16,
}

/// 一次排布的完整结果。
#[derive(Debug, Default, PartialEq, Eq)]
struct Arrangement {
    placed: Vec<Placed>,
    /// 放不下、被收纳进「更多」的可交互段（保持原顺序）。
    collapsed: Vec<StatusBarSlot>,
    more: Option<Placed>,
}

/// 单次线性排布：按顺序放进 `rows` 行，放不下的可交互段收进 `collapsed`。
///
/// `expect_more` 为真时给末尾的「更多」预留宽度 —— 否则最窄的情况下会出现
/// "东西被收起来了、入口也被挤掉了"，等于彻底点不到。
fn arrange(
    candidates: &[(Candidate, Style)],
    widths: &[usize],
    width: usize,
    rows: usize,
    expect_more: bool,
) -> Arrangement {
    let more_width = UnicodeWidthStr::width(MORE_LABEL);
    let reserve = if expect_more {
        separator_width() + more_width
    } else {
        0
    };
    let effective = width.saturating_sub(reserve);
    let rows = rows.max(1);

    let mut arrangement = Arrangement::default();
    if effective == 0 {
        arrangement.collapsed = candidates
            .iter()
            .map(|(candidate, _)| candidate.slot)
            .filter(|slot| slot_is_interactive(*slot))
            .collect();
        return arrangement;
    }

    let mut used = 0usize;
    let mut row = 0usize;
    for (index, (candidate, _)) in candidates.iter().enumerate() {
        let segment = widths[index];
        if segment == 0 {
            continue;
        }
        if used > 0 && used + separator_width() + segment > effective && row + 1 < rows {
            row += 1;
            used = 0;
        }
        let need = if used == 0 {
            segment
        } else {
            separator_width() + segment
        };
        if used + need > effective {
            if slot_is_interactive(candidate.slot) {
                arrangement.collapsed.push(candidate.slot);
            }
            continue;
        }
        let x = if used == 0 {
            0
        } else {
            used + separator_width()
        };
        arrangement.placed.push(Placed {
            index,
            slot: candidate.slot,
            x: x as u16,
            width: segment as u16,
            row: row as u16,
        });
        used = x + segment;
    }

    if expect_more && !arrangement.collapsed.is_empty() {
        if used > 0 && used + separator_width() + more_width > width && row + 1 < rows {
            row += 1;
            used = 0;
        }
        let need = if used == 0 {
            more_width
        } else {
            separator_width() + more_width
        };
        if used + need <= width {
            let x = if used == 0 {
                0
            } else {
                used + separator_width()
            };
            arrangement.more = Some(Placed {
                index: candidates.len(),
                slot: StatusBarSlot::More,
                x: x as u16,
                width: more_width as u16,
                row: row as u16,
            });
        }
    }
    arrangement
}

/// 宽度分配：先压缩可压缩段，再决定谁被丢弃 / 收纳。
fn allocate(candidates: &[(Candidate, Style)], width: u16, rows: u16) -> (Arrangement, Vec<usize>) {
    let width = width as usize;
    let rows = rows.max(1) as usize;
    let mut widths: Vec<usize> = candidates
        .iter()
        .map(|(candidate, _)| candidate.natural_width())
        .collect();
    if width == 0 {
        return (Arrangement::default(), widths);
    }

    // 总宽超过总容量时，从"余量最大"的可压缩段开始各减 1 列：
    // 「歌曲」这类可伸缩字段先让位，后面的控件才不会被整段丢掉。
    let linear_total = |widths: &[usize]| -> usize {
        widths
            .iter()
            .enumerate()
            .map(|(index, value)| {
                if index == 0 {
                    *value
                } else {
                    separator_width() + *value
                }
            })
            .sum()
    };
    let mut guard = 0;
    while linear_total(&widths) > width * rows && guard < 4096 {
        let target = (0..widths.len())
            .filter(|index| widths[*index] > candidates[*index].0.min_width)
            .max_by_key(|index| widths[*index] - candidates[*index].0.min_width);
        let Some(index) = target else { break };
        widths[index] -= 1;
        guard += 1;
    }

    let plain = arrange(candidates, &widths, width, rows, false);
    if plain.collapsed.is_empty() {
        return (plain, widths);
    }
    // 有控件被收纳 → 预留「更多」入口的宽度重排一次，保证入口一定存在。
    let reserved = arrange(candidates, &widths, width, rows, true);
    if reserved.more.is_some() {
        (reserved, widths)
    } else {
        (plain, widths)
    }
}

// ───────────────────────── 渲染 ─────────────────────────

/// 渲染状态栏，返回本帧实际排布出来的几何（供鼠标命中复用）。
///
/// `hovered` 用于给鼠标悬停的段加高亮；`resize_hover` 用于给顶边的
/// 拖拽把手加高亮（鼠标悬停或正在拖拽时为真）。
pub fn render(
    area: Rect,
    buf: &mut Buffer,
    ctx: &AppContext,
    sort_status: Option<&'static str>,
    sort_hint: Option<String>,
    hovered: Option<StatusBarSlot>,
    resize_hover: bool,
) -> StatusBarFrame {
    if area.height == 0 || area.width == 0 {
        return StatusBarFrame::default();
    }
    let rows = ctx
        .config
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .ui
        .status_bar_rows()
        .clamp(1, area.height.max(1));
    let background = crate::theme::mantle(ctx);
    let segments = candidates(area, ctx, sort_status, sort_hint);
    // 把手占掉顶行最右边几列：从排布宽度里扣掉，段就不会被把手盖住
    // （命中因此天然互斥，"点上却变成拖高度"不可能发生）。
    let handle = resize_handle(area);
    let layout_width = match handle {
        Some(_) => area.width.saturating_sub(RESIZE_HANDLE_WIDTH),
        None => area.width,
    };
    let (arrangement, widths) = allocate(&segments, layout_width, rows);

    Block::default()
        .style(Style::new().bg(background).fg(crate::theme::text(ctx)))
        .render(area, buf);

    // 按"绝对列位置"重建每一行：段与段之间的空隙必须原样补回来，
    // 否则渲染出来的列与上面算出的命中矩形会错位（这正是本项目修过好几次
    // 的那类"看得见点不准"问题）。
    let mut pieces: Vec<Piece> = Vec::new();
    for placed in &arrangement.placed {
        let (candidate, style) = &segments[placed.index];
        let mut text = candidate.render_text(widths[placed.index].min(placed.width as usize));
        // 截断结果可能比分配宽度窄（例如只放得下省略号），补齐到精确宽度。
        let rendered = UnicodeWidthStr::width(text.as_str());
        if rendered < placed.width as usize {
            text.push_str(&" ".repeat(placed.width as usize - rendered));
        }
        let style = if hovered == Some(placed.slot) {
            style
                .bg(crate::theme::surface1(ctx))
                .add_modifier(Modifier::UNDERLINED | Modifier::BOLD)
        } else {
            *style
        };
        pieces.push(Piece {
            row: placed.row,
            x: placed.x,
            text,
            style,
        });
    }
    if let Some(more) = arrangement.more {
        let style = Style::new().fg(crate::theme::overlay1(ctx)).bg(background);
        let style = if hovered == Some(StatusBarSlot::More) {
            style
                .bg(crate::theme::surface1(ctx))
                .add_modifier(Modifier::UNDERLINED | Modifier::BOLD)
        } else {
            style
        };
        pieces.push(Piece {
            row: more.row,
            x: more.x,
            text: MORE_LABEL.to_string(),
            style,
        });
    }
    pieces.sort_by_key(|piece| (piece.row, piece.x));

    let lines: Vec<Line<'static>> = (0..rows).map(|row| build_row_line(&pieces, row)).collect();
    Paragraph::new(lines)
        .style(Style::new().bg(background))
        .render(Rect::new(area.x, area.y, area.width, rows), buf);

    // 顶边把手：常驻 accent 色（"这里能拖"），悬停 / 拖拽中再加亮。
    if let Some(handle) = handle {
        let style = if resize_hover {
            Style::new()
                .fg(crate::theme::accent(ctx))
                .bg(crate::theme::surface1(ctx))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new()
                .fg(crate::theme::accent(ctx))
                .bg(background)
                .add_modifier(Modifier::BOLD)
        };
        Paragraph::new(Line::from(Span::styled(RESIZE_HANDLE_GLYPH, style)))
            .style(Style::new().bg(background))
            .render(handle, buf);
    }

    let mut hits: Vec<StatusBarHit> = arrangement
        .placed
        .iter()
        .map(|placed| StatusBarHit {
            slot: placed.slot,
            rect: Rect::new(area.x + placed.x, area.y + placed.row, placed.width, 1),
        })
        .collect();
    if let Some(more) = arrangement.more {
        hits.push(StatusBarHit {
            slot: StatusBarSlot::More,
            rect: Rect::new(area.x + more.x, area.y + more.row, more.width, 1),
        });
    }
    StatusBarFrame {
        hits,
        collapsed: arrangement.collapsed,
        rows,
        handle,
    }
}

/// 组装候选段（文本 + 样式 + 宽度约束），顺序即配置顺序。
fn candidates(
    area: Rect,
    ctx: &AppContext,
    sort_status: Option<&'static str>,
    sort_hint: Option<String>,
) -> Vec<(Candidate, Style)> {
    let background = crate::theme::mantle(ctx);
    let state = *ctx.player_state.borrow();
    let current_song = ctx.current_song.read().unwrap_or_else(|e| e.into_inner());
    let position = *ctx.position.borrow();
    let duration = *ctx.duration.borrow();
    let audio_info = ctx.audio_info.borrow().clone();
    let volume = ctx.player.volume();
    let queue = ctx.playlist.borrow();
    let queue_index = ctx.playlist.current_index();
    let (quality, status_bar_items, policy, policy_platform) = {
        let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
        (
            config.player.quality,
            config.ui.status_bar_items.clone(),
            config.source.policy,
            config.source.policy_platform,
        )
    };

    let (state_text, state_color) = match state {
        PlayerState::Playing => ("播放", crate::theme::green(ctx)),
        PlayerState::Paused => ("暂停", crate::theme::yellow(ctx)),
        PlayerState::Loading => ("缓冲", crate::theme::sapphire(ctx)),
        PlayerState::Stopped => ("停止", crate::theme::overlay1(ctx)),
        PlayerState::Idle => ("空闲", crate::theme::overlay1(ctx)),
    };
    let time = if duration.is_zero() {
        format_duration(position)
    } else {
        format!(
            "{}/{}",
            format_duration(position),
            format_duration(duration)
        )
    };
    let mut song = current_song.as_ref().map_or_else(
        || "voicefox".to_string(),
        |song| {
            if song.singer.trim().is_empty() {
                song.name.clone()
            } else {
                format!("{} - {}", song.name, song.singer)
            }
        },
    );
    let source = current_song
        .as_ref()
        .map(|song| {
            // 锁中毒时退回原始数据渲染，避免状态栏每帧 panic
            let js_index = *ctx
                .play_js_source_index
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            js_index
                .and_then(|index| ctx.source_manager.js_source_name(index))
                .or_else(|| {
                    ctx.source_manager
                        .get(song.source)
                        .map(|source| source.name().to_string())
                })
                .unwrap_or_else(|| song.source.as_str().to_string())
        })
        .unwrap_or_else(|| "-".to_string());
    let js_status = ctx.js_source_status();
    let show_quality_item = status_bar_items.contains(&StatusBarItem::Quality);
    if !show_quality_item && let Some(audio_label) = audio_info.label() {
        song.push_str(&format!(" · {audio_label}"));
    }
    let queue_position = if queue.is_empty() {
        "0/0".to_string()
    } else {
        format!("{}/{}", queue_index.saturating_add(1), queue.len())
    };
    let mode = ctx.playlist.mode().label();
    let source_cap = match area.width {
        0..=49 => 8,
        50..=89 => 14,
        _ => 20,
    };

    let mut out: Vec<(Candidate, Style)> = Vec::new();
    // 有下载任务时放在最前，切页也能看到；它同时是打开下载面板的入口。
    if let Some(text) = download_indicator(ctx) {
        out.push((
            Candidate::fixed(StatusBarSlot::Download, text),
            Style::new()
                .fg(crate::theme::teal(ctx))
                .bg(background)
                .add_modifier(Modifier::BOLD),
        ));
    }
    // 睡眠定时器启用时紧随其后展示倒计时；点击打开定时器菜单。
    if let Some(label) = ctx.sleep_timer.status_label() {
        out.push((
            Candidate::fixed(StatusBarSlot::SleepTimer, label),
            Style::new()
                .fg(crate::theme::sapphire(ctx))
                .bg(background)
                .add_modifier(Modifier::BOLD),
        ));
    }
    for item in status_bar_items {
        // 可交互段铺一层浅底（"按钮片"），只读段保持与背景同色 ——
        // 不用说明文字，用户也能一眼看出哪几段能点。
        let chip = if is_interactive(item) {
            crate::theme::surface0(ctx)
        } else {
            background
        };
        let entry = match item {
            StatusBarItem::State => Some((
                Candidate::fixed(StatusBarSlot::Item(item), format!(" {state_text} ")),
                Style::new()
                    .fg(state_color)
                    .bg(crate::theme::surface0(ctx))
                    .add_modifier(Modifier::BOLD),
            )),
            StatusBarItem::Source => {
                let platform_name = policy_platform.map(|platform| {
                    ctx.source_manager
                        .get(platform)
                        .map(|source| source.name().to_string())
                        .unwrap_or_else(|| platform.as_str().to_string())
                });
                let policy_suffix = policy_suffix(policy, platform_name.as_deref());
                Some((
                    Candidate::flexible(
                        StatusBarSlot::Item(item),
                        "音源 ",
                        format!("{source}{policy_suffix}"),
                        9,
                        5 + source_cap,
                    ),
                    Style::new()
                        .fg(crate::theme::peach(ctx))
                        .bg(chip)
                        .add_modifier(Modifier::BOLD),
                ))
            }
            // 键位提示由调用方从真实键位配置反查传入（用户改键后不再失真）；
            // 没有绑定（用户删掉了 s）时省略提示段。
            StatusBarItem::Sort => sort_status.map(|sort_status| {
                let text = match sort_hint.as_deref() {
                    Some(hint) => format!("排序 {sort_status} ({hint})"),
                    None => format!("排序 {sort_status}"),
                };
                (
                    Candidate::fixed(StatusBarSlot::Item(item), text),
                    Style::new()
                        .fg(crate::theme::yellow(ctx))
                        .bg(background)
                        .add_modifier(Modifier::BOLD),
                )
            }),
            StatusBarItem::Song => Some((
                Candidate::flexible(StatusBarSlot::Item(item), "", song.clone(), 6, 28),
                Style::new()
                    .fg(crate::theme::text(ctx))
                    .bg(background)
                    .add_modifier(Modifier::BOLD),
            )),
            StatusBarItem::Time => Some((
                Candidate::fixed(StatusBarSlot::Item(item), time.clone()),
                Style::new().fg(crate::theme::subtext1(ctx)).bg(chip),
            )),
            StatusBarItem::Volume => Some((
                Candidate::fixed(StatusBarSlot::Item(item), format!("音量 {}%", volume)),
                Style::new().fg(crate::theme::sky(ctx)).bg(chip),
            )),
            StatusBarItem::PlayMode => Some((
                Candidate::fixed(StatusBarSlot::Item(item), mode.to_string()),
                Style::new().fg(crate::theme::lavender(ctx)).bg(chip),
            )),
            StatusBarItem::Quality => Some((
                Candidate::fixed(
                    StatusBarSlot::Item(item),
                    quality_segment_text(quality, audio_info.label()),
                ),
                Style::new().fg(crate::theme::peach(ctx)).bg(chip),
            )),
            StatusBarItem::Queue => Some((
                Candidate::fixed(
                    StatusBarSlot::Item(item),
                    format!("队列 {}", queue_position),
                ),
                Style::new().fg(crate::theme::teal(ctx)).bg(chip),
            )),
            StatusBarItem::JsSourceState => Some((
                Candidate::fixed(StatusBarSlot::Item(item), js_status.summary()),
                Style::new()
                    .fg(if js_status.is_healthy() {
                        crate::theme::green(ctx)
                    } else {
                        crate::theme::yellow(ctx)
                    })
                    .bg(chip),
            )),
        };
        if let Some(entry) = entry {
            out.push(entry);
        }
    }
    out
}

/// 音质段文本：显示**偏好**（点击改的就是它），实播音质不同才附在后面。
///
/// 反例（改动前的行为）：只显示实播音质 → 点它只弹通知、界面纹丝不动，
/// 用户会以为没生效。
fn quality_segment_text(preference: Quality, actual: Option<String>) -> String {
    let preference_label = preference.label();
    match actual {
        Some(actual) if actual != preference_label => {
            format!("{preference_label} · 实播 {actual}")
        }
        _ => preference_label.to_string(),
    }
}

/// 解析策略后缀：`auto` 不显示，其余把策略与目标平台显式写在音源段上，
/// 否则用户选了"只用网易"在界面上看不到任何变化。
fn policy_suffix(policy: SourcePolicy, platform_name: Option<&str>) -> String {
    match (policy, platform_name) {
        (SourcePolicy::Auto, _) => String::new(),
        (mode, Some(name)) => format!(" · {} {name}", mode.label()),
        (mode, None) => format!(" · {}", mode.label()),
    }
}

/// 状态栏上的下载进度摘要；没有进行中的任务时返回 `None`。
fn download_indicator(ctx: &AppContext) -> Option<String> {
    // 轻量摘要（锁内算好，零克隆）：底栏每帧走到这里，snapshot() 的全量
    // 任务视图深克隆在下载记录堆积后是每帧固定开销。
    let (active, percent) = ctx.downloads.progress_summary()?;
    Some(match percent {
        Some(percent) => format!("下载 {active} 项 {percent}%"),
        None => format!("下载 {active} 项"),
    })
}

fn separator_width() -> usize {
    UnicodeWidthStr::width(SEGMENT_SEPARATOR)
}

/// 按显示宽度截断（复用共享实现；这里只需要 `String`）。
fn truncate(value: &str, width: usize) -> String {
    truncate_width(value, width).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_only(text: &str) -> (Candidate, Style) {
        (
            Candidate::fixed(StatusBarSlot::Item(StatusBarItem::Time), text),
            Style::default(),
        )
    }

    /// 不可压缩的控件段（固定宽度）。
    fn control(item: StatusBarItem, text: &str) -> (Candidate, Style) {
        (
            Candidate::fixed(StatusBarSlot::Item(item), text),
            Style::default(),
        )
    }

    fn hits_of(arrangement: &Arrangement, y: u16) -> Vec<StatusBarHit> {
        let mut hits: Vec<StatusBarHit> = arrangement
            .placed
            .iter()
            .map(|placed| StatusBarHit {
                slot: placed.slot,
                rect: Rect::new(placed.x, y + placed.row, placed.width, 1),
            })
            .collect();
        if let Some(more) = arrangement.more {
            hits.push(StatusBarHit {
                slot: StatusBarSlot::More,
                rect: Rect::new(more.x, y + more.row, more.width, 1),
            });
        }
        hits
    }

    /// 命中必须与排布完全一致：段内任意可见列都要命中它自己。
    #[test]
    fn every_visible_column_of_a_segment_hits_itself() {
        let segments = vec![read_only("aaaa"), read_only("bb"), read_only("ccccccc")];
        let (arrangement, _) = allocate(&segments, 60, 1);
        assert_eq!(arrangement.placed.len(), 3);
        let hits = hits_of(&arrangement, 0);
        for placed in &arrangement.placed {
            for column in placed.x..placed.x + placed.width {
                assert_eq!(
                    hit_test(&hits, column, placed.row),
                    Some(placed.slot),
                    "第 {column} 列应当命中它所在的段"
                );
            }
        }
        // 段与段之间是分隔符，不属于任何可点区域
        assert_eq!(hit_test(&hits, arrangement.placed[0].x + 4, 0), None);
    }

    /// 放不下的可交互段收进「更多」，且入口自己必须放得下。
    #[test]
    fn controls_that_do_not_fit_are_collapsed_into_the_more_entry() {
        let segments = vec![
            control(StatusBarItem::State, " 播放 "),
            control(StatusBarItem::Volume, "音量 100%"),
            control(StatusBarItem::Queue, "队列 52/100"),
            control(StatusBarItem::JsSourceState, "● 自定义音源 3/3"),
        ];
        let (arrangement, _) = allocate(&segments, 24, 1);
        assert!(
            arrangement.more.is_some(),
            "有控件被收纳时必须给出「更多」入口"
        );
        assert!(!arrangement.collapsed.is_empty());
        let more = arrangement.more.unwrap();
        assert!(
            more.x as usize + more.width as usize <= 24,
            "「更多」入口不能越界"
        );
        // 收纳入口必须可命中
        let hits = hits_of(&arrangement, 0);
        assert_eq!(
            hit_test(&hits, more.x + more.width - 1, more.row),
            Some(StatusBarSlot::More)
        );
    }

    /// 只读段（时间/排序）挤不下就直接丢，不会因此弹出「更多」。
    #[test]
    fn read_only_segments_are_dropped_without_the_more_entry() {
        let segments = vec![
            control(StatusBarItem::State, " 播放 "),
            read_only("00:09/03:45"),
            read_only("00:09/03:45"),
            read_only("00:09/03:45"),
        ];
        let (arrangement, _) = allocate(&segments, 20, 1);
        assert!(arrangement.collapsed.is_empty());
        assert!(arrangement.more.is_none());
        assert!(
            arrangement.placed.len() < segments.len(),
            "多余的只读段应被丢弃"
        );
    }

    /// 可压缩段先让位，后面的控件不该因为前面太宽而消失。
    #[test]
    fn flexible_segments_shrink_before_controls_are_collapsed() {
        let segments = vec![
            control(StatusBarItem::State, " 播放 "),
            (
                Candidate::flexible(
                    StatusBarSlot::Item(StatusBarItem::Song),
                    "",
                    "一个很长很长的歌曲名字 - 一个很长很长的歌手名字",
                    6,
                    28,
                ),
                Style::default(),
            ),
            control(StatusBarItem::Queue, "队列 52/100"),
        ];
        // 状态 4 + 歌曲(5+6..28) + 队列(5+11)：34 列时应当靠压缩歌曲放下全部三段
        let (arrangement, widths) = allocate(&segments, 34, 1);
        assert!(
            arrangement.collapsed.is_empty(),
            "歌曲段应当被压缩让位，而不是把队列收起来：{:?}",
            arrangement.collapsed
        );
        assert!(widths[1] < 28, "歌曲段确实被压缩了：{}", widths[1]);
        assert_eq!(arrangement.placed.len(), 3);
    }

    /// 两行布局：排不下的段换到第二行，命中要用第二行的 y。
    #[test]
    fn two_row_layout_wraps_and_reports_the_second_row() {
        let segments = vec![
            control(StatusBarItem::State, " 播放 "),
            control(StatusBarItem::Volume, "音量 100%"),
            control(StatusBarItem::Queue, "队列 52/100"),
        ];
        let (arrangement, _) = allocate(&segments, 20, 2);
        assert!(arrangement.collapsed.is_empty(), "两行足够放下这三段");
        let rows: Vec<u16> = arrangement.placed.iter().map(|placed| placed.row).collect();
        assert_eq!(rows, vec![0, 0, 1], "第三段应换到第二行：{rows:?}");

        // 第二行第一段的坐标必须带行号，否则鼠标命中会算到第一行上
        let second = arrangement
            .placed
            .iter()
            .find(|placed| placed.row == 1)
            .expect("有第二行");
        let hits = hits_of(&arrangement, 5);
        assert_eq!(hit_test(&hits, second.x, 5 + second.row), Some(second.slot));
        assert_ne!(
            hit_test(&hits, second.x, 5),
            Some(second.slot),
            "行号必须参与命中：同一列的第一行不该命中第二行的段"
        );
    }

    #[test]
    fn a_zero_width_bar_never_panics() {
        let segments = vec![read_only("aaaa"), read_only("bb")];
        let (arrangement, _) = allocate(&segments, 0, 1);
        assert!(arrangement.placed.is_empty());
        assert!(arrangement.more.is_none());
    }

    /// 渲染出来的每一段必须正好落在命中矩形的那几列上。
    ///
    /// 锁住的是"段间分隔符有没有被补回来"以及"段与段之间的列有没有错位"——
    /// 丢了任何一个，都会变成看得见却点不准。
    #[test]
    fn rendered_columns_match_the_hit_rectangles() {
        let pieces = vec![
            Piece {
                row: 0,
                x: 0,
                text: " 播放 ".to_string(),
                style: Style::default(),
            },
            // 中间隔了 5 列分隔符（x=4 → 9）
            Piece {
                row: 0,
                x: 9,
                text: "队列 1/10  ".to_string(),
                style: Style::default(),
            },
            Piece {
                row: 1,
                x: 0,
                text: "第二行".to_string(),
                style: Style::default(),
            },
        ];

        let line = build_row_line(&pieces, 0);
        let mut column = 0usize;
        let mut starts: Vec<(String, usize)> = Vec::new();
        for span in &line.spans {
            starts.push((span.content.to_string(), column));
            column += UnicodeWidthStr::width(span.content.as_ref());
        }
        for piece in pieces.iter().filter(|piece| piece.row == 0) {
            let (_, start) = starts
                .iter()
                .find(|(text, _)| text == &piece.text)
                .unwrap_or_else(|| panic!("段 {:?} 必须出现在该行里", piece.text));
            assert_eq!(
                *start, piece.x as usize,
                "段 {:?} 的渲染起始列必须等于命中矩形的 x",
                piece.text
            );
        }
        // 段间空隙必须补回来（否则第二段会紧贴第一段，与命中矩形错位）
        assert!(starts.len() > 2, "两段之间应当有分隔符 span：{starts:?}");

        // 第二行独立重建，不会混入第一行的内容
        let second: String = build_row_line(&pieces, 1)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(second, "第二行");
    }

    /// 音质段必须显示"偏好"：点击后界面要有变化。
    #[test]
    fn quality_segment_shows_the_preference_that_the_click_changes() {
        // 没有实播信息：显示偏好本身
        assert_eq!(
            quality_segment_text(Quality::Flac, None),
            Quality::Flac.label()
        );
        // 实播与偏好一致：不啰嗦
        assert_eq!(
            quality_segment_text(Quality::High320, Some(Quality::High320.label().to_string())),
            Quality::High320.label()
        );
        // 实播与偏好不同：偏好在前（点了会变），实播在后
        let text = quality_segment_text(Quality::Flac, Some("MP3 320K".to_string()));
        assert!(text.starts_with(Quality::Flac.label()), "{text}");
        assert!(text.contains("实播 MP3 320K"), "{text}");

        // 关键回归：切换偏好后显示文本必须跟着变
        let before = quality_segment_text(Quality::High320, Some("MP3 320K".to_string()));
        let after = quality_segment_text(Quality::Flac, Some("MP3 320K".to_string()));
        assert_ne!(before, after, "点了音质循环，状态栏文字必须变化");
    }

    /// 解析策略必须写在音源段上，否则"选了策略"看不见。
    #[test]
    fn policy_is_visible_on_the_source_segment() {
        assert_eq!(policy_suffix(SourcePolicy::Auto, None), "");
        assert_eq!(
            policy_suffix(SourcePolicy::Only, Some("网易云音乐")),
            " · 只用指定平台 网易云音乐"
        );
        assert_eq!(
            policy_suffix(SourcePolicy::Prefer, Some("酷狗音乐")),
            " · 优先指定平台 酷狗音乐"
        );
        assert_eq!(policy_suffix(SourcePolicy::Only, None), " · 只用指定平台");
    }

    #[test]
    fn truncation_keeps_the_prefix() {
        let candidate = Candidate::flexible(
            StatusBarSlot::Item(StatusBarItem::Source),
            "音源 ",
            "聚合API接口（CF）",
            9,
            25,
        );
        // "聚合API接口（CF）" 显示宽度 17，加前缀 5 列 = 22（未触及上限 25）
        assert_eq!(candidate.natural_width(), 22);

        // 更长的正文才会顶到 max_width
        let long = Candidate::flexible(
            StatusBarSlot::Item(StatusBarItem::Source),
            "音源 ",
            "一个非常非常非常长的自定义音源名字",
            9,
            25,
        );
        assert_eq!(long.natural_width(), 25, "超过上限要夹住");

        let text = candidate.render_text(12);
        assert!(text.starts_with("音源 "), "前缀必须保留：{text}");
        assert!(UnicodeWidthStr::width(text.as_str()) <= 12);
    }

    /// 十项全开 + 160 列（用户的实际配置）必须全都放得下，不触发收纳。
    #[test]
    fn a_wide_terminal_fits_every_enabled_item() {
        let mut segments = vec![control(StatusBarItem::State, " 播放 ")];
        segments.push((
            Candidate::flexible(
                StatusBarSlot::Item(StatusBarItem::Song),
                "",
                "晴天 - 周杰伦",
                6,
                28,
            ),
            Style::default(),
        ));
        segments.push(control(StatusBarItem::Volume, "音量 100%"));
        segments.push(read_only("00:09/03:45"));
        segments.push(control(StatusBarItem::PlayMode, "列表循环"));
        segments.push((
            Candidate::flexible(
                StatusBarSlot::Item(StatusBarItem::Source),
                "音源 ",
                "聚合API接口（CF）",
                9,
                25,
            ),
            Style::default(),
        ));
        segments.push(control(StatusBarItem::Quality, "MP3 320K"));
        segments.push(control(StatusBarItem::Queue, "队列 52/100"));
        segments.push(control(StatusBarItem::JsSourceState, "● 自定义音源 3/3"));

        let (arrangement, _) = allocate(&segments, 160, 1);
        assert!(arrangement.more.is_none(), "160 列不该需要收纳");
        assert_eq!(arrangement.placed.len(), segments.len());
    }

    /// 把手必须落在顶边最右端，并且**排布已经为它让开宽度** ——
    /// 于是任何段都不可能和把手重叠，"点上却变成拖高度"从几何上被排除。
    #[test]
    fn the_resize_handle_sits_on_the_top_edge_and_never_overlaps_a_segment() {
        let area = Rect::new(3, 20, 40, 1);
        let handle = resize_handle(area).expect("宽度足够时必须有把手");
        assert_eq!(handle.y, area.y, "把手在顶边");
        assert_eq!(handle.height, 1);
        assert_eq!(handle.right(), area.right(), "把手贴着右边缘");
        assert_eq!(handle.width, RESIZE_HANDLE_WIDTH);

        let layout_width = area.width - RESIZE_HANDLE_WIDTH;
        let segments = vec![
            control(StatusBarItem::State, " 播放 "),
            read_only("00:00/03:00"),
            control(StatusBarItem::Volume, "音量 100%"),
        ];
        let (arrangement, _) = allocate(&segments, layout_width, 1);
        assert!(!arrangement.placed.is_empty(), "扣掉把手宽度后仍要画得出段");
        for placed in &arrangement.placed {
            assert!(
                placed.x + placed.width <= layout_width,
                "段 {:?} 伸进了把手占的列里",
                placed.slot
            );
        }

        // 过窄 / 空面板不给把手：宁可没有把手，也不能把内容全挤掉
        assert!(resize_handle(Rect::new(0, 0, RESIZE_HANDLE_WIDTH, 1)).is_none());
        assert!(resize_handle(Rect::new(0, 0, 40, 0)).is_none());
    }

    /// 指针行 → 行数的纯映射（含上下夹取）。上下两条边界都要能拖到。
    #[test]
    fn pointer_row_maps_to_a_clamped_row_count() {
        let bottom = 24; // 屏幕 24 行、底栏钉在底部 → 底边（半开）= 24
        assert_eq!(rows_for_pointer(bottom, 23), 1, "顶边就是当前 1 行");
        assert_eq!(rows_for_pointer(bottom, 22), 2);
        assert_eq!(rows_for_pointer(bottom, 18), 6, "6 行正好可拖到");

        // 往上越过上限 → 夹到 STATUS_BAR_MAX_HEIGHT
        assert_eq!(
            rows_for_pointer(bottom, 17),
            u16::from(STATUS_BAR_MAX_HEIGHT)
        );
        assert_eq!(
            rows_for_pointer(bottom, 0),
            u16::from(STATUS_BAR_MAX_HEIGHT)
        );
        // 往下 / 越过屏幕底边 → 至少留 1 行
        assert_eq!(rows_for_pointer(bottom, 24), 1);
        assert_eq!(rows_for_pointer(bottom, 99), 1);
        // 退化输入不 panic
        assert_eq!(rows_for_pointer(0, 0), 1);
    }

    /// 行数变了之后，命中账本必须跟着排布走（hits 与渲染同源）。
    #[test]
    fn hits_follow_the_row_count_the_bar_was_laid_out_with() {
        let segments = vec![
            control(StatusBarItem::State, " 播放 "),
            control(StatusBarItem::Volume, "音量 100%"),
            control(StatusBarItem::Queue, "队列 52/100"),
            control(StatusBarItem::PlayMode, "列表循环"),
        ];
        // 20 列 × 3 行：一行放不下，必须折到下面几行；宽度又够宽，
        // 不会触发「更多」收纳（收纳会进一步压缩可用宽度）。
        let (arrangement, _) = allocate(&segments, 20, 3);
        assert!(
            arrangement.placed.iter().any(|placed| placed.row > 0),
            "3 行排布必须真的用到第二行"
        );
        assert!(
            arrangement.placed.iter().all(|placed| placed.row < 3),
            "排布不能越过给定的行数"
        );

        let hits = hits_of(&arrangement, 10);
        for placed in &arrangement.placed {
            let row = 10 + placed.row;
            assert_eq!(
                hit_test(&hits, placed.x, row),
                Some(placed.slot),
                "段的首列必须命中它自己（行 {row}）"
            );
            assert_eq!(
                hit_test(&hits, placed.x + placed.width - 1, row),
                Some(placed.slot),
                "段的末列必须命中它自己（行 {row}）"
            );
        }
    }
}
