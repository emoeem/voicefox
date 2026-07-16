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
            Self::List => Self::None,
            Self::None => Self::ListLoop,
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

    /// 用户主动切换下一首。单曲循环只约束自然播放结束，不拦截手动切歌。
    pub fn manual_next_index(&self, current: usize, total: usize) -> Option<usize> {
        if total == 0 {
            return None;
        }
        match self {
            PlayMode::Random => self.next_index(current, total),
            PlayMode::ListLoop | PlayMode::SingleLoop => Some((current + 1) % total),
            PlayMode::List | PlayMode::None => (current + 1 < total).then_some(current + 1),
        }
    }

    /// 用户主动切换上一首。
    pub fn manual_prev_index(&self, current: usize, total: usize) -> Option<usize> {
        if total == 0 {
            return None;
        }
        match self {
            PlayMode::Random => self.prev_index(current, total),
            PlayMode::ListLoop | PlayMode::SingleLoop => Some((current + total - 1) % total),
            PlayMode::List | PlayMode::None => current.checked_sub(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PlayMode;

    #[test]
    fn cycles_through_every_configurable_mode() {
        let mut mode = PlayMode::ListLoop;
        let mut values = Vec::new();
        for _ in 0..5 {
            values.push(mode.as_config());
            mode = mode.next_mode();
        }

        assert_eq!(
            values,
            vec!["list-loop", "single-loop", "random", "list", "none"]
        );
        assert_eq!(mode, PlayMode::ListLoop);
    }

    #[test]
    fn single_loop_does_not_block_manual_track_changes() {
        assert_eq!(PlayMode::SingleLoop.next_index(1, 3), Some(1));
        assert_eq!(PlayMode::SingleLoop.manual_next_index(1, 3), Some(2));
        assert_eq!(PlayMode::SingleLoop.manual_prev_index(1, 3), Some(0));
    }
}
