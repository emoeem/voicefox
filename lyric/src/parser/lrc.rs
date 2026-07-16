//! 标准 LRC 格式解析
//!
//! 格式：[mm:ss.xx]歌词文本
//!
//! 参考 go-musicfox pkg/lyric/lrc.go

use lx_core::model::lyric::LyricLine;
use regex::Regex;

/// 解析 LRC 文本为 LyricLine 数组（按时间升序排列）
pub fn parse(content: &str) -> Vec<LyricLine> {
    let re = Regex::new(r"\[(\d{2}):(\d{2})\.(\d{2,3})\]").unwrap();
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
                let ms: u64 = {
                    let ms_str = &cap[3];
                    let val: u64 = ms_str.parse().unwrap_or(0);
                    if ms_str.len() == 2 {
                        val * 10 // 2 位 ms → 补到 3 位精度
                    } else {
                        val
                    }
                };
                timestamps.push(min * 60_000 + sec * 1000 + ms);
                last_end = m.end();
            }
        }

        // 提取文本（最后一个时间戳之后的内容）
        let text = line[last_end..].trim().to_string();
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
}
