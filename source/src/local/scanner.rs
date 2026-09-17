//! 本地音乐目录扫描器

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::time::UNIX_EPOCH;

use crate::local::metadata;
use cue_rw::CUEFile;
use lx_core::model::song::EXTRA_FILE_MODIFIED_UNIX_NANOS;

use super::LocalSong;

/// 支持的音频文件扩展名。
///
/// `lofty` 能够直接读取 Opus（通常位于 Ogg 容器中），因此这里单独列出
/// `opus`，否则用户的文件会在扩展名过滤阶段被静默忽略。
const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "m4a", "ogg", "opus", "wav", "wma", "aac", "ape", "aiff", "aif",
];
const CUE_EXTENSION: &str = "cue";

/// 需要排除的目录名
const EXCLUDED_DIRS: &[&str] = &[".stfolder", "node_modules", ".git", ".Trash"];

/// 判断文件扩展名是否属于本地音频格式。
pub fn is_supported_audio_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|extension| AUDIO_EXTENSIONS.contains(&extension.as_str()))
}

/// CUE 文件也属于可索引的本地媒体描述文件。
pub fn is_supported_media_extension(path: &Path) -> bool {
    is_supported_audio_extension(path)
        || path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case(CUE_EXTENSION))
}

/// 文件系统指纹。
///
/// 目录监听只需要比较大小和修改时间，不需要读取文件内容。修改时间使用纳秒，
/// 以避免编辑器在同一秒内连续保存标签时遗漏更新。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileFingerprint {
    pub size: u64,
    pub modified_unix_nanos: u128,
}

impl FileFingerprint {
    pub fn from_path(path: &Path) -> std::io::Result<Self> {
        let metadata = std::fs::metadata(path)?;
        let modified_unix_nanos = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_nanos());
        Ok(Self {
            size: metadata.len(),
            modified_unix_nanos,
        })
    }
}

/// 目录级指纹：目录自身的大小与修改时间。
///
/// 文件/子目录的创建、删除、重命名都会更新父目录的 mtime；但**原地修改**
/// 文件内容不会。因此目录签名只能证明“没有增删/重命名”，不能证明内容
/// 未变，只适用于没有任何变化证据时的跳过遍历优化（监听器触发的重扫必须
/// 强制遍历）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirSignature {
    pub size: u64,
    pub modified_unix_nanos: u128,
}

impl DirSignature {
    pub fn from_path(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        if !metadata.is_dir() {
            return None;
        }
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_nanos());
        Some(Self {
            size: metadata.len(),
            modified_unix_nanos: modified,
        })
    }
}

/// 扫描失败的文件。失败文件不会进入歌曲列表，但会保留下来供“损坏文件”页面展示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanFailure {
    pub path: PathBuf,
    pub error: String,
}

/// 一次目录扫描的结果。
#[derive(Debug, Default, Clone)]
pub struct ScanReport {
    pub songs: Vec<LocalSong>,
    pub failures: Vec<ScanFailure>,
    /// 本次遍历看到的全部音频文件（包括元数据损坏的文件）。
    pub fingerprints: HashMap<PathBuf, FileFingerprint>,
    pub reused: usize,
    pub parsed: usize,
}

/// 快速获取目录内音频文件的指纹快照。
///
/// 该函数不读取标签，供后台监听器判断目录是否发生变化。
pub fn snapshot_directory(path: &Path, max_depth: u32) -> HashMap<PathBuf, FileFingerprint> {
    let mut snapshot = HashMap::new();
    for entry in audio_entries(path, max_depth) {
        let Ok(fingerprint) = FileFingerprint::from_path(&entry) else {
            continue;
        };
        let absolute = canonical_path(&entry);
        snapshot.insert(absolute, fingerprint);
    }
    snapshot
}

/// 扫描指定目录下的所有音频文件。
pub fn scan_directory(path: &Path, max_depth: u32) -> Vec<LocalSong> {
    scan_directory_report(path, max_depth).songs
}

/// 扫描并返回失败文件及指纹信息。
pub fn scan_directory_report(path: &Path, max_depth: u32) -> ScanReport {
    scan_directory_incremental(path, max_depth, &HashMap::new(), &HashMap::new())
}

/// 增量扫描目录。
///
/// `previous` 和 `previous_fingerprints` 来自上一次扫描，均以“来源文件”为
/// key：普通歌曲是其音频文件，CUE 分轨是其 CUE 文件（一个 key 对应多条轨，
/// 避免 CUE 轨与整轨歌曲因共用音频路径而互相顶替）。指纹未变化的文件会
/// 直接复用旧的 `LocalSong`，因此不会重复解析标签或提取封面。
pub fn scan_directory_incremental(
    path: &Path,
    max_depth: u32,
    previous: &HashMap<PathBuf, Vec<LocalSong>>,
    previous_fingerprints: &HashMap<PathBuf, FileFingerprint>,
) -> ScanReport {
    let mut report = ScanReport::default();

    for entry_path in audio_entries(path, max_depth) {
        let absolute = canonical_path(&entry_path);
        let fingerprint = match FileFingerprint::from_path(&entry_path) {
            Ok(value) => value,
            Err(error) => {
                report.failures.push(ScanFailure {
                    path: absolute,
                    error: error.to_string(),
                });
                continue;
            }
        };
        report.fingerprints.insert(absolute.clone(), fingerprint);

        if previous_fingerprints.get(&absolute) == Some(&fingerprint)
            && let Some(songs) = previous.get(&absolute)
        {
            report.songs.extend(songs.iter().cloned());
            report.reused += songs.len();
            continue;
        }

        if entry_path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case(CUE_EXTENSION))
        {
            match read_cue_tracks(&entry_path) {
                Ok(tracks) => {
                    // 复用 key 用 CUE 文件自身的路径：CUE 轨与其引用的整轨
                    // 音频共用音频路径，按音频路径做 key 会互相顶替。播放
                    // 用的 song.file_path 仍指向音频文件。
                    report.songs.extend(tracks.into_iter().map(|mut track| {
                        track.file_path = absolute.clone();
                        track
                    }));
                    report.parsed += 1;
                }
                Err(error) => report.failures.push(ScanFailure {
                    path: absolute,
                    error,
                }),
            }
            continue;
        }

        match metadata::read_metadata(&entry_path) {
            Ok(mut song) => {
                song.file_path = Some(absolute.clone());
                song.extra.insert(
                    EXTRA_FILE_MODIFIED_UNIX_NANOS.to_string(),
                    fingerprint.modified_unix_nanos.to_string(),
                );
                song.extra
                    .insert("file_size".to_string(), fingerprint.size.to_string());
                report.songs.push(LocalSong {
                    song,
                    file_path: absolute,
                });
                report.parsed += 1;
            }
            Err(error) => {
                tracing::debug!("跳过文件 {}: {}", entry_path.display(), error);
                report.failures.push(ScanFailure {
                    path: absolute,
                    error,
                });
            }
        }
    }

    report.songs.sort_by(|a, b| a.file_path.cmp(&b.file_path));
    report.failures.sort_by(|a, b| a.path.cmp(&b.path));
    report
}

fn audio_entries(path: &Path, max_depth: u32) -> Vec<PathBuf> {
    let walker = walkdir::WalkDir::new(path)
        .follow_links(false)
        .max_depth(if max_depth == 0 {
            usize::MAX
        } else {
            max_depth as usize
        });

    walker
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0
                || !entry.file_type().is_dir()
                || !EXCLUDED_DIRS.contains(&entry.file_name().to_str().unwrap_or_default())
        })
        .filter_map(Result::ok)
        .filter(|entry| !entry.path().is_dir())
        .filter_map(|entry| {
            is_supported_media_extension(entry.path()).then(|| entry.path().to_path_buf())
        })
        .collect()
}

#[derive(Default)]
struct CueTrack {
    number: String,
    title: Option<String>,
    performer: Option<String>,
    start: Option<Duration>,
    /// Index into the FILE entries declared before this track.
    file_index: Option<usize>,
}

/// 解析常见的单文件 CUE。未识别的命令会被忽略，以兼容附加 REM 元数据。
fn read_cue_tracks(path: &Path) -> Result<Vec<LocalSong>, String> {
    let content = read_cue_text(path)?;

    // `cue-rw` handles quoted paths, multiple FILE sections, comments, track
    // flags and the 75-frames-per-second timestamp grammar.  A small fallback
    // parser below keeps compatibility with older sheets that omit optional
    // album fields or use non-standard indentation rejected by the crate.
    if let Ok(cue) = CUEFile::try_from(content.as_str()) {
        return read_cue_tracks_with_crate(path, &cue);
    }

    read_cue_tracks_legacy(path, &content)
}

/// 读取 CUE 文本。部分整轨 CUE 使用 GBK 编码，UTF-8 解析失败时降级用
/// GB18030 重试，避免"stream did not contain valid UTF-8"导致整张专辑无法入库。
fn read_cue_text(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("读取 CUE 失败: {}: {error}", path.display()))?;
    match String::from_utf8(bytes) {
        Ok(content) => Ok(content),
        Err(error) => {
            let (content, _, had_errors) = encoding_rs::GB18030.decode(error.as_bytes());
            tracing::warn!(
                "CUE 不是有效 UTF-8，已用 GB18030 降级解码: {}",
                path.display()
            );
            if had_errors {
                return Err(format!(
                    "读取 CUE 失败: {}: 既不是 UTF-8 也无法按 GB18030 解码",
                    path.display()
                ));
            }
            Ok(content.into_owned())
        }
    }
}

fn read_cue_tracks_with_crate(path: &Path, cue: &CUEFile) -> Result<Vec<LocalSong>, String> {
    if cue.tracks.is_empty() {
        return Err("CUE 未包含曲目".to_string());
    }

    let mut file_metadata = Vec::with_capacity(cue.files.len());
    for file in &cue.files {
        let referenced = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(decode_cue_text(file));
        let referenced = referenced.canonicalize().unwrap_or(referenced);
        let base = metadata::read_metadata(&referenced)
            .map_err(|error| format!("读取 CUE 音频失败 ({}): {error}", referenced.display()))?;
        file_metadata.push((referenced, base));
    }

    // CUEFile intentionally does not retain the numeric TRACK token.  Track
    // order is the canonical identity for a sheet, and numbering is normally
    // sequential; use two digits to match conventional CUE identifiers.
    let mut result = Vec::with_capacity(cue.tracks.len());
    for (index, (file_id, track)) in cue.tracks.iter().enumerate() {
        let Some((referenced, base)) = file_metadata.get(*file_id) else {
            return Err(format!("CUE 曲目引用了不存在的 FILE #{}", file_id + 1));
        };
        let Some((_, start_stamp)) = track.indices.iter().find(|(number, _)| *number == 1) else {
            continue;
        };
        let start = parse_cue_time(&start_stamp.to_string())
            .ok_or_else(|| format!("CUE 曲目 {} 的 INDEX 01 无效", index + 1))?;

        let end = cue
            .tracks
            .iter()
            .skip(index + 1)
            .find_map(|(next_file_id, next_track)| {
                (*next_file_id == *file_id).then(|| {
                    next_track
                        .indices
                        .iter()
                        .find(|(number, _)| *number == 1)
                        .and_then(|(_, stamp)| parse_cue_time(&stamp.to_string()))
                })
            })
            .flatten()
            .unwrap_or(base.duration);

        let mut song = base.clone();
        let number = format!("{:02}", index + 1);
        song.id = format!("{}#{}", path.to_string_lossy(), number);
        let title = decode_cue_text(&track.title);
        song.name = if title.trim().is_empty() {
            format!("Track {:02}", index + 1)
        } else {
            title
        };
        let performer = track
            .performer
            .as_deref()
            .map(decode_cue_text)
            .unwrap_or_else(|| decode_cue_text(&cue.performer));
        song.singer = if performer.trim().is_empty() {
            "未知艺术家".to_string()
        } else {
            performer
        };
        let album = decode_cue_text(&cue.title);
        song.album_name = if album.trim().is_empty() {
            path.file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("未知专辑")
                .to_string()
        } else {
            album
        };
        song.duration = end.saturating_sub(start);
        song.file_path = Some(referenced.clone());
        song.extra
            .insert("cue_file".to_string(), path.to_string_lossy().to_string());
        song.extra.insert("cue_track".to_string(), number);
        song.extra
            .insert("cue_start_ms".to_string(), start.as_millis().to_string());
        result.push(LocalSong {
            file_path: referenced.clone(),
            song,
        });
    }

    if result.is_empty() {
        return Err("CUE 没有带 INDEX 01 的曲目".to_string());
    }
    Ok(result)
}

/// Compatibility parser for permissive sheets which `cue-rw` intentionally
/// rejects (for example sheets without global TITLE/PERFORMER).  Keep this
/// isolated so the standards-compliant path remains delegated to the crate.
fn read_cue_tracks_legacy(path: &Path, content: &str) -> Result<Vec<LocalSong>, String> {
    let mut referenced = Vec::new();
    let mut current_file_index = None;
    let mut album = None;
    let mut album_performer = None;
    let mut tracks = Vec::new();
    let mut current: Option<CueTrack> = None;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("REM ") {
            continue;
        }
        let (command, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let value = rest.trim();
        match command.to_ascii_uppercase().as_str() {
            "FILE" => {
                if let Some(name) = cue_value(value) {
                    let candidate = path.parent().unwrap_or_else(|| Path::new(".")).join(name);
                    referenced.push(candidate);
                    current_file_index = Some(referenced.len() - 1);
                }
            }
            "TITLE" => {
                if let Some(track) = current.as_mut() {
                    track.title = cue_value(value);
                } else {
                    album = cue_value(value);
                }
            }
            "PERFORMER" => {
                if let Some(track) = current.as_mut() {
                    track.performer = cue_value(value);
                } else {
                    album_performer = cue_value(value);
                }
            }
            "TRACK" => {
                if let Some(track) = current.take() {
                    tracks.push(track);
                }
                let number = value.split_whitespace().next().unwrap_or("00");
                current = Some(CueTrack {
                    number: number.to_string(),
                    file_index: current_file_index,
                    ..Default::default()
                });
            }
            "INDEX" if current.is_some() => {
                let mut parts = value.split_whitespace();
                if parts.next().and_then(|number| number.parse::<u8>().ok()) == Some(1)
                    && let Some(time) = parts.next()
                {
                    current.as_mut().unwrap().start = parse_cue_time(time);
                }
            }
            _ => {}
        }
    }
    if let Some(track) = current {
        tracks.push(track);
    }
    if referenced.is_empty() {
        return Err("CUE 未包含 FILE 指令".to_string());
    }
    if tracks.is_empty() {
        return Err("CUE 未包含 TRACK 指令".to_string());
    }

    // Read each referenced file once. Multi-file sheets are common for cues
    // generated by ripping tools; each track keeps the FILE it belongs to.
    let file_metadata = referenced
        .into_iter()
        .map(|referenced| {
            let referenced = referenced.canonicalize().unwrap_or(referenced);
            let base = metadata::read_metadata(&referenced).map_err(|error| {
                format!("读取 CUE 音频失败 ({}): {error}", referenced.display())
            })?;
            Ok::<_, String>((referenced, base))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let album = album.unwrap_or_else(|| {
        path.file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("未知专辑")
            .to_string()
    });
    let performer = album_performer.unwrap_or_else(|| "未知艺术家".to_string());
    let mut result = Vec::new();
    for (index, track) in tracks.iter().enumerate() {
        let Some(start) = track.start else { continue };
        let Some(file_index) = track.file_index else {
            // TRACK before FILE is invalid CUE. Ignore that block while
            // retaining any subsequent valid tracks.
            continue;
        };
        let Some((referenced, base)) = file_metadata.get(file_index) else {
            return Err(format!(
                "CUE 曲目 {} 引用了不存在的 FILE #{}",
                track.number,
                file_index + 1
            ));
        };
        let mut song = base.clone();
        song.id = format!("{}#{}", path.to_string_lossy(), track.number);
        song.name = track
            .title
            .clone()
            .unwrap_or_else(|| format!("Track {}", track.number));
        song.singer = track.performer.clone().unwrap_or_else(|| performer.clone());
        song.album_name = album.clone();
        // A track ends at the next INDEX 01 in the same FILE. If the next
        // track belongs to another file, use this file's duration.
        let end = tracks
            .iter()
            .skip(index + 1)
            .find(|next| next.file_index == Some(file_index))
            .and_then(|next| next.start)
            .unwrap_or(base.duration);
        song.duration = end.saturating_sub(start);
        song.file_path = Some(referenced.clone());
        song.extra
            .insert("cue_file".to_string(), path.to_string_lossy().to_string());
        song.extra
            .insert("cue_track".to_string(), track.number.clone());
        song.extra
            .insert("cue_start_ms".to_string(), start.as_millis().to_string());
        result.push(LocalSong {
            file_path: referenced.clone(),
            song,
        });
    }
    if result.is_empty() {
        return Err("CUE 未包含带 INDEX 01 的曲目".to_string());
    }
    Ok(result)
}

fn cue_value(value: &str) -> Option<String> {
    let value = value.trim();
    if let Some(body) = value.strip_prefix('"') {
        let mut escaped = false;
        for (index, ch) in body.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == '"' {
                return Some(decode_cue_text(&body[..index]));
            }
        }
        return None;
    }
    (!value.is_empty()).then(|| value.split_whitespace().next().unwrap_or(value).to_string())
}

/// Decode the two escapes permitted in quoted CUE strings (`\"` and `\\`).
/// `cue-rw` intentionally preserves escapes in its owned strings, so this
/// adapter keeps the values presented to the rest of the app human-readable.
fn decode_cue_text(value: &str) -> String {
    let mut decoded = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('"') => decoded.push('"'),
                Some('\\') => decoded.push('\\'),
                Some(other) => {
                    decoded.push('\\');
                    decoded.push(other);
                }
                None => decoded.push('\\'),
            }
        } else {
            decoded.push(ch);
        }
    }
    decoded
}

fn parse_cue_time(value: &str) -> Option<Duration> {
    let mut parts = value.split(':');
    let minutes = parts.next()?.parse::<u64>().ok()?;
    let seconds = parts.next()?.parse::<u64>().ok()?;
    let frames = parts.next()?.parse::<u64>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    (seconds < 60 && frames < 75).then(|| {
        Duration::from_secs(minutes * 60 + seconds)
            + Duration::from_nanos(frames * 1_000_000_000 / 75)
    })
}

fn canonical_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{FileFingerprint, decode_cue_text, parse_cue_time, snapshot_directory};

    #[test]
    fn snapshot_includes_opus_files_and_ignores_unknown_extensions() {
        let root = std::env::temp_dir().join(format!(
            "voicefox-scanner-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("track.opus"), b"not a valid opus file").unwrap();
        fs::write(root.join("cover.jpg"), b"image").unwrap();

        let snapshot = snapshot_directory(&root, 0);
        assert_eq!(snapshot.len(), 1);
        assert!(snapshot.contains_key(&root.join("track.opus").canonicalize().unwrap()));

        let fingerprint = FileFingerprint::from_path(&root.join("track.opus")).unwrap();
        assert!(fingerprint.size > 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cue_time_uses_seventy_five_frames_per_second() {
        assert_eq!(
            parse_cue_time("01:02:37"),
            Some(std::time::Duration::from_secs(62) + std::time::Duration::from_nanos(493_333_333))
        );
        assert!(parse_cue_time("01:60:00").is_none());
        assert!(parse_cue_time("01:02:03:04").is_none());
        assert!(parse_cue_time("bad").is_none());
    }

    #[test]
    fn cue_quoted_escapes_are_decoded() {
        assert_eq!(
            decode_cue_text(r#"A \"quote\" and \\ path"#),
            r#"A "quote" and \ path"#
        );
    }
}
