use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use lx_core::model::config::TableColumnConfig;
use lx_core::model::song::SongInfo;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::pages::components::text::{pad_display, pad_display_left, truncate_width};

use crate::context::AppContext;

/// 列之间的分隔符。同色的纯文本流里，短值后面的 padding 会让两列糊在一起，
/// 一个字符的竖线就能提供稳定的视觉边界（代价是每列少 1 个字符的文本宽度，
/// **总宽度不变**，所以命中测试与列宽持久化完全不受影响）。
pub const COLUMN_SEPARATOR: &str = "│";

/// 表格配色。组件本身不依赖 `AppContext`，由页面从主题取色后传进来。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TablePalette {
    /// 列分隔符颜色（要在普通行与选中行底色上都看得见）。
    pub separator: Color,
    /// 表头文字颜色。
    pub header_fg: Color,
    /// 表头底色（横贯整行的色带，把表头和数据行分开）。
    pub header_bg: Color,
}

impl TablePalette {
    pub fn from_theme(ctx: &AppContext) -> Self {
        Self {
            // 分隔符用 overlay1（比 overlay0 亮一档）：几列挨在一起时，
            // "哪一列到哪一列结束"必须一眼看得出来。
            separator: crate::theme::overlay1(ctx),
            // 表头文字比数据行亮一档 + 色带更亮一档，与数据行分层。
            header_fg: crate::theme::subtext1(ctx),
            header_bg: crate::theme::surface1(ctx),
        }
    }

    /// 表头文字的样式（加粗 + 色带底色）。
    /// 表头单元格样式：按列层级再加粗，和下面的数据列一一对应。
    pub fn header_cell_style(&self, key: &str) -> Style {
        Style::new()
            .fg(self.header_fg)
            .bg(self.header_bg)
            .add_modifier(Modifier::BOLD | column_emphasis(key))
    }

    /// 表头整行底色，用于 `Paragraph::style`，让色带横贯整行。
    pub fn header_band(&self) -> Style {
        Style::new().bg(self.header_bg)
    }

    fn separator_style(&self) -> Style {
        Style::new().fg(self.separator)
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedColumn {
    pub original_index: usize,
    pub start_x: u16,
    pub width: u16,
}

/// 一次列宽拖拽的会话状态。
#[derive(Debug, Clone)]
pub struct ColumnResizeState {
    start_x: u16,
    boundary_index: usize,
    starting_columns: Vec<TableColumnConfig>,
}

impl ColumnResizeState {
    fn begin(
        columns: &[TableColumnConfig],
        boundary_index: usize,
        total_width: u16,
        start_x: u16,
    ) -> Self {
        let mut starting_columns = columns.to_vec();
        for resolved in compute_layout(columns, total_width) {
            let column = &mut starting_columns[resolved.original_index];
            column.width = resolved.width.clamp(column.min_width, column.max_width);
        }
        Self {
            start_x,
            boundary_index,
            starting_columns,
        }
    }

    fn resized_columns(&self, total_width: u16, pointer_x: u16) -> Vec<TableColumnConfig> {
        adjust_widths(
            &self.starting_columns,
            self.boundary_index,
            total_width,
            i32::from(pointer_x) - i32::from(self.start_x),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnResizeOutcome {
    NotHandled,
    Updated,
    Finished,
}

/// 在所有歌曲表格页面共用列宽拖拽事件处理。
pub fn handle_column_resize(
    state: &mut Option<ColumnResizeState>,
    columns: &mut Vec<TableColumnConfig>,
    event: MouseEvent,
    header: Option<Rect>,
    inner: Rect,
) -> ColumnResizeOutcome {
    if let Some(resize) = state.as_ref() {
        // 终端和平台对“按住拖动”的事件分发并不一致：有些会发 `Drag`，
        // 有些则直接持续发 `Moved`。两者都要实时更新，才能避免“拖住了但列宽不动”的错觉。
        return match event.kind {
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved => {
                *columns = resize.resized_columns(inner.width, event.column);
                ColumnResizeOutcome::Updated
            }
            MouseEventKind::Up(MouseButton::Left) => {
                *state = None;
                ColumnResizeOutcome::Finished
            }
            _ => ColumnResizeOutcome::Updated,
        };
    }

    if matches!(event.kind, MouseEventKind::Down(MouseButton::Left))
        && header.is_some_and(|header| header.contains(Position::new(event.column, event.row)))
    {
        let local_x = event.column.saturating_sub(inner.x);
        if let Some(boundary) = find_boundary(columns, inner.width, local_x) {
            let layout = compute_layout(columns, inner.width);
            if boundary + 1 < layout.len() {
                *state = Some(ColumnResizeState::begin(
                    columns,
                    boundary,
                    inner.width,
                    event.column,
                ));
                return ColumnResizeOutcome::Updated;
            }
        }
    }

    ColumnResizeOutcome::NotHandled
}

pub fn adjust_widths(
    columns: &[TableColumnConfig],
    boundary_index: usize,
    total_width: u16,
    delta_x: i32,
) -> Vec<TableColumnConfig> {
    let visible: Vec<(usize, &TableColumnConfig)> = columns
        .iter()
        .enumerate()
        .filter(|(_, c)| c.visible)
        .collect();

    if boundary_index + 1 >= visible.len() {
        return columns.to_vec();
    }

    let layout = compute_layout(columns, total_width);
    if boundary_index + 1 >= layout.len() {
        return columns.to_vec();
    }

    let left_layout = &layout[boundary_index];
    let right_layout = &layout[boundary_index + 1];
    let left_col = &columns[left_layout.original_index];
    let right_col = &columns[right_layout.original_index];

    let current_left = left_col.width as i32;
    let current_right = right_col.width as i32;
    let mut new_left = current_left.saturating_add(delta_x);
    let mut new_right = current_right.saturating_sub(delta_x);

    if new_left < left_col.min_width as i32 {
        new_right = (current_left + current_right).saturating_sub(left_col.min_width as i32);
        new_left = left_col.min_width as i32;
    }
    if new_right < right_col.min_width as i32 {
        new_left = (current_left + current_right).saturating_sub(right_col.min_width as i32);
        new_right = right_col.min_width as i32;
    }

    new_left = new_left.clamp(left_col.min_width as i32, left_col.max_width as i32);
    new_right = new_right.clamp(right_col.min_width as i32, right_col.max_width as i32);

    let mut result = columns.to_vec();
    result[left_layout.original_index].width = new_left as u16;
    result[right_layout.original_index].width = new_right as u16;
    result
}

/// 读取某页面的列配置；没有配置或配置不可用时回落到默认档位。
///
/// 配置来自用户可手写的 `config.toml`，因此这里必须做一次校正：
/// - `min_width > max_width` 会让 [`adjust_widths`] 里的 `Ord::clamp` **panic**；
/// - `width = 0` 会让该列在界面上静默消失；
/// - `key` 为空表示这行配置没有意义，直接丢弃。
pub fn load_columns_for_page(
    columns: &std::collections::HashMap<String, Vec<TableColumnConfig>>,
    page_key: &str,
    width: u16,
) -> Vec<TableColumnConfig> {
    columns
        .get(page_key)
        .map(|configured| {
            configured
                .iter()
                .cloned()
                .filter_map(sanitize_column)
                .collect::<Vec<_>>()
        })
        .filter(|cs| !cs.is_empty())
        .unwrap_or_else(|| default_columns_for_page(page_key, width))
}

/// 页面级默认列：本地音乐页的「来源」列恒为「本地」，信息熵为零，
/// 默认隐藏（用户显式配置过该页列时尊重用户配置）。
fn default_columns_for_page(page_key: &str, width: u16) -> Vec<TableColumnConfig> {
    if page_key == "local_music" {
        default_local_columns(width)
    } else {
        default_columns(width)
    }
}

/// 校正单列配置，返回 `None` 表示这行配置应被丢弃。
fn sanitize_column(mut column: TableColumnConfig) -> Option<TableColumnConfig> {
    if column.key.trim().is_empty() {
        return None;
    }
    column.min_width = column.min_width.max(1);
    column.max_width = column.max_width.max(column.min_width);
    column.width = column.width.clamp(column.min_width, column.max_width);
    if column.label.trim().is_empty() {
        column.label = column.key.clone();
    }
    Some(column)
}

pub fn default_columns(width: u16) -> Vec<TableColumnConfig> {
    if width >= 96 {
        default_wide()
    } else if width >= 64 {
        default_medium()
    } else {
        default_narrow(width)
    }
}

fn default_local_columns(width: u16) -> Vec<TableColumnConfig> {
    let mut columns = default_columns(width);
    for column in &mut columns {
        if column.key == "duration" {
            column.key = "duration_quality".to_string();
            column.label = "播放".to_string();
            column.width = 12;
            column.min_width = 8;
            column.max_width = 16;
        }
    }
    columns.retain(|column| column.key != "quality" && column.key != "source");
    columns
}

fn default_wide() -> Vec<TableColumnConfig> {
    vec![
        col("index", "#", 4, 2, 6),
        col("name", "歌曲", 24, 4, 64),
        col("singer", "歌手", 20, 4, 40),
        col("album", "专辑", 16, 4, 40),
        col("duration", "时长", 7, 5, 9),
        col("quality", "音质", 9, 3, 12),
        col("source", "来源", 7, 3, 10),
    ]
}

fn default_medium() -> Vec<TableColumnConfig> {
    vec![
        col("index", "#", 4, 2, 6),
        col("name", "歌曲", 22, 4, 64),
        col("singer", "歌手", 20, 4, 40),
        col("duration", "时长", 7, 5, 9),
        col("source", "来源", 7, 3, 10),
    ]
}

fn default_narrow(width: u16) -> Vec<TableColumnConfig> {
    let name_w = width.saturating_sub(4 + 7).max(10);
    vec![
        col("index", "#", 4, 2, 6),
        col("name", "歌曲", name_w, 4, 64),
        col("source", "来源", 7, 3, 10),
    ]
}

fn col(key: &str, label: &str, width: u16, min: u16, max: u16) -> TableColumnConfig {
    TableColumnConfig {
        key: key.to_string(),
        label: label.to_string(),
        visible: true,
        width,
        min_width: min,
        max_width: max,
    }
}

pub fn compute_layout(columns: &[TableColumnConfig], total_width: u16) -> Vec<ResolvedColumn> {
    let visible: Vec<(usize, &TableColumnConfig)> = columns
        .iter()
        .enumerate()
        .filter(|(_, c)| c.visible)
        .collect();

    let total_preferred: u16 = visible.iter().map(|(_, c)| c.width).sum();
    let shortfall = total_width.saturating_sub(total_preferred);

    if shortfall == 0 || visible.is_empty() {
        let mut x: u16 = 0;
        return visible
            .iter()
            .map(|(orig, c)| {
                let col = ResolvedColumn {
                    original_index: *orig,
                    start_x: x,
                    width: c.width.min(total_width.saturating_sub(x)),
                };
                x = x.saturating_add(col.width);
                col
            })
            .collect();
    }

    let flexible_total: u16 = visible
        .iter()
        .map(|(_, c)| c.max_width.saturating_sub(c.width))
        .sum();

    if flexible_total == 0 {
        let mut x: u16 = 0;
        return visible
            .iter()
            .map(|(orig, c)| {
                let w = c.width.min(total_width.saturating_sub(x));
                let col = ResolvedColumn {
                    original_index: *orig,
                    start_x: x,
                    width: w,
                };
                x = x.saturating_add(w);
                col
            })
            .collect();
    }

    let mut x: u16 = 0;
    let mut remaining = shortfall;
    let mut result = Vec::with_capacity(visible.len());
    for (orig, c) in &visible {
        let max_gain = c.max_width.saturating_sub(c.width);
        let proportional =
            ((shortfall as f32) * (max_gain as f32) / (flexible_total as f32)).round() as u16;
        let gain = proportional.min(max_gain).min(remaining);
        let w = c.width.saturating_add(gain);
        remaining = remaining.saturating_sub(gain);
        let actual_w = w.min(total_width.saturating_sub(x));
        let col = ResolvedColumn {
            original_index: *orig,
            start_x: x,
            width: actual_w,
        };
        x = x.saturating_add(actual_w);
        result.push(col);
    }

    if remaining > 0 && !result.is_empty() {
        let last = result.last_mut().unwrap();
        last.width = last.width.saturating_add(remaining);
    }
    result
}

/// 命中表头上的列分隔线：返回分隔线左侧那一列在 `columns` 中的下标。
///
/// 拖动分隔线会让左右两列各自增减宽度，因此命中范围取分隔线左右各一个字符。
pub fn find_boundary(
    columns: &[TableColumnConfig],
    total_width: u16,
    local_x: u16,
) -> Option<usize> {
    if local_x >= total_width {
        return None;
    }
    let layout = compute_layout(columns, total_width);
    for (i, rc) in layout.iter().enumerate() {
        let boundary_x = rc.start_x.saturating_add(rc.width);
        if i < layout.len() - 1
            && local_x >= boundary_x.saturating_sub(1)
            && local_x <= boundary_x.saturating_add(1)
        {
            return Some(i);
        }
    }
    None
}

/// 单元格对齐方式。数字列右对齐，文本列左对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellAlign {
    Left,
    Right,
}

/// 该列是否按数字右对齐（`#` 与 `时长`）。
fn align_for_key(key: &str) -> CellAlign {
    match key {
        "index" | "duration" | "duration_quality" => CellAlign::Right,
        _ => CellAlign::Left,
    }
}

/// 把 `value` 渲染成恰好 `width` 显示宽度的单元格（超宽则截断并加 `…`）。
///
/// 收敛到共享的 [`truncate_width`]（按显示宽度裁到 `width-1` 再补 `…`）
/// 与 [`pad_display`] / [`pad_display_left`]（补齐到定宽）。右对齐时省略号
/// 仍留在末尾、空格补在前面，与旧的本地实现一致。
fn cell_aligned(value: &str, width: usize, align: CellAlign) -> String {
    if width == 0 {
        return String::new();
    }
    let value = value.trim();
    if UnicodeWidthStr::width(value) <= width {
        return match align {
            CellAlign::Left => pad_display(value, width),
            CellAlign::Right => pad_display_left(value, width),
        };
    }
    match align {
        CellAlign::Left => pad_display(truncate_width(value, width).as_ref(), width),
        // 右对齐：省略号在最右，剩余空格补在左边
        CellAlign::Right => {
            let content = truncate_width(value, width).into_owned();
            let padding = width.saturating_sub(UnicodeWidthStr::width(content.as_str()));
            format!("{}{content}", " ".repeat(padding))
        }
    }
}

/// 一列的可渲染宽度与分隔符占位。
///
/// 非最后一列预留 1 个字符给分隔符（列宽 < 2 时没有余量，就不画分隔符），
/// 因此**一行渲染出来的总宽度恒等于传入的 `width`**。
fn cell_budget(column_width: u16, is_last: bool) -> (usize, bool) {
    if is_last || column_width < 2 {
        (column_width as usize, false)
    } else {
        ((column_width - 1) as usize, true)
    }
}

fn header_line(width: u16, columns: &[TableColumnConfig], palette: TablePalette) -> Line<'static> {
    let layout = compute_layout(columns, width);
    let last = layout.len().saturating_sub(1);
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(layout.len() * 2);
    for (i, rc) in layout.iter().enumerate() {
        let column = &columns[rc.original_index];
        let (text_width, with_separator) = cell_budget(rc.width, i == last);
        spans.push(Span::styled(
            cell_aligned(&column.label, text_width, align_for_key(&column.key)),
            palette.header_cell_style(&column.key),
        ));
        if with_separator {
            spans.push(Span::styled(
                COLUMN_SEPARATOR,
                palette.separator_style().bg(palette.header_bg),
            ));
        }
    }
    Line::from(spans)
}

fn row_line(
    song: &SongInfo,
    index: usize,
    width: u16,
    columns: &[TableColumnConfig],
    palette: TablePalette,
) -> Line<'static> {
    let layout = compute_layout(columns, width);
    let last = layout.len().saturating_sub(1);

    let index_text = (index + 1).to_string();
    // 时长为 0（未取到）在表格里显示占位符，而不是误导性的 00:00。
    let duration_text = if song.duration.is_zero() {
        "--:--".to_string()
    } else {
        crate::fmt::format_duration(song.duration)
    };
    let quality_text = song.quality_label();
    let duration_quality_text = if song.duration.is_zero() {
        format!("--:-- · {quality_text}")
    } else {
        format!(
            "{} · {quality_text}",
            crate::fmt::format_duration(song.duration)
        )
    };
    // 显示中文名（网易云/酷我…）而不是原始代号 wy/kw——代号只用于配置与日志。
    let source_text = song.source.display_name();

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(layout.len() * 2);
    for (i, rc) in layout.iter().enumerate() {
        let column = &columns[rc.original_index];
        let (text_width, with_separator) = cell_budget(rc.width, i == last);
        let text = match column.key.as_str() {
            "index" => index_text.as_str(),
            "name" => &song.name,
            "singer" => &song.singer,
            "album" => &song.album_name,
            "duration" => &duration_text,
            "quality" => &quality_text,
            "duration_quality" => &duration_quality_text,
            "source" => source_text,
            _ => "",
        };
        // 只加 modifier（层级），前景色/底色仍由页面通过 `Paragraph::style`
        // 施加 —— 否则选中行的强调底色会被列样式盖掉。
        spans.push(Span::styled(
            cell_aligned(text, text_width, align_for_key(&column.key)),
            Style::new().add_modifier(column_emphasis(&column.key)),
        ));
        if with_separator {
            spans.push(Span::styled(COLUMN_SEPARATOR, palette.separator_style()));
        }
    }
    Line::from(spans)
}

/// 该列是否允许用户隐藏。
///
/// "歌曲"列是列表的主体，隐藏它等于把表变成一排无意义的数字，所以固定必显。
pub fn column_is_hideable(key: &str) -> bool {
    key != "name"
}

/// 切换某列的显示 / 隐藏，返回新的列配置。
///
/// 不改宽度、不改顺序；key 不存在或该列必显时原样返回。
pub fn toggle_column_visibility(
    columns: &[TableColumnConfig],
    key: &str,
) -> Vec<TableColumnConfig> {
    let mut next = columns.to_vec();
    if !column_is_hideable(key) {
        return next;
    }
    if let Some(target) = next.iter_mut().find(|column| column.key == key) {
        target.visible = !target.visible;
    }
    next
}

/// 按当前列表内容测算每列的合适宽度（"自动调整列宽"）。
///
/// 只参考该列真正会渲染的字段 + 表头标签，并夹到 `[min_width, max_width]`；
/// 若总宽超过 `total_width`，从"余量最大"的列开始等量回收，直到放得下。
pub fn auto_fit_columns(
    columns: &[TableColumnConfig],
    songs: &[SongInfo],
    total_width: u16,
) -> Vec<TableColumnConfig> {
    let mut out = columns.to_vec();

    for column in out.iter_mut() {
        if !column.visible {
            continue;
        }
        let mut widest = UnicodeWidthStr::width(column.label.as_str());
        for song in songs {
            let text = match column.key.as_str() {
                // 序号按 4 位预留，避免列表长了之后这一列反复变宽
                "index" => "9999".to_string(),
                "name" => song.name.clone(),
                "singer" => song.singer.clone(),
                "album" => song.album_name.clone(),
                "duration" => "00:00".to_string(),
                "quality" => song.quality_label(),
                "duration_quality" => format!("00:00 · {}", song.quality_label()),
                "source" => song.source.display_name().to_string(),
                _ => String::new(),
            };
            widest = widest.max(UnicodeWidthStr::width(text.as_str()));
        }
        // +2：列分隔符 1 列 + 一点呼吸空间
        let ideal = (widest as u16).saturating_add(2);
        column.width = ideal.clamp(column.min_width, column.max_width);
    }

    // 总宽超了就回收：每次从"可压缩余量最大"的可见列里减 1 列。
    let visible_total = |cols: &[TableColumnConfig]| -> u16 {
        cols.iter().filter(|c| c.visible).map(|c| c.width).sum()
    };
    let mut guard = 0;
    while visible_total(&out) > total_width && guard < 4096 {
        guard += 1;
        let Some(target) = out
            .iter_mut()
            .filter(|c| c.visible && c.width > c.min_width)
            .max_by_key(|c| c.width - c.min_width)
        else {
            break;
        };
        target.width -= 1;
    }
    out
}

/// 表头（自定义列配置）。调用方用 `Paragraph::style(palette.header_band())` 让色带横贯整行。
pub fn header_paragraph(
    width: u16,
    columns: &[TableColumnConfig],
    palette: TablePalette,
) -> Paragraph<'static> {
    Paragraph::new(header_line(width, columns, palette)).style(palette.header_band())
}

/// 数据行（自定义列配置）。
pub fn row_paragraph(
    song: &SongInfo,
    index: usize,
    width: u16,
    columns: &[TableColumnConfig],
    palette: TablePalette,
) -> Paragraph<'static> {
    Paragraph::new(row_line(song, index, width, columns, palette))
}

/// 表头（默认档位列）。
pub fn header_paragraph_default(width: u16, palette: TablePalette) -> Paragraph<'static> {
    header_paragraph(width, &default_columns(width), palette)
}

/// 数据行（默认档位列）。
pub fn row_paragraph_default(
    song: &SongInfo,
    index: usize,
    width: u16,
    palette: TablePalette,
) -> Paragraph<'static> {
    row_paragraph(song, index, width, &default_columns(width), palette)
}

/// 列的视觉层级：主列（歌名）加粗，次要列（专辑 / 来源 / 音质 / 数字）压暗。
///
/// 故意用 modifier 而不是写死前景色：行样式（尤其选中行的强调底色 + 高亮文字）
/// 由页面通过 `Paragraph::style` 施加，这里若设定 fg 会把选中行盖掉。
/// modifier 会与页面样式叠加，所以选中行依旧清晰，同时列与列之间有层次。
fn column_emphasis(key: &str) -> Modifier {
    match key {
        "name" => Modifier::BOLD,
        "singer" => Modifier::empty(),
        "album" | "source" | "quality" | "index" | "duration" | "duration_quality" => Modifier::DIM,
        _ => Modifier::empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lx_core::model::source::{Quality, SourceId};
    use std::time::Duration;
    use unicode_width::UnicodeWidthStr;

    fn palette() -> TablePalette {
        TablePalette {
            separator: Color::DarkGray,
            header_fg: Color::Gray,
            header_bg: Color::Black,
        }
    }

    fn song() -> SongInfo {
        let mut song = SongInfo::new(
            "1".to_string(),
            SourceId::Wy,
            "反方向的钟".to_string(),
            "周杰伦".to_string(),
        );
        song.album_name = "范特西".to_string();
        song.duration = Duration::from_secs(269);
        song.qualities.insert(Quality::Flac);
        song
    }

    fn flatten(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn resizing_tracks_pointer_when_columns_are_stretched_to_fit() {
        let columns = default_columns(100);
        let before = compute_layout(&columns, 100);
        let state = ColumnResizeState::begin(&columns, 1, 100, 50);

        let resized = state.resized_columns(100, 55);
        let after = compute_layout(&resized, 100);

        assert_eq!(after[1].width, before[1].width + 5);
        assert_eq!(after[2].width, before[2].width - 5);
    }

    #[test]
    fn live_moved_events_keep_table_resize_in_sync_with_the_pointer() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

        let mut columns = default_columns(100);
        let mut state = None;
        let header = Some(Rect::new(0, 0, 100, 1));
        let inner = Rect::new(0, 0, 100, 1);
        let layout = compute_layout(&columns, 100);
        let boundary = layout[1].start_x + layout[1].width;

        let begin = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: boundary,
            row: 0,
            modifiers: KeyModifiers::empty(),
        };
        assert_eq!(
            handle_column_resize(&mut state, &mut columns, begin, header, inner),
            ColumnResizeOutcome::Updated
        );
        assert!(state.is_some());

        let moved = MouseEvent {
            kind: MouseEventKind::Moved,
            column: boundary + 5,
            row: 0,
            modifiers: KeyModifiers::empty(),
        };
        let before = columns.clone();
        let outcome = handle_column_resize(&mut state, &mut columns, moved, header, inner);
        assert_eq!(outcome, ColumnResizeOutcome::Updated);
        assert_ne!(columns, before, "连续的 Moved 事件必须实时更新列宽");
    }

    #[test]
    fn truncates_cjk_to_terminal_width() {
        let value = cell_aligned("一首很长的中文歌曲", 8, CellAlign::Left);
        assert_eq!(UnicodeWidthStr::width(value.as_str()), 8);
        assert!(value.contains('…'));
    }

    #[test]
    fn right_aligned_cells_keep_the_same_width() {
        assert_eq!(cell_aligned("1", 4, CellAlign::Right), "   1");
        assert_eq!(cell_aligned("1", 4, CellAlign::Left), "1   ");
        // 右对齐且需要截断时，省略号仍在末尾，宽度不变
        let truncated = cell_aligned("一首很长的中文歌曲", 8, CellAlign::Right);
        assert_eq!(UnicodeWidthStr::width(truncated.as_str()), 8);
        assert!(truncated.ends_with('…'));
    }

    /// 关键不变量：加了分隔符之后，一行的**显示宽度必须仍等于传入宽度**。
    /// 否则表头/数据行会撑破布局，命中测试也会跟着错位。
    #[test]
    fn local_defaults_combine_duration_and_quality() {
        let columns = super::default_columns_for_page("local_music", 120);
        assert!(columns.iter().any(|c| c.key == "duration_quality"));
        assert!(!columns.iter().any(|c| c.key == "duration"));
        assert!(!columns.iter().any(|c| c.key == "quality"));
        assert!(!columns.iter().any(|c| c.key == "source"));
        let playback = columns
            .iter()
            .find(|c| c.key == "duration_quality")
            .unwrap();
        assert_eq!(playback.label, "播放");
        assert_eq!(playback.width, 12);
    }

    #[test]
    fn duration_quality_renders_duration_and_quality_together() {
        let columns = vec![TableColumnConfig {
            key: "duration_quality".to_string(),
            label: "播放".to_string(),
            visible: true,
            width: 16,
            min_width: 8,
            max_width: 16,
        }];
        let row = flatten(&row_line(&song(), 0, 16, &columns, palette()));
        assert!(row.contains("4:29") || row.contains("04:29"));
        assert!(row.contains("·"));
    }

    #[test]
    fn rendered_width_always_matches_the_requested_width() {
        for width in [24u16, 40, 64, 80, 96, 120, 200] {
            let columns = default_columns(width);
            let header = header_line(width, &columns, palette());
            let row = row_line(&song(), 0, width, &columns, palette());
            assert_eq!(
                header.width(),
                width as usize,
                "表头宽度应在 {width} 列时保持不变"
            );
            assert_eq!(
                row.width(),
                width as usize,
                "数据行宽度应在 {width} 列时保持不变"
            );
        }
    }

    /// 列之间必须有可见分隔符，否则短值后面的 padding 会让两列糊在一起。
    #[test]
    fn columns_are_separated_by_a_visible_divider() {
        let width = 120u16;
        let columns = default_columns(width);
        let header = flatten(&header_line(width, &columns, palette()));
        let row = flatten(&row_line(&song(), 0, width, &columns, palette()));

        let visible = compute_layout(&columns, width).len();
        assert_eq!(
            header.matches(COLUMN_SEPARATOR).count(),
            visible - 1,
            "分隔符数量 = 可见列数 - 1（末列不加）"
        );
        assert_eq!(row.matches(COLUMN_SEPARATOR).count(), visible - 1);
        assert!(!header.ends_with(COLUMN_SEPARATOR), "末列不应有尾随分隔符");
    }

    /// 数字列右对齐：`#` 与 `时长` 的末位应当对齐。
    #[test]
    fn numeric_columns_are_right_aligned() {
        let width = 120u16;
        let columns = default_columns(width);
        let layout = compute_layout(&columns, width);
        let index_rc = layout
            .iter()
            .find(|rc| columns[rc.original_index].key == "index")
            .expect("默认列里有 # 列");
        let (text_width, with_sep) = cell_budget(index_rc.width, false);
        let rendered = cell_aligned("7", text_width, CellAlign::Right);
        assert!(with_sep);
        assert!(
            rendered.starts_with(' '),
            "右对齐的序号应当在左侧补空格（实际 {rendered:?}）"
        );
        assert!(rendered.ends_with('7'));
    }

    /// 列宽为 1 时没有余量画分隔符：不允许为了分隔符把整行撑宽。
    #[test]
    fn narrow_columns_skip_the_separator_instead_of_overflowing() {
        let columns = vec![
            TableColumnConfig {
                key: "index".to_string(),
                label: "#".to_string(),
                visible: true,
                width: 1,
                min_width: 1,
                max_width: 4,
            },
            TableColumnConfig {
                key: "name".to_string(),
                label: "歌曲".to_string(),
                visible: true,
                width: 1,
                min_width: 1,
                max_width: 40,
            },
        ];
        let line = row_line(&song(), 0, 2, &columns, palette());
        assert_eq!(line.width(), 2);
    }

    /// 手写的列配置可能非法：`min_width > max_width` 会让 `clamp` panic，
    /// 空 key 会让这一列永远显示为空。读取时必须就地校正/丢弃。
    /// 自动列宽：按内容测量、夹在 min/max 内、且总宽不超过可用宽度。
    #[test]
    fn auto_fit_shrinks_and_clamps_columns_to_fit_the_width() {
        let width = 60u16;
        let columns = default_columns(width);
        let songs = vec![song()];

        let fitted = auto_fit_columns(&columns, &songs, width);
        let total: u16 = fitted.iter().filter(|c| c.visible).map(|c| c.width).sum();
        assert!(total <= width, "自动列宽后总宽 {total} 不应超过 {width}");
        for (before, after) in columns.iter().zip(fitted.iter()) {
            assert!(after.width >= after.min_width);
            assert!(after.width <= after.max_width);
            assert_eq!(before.visible, after.visible, "自动列宽不改可见性");
        }

        // 宽终端下"歌曲"列应当被撑到能放下标题
        let wide = auto_fit_columns(&default_columns(160), &songs, 160);
        let name = wide.iter().find(|c| c.key == "name").expect("有歌曲列");
        assert!(name.width >= UnicodeWidthStr::width("反方向的钟") as u16);
    }

    /// 可见性切换只改 `visible`，宽度与顺序不动。
    #[test]
    fn toggling_visibility_leaves_widths_untouched() {
        let mut columns = default_columns(120);
        let before: Vec<u16> = columns.iter().map(|c| c.width).collect();
        let source = columns
            .iter_mut()
            .find(|c| c.key == "source")
            .expect("有来源列");
        source.visible = false;
        let after: Vec<u16> = columns.iter().map(|c| c.width).collect();
        assert_eq!(before, after);
        assert!(!columns.iter().find(|c| c.key == "source").unwrap().visible);
    }

    /// 隐藏列：只改 visible，宽度/顺序不变；"歌曲"列不允许隐藏。
    #[test]
    fn toggling_a_column_only_flips_visibility() {
        let columns = default_columns(120);
        let next = toggle_column_visibility(&columns, "source");
        assert!(!next.iter().find(|c| c.key == "source").unwrap().visible);
        // 其余列不受影响
        for (before, after) in columns.iter().zip(next.iter()) {
            assert_eq!(before.key, after.key);
            assert_eq!(before.width, after.width);
            if before.key != "source" {
                assert_eq!(before.visible, after.visible);
            }
        }
        // 再切回来
        let back = toggle_column_visibility(&next, "source");
        assert!(back.iter().find(|c| c.key == "source").unwrap().visible);
    }

    #[test]
    fn the_song_name_column_cannot_be_hidden() {
        assert!(!column_is_hideable("name"));
        assert!(column_is_hideable("source"));
        let columns = default_columns(120);
        let next = toggle_column_visibility(&columns, "name");
        assert!(
            next.iter().find(|c| c.key == "name").unwrap().visible,
            "歌曲列必显"
        );
    }

    #[test]
    fn invalid_column_config_is_sanitized_instead_of_panicking() {
        let mut configured = std::collections::HashMap::new();
        configured.insert(
            "queue".to_string(),
            vec![
                TableColumnConfig {
                    key: "name".to_string(),
                    label: String::new(),
                    visible: true,
                    width: 0,
                    min_width: 9,
                    max_width: 3,
                },
                TableColumnConfig {
                    key: "   ".to_string(),
                    ..TableColumnConfig::default()
                },
            ],
        );

        let columns = super::load_columns_for_page(&configured, "queue", 80);

        assert_eq!(columns.len(), 1, "空 key 的条目应当被丢弃");
        let column = &columns[0];
        assert!(column.min_width <= column.max_width);
        assert!((column.min_width..=column.max_width).contains(&column.width));
        assert_eq!(column.label, "name", "空 label 回落到 key");
        // 校正后拖拽不应 panic
        let _ = super::adjust_widths(&columns, 0, 80, 5);
    }

    /// 列的视觉层级：主列加粗、次要列压暗，且**不写死前景色**。
    ///
    /// 写死 fg 会把页面的选中行样式（强调底 + 高亮文字）盖掉，所以这里
    /// 只允许用 modifier。
    #[test]
    fn columns_have_a_visual_hierarchy_without_hardcoding_colors() {
        assert_eq!(column_emphasis("name"), Modifier::BOLD, "歌名是主列");
        assert_eq!(column_emphasis("singer"), Modifier::empty(), "歌手不压暗");
        for key in [
            "album",
            "source",
            "quality",
            "index",
            "duration",
            "duration_quality",
        ] {
            assert_eq!(
                column_emphasis(key),
                Modifier::DIM,
                "{key} 属于次要信息，应当压暗"
            );
        }
        // 未知列不改变任何东西
        assert_eq!(column_emphasis("unknown"), Modifier::empty());
    }

    /// 表头单元格样式必须带上"色带底 + 加粗"，这样表头与数据行才分层。
    #[test]
    fn header_cells_keep_the_band_and_add_per_column_emphasis() {
        let palette = TablePalette {
            separator: ratatui::style::Color::Reset,
            header_fg: ratatui::style::Color::Reset,
            header_bg: ratatui::style::Color::Reset,
        };
        let name = palette.header_cell_style("name");
        assert!(name.add_modifier.contains(Modifier::BOLD));
        let album = palette.header_cell_style("album");
        assert!(album.add_modifier.contains(Modifier::BOLD));
        assert!(album.add_modifier.contains(Modifier::DIM));
    }
}
