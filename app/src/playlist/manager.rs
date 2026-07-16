//! 播放列表管理器
//!
//! 对标 go-musicfox PlaylistManager + lx-music player/action.ts

use std::sync::Mutex;

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

    pub fn prev_entry(&self) -> Option<(Vec<SongInfo>, usize)> {
        let list = self.current_list.lock().unwrap();
        if list.is_empty() {
            return None;
        }
        let mut index = self.current_index.lock().unwrap();
        let mode = *self.play_mode.lock().unwrap();
        let previous = mode.prev_index(*index, list.len())?;
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
}
