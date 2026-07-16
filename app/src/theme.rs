use ratatui::style::Color;

use crate::context::AppContext;

pub fn accent(ctx: &AppContext) -> Color {
    parse(&ctx.config.read().unwrap().theme.accent, Color::Cyan)
}

pub fn border(ctx: &AppContext) -> Color {
    parse(&ctx.config.read().unwrap().theme.border, accent(ctx))
}

pub fn text(ctx: &AppContext) -> Color {
    parse(&ctx.config.read().unwrap().theme.text, Color::White)
}

pub fn muted(ctx: &AppContext) -> Color {
    parse(&ctx.config.read().unwrap().theme.muted, Color::DarkGray)
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
