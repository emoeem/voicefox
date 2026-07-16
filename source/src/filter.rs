//! 字符串过滤工具（对标 lx-music src/renderer/utils/musicSdk/utils.js）
//!
//! findMusic 算法使用的标准化函数 — TODO: Phase 6

/// 去除特殊字符 + 小写化
pub fn filter_str(s: &str) -> String {
    s.chars()
        .filter(|c| {
            !matches!(
                c,
                ' ' | '\'' | '.' | ',' | '&' | '"' | '、' | '(' | ')' | '（'
                    | '）' | '`' | '~' | '-' | '<' | '>' | '|' | '/' | ']' | '['
                    | '!' | '！'
            )
        })
        .collect::<String>()
        .to_lowercase()
}

/// 歌手名排序（按分隔符拆分后排序，再合并）
/// "周杰伦、方文山" → "方文山周杰伦"
pub fn sort_singer(singer: &str) -> String {
    let mut parts: Vec<&str> = singer
        .split(&['、', '&', ';', '；', '/', ',', '，', '|'][..])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    parts.sort();
    parts.concat()
}
