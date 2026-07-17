//! JS 音源下载与缓存
//!
//! 从 URL 下载 JS 脚本，缓存到 `~/.config/lx-tui/sources/` 目录

use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use super::engine::JsEngine;
use super::js_source::JsSource;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IntegrityPolicy {
    Enforce,
    ApproveChanges,
}

fn local_source_path(input: &str) -> Option<PathBuf> {
    let value = input.strip_prefix("file://").unwrap_or(input);
    let path = if let Some(relative) = value.strip_prefix("~/") {
        dirs::home_dir()?.join(relative)
    } else {
        PathBuf::from(value)
    };
    path.is_file().then_some(path)
}

/// 获取 URL 对应的本地缓存路径。
pub fn cached_source_path(url: &str) -> PathBuf {
    use md5::Digest;

    let cache_dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("lx-tui")
        .join("sources");
    let hash = md5::Md5::digest(url.as_bytes());
    cache_dir.join(format!("{:x}.js", hash))
}

pub fn is_source_cached(url: &str) -> bool {
    valid_cached_path(&cached_source_path(url)).is_some()
}

fn valid_cached_path(path: &Path) -> Option<PathBuf> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() && metadata.len() > 0 => Some(path.to_path_buf()),
        _ => None,
    }
}

fn integrity_path(path: &Path) -> PathBuf {
    path.with_extension("sha256")
}

fn sha256_hex(code: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(code))
}

fn write_integrity(path: &Path, code: &[u8]) -> Result<(), String> {
    std::fs::write(integrity_path(path), sha256_hex(code))
        .map_err(|e| format!("写入 JS 音源完整性记录失败: {e}"))
}

fn verified_cached_path(path: &Path) -> Result<Option<PathBuf>, String> {
    let Some(path) = valid_cached_path(path) else {
        return Ok(None);
    };
    let code = std::fs::read(&path).map_err(|e| format!("读取 JS 音源缓存失败: {e}"))?;
    let actual = sha256_hex(&code);
    let pin_path = integrity_path(&path);
    match std::fs::read_to_string(&pin_path) {
        Ok(expected) if expected.trim().eq_ignore_ascii_case(&actual) => Ok(Some(path)),
        Ok(_) => Err("JS 音源缓存完整性校验失败".to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write_integrity(&path, &code)?;
            Ok(Some(path))
        }
        Err(error) => Err(format!("读取 JS 音源完整性记录失败: {error}")),
    }
}

fn write_cache(path: &Path, code: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建缓存目录失败: {e}"))?;
    }

    let temporary_path = path.with_extension(format!("js.{}.tmp", std::process::id()));
    std::fs::write(&temporary_path, code).map_err(|e| format!("写入缓存临时文件失败: {e}"))?;
    if let Err(error) = std::fs::rename(&temporary_path, path) {
        let _ = std::fs::remove_file(&temporary_path);
        return Err(format!("替换 JS 音源缓存失败: {error}"));
    }
    Ok(())
}

fn replace_cache(path: &Path, code: &[u8]) -> Result<(), String> {
    write_cache(path, code)?;
    if let Err(error) = write_integrity(path, code) {
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(())
}

/// 下载 JS 音源文件到本地缓存。网络失败时保留并使用已有有效缓存。
///
/// 返回缓存后的文件路径。文件名基于 URL 的 MD5 hash。
pub async fn download_source(url: &str) -> Result<PathBuf, String> {
    download_source_with_policy(url, IntegrityPolicy::Enforce).await
}

async fn download_source_with_policy(
    url: &str,
    policy: IntegrityPolicy,
) -> Result<PathBuf, String> {
    let path = cached_source_path(url);

    if let Some(local_path) = local_source_path(url) {
        let code = std::fs::read(&local_path)
            .map_err(|e| format!("读取本地 JS 音源失败（{}）: {e}", local_path.display()))?;
        if code.iter().all(u8::is_ascii_whitespace) {
            return verified_cached_path(&path)?.ok_or_else(|| "本地 JS 音源内容为空".to_string());
        }
        replace_cache(&path, &code)?;
        return Ok(path);
    }

    let trusted_cache = verified_cached_path(&path)?;
    let client = super::super::http::client();

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("下载 JS 音源失败（网络错误）: {e}"));

    let resp = match response {
        Ok(resp) if resp.status().is_success() => resp,
        Ok(resp) => {
            let error = format!("下载 JS 音源失败（HTTP {}）", resp.status().as_u16());
            return trusted_cache.ok_or(error);
        }
        Err(error) => return trusted_cache.ok_or(error),
    };

    let code = resp
        .text()
        .await
        .map_err(|e| format!("读取 JS 音源内容失败: {e}"));
    let code = match code {
        Ok(code) if !code.trim().is_empty() => code,
        Ok(_) => {
            return trusted_cache.ok_or_else(|| "下载的 JS 音源内容为空".to_string());
        }
        Err(error) => return trusted_cache.ok_or(error),
    };

    let downloaded_hash = sha256_hex(code.as_bytes());
    if policy == IntegrityPolicy::Enforce
        && let Ok(expected) = std::fs::read_to_string(integrity_path(&path))
        && !expected.trim().eq_ignore_ascii_case(&downloaded_hash)
    {
        tracing::warn!(
            "remote JS source changed; keeping pinned cache until the user re-imports it"
        );
        return trusted_cache
            .ok_or_else(|| "远程 JS 音源内容已变化，请在设置中重新导入以确认更新".to_string());
    }

    // 先完整写入临时文件，再替换缓存，避免失败下载截断上一份可用脚本。
    replace_cache(&path, code.as_bytes())?;

    Ok(path)
}

/// 下载并启动一个 JS 音源。下载、脚本初始化均最多重试三次。
pub async fn load_source(url: &str, default_source: &str) -> Result<JsSource, String> {
    load_source_with_policy(url, default_source, IntegrityPolicy::Enforce).await
}

/// 用户主动导入音源时允许确认同一 URL 的内容更新。
pub async fn load_source_approving_update(
    url: &str,
    default_source: &str,
) -> Result<JsSource, String> {
    load_source_with_policy(url, default_source, IntegrityPolicy::ApproveChanges).await
}

async fn load_source_with_policy(
    url: &str,
    default_source: &str,
    policy: IntegrityPolicy,
) -> Result<JsSource, String> {
    let mut last_error = "未知错误".to_string();
    let cache_path = cached_source_path(url);
    let previous_cache = std::fs::read(&cache_path)
        .ok()
        .filter(|code| !code.is_empty());
    let previous_integrity = std::fs::read(integrity_path(&cache_path)).ok();

    for attempt in 1..=3 {
        let result = async {
            let path = download_source_with_policy(url, policy).await?;
            let path_string = path
                .to_str()
                .ok_or_else(|| "JS 音源路径包含无效字符".to_string())?
                .to_string();
            let engine = tokio::task::spawn_blocking(move || JsEngine::new(&path_string))
                .await
                .map_err(|error| format!("启动 JS 引擎任务失败: {error}"))??;
            let name = path
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("js-source")
                .to_string();
            Ok::<_, String>(JsSource::new(name, engine, default_source.to_string()))
        }
        .await;

        match result {
            Ok(source) => return Ok(source),
            Err(error) => {
                last_error = error;
                if let Some(code) = &previous_cache {
                    if let Err(restore_error) = write_cache(&cache_path, code) {
                        last_error = format!("{last_error}；恢复旧缓存失败: {restore_error}");
                    } else if let Some(integrity) = &previous_integrity {
                        if let Err(restore_error) =
                            std::fs::write(integrity_path(&cache_path), integrity)
                        {
                            last_error =
                                format!("{last_error}；恢复完整性记录失败: {restore_error}");
                        }
                    } else if let Err(restore_error) = write_integrity(&cache_path, code) {
                        last_error = format!("{last_error}；重建完整性记录失败: {restore_error}");
                    }
                } else {
                    let _ = std::fs::remove_file(&cache_path);
                    let _ = std::fs::remove_file(integrity_path(&cache_path));
                }
                if attempt < 3 {
                    tokio::time::sleep(Duration::from_millis(250 * attempt as u64)).await;
                }
            }
        }
    }

    Err(format!("JS 音源加载失败（已重试 3 次）: {last_error}"))
}
