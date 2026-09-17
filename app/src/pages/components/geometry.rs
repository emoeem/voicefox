use ratatui::layout::Rect;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabRect {
    pub index: usize,
    pub rect: Rect,
}

/// Calculate tab hit boxes from the exact display labels used by the renderer.
pub fn tab_rects<I, S>(area: Rect, labels: I, gap: u16, max_width: usize) -> Vec<TabRect>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut x = area.x;
    let mut result = Vec::new();
    for (index, label) in labels.into_iter().enumerate() {
        let label = truncate_width(label.as_ref(), max_width);
        let width = UnicodeWidthStr::width(label.as_str()) as u16 + 2;
        if width == 0 || x.saturating_add(width) > area.right() {
            break;
        }
        result.push(TabRect {
            index,
            rect: Rect::new(x, area.y, width, 1),
        });
        x = x.saturating_add(width + gap);
    }
    result
}

pub fn truncate_width(value: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(value) <= max_width {
        return value.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in value.chars() {
        let width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + width + 1 > max_width {
            break;
        }
        out.push(ch);
        used += width;
    }
    out.push('…');
    out
}
