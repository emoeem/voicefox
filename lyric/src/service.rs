use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use lx_core::model::lyric::{LyricLine, LyricState, YrcLine};
use lx_core::model::song::SongInfo;
use lx_core::traits::lyric_fetcher::LyricFetcher;

/// 歌词服务：获取 → 解析 → 进度跟踪
#[allow(dead_code)]
pub struct LyricService {
    fetcher: Arc<dyn LyricFetcher>,
    state: RwLock<LyricState>,
    lines: RwLock<Vec<LyricLine>>,
    yrc_lines: RwLock<Vec<YrcLine>>,
    trans_lines: RwLock<Vec<(usize, String)>>,
    show_translation: RwLock<bool>,
    show_yrc: RwLock<bool>,
    offset_ms: RwLock<i64>,
    generation: AtomicU64,
}

impl LyricService {
    pub fn new(fetcher: Arc<dyn LyricFetcher>) -> Self {
        Self {
            fetcher,
            state: RwLock::new(LyricState::default()),
            lines: RwLock::new(Vec::new()),
            yrc_lines: RwLock::new(Vec::new()),
            trans_lines: RwLock::new(Vec::new()),
            show_translation: RwLock::new(false),
            show_yrc: RwLock::new(false),
            offset_ms: RwLock::new(0),
            generation: AtomicU64::new(0),
        }
    }

    pub fn prepare(&self) -> u64 {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.lines.write().unwrap().clear();
        self.yrc_lines.write().unwrap().clear();
        self.trans_lines.write().unwrap().clear();
        *self.state.write().unwrap() = LyricState::default();
        generation
    }

    /// 为一首歌加载歌词
    pub async fn load(&self, song: &SongInfo, generation: u64) -> anyhow::Result<()> {
        self.load_at(song, generation, Duration::ZERO).await
    }

    /// 为一首歌加载歌词，并在解析完成后立即定位到当前播放位置。
    pub async fn load_at(
        &self,
        song: &SongInfo,
        generation: u64,
        position: Duration,
    ) -> anyhow::Result<()> {
        let data = self
            .fetcher
            .fetch(song)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        if self.generation.load(Ordering::SeqCst) != generation {
            return Ok(());
        }
        let mut lrc_lines = crate::parser::lrc::parse(&data.lyric);
        let yrc_lines = if *self.show_yrc.read().unwrap() {
            data.lxlyric
                .as_deref()
                .map(crate::parser::parse_karaoke)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        if lrc_lines.is_empty() && !yrc_lines.is_empty() {
            lrc_lines = Self::karaoke_as_lines(&yrc_lines);
        }

        // 解析翻译歌词
        let trans = if let Some(ref t) = data.tlyric {
            Self::align_translation(&lrc_lines, t)
        } else {
            vec![]
        };

        if self.generation.load(Ordering::SeqCst) == generation {
            *self.lines.write().unwrap() = lrc_lines;
            *self.yrc_lines.write().unwrap() = yrc_lines;
            *self.trans_lines.write().unwrap() = trans;
            *self.state.write().unwrap() = LyricState::default();
            self.update_position(position);
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
            state.yrc_words.clear();
            state.position_ms = position.as_millis() as u64;
            state.is_empty = true;
            return;
        }

        let offset = *self.offset_ms.read().unwrap();
        let pos_ms = (position.as_millis() as i64).saturating_add(offset).max(0) as u64;

        // 找到当前时间戳对应的行：最后一个 timestamp <= pos_ms 的行
        let current = lines
            .iter()
            .enumerate()
            .rev()
            .find(|(_, line)| line.timestamp <= pos_ms)
            .map(|(i, _)| i)
            .unwrap_or(0);

        let trans = self.trans_lines.read().unwrap();
        let translation = if *self.show_translation.read().unwrap() {
            trans
                .iter()
                .find(|(i, _)| *i == current)
                .map(|(_, t)| t.clone())
        } else {
            None
        };
        let yrc_words = if *self.show_yrc.read().unwrap() {
            self.yrc_lines
                .read()
                .unwrap()
                .iter()
                .rev()
                .find(|line| line.timestamp <= pos_ms)
                .map(|line| line.words.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        let mut state = self.state.write().unwrap();
        state.current_line = current;
        state.lines = lines.clone();
        state.translation = translation;
        state.yrc_words = yrc_words;
        state.position_ms = pos_ms;
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

    fn karaoke_as_lines(lines: &[YrcLine]) -> Vec<LyricLine> {
        lines
            .iter()
            .enumerate()
            .map(|(index, line)| {
                let words_end = line
                    .words
                    .iter()
                    .map(|word| word.start.saturating_add(word.duration))
                    .max()
                    .unwrap_or(line.timestamp);
                let next_timestamp = lines
                    .get(index + 1)
                    .map(|next| next.timestamp)
                    .unwrap_or(words_end.max(line.timestamp.saturating_add(5_000)));
                LyricLine {
                    timestamp: line.timestamp,
                    text: line.words.iter().map(|word| word.text.as_str()).collect(),
                    duration: next_timestamp.saturating_sub(line.timestamp),
                }
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

    pub fn set_yrc_enabled(&self, enabled: bool) {
        *self.show_yrc.write().unwrap() = enabled;
    }

    pub fn set_offset_ms(&self, offset_ms: i32) {
        *self.offset_ms.write().unwrap() = i64::from(offset_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::LyricService;
    use async_trait::async_trait;
    use lx_core::model::lyric::LyricData;
    use lx_core::model::song::SongInfo;
    use lx_core::model::source::SourceId;
    use lx_core::traits::lyric_fetcher::{LyricFetchError, LyricFetcher};
    use std::sync::Arc;
    use std::time::Duration;

    struct StaticFetcher {
        karaoke: bool,
    }

    #[async_trait]
    impl LyricFetcher for StaticFetcher {
        async fn fetch(&self, _song: &SongInfo) -> Result<LyricData, LyricFetchError> {
            if self.karaoke {
                Ok(LyricData {
                    lxlyric: Some(
                        "[5000,3000](5000,500,0)逐(5500,500,0)字(6000,500,0)歌词".to_string(),
                    ),
                    ..LyricData::default()
                })
            } else {
                Ok(LyricData {
                    lyric: "[00:00.00]第一行\n[00:05.00]第二行\n[00:10.00]第三行".to_string(),
                    ..LyricData::default()
                })
            }
        }
    }

    #[tokio::test]
    async fn positions_newly_loaded_lyrics_at_current_playback_time() {
        let service = LyricService::new(Arc::new(StaticFetcher { karaoke: false }));
        let generation = service.prepare();
        let song = SongInfo::new(
            "song".to_string(),
            SourceId::Kw,
            "title".to_string(),
            "artist".to_string(),
        );

        service
            .load_at(&song, generation, Duration::from_secs(7))
            .await
            .unwrap();

        let state = service.current_state();
        assert_eq!(state.current_line, 1);
        assert_eq!(state.lines[state.current_line].text, "第二行");
    }

    #[tokio::test]
    async fn keeps_the_same_line_when_playback_position_is_paused() {
        let service = LyricService::new(Arc::new(StaticFetcher { karaoke: false }));
        let generation = service.prepare();
        let song = SongInfo::new(
            "song".to_string(),
            SourceId::Kw,
            "title".to_string(),
            "artist".to_string(),
        );
        service
            .load_at(&song, generation, Duration::from_secs(7))
            .await
            .unwrap();

        service.update_position(Duration::from_secs(7));
        service.update_position(Duration::from_secs(7));

        assert_eq!(service.current_state().current_line, 1);
    }

    #[tokio::test]
    async fn exposes_word_timing_at_the_current_position() {
        let service = LyricService::new(Arc::new(StaticFetcher { karaoke: true }));
        service.set_yrc_enabled(true);
        let generation = service.prepare();
        let song = SongInfo::new(
            "song".to_string(),
            SourceId::Kw,
            "title".to_string(),
            "artist".to_string(),
        );

        service
            .load_at(&song, generation, Duration::from_millis(5_750))
            .await
            .unwrap();

        let state = service.current_state();
        assert_eq!(state.lines[0].text, "逐字歌词");
        assert_eq!(state.yrc_words.len(), 3);
        assert_eq!(state.position_ms, 5_750);
    }
}
