//! JSON 文件存储 — 歌曲/歌单收藏、播放历史和播放会话

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use lx_core::model::playlist::Playlist;
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;

/// 播放历史的紧凑存储条目。
///
/// 历史页只展示歌曲名/歌手/专辑/时长/来源，播放时 URL 也会重新解析，
/// 因此不需要持久化 `SongInfo` 里的大字段（`extra` 哈希表、`toggle_source`
/// 递归结构、`audio` 编码参数等）。存成紧凑条目后，收藏/历史文件体积和
/// 内存占用都能明显下降；`#[serde(default)]` 保证旧版完整 `SongInfo` JSON
/// 仍可平滑读入。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default = "default_history_source")]
    pub source: SourceId,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub singer: String,
    #[serde(default)]
    pub album_name: String,
    #[serde(default)]
    pub duration: Duration,
    #[serde(default)]
    pub cover_url: Option<String>,
    #[serde(default)]
    pub qualities: std::collections::BTreeSet<lx_core::model::source::Quality>,
    #[serde(default)]
    pub file_path: Option<PathBuf>,
}

fn default_history_source() -> SourceId {
    SourceId::Kw
}

impl HistoryEntry {
    /// 从完整歌曲信息中提取历史需要的字段，丢弃重型字段。
    fn from_song(song: &SongInfo) -> Self {
        Self {
            id: song.id.clone(),
            source: song.source,
            name: song.name.clone(),
            singer: song.singer.clone(),
            album_name: song.album_name.clone(),
            duration: song.duration,
            cover_url: song.cover_url.clone(),
            qualities: song.qualities.clone(),
            file_path: song.file_path.clone(),
        }
    }

    /// 还原为可播放的 `SongInfo`。播放路径会重新解析 URL，因此重型字段
    /// 使用默认空值即可。
    fn into_song(self) -> SongInfo {
        SongInfo {
            id: self.id,
            source: self.source,
            name: self.name,
            singer: self.singer,
            album_name: self.album_name,
            album_id: String::new(),
            duration: self.duration,
            cover_url: self.cover_url,
            qualities: self.qualities,
            audio: None,
            extra: Default::default(),
            toggle_source: None,
            file_path: self.file_path,
            file_ext: None,
        }
    }
}

/// Version of the user-data interchange document.  Keep this independent of
/// the application config version so data can migrate on its own schedule.
pub const DATA_BACKUP_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataBackup {
    pub version: u32,
    #[serde(default)]
    pub exported_at_unix: u64,
    #[serde(default)]
    pub favorites: Vec<SongInfo>,
    #[serde(default)]
    pub favorite_playlists: Vec<Playlist>,
    #[serde(default)]
    pub custom_playlists: Vec<CustomPlaylist>,
    #[serde(default)]
    pub history: Vec<HistoryEntry>,
}

/// Result of importing an external playlist file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistImportReport {
    pub playlist_id: String,
    pub playlist_name: String,
    pub imported: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, Default)]
struct ImportedEntry {
    title: String,
    artist: String,
    album: String,
    id: Option<String>,
    url: Option<String>,
    duration: Option<Duration>,
    source: Option<SourceId>,
}

impl DataBackup {
    /// Decode current and legacy documents, applying deterministic migrations.
    /// Version `0` was the short-lived unversioned export format and contained
    /// the same arrays without a wrapper version field.
    pub fn from_json(bytes: &[u8]) -> Result<Self, String> {
        let mut value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|error| format!("导入文件格式无效: {error}"))?;
        let version = value
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as u32;
        if version > DATA_BACKUP_VERSION {
            return Err(format!(
                "不支持的数据版本 {}，当前支持版本 {}",
                version, DATA_BACKUP_VERSION
            ));
        }
        if version == 0 {
            // Mark the legacy object as migrated before deserializing. Unknown
            // legacy fields are intentionally ignored for forward compatibility.
            if let Some(object) = value.as_object_mut() {
                object.insert(
                    "version".to_string(),
                    serde_json::Value::from(DATA_BACKUP_VERSION),
                );
            }
        }
        serde_json::from_value(value).map_err(|error| format!("导入文件格式无效: {error}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SavedPlayerState {
    Playing,
    Paused,
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackSession {
    pub playlist: Vec<SongInfo>,
    pub current_index: usize,
    pub position: Duration,
    pub state: SavedPlayerState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomPlaylist {
    pub id: String,
    pub name: String,
    pub songs: Vec<SongInfo>,
    pub created_at_unix_nanos: u128,
    pub updated_at_unix_nanos: u128,
}

#[derive(Debug, Clone)]
pub struct CustomPlaylistSummary {
    pub id: String,
    pub name: String,
    pub cover_url: Option<String>,
    pub song_count: u32,
}

/// voicefox 的数据目录：`~/.config/voicefox/data`。
///
/// 收藏、历史、下载记录等持久化文件都落在这里；集中成一个函数，
/// 避免各模块各写一份 `dirs::config_dir()` 拼接逻辑。
pub fn default_data_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("voicefox")
        .join("data")
}

pub struct Storage {
    data_dir: PathBuf,
    favorites: RwLock<Vec<SongInfo>>,
    favorite_playlists: RwLock<Vec<Playlist>>,
    custom_playlists: RwLock<Vec<CustomPlaylist>>,
    history: RwLock<Vec<HistoryEntry>>,
    /// 自定义歌单更新串行锁：保证"读快照 → 更新 → 落盘 → 提交内存"整体
    /// 串行，同时不让读锁（页面只读快照）被磁盘 I/O 阻塞。
    custom_playlists_update: Mutex<()>,
    /// 数据代次：任何收藏/歌单/历史写入都会递增，供页面缓存判断是否重建。
    generation: AtomicU64,
}

impl Storage {
    pub fn new() -> Self {
        let dir = default_data_dir();
        fs::create_dir_all(&dir).ok();
        let favorites = Self::load_file(&dir.join("favorites.json"));
        let favorite_playlists = Self::load_file(&dir.join("favorite_playlists.json"));
        let custom_playlists = Self::load_file(&dir.join("custom_playlists.json"));
        let history = Self::load_history_file(&dir.join("history.json"));
        let storage = Self {
            data_dir: dir,
            favorites: RwLock::new(favorites),
            favorite_playlists: RwLock::new(favorite_playlists),
            custom_playlists: RwLock::new(custom_playlists),
            history: RwLock::new(history),
            custom_playlists_update: std::sync::Mutex::new(()),
            generation: AtomicU64::new(0),
        };
        storage.create_startup_backup();
        storage
    }

    /// 数据代次，收藏或历史发生变化时递增。
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    fn bump_generation(&self) {
        self.generation.fetch_add(1, Ordering::Release);
    }

    /// Export all durable user library data in one versioned document.
    pub fn export_data(&self, path: &Path) -> Result<(), String> {
        let backup = self.snapshot();
        let json = serde_json::to_vec_pretty(&backup).map_err(|error| error.to_string())?;
        save_atomic(path, &json)
    }

    /// One-click export location inside the Voicefox data directory.
    pub fn export_default(&self) -> Result<PathBuf, String> {
        let path = self.data_dir.join("voicefox-export.json");
        self.export_data(&path)?;
        Ok(path)
    }

    /// Import the one-click export file, creating the normal automatic backup first.
    pub fn import_default(&self) -> Result<PathBuf, String> {
        let path = self.data_dir.join("voicefox-export.json");
        self.import_data(&path)
    }

    /// Replace user library data after validation, preserving a timestamped snapshot first.
    pub fn import_data(&self, path: &Path) -> Result<PathBuf, String> {
        let json = fs::read(path).map_err(|error| format!("读取导入文件失败: {error}"))?;
        let backup = DataBackup::from_json(&json)?;

        let automatic_backup = self.automatic_backup_path();
        self.export_data(&automatic_backup)
            .map_err(|error| format!("创建导入前备份失败: {error}"))?;

        self.persist_backup(&backup)?;
        *self.favorites.write().unwrap_or_else(|e| e.into_inner()) = backup.favorites;
        *self
            .favorite_playlists
            .write()
            .unwrap_or_else(|e| e.into_inner()) = backup.favorite_playlists;
        *self
            .custom_playlists
            .write()
            .unwrap_or_else(|e| e.into_inner()) = backup.custom_playlists;
        *self.history.write().unwrap_or_else(|e| e.into_inner()) = backup.history;
        Ok(automatic_backup)
    }

    /// Import an M3U/M3U8, LX Music JSON, or NetEase playlist export into a
    /// new custom playlist. Local entries reuse the local source's lofty
    /// metadata reader; online entries retain their source/id and therefore
    /// continue through the normal SourceManager resolution path.
    pub fn import_external_playlist(&self, path: &Path) -> Result<PlaylistImportReport, String> {
        let bytes = fs::read(path).map_err(|error| format!("读取歌单文件失败: {error}"))?;
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let entries =
            if matches!(extension.as_str(), "m3u" | "m3u8") || bytes.starts_with(b"#EXTM3U") {
                parse_m3u(&String::from_utf8_lossy(&bytes))
            } else {
                parse_playlist_json(&bytes)?
            };
        if entries.is_empty() {
            return Err("歌单文件中没有可导入的歌曲".to_string());
        }

        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("导入歌单");
        let playlist_name = self.unique_import_name(stem);
        let playlist = self.create_custom_playlist(&playlist_name)?;
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        // 先解析并去重全部条目，再一次性批量写入歌单，避免每首歌触发一次
        // 全量序列化与磁盘写入（N 首歌会退化为 O(N^2) 的写放大）。
        let mut songs_to_add = Vec::new();
        let mut skipped = 0;
        let mut identities = HashSet::new();
        for entry in entries {
            let Some(song) = imported_entry_song(&entry, parent) else {
                skipped += 1;
                continue;
            };
            let identity = format!("{}\u{1f}{}", song.source.as_str(), song.id);
            if !identities.insert(identity) {
                skipped += 1;
                continue;
            }
            songs_to_add.push(song);
        }
        let imported = match self.add_songs_to_custom_playlist(&playlist.id, &songs_to_add) {
            Ok((added, duplicate_skipped)) => {
                skipped += duplicate_skipped;
                added
            }
            Err(error) => {
                let _ = self.delete_custom_playlist(&playlist.id);
                return Err(format!("写入导入歌单失败: {error}"));
            }
        };
        if imported == 0 {
            let _ = self.delete_custom_playlist(&playlist.id);
            return Err("歌单中没有可识别的歌曲".to_string());
        }
        Ok(PlaylistImportReport {
            playlist_id: playlist.id,
            playlist_name,
            imported,
            skipped,
        })
    }

    fn unique_import_name(&self, stem: &str) -> String {
        let base = stem.trim();
        let existing = self.custom_playlist_summaries();
        if !existing
            .iter()
            .any(|playlist| playlist.name.eq_ignore_ascii_case(base))
        {
            return base.to_string();
        }
        for index in 2..=999 {
            let candidate = format!("{base} ({index})");
            if !existing
                .iter()
                .any(|playlist| playlist.name.eq_ignore_ascii_case(&candidate))
            {
                return candidate;
            }
        }
        format!("{base} ({})", unix_nanos())
    }

    fn snapshot(&self) -> DataBackup {
        DataBackup {
            version: DATA_BACKUP_VERSION,
            exported_at_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            favorites: self
                .favorites
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            favorite_playlists: self
                .favorite_playlists
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            custom_playlists: self
                .custom_playlists
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            history: self
                .history
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        }
    }

    fn automatic_backup_path(&self) -> PathBuf {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        self.data_dir
            .join("backups")
            .join(format!("before-import-{timestamp}.json"))
    }

    fn create_startup_backup(&self) {
        let has_data = !self
            .favorites
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
            || !self
                .favorite_playlists
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty()
            || !self
                .custom_playlists
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty()
            || !self
                .history
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty();
        if !has_data {
            return;
        }
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = self
            .data_dir
            .join("backups")
            .join(format!("auto-{timestamp}.json"));
        if let Err(error) = self.export_data(&path) {
            tracing::warn!("创建自动数据备份失败: {error}");
            return;
        }

        let Ok(entries) = fs::read_dir(self.data_dir.join("backups")) else {
            return;
        };
        let mut backups = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("auto-"))
            .filter_map(|entry| {
                let modified = entry.metadata().ok()?.modified().ok()?;
                Some((modified, entry.path()))
            })
            .collect::<Vec<_>>();
        backups.sort_by_key(|(modified, _)| *modified);
        for (_, path) in backups.into_iter().rev().skip(10) {
            let _ = fs::remove_file(path);
        }
    }

    fn persist_backup(&self, backup: &DataBackup) -> Result<(), String> {
        // 先把四个文件全部序列化并写成临时文件，再统一换名，
        // 避免中途失败（如磁盘满）留下只导入了一半的数据目录。
        let staged: Vec<(&str, Vec<u8>)> = [
            (
                "favorites.json",
                serde_json::to_vec_pretty(&backup.favorites),
            ),
            (
                "favorite_playlists.json",
                serde_json::to_vec_pretty(&backup.favorite_playlists),
            ),
            (
                "custom_playlists.json",
                serde_json::to_vec_pretty(&backup.custom_playlists),
            ),
            ("history.json", serde_json::to_vec_pretty(&backup.history)),
        ]
        .into_iter()
        .map(|(name, value)| Ok((name, value.map_err(|error| error.to_string())?)))
        .collect::<Result<_, String>>()?;

        let mut temp_files = Vec::new();
        for (name, json) in &staged {
            let target = self.data_dir.join(name);
            let temp = stage_atomic(&target, json)?;
            temp_files.push((temp, target));
        }
        // 换名阶段失败的概率极低（同目录 rename）；失败时清理未提交的临时文件并报错
        for (temp, target) in temp_files {
            if let Err(error) = commit_staged(&temp, &target) {
                let _ = fs::remove_file(&temp);
                return Err(error);
            }
        }
        self.bump_generation();
        Ok(())
    }

    // ── 收藏 ──────────────────────────────────────────

    pub fn add_favorite(&self, song: &SongInfo) -> bool {
        let mut favs = self.favorites.write().unwrap_or_else(|e| e.into_inner());
        if favs.iter().any(|favorite| songs_equivalent(favorite, song)) {
            return false;
        }
        favs.push(song.clone());
        self.save_favorites(&favs);
        true
    }

    pub fn remove_favorite(&self, song: &SongInfo) -> bool {
        let mut favs = self.favorites.write().unwrap_or_else(|e| e.into_inner());
        let old_len = favs.len();
        favs.retain(|favorite| !songs_equivalent(favorite, song));
        if favs.len() != old_len {
            self.save_favorites(&favs);
            return true;
        }
        false
    }

    pub fn is_favorite(&self, song: &SongInfo) -> bool {
        self.favorites
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|favorite| songs_equivalent(favorite, song))
    }

    // ── 歌单收藏 ──────────────────────────────────────

    pub fn add_favorite_playlist(&self, playlist: &Playlist) -> bool {
        let mut favorites = self
            .favorite_playlists
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if favorites
            .iter()
            .any(|favorite| favorite.id == playlist.id && favorite.source == playlist.source)
        {
            return false;
        }
        favorites.push(playlist.clone());
        self.save_favorite_playlists(&favorites);
        true
    }

    pub fn remove_favorite_playlist(&self, playlist: &Playlist) -> bool {
        let mut favorites = self
            .favorite_playlists
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let old_len = favorites.len();
        favorites
            .retain(|favorite| favorite.id != playlist.id || favorite.source != playlist.source);
        if favorites.len() == old_len {
            return false;
        }
        self.save_favorite_playlists(&favorites);
        true
    }

    pub fn is_favorite_playlist(&self, playlist: &Playlist) -> bool {
        self.favorite_playlists
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|favorite| favorite.id == playlist.id && favorite.source == playlist.source)
    }

    pub fn load_favorite_playlists(&self) -> Vec<Playlist> {
        self.favorite_playlists
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    // ── 自定义歌单 ────────────────────────────────────

    pub fn custom_playlist_summaries(&self) -> Vec<CustomPlaylistSummary> {
        self.custom_playlists
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|playlist| CustomPlaylistSummary {
                id: playlist.id.clone(),
                name: playlist.name.clone(),
                cover_url: playlist
                    .songs
                    .iter()
                    .find_map(|song| song.cover_url.clone()),
                song_count: u32::try_from(playlist.songs.len()).unwrap_or(u32::MAX),
            })
            .collect()
    }

    pub fn custom_playlist_choices(&self) -> Vec<(String, String)> {
        self.custom_playlists
            .read()
            .unwrap()
            .iter()
            .map(|playlist| (playlist.id.clone(), playlist.name.clone()))
            .collect()
    }

    pub fn custom_playlist(&self, playlist_id: &str) -> Option<CustomPlaylist> {
        self.custom_playlists
            .read()
            .unwrap()
            .iter()
            .find(|playlist| playlist.id == playlist_id)
            .cloned()
    }

    pub fn create_custom_playlist(&self, name: &str) -> Result<CustomPlaylist, String> {
        let name = validate_custom_playlist_name(name)?;
        self.update_custom_playlists(|playlists| {
            ensure_unique_custom_playlist_name(playlists, &name, None)?;
            let now = unix_nanos();
            let playlist = CustomPlaylist {
                id: format!("custom-{now}-{}", std::process::id()),
                name,
                songs: Vec::new(),
                created_at_unix_nanos: now,
                updated_at_unix_nanos: now,
            };
            playlists.push(playlist.clone());
            Ok(playlist)
        })
    }

    pub fn rename_custom_playlist(&self, playlist_id: &str, name: &str) -> Result<(), String> {
        let name = validate_custom_playlist_name(name)?;
        self.update_custom_playlists(|playlists| {
            ensure_unique_custom_playlist_name(playlists, &name, Some(playlist_id))?;
            let playlist = playlists
                .iter_mut()
                .find(|playlist| playlist.id == playlist_id)
                .ok_or_else(|| "自定义歌单不存在".to_string())?;
            playlist.name = name;
            playlist.updated_at_unix_nanos = unix_nanos();
            Ok(())
        })
    }

    pub fn delete_custom_playlist(&self, playlist_id: &str) -> Result<bool, String> {
        self.update_custom_playlists(|playlists| {
            let old_len = playlists.len();
            playlists.retain(|playlist| playlist.id != playlist_id);
            Ok(playlists.len() != old_len)
        })
    }

    pub fn add_song_to_custom_playlist(
        &self,
        playlist_id: &str,
        song: &SongInfo,
    ) -> Result<bool, String> {
        self.update_custom_playlists(|playlists| {
            let playlist = playlists
                .iter_mut()
                .find(|playlist| playlist.id == playlist_id)
                .ok_or_else(|| "自定义歌单不存在".to_string())?;
            if playlist
                .songs
                .iter()
                .any(|item| same_song_identity(item, song))
            {
                return Ok(false);
            }
            playlist.songs.push(song.clone());
            playlist.updated_at_unix_nanos = unix_nanos();
            Ok(true)
        })
    }

    /// 批量把歌曲加入自定义歌单，整个批次只更新一次内存并写盘一次。
    ///
    /// 返回 `(新增数量, 因与歌单内已有歌曲重复而跳过的数量)`。
    /// 单曲调用仍走 `add_song_to_custom_playlist`，大歌单导入使用本方法，
    /// 避免 N 首歌曲触发 N 次全量序列化和磁盘写入。
    pub fn add_songs_to_custom_playlist(
        &self,
        playlist_id: &str,
        songs: &[SongInfo],
    ) -> Result<(usize, usize), String> {
        self.update_custom_playlists(|playlists| {
            let playlist = playlists
                .iter_mut()
                .find(|playlist| playlist.id == playlist_id)
                .ok_or_else(|| "自定义歌单不存在".to_string())?;
            let mut added = 0;
            let mut skipped = 0;
            for song in songs {
                if playlist
                    .songs
                    .iter()
                    .any(|item| same_song_identity(item, song))
                {
                    skipped += 1;
                    continue;
                }
                playlist.songs.push(song.clone());
                added += 1;
            }
            if added > 0 {
                playlist.updated_at_unix_nanos = unix_nanos();
            }
            Ok((added, skipped))
        })
    }

    pub fn remove_song_from_custom_playlist(
        &self,
        playlist_id: &str,
        song: &SongInfo,
    ) -> Result<bool, String> {
        self.update_custom_playlists(|playlists| {
            let playlist = playlists
                .iter_mut()
                .find(|playlist| playlist.id == playlist_id)
                .ok_or_else(|| "自定义歌单不存在".to_string())?;
            let old_len = playlist.songs.len();
            playlist
                .songs
                .retain(|item| !same_song_identity(item, song));
            if playlist.songs.len() == old_len {
                return Ok(false);
            }
            playlist.updated_at_unix_nanos = unix_nanos();
            Ok(true)
        })
    }

    pub fn remove_local_path_from_custom_playlists(&self, path: &Path) -> Result<usize, String> {
        self.update_custom_playlists(|playlists| {
            let mut removed = 0;
            for playlist in playlists {
                let old_len = playlist.songs.len();
                playlist
                    .songs
                    .retain(|song| !local_song_matches_path(song, path));
                let removed_from_playlist = old_len.saturating_sub(playlist.songs.len());
                if removed_from_playlist > 0 {
                    removed += removed_from_playlist;
                    playlist.updated_at_unix_nanos = unix_nanos();
                }
            }
            Ok(removed)
        })
    }

    fn update_custom_playlists<T>(
        &self,
        update: impl FnOnce(&mut Vec<CustomPlaylist>) -> Result<T, String>,
    ) -> Result<T, String> {
        // 串行化"更新 → 落盘 → 提交内存"整体过程：磁盘 I/O 期间只持有
        // 这把小锁，读侧（custom_playlist_summaries 等每帧调用的接口）
        // 不会再被写锁 + 磁盘写阻塞住。
        let _guard = self
            .custom_playlists_update
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (result, updated) = {
            let mut updated = self
                .custom_playlists
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let result = update(&mut updated)?;
            (result, updated)
        };
        let json = serde_json::to_vec_pretty(&updated).map_err(|error| error.to_string())?;
        save_atomic(&self.data_dir.join("custom_playlists.json"), &json)?;
        *self
            .custom_playlists
            .write()
            .unwrap_or_else(|e| e.into_inner()) = updated;
        // 页面（如歌单页的 sync_saved_playlists）按 generation 判断是否重建
        self.bump_generation();
        Ok(result)
    }

    // ── 播放历史 ──────────────────────────────────────

    pub fn add_history(&self, song: &SongInfo, limit: usize) {
        let mut history = self.history.write().unwrap_or_else(|e| e.into_inner());
        history.retain(|s| !(s.id == song.id && s.source == song.source));
        history.insert(0, HistoryEntry::from_song(song));
        history.truncate(limit.max(1));
        self.save_history(&history);
    }

    pub fn remove_history(&self, song: &SongInfo) -> bool {
        let mut history = self.history.write().unwrap_or_else(|e| e.into_inner());
        let old_len = history.len();
        history.retain(|item| !(item.id == song.id && item.source == song.source));
        if history.len() == old_len {
            return false;
        }
        self.save_history(&history);
        true
    }

    pub fn clear_history(&self) -> bool {
        let mut history = self.history.write().unwrap_or_else(|e| e.into_inner());
        if history.is_empty() {
            return false;
        }
        history.clear();
        self.save_history(&history);
        true
    }

    pub fn trim_history(&self, limit: usize) -> bool {
        let mut history = self.history.write().unwrap_or_else(|e| e.into_inner());
        let old_len = history.len();
        history.truncate(limit.max(1));
        if history.len() == old_len {
            return false;
        }
        self.save_history(&history);
        true
    }

    // ── 内部序列化 ────────────────────────────────────

    pub fn load_favorites(&self) -> Vec<SongInfo> {
        self.favorites
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn save_favorites(&self, favs: &[SongInfo]) {
        self.save_file("favorites.json", favs);
    }

    fn save_favorite_playlists(&self, playlists: &[Playlist]) {
        self.save_file("favorite_playlists.json", playlists);
    }

    pub fn load_history(&self) -> Vec<SongInfo> {
        self.history
            .read()
            .unwrap()
            .iter()
            .cloned()
            .map(HistoryEntry::into_song)
            .collect()
    }

    // ── 播放会话 ──────────────────────────────────────

    pub fn load_playback_session(&self) -> Option<PlaybackSession> {
        let path = self.data_dir.join("playback_state.json");
        let json = fs::read_to_string(path).ok()?;
        let mut session = serde_json::from_str::<PlaybackSession>(&json).ok()?;
        if session.playlist.is_empty() {
            return None;
        }
        session.current_index = session
            .current_index
            .min(session.playlist.len().saturating_sub(1));
        Some(session)
    }

    /// 保留给测试与外部调用；生产持久化走
    /// [`save_playback_session_borrowed`](Self::save_playback_session_borrowed)。
    #[allow(dead_code)]
    pub fn save_playback_session(&self, session: &PlaybackSession) -> Result<(), String> {
        let path = self.data_dir.join("playback_state.json");
        let json = serde_json::to_vec_pretty(session).map_err(|error| error.to_string())?;
        save_atomic(&path, &json)
    }

    /// 零拷贝保存播放会话：队列以 `&[SongInfo]` 借用序列化，
    /// 不再为 5 秒一次的定时持久化深拷贝整张播放列表。
    pub fn save_playback_session_borrowed(
        &self,
        playlist: &[SongInfo],
        current_index: usize,
        position: Duration,
        state: SavedPlayerState,
    ) -> Result<(), String> {
        #[derive(serde::Serialize)]
        struct PlaybackSessionRef<'a> {
            playlist: &'a [SongInfo],
            current_index: usize,
            position: Duration,
            state: SavedPlayerState,
        }
        let session = PlaybackSessionRef {
            playlist,
            current_index,
            position,
            state,
        };
        let path = self.data_dir.join("playback_state.json");
        let json = serde_json::to_vec_pretty(&session).map_err(|error| error.to_string())?;
        save_atomic_relaxed(&path, &json)
    }

    pub fn clear_playback_session(&self) -> Result<(), String> {
        let path = self.data_dir.join("playback_state.json");
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    /// 把解析失败的数据文件改名保留现场，避免随后一次保存用空数据覆盖原文件。
    fn quarantine_corrupt_file(path: &Path, reason: &str) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "data".to_string());
        let backup = path.with_file_name(format!("{file_name}.corrupt-{timestamp}"));
        match fs::rename(path, &backup) {
            Ok(()) => tracing::error!(
                "数据文件 {} 解析失败（{reason}），已重命名为 {} 保留现场；本次以空数据启动",
                path.display(),
                backup.display()
            ),
            Err(error) => tracing::error!(
                "数据文件 {} 解析失败（{reason}）且改名保留失败: {error}",
                path.display()
            ),
        }
    }

    fn load_file<T: DeserializeOwned>(path: &std::path::Path) -> Vec<T> {
        if path.exists() {
            match fs::read_to_string(path) {
                Ok(json) => match serde_json::from_str(&json) {
                    Ok(data) => data,
                    Err(error) => {
                        Self::quarantine_corrupt_file(path, &error.to_string());
                        vec![]
                    }
                },
                Err(error) => {
                    tracing::error!("读取数据文件 {} 失败: {error}", path.display());
                    vec![]
                }
            }
        } else {
            vec![]
        }
    }

    fn save_history(&self, history: &[HistoryEntry]) {
        self.save_file("history.json", history);
    }

    fn load_history_file(path: &Path) -> Vec<HistoryEntry> {
        if path.exists() {
            match fs::read_to_string(path) {
                Ok(json) => match serde_json::from_str(&json) {
                    Ok(data) => data,
                    Err(error) => {
                        Self::quarantine_corrupt_file(path, &error.to_string());
                        vec![]
                    }
                },
                Err(error) => {
                    tracing::error!("读取数据文件 {} 失败: {error}", path.display());
                    vec![]
                }
            }
        } else {
            vec![]
        }
    }

    fn save_file<T: Serialize + ?Sized>(&self, file_name: &str, value: &T) {
        let path = self.data_dir.join(file_name);
        // 序列化或写盘失败都不递增代次：页面缓存不应基于未写盘的数据重建
        let saved = match serde_json::to_vec_pretty(value) {
            Ok(json) => match save_atomic(&path, &json) {
                Ok(()) => true,
                Err(error) => {
                    tracing::warn!("保存数据文件 {} 失败: {}", path.display(), error);
                    false
                }
            },
            Err(error) => {
                tracing::warn!("序列化数据文件 {} 失败: {}", path.display(), error);
                false
            }
        };
        if saved {
            self.bump_generation();
        }
    }
}

pub(crate) fn save_atomic(path: &std::path::Path, content: &[u8]) -> Result<(), String> {
    save_atomic_impl(path, content, true)
}

/// 原子写但不做 fsync：只保证不出现半截文件，不保证断电后内容还在。
/// 适用于可再生的缓存型数据（如播放会话），避免 5 秒一次的持久化
/// 在 TUI 线程上产生磁盘刷写停顿。
fn save_atomic_relaxed(path: &std::path::Path, content: &[u8]) -> Result<(), String> {
    save_atomic_impl(path, content, false)
}

/// 只写临时文件不换名，由 [`commit_staged`] 完成最终落盘。
fn stage_atomic(path: &std::path::Path, content: &[u8]) -> Result<std::path::PathBuf, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let temp_path = path.with_extension(format!(
        "json.tmp-{}-{}",
        std::process::id(),
        STORAGE_TEMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut file = fs::File::create(&temp_path).map_err(|error| error.to_string())?;
    file.write_all(content).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    Ok(temp_path)
}

fn commit_staged(temp_path: &std::path::Path, target: &std::path::Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        let old_path = target.with_extension(format!("json.old-{}", std::process::id()));
        if target.exists() {
            fs::rename(target, &old_path).map_err(|error| error.to_string())?;
        }
        match fs::rename(temp_path, target) {
            Ok(()) => {
                let _ = fs::remove_file(&old_path);
                Ok(())
            }
            Err(error) => {
                let _ = fs::rename(&old_path, target);
                Err(error.to_string())
            }
        }
    }
    #[cfg(not(windows))]
    {
        fs::rename(temp_path, target).map_err(|error| error.to_string())
    }
}

fn save_atomic_impl(path: &std::path::Path, content: &[u8], durable: bool) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let temp_path = path.with_extension(format!(
        "json.tmp-{}-{}",
        std::process::id(),
        STORAGE_TEMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = fs::File::create(&temp_path).map_err(|error| error.to_string())?;
        file.write_all(content).map_err(|error| error.to_string())?;
        // 数据落盘后再换名，避免断电时目录项先于数据持久化，留下空文件。
        if durable {
            file.sync_all().map_err(|error| error.to_string())?;
        }
        drop(file);

        #[cfg(windows)]
        {
            // Windows 不支持覆盖式 rename：先把旧文件挪走（而非删除），
            // 换名失败时把它挪回来，保证任何时刻目标文件都存在。
            let old_path = path.with_extension(format!("json.old-{}", std::process::id()));
            if path.exists() {
                fs::rename(path, &old_path).map_err(|error| error.to_string())?;
            }
            match fs::rename(&temp_path, path) {
                Ok(()) => {
                    let _ = fs::remove_file(&old_path);
                }
                Err(error) => {
                    let _ = fs::rename(&old_path, path);
                    return Err(error.to_string());
                }
            }
        }

        #[cfg(not(windows))]
        fs::rename(&temp_path, path).map_err(|error| error.to_string())?;

        #[cfg(unix)]
        if durable
            && let Some(parent) = path.parent()
            && let Ok(dir) = fs::File::open(parent)
        {
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp_path);
    }
    result
}

static STORAGE_TEMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn parse_m3u(content: &str) -> Vec<ImportedEntry> {
    let mut entries = Vec::new();
    let mut pending = None;
    for raw_line in content.lines() {
        let line = raw_line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with('#') && !line.starts_with("#EXTINF:") {
            continue;
        }
        if let Some(info) = line.strip_prefix("#EXTINF:") {
            let (duration, label) = info.split_once(',').unwrap_or((info, ""));
            let duration = duration
                .trim()
                .parse::<i64>()
                .ok()
                .filter(|seconds| *seconds >= 0)
                .map(|seconds| Duration::from_secs(seconds as u64));
            let (artist, title) = split_m3u_label(label);
            pending = Some(ImportedEntry {
                title,
                artist,
                duration,
                ..Default::default()
            });
            continue;
        }

        let mut entry = pending.take().unwrap_or_default();
        if entry.title.is_empty() {
            entry.title = Path::new(line)
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or(line)
                .to_string();
        }
        entry.url = Some(line.to_string());
        entries.push(entry);
    }
    entries
}

fn split_m3u_label(label: &str) -> (String, String) {
    let label = label.trim();
    if let Some((artist, title)) = label.split_once(" - ") {
        (artist.trim().to_string(), title.trim().to_string())
    } else {
        (String::new(), label.to_string())
    }
}

fn parse_playlist_json(bytes: &[u8]) -> Result<Vec<ImportedEntry>, String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| format!("歌单 JSON 格式无效: {error}"))?;
    let mut entries = Vec::new();
    collect_json_entries(&value, &mut entries);
    Ok(entries)
}

fn collect_json_entries(value: &serde_json::Value, entries: &mut Vec<ImportedEntry>) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                collect_json_entries(value, entries);
            }
        }
        serde_json::Value::Object(object) => {
            if let Some(entry) = imported_entry_from_json(object) {
                entries.push(entry);
                // A song object can contain nested artist/album objects. Do
                // not descend into those once the parent was recognized.
                return;
            }
            for value in object.values() {
                collect_json_entries(value, entries);
            }
        }
        _ => {}
    }
}

fn imported_entry_from_json(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Option<ImportedEntry> {
    let title = json_string(
        object,
        &["name", "songName", "song_name", "title", "trackName"],
    )?;
    let artist = json_artist(object);
    let id = json_string(
        object,
        &["id", "songId", "song_id", "songmid", "mid", "bvid", "hash"],
    );
    let url = json_string(
        object,
        &["url", "path", "file", "file_path", "location", "playUrl"],
    );
    if artist.is_empty() && id.is_none() && url.is_none() {
        return None;
    }
    let album = json_string(object, &["album", "albumName", "album_name"])
        .or_else(|| {
            object
                .get("al")
                .and_then(|value| value.get("name"))
                .and_then(value_to_string)
        })
        .unwrap_or_default();
    let duration = json_number(object, &["duration", "duration_ms", "dt"]).map(|value| {
        if value > 10_000.0 {
            Duration::from_millis(value as u64)
        } else {
            Duration::from_secs(value.max(0.0) as u64)
        }
    });
    let source = json_string(object, &["source", "platform", "sourceId", "source_id"])
        .as_deref()
        .and_then(parse_source_id);
    Some(ImportedEntry {
        title,
        artist,
        album,
        id,
        url,
        duration,
        source,
    })
}

fn json_string(
    object: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Option<String> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(value_to_string))
        .filter(|value| !value.trim().is_empty())
}

fn value_to_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn json_artist(object: &serde_json::Map<String, serde_json::Value>) -> String {
    if let Some(value) = json_string(object, &["artist", "singer", "artists", "author"]) {
        return value;
    }
    for key in ["ar", "artists", "artist"] {
        if let Some(values) = object.get(key).and_then(serde_json::Value::as_array) {
            let names = values
                .iter()
                .filter_map(|value| {
                    value
                        .get("name")
                        .and_then(value_to_string)
                        .or_else(|| value_to_string(value))
                })
                .collect::<Vec<_>>();
            if !names.is_empty() {
                return names.join("、");
            }
        }
    }
    String::new()
}

fn json_number(object: &serde_json::Map<String, serde_json::Value>, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| {
        object.get(*key).and_then(|value| match value {
            serde_json::Value::Number(value) => value.as_f64(),
            serde_json::Value::String(value) => value.parse::<f64>().ok(),
            _ => None,
        })
    })
}

fn parse_source_id(value: &str) -> Option<SourceId> {
    match value.trim().to_ascii_lowercase().as_str() {
        "kw" | "kuwo" => Some(SourceId::Kw),
        "kg" | "kugou" => Some(SourceId::Kg),
        "tx" | "qq" | "tencent" => Some(SourceId::Tx),
        "wy" | "163" | "netease" | "neteasecloud" => Some(SourceId::Wy),
        "mg" | "migu" => Some(SourceId::Mg),
        "bili" | "bilibili" => Some(SourceId::Bili),
        "local" => Some(SourceId::Local),
        _ => None,
    }
}

fn imported_entry_song(entry: &ImportedEntry, parent: &Path) -> Option<SongInfo> {
    let raw_url = entry.url.as_deref().or(entry.id.as_deref())?;
    let local_path = if let Some(url) = entry.url.as_deref() {
        if url.starts_with("file://") {
            Some(PathBuf::from(url.trim_start_matches("file://")))
        } else {
            let path = PathBuf::from(url);
            (!url.contains("://")).then(|| {
                if path.is_absolute() {
                    path
                } else {
                    parent.join(path)
                }
            })
        }
    } else {
        None
    };

    let is_local_reference = local_path.is_some() && !raw_url.contains("://");
    if let Some(path) = local_path.filter(|path| path.exists())
        && let Ok(mut song) = lx_source::local::metadata::read_metadata(&path)
    {
        let canonical = path.canonicalize().unwrap_or(path);
        song.file_path = Some(canonical);
        return Some(song);
    }

    let source = entry.source.unwrap_or_else(|| {
        if raw_url.contains("music.163.com") {
            SourceId::Wy
        } else if raw_url.contains("://") {
            // A direct stream URL is handled by LocalSource's imported-URL
            // path, while an id-only entry defaults to NetEase below.
            SourceId::Local
        } else if is_local_reference {
            SourceId::Local
        } else {
            SourceId::Wy
        }
    });
    let id = entry.id.clone().unwrap_or_else(|| raw_url.to_string());
    let mut song = SongInfo::new(
        id,
        source,
        entry.title.trim().to_string(),
        if entry.artist.trim().is_empty() {
            "未知艺术家".to_string()
        } else {
            entry.artist.trim().to_string()
        },
    );
    song.album_name = entry.album.clone();
    song.duration = entry.duration.unwrap_or_default();
    if raw_url.contains("://") {
        song.extra
            .insert("import_url".to_string(), raw_url.to_string());
    }
    Some(song)
}

impl Default for Storage {
    fn default() -> Self {
        Self::new()
    }
}

fn songs_equivalent(left: &SongInfo, right: &SongInfo) -> bool {
    if left.source == right.source && left.id == right.id {
        return true;
    }
    let left_name = normalize_text(&left.name);
    let right_name = normalize_text(&right.name);
    let left_singer = normalize_singer(&left.singer);
    let right_singer = normalize_singer(&right.singer);
    !left_name.is_empty()
        && left_name == right_name
        && !left_singer.is_empty()
        && left_singer == right_singer
}

fn normalize_text(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect()
}

fn normalize_singer(value: &str) -> String {
    let mut singers = value
        .split(['、', ',', '&', '/', ';', '，'])
        .map(normalize_text)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    singers.sort();
    singers.dedup();
    singers.join("|")
}

fn unix_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn validate_custom_playlist_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("歌单名称不能为空".to_string());
    }
    if name.chars().count() > 80 {
        return Err("歌单名称不能超过 80 个字符".to_string());
    }
    Ok(name.to_string())
}

fn ensure_unique_custom_playlist_name(
    playlists: &[CustomPlaylist],
    name: &str,
    except_id: Option<&str>,
) -> Result<(), String> {
    if playlists.iter().any(|playlist| {
        Some(playlist.id.as_str()) != except_id && playlist.name.eq_ignore_ascii_case(name)
    }) {
        return Err("已经存在同名自定义歌单".to_string());
    }
    Ok(())
}

pub(crate) fn same_song_identity(left: &SongInfo, right: &SongInfo) -> bool {
    if left.source == right.source && !left.id.is_empty() && left.id == right.id {
        return true;
    }
    left.source == lx_core::model::source::SourceId::Local
        && right.source == lx_core::model::source::SourceId::Local
        && left.file_path.is_some()
        && left.file_path == right.file_path
}

pub(crate) fn local_song_matches_path(song: &SongInfo, path: &Path) -> bool {
    song.source == lx_core::model::source::SourceId::Local
        && (song.file_path.as_deref() == Some(path) || song.id == path.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::{
        CustomPlaylist, DataBackup, PlaybackSession, SavedPlayerState, Storage, same_song_identity,
        songs_equivalent, unix_nanos,
    };
    use lx_core::model::song::SongInfo;
    use lx_core::model::source::SourceId;
    use std::sync::RwLock;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;

    fn song(id: &str, source: SourceId, name: &str, singer: &str) -> SongInfo {
        SongInfo::new(id.to_string(), source, name.to_string(), singer.to_string())
    }

    #[test]
    fn same_song_from_another_source_matches_favorite() {
        let favorite = song("1", SourceId::Kw, "晴天 (Live)", "周杰伦、五月天");
        let toggled = song("2", SourceId::Kg, "晴天 Live", "五月天 & 周杰伦");

        assert!(songs_equivalent(&favorite, &toggled));
    }

    #[test]
    fn different_singers_do_not_match() {
        let left = song("1", SourceId::Kw, "后来", "刘若英");
        let right = song("2", SourceId::Wy, "后来", "其他歌手");

        assert!(!songs_equivalent(&left, &right));
    }

    #[test]
    fn empty_metadata_does_not_match_across_sources() {
        let left = song("1", SourceId::Kw, "", "");
        let right = song("2", SourceId::Wy, "", "");

        assert!(!songs_equivalent(&left, &right));
    }

    #[test]
    fn missing_singers_do_not_match_across_sources() {
        let left = song("1", SourceId::Kw, "纯音乐", "");
        let right = song("2", SourceId::Wy, "纯音乐", "");

        assert!(!songs_equivalent(&left, &right));
    }

    #[test]
    fn history_limit_delete_and_clear_are_persisted_in_memory() {
        let data_dir = std::env::temp_dir().join(format!(
            "voicefox-history-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&data_dir).unwrap();
        let storage = Storage {
            data_dir: data_dir.clone(),
            favorites: RwLock::new(Vec::new()),
            favorite_playlists: RwLock::new(Vec::new()),
            custom_playlists: RwLock::new(Vec::new()),
            history: RwLock::new(Vec::new()),
            custom_playlists_update: std::sync::Mutex::new(()),
            generation: AtomicU64::new(0),
        };
        let first = song("1", SourceId::Kw, "One", "Artist");
        let second = song("2", SourceId::Kg, "Two", "Artist");
        let third = song("3", SourceId::Wy, "Three", "Artist");

        storage.add_history(&first, 2);
        storage.add_history(&second, 2);
        storage.add_history(&third, 2);
        assert_eq!(
            storage
                .load_history()
                .iter()
                .map(|song| song.id.as_str())
                .collect::<Vec<_>>(),
            vec!["3", "2"]
        );
        assert!(storage.remove_history(&second));
        assert_eq!(storage.load_history().len(), 1);
        assert!(storage.clear_history());
        assert!(storage.load_history().is_empty());

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn playback_session_round_trips_and_clamps_the_index() {
        let data_dir = std::env::temp_dir().join(format!(
            "voicefox-playback-state-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&data_dir).unwrap();
        let storage = Storage {
            data_dir: data_dir.clone(),
            favorites: RwLock::new(Vec::new()),
            favorite_playlists: RwLock::new(Vec::new()),
            custom_playlists: RwLock::new(Vec::new()),
            history: RwLock::new(Vec::new()),
            custom_playlists_update: std::sync::Mutex::new(()),
            generation: AtomicU64::new(0),
        };
        let session = PlaybackSession {
            playlist: vec![song("1", SourceId::Bili, "第一 P", "UP主")],
            current_index: 8,
            position: Duration::from_secs(37),
            state: SavedPlayerState::Paused,
        };

        storage.save_playback_session(&session).unwrap();
        let restored = storage.load_playback_session().unwrap();

        assert_eq!(restored.current_index, 0);
        assert_eq!(restored.position, Duration::from_secs(37));
        assert_eq!(restored.state, SavedPlayerState::Paused);
        storage.clear_playback_session().unwrap();
        assert!(storage.load_playback_session().is_none());
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn custom_playlists_store_network_and_local_songs_and_support_editing() {
        let data_dir = std::env::temp_dir().join(format!(
            "voicefox-custom-playlist-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&data_dir).unwrap();
        let storage = Storage {
            data_dir: data_dir.clone(),
            favorites: RwLock::new(Vec::new()),
            favorite_playlists: RwLock::new(Vec::new()),
            custom_playlists: RwLock::new(Vec::new()),
            history: RwLock::new(Vec::new()),
            custom_playlists_update: std::sync::Mutex::new(()),
            generation: AtomicU64::new(0),
        };
        assert!(storage.create_custom_playlist("   ").is_err());
        let playlist = storage.create_custom_playlist("通勤").unwrap();
        assert!(storage.create_custom_playlist("通勤").is_err());
        let network = song("network", SourceId::Wy, "Network", "Artist");
        let mut local = song("local", SourceId::Local, "Local", "Artist");
        local.file_path = Some(data_dir.join("local.flac"));

        assert!(
            storage
                .add_song_to_custom_playlist(&playlist.id, &network)
                .unwrap()
        );
        assert!(
            storage
                .add_song_to_custom_playlist(&playlist.id, &local)
                .unwrap()
        );
        assert!(
            !storage
                .add_song_to_custom_playlist(&playlist.id, &local)
                .unwrap()
        );
        storage
            .rename_custom_playlist(&playlist.id, "夜间通勤")
            .unwrap();

        let stored = storage.custom_playlist(&playlist.id).unwrap();
        assert_eq!(stored.name, "夜间通勤");
        assert_eq!(stored.songs.len(), 2);
        assert_eq!(stored.songs[1].file_path, local.file_path);
        let summaries = storage.custom_playlist_summaries();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].song_count, 2);
        let persisted: Vec<CustomPlaylist> =
            Storage::load_file(&data_dir.join("custom_playlists.json"));
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].name, "夜间通勤");
        assert_eq!(persisted[0].songs.len(), 2);
        assert!(
            storage
                .remove_song_from_custom_playlist(&playlist.id, &network)
                .unwrap()
        );
        assert_eq!(
            storage.custom_playlist(&playlist.id).unwrap().songs.len(),
            1
        );
        assert!(storage.delete_custom_playlist(&playlist.id).unwrap());
        assert!(storage.custom_playlist_summaries().is_empty());

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn batch_add_reports_added_and_duplicate_skipped_counts() {
        let data_dir = std::env::temp_dir().join(format!(
            "voicefox-custom-playlist-batch-test-{}-{}",
            std::process::id(),
            unix_nanos()
        ));
        std::fs::create_dir_all(&data_dir).unwrap();
        let storage = Storage {
            data_dir: data_dir.clone(),
            favorites: RwLock::new(Vec::new()),
            favorite_playlists: RwLock::new(Vec::new()),
            custom_playlists: RwLock::new(Vec::new()),
            history: RwLock::new(Vec::new()),
            custom_playlists_update: std::sync::Mutex::new(()),
            generation: AtomicU64::new(0),
        };
        let playlist = storage.create_custom_playlist("批量").unwrap();
        storage
            .add_song_to_custom_playlist(&playlist.id, &song("a", SourceId::Wy, "A", "Artist"))
            .unwrap();

        let batch = vec![
            song("a", SourceId::Wy, "A", "Artist"), // 重复
            song("b", SourceId::Wy, "B", "Artist"),
            song("c", SourceId::Kw, "C", "Artist"),
        ];
        let (added, skipped) = storage
            .add_songs_to_custom_playlist(&playlist.id, &batch)
            .unwrap();

        assert_eq!((added, skipped), (2, 1));
        assert_eq!(
            storage.custom_playlist(&playlist.id).unwrap().songs.len(),
            3
        );
        let persisted: Vec<CustomPlaylist> =
            Storage::load_file(&data_dir.join("custom_playlists.json"));
        assert_eq!(persisted[0].songs.len(), 3);
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn empty_song_ids_do_not_collapse_unrelated_tracks() {
        let first = song("", SourceId::Kw, "First", "Artist");
        let second = song("", SourceId::Kw, "Second", "Artist");

        assert!(!same_song_identity(&first, &second));
    }

    #[test]
    fn deleting_a_local_file_cleans_all_custom_playlists_and_persists() {
        let data_dir = std::env::temp_dir().join(format!(
            "voicefox-custom-local-delete-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&data_dir).unwrap();
        let storage = Storage {
            data_dir: data_dir.clone(),
            favorites: RwLock::new(Vec::new()),
            favorite_playlists: RwLock::new(Vec::new()),
            custom_playlists: RwLock::new(Vec::new()),
            history: RwLock::new(Vec::new()),
            custom_playlists_update: std::sync::Mutex::new(()),
            generation: AtomicU64::new(0),
        };
        let first_playlist = storage.create_custom_playlist("通勤").unwrap();
        let second_playlist = storage.create_custom_playlist("夜晚").unwrap();
        let path = data_dir.join("local.flac");
        let mut local = song(&path.to_string_lossy(), SourceId::Local, "Local", "Artist");
        local.file_path = Some(path.clone());
        let legacy_local = song(&path.to_string_lossy(), SourceId::Local, "Legacy", "Artist");
        storage
            .add_song_to_custom_playlist(&first_playlist.id, &local)
            .unwrap();
        storage
            .add_song_to_custom_playlist(&second_playlist.id, &legacy_local)
            .unwrap();

        assert_eq!(
            storage
                .remove_local_path_from_custom_playlists(&path)
                .unwrap(),
            2
        );
        assert!(
            storage
                .custom_playlist(&first_playlist.id)
                .unwrap()
                .songs
                .is_empty()
        );
        assert!(
            storage
                .custom_playlist(&second_playlist.id)
                .unwrap()
                .songs
                .is_empty()
        );
        let persisted: Vec<CustomPlaylist> =
            Storage::load_file(&data_dir.join("custom_playlists.json"));
        assert!(persisted.iter().all(|playlist| playlist.songs.is_empty()));

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn versioned_export_import_round_trips_and_creates_backup() {
        let root = std::env::temp_dir().join(format!(
            "voicefox-backup-test-{}-{}",
            std::process::id(),
            unix_nanos()
        ));
        let source_dir = root.join("source");
        let target_dir = root.join("target");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir_all(&target_dir).unwrap();
        let make_storage = |data_dir| Storage {
            data_dir,
            favorites: RwLock::new(Vec::new()),
            favorite_playlists: RwLock::new(Vec::new()),
            custom_playlists: RwLock::new(Vec::new()),
            history: RwLock::new(Vec::new()),
            custom_playlists_update: std::sync::Mutex::new(()),
            generation: AtomicU64::new(0),
        };
        let source = make_storage(source_dir);
        let favorite = song("favorite", SourceId::Wy, "Favorite", "Artist");
        let history = song("history", SourceId::Kg, "History", "Artist");
        source.add_favorite(&favorite);
        source.add_history(&history, 100);
        source.create_custom_playlist("通勤").unwrap();
        let export_path = root.join("voicefox-data.json");
        source.export_data(&export_path).unwrap();

        let target = make_storage(target_dir);
        target.add_favorite(&song("old", SourceId::Kw, "Old", "Artist"));
        let automatic_backup = target.import_data(&export_path).unwrap();

        assert!(automatic_backup.exists());
        assert_eq!(target.load_favorites()[0].id, favorite.id);
        assert_eq!(target.load_history()[0].id, history.id);
        assert_eq!(target.custom_playlist_summaries()[0].name, "通勤");
        let persisted: Vec<SongInfo> = Storage::load_file(&target.data_dir.join("favorites.json"));
        assert_eq!(persisted[0].id, favorite.id);
        let old_backup: DataBackup =
            serde_json::from_slice(&std::fs::read(automatic_backup).unwrap()).unwrap();
        assert_eq!(old_backup.favorites[0].id, "old");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn import_rejects_unknown_data_version_without_changing_state() {
        let root = std::env::temp_dir().join(format!(
            "voicefox-backup-version-test-{}-{}",
            std::process::id(),
            unix_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let storage = Storage {
            data_dir: root.clone(),
            favorites: RwLock::new(Vec::new()),
            favorite_playlists: RwLock::new(Vec::new()),
            custom_playlists: RwLock::new(Vec::new()),
            history: RwLock::new(Vec::new()),
            custom_playlists_update: std::sync::Mutex::new(()),
            generation: AtomicU64::new(0),
        };
        storage.add_favorite(&song("keep", SourceId::Kw, "Keep", "Artist"));
        let path = root.join("future.json");
        let mut backup = storage.snapshot();
        backup.version += 1;
        std::fs::write(&path, serde_json::to_vec(&backup).unwrap()).unwrap();

        assert!(storage.import_data(&path).is_err());
        assert_eq!(storage.load_favorites()[0].id, "keep");
        assert!(!root.join("backups").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn imports_m3u_and_netease_style_json_into_custom_playlists() {
        let root = std::env::temp_dir().join(format!(
            "voicefox-playlist-import-test-{}-{}",
            std::process::id(),
            unix_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let storage = Storage {
            data_dir: root.clone(),
            favorites: RwLock::new(Vec::new()),
            favorite_playlists: RwLock::new(Vec::new()),
            custom_playlists: RwLock::new(Vec::new()),
            history: RwLock::new(Vec::new()),
            custom_playlists_update: std::sync::Mutex::new(()),
            generation: AtomicU64::new(0),
        };
        let m3u = root.join("mix.m3u");
        std::fs::write(
            &m3u,
            "#EXTM3U\n#EXTINF:123,Artist - Local song\nmissing.flac\n",
        )
        .unwrap();
        let report = storage.import_external_playlist(&m3u).unwrap();
        assert_eq!(report.imported, 1);
        assert_eq!(
            storage.custom_playlist(&report.playlist_id).unwrap().songs[0].source,
            SourceId::Local
        );

        let json = root.join("netease.json");
        std::fs::write(
            &json,
            serde_json::json!({
                "playlist": {"tracks": [{"id": 42, "name": "Online", "ar": [{"name": "Singer"}], "al": {"name": "Album"}}]}
            })
            .to_string(),
        )
        .unwrap();
        let report = storage.import_external_playlist(&json).unwrap();
        let songs = &storage.custom_playlist(&report.playlist_id).unwrap().songs;
        assert_eq!(songs.len(), 1);
        assert_eq!(songs[0].source, SourceId::Wy);
        assert_eq!(songs[0].id, "42");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_unversioned_backup_is_migrated_on_import() {
        let root = std::env::temp_dir().join(format!(
            "voicefox-legacy-import-test-{}-{}",
            std::process::id(),
            unix_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let storage = Storage {
            data_dir: root.clone(),
            favorites: RwLock::new(Vec::new()),
            favorite_playlists: RwLock::new(Vec::new()),
            custom_playlists: RwLock::new(Vec::new()),
            history: RwLock::new(Vec::new()),
            custom_playlists_update: std::sync::Mutex::new(()),
            generation: AtomicU64::new(0),
        };
        let path = root.join("legacy.json");
        let favorite = song("legacy", SourceId::Wy, "Legacy", "Artist");
        std::fs::write(
            &path,
            serde_json::json!({
                "favorites": [favorite],
                "favorite_playlists": [],
                "custom_playlists": [],
                "history": []
            })
            .to_string(),
        )
        .unwrap();
        storage.import_data(&path).unwrap();
        assert_eq!(storage.load_favorites()[0].id, "legacy");
        let _ = std::fs::remove_dir_all(root);
    }
}
