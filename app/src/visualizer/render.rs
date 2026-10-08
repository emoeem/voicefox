//! 频谱的 ratatui 渲染（视觉设计对齐 cava 并加以润色）：
//!
//! - **暗色舞台**：主题 base 压暗 35% 作为频谱背景，叠加层看起来像一块
//!   独立的"舞台"而不是一块实心色板；底层内容的 DIM/BOLD 会被清掉；
//! - **连续纵向渐变**：主题色锚点（blue → sapphire → sky → teal → green）经
//!   [`colorgrad`] 插值，每个"半行"按全高比例取色，柱子内部也有色彩过渡；
//! - **柱顶高光**：每根柱子最顶端的半格朝帽色提亮一档，形成柔光的柱尖；
//! - **待机基线**：柱槽底部常亮一格暗色半块，频谱静默时也能看出舞台仍在；
//! - **cava 式柱槽**：classic 是 2 列柱无缝铺满（窄终端降为 1 列），modern
//!   在此基础上加 1 列间隙，观感更透气；两者都带峰顶小帽（peak caps）。
//!
//! 颜色与几何分离：[`Palette`] 是纯数据（可离线测试），`render` 只负责
//! 从主题解析出调色板再走同一条渲染路径。调色板经 [`PaletteCache`] 缓存，
//! 主题色指纹没变就跨帧复用渐变，不在每帧重建。

use colorgrad::{Gradient as _, GradientBuilder, LinearGradient};
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use super::VisualizerData;
use crate::context::AppContext;
use crate::theme;

/// 渐变锚点（底部 → 顶部）与峰顶标记色。
#[derive(Debug)]
pub struct Palette {
    /// 舞台背景：主题 base 压暗后的颜色。
    pub background: Color,
    /// 峰顶小帽与柱顶高光的基准色：全画面最亮的颜色。
    pub cap: Color,
    /// 纵向线性渐变（自下而上），交给 colorgrad 插值。
    pub gradient: LinearGradient,
}

impl Palette {
    /// 从主题槽位色构建调色板：`background` 是 base、`cap` 是 text、
    /// `stops` 是自下而上的 5 个渐变锚点。
    fn from_colors(background: Color, cap: Color, stops: [Color; 5]) -> Self {
        let colors: Vec<colorgrad::Color> = stops
            .iter()
            .map(|color| to_colorgrad_color(*color))
            .collect();
        let fallback = || {
            let only = colors.first().copied().unwrap_or_default();
            GradientBuilder::new()
                .colors(&[only, only])
                .build::<LinearGradient>()
                .expect("two identical colors always build")
        };
        let gradient = GradientBuilder::new()
            .colors(&colors)
            .build::<LinearGradient>()
            .unwrap_or_else(|_| fallback());
        Self {
            background: theme::blend(background, Color::Rgb(0, 0, 0), STAGE_DIM),
            cap,
            gradient,
        }
    }
}

/// 舞台背景把主题 base 压暗的比例：0 是实心 base（旧观感），1 是纯黑。
/// 实测 0.35 足够与页面底色拉开层次，又不至于让浅色主题变成大黑块。
const STAGE_DIM: f32 = 0.35;
/// 柱顶高光朝帽色（最亮色）提亮的比例：过高会糊成白柱，过低看不出柔光。
const TIP_GLOW: f32 = 0.35;
/// 待机基线朝帽色提亮的比例：一条"地板"，静默时舞台仍隐约可见。
const BASELINE_GLOW: f32 = 0.16;

/// 主题色指纹 + 调色板的跨帧缓存。指纹取 [`Palette`] 用到的全部 7 个
/// 主题槽位色：任何一个变化（换主题、终端配色）都会触发重建，其余帧
/// 直接复用 —— colorgrad 的渐变构建与分配从每帧一次降到换主题时一次。
#[derive(Debug, Default)]
pub struct PaletteCache {
    cached: Option<([Color; 7], Palette)>,
}

impl PaletteCache {
    /// 返回当前主题的调色板，必要时按最新主题色重建。
    pub fn palette(&mut self, ctx: &AppContext) -> &Palette {
        let key = [
            theme::base(ctx),
            theme::text(ctx),
            theme::blue(ctx),
            theme::sapphire(ctx),
            theme::sky(ctx),
            theme::teal(ctx),
            theme::green(ctx),
        ];
        if self
            .cached
            .as_ref()
            .is_none_or(|(cached_key, _)| *cached_key != key)
        {
            let palette =
                Palette::from_colors(key[0], key[1], [key[2], key[3], key[4], key[5], key[6]]);
            self.cached = Some((key, palette));
        }
        &self.cached.as_ref().expect("cache populated above").1
    }
}

/// 把主题色转换成 colorgrad 颜色；非 Rgb 主题色退化为中性灰蓝。
fn to_colorgrad_color(color: Color) -> colorgrad::Color {
    match color {
        Color::Rgb(r, g, b) => colorgrad::Color::new(
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
            1.0,
        ),
        _ => colorgrad::Color::new(0.6, 0.65, 0.7, 1.0),
    }
}

/// 渐变在 `fraction`（0..1，自底部向上）处的颜色。
fn level_color(palette: &Palette, fraction: f32) -> Color {
    let rgba = palette.gradient.at(fraction.clamp(0.0, 1.0));
    Color::Rgb(
        (rgba.r * 255.0).round() as u8,
        (rgba.g * 255.0).round() as u8,
        (rgba.b * 255.0).round() as u8,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisualizerStyle {
    Classic,
    Modern,
}

impl VisualizerStyle {
    pub fn from_ctx(ctx: &AppContext) -> Self {
        let value = ctx
            .config
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ui
            .visualizer_style
            .trim()
            .to_ascii_lowercase();
        match value.as_str() {
            "modern" => Self::Modern,
            _ => Self::Classic,
        }
    }
}

/// classic 的柱槽布局：参考 cava 的连续填满风格，柱间无缝。
/// 窄终端保留 1 列柱；中宽/宽终端用 2 列柱，视觉更像传统 cava 的紧密输出。
fn slot_layout(width: u16) -> (u16, u16) {
    match width {
        0..=15 => (1, 0),
        _ => (2, 0),
    }
}

/// modern 在 classic 基础上加 1 列间隙：柱子更透气，与紧密的 classic
/// 形成可见的风格差。极窄终端没有余量给间隙，退回无缝单列柱。
fn slot_layout_for_style(width: u16, style: VisualizerStyle) -> (u16, u16) {
    match style {
        VisualizerStyle::Classic => slot_layout(width),
        VisualizerStyle::Modern => match width {
            0..=9 => (1, 0),
            10..=39 => (1, 1),
            _ => (2, 1),
        },
    }
}

pub fn render_data(
    area: Rect,
    buf: &mut Buffer,
    ctx: &AppContext,
    palette: &Palette,
    data: &VisualizerData,
    peaks: &[f32],
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    render_with_style(
        area,
        buf,
        &data.spectrum,
        peaks,
        palette,
        VisualizerStyle::from_ctx(ctx),
    );
}

/// 主题无关的渲染主体（[`Palette::from_theme`] 解耦出的可测试部分）。
#[cfg(test)]
pub fn render_with(area: Rect, buf: &mut Buffer, bars: &[f32], peaks: &[f32], palette: &Palette) {
    render_with_style(area, buf, bars, peaks, palette, VisualizerStyle::Classic);
}

/// 画一枚叠加层单元格：频谱覆盖处底层内容（暗色列、加粗选中行…）的
/// DIM/BOLD 必须清掉，否则柱子会带着底下的修饰色块，既难看又凭空多出
/// 一堆 SGR 切换。
///
/// 同一层原因还必须清掉 `diff_option`：封面用终端图形协议（kitty/iTerm2/
/// sixel）绘制时，会把图片区域的单元格标成 `CellDiffOption::Skip` 并把整行
/// 转义序列塞进首列（见 `ratatui_image::protocol::kitty`）。`Cell::set_char`
/// /`set_style` 都不会碰这个标志，于是这些格子被永久排除在缓冲区差分之外，
/// 终端上保留着旧画面，频谱里就出现一块矩形"黑框"。
fn paint_cell(buf: &mut Buffer, x: u16, y: u16, glyph: char, fg: Color, background: Color) {
    if let Some(cell) = buf.cell_mut((x, y)) {
        cell.set_char(glyph);
        cell.set_fg(fg);
        cell.set_bg(background);
        cell.set_style(Style::new().remove_modifier(Modifier::all()));
        cell.set_diff_option(CellDiffOption::None);
    }
}

fn render_with_style(
    area: Rect,
    buf: &mut Buffer,
    bars: &[f32],
    peaks: &[f32],
    palette: &Palette,
    style: VisualizerStyle,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    paint_background(area, buf, palette.background);
    if bars.is_empty() {
        return;
    }

    let (bar_width, gap_width) = slot_layout_for_style(area.width, style);
    let slot = (bar_width + gap_width).max(1) as usize;
    let visible_bars = ((area.width as usize) / slot).max(1).min(bars.len());
    let half_rows = area.height as usize * 2;
    let baseline_color = theme::blend(palette.background, palette.cap, BASELINE_GLOW);

    for bar_index in 0..visible_bars {
        let x0 = area.x + (bar_index * slot) as u16;
        let x1 = (x0 + bar_width).min(area.right());
        // 96 个频带均匀映射到可见柱：每柱取对应区间的最大值（保峰）。
        let band_start = bar_index * bars.len() / visible_bars;
        let band_end = ((bar_index + 1) * bars.len() / visible_bars).max(band_start + 1);
        let level = bars[band_start..band_end.min(bars.len())]
            .iter()
            .fold(0.0f32, |acc, value| acc.max(*value));
        // 柱高换算成"半行"数并向上取整：低电平也至少显示一格。
        let filled = (level.clamp(0.0, 1.0) * half_rows as f32).ceil() as usize;

        for y in 0..area.height {
            let row_from_bottom = (area.height - 1 - y) as usize;
            // 一个终端行由上下两个"半行"组成：下 = 2*row，上 = 2*row+1。
            let bottom_half = row_from_bottom * 2;
            let bottom_filled = filled > bottom_half;
            let top_filled = filled > bottom_half + 1;
            if !bottom_filled && !top_filled {
                // 待机基线：柱槽最底一行常亮一格暗色半块，频谱静默时
                // 舞台仍隐约可见，不会像没画一样。
                if row_from_bottom == 0 {
                    for x in x0..x1 {
                        paint_cell(buf, x, area.y + y, '▄', baseline_color, palette.background);
                    }
                }
                continue;
            }
            let bottom_color = level_color(palette, (bottom_half + 1) as f32 / half_rows as f32);
            let top_color = level_color(palette, (bottom_half + 2) as f32 / half_rows as f32);

            let (glyph, fg) = match (top_filled, bottom_filled) {
                // 整格都是柱：fg 覆盖全格，取上半格颜色即可（同一根柱同色系）。
                (true, true) => ('█', top_color),
                // 只有上半格是柱：下半格露背景。
                (true, false) => ('▀', top_color),
                // 只有下半格是柱：上半格露背景。
                (false, true) => ('▄', bottom_color),
                (false, false) => continue,
            };
            // 柱顶高光：最上面的半格朝帽色提亮一档，形成柔光柱尖。
            let tip = match (top_filled, bottom_filled) {
                (true, true) => bottom_half + 1 == filled - 1,
                (true, false) => true,
                (false, true) => bottom_half == filled - 1,
                (false, false) => false,
            };
            let fg = if tip {
                theme::blend(fg, palette.cap, TIP_GLOW)
            } else {
                fg
            };
            for x in x0..x1 {
                paint_cell(buf, x, area.y + y, glyph, fg, palette.background);
            }
        }

        // 峰顶小帽：柱顶上方一格内的亮色半块（cava peak cap）。
        if let Some(cap) = peaks.get(bar_index) {
            let cap_half = (cap.clamp(0.0, 1.0) * half_rows as f32).ceil() as usize;
            // 帽只画在柱体之外，否则会盖住渐变顶端。
            if cap_half > filled && cap_half >= 1 {
                let half_index = cap_half - 1;
                let y = area.y + (half_rows - 1 - half_index) as u16 / 2;
                let upper = half_index % 2 == 1;
                let glyph = if upper { '▀' } else { '▄' };
                for x in x0..x1 {
                    paint_cell(buf, x, y, glyph, palette.cap, palette.background);
                }
            }
        }
    }
}

fn paint_background(area: Rect, buf: &mut Buffer, background: Color) {
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(' ');
                // `Cell::set_style` 只增删 modifier，不会清空；必须显式移除全部
                // 修饰位，否则会继承底层歌词/表格的 DIM、BOLD。
                cell.set_style(Style::new().bg(background).remove_modifier(Modifier::all()));
                // 封面图片区带 `Skip`/`ForcedWidth` 差分标志，覆盖时必须解开，
                // 否则整片区域不会重新输出（详见 `paint_cell`）。
                cell.set_diff_option(CellDiffOption::None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette() -> Palette {
        let gradient = GradientBuilder::new()
            .colors(&[
                colorgrad::Color::new(0.0, 0.0, 1.0, 1.0),
                colorgrad::Color::new(0.0, 1.0, 0.0, 1.0),
            ])
            .build::<LinearGradient>()
            .unwrap();
        Palette {
            background: Color::Black,
            // 注意必须是 Rgb：ANSI 命名色无法参与 theme::blend 的混色。
            cap: Color::Rgb(255, 255, 255),
            gradient,
        }
    }

    fn buffer(width: u16, height: u16) -> Buffer {
        Buffer::empty(Rect::new(0, 0, width, height))
    }

    #[test]
    fn full_height_bar_fills_whole_column() {
        let mut buf = buffer(6, 2);
        render_with(
            Rect::new(0, 0, 6, 2),
            &mut buf,
            &[1.0, 0.0],
            &[],
            &palette(),
        );
        // 6 列宽度下每列一根柱：第一根柱满高占第 0 列。
        assert_eq!(buf[(0u16, 0u16)].symbol(), "█");
        assert_eq!(buf[(0u16, 1u16)].symbol(), "█");
        // 第二根柱电平为 0：只在底部显示待机基线，上方仍是背景。
        assert_eq!(buf[(1u16, 1u16)].symbol(), "▄");
        assert_eq!(buf[(1u16, 0u16)].symbol(), " ");
        // 柱槽之外保持背景。
        assert_eq!(buf[(2u16, 0u16)].symbol(), " ");
    }

    #[test]
    fn bars_fill_columns_without_visible_gaps() {
        let mut buf = buffer(20, 1);
        render_with(
            Rect::new(0, 0, 20, 1),
            &mut buf,
            &[1.0, 1.0, 1.0, 1.0],
            &[],
            &palette(),
        );
        for x in 0..8u16 {
            assert_eq!(
                buf[(x, 0u16)].symbol(),
                "█",
                "col {x} should be filled in classic cava layout"
            );
        }
        for x in 8..20u16 {
            assert_eq!(
                buf[(x, 0u16)].symbol(),
                " ",
                "trailing cols beyond the compact fill should stay blank"
            );
        }
    }

    #[test]
    fn wide_terminal_uses_compact_two_column_bars() {
        let mut buf = buffer(20, 1);
        assert_eq!(slot_layout(96), (2, 0));
        render_with(
            Rect::new(0, 0, 20, 1),
            &mut buf,
            &[1.0, 1.0, 1.0, 1.0],
            &[],
            &Palette {
                background: Color::Black,
                cap: Color::White,
                gradient: palette().gradient,
            },
        );
        for x in 0..8u16 {
            assert_eq!(buf[(x, 0u16)].symbol(), "█");
        }
        for x in 8..20u16 {
            assert_eq!(buf[(x, 0u16)].symbol(), " ");
        }
    }

    #[test]
    fn gradient_is_continuous_not_hard_banded() {
        let palette = palette();
        // 中点两侧的颜色应介于两个锚点之间（插值生效，而不是硬切色块）。
        let low = level_color(&palette, 0.1);
        let mid = level_color(&palette, 0.5);
        let high = level_color(&palette, 0.9);
        let Color::Rgb(lr, lg, _) = low else {
            panic!("expected rgb")
        };
        let Color::Rgb(_mr, mg, _) = mid else {
            panic!("expected rgb")
        };
        let Color::Rgb(_hr, hg, _) = high else {
            panic!("expected rgb")
        };
        // 蓝→绿渐变：绿分量单调上升、蓝分量单调下降（红恒为 0）。
        assert_eq!(lr, 0);
        assert!(lg < mg && mg < hg, "green channel {lg} {mg} {hg}");
        let Color::Rgb(_, _, lb) = low else {
            panic!("expected rgb")
        };
        let Color::Rgb(_, _, mb) = mid else {
            panic!("expected rgb")
        };
        let Color::Rgb(_, _, hb) = high else {
            panic!("expected rgb")
        };
        assert!(lb > mb && mb > hb, "blue channel {lb} {mb} {hb}");
    }

    #[test]
    fn peak_cap_drawn_above_bar_top() {
        let mut buf = buffer(3, 4);
        // 柱 0.5（8 半行中的 4 格），峰 0.75（第 6 半行）→ 帽悬在柱顶上方。
        render_with(
            Rect::new(0, 0, 3, 4),
            &mut buf,
            &[0.5, 0.0],
            &[0.75, 0.0],
            &palette(),
        );
        // 半行索引 5（0 起）是奇数 → 行 1 的上半格 → '▀'，颜色是帽色（白）。
        assert_eq!(buf[(0u16, 1u16)].symbol(), "▀");
        assert_eq!(buf[(0u16, 1u16)].fg, Color::Rgb(255, 255, 255));
        // 柱顶本身（半行 3 → 行 2 上半格）保持柱色而不是帽色。
        assert_ne!(buf[(0u16, 2u16)].fg, Color::White);
    }

    #[test]
    fn empty_bars_paint_only_background() {
        let mut buf = buffer(3, 2);
        render_with(Rect::new(0, 0, 3, 2), &mut buf, &[], &[], &palette());
        for y in 0..2u16 {
            for x in 0..3u16 {
                assert_eq!(buf[(x, y)].symbol(), " ");
                assert_eq!(buf[(x, y)].bg, Color::Black);
            }
        }
    }

    /// 频谱是叠加层：底层内容的 DIM/BOLD 不能被柱子继承（既难看又多出 SGR）。
    #[test]
    fn spectrum_strips_modifiers_inherited_from_the_underlying_content() {
        let mut buf = buffer(6, 2);
        for y in 0..2u16 {
            for x in 0..6u16 {
                buf.cell_mut((x, y))
                    .expect("cell in range")
                    .set_style(Style::new().add_modifier(Modifier::DIM | Modifier::BOLD));
            }
        }

        render_with(
            Rect::new(0, 0, 6, 2),
            &mut buf,
            &[1.0, 1.0],
            &[1.0, 1.0],
            &palette(),
        );

        for y in 0..2u16 {
            for x in 0..6u16 {
                assert!(
                    buf[(x, y)].modifier.is_empty(),
                    "cell ({x},{y}) kept {:?}",
                    buf[(x, y)].modifier
                );
            }
        }
    }

    #[test]
    fn render_data_keeps_a_classic_cava_style_without_waveform_or_meter() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 8));
        let palette = palette();

        render_with(
            Rect::new(0, 0, 20, 8),
            &mut buf,
            &[0.0; 12],
            &[0.0; 12],
            &palette,
        );

        for y in 0..8u16 {
            for x in 0..20u16 {
                assert_ne!(buf[(x, y)].symbol(), "•");
                assert_ne!(buf[(x, y)].symbol(), "R");
                assert_ne!(buf[(x, y)].symbol(), "M");
                assert_ne!(buf[(x, y)].symbol(), "T");
            }
        }
    }

    #[test]
    fn modern_style_adds_gaps_between_bars() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 12, 1));
        render_with_style(
            Rect::new(0, 0, 12, 1),
            &mut buf,
            &[1.0, 1.0, 1.0],
            &[],
            &palette(),
            VisualizerStyle::Modern,
        );
        // 中等宽度下 modern 是 1 列柱 + 1 列间隙：柱、隙交替。
        assert_eq!(slot_layout_for_style(12, VisualizerStyle::Modern), (1, 1));
        assert_eq!(buf[(0u16, 0u16)].symbol(), "█");
        assert_eq!(buf[(1u16, 0u16)].symbol(), " ");
        assert_eq!(buf[(2u16, 0u16)].symbol(), "█");
        assert_eq!(buf[(3u16, 0u16)].symbol(), " ");
        assert_eq!(buf[(4u16, 0u16)].symbol(), "█");
        // 宽终端下 modern 是 2 列柱 + 1 列间隙。
        assert_eq!(slot_layout_for_style(60, VisualizerStyle::Modern), (2, 1));
        // classic 始终无缝。
        assert_eq!(slot_layout_for_style(60, VisualizerStyle::Classic), (2, 0));
    }

    #[test]
    fn stage_background_is_theme_base_dimmed() {
        // from_colors 把 base 朝黑压暗 STAGE_DIM：(30,30,46) → 65% 亮度。
        let palette = Palette::from_colors(
            Color::Rgb(30, 30, 46),
            Color::Rgb(205, 214, 244),
            [Color::Rgb(137, 180, 250); 5],
        );
        assert_eq!(palette.background, Color::Rgb(20, 20, 30));
    }

    #[test]
    fn bar_tip_is_brightened_toward_the_cap_color() {
        let palette = palette();
        let mut buf = buffer(3, 4);
        render_with(Rect::new(0, 0, 3, 4), &mut buf, &[1.0], &[], &palette);
        // 满高柱的柱尖是最顶上的半格（y=0），朝帽色（RGB 白）提亮；
        // 柱身（y=1）保持纯渐变色。
        let tip = buf[(0u16, 0u16)].fg;
        let body = buf[(0u16, 1u16)].fg;
        let body_expect = level_color(&palette, (2 * 2 + 2) as f32 / 8.0);
        assert_eq!(body, body_expect);
        let tip_expect = theme::blend(
            level_color(&palette, 1.0),
            Color::Rgb(255, 255, 255),
            TIP_GLOW,
        );
        assert_eq!(tip, tip_expect);
    }

    #[test]
    fn silent_bars_show_a_dim_baseline() {
        let mut buf = buffer(3, 2);
        render_with(
            Rect::new(0, 0, 3, 2),
            &mut buf,
            &[0.0, 0.0],
            &[],
            &palette(),
        );
        // 全静默：底部一行是暗色基线（黑底混 16% 白），上方仍是背景。
        let baseline = theme::blend(Color::Black, Color::White, BASELINE_GLOW);
        for x in 0..2u16 {
            assert_eq!(buf[(x, 1u16)].symbol(), "▄");
            assert_eq!(buf[(x, 1u16)].fg, baseline);
            assert_eq!(buf[(x, 0u16)].symbol(), " ");
        }
    }

    #[test]
    fn narrow_terminal_drops_gaps() {
        assert_eq!(slot_layout(10), (1, 0));
        assert_eq!(slot_layout(32), (2, 0));
        assert_eq!(slot_layout(120), (2, 0));
    }

    /// 回归：封面用终端图形协议绘制后，图片区域内的格子会被标成
    /// `Skip`（首列则是携带整行转义序列的 `ForcedWidth`）。频谱覆盖这块
    /// 区域时必须把差分标志解开，否则这些格子永远不参与差分输出，屏幕
    /// 上就留下封面那块旧画面 —— 也就是用户看到的矩形"黑框"。
    #[test]
    fn spectrum_clears_diff_markers_left_by_cover_graphics() {
        let mut buf = buffer(8, 3);
        let image_area = Rect::new(1, 0, 6, 3);
        for y in image_area.y..image_area.bottom() {
            for x in image_area.x..image_area.right() {
                let cell = buf.cell_mut((x, y)).expect("cell in bounds");
                if x == image_area.x {
                    // ratatui-image 把整行图片转义序列塞进首列。
                    cell.set_symbol("\u{1b}_Gi=1;a=T\u{1b}\\").set_diff_option(
                        CellDiffOption::ForcedWidth(std::num::NonZeroU16::new(1).expect("nonzero")),
                    );
                } else {
                    cell.set_diff_option(CellDiffOption::Skip);
                }
            }
        }

        render_with(
            Rect::new(0, 0, 8, 3),
            &mut buf,
            &[1.0, 1.0],
            &[],
            &palette(),
        );

        for y in 0..3u16 {
            for x in 0..8u16 {
                assert_eq!(
                    buf[(x, y)].diff_option,
                    CellDiffOption::None,
                    "cell ({x},{y}) must be re-emitted after the spectrum paints over it"
                );
            }
        }
    }
}
