#![allow(dead_code)]

use lx_core::model::config::ThemeConfig;
use ratatui::style::Color;

use crate::context::AppContext;

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

pub fn accent(ctx: &AppContext) -> Color {
    configured(ctx, |theme| &theme.accent, MAUVE)
}

pub fn border(ctx: &AppContext) -> Color {
    configured(ctx, |theme| &theme.border, SURFACE_2)
}

pub fn text(ctx: &AppContext) -> Color {
    configured(ctx, |theme| &theme.text, TEXT)
}

pub fn muted(ctx: &AppContext) -> Color {
    configured(ctx, |theme| &theme.muted, SUBTEXT_0)
}

macro_rules! palette_color {
    ($name:ident, $field:ident, $fallback:ident) => {
        pub fn $name(ctx: &AppContext) -> Color {
            configured(ctx, |theme| &theme.$field, $fallback)
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
    crust(ctx)
}

fn configured(ctx: &AppContext, value: fn(&ThemeConfig) -> &String, fallback: Color) -> Color {
    let config = ctx.config.read().unwrap();
    parse(value(&config.theme), fallback)
}

fn parse(value: &str, fallback: Color) -> Color {
    match value.trim().to_ascii_lowercase().as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "dark_gray" | "dark-grey" | "darkgray" => Color::DarkGray,
        "white" => Color::White,
        value if value.starts_with('#') && value.len() == 7 => {
            let red = u8::from_str_radix(&value[1..3], 16);
            let green = u8::from_str_radix(&value[3..5], 16);
            let blue = u8::from_str_radix(&value[5..7], 16);
            match (red, green, blue) {
                (Ok(red), Ok(green), Ok(blue)) => Color::Rgb(red, green, blue),
                _ => fallback,
            }
        }
        _ => fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::parse;
    use ratatui::style::Color;

    #[test]
    fn parses_hex_color() {
        assert_eq!(parse("#12aBcD", Color::Black), Color::Rgb(0x12, 0xab, 0xcd));
    }
}
