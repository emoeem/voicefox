use serde::{Deserialize, Serialize};

/// 歌词数据
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LyricData {
    /// 原始歌词（LRC 格式）
    pub lyric: String,
    /// 翻译歌词
    pub tlyric: Option<String>,
    /// 罗马音歌词
    pub rlyric: Option<String>,
    /// 逐字歌词（YRC/QRC 格式）
    pub lxlyric: Option<String>,
    /// 未处理的原始歌词
    pub raw_lrc: Option<String>,
}

/// LRC 解析后的一行歌词
#[derive(Debug, Clone)]
pub struct LyricLine {
    /// 该行起始时间（毫秒）
    pub timestamp: u64,
    /// 该行歌词文本
    pub text: String,
    /// 该行持续时长（毫秒），由下一行时间戳推算
    pub duration: u64,
}

/// YRC/QRC 逐字歌词的一行
#[derive(Debug, Clone)]
pub struct YrcLine {
    pub timestamp: u64,
    pub words: Vec<YrcWord>,
}

/// 单个字的时间信息
#[derive(Debug, Clone)]
pub struct YrcWord {
    pub text: String,
    pub start: u64,
    pub duration: u64,
}

/// 歌词当前状态（线程安全快照）
#[derive(Debug, Clone, Default)]
pub struct LyricState {
    pub current_line: usize,
    pub lines: Vec<LyricLine>,
    pub translation: Option<String>,
    pub yrc_words: Vec<YrcWord>,
    /// 当前播放器位置（毫秒），用于逐字高亮。
    pub position_ms: u64,
    pub is_empty: bool,
}
