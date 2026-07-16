use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use lx_core::model::lyric::{LyricLine, LyricState};
use lx_core::model::song::SongInfo;
use lx_core::traits::lyric_fetcher::LyricFetcher;

/// 歌词服务：获取 → 解析 → 进度跟踪
#[allow(dead_code)]
pub struct LyricService {
    fetcher: Arc<dyn LyricFetcher>,
    state: RwLock<LyricState>,
    lines: RwLock<Vec<LyricLine>>,
    trans_lines: RwLock<Vec<(usize, String)>>,
    show_translation: RwLock<bool>,
    show_yrc: RwLock<bool>,
    offset: RwLock<Duration>,
    generation: AtomicU64,
}

impl LyricService {
    pub fn new(fetcher: Arc<dyn LyricFetcher>) -> Self {
        Self {
            fetcher,
            state: RwLock::new(LyricState::default()),
            lines: RwLock::new(Vec::new()),
            trans_lines: RwLock::new(Vec::new()),
            show_translation: RwLock::new(false),
            show_yrc: RwLock::new(false),
            offset: RwLock::new(Duration::ZERO),
            generation: AtomicU64::new(0),
        }
    }

    pub fn prepare(&self) -> u64 {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.lines.write().unwrap().clear();
        self.trans_lines.write().unwrap().clear();
        *self.state.write().unwrap() = LyricState::default();
        generation
    }

    /// 为一首歌加载歌词
    pub async fn load(&self, song: &SongInfo, generation: u64) -> anyhow::Result<()> {
        let data = self
            .fetcher
            .fetch(song)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        if self.generation.load(Ordering::SeqCst) != generation {
            return Ok(());
        }
        let lrc_lines = crate::parser::lrc::parse(&data.lyric);

        // 解析翻译歌词
        let trans = if let Some(ref t) = data.tlyric {
            Self::align_translation(&lrc_lines, t)
        } else {
            vec![]
        };

        if self.generation.load(Ordering::SeqCst) == generation {
            *self.lines.write().unwrap() = lrc_lines;
            *self.trans_lines.write().unwrap() = trans;
            *self.state.write().unwrap() = LyricState::default();
        }
        Ok(())
    }

    pub fn clear(&self) {
        self.prepare();
    }

    /// 根据播放位置更新当前歌词行
    pub fn update_position(&self, position: Duration) {
        let lines = self.lines.read().unwrap();
        if lines.is_empty() {
            let mut state = self.state.write().unwrap();
            state.current_line = 0;
            state.lines = vec![];
            state.translation = None;
            state.is_empty = true;
            return;
        }

        let pos_ms = position.as_millis() as u64;

        // 找到当前时间戳对应的行：最后一个 timestamp <= pos_ms 的行
        let current = lines
            .iter()
            .enumerate()
            .rev()
            .find(|(_, line)| line.timestamp <= pos_ms)
            .map(|(i, _)| i)
            .unwrap_or(0);

        let trans = self.trans_lines.read().unwrap();
        let translation = trans
            .iter()
            .find(|(i, _)| *i == current)
            .map(|(_, t)| t.clone());

        let mut state = self.state.write().unwrap();
        state.current_line = current;
        state.lines = lines.clone();
        state.translation = translation;
        state.is_empty = false;
    }

    /// 对齐翻译歌词到原文行
    fn align_translation(lines: &[LyricLine], tlyric: &str) -> Vec<(usize, String)> {
        // 解析翻译 LRC
        let trans_lines = crate::parser::lrc::parse(tlyric);
        // 按时间戳匹配最近的主歌词行
        trans_lines
            .iter()
            .map(|t| {
                let idx = lines
                    .iter()
                    .position(|l| l.timestamp >= t.timestamp)
                    .unwrap_or(lines.len().saturating_sub(1));
                (idx, t.text.clone())
            })
            .collect()
    }

    /// 获取当前歌词状态快照
    pub fn current_state(&self) -> LyricState {
        self.state.read().unwrap().clone()
    }

    pub fn set_translation_enabled(&self, enabled: bool) {
        *self.show_translation.write().unwrap() = enabled;
    }
}
