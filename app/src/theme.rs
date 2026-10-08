#![allow(dead_code)]

use lx_core::model::config::ThemeConfig;
use ratatui::style::Color;
use ratatui_themes::{ThemeName, ThemePalette};

use crate::context::AppContext;
use lx_core::model::config::AccentFollowCover;

/// 默认皮肤：使用 `[theme]` 里手工调好的槽位（与历史版本观感一致）。
pub const SKIN_VOICEFOX: &str = "voicefox";

/// 可选的界面主题名：`voicefox` + 主题库的全部主题，顺序即循环顺序。
pub fn skin_names() -> Vec<String> {
    let mut names = vec![SKIN_VOICEFOX.to_string()];
    names.extend(ThemeName::all().iter().map(|name| skin_key(*name)));
    names
}

/// 主题在配置里的名字。
///
/// 直接用主题库的 serde 表现（kebab-case，如 `tokyo-night`），
/// 而不是自己按显示名拼，避免两边命名规则漂移。
fn skin_key(name: ThemeName) -> String {
    serde_json::to_value(name)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// 主题名的可读标签（设置页显示用）。
pub fn skin_label(name: &str) -> String {
    match library_theme(name) {
        Some(theme) => theme.display_name().to_string(),
        None => "Voicefox".to_string(),
    }
}

/// 循环到下一个主题名。
pub fn next_skin_name(current: &str) -> String {
    let names = skin_names();
    let index = names.iter().position(|name| name == current).unwrap_or(0);
    names[(index + 1) % names.len()].clone()
}

/// 主题名 → 主题库主题；未知名字返回 `None`（调用方回退到 voicefox 槽位）。
fn library_theme(value: &str) -> Option<ThemeName> {
    let wanted = value.trim().to_ascii_lowercase();
    ThemeName::all()
        .iter()
        .copied()
        .find(|name| skin_key(*name) == wanted)
}

/// 两个颜色按比例混合：`t = 0` 全取 `a`，`t = 1` 全取 `b`。
///
/// 非 RGB 颜色（终端默认色 / ANSI 名）无法参与混合，原样返回 `a`。
pub(crate) fn blend(a: Color, b: Color, t: f32) -> Color {
    match (a, b) {
        (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) => {
            let mix = |x: u8, y: u8| {
                (f32::from(x) + (f32::from(y) - f32::from(x)) * t)
                    .round()
                    .clamp(0.0, 255.0) as u8
            };
            Color::Rgb(mix(ar, br), mix(ag, bg), mix(ab, bb))
        }
        _ => a,
    }
}

/// 把主题库的 10 个语义色展开成项目在用的 24 个槽位。
///
/// 项目现有 294 处调用点用的是 Catppuccin 的槽位名（surface/overlay/peach…）。
/// 与其把它们全部改写成语义名（高风险大重构），不如在这里做一次映射：
/// 层次色由 `bg → fg` 的插值生成，色相槽位折叠到主题库的
/// `error/warning/success/info/accent/secondary` 上。
///
/// 未知槽位返回 `None`，由调用方回退到配置里的十六进制色。
fn slot_color(palette: ThemePalette, slot: &str) -> Option<Color> {
    // 层次色一律朝前景色方向插值：深色主题是"提亮"，浅色主题是"压暗"，
    // 两边都能得到正确的分层方向。
    let layer = |t: f32| blend(palette.bg, palette.fg, t);
    Some(match slot {
        "base" => palette.bg,
        "mantle" => layer(0.04),
        "crust" => layer(0.08),
        "surface_0" => layer(0.10),
        "surface_1" => layer(0.16),
        "surface_2" => layer(0.24),
        "overlay_0" => blend(palette.muted, palette.bg, 0.35),
        "overlay_1" => palette.muted,
        "overlay_2" => blend(palette.muted, palette.fg, 0.35),
        "text" => palette.fg,
        "subtext_0" => blend(palette.fg, palette.bg, 0.30),
        "subtext_1" => blend(palette.fg, palette.bg, 0.18),
        "accent" => palette.accent,
        "border" => layer(0.28),
        // 语义色直接对应
        "yellow" => palette.warning,
        "red" => palette.error,
        "green" => palette.success,
        "blue" => palette.info,
        // 主题库没有这么多色相：次要槽位折叠到最近的语义色上，保持"颜色含义"不丢
        "peach" => palette.secondary,
        "mauve" | "lavender" | "rosewater" | "flamingo" | "pink" => palette.accent,
        "sapphire" | "sky" => palette.info,
        "teal" => palette.success,
        "maroon" => palette.error,
        // 选中行上的文字：用主题背景色，在强调色底上一定读得清
        "selection_fg" => palette.bg,
        _ => return None,
    })
}

const ROSEWATER: Color = Color::Rgb(245, 224, 220);
const FLAMINGO: Color = Color::Rgb(242, 205, 205);
const PINK: Color = Color::Rgb(245, 194, 231);
const MAUVE: Color = Color::Rgb(203, 166, 247);
const RED: Color = Color::Rgb(243, 139, 168);
const MAROON: Color = Color::Rgb(235, 160, 172);
const PEACH: Color = Color::Rgb(250, 179, 135);
const YELLOW: Color = Color::Rgb(249, 226, 175);
const GREEN: Color = Color::Rgb(166, 227, 161);
const TEAL: Color = Color::Rgb(148, 226, 213);
const SKY: Color = Color::Rgb(137, 220, 235);
const SAPPHIRE: Color = Color::Rgb(116, 199, 236);
const BLUE: Color = Color::Rgb(137, 180, 250);
const LAVENDER: Color = Color::Rgb(180, 190, 254);
const TEXT: Color = Color::Rgb(205, 214, 244);
const SUBTEXT_1: Color = Color::Rgb(186, 194, 222);
const SUBTEXT_0: Color = Color::Rgb(166, 173, 200);
const OVERLAY_2: Color = Color::Rgb(147, 153, 178);
const OVERLAY_1: Color = Color::Rgb(127, 132, 156);
const OVERLAY_0: Color = Color::Rgb(108, 112, 134);
const SURFACE_2: Color = Color::Rgb(88, 91, 112);
const SURFACE_1: Color = Color::Rgb(69, 71, 90);
const SURFACE_0: Color = Color::Rgb(49, 50, 68);
const BASE: Color = Color::Rgb(30, 30, 46);
const MANTLE: Color = Color::Rgb(24, 24, 37);
const CRUST: Color = Color::Rgb(17, 17, 27);

/// 各档位把封面主色混入 accent 的比例：过高会失去可读性与主题个性，
/// 过低则感知不到（提取色已归一化到鲜艳区间，见 cover::accent）。
const ACCENT_COVER_BLEND_SUBTLE: f32 = 0.45;
const ACCENT_COVER_BLEND_STRONG: f32 = 0.75;

pub fn accent(ctx: &AppContext) -> Color {
    let base = configured(ctx, "accent", |theme| &theme.accent, MAUVE);
    blend_with_cover(ctx, base)
}

/// 「界面强调色跟随封面」：把当前专辑封面提取出的主色混进 accent。
///
/// 只动 accent 一个槽位（`blend_with_cover` 的调用点仅此一处），层次色与
/// 语义色保持主题原样；档位关闭 / 无封面 / 封面是灰调（无主色）时原样返回。
fn blend_with_cover(ctx: &AppContext, base: Color) -> Color {
    let mode = ctx
        .config
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .ui
        .accent_follow_cover;
    let blend_t = match mode {
        AccentFollowCover::Off => return base,
        AccentFollowCover::Subtle => ACCENT_COVER_BLEND_SUBTLE,
        AccentFollowCover::Strong => ACCENT_COVER_BLEND_STRONG,
    };
    let Some(path) = ctx.cover_service.image_path() else {
        return base;
    };
    let Some(rgb) = crate::cover::accent::current(Some(&path)) else {
        return base;
    };
    blend(base, Color::Rgb(rgb[0], rgb[1], rgb[2]), blend_t)
}

pub fn border(ctx: &AppContext) -> Color {
    configured(ctx, "border", |theme| &theme.border, SURFACE_2)
}

pub fn text(ctx: &AppContext) -> Color {
    configured(ctx, "text", |theme| &theme.text, TEXT)
}

pub fn muted(ctx: &AppContext) -> Color {
    configured(ctx, "muted", |theme| &theme.muted, SUBTEXT_0)
}

macro_rules! palette_color {
    ($name:ident, $field:ident, $fallback:ident) => {
        pub fn $name(ctx: &AppContext) -> Color {
            configured(ctx, stringify!($field), |theme| &theme.$field, $fallback)
        }
    };
}

palette_color!(rosewater, rosewater, ROSEWATER);
palette_color!(flamingo, flamingo, FLAMINGO);
palette_color!(pink, pink, PINK);
palette_color!(mauve, mauve, MAUVE);
palette_color!(red, red, RED);
palette_color!(maroon, maroon, MAROON);
palette_color!(peach, peach, PEACH);
palette_color!(yellow, yellow, YELLOW);
palette_color!(green, green, GREEN);
palette_color!(teal, teal, TEAL);
palette_color!(sky, sky, SKY);
palette_color!(sapphire, sapphire, SAPPHIRE);
palette_color!(blue, blue, BLUE);
palette_color!(lavender, lavender, LAVENDER);
palette_color!(subtext1, subtext_1, SUBTEXT_1);
palette_color!(subtext0, subtext_0, SUBTEXT_0);
palette_color!(overlay2, overlay_2, OVERLAY_2);
palette_color!(overlay1, overlay_1, OVERLAY_1);
palette_color!(overlay0, overlay_0, OVERLAY_0);
palette_color!(surface2, surface_2, SURFACE_2);
palette_color!(surface1, surface_1, SURFACE_1);
palette_color!(surface0, surface_0, SURFACE_0);
palette_color!(base, base, BASE);
palette_color!(mantle, mantle, MANTLE);
palette_color!(crust, crust, CRUST);

pub fn selection_fg(ctx: &AppContext) -> Color {
    configured(ctx, "selection_fg", |theme| &theme.crust, CRUST)
}

/// 取一个槽位的颜色。
///
/// 两条来源，优先级从高到低：
/// 1. `theme.name` 选了主题库里的具名主题 → 由它的语义色推导（整界面一起换肤）；
/// 2. 否则（默认 `voicefox`）→ 用 `[theme]` 里手工调好的十六进制色，观感与历史一致。
fn configured(
    ctx: &AppContext,
    slot: &str,
    value: fn(&ThemeConfig) -> &String,
    fallback: Color,
) -> Color {
    let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
    if let Some(theme) = library_theme(&config.theme.name)
        && let Some(color) = slot_color(theme.palette(), slot)
    {
        return color;
    }
    parse(value(&config.theme), fallback)
}

fn parse(value: &str, fallback: Color) -> Color {
    parse_value(value).unwrap_or(fallback)
}

/// 解析一个主题色值；无法识别时返回 `None`。
///
/// 支持四种写法：
///
/// - **跟随终端**：`default` / `reset` / `terminal` / `none` / `transparent` / `-`
///   都映射到 `Color::Reset`，也就是终端自己的默认前景/背景色。把 `base` 设成它
///   就能透出终端主题（配合透明终端 / 壁纸）；把 `text` 设成它就用终端的前景色。
/// - 16 个 ANSI 名字：`black`…`white`，另接受 `light_*` / `bright_*` 与
///   `dark_gray` 的几种写法。
/// - `#rgb` 与 `#rrggbb`。
/// - ANSI 256 色号：`236` 或 `color236`（引用终端调色板，做"半透明感"常用）。
pub fn parse_value(value: &str) -> Option<Color> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty() {
        // 空值表示"没配"，交给调用方的默认色。
        return None;
    }
    match value.as_str() {
        "default" | "reset" | "terminal" | "none" | "transparent" | "-" => {
            return Some(Color::Reset);
        }
        "black" => return Some(Color::Black),
        "red" => return Some(Color::Red),
        "green" => return Some(Color::Green),
        "yellow" => return Some(Color::Yellow),
        "blue" => return Some(Color::Blue),
        "magenta" => return Some(Color::Magenta),
        "cyan" => return Some(Color::Cyan),
        "gray" | "grey" => return Some(Color::Gray),
        "dark_gray" | "dark-grey" | "darkgray" | "dark_grey" => return Some(Color::DarkGray),
        "white" => return Some(Color::White),
        "light_red" | "light-red" | "lightred" | "bright_red" | "bright-red" => {
            return Some(Color::LightRed);
        }
        "light_green" | "light-green" | "lightgreen" | "bright_green" | "bright-green" => {
            return Some(Color::LightGreen);
        }
        "light_yellow" | "light-yellow" | "lightyellow" | "bright_yellow" | "bright-yellow" => {
            return Some(Color::LightYellow);
        }
        "light_blue" | "light-blue" | "lightblue" | "bright_blue" | "bright-blue" => {
            return Some(Color::LightBlue);
        }
        "light_magenta" | "light-magenta" | "lightmagenta" | "bright_magenta" => {
            return Some(Color::LightMagenta);
        }
        "light_cyan" | "light-cyan" | "lightcyan" | "bright_cyan" | "bright-cyan" => {
            return Some(Color::LightCyan);
        }
        "light_white" | "light-white" | "lightwhite" | "bright_white" => {
            return Some(Color::White);
        }
        _ => {}
    }
    if let Some(hex) = value.strip_prefix('#') {
        return parse_hex(hex);
    }
    if let Some(index) = value
        .strip_prefix("color")
        .and_then(|rest| rest.parse::<u8>().ok())
    {
        return Some(Color::Indexed(index));
    }
    value.parse::<u8>().ok().map(Color::Indexed)
}

/// 解析 `#` 之后的十六进制部分；`3` 位缩写与 `6` 位写法都支持。
///
/// 这里**必须先确认整串都是 ASCII 十六进制字符再切**：以前直接判 `value.len() == 7`
/// （字节长度）就切片，`"#abc晴"` 这种值字节长正好是 7，`&value[3..5]` 会切在汉字
/// 中间，在非字符边界上 panic —— 而主题色每帧都会解析，等于一启动就崩。
fn parse_hex(hex: &str) -> Option<Color> {
    if !hex.chars().all(|character| character.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        3 => {
            let channel = |index: usize| {
                let digit = hex.as_bytes()[index] as char;
                u8::from_str_radix(&format!("{digit}{digit}"), 16).ok()
            };
            Some(Color::Rgb(channel(0)?, channel(1)?, channel(2)?))
        }
        6 => {
            let red = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let green = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let blue = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some(Color::Rgb(red, green, blue))
        }
        _ => None,
    }
}

/// 在配置加载后调用一次：把识别不了的主题色值报出来。
///
/// `parse_value` 认不出来时会静默回退到默认色，用户会以为"改了配置却没生效"，
/// 所以在加载阶段集中提示一次（渲染路径每帧都会解析颜色，不能在那里打日志）。
pub fn warn_unrecognized(theme: &ThemeConfig) {
    let fields: [(&str, &String); 29] = [
        ("accent", &theme.accent),
        ("text", &theme.text),
        ("muted", &theme.muted),
        ("border", &theme.border),
        ("rosewater", &theme.rosewater),
        ("flamingo", &theme.flamingo),
        ("pink", &theme.pink),
        ("mauve", &theme.mauve),
        ("red", &theme.red),
        ("maroon", &theme.maroon),
        ("peach", &theme.peach),
        ("yellow", &theme.yellow),
        ("green", &theme.green),
        ("teal", &theme.teal),
        ("sky", &theme.sky),
        ("sapphire", &theme.sapphire),
        ("blue", &theme.blue),
        ("lavender", &theme.lavender),
        ("subtext_1", &theme.subtext_1),
        ("subtext_0", &theme.subtext_0),
        ("overlay_2", &theme.overlay_2),
        ("overlay_1", &theme.overlay_1),
        ("overlay_0", &theme.overlay_0),
        ("surface_2", &theme.surface_2),
        ("surface_1", &theme.surface_1),
        ("surface_0", &theme.surface_0),
        ("base", &theme.base),
        ("mantle", &theme.mantle),
        ("crust", &theme.crust),
    ];
    for (name, value) in fields {
        if parse_value(value).is_none() && !value.trim().is_empty() {
            tracing::warn!(
                "主题色 theme.{name} = {value:?} 无法识别，已回退到默认色；\
                 可用 #rgb / #rrggbb、ANSI 名字（red/light_blue/…）、256 色号（236 / color236），\
                 或 default（跟随终端）"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SKIN_VOICEFOX;
    use super::{blend, library_theme, next_skin_name, skin_label, skin_names, slot_color};
    use super::{parse, parse_value};
    use lx_core::model::config::ThemeConfig;
    use ratatui::style::Color;
    use ratatui_themes::ThemeName;

    #[test]
    fn parses_hex_color() {
        assert_eq!(parse("#12aBcD", Color::Black), Color::Rgb(0x12, 0xab, 0xcd));
    }

    #[test]
    fn non_ascii_hex_falls_back_instead_of_panicking() {
        // 修复前："#abc晴" 字节长度正好是 7，会走进 &value[3..5] 这类汉字中间的
        // 切片并在非字符边界 panic；主题色每帧都解析，等于一启动就崩。
        assert_eq!(parse("#abc晴", Color::Red), Color::Red);
        assert_eq!(parse("#日日", Color::Red), Color::Red);
        // 位数不对的也一律回退。
        assert_eq!(parse("#12345", Color::Red), Color::Red);
        assert_eq!(parse("#1234567", Color::Red), Color::Red);
    }

    #[test]
    fn three_digit_hex_is_expanded() {
        assert_eq!(parse_value("#abc"), Some(Color::Rgb(0xaa, 0xbb, 0xcc)));
        assert_eq!(parse_value("#F0A"), Some(Color::Rgb(0xff, 0x00, 0xaa)));
    }

    #[test]
    fn default_tokens_follow_the_terminal() {
        for token in [
            "default",
            "reset",
            "terminal",
            "none",
            "transparent",
            "-",
            "  DEFAULT  ",
        ] {
            assert_eq!(parse_value(token), Some(Color::Reset), "{token}");
        }
    }

    #[test]
    fn ansi_names_and_256_indexes_are_supported() {
        assert_eq!(parse_value("light_blue"), Some(Color::LightBlue));
        assert_eq!(parse_value("bright-blue"), Some(Color::LightBlue));
        assert_eq!(parse_value("dark_gray"), Some(Color::DarkGray));
        assert_eq!(parse_value("236"), Some(Color::Indexed(236)));
        assert_eq!(parse_value("color236"), Some(Color::Indexed(236)));
    }

    #[test]
    fn empty_and_unknown_values_are_reported_as_none() {
        assert_eq!(parse_value(""), None);
        assert_eq!(parse_value("   "), None);
        assert_eq!(parse_value("greyish"), None);
        assert_eq!(parse_value("color999"), None);
        assert_eq!(parse_value("300"), None);
    }

    #[test]
    fn base_can_follow_the_terminal() {
        // 方案 A：默认配色不变，但 base 显式设成 default 时必须真的透明。
        let theme = ThemeConfig {
            base: "default".to_string(),
            ..ThemeConfig::default()
        };
        assert_eq!(parse(&theme.base, super::BASE), Color::Reset);
        // 没配的字段仍然用 Mocha 默认值。
        assert_eq!(theme.accent, "#cba6f7");
    }

    // ── 多主题：槽位映射 ──

    const ALL_SLOTS: [&str; 25] = [
        "base",
        "mantle",
        "crust",
        "surface_0",
        "surface_1",
        "surface_2",
        "overlay_0",
        "overlay_1",
        "overlay_2",
        "text",
        "subtext_0",
        "subtext_1",
        "accent",
        "border",
        "yellow",
        "red",
        "green",
        "blue",
        "peach",
        "mauve",
        "lavender",
        "sapphire",
        "sky",
        "teal",
        "selection_fg",
    ];

    #[test]
    fn blend_hits_both_ends_and_the_middle() {
        let black = Color::Rgb(0, 0, 0);
        let white = Color::Rgb(255, 255, 255);
        assert_eq!(blend(black, white, 0.0), black);
        assert_eq!(blend(black, white, 1.0), white);
        let mid = blend(black, white, 0.5);
        assert_eq!(mid, Color::Rgb(128, 128, 128));
        // 非 RGB 颜色无法插值，原样返回起点（终端默认色 / ANSI 名）
        assert_eq!(blend(Color::Reset, white, 0.5), Color::Reset);
        assert_eq!(blend(Color::Red, white, 0.5), Color::Red);
    }

    /// 每个在用的槽位都要能从主题库推导出来，未知槽位返回 None 由调用方回退。
    #[test]
    fn every_slot_used_by_the_ui_maps_to_a_library_theme() {
        let palette = ThemeName::Dracula.palette();
        for slot in ALL_SLOTS {
            assert!(
                slot_color(palette, slot).is_some(),
                "槽位 {slot} 必须有映射，否则该处会退回过时的十六进制默认色"
            );
        }
        assert_eq!(slot_color(palette, "not_a_slot"), None);
    }

    /// 层次色必须按"离背景越来越远"排序，深色与浅色主题都要成立。
    #[test]
    fn surface_layers_are_ordered_for_dark_and_light_themes() {
        let luminance = |color: Color| match color {
            Color::Rgb(r, g, b) => u32::from(r) + u32::from(g) + u32::from(b),
            _ => 0,
        };
        for name in [ThemeName::Dracula, ThemeName::CatppuccinLatte] {
            let palette = name.palette();
            let bg = luminance(palette.bg);
            let fg = luminance(palette.fg);
            let surfaces = ["surface_0", "surface_1", "surface_2"]
                .map(|slot| luminance(slot_color(palette, slot).unwrap()));
            // 深浅方向由 bg/fg 决定，层与层之间必须单调
            if fg >= bg {
                assert!(
                    surfaces[0] < surfaces[1] && surfaces[1] < surfaces[2],
                    "{name:?}"
                );
            } else {
                assert!(
                    surfaces[0] > surfaces[1] && surfaces[1] > surfaces[2],
                    "{name:?}"
                );
            }
            // 每一层都要落在 bg 与 fg 之间（不能跑到更极端，否则会过曝/糊掉）
            let (low, high) = (bg.min(fg), bg.max(fg));
            for value in surfaces {
                assert!(value >= low && value <= high, "{name:?}: {value} 越界");
            }
        }
    }

    /// 语义色必须真的对得上：错误红、成功绿、警告黄。
    #[test]
    fn semantic_slots_take_the_matching_semantic_color() {
        let palette = ThemeName::Nord.palette();
        assert_eq!(slot_color(palette, "red").unwrap(), palette.error);
        assert_eq!(slot_color(palette, "green").unwrap(), palette.success);
        assert_eq!(slot_color(palette, "yellow").unwrap(), palette.warning);
        assert_eq!(slot_color(palette, "blue").unwrap(), palette.info);
        // 选中行文字用主题背景色，才能压在强调色底上读清
        assert_eq!(slot_color(palette, "selection_fg").unwrap(), palette.bg);
    }

    /// 浅色主题也要可用：文字比背景暗。
    #[test]
    fn light_themes_stay_readable() {
        let palette = ThemeName::CatppuccinLatte.palette();
        assert!(palette.is_light(), "这个主题应当是浅色的");
        let text = slot_color(palette, "text").unwrap();
        assert_eq!(text, palette.fg);
        assert_ne!(text, palette.bg, "文字不能和背景同色");
    }

    #[test]
    fn voicefox_is_the_default_and_keeps_the_hand_tuned_slots() {
        // 默认名不走主题库；`configured()` 会用配置里的十六进制色
        assert_eq!(library_theme(SKIN_VOICEFOX), None);
        assert_eq!(library_theme(""), None);
        assert_eq!(library_theme("不存在的主题"), None);
        // 名字大小写/空格不敏感
        assert_eq!(library_theme(" Tokyo-Night "), Some(ThemeName::TokyoNight));
    }

    #[test]
    fn skin_names_cover_voicefox_plus_the_library_and_cycle() {
        let names = skin_names();
        assert_eq!(names[0], SKIN_VOICEFOX, "默认皮肤排第一");
        assert_eq!(names.len(), ThemeName::all().len() + 1);
        let unique: std::collections::HashSet<&String> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "主题名不能重复");

        // 循环一圈回到起点，且每一步都不一样
        let mut current = SKIN_VOICEFOX.to_string();
        for _ in 0..names.len() {
            current = next_skin_name(&current);
        }
        assert_eq!(current, SKIN_VOICEFOX, "循环一轮应当回到起点");
        // 未知名字也要能接上（回退到第二个）
        assert_eq!(next_skin_name("未知"), names[1]);
    }

    #[test]
    fn skin_labels_are_human_readable() {
        assert_eq!(skin_label("tokyo-night"), "Tokyo Night");
        assert_eq!(skin_label(SKIN_VOICEFOX), "Voicefox");
        assert_eq!(skin_label("未知"), "Voicefox");
    }
}
