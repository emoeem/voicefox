//! 标准 LRC 格式解析
//!
//! 格式：[mm:ss.xx]歌词文本
//!
//! 参考 go-musicfox pkg/lyric/lrc.go

use std::sync::OnceLock;

use lx_core::model::lyric::LyricLine;
use regex::Regex;

/// 解析 LRC 文本为 LyricLine 数组（按时间升序排列）
pub fn parse(content: &str) -> Vec<LyricLine> {
    static TIMESTAMP: OnceLock<Regex> = OnceLock::new();
    static OFFSET: OnceLock<Regex> = OnceLock::new();
    static WORD_TAG: OnceLock<Regex> = OnceLock::new();
    let re = TIMESTAMP.get_or_init(|| {
        Regex::new(r"\[(\d{1,3}):(\d{2})(?:[.:,](\d{1,3}))?\]").expect("valid LRC timestamp regex")
    });
    let offset_re = OFFSET.get_or_init(|| {
        Regex::new(r"(?i)\[offset:\s*([+-]?\d+)\s*\]").expect("valid LRC offset regex")
    });
    // 增强型 LRC 的行内字标签（如 <00:12.34>）不是正文，解析时剥离
    let word_tag_re = WORD_TAG.get_or_init(|| {
        Regex::new(r"<\d{1,3}:\d{1,2}(?:[.:,]\d{1,3})?>").expect("valid LRC word tag regex")
    });
    // offset 标签按规范首次出现即生效
    let offset_ms = offset_re
        .captures_iter(content)
        .filter_map(|captures| captures.get(1)?.as_str().parse::<i64>().ok())
        .next()
        .unwrap_or_default();
    let mut lines: Vec<LyricLine> = Vec::new();

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        // 找所有时间戳匹配
        let mut timestamps: Vec<u64> = Vec::new();
        let mut last_end = 0;
        for cap in re.captures_iter(line) {
            if let Some(m) = cap.get(0) {
                let min: u64 = cap[1].parse().unwrap_or(0);
                let sec: u64 = cap[2].parse().unwrap_or(0);
                let ms: u64 = cap.get(3).map_or(0, |fraction| {
                    let ms_str = fraction.as_str();
                    let val: u64 = ms_str.parse().unwrap_or(0);
                    match ms_str.len() {
                        1 => val * 100,
                        2 => val * 10,
                        _ => val,
                    }
                });
                let timestamp = min * 60_000 + sec * 1000 + ms;
                timestamps.push(apply_offset(timestamp, offset_ms));
                last_end = m.end();
            }
        }

        // 提取文本（最后一个时间戳之后的内容），并剥离行内 <mm:ss.xx> 字标签
        let text = word_tag_re
            .replace_all(&line[last_end..], "")
            .trim()
            .to_string();
        if text.is_empty() {
            continue;
        }

        // 每个时间戳对应一行相同的歌词文本
        for ts in timestamps {
            lines.push(LyricLine {
                timestamp: ts,
                text: text.clone(),
                duration: 0, // 先置0，后续计算
            });
        }
    }

    // 按时间戳升序排序
    lines.sort_by_key(|l| l.timestamp);

    // 计算每行 duration：下一行的 timestamp - 当前行 timestamp
    for i in 0..lines.len() {
        let next_ts = if i + 1 < lines.len() {
            lines[i + 1].timestamp
        } else {
            lines[i].timestamp + 5000 // 最后一行默认 5s
        };
        // duration 不会为负（已按时间排序）
        lines[i].duration = next_ts.saturating_sub(lines[i].timestamp);
    }

    lines
}

fn apply_offset(timestamp: u64, offset_ms: i64) -> u64 {
    if offset_ms >= 0 {
        timestamp.saturating_add(offset_ms as u64)
    } else {
        timestamp.saturating_sub(offset_ms.unsigned_abs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_basic() {
        let input = "[00:12.34]第一行歌词\n[00:45.67]第二行歌词\n[01:20.00]第三行歌词";
        let result = parse(input);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].text, "第一行歌词");
        assert_eq!(result[0].timestamp, 12 * 1000 + 340); // 12.34s
        assert_eq!(result[1].text, "第二行歌词");
        assert_eq!(result[2].text, "第三行歌词");
    }

    #[test]
    fn test_parse_empty() {
        let result = parse("");
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_parse_duration() {
        let input = "[00:00.00]第一行\n[00:05.000]第二行\n[00:10.000]第三行";
        let result = parse(input);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].duration, 5000);
        assert_eq!(result[1].duration, 5000);
        assert_eq!(result[2].duration, 5000); // 最后一行默认5s
    }

    #[test]
    fn test_parse_multi_timestamp() {
        let input = "[00:01.00][00:02.00]重复歌词";
        let result = parse(input);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].timestamp, 1000);
        assert_eq!(result[1].timestamp, 2000);
        assert_eq!(result[0].text, "重复歌词");
        assert_eq!(result[1].text, "重复歌词");
    }

    #[test]
    fn applies_lrc_offset_to_every_timestamp() {
        let result = parse("[offset:+500]\n[00:01.00]第一行\n[00:02.00]第二行");

        assert_eq!(result[0].timestamp, 1_500);
        assert_eq!(result[1].timestamp, 2_500);

        let result = parse("[offset:-1500]\n[00:01]第一行\n[0:02,5]第二行");
        assert_eq!(result[0].timestamp, 0);
        assert_eq!(result[1].timestamp, 1_000);
    }

    #[test]
    fn uses_the_first_offset_tag() {
        let result = parse("[offset:+1000]\n[offset:+5000]\n[00:01.00]第一行");

        assert_eq!(result[0].timestamp, 2_000);
    }

    #[test]
    fn strips_enhanced_word_tags_from_text() {
        let result = parse("[00:01.00]<00:01.00>你<00:01.50>好");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].text, "你好");
    }
}
