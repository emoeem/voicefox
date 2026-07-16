//! 播放模式策略
//!
//! 对标 go-musicfox playlist/interfaces.go PlayMode

/// 播放模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayMode {
    /// 列表循环
    ListLoop,
    /// 顺序播放（到底停止）
    List,
    /// 单曲循环
    SingleLoop,
    /// 随机播放
    Random,
    /// 停止
    None,
}

impl PlayMode {
    pub fn from_config(value: &str) -> Self {
        match value {
            "list" => Self::List,
            "single-loop" => Self::SingleLoop,
            "random" => Self::Random,
            "none" => Self::None,
            _ => Self::ListLoop,
        }
    }

    pub fn as_config(self) -> &'static str {
        match self {
            Self::ListLoop => "list-loop",
            Self::List => "list",
            Self::SingleLoop => "single-loop",
            Self::Random => "random",
            Self::None => "none",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ListLoop => "列表循环",
            Self::List => "顺序播放",
            Self::SingleLoop => "单曲循环",
            Self::Random => "随机播放",
            Self::None => "播完停止",
        }
    }

    pub fn next_mode(self) -> Self {
        match self {
            Self::ListLoop => Self::SingleLoop,
            Self::SingleLoop => Self::Random,
            Self::Random => Self::List,
            Self::List | Self::None => Self::ListLoop,
        }
    }

    /// 计算下一首索引
    pub fn next_index(&self, current: usize, total: usize) -> Option<usize> {
        if total == 0 {
            return None;
        }
        match self {
            PlayMode::ListLoop => Some((current + 1) % total),
            PlayMode::List => {
                if current + 1 < total {
                    Some(current + 1)
                } else {
                    None
                }
            }
            PlayMode::SingleLoop => Some(current),
            PlayMode::Random => {
                let seed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .subsec_nanos() as usize;
                let mut next = seed % total;
                if total > 1 && next == current {
                    next = (next + 1) % total;
                }
                Some(next)
            }
            PlayMode::None => None,
        }
    }

    /// 计算上一首索引
    pub fn prev_index(&self, current: usize, total: usize) -> Option<usize> {
        if total == 0 {
            return None;
        }
        match self {
            PlayMode::ListLoop | PlayMode::List => Some((current + total - 1) % total),
            PlayMode::SingleLoop => Some(current),
            PlayMode::Random => {
                let seed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .subsec_nanos() as usize;
                let mut previous = seed % total;
                if total > 1 && previous == current {
                    previous = (previous + total - 1) % total;
                }
                Some(previous)
            }
            PlayMode::None => None,
        }
    }
}
