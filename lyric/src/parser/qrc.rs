//! QQ 音乐 QRC 逐字歌词解析。

use lx_core::model::lyric::{YrcLine, YrcWord};
use regex::Regex;

/// QRC 的时间标签位于对应字词之后：
/// `[14727,2711]You (14727,169)know (14896,175)...`
pub fn parse(content: &str) -> Vec<YrcLine> {
    let content = extract_lyric_content(content);
    let line_regex = Regex::new(r"^\[\s*(\d+)\s*,\s*\d+\s*\]").unwrap();
    let word_regex = Regex::new(r"\((-?\d+),(-?\d+)(?:,-?\d+)?\)").unwrap();
    let mut lines = Vec::new();

    for raw_line in content.lines() {
        let line = raw_line.trim();
        let Some(captures) = line_regex.captures(line) else {
            continue;
        };
        let timestamp = captures
            .get(1)
            .and_then(|value| value.as_str().parse::<u64>().ok())
            .unwrap_or(0);
        let Some(header) = captures.get(0) else {
            continue;
        };
        let body = &line[header.end()..];
        let mut previous_end = 0;
        let mut words = Vec::new();

        for captures in word_regex.captures_iter(body) {
            let Some(marker) = captures.get(0) else {
                continue;
            };
            let text = body[previous_end..marker.start()].to_string();
            previous_end = marker.end();
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
            let start = if raw_start < timestamp {
                timestamp.saturating_add(raw_start)
            } else {
                raw_start
            };
            words.push(YrcWord {
                text,
                start,
                duration,
            });
        }

        if !words.is_empty() {
            lines.push(YrcLine { timestamp, words });
        }
    }

    lines.sort_by_key(|line| line.timestamp);
    lines
}

fn extract_lyric_content(content: &str) -> String {
    let Some((_, rest)) = content.split_once("LyricContent=\"") else {
        return content.to_string();
    };
    let body = rest.split_once("\"/>").map_or(rest, |(body, _)| body);
    body.replace("&#10;", "\n")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn parses_qrc_suffix_timestamps() {
        let lines =
            parse("[14727,2711]You (14727,169)know (14896,175)you (15071,177)care(15248,328)");

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].words.len(), 4);
        assert_eq!(lines[0].words[0].text, "You ");
        assert_eq!(lines[0].words[3].text, "care");
        assert_eq!(lines[0].words[3].start, 15_248);
    }
}
