//! 本地音乐目录扫描器

use std::path::Path;

use crate::local::metadata;

use super::LocalSong;

/// 支持的音频文件扩展名
const AUDIO_EXTENSIONS: &[&str] = &["mp3", "flac", "m4a", "ogg", "wav", "wma", "aac", "ape"];

/// 需要排除的目录名
const EXCLUDED_DIRS: &[&str] = &[".stfolder", "node_modules", ".git", ".Trash"];

/// 扫描指定目录下的所有音频文件
pub fn scan_directory(path: &Path, max_depth: u32) -> Vec<LocalSong> {
    let mut songs = Vec::new();

    let walker = walkdir::WalkDir::new(path)
        .follow_links(false)
        .max_depth(if max_depth == 0 { std::usize::MAX } else { max_depth as usize });

    for entry in walker.into_iter().filter_map(|e| e.ok()) {
        let entry_path = entry.path();

        // 跳过目录
        if entry_path.is_dir() {
            let dir_name = entry_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if EXCLUDED_DIRS.contains(&dir_name) {
                continue;
            }
            continue;
        }

        // 检查扩展名
        let ext = entry_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        if !AUDIO_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }

        // 读取元数据
        match metadata::read_metadata(entry_path) {
            Ok(mut song) => {
                let abs_path = entry_path
                    .canonicalize()
                    .unwrap_or_else(|_| entry_path.to_path_buf());
                song.file_path = Some(abs_path.clone());

                songs.push(LocalSong {
                    song,
                    file_path: abs_path,
                });
            }
            Err(e) => {
                tracing::debug!("跳过文件 {}: {}", entry_path.display(), e);
            }
        }
    }

    // 按文件名排序
    songs.sort_by(|a, b| a.file_path.cmp(&b.file_path));

    songs
}
