//! 播放列表管理器
//!
//! 对标 go-musicfox PlaylistManager + lx-music player/action.ts

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

use lx_core::events::InsertPosition;
use lx_core::model::song::SongInfo;
use rand::SeedableRng;
use rand::rngs::StdRng;

use crate::playlist::mode::PlayMode;
use crate::playlist::shuffle::{ABSENT_CAP, ShuffleScheduler, ShuffleState, SongKey};

/// 替换队列时调度器的同步方式
enum ListChange<'a> {
    /// 只同步歌曲集合，当前曲已由切歌路径登记
    Sync,
    /// 从 `index` 开始播放，登记为一次新的播放
    Start,
    /// 当前曲是展开 `replaced` 得到的，接替它已登记的这次播放
    Expand(&'a SongInfo),
    /// 恢复上次的会话，调度器尚未把当前曲作为当前播放时登记它
    Restore,
}

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
    play_mode: Mutex<PlayMode>,
    /// 从上一次成功开始播放后累计的连续失败次数。
    consecutive_failures: Mutex<usize>,
    /// 随机播放调度器，歌曲集合与队列保持一致。与队列同时加锁时先锁队列。
    shuffle: Mutex<ShuffleScheduler>,
    /// 最近一次写入磁盘的调度器修订号
    shuffle_saved: AtomicU64,
}

impl PlaylistManager {
    pub fn new(play_mode: PlayMode) -> Self {
        Self {
            current_list: RwLock::new(Arc::new(vec![])),
            current_index: AtomicUsize::new(0),
            play_mode: Mutex::new(play_mode),
            consecutive_failures: Mutex::new(0),
            shuffle: Mutex::new(ShuffleScheduler::new(
                StdRng::from_entropy(),
                play_mode == PlayMode::Random,
            )),
            shuffle_saved: AtomicU64::new(0),
        }
    }

    fn shuffle(&self) -> MutexGuard<'_, ShuffleScheduler> {
        self.shuffle.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 设置播放列表，当前曲不计为一次新的播放
    pub fn set_playlist(&self, songs: Vec<SongInfo>, index: usize) {
        self.set_playlist_arc(Arc::new(songs), index);
    }

    /// 用户选择从 `index` 开始播放：替换列表并把该曲登记为一次新的播放。
    ///
    /// 随机模式下以该曲为起点重新生成后续顺序。
    pub fn start_playlist(&self, songs: Vec<SongInfo>, index: usize) {
        self.replace_list(Arc::new(songs), index, true, ListChange::Start);
    }

    /// 恢复上次的播放会话。
    ///
    /// 调度器的当前曲与会话一致时只同步歌曲集合。不一致时（从旧版升级、调度器状态丢失或损坏、
    /// 两份文件不同步）把当前曲登记为一次播放，使随后的选歌避开它，回退也能回到它。
    pub fn restore_playlist(&self, songs: Vec<SongInfo>, index: usize) {
        self.replace_list(Arc::new(songs), index, true, ListChange::Restore);
    }

    /// 自动跳过失败歌曲时从 `index` 开始播放，保留连续失败计数。
    pub fn start_playlist_after_failure(&self, songs: Vec<SongInfo>, index: usize) {
        self.replace_list(Arc::new(songs), index, false, ListChange::Start);
    }

    /// 队列中的 `replaced` 在开始播放前被展开替换（B 站视频展开为分 P）。
    ///
    /// `index` 处的歌接替 `replaced` 已登记的这次播放，不算新的播放。
    pub fn expand_playlist(
        &self,
        songs: Vec<SongInfo>,
        index: usize,
        replaced: &SongInfo,
        reset_failures: bool,
    ) {
        self.replace_list(
            Arc::new(songs),
            index,
            reset_failures,
            ListChange::Expand(replaced),
        );
    }

    /// 以 `Arc` 快照设置播放列表（不复制歌曲数据），供切歌路径使用。
    ///
    /// 切歌路径的下标已由播放模式或调度器选定，当前曲不再登记。
    pub fn set_playlist_arc(&self, songs: Arc<Vec<SongInfo>>, index: usize) {
        self.replace_list(songs, index, true, ListChange::Sync);
    }

    /// 自动跳过失败歌曲时以 `Arc` 快照更新列表，保留连续失败计数。
    pub fn set_playlist_arc_after_failure(&self, songs: Arc<Vec<SongInfo>>, index: usize) {
        self.replace_list(songs, index, false, ListChange::Sync);
    }

    fn replace_list(
        &self,
        songs: Arc<Vec<SongInfo>>,
        index: usize,
        reset_failures: bool,
        change: ListChange<'_>,
    ) {
        let mut list = self.current_list.write().unwrap_or_else(|e| e.into_inner());
        let index = index.min(songs.len().saturating_sub(1));
        let keys = songs.iter().map(SongKey::of);
        match change {
            // 同一份快照的歌曲集合必然未变，无需同步调度器
            ListChange::Sync if Arc::ptr_eq(&list, &songs) => {}
            ListChange::Sync => self.shuffle().set_list(keys, None),
            ListChange::Start => {
                let start = songs.get(index).map(SongKey::of);
                self.shuffle().set_list(keys, start.as_ref());
            }
            ListChange::Restore => {
                let mut shuffle = self.shuffle();
                let start = songs
                    .get(index)
                    .map(SongKey::of)
                    .filter(|key| shuffle.current() != Some(key));
                shuffle.set_list(keys, start.as_ref());
            }
            ListChange::Expand(replaced) => {
                if let Some(song) = songs.get(index) {
                    self.shuffle().replace_current(
                        keys,
                        &SongKey::of(replaced),
                        &SongKey::of(song),
                    );
                }
            }
        }
        self.current_index.store(index, Ordering::Release);
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

    /// 当前播放列表长度。
    ///
    /// 注意：**不要**给这个方法加平台 cfg —— 状态栏（`StatusBarItem::Queue`）在
    /// 所有平台都会调用它，加 cfg 会让 macOS / Windows 构建直接失败（E0599）。
    pub fn len(&self) -> usize {
        self.current_list
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// 将单首歌曲插入当前播放列表，返回插入后的索引。
    ///
    /// 插入空队列即从该曲开始播放。随机模式下"下一首播放"同时进入调度器的插队栈。
    pub fn insert(&self, song: SongInfo, position: InsertPosition) -> usize {
        let mut list = self.current_list.write().unwrap_or_else(|e| e.into_inner());
        let current = self.current_index();
        let list = Arc::make_mut(&mut *list);
        let key = SongKey::of(&song);
        if list.is_empty() {
            list.push(song);
            self.current_index.store(0, Ordering::Release);
            self.shuffle().set_list([key.clone()], Some(&key));
            return 0;
        }

        let index = match position {
            InsertPosition::Next => current.saturating_add(1).min(list.len()),
            InsertPosition::End => list.len(),
        };
        list.insert(index, song);
        let mut shuffle = self.shuffle();
        shuffle.add(key.clone());
        if matches!(position, InsertPosition::Next) {
            shuffle.insert(&key);
        }
        index
    }

    pub fn remove(&self, target: usize) {
        let mut list = self.current_list.write().unwrap_or_else(|e| e.into_inner());
        if target >= list.len() {
            return;
        }
        let list = Arc::make_mut(&mut *list);
        let current = self.current_index();
        let removed = SongKey::of(&list[target]);
        // 与 locate 的查找顺序一致：从当前曲起向后的距离，当前曲本身排在最后
        let n = list.len();
        let distance = |i: usize| (i + n - current.min(n - 1) - 1) % n;
        // 同一首歌在队列中出现多次时只算一个候选。插队时播放的是当前曲之后最近的一条，
        // 删掉这一条即撤销插队
        let mut duplicated = false;
        let mut pending = true;
        for (i, song) in list.iter().enumerate() {
            if i != target && SongKey::of(song) == removed {
                duplicated = true;
                pending &= distance(i) > distance(target);
            }
        }
        list.remove(target);
        if !duplicated {
            self.shuffle().remove(&removed);
        } else if pending {
            self.shuffle().cancel_insert(&removed);
        }
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
        let mut list = self.current_list.write().unwrap_or_else(|e| e.into_inner());
        *list = Arc::new(vec![]);
        self.shuffle().set_list([], None);
        self.current_index.store(0, Ordering::Release);
        *self
            .consecutive_failures
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = 0;
    }

    /// 按播放模式选出下一首或上一首的下标并登记播放。
    ///
    /// 随机模式由调度器选歌，其余模式由 `by_index` 按下标计算。
    fn step(
        &self,
        list: &[SongInfo],
        scheduled: fn(&mut ShuffleScheduler) -> Option<SongKey>,
        backward: bool,
        by_index: fn(&PlayMode, usize, usize) -> Option<usize>,
    ) -> Option<usize> {
        let mode = self.mode();
        if mode != PlayMode::Random {
            let index = by_index(&mode, self.current_index(), list.len())?;
            self.shuffle().observe(&SongKey::of(&list[index]));
            return Some(index);
        }
        let mut shuffle = self.shuffle();
        for _ in 0..2 {
            let key = scheduled(&mut shuffle)?;
            if let Some(index) = Self::locate(list, &key, self.current_index(), backward) {
                return Some(index);
            }
            // 调度器选出的歌不在队列中，说明二者不一致，按队列重新同步后再选一次
            tracing::warn!("shuffle scheduler out of sync with the queue");
            shuffle.set_list(list.iter().map(SongKey::of), None);
        }
        None
    }

    /// `key` 在队列中的下标。同一首歌有多个条目时，从当前曲起沿播放方向取最近的一条，
    /// 使"下一首播放"插在当前曲之后的重复条目被选中。
    fn locate(list: &[SongInfo], key: &SongKey, current: usize, backward: bool) -> Option<usize> {
        let n = list.len();
        let current = current.min(n.saturating_sub(1));
        (1..=n)
            .map(|d| {
                if backward {
                    (current + n - d) % n
                } else {
                    (current + d) % n
                }
            })
            .find(|&i| SongKey::of(&list[i]) == *key)
    }

    fn scheduled_next(shuffle: &mut ShuffleScheduler) -> Option<SongKey> {
        shuffle.next().map(|(key, _)| key)
    }

    fn scheduled_prev(shuffle: &mut ShuffleScheduler) -> Option<SongKey> {
        shuffle.prev()
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
        let next = self.step(&list, Self::scheduled_next, false, PlayMode::next_index)?;
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
        let next = self.step(
            &list,
            Self::scheduled_next,
            false,
            PlayMode::manual_next_index,
        )?;
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

        {
            let mut failures = self
                .consecutive_failures
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *failures = failures.saturating_add(1);
            if *failures >= list.len() {
                return None;
            }
        }

        let next = self.step(
            &list,
            Self::scheduled_next,
            false,
            PlayMode::manual_next_index,
        )?;
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

    /// 用户主动切换上一首。随机模式沿播放历史回退，已在历史起点时重播起点那首。
    pub fn prev_manual_entry_arc(&self) -> Option<(Arc<Vec<SongInfo>>, usize)> {
        let list = self.current_list.read().unwrap_or_else(|e| e.into_inner());
        if list.is_empty() {
            return None;
        }
        let previous = self.step(
            &list,
            Self::scheduled_prev,
            true,
            PlayMode::manual_prev_index,
        )?;
        self.current_index.store(previous, Ordering::Release);
        Some((Arc::clone(&list), previous))
    }

    pub fn mode(&self) -> PlayMode {
        *self.play_mode.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn cycle_mode(&self) -> PlayMode {
        let mode = {
            let mut mode = self.play_mode.lock().unwrap_or_else(|e| e.into_inner());
            *mode = mode.next_mode();
            *mode
        };
        self.shuffle().set_random(mode == PlayMode::Random);
        mode
    }

    /// 直接指定播放模式（底栏菜单里的"精确选择"用）。
    pub fn set_mode(&self, mode: PlayMode) -> PlayMode {
        *self.play_mode.lock().unwrap_or_else(|e| e.into_inner()) = mode;
        self.shuffle().set_random(mode == PlayMode::Random);
        mode
    }

    /// 以持久化的调度器状态替换当前调度器，随机模式以当前播放模式为准。
    ///
    /// 返回状态是否可用。歌曲集合随后由恢复的队列同步。
    pub fn restore_shuffle(&self, state: ShuffleState) -> bool {
        let Some(mut scheduler) = ShuffleScheduler::from_state(state, StdRng::from_entropy())
        else {
            return false;
        };
        let saved = scheduler.revision();
        scheduler.set_random(self.mode() == PlayMode::Random);
        *self.shuffle() = scheduler;
        self.shuffle_saved.store(saved, Ordering::Release);
        true
    }

    /// 调度器自上次保存以来有变化时，清理缺席状态并返回修订号与待保存的状态
    pub fn shuffle_state_for_save(&self) -> Option<(u64, ShuffleState)> {
        let mut shuffle = self.shuffle();
        if shuffle.revision() == self.shuffle_saved.load(Ordering::Acquire) {
            return None;
        }
        shuffle.prune(ABSENT_CAP);
        Some((shuffle.revision(), shuffle.state()))
    }

    pub fn mark_shuffle_saved(&self, revision: u64) {
        self.shuffle_saved.store(revision, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::PlaylistManager;
    use crate::playlist::mode::PlayMode;
    use crate::playlist::shuffle::SongKey;
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

    fn ids(songs: &[SongInfo]) -> Vec<&str> {
        songs.iter().map(|song| song.id.as_str()).collect()
    }

    #[test]
    fn random_mode_follows_the_scheduler_without_immediate_repeats() {
        let playlist = PlaylistManager::new(PlayMode::Random);
        let songs: Vec<SongInfo> = (0..8).map(|i| song(&format!("s{i}"))).collect();
        playlist.start_playlist(songs, 0);

        let mut played = vec!["s0".to_string()];
        for _ in 0..200 {
            let (songs, index) = playlist.next_entry().expect("random mode never ends");
            assert_eq!(playlist.current_index(), index);
            let id = songs[index].id.clone();
            assert_ne!(played.last(), Some(&id));
            played.push(id);
        }
        played.sort();
        played.dedup();
        assert_eq!(played.len(), 8);
    }

    #[test]
    fn random_prev_walks_back_through_history() {
        let playlist = PlaylistManager::new(PlayMode::Random);
        let songs: Vec<SongInfo> = (0..10).map(|i| song(&format!("s{i}"))).collect();
        playlist.start_playlist(songs, 4);
        let first = playlist.next_entry().unwrap().1;
        let second = playlist.next_entry().unwrap().1;
        assert_ne!(first, second);

        assert_eq!(playlist.prev_manual_entry().unwrap().1, first);
        assert_eq!(playlist.prev_manual_entry().unwrap().1, 4);
        assert_eq!(playlist.prev_manual_entry().unwrap().1, 4);
        assert_eq!(playlist.next_manual_entry().unwrap().1, first);
        assert_eq!(playlist.next_manual_entry().unwrap().1, second);
    }

    #[test]
    fn play_next_of_a_duplicate_plays_the_inserted_entry() {
        let playlist = PlaylistManager::new(PlayMode::Random);
        let songs: Vec<SongInfo> = (0..10).map(|i| song(&format!("s{i}"))).collect();
        playlist.start_playlist(songs, 5);
        let inserted = playlist.insert(song("s2"), InsertPosition::Next);
        assert_eq!(inserted, 6);
        assert_eq!(playlist.next_entry().unwrap().1, 6);
        // 回退时取当前曲之前最近的一条
        assert_eq!(playlist.prev_manual_entry().unwrap().1, 5);
    }

    #[test]
    fn removing_the_inserted_duplicate_cancels_play_next() {
        let playlist = PlaylistManager::new(PlayMode::Random);
        let songs: Vec<SongInfo> = (0..30).map(|i| song(&format!("s{i}"))).collect();
        playlist.start_playlist(songs, 10);
        let key = SongKey::of(&song("s2"));

        let inserted = playlist.insert(song("s2"), InsertPosition::Next);
        playlist.remove(inserted);
        assert!(!playlist.shuffle().is_inserted(&key));

        // 删掉的是别处原有的那一条时，插队保留
        let inserted = playlist.insert(song("s2"), InsertPosition::Next);
        playlist.remove(2);
        assert!(playlist.shuffle().is_inserted(&key));
        let (songs, next) = playlist.next_entry().unwrap();
        assert_eq!((songs[next].id.as_str(), next), ("s2", inserted - 1));
    }

    #[test]
    fn play_next_in_random_mode_is_played_next() {
        let playlist = PlaylistManager::new(PlayMode::Random);
        let songs: Vec<SongInfo> = (0..30).map(|i| song(&format!("s{i}"))).collect();
        playlist.start_playlist(songs, 0);
        playlist.next_entry();

        playlist.insert(song("inserted"), InsertPosition::Next);
        playlist.insert(song("s20"), InsertPosition::End);

        let (songs, index) = playlist.next_entry().unwrap();
        assert_eq!(songs[index].id, "inserted");
        assert_eq!(songs.len(), 32);
    }

    #[test]
    fn expanded_parts_take_over_the_current_play() {
        let playlist = PlaylistManager::new(PlayMode::Random);
        let mut songs: Vec<SongInfo> = (0..30).map(|i| song(&format!("s{i}"))).collect();
        songs.push(song("video"));
        playlist.start_playlist(songs, 30);
        playlist.insert(song("inserted"), InsertPosition::Next);

        let (mut songs, index) = playlist.snapshot();
        let video = songs.splice(index..=index, [song("video-p1"), song("video-p2")]);
        let video = video.collect::<Vec<_>>().remove(0);
        playlist.expand_playlist(songs, index, &video, true);

        let (songs, next) = playlist.next_entry().unwrap();
        assert_eq!(songs[next].id, "inserted");
        let (songs, prev) = playlist.prev_manual_entry().unwrap();
        assert_eq!(songs[prev].id, "video-p1");
        let (songs, prev) = playlist.prev_manual_entry().unwrap();
        assert_eq!(songs[prev].id, "video-p1");
    }

    #[test]
    fn inserting_into_empty_random_queue_starts_from_that_song() {
        let playlist = PlaylistManager::new(PlayMode::Random);
        playlist.insert(song("a"), InsertPosition::Next);
        playlist.insert(song("b"), InsertPosition::End);

        let (songs, index) = playlist.next_entry().unwrap();
        assert_eq!(songs[index].id, "b");
        assert_eq!(playlist.prev_manual_entry().unwrap().1, 0);
    }

    #[test]
    fn duplicate_entries_share_one_candidate() {
        let playlist = PlaylistManager::new(PlayMode::Random);
        playlist.start_playlist(vec![song("a"), song("b"), song("a")], 1);

        playlist.remove(0);
        let (songs, _) = playlist.snapshot();
        assert_eq!(ids(&songs), vec!["b", "a"]);
        let (songs, index) = playlist.next_entry().unwrap();
        assert_eq!(songs[index].id, "a");
        assert_eq!(index, 1);

        playlist.remove(1);
        for _ in 0..5 {
            let (songs, index) = playlist.next_entry().unwrap();
            assert_eq!(songs[index].id, "b");
        }
    }

    #[test]
    fn leaving_random_mode_returns_to_index_order() {
        let playlist = PlaylistManager::new(PlayMode::Random);
        playlist.start_playlist(vec![song("a"), song("b"), song("c")], 0);
        playlist.next_entry();

        playlist.set_mode(PlayMode::ListLoop);
        let current = playlist.current_index();
        let (_, index) = playlist.next_entry().unwrap();
        assert_eq!(index, (current + 1) % 3);
    }

    #[test]
    fn shuffle_state_is_saved_only_after_changes() {
        let playlist = PlaylistManager::new(PlayMode::Random);
        assert!(playlist.shuffle_state_for_save().is_none());

        playlist.start_playlist(vec![song("a"), song("b"), song("c")], 0);
        let (revision, state) = playlist.shuffle_state_for_save().unwrap();
        playlist.mark_shuffle_saved(revision);
        assert!(playlist.shuffle_state_for_save().is_none());

        let restored = PlaylistManager::new(PlayMode::Random);
        assert!(restored.restore_shuffle(state));
        assert!(restored.shuffle_state_for_save().is_none());
        // 恢复的队列与保存时一致，当前曲已是调度器的当前曲，恢复不改变调度器状态
        restored.restore_playlist(vec![song("a"), song("b"), song("c")], 0);
        assert!(restored.shuffle_state_for_save().is_none());
    }

    #[test]
    fn restoring_without_shuffle_state_registers_the_current_song() {
        for _ in 0..50 {
            let playlist = PlaylistManager::new(PlayMode::Random);
            playlist.restore_playlist(vec![song("a"), song("b")], 0);
            assert_eq!(playlist.next_entry().unwrap().1, 1);
            assert_eq!(playlist.prev_manual_entry().unwrap().1, 0);
        }
        // 非随机模式下同样登记，切到随机模式后不会紧接着重播当前曲
        for _ in 0..50 {
            let playlist = PlaylistManager::new(PlayMode::ListLoop);
            playlist.restore_playlist(vec![song("a"), song("b")], 0);
            playlist.set_mode(PlayMode::Random);
            assert_eq!(playlist.next_entry().unwrap().1, 1);
        }
    }
}
