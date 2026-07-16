//! 歌词格式解析器

use lx_core::model::lyric::YrcLine;

pub mod krc; // 酷狗 KRC
pub mod lrc; // 标准 LRC
pub mod qrc; // QQ音乐 QRC
pub mod yrc; // 网易云 YRC

/// 自动识别 YRC、QRC 和 lx-music 的统一逐字格式。
pub fn parse_karaoke(content: &str) -> Vec<YrcLine> {
    let yrc = yrc::parse(content);
    let qrc = qrc::parse(content);
    let yrc_words = yrc.iter().map(|line| line.words.len()).sum::<usize>();
    let qrc_words = qrc.iter().map(|line| line.words.len()).sum::<usize>();
    if qrc_words > yrc_words { qrc } else { yrc }
}
