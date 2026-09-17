//! 播放列表管理器
//!
//! 对标 go-musicfox PlaylistManager + lx-music player/action.ts

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use lx_core::events::InsertPosition;
use lx_core::model::song::SongInfo;

/// 队列本身用 `RwLock<Arc<Vec<SongInfo>>>` 持有：
///
/// - 渲染/鼠标路径通过 [`borrow`](Self::borrow) 直接借用，零拷贝；
/// - 插入/删除/拖动用 [`Arc::make_mut`](std::sync::Arc::make_mut) 原地修改：
///   没有并发快照时零拷贝，仅当切歌路径还持有旧快照时才复制一次槽位；
/// - 切歌/持久化路径用 [`snapshot_arc`](Self::snapshot_arc) O(1) 拿到
///   `Arc` 快照，跨线程传递不再复制任何歌曲数据。
pub struct PlaylistManager {
    /// 当前播放列表
    current_list: RwLock<Arc<Vec<SongInfo>>>,
    /// 当前播放索引
    current_index: AtomicUsize,
    /// 播放模式（默认列表循环）
    play_mode: Mutex<crate::playlist::mode::PlayMode>,
    /// 从上一次成功开始播放后累计的连续失败次数。
    consecutive_failures: Mutex<usize>,
}

impl PlaylistManager {
    pub fn new(play_mode: crate::playlist::mode::PlayMode) -> Self {
        Self {
            current_list: RwLock::new(Arc::new(vec![])),
            current_index: AtomicUsize::new(0),
            play_mode: Mutex::new(play_mode),
            consecutive_failures: Mutex::new(0),
        }
    }

    /// 设置播放列表
    pub fn set_playlist(&self, songs: Vec<SongInfo>, index: usize) {
        self.set_playlist_arc(Arc::new(songs), index);
    }

    /// 自动跳过失败歌曲时更新列表，但保留连续失败计数。
    pub fn set_playlist_after_failure(&self, songs: Vec<SongInfo>, index: usize) {
        self.set_playlist_arc_after_failure(Arc::new(songs), index);
    }

    /// 以 `Arc` 快照设置播放列表（不复制歌曲数据），供切歌路径使用。
    pub fn set_playlist_arc(&self, songs: Arc<Vec<SongInfo>>, index: usize) {
        self.set_playlist_arc_inner(songs, index, true);
    }

    /// 自动跳过失败歌曲时以 `Arc` 快照更新列表，保留连续失败计数。
    pub fn set_playlist_arc_after_failure(&self, songs: Arc<Vec<SongInfo>>, index: usize) {
        self.set_playlist_arc_inner(songs, index, false);
    }

    fn set_playlist_arc_inner(
        &self,
        songs: Arc<Vec<SongInfo>>,
        index: usize,
        reset_failures: bool,
    ) {
        let mut list = self.current_list.write().unwrap_or_else(|e| e.into_inner());
        self.current_index
            .store(index.min(songs.len().saturating_sub(1)), Ordering::Release);
        *list = songs;
        if reset_failures {
            *self
                .consecutive_failures
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = 0;
        }
    }

    /// 只读借用队列，供渲染与鼠标命中测试使用（零拷贝）。
    pub fn borrow(&self) -> std::sync::RwLockReadGuard<'_, Arc<Vec<SongInfo>>> {
        self.current_list.read().unwrap_or_else(|e| e.into_inner())
    }

    /// 当前索引
    pub fn current_index(&self) -> usize {
        self.current_index.load(Ordering::Acquire)
    }

    /// 全量快照：深拷贝整张队列。仅供低频路径（右键菜单、测试）使用。
    pub fn snapshot(&self) -> (Vec<SongInfo>, usize) {
        (
            self.current_list
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .clone(),
            self.current_index(),
        )
    }

    /// 共享快照：O(1) 复制 `Arc`。供持久化与切歌这类跨线程路径使用。
    pub fn snapshot_arc(&self) -> (Arc<Vec<SongInfo>>, usize) {
        (
            Arc::clone(&self.current_list.read().unwrap_or_else(|e| e.into_inner())),
            self.current_index(),
        )
    }

    #[cfg(target_os = "linux")]
    pub fn len(&self) -> usize {
        self.current_list
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// 将单首歌曲插入当前播放列表，返回插入后的索引。
    pub fn insert(&self, song: SongInfo, position: InsertPosition) -> usize {
        let mut list = self.current_list.write().unwrap_or_else(|e| e.into_inner());
        let current = self.current_index();
        let list = Arc::make_mut(&mut *list);
        if list.is_empty() {
            list.push(song);
            self.current_index.store(0, Ordering::Release);
            return 0;
        }

        let index = match position {
            InsertPosition::Next => current.saturating_add(1).min(list.len()),
            InsertPosition::End => list.len(),
        };
        list.insert(index, song);
        index
    }

    pub fn remove(&self, target: usize) {
        let mut list = self.current_list.write().unwrap_or_else(|e| e.into_inner());
        if target >= list.len() {
            return;
        }
        let list = Arc::make_mut(&mut *list);
        list.remove(target);
        let current = self.current_index();
        let next = if target < current {
            current.saturating_sub(1)
        } else if current >= list.len() {
            list.len().saturating_sub(1)
        } else {
            current
        };
        self.current_index.store(next, Ordering::Release);
    }

    pub fn move_item(&self, from: usize, to: usize) {
        let mut list = self.current_list.write().unwrap_or_else(|e| e.into_inner());
        if from >= list.len() || to >= list.len() || from == to {
            return;
        }
        let list = Arc::make_mut(&mut *list);
        let song = list.remove(from);
        list.insert(to, song);

        let current = self.current_index();
        let next = if current == from {
            to
        } else if from < current && to >= current {
            current.saturating_sub(1)
        } else if from > current && to <= current {
            current.saturating_add(1)
        } else {
            current
        };
        self.current_index.store(next, Ordering::Release);
    }

    pub fn clear(&self) {
        *self.current_list.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(vec![]);
        self.current_index.store(0, Ordering::Release);
        *self
            .consecutive_failures
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = 0;
    }

    /// 非零拷贝版 `next_entry`，保留给测试与外部使用；生产切歌走
    /// [`next_entry_arc`](Self::next_entry_arc) 避免整表深拷贝。
    #[allow(dead_code)]
    pub fn next_entry(&self) -> Option<(Vec<SongInfo>, usize)> {
        self.next_entry_arc()
            .map(|(songs, index)| (songs.as_ref().clone(), index))
    }

    /// 共享快照版“下一首”：只更新索引，列表以 `Arc` 共享返回。
    ///
    /// 播放结束自动切歌走此路径，避免每首歌都复制整张队列。
    pub fn next_entry_arc(&self) -> Option<(Arc<Vec<SongInfo>>, usize)> {
        let list = self.current_list.read().unwrap_or_else(|e| e.into_inner());
        if list.is_empty() {
            return None;
        }
        let current = self.current_index();
        let mode = *self.play_mode.lock().unwrap_or_else(|e| e.into_inner());
        let next = mode.next_index(current, list.len())?;
        self.current_index.store(next, Ordering::Release);
        Some((Arc::clone(&list), next))
    }

    #[allow(dead_code)]
    pub fn next_manual_entry(&self) -> Option<(Vec<SongInfo>, usize)> {
        self.next_manual_entry_arc()
            .map(|(songs, index)| (songs.as_ref().clone(), index))
    }

    pub fn next_manual_entry_arc(&self) -> Option<(Arc<Vec<SongInfo>>, usize)> {
        let list = self.current_list.read().unwrap_or_else(|e| e.into_inner());
        if list.is_empty() {
            return None;
        }
        let current = self.current_index();
        let mode = *self.play_mode.lock().unwrap_or_else(|e| e.into_inner());
        let next = mode.manual_next_index(current, list.len())?;
        self.current_index.store(next, Ordering::Release);
        Some((Arc::clone(&list), next))
    }

    /// 当前歌曲所有可用解析方式都失败后选择下一首。
    ///
    /// 单曲循环在失败时也会前进；列表循环最多尝试一轮，避免所有歌曲
    /// 都不可播放时无限循环。
    #[allow(dead_code)]
    pub fn next_after_failure(&self) -> Option<(Vec<SongInfo>, usize)> {
        self.next_after_failure_arc()
            .map(|(songs, index)| (songs.as_ref().clone(), index))
    }

    pub fn next_after_failure_arc(&self) -> Option<(Arc<Vec<SongInfo>>, usize)> {
        let list = self.current_list.read().unwrap_or_else(|e| e.into_inner());
        if list.is_empty() {
            return None;
        }

        let mut failures = self
            .consecutive_failures
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *failures = failures.saturating_add(1);
        if *failures >= list.len() {
            return None;
        }

        let current = self.current_index();
        let mode = *self.play_mode.lock().unwrap_or_else(|e| e.into_inner());
        let next = mode.manual_next_index(current, list.len())?;
        self.current_index.store(next, Ordering::Release);
        Some((Arc::clone(&list), next))
    }

    /// mpv 已确认开始播放，新的连续失败计数从零开始。
    pub fn mark_playback_started(&self) {
        *self
            .consecutive_failures
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = 0;
    }

    #[allow(dead_code)]
    pub fn prev_manual_entry(&self) -> Option<(Vec<SongInfo>, usize)> {
        self.prev_manual_entry_arc()
            .map(|(songs, index)| (songs.as_ref().clone(), index))
    }

    pub fn prev_manual_entry_arc(&self) -> Option<(Arc<Vec<SongInfo>>, usize)> {
        let list = self.current_list.read().unwrap_or_else(|e| e.into_inner());
        if list.is_empty() {
            return None;
        }
        let current = self.current_index();
        let mode = *self.play_mode.lock().unwrap_or_else(|e| e.into_inner());
        let previous = mode.manual_prev_index(current, list.len())?;
        self.current_index.store(previous, Ordering::Release);
        Some((Arc::clone(&list), previous))
    }

    pub fn mode(&self) -> crate::playlist::mode::PlayMode {
        *self.play_mode.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn cycle_mode(&self) -> crate::playlist::mode::PlayMode {
        let mut mode = self.play_mode.lock().unwrap_or_else(|e| e.into_inner());
        *mode = mode.next_mode();
        *mode
    }
}

#[cfg(test)]
mod tests {
    use super::PlaylistManager;
    use crate::playlist::mode::PlayMode;
    use lx_core::events::InsertPosition;
    use lx_core::model::song::SongInfo;
    use lx_core::model::source::SourceId;

    fn song(id: &str) -> SongInfo {
        SongInfo::new(
            id.to_string(),
            SourceId::Kw,
            id.to_string(),
            "artist".to_string(),
        )
    }

    #[test]
    fn removing_before_current_keeps_the_same_current_song() {
        let playlist = PlaylistManager::new(PlayMode::ListLoop);
        playlist.set_playlist(vec![song("a"), song("b"), song("c")], 2);

        playlist.remove(0);

        let (songs, current) = playlist.snapshot();
        assert_eq!(current, 1);
        assert_eq!(songs[current].id, "c");
    }

    #[test]
    fn removing_current_selects_the_following_song() {
        let playlist = PlaylistManager::new(PlayMode::ListLoop);
        playlist.set_playlist(vec![song("a"), song("b"), song("c")], 1);

        playlist.remove(1);

        let (songs, current) = playlist.snapshot();
        assert_eq!(current, 1);
        assert_eq!(songs[current].id, "c");
    }

    #[test]
    fn removing_last_current_song_selects_previous_song() {
        let playlist = PlaylistManager::new(PlayMode::ListLoop);
        playlist.set_playlist(vec![song("a"), song("b"), song("c")], 2);

        playlist.remove(2);

        let (songs, current) = playlist.snapshot();
        assert_eq!(current, 1);
        assert_eq!(songs[current].id, "b");
    }

    #[test]
    fn moving_queue_items_preserves_current_song() {
        let playlist = PlaylistManager::new(PlayMode::ListLoop);
        playlist.set_playlist(vec![song("a"), song("b"), song("c"), song("d")], 2);

        playlist.move_item(0, 3);

        let (songs, current) = playlist.snapshot();
        assert_eq!(
            songs
                .iter()
                .map(|song| song.id.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "c", "d", "a"]
        );
        assert_eq!(songs[current].id, "c");
    }

    #[test]
    fn appending_song_keeps_the_current_song() {
        let playlist = PlaylistManager::new(PlayMode::ListLoop);
        playlist.set_playlist(vec![song("a"), song("b")], 0);

        let inserted = playlist.insert(song("c"), InsertPosition::End);

        let (songs, current) = playlist.snapshot();
        assert_eq!(inserted, 2);
        assert_eq!(
            songs
                .iter()
                .map(|song| song.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
        assert_eq!(songs[current].id, "a");
    }

    #[test]
    fn inserting_next_places_song_after_current() {
        let playlist = PlaylistManager::new(PlayMode::ListLoop);
        playlist.set_playlist(vec![song("a"), song("b"), song("c")], 1);

        let inserted = playlist.insert(song("next"), InsertPosition::Next);

        let (songs, current) = playlist.snapshot();
        assert_eq!(inserted, 2);
        assert_eq!(
            songs
                .iter()
                .map(|song| song.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "next", "c"]
        );
        assert_eq!(songs[current].id, "b");
    }

    #[test]
    fn inserting_into_empty_queue_selects_the_first_song() {
        let playlist = PlaylistManager::new(PlayMode::ListLoop);

        let inserted = playlist.insert(song("a"), InsertPosition::Next);

        let (songs, current) = playlist.snapshot();
        assert_eq!(inserted, 0);
        assert_eq!(current, 0);
        assert_eq!(songs[current].id, "a");
    }

    #[test]
    fn single_loop_repeats_the_selected_bili_part() {
        let playlist = PlaylistManager::new(PlayMode::SingleLoop);
        playlist.set_playlist(
            vec![
                SongInfo::new(
                    "video-p1".to_string(),
                    SourceId::Bili,
                    "视频 · P1".to_string(),
                    "UP主".to_string(),
                ),
                SongInfo::new(
                    "video-p2".to_string(),
                    SourceId::Bili,
                    "视频 · P2".to_string(),
                    "UP主".to_string(),
                ),
                SongInfo::new(
                    "video-p3".to_string(),
                    SourceId::Bili,
                    "视频 · P3".to_string(),
                    "UP主".to_string(),
                ),
            ],
            1,
        );

        let (songs, index) = playlist.next_entry().expect("single loop has a next entry");

        assert_eq!(index, 1);
        assert_eq!(songs[index].id, "video-p2");
    }

    #[test]
    fn natural_end_respects_list_modes() {
        let list_loop = PlaylistManager::new(PlayMode::ListLoop);
        list_loop.set_playlist(vec![song("a"), song("b")], 1);
        assert_eq!(list_loop.next_entry().unwrap().1, 0);

        let list = PlaylistManager::new(PlayMode::List);
        list.set_playlist(vec![song("a"), song("b")], 0);
        assert_eq!(list.next_entry().unwrap().1, 1);
        assert!(list.next_entry().is_none());

        let stopped = PlaylistManager::new(PlayMode::None);
        stopped.set_playlist(vec![song("a"), song("b")], 0);
        assert!(stopped.next_entry().is_none());
    }

    #[test]
    fn playback_failure_skips_single_loop_song() {
        let playlist = PlaylistManager::new(PlayMode::SingleLoop);
        playlist.set_playlist(vec![song("a"), song("b"), song("c")], 0);

        let (songs, index) = playlist
            .next_after_failure()
            .expect("another song should be attempted");

        assert_eq!(index, 1);
        assert_eq!(songs[index].id, "b");
    }

    #[test]
    fn playback_failures_stop_after_one_list_loop_pass() {
        let playlist = PlaylistManager::new(PlayMode::ListLoop);
        playlist.set_playlist(vec![song("a"), song("b"), song("c")], 1);

        assert_eq!(playlist.next_after_failure().unwrap().1, 2);
        assert_eq!(playlist.next_after_failure().unwrap().1, 0);
        assert!(playlist.next_after_failure().is_none());
    }

    #[test]
    fn successful_playback_resets_failure_limit() {
        let playlist = PlaylistManager::new(PlayMode::ListLoop);
        playlist.set_playlist(vec![song("a"), song("b")], 0);

        assert_eq!(playlist.next_after_failure().unwrap().1, 1);
        playlist.mark_playback_started();
        assert_eq!(playlist.next_after_failure().unwrap().1, 0);
    }
}
