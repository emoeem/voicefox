//! 下载记录与跨会话去重。
//!
//! 移植自 go-music-dl 的 `core/download_record.go`：下载完成后按
//! 「歌手 - 歌名」记录一条历史，并把它作为去重索引。这样即使换了文件名
//! 模板、或者重启了程序，同一首歌也不会被重复下载。
//!
//! 与参考实现一致的两个细节：
//! - 去重命中的前提是文件仍然存在；文件被用户删掉后会回收该条索引，
//!   下次可以重新下载，而不是永远判定为「已下载」；
//! - 去重键只做控制字符清洗，不做大小写折叠，避免把不同歌曲误判为同一首。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use serde::{Deserialize, Serialize};

/// 磁盘上保留的下载记录条数上限。
pub const DEFAULT_RECORD_LIMIT: usize = 200;

/// 一次下载的终态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadStatus {
    /// 下载并写入标签完成。
    Done,
    /// 命中已有文件或去重索引，未重复下载。
    Skipped,
    /// 失败，原因见 `error`。
    Failed,
}

impl DownloadStatus {
    pub fn label(self) -> &'static str {
        match self {
            DownloadStatus::Done => "成功",
            DownloadStatus::Skipped => "跳过",
            DownloadStatus::Failed => "失败",
        }
    }
}

/// 一条下载记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadRecord {
    pub name: String,
    #[serde(default)]
    pub singer: String,
    pub source: SourceId,
    pub status: DownloadStatus,
    #[serde(default)]
    pub error: Option<String>,
    /// 相对下载目录的路径；失败记录没有落盘文件。
    #[serde(default)]
    pub rel_path: Option<String>,
    /// 结束时间（Unix 秒）。
    pub finished_at: i64,
}

impl DownloadRecord {
    pub fn new(
        song: &SongInfo,
        status: DownloadStatus,
        rel_path: Option<String>,
        error: Option<String>,
    ) -> Self {
        Self {
            name: song.name.clone(),
            singer: song.singer.clone(),
            source: song.source,
            status,
            error,
            rel_path,
            finished_at: unix_seconds(),
        }
    }
}

/// 落盘格式：记录列表 + 版本号。去重索引在加载时从记录推导，避免两份
/// 数据不一致（参考实现用两张 SQLite 表，这里用单一来源更简单）。
#[derive(Debug, Serialize, Deserialize)]
struct RecordsFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    records: Vec<DownloadRecord>,
}

impl Default for RecordsFile {
    fn default() -> Self {
        Self {
            version: 1,
            records: Vec::new(),
        }
    }
}

struct RecordsState {
    records: Vec<DownloadRecord>,
    /// 去重索引：`歌手 - 歌名` → 相对下载目录的路径。
    dedup: HashMap<String, String>,
}

/// 下载记录存储。内存常驻，写入即落盘。
pub struct DownloadRecords {
    path: PathBuf,
    limit: usize,
    state: Mutex<RecordsState>,
}

impl DownloadRecords {
    /// 从默认数据目录加载。
    pub fn load_default() -> Self {
        Self::at_path(crate::storage::default_data_dir().join("downloads.json"))
    }

    /// 从指定文件加载，供测试与自定义数据目录使用。
    pub fn at_path(path: PathBuf) -> Self {
        let file = fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<RecordsFile>(&raw).ok())
            .unwrap_or_default();
        let mut records = file.records;
        let limit = DEFAULT_RECORD_LIMIT;
        if records.len() > limit {
            records.drain(..records.len() - limit);
        }
        let dedup = build_dedup(&records);
        Self {
            path,
            limit,
            state: Mutex::new(RecordsState { records, dedup }),
        }
    }

    /// 去重键：`歌手 - 歌名`，清洗控制字符并裁剪空白。
    pub fn song_key(singer: &str, name: &str) -> String {
        let singer = clean_text(singer);
        let name = clean_text(name);
        let singer = if singer.is_empty() {
            "Unknown".to_string()
        } else {
            singer
        };
        let name = if name.is_empty() {
            "Unknown".to_string()
        } else {
            name
        };
        format!("{singer} - {name}")
    }

    /// 这首歌是否已经下载过，且文件仍在下载目录里。
    ///
    /// 命中但文件已被删除时回收这条索引：用户手动删掉的文件应当可以重新
    /// 下载，而不是被旧记录永久挡住。
    pub fn existing_download(&self, song: &SongInfo, download_dir: &Path) -> Option<PathBuf> {
        let key = Self::song_key(&song.singer, &song.name);
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let rel_path = state.dedup.get(&key)?.clone();
        let path = download_dir.join(&rel_path);
        if path.is_file() {
            return Some(path);
        }
        state.dedup.remove(&key);
        let records = state.records.clone();
        drop(state);
        self.persist(&records);
        None
    }

    /// 追加一条记录并落盘；超出上限的旧记录会被裁剪。
    pub fn record(&self, record: DownloadRecord) {
        let records = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.records.push(record);
            let overflow = state.records.len().saturating_sub(self.limit);
            if overflow > 0 {
                state.records.drain(..overflow);
            }
            state.dedup = build_dedup(&state.records);
            state.records.clone()
        };
        self.persist(&records);
    }

    /// 最近 `limit` 条记录，按时间倒序（最新在前）。
    pub fn recent(&self, limit: usize) -> Vec<DownloadRecord> {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.records.iter().rev().take(limit).cloned().collect()
    }

    pub fn len(&self) -> usize {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.records.len()
    }

    /// 清空记录与去重索引，并删除文件。
    pub fn clear(&self) -> Result<(), String> {
        {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.records.clear();
            state.dedup.clear();
        }
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("删除下载记录失败: {error}")),
        }
    }

    fn persist(&self, records: &[DownloadRecord]) {
        let file = RecordsFile {
            version: 1,
            records: records.to_vec(),
        };
        let Ok(json) = serde_json::to_vec_pretty(&file) else {
            return;
        };
        if let Some(parent) = self.path.parent()
            && let Err(error) = fs::create_dir_all(parent)
        {
            tracing::warn!("创建下载记录目录失败: {error}");
            return;
        }
        if let Err(error) = crate::storage::save_atomic(&self.path, &json) {
            tracing::warn!("写入下载记录失败: {error}");
        }
    }
}

/// 从记录推导去重索引：只保留成功/跳过且有路径的记录，后者覆盖前者。
fn build_dedup(records: &[DownloadRecord]) -> HashMap<String, String> {
    let mut dedup = HashMap::new();
    for record in records {
        let Some(rel_path) = record.rel_path.as_deref().filter(|path| !path.is_empty()) else {
            continue;
        };
        if !matches!(
            record.status,
            DownloadStatus::Done | DownloadStatus::Skipped
        ) {
            continue;
        }
        dedup.insert(
            DownloadRecords::song_key(&record.singer, &record.name),
            rel_path.to_string(),
        );
    }
    dedup
}

/// 清洗控制字符并按空白裁剪，保证同一首歌在不同来源下得到同一个键。
fn clean_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| *character >= ' ' && *character != '\u{7f}')
        .collect::<String>()
        .trim()
        .to_string()
}

/// 下载目录内文件的相对路径，统一用 `/` 分隔，便于跨平台读取旧记录。
pub fn relative_path(download_dir: &Path, file: &Path) -> Option<String> {
    let relative = file.strip_prefix(download_dir).ok()?;
    let parts = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "voicefox-records-{tag}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn song(name: &str, singer: &str) -> SongInfo {
        SongInfo::new(
            "id-1".to_string(),
            SourceId::Kw,
            name.to_string(),
            singer.to_string(),
        )
    }

    #[test]
    fn song_key_normalizes_whitespace_and_control_characters() {
        assert_eq!(
            DownloadRecords::song_key("  周杰伦 ", "晴天\u{7f}"),
            "周杰伦 - 晴天"
        );
        assert_eq!(DownloadRecords::song_key("", ""), "Unknown - Unknown");
    }

    #[test]
    fn skipped_song_is_remembered_until_the_file_disappears() {
        let dir = temp_dir("dedup");
        let store = DownloadRecords::at_path(dir.join("downloads.json"));
        let file = dir.join("周杰伦 - 晴天.flac");
        fs::write(&file, b"audio").unwrap();

        store.record(DownloadRecord::new(
            &song("晴天", "周杰伦"),
            DownloadStatus::Done,
            Some("周杰伦 - 晴天.flac".to_string()),
            None,
        ));

        assert_eq!(
            store.existing_download(&song("晴天", "周杰伦"), &dir),
            Some(file.clone())
        );
        // 换一份文件名模板不影响去重：键只看歌手与歌名。
        assert!(
            store
                .existing_download(&song("晴天", "周杰伦"), &dir)
                .is_some()
        );

        fs::remove_file(&file).unwrap();
        assert!(
            store
                .existing_download(&song("晴天", "周杰伦"), &dir)
                .is_none()
        );
        // 索引已回收，重新下载同名文件后可以再次命中。
        fs::write(&file, b"audio").unwrap();
        assert!(
            store
                .existing_download(&song("晴天", "周杰伦"), &dir)
                .is_none()
        );
        store.record(DownloadRecord::new(
            &song("晴天", "周杰伦"),
            DownloadStatus::Done,
            Some("周杰伦 - 晴天.flac".to_string()),
            None,
        ));
        assert!(
            store
                .existing_download(&song("晴天", "周杰伦"), &dir)
                .is_some()
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn failed_downloads_are_recorded_without_becoming_dedup_entries() {
        let dir = temp_dir("failed");
        let store = DownloadRecords::at_path(dir.join("downloads.json"));
        store.record(DownloadRecord::new(
            &song("晴天", "周杰伦"),
            DownloadStatus::Failed,
            None,
            Some("获取播放地址失败".to_string()),
        ));

        let recent = store.recent(10);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].status, DownloadStatus::Failed);
        assert!(
            store
                .existing_download(&song("晴天", "周杰伦"), &dir)
                .is_none()
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn records_survive_a_reload_and_keep_newest_first() {
        let dir = temp_dir("reload");
        let path = dir.join("downloads.json");
        let file = dir.join("第一首.mp3");
        fs::write(&file, b"audio").unwrap();
        {
            let store = DownloadRecords::at_path(path.clone());
            store.record(DownloadRecord::new(
                &song("第一首", "歌手"),
                DownloadStatus::Done,
                Some("第一首.mp3".to_string()),
                None,
            ));
            store.record(DownloadRecord::new(
                &song("第二首", "歌手"),
                DownloadStatus::Failed,
                None,
                Some("下载失败".to_string()),
            ));
        }

        let reloaded = DownloadRecords::at_path(path);
        let recent = reloaded.recent(10);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].name, "第二首");
        assert_eq!(recent[1].name, "第一首");
        // 去重索引随记录一起恢复。
        assert_eq!(
            reloaded.existing_download(&song("第一首", "歌手"), &dir),
            Some(file)
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn clearing_records_also_clears_the_dedup_index() {
        let dir = temp_dir("clear");
        let path = dir.join("downloads.json");
        let file = dir.join("晴天.mp3");
        fs::write(&file, b"audio").unwrap();
        let store = DownloadRecords::at_path(path.clone());
        store.record(DownloadRecord::new(
            &song("晴天", "周杰伦"),
            DownloadStatus::Done,
            Some("晴天.mp3".to_string()),
            None,
        ));
        assert!(
            store
                .existing_download(&song("晴天", "周杰伦"), &dir)
                .is_some()
        );

        store.clear().unwrap();
        assert_eq!(store.len(), 0);
        assert!(
            store
                .existing_download(&song("晴天", "周杰伦"), &dir)
                .is_none()
        );
        assert!(!path.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn relative_path_uses_forward_slashes() {
        let dir = PathBuf::from("/music");
        let file = dir.join("album").join("a.mp3");
        assert_eq!(relative_path(&dir, &file).as_deref(), Some("album/a.mp3"));
        assert!(relative_path(&dir, Path::new("/elsewhere/a.mp3")).is_none());
    }
}
