//! 网易云 YRC 与 lx-music 统一逐字歌词格式解析。

use std::sync::OnceLock;

use lx_core::model::lyric::{YrcLine, YrcWord};
use regex::Regex;

/// 支持两种常见格式：
///
/// - `[1234,3000](1234,300,0)字...`
/// - `[00:01.234]<0,300>字...`
pub fn parse(content: &str) -> Vec<YrcLine> {
    static NUMERIC_LINE: OnceLock<Regex> = OnceLock::new();
    static TIMESTAMP_LINE: OnceLock<Regex> = OnceLock::new();
    static YRC_WORD: OnceLock<Regex> = OnceLock::new();
    static LX_WORD: OnceLock<Regex> = OnceLock::new();
    let numeric_line = NUMERIC_LINE.get_or_init(|| {
        Regex::new(r"^\[\s*(\d+)\s*,\s*\d+\s*\]").expect("valid YRC numeric line regex")
    });
    let timestamp_line = TIMESTAMP_LINE.get_or_init(|| {
        Regex::new(r"^\[(\d+):(\d{1,2})[.:](\d{1,3})\]").expect("valid YRC timestamp line regex")
    });
    let yrc_word = YRC_WORD.get_or_init(|| {
        Regex::new(r"\((-?\d+),(-?\d+)(?:,-?\d+)?\)").expect("valid YRC word regex")
    });
    let lx_word = LX_WORD
        .get_or_init(|| Regex::new(r"<(-?\d+),(-?\d+)(?:,-?\d+)?>").expect("valid LX word regex"));
    let mut lines = Vec::new();

    for raw_line in content.lines() {
        let line = raw_line.trim();
        let Some((timestamp, body_start)) =
            parse_line_timestamp(line, numeric_line, timestamp_line)
        else {
            continue;
        };
        let body = &line[body_start..];

        let words = if lx_word.is_match(body) {
            parse_prefixed_words(body, timestamp, lx_word, true)
        } else {
            parse_prefixed_words(body, timestamp, yrc_word, false)
        };
        if !words.is_empty() {
            lines.push(YrcLine { timestamp, words });
        }
    }

    lines.sort_by_key(|line| line.timestamp);
    lines
}

fn parse_line_timestamp(
    line: &str,
    numeric_line: &Regex,
    timestamp_line: &Regex,
) -> Option<(u64, usize)> {
    if let Some(captures) = numeric_line.captures(line) {
        return Some((
            captures.get(1)?.as_str().parse().ok()?,
            captures.get(0)?.end(),
        ));
    }

    let captures = timestamp_line.captures(line)?;
    let minutes: u64 = captures.get(1)?.as_str().parse().ok()?;
    let seconds: u64 = captures.get(2)?.as_str().parse().ok()?;
    let fraction = captures.get(3)?.as_str();
    let value: u64 = fraction.parse().ok()?;
    let millis = match fraction.len() {
        1 => value * 100,
        2 => value * 10,
        _ => value,
    };
    Some((
        minutes * 60_000 + seconds * 1_000 + millis,
        captures.get(0)?.end(),
    ))
}

fn parse_prefixed_words(
    body: &str,
    line_timestamp: u64,
    word_regex: &Regex,
    starts_are_relative: bool,
) -> Vec<YrcWord> {
    let matches = word_regex.captures_iter(body).collect::<Vec<_>>();
    let mut words = Vec::with_capacity(matches.len());

    for (index, captures) in matches.iter().enumerate() {
        let Some(marker) = captures.get(0) else {
            continue;
        };
        let text_end = matches
            .get(index + 1)
            .and_then(|next| next.get(0))
            .map_or(body.len(), |next| next.start());
        let text = body[marker.end()..text_end].to_string();
        if text.is_empty() {
            continue;
        }

        let raw_start = captures
            .get(1)
            .and_then(|value| value.as_str().parse::<i64>().ok())
            .unwrap_or(0)
            .max(0) as u64;
        let duration = captures
            .get(2)
            .and_then(|value| value.as_str().parse::<i64>().ok())
            .unwrap_or(0)
            .unsigned_abs();
        // 绝对时间戳略小于行首时间通常是舍入误差，直接钳制到行首，
        // 不要当作相对偏移再叠加整个行时间戳
        let start = if starts_are_relative {
            line_timestamp.saturating_add(raw_start)
        } else {
            raw_start.max(line_timestamp)
        };
        words.push(YrcWord {
            text,
            start,
            duration,
        });
    }

    words
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn parses_yrc_absolute_word_timestamps() {
        let lines = parse("[3380,3388](3380,847,0)词(4227,847,0)：(5074,847,0)许(5921,847,0)嵩");

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].timestamp, 3380);
        assert_eq!(lines[0].words[1].text, "：");
        assert_eq!(lines[0].words[1].start, 4227);
        assert_eq!(lines[0].words[1].duration, 847);
    }

    #[test]
    fn parses_lx_music_relative_word_timestamps() {
        let lines = parse("[00:12.340]<0,200>你<200,300>好");

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].words[0].start, 12_340);
        assert_eq!(lines[0].words[1].start, 12_540);
    }
}
