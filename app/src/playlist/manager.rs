//! 播放列表管理器
//!
//! 对标 go-musicfox PlaylistManager + lx-music player/action.ts

use std::sync::Mutex;

use lx_core::events::InsertPosition;
use lx_core::model::song::SongInfo;

pub struct PlaylistManager {
    /// 当前播放列表
    current_list: Mutex<Vec<SongInfo>>,
    /// 当前播放索引
    current_index: Mutex<usize>,
    /// 播放模式（默认列表循环）
    play_mode: Mutex<crate::playlist::mode::PlayMode>,
}

impl PlaylistManager {
    pub fn new(play_mode: crate::playlist::mode::PlayMode) -> Self {
        Self {
            current_list: Mutex::new(vec![]),
            current_index: Mutex::new(0),
            play_mode: Mutex::new(play_mode),
        }
    }

    /// 设置播放列表
    pub fn set_playlist(&self, songs: Vec<SongInfo>, index: usize) {
        let mut list = self.current_list.lock().unwrap();
        *list = songs;
        let mut idx = self.current_index.lock().unwrap();
        *idx = index.min(list.len().saturating_sub(1));
    }

    pub fn snapshot(&self) -> (Vec<SongInfo>, usize) {
        (
            self.current_list.lock().unwrap().clone(),
            *self.current_index.lock().unwrap(),
        )
    }

    /// 将单首歌曲插入当前播放列表，返回插入后的索引。
    pub fn insert(&self, song: SongInfo, position: InsertPosition) -> usize {
        let mut list = self.current_list.lock().unwrap();
        let mut current = self.current_index.lock().unwrap();
        if list.is_empty() {
            list.push(song);
            *current = 0;
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
        let mut list = self.current_list.lock().unwrap();
        if target >= list.len() {
            return;
        }
        list.remove(target);
        let mut index = self.current_index.lock().unwrap();
        if target < *index {
            *index = index.saturating_sub(1);
        } else if *index >= list.len() {
            *index = list.len().saturating_sub(1);
        }
    }

    pub fn move_item(&self, from: usize, to: usize) {
        let mut list = self.current_list.lock().unwrap();
        if from >= list.len() || to >= list.len() || from == to {
            return;
        }
        let song = list.remove(from);
        list.insert(to, song);

        let mut current = self.current_index.lock().unwrap();
        if *current == from {
            *current = to;
        } else if from < *current && to >= *current {
            *current -= 1;
        } else if from > *current && to <= *current {
            *current += 1;
        }
    }

    pub fn clear(&self) {
        self.current_list.lock().unwrap().clear();
        *self.current_index.lock().unwrap() = 0;
    }

    pub fn next_entry(&self) -> Option<(Vec<SongInfo>, usize)> {
        let list = self.current_list.lock().unwrap();
        if list.is_empty() {
            return None;
        }
        let mut index = self.current_index.lock().unwrap();
        let mode = *self.play_mode.lock().unwrap();
        let next = mode.next_index(*index, list.len())?;
        *index = next;
        Some((list.clone(), next))
    }

    pub fn next_manual_entry(&self) -> Option<(Vec<SongInfo>, usize)> {
        let list = self.current_list.lock().unwrap();
        if list.is_empty() {
            return None;
        }
        let mut index = self.current_index.lock().unwrap();
        let mode = *self.play_mode.lock().unwrap();
        let next = mode.manual_next_index(*index, list.len())?;
        *index = next;
        Some((list.clone(), next))
    }

    pub fn prev_manual_entry(&self) -> Option<(Vec<SongInfo>, usize)> {
        let list = self.current_list.lock().unwrap();
        if list.is_empty() {
            return None;
        }
        let mut index = self.current_index.lock().unwrap();
        let mode = *self.play_mode.lock().unwrap();
        let previous = mode.manual_prev_index(*index, list.len())?;
        *index = previous;
        Some((list.clone(), previous))
    }

    pub fn mode(&self) -> crate::playlist::mode::PlayMode {
        *self.play_mode.lock().unwrap()
    }

    pub fn cycle_mode(&self) -> crate::playlist::mode::PlayMode {
        let mut mode = self.play_mode.lock().unwrap();
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
}
