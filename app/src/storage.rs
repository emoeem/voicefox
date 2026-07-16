//! JSON 文件存储 — 收藏 + 播放历史

use std::fs;
use std::path::PathBuf;
use std::sync::RwLock;

use lx_core::model::song::SongInfo;

const MAX_HISTORY: usize = 100;

pub struct Storage {
    data_dir: PathBuf,
    favorites: RwLock<Vec<SongInfo>>,
    history: RwLock<Vec<SongInfo>>,
}

impl Storage {
    pub fn new() -> Self {
        let dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("voicefox")
            .join("data");
        fs::create_dir_all(&dir).ok();
        let favorites = Self::load_file(&dir.join("favorites.json"));
        let history = Self::load_file(&dir.join("history.json"));
        Self {
            data_dir: dir,
            favorites: RwLock::new(favorites),
            history: RwLock::new(history),
        }
    }

    // ── 收藏 ──────────────────────────────────────────

    pub fn add_favorite(&self, song: &SongInfo) {
        let mut favs = self.favorites.write().unwrap();
        if !favs
            .iter()
            .any(|s| s.id == song.id && s.source == song.source)
        {
            favs.push(song.clone());
            self.save_favorites(&favs);
        }
    }

    pub fn remove_favorite(&self, song: &SongInfo) {
        let mut favs = self.favorites.write().unwrap();
        let old_len = favs.len();
        favs.retain(|s| !(s.id == song.id && s.source == song.source));
        if favs.len() != old_len {
            self.save_favorites(&favs);
        }
    }

    pub fn is_favorite(&self, song: &SongInfo) -> bool {
        self.favorites
            .read()
            .unwrap()
            .iter()
            .any(|s| s.id == song.id && s.source == song.source)
    }

    // ── 播放历史 ──────────────────────────────────────

    pub fn add_history(&self, song: &SongInfo) {
        let mut history = self.history.write().unwrap();
        history.retain(|s| !(s.id == song.id && s.source == song.source));
        history.insert(0, song.clone());
        history.truncate(MAX_HISTORY);
        self.save_history(&history);
    }

    // ── 内部序列化 ────────────────────────────────────

    pub fn load_favorites(&self) -> Vec<SongInfo> {
        self.favorites.read().unwrap().clone()
    }

    fn save_favorites(&self, favs: &[SongInfo]) {
        let path = self.data_dir.join("favorites.json");
        if let Ok(json) = serde_json::to_string_pretty(favs) {
            let _ = fs::write(&path, json);
        }
    }

    pub fn load_history(&self) -> Vec<SongInfo> {
        self.history.read().unwrap().clone()
    }

    fn load_file(path: &std::path::Path) -> Vec<SongInfo> {
        if path.exists() {
            match fs::read_to_string(path) {
                Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
                Err(_) => vec![],
            }
        } else {
            vec![]
        }
    }

    fn save_history(&self, history: &[SongInfo]) {
        let path = self.data_dir.join("history.json");
        if let Ok(json) = serde_json::to_string_pretty(history) {
            let _ = fs::write(&path, json);
        }
    }
}

impl Default for Storage {
    fn default() -> Self {
        Self::new()
    }
}
