use lx_core::model::song::SongInfo;
use lx_core::model::source::Quality;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub fn header(width: u16) -> String {
    format_columns(
        width as usize,
        "#",
        "歌曲",
        "歌手",
        "专辑",
        "时长",
        "音质",
        "来源",
    )
}

pub fn row(song: &SongInfo, index: usize, width: u16) -> String {
    let index = (index + 1).to_string();
    let duration = format_duration(song.duration);
    let quality = song
        .qualities
        .iter()
        .next_back()
        .map(|quality| quality_label(*quality))
        .unwrap_or("-");

    format_columns(
        width as usize,
        &index,
        &song.name,
        &song.singer,
        &song.album_name,
        &duration,
        quality,
        song.source.as_str(),
    )
}

#[allow(clippy::too_many_arguments)]
fn format_columns(
    width: usize,
    index: &str,
    name: &str,
    singer: &str,
    album: &str,
    duration: &str,
    quality: &str,
    source: &str,
) -> String {
    if width >= 96 {
        let fixed = 4 + 20 + 7 + 8 + 7;
        let flexible = width.saturating_sub(fixed);
        let name_width = flexible.div_ceil(2);
        let album_width = flexible.saturating_sub(name_width);
        return [
            cell(index, 4),
            cell(name, name_width),
            cell(singer, 20),
            cell(album, album_width),
            cell(duration, 7),
            cell(quality, 8),
            cell(source, 7),
        ]
        .concat();
    }

    if width >= 64 {
        let name_width = width.saturating_sub(4 + 20 + 7 + 7);
        return [
            cell(index, 4),
            cell(name, name_width),
            cell(singer, 20),
            cell(duration, 7),
            cell(source, 7),
        ]
        .concat();
    }

    let name_width = width.saturating_sub(4 + 7);
    let compact_name = if singer.trim().is_empty() {
        name.to_string()
    } else {
        format!("{} - {}", name.trim(), singer.trim())
    };
    [
        cell(index, 4),
        cell(&compact_name, name_width),
        cell(source, 7),
    ]
    .concat()
}

fn cell(value: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }

    let value = value.trim();
    let value_width = UnicodeWidthStr::width(value);
    if value_width <= width {
        return format!("{}{}", value, " ".repeat(width - value_width));
    }
    if width == 1 {
        return "…".to_string();
    }

    let content_width = width - 1;
    let mut rendered = String::new();
    let mut rendered_width = 0;
    for ch in value.chars() {
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if rendered_width + char_width > content_width {
            break;
        }
        rendered.push(ch);
        rendered_width += char_width;
    }
    rendered.push('…');
    rendered.push_str(&" ".repeat(width.saturating_sub(rendered_width + 1)));
    rendered
}

fn format_duration(duration: std::time::Duration) -> String {
    if duration.is_zero() {
        return "--:--".to_string();
    }
    let total = duration.as_secs();
    format!("{:02}:{:02}", total / 60, total % 60)
}

fn quality_label(quality: Quality) -> &'static str {
    match quality {
        Quality::Low128 => "128K",
        Quality::High320 => "320K",
        Quality::Flac => "FLAC",
        Quality::Flac24 => "Hi-Res",
    }
}

#[cfg(test)]
mod tests {
    use super::cell;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn truncates_cjk_to_terminal_width() {
        let value = cell("一首很长的中文歌曲", 8);
        assert_eq!(UnicodeWidthStr::width(value.as_str()), 8);
        assert!(value.contains('…'));
    }
}
