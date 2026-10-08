//! 封面磁盘缓存：远程封面与本地文件内嵌封面**共用**一套目录、命名与校验规则。
//!
//! 这里曾经有两份逐字复制的实现（`voicefox-runtime` 的远程封面缓存、
//! `lx-source` 本地文件的内嵌封面缓存）。它们在 Linux 上解析到**同一个目录**
//! `~/.cache/voicefox/covers`，用同一套 `{sha256}.jpg` 命名、同一套
//! `.part.{pid}.{seq}` 临时文件、同一条"解码验证后才原子 rename"的规则——
//! 也就是说 runtime 侧的 `sweep` 已经在按自己的口径淘汰 lx-source 写进去的
//! 文件。合并到这里之后：
//!
//! - 目录只有一个定义，两边不可能再漂移；
//! - [`CACHE_LIMIT`] 是**整份缓存**的预算，不会出现两套 sweep 互相清对方的文件；
//! - 校验规则（必须能完整解码且有非零宽高）只有一处，损坏文件的处理一致。

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// 临时文件名的流水号（同进程内递增，避免并发写同一目标时撞名）。
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// 临时文件名中的标记：`sweep` 靠它区分"正在写入"与"缓存文件"。
pub const TEMP_INFIX: &str = ".part.";

/// 临时文件在此时长内被视为其他实例正在写入，不清理。
pub const TEMP_GRACE: Duration = Duration::from_secs(60);

/// 整份封面缓存的文件数上限（远程封面 + 本地内嵌封面共享这一份预算）。
pub const CACHE_LIMIT: usize = 512;

/// 封面缓存目录。
///
/// 优先用 `directories::ProjectDirs`（与 runtime 侧原有取值一致），拿不到时
/// 退回 `dirs::cache_dir()/voicefox/covers`——两条路在 Linux 上是同一个目录。
pub fn cache_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "voicefox")
        .map(|project| project.cache_dir().to_path_buf())
        .unwrap_or_else(|| {
            dirs::cache_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("voicefox")
        })
        .join("covers")
}

/// 内容哈希（sha256 十六进制）。
///
/// 之前两处都用 `std::collections::hash_map::DefaultHasher` 作为**磁盘文件名**：
/// std 明确声明其算法不保证跨 Rust 版本稳定，换工具链会让整份缓存失配。
pub fn content_hash(data: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(data))
}

/// 缓存文件名（`{内容哈希}.jpg`；扩展名只是占位，真实格式由内容嗅探）。
fn cache_file_name(hash: &str, extension: &str) -> String {
    format!("{hash}.{extension}")
}

/// 读取图片像素尺寸；文件不存在、无法解码、宽高为 0 都返回 `None`。
pub fn probe_dimensions(path: impl AsRef<Path>) -> Option<(u32, u32)> {
    let image = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    let (width, height) = (image.width(), image.height());
    (width > 0 && height > 0).then_some((width, height))
}

/// 像素宽高比；读不出尺寸返回 `None`。
pub fn probe_aspect(path: impl AsRef<Path>) -> Option<f32> {
    probe_dimensions(path).map(|(width, height)| width as f32 / height as f32)
}

/// 写入临时文件，完整解码验证后原子替换目标，返回像素尺寸。
///
/// 目标已存在时由 `rename` 原子替换（先打开的句柄仍读到旧内容）。
/// 任何失败都会清掉临时文件，不把半截数据留在缓存里。
pub fn store_bytes(target: &Path, bytes: &[u8]) -> io::Result<(u32, u32)> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp_path = temp_path_for(target);
    let result = (|| {
        std::fs::write(&temp_path, bytes)?;
        let dimensions = probe_dimensions(&temp_path)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "cover image is corrupt"))?;
        std::fs::rename(&temp_path, target)?;
        Ok(dimensions)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

/// 远端 URL 对应的缓存文件路径（缓存键是 URL 的内容哈希）。
///
/// 只给路径不落盘：调用方通常要先探测命中，再决定是否下载。
pub fn remote_cache_path(url: &str) -> PathBuf {
    cache_dir().join(cache_file_name(&content_hash(url.as_bytes()), "jpg"))
}

/// 本地文件内嵌封面的缓存路径。
///
/// 缓存键 = 音频文件路径 + 图片字节：同一路径被重新嵌入封面（改标签）后
/// 会得到新键，不会继续显示旧封面。
pub fn embedded_cover_path(audio_path: &Path, picture: &[u8]) -> PathBuf {
    let mut key = audio_path.to_string_lossy().as_bytes().to_vec();
    key.extend_from_slice(picture);
    cache_dir().join(cache_file_name(&content_hash(&key), "jpg"))
}

/// 同目录下的临时文件路径。
fn temp_path_for(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(format!(
        "{TEMP_INFIX}{}.{}",
        std::process::id(),
        TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    target.with_file_name(name)
}

/// 清理进程异常退出后残留的临时文件，并把缓存总量压回 [`CACHE_LIMIT`]。
///
/// 淘汰口径是"最旧访问时间优先"；注意 Linux 默认的 `relatime` 会让 atime 只在
/// 一天一次或早于 mtime 时更新，因此这里的 atime 更接近"最久没被写过"。
///
/// 目录不存在（还没写过任何封面）时直接返回，不报错。
pub async fn sweep() {
    sweep_in(&cache_dir(), CACHE_LIMIT, TEMP_GRACE).await;
}

/// [`sweep`] 的实现体：目录与预算显式传入，便于测试与复用。
pub async fn sweep_in(dir: &Path, limit: usize, grace: Duration) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    // (访问时间, 路径)；临时文件按宽限期单独处理，不计入缓存总量
    let mut cached: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if entry.file_name().to_string_lossy().contains(TEMP_INFIX) {
            if let Ok(metadata) = entry.metadata().await
                && metadata
                    .modified()
                    .is_ok_and(|modified| modified.elapsed().is_ok_and(|age| age < grace))
            {
                continue;
            }
            match tokio::fs::remove_file(&path).await {
                Ok(()) => tracing::debug!("removed stale cover temp file {path:?}"),
                Err(error) => tracing::debug!("remove stale cover temp file failed: {error}"),
            }
            continue;
        }
        if let Ok(metadata) = entry.metadata().await
            && let Ok(atime) = metadata.accessed()
        {
            cached.push((atime, path));
        }
    }
    if cached.len() > limit {
        cached.sort_by_key(|(atime, _)| *atime);
        for (_, path) in &cached[..cached.len() - limit] {
            match tokio::fs::remove_file(path).await {
                Ok(()) => tracing::debug!("evicted cover cache {path:?}"),
                Err(error) => tracing::debug!("evict cover cache failed: {error}"),
            }
        }
    }
}

/// 缓存统计：`(文件数, 总字节)`；目录不存在时为 `(0, 0)`。
pub fn cache_stats() -> (usize, u64) {
    let dir = cache_dir();
    let mut files = 0usize;
    let mut bytes = 0u64;
    for entry in walkdir::WalkDir::new(&dir)
        .into_iter()
        .filter_map(Result::ok)
    {
        if entry.file_type().is_file() {
            files += 1;
            bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    (files, bytes)
}

/// 清空缓存目录，返回 `(删除的文件数, 释放的字节)`。
///
/// 先统计再删除：删除失败时统计仍可用于日志；目录不存在视为已清理。
pub fn clear_cache() -> std::io::Result<(usize, u64)> {
    let stats = cache_stats();
    if stats.0 > 0 {
        std::fs::remove_dir_all(cache_dir())?;
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        CACHE_LIMIT, cache_dir, content_hash, embedded_cover_path, probe_aspect, remote_cache_path,
        store_bytes, sweep_in,
    };

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("voicefox-cover-cache-{name}"));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let image = image::DynamicImage::ImageRgba8(image::RgbaImage::new(width, height));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_cache_write_leaves_exactly_the_target_file() {
        let dir = temp_dir("store-ok");
        let target = dir.join("cover.jpg");
        let dimensions = store_bytes(&target, &png_bytes(20, 10)).unwrap();

        assert_eq!(dimensions, (20, 10));
        assert_eq!(names(&dir), ["cover.jpg"]);
        assert_eq!(probe_aspect(&target), Some(2.0));
    }

    #[test]
    fn a_corrupt_image_is_rejected_and_leaves_nothing_behind() {
        let dir = temp_dir("store-corrupt");
        let target = dir.join("cover.jpg");
        let mut bytes = png_bytes(20, 10);
        bytes.truncate(33);

        assert!(store_bytes(&target, &bytes).is_err());
        assert!(!target.exists());
        assert!(names(&dir).is_empty(), "损坏文件与临时文件都不该留下");
    }

    #[test]
    fn a_failed_write_does_not_truncate_the_existing_cache() {
        let dir = temp_dir("store-replace");
        let target = dir.join("cover.jpg");
        std::fs::write(&target, b"old").unwrap();

        assert!(store_bytes(&target, b"not an image").is_err());
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"old",
            "旧缓存必须原样保留"
        );
        assert_eq!(names(&dir), ["cover.jpg"], "临时文件应已清掉");
    }

    /// 缓存键是内容寻址的：同一输入稳定，输入变化即换键。
    /// 这里只比较**路径**，不往真实缓存目录里写（CI 上它可能是只读的）。
    #[test]
    fn remote_and_embedded_keys_are_content_addressed() {
        let url = "https://p2.music.126.net/x==/1.jpg";
        let remote = remote_cache_path(url);
        assert!(remote.starts_with(cache_dir()));
        assert_eq!(remote.file_name().unwrap().to_string_lossy().len(), 64 + 4);
        assert_eq!(remote, remote_cache_path(url), "同一 URL 必须同一路径");
        assert_ne!(
            remote,
            remote_cache_path("https://p2.music.126.net/y==/2.jpg")
        );

        // 本地内嵌封面：路径或图片任一变化都会换键
        let audio = PathBuf::from("/music/a.flac");
        let first = embedded_cover_path(&audio, b"pic-1");
        assert_eq!(first, embedded_cover_path(&audio, b"pic-1"));
        assert_ne!(first, embedded_cover_path(&audio, b"pic-2"));
        assert_ne!(
            first,
            embedded_cover_path(Path::new("/music/b.flac"), b"pic-1")
        );
        assert!(first.starts_with(cache_dir()));

        // 真的写盘走显式目录（测试不碰真实缓存目录）
        let dir = temp_dir("store-remote");
        let target = dir.join(remote.file_name().unwrap());
        assert_eq!(store_bytes(&target, &png_bytes(4, 4)).unwrap(), (4, 4));
        assert!(target.exists());
    }

    #[test]
    fn content_hash_is_stable_and_hex() {
        let hash = content_hash(b"https://example.com/x.jpg");
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(hash, content_hash(b"https://example.com/x.jpg"));
    }

    #[tokio::test]
    async fn sweep_removes_stale_temp_files_but_keeps_fresh_ones() {
        let dir = temp_dir("sweep-temp");
        let stale = dir.join(format!("a{TEMP_SUFFIX}1.1"));
        let fresh = dir.join(format!("b{TEMP_SUFFIX}1.2"));
        std::fs::write(&stale, png_bytes(2, 2)).unwrap();
        std::fs::write(&fresh, png_bytes(2, 2)).unwrap();
        // 把 stale 的 mtime 推到宽限期之外
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
        set_mtime(&stale, old);

        let keep = dir.join("keep.jpg");
        store_bytes(&keep, &png_bytes(2, 2)).unwrap();

        sweep_in(&dir, CACHE_LIMIT, super::TEMP_GRACE).await;

        let names = names(&dir);
        assert!(!names.contains(&stale.file_name().unwrap().to_string_lossy().to_string()));
        assert!(names.contains(&fresh.file_name().unwrap().to_string_lossy().to_string()));
        assert!(names.contains(&keep.file_name().unwrap().to_string_lossy().to_string()));
    }

    #[tokio::test]
    async fn sweep_evicts_down_to_the_limit_by_oldest_access() {
        let dir = temp_dir("sweep-limit");
        for index in 0..6 {
            let path = dir.join(format!("{index}.jpg"));
            store_bytes(&path, &png_bytes(2, 2)).unwrap();
            let old =
                std::time::SystemTime::now() - std::time::Duration::from_secs(600 - index as u64);
            set_mtime(&path, old);
        }

        sweep_in(&dir, 3, super::TEMP_GRACE).await;

        let names = names(&dir);
        assert_eq!(names.len(), 3, "应只保留 limit 个");
        // 留下的是 mtime 最新的三个：3/4/5
        assert_eq!(names, ["3.jpg", "4.jpg", "5.jpg"]);
    }

    #[tokio::test]
    async fn sweeping_a_missing_directory_is_not_an_error() {
        sweep_in(
            &temp_dir("sweep-missing").join("nope"),
            1,
            super::TEMP_GRACE,
        )
        .await;
    }

    const TEMP_SUFFIX: &str = ".part.";

    /// 直接设置 mtime（测试用）：`filetime` 不是依赖，走 std 的 File API 不便，
    /// 这里用平台无关的"回拨"手法——把文件内容重写一次并把 mtime 往前挪。
    fn set_mtime(path: &std::path::Path, time: std::time::SystemTime) {
        // Linux 上用 utimensat 需要 libc；这里改用最简单可移植的办法：
        // 通过 `std::fs::File::set_modified`（Rust 1.75+）。
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(time).unwrap();
    }
}
