//! 音频元数据读取（使用 lofty）

use std::path::Path;

use lofty::file::{AudioFile, TaggedFileExt};
use lofty::tag::Accessor;

use lx_core::model::song::SongInfo;

/// 读取音频文件的元数据
pub fn read_metadata(path: &Path) -> Result<SongInfo, String> {
    let tagged = lofty::read_from_path(path).map_err(|e| format!("lofty error: {}", e))?;

    let properties = tagged.properties();
    let duration = properties.duration();

    // 提取主标签
    let (title, artist, album) = if let Some(tag) = tagged.first_tag() {
        let title = tag
            .title()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| file_stem(path));

        let artist = tag
            .artist()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "未知艺术家".to_string());

        let album = tag
            .album()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "未知专辑".to_string());

        (title, artist, album)
    } else {
        (file_stem(path), "未知艺术家".to_string(), "未知专辑".to_string())
    };

    // 提取封面并缓存
    let cover_path = extract_cover(&tagged, path);

    Ok(SongInfo {
        id: path.to_string_lossy().to_string(),
        name: title,
        singer: artist,
        album_name: album,
        album_id: String::new(),
        duration,
        source: lx_core::model::source::SourceId::Local,
        qualities: std::collections::BTreeSet::from([lx_core::model::source::Quality::High320]),
        cover_url: cover_path,
        extra: std::collections::HashMap::new(),
        toggle_source: None,
        file_path: None,
        file_ext: path.extension().and_then(|e| e.to_str()).map(|s| s.to_string()),
    })
}

/// 从文件名提取歌曲名（不含扩展名）
fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("未知歌曲")
        .to_string()
}

/// 提取嵌入的封面图并缓存到磁盘
fn extract_cover(tagged: &lofty::file::TaggedFile, audio_path: &Path) -> Option<String> {
    let tag = tagged.first_tag()?;

    // 尝试读取封面
    let picture = tag.pictures().first()?;

    // 缓存目录
    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("voicefox")
        .join("covers");

    if !cache_dir.exists() {
        let _ = std::fs::create_dir_all(&cache_dir);
    }

    // 用文件路径的 hash 作为缓存文件名
    let hash = simple_hash(audio_path.to_string_lossy().as_bytes());
    let cover_path = cache_dir.join(format!("{}.jpg", hash));

    if cover_path.exists() {
        return Some(cover_path.to_string_lossy().to_string());
    }

    let data = picture.data();
    if std::fs::write(&cover_path, data).is_ok() {
        Some(cover_path.to_string_lossy().to_string())
    } else {
        None
    }
}

/// 简单的字符串哈希
fn simple_hash(data: &[u8]) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}
