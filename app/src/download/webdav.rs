//! WebDAV 同步：下载完成后把音频推送到远端。
//!
//! 移植自 go-music-dl 的 `core/webdav.go`，行为对齐：
//! - 远端目录按需创建（逐级 `MKCOL`，`405 Method Not Allowed` 视为已存在）；
//! - 文件名沿用本地下载目录里的相对结构，远端不会丢层级；
//! - 上传失败只作为警告回报，本地文件已经落盘，不影响下载结果。

use std::path::Path;

use lx_core::model::config::WebdavConfig;

/// 创建远端目录时接受的状态码：`201 Created` 是新建成功，
/// `200/204` 是部分服务端的返回，`405` 表示目录已经存在。
const MKCOL_ACCEPTED: [u16; 4] = [200, 201, 204, 405];

/// WebDAV 是否可用：开关打开且填了地址。
pub fn is_configured(config: &WebdavConfig) -> bool {
    config.enabled && !config.url.trim().is_empty()
}

/// 解析远端地址：只接受 http/https，去掉末尾斜杠便于拼接。
pub fn parse_base_url(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("WebDAV 地址为空".to_string());
    }
    let (scheme, rest) = raw
        .split_once("://")
        .ok_or_else(|| "WebDAV 地址必须以 http:// 或 https:// 开头".to_string())?;
    if !matches!(scheme, "http" | "https") {
        return Err("WebDAV 地址必须以 http:// 或 https:// 开头".to_string());
    }
    let host = rest.split('/').next().unwrap_or_default();
    if host.is_empty() {
        return Err("WebDAV 地址缺少主机名".to_string());
    }
    Ok(raw.trim_end_matches('/').to_string())
}

/// 远端相对路径：`<远端目录>/<文件名的层级>`。
///
/// 文件名里的路径分隔符会被拆成多级目录，`..` 之类的相对段会被丢弃，
/// 避免模板中的 `{artist}/{album}` 在远端写到目录之外。
pub fn remote_relative_path(dir: &str, filename: &str) -> Result<String, String> {
    let dir = dir.trim().trim_matches('/');
    let mut parts: Vec<String> = Vec::new();
    if !dir.is_empty() {
        for segment in dir.split('/') {
            let segment = safe_segment(segment);
            if !segment.is_empty() {
                parts.push(segment);
            }
        }
    }
    for segment in filename.replace('\\', "/").split('/') {
        let segment = safe_segment(segment);
        if !segment.is_empty() {
            parts.push(segment);
        }
    }
    if parts.is_empty() {
        return Err("WebDAV 上传路径为空".to_string());
    }
    Ok(parts.join("/"))
}

/// 逐段创建远端目录，返回最终可用于 PUT 的目录路径。
pub async fn ensure_directories(
    client: &reqwest::Client,
    base: &str,
    config: &WebdavConfig,
    remote_rel: &str,
) -> Result<(), String> {
    let segments: Vec<&str> = remote_rel.split('/').collect();
    if segments.len() <= 1 {
        return Ok(());
    }
    let mut current = base.to_string();
    for segment in &segments[..segments.len() - 1] {
        if segment.trim().is_empty() {
            continue;
        }
        current = format!("{}/{segment}/", current.trim_end_matches('/'));
        let request = client
            .request(reqwest::Method::from_bytes(b"MKCOL").unwrap(), &current)
            .basic_auth(&config.username, Some(&config.password));
        let status = request
            .send()
            .await
            .map_err(|error| format!("创建 WebDAV 目录 {segment} 失败: {error}"))?
            .status()
            .as_u16();
        if !MKCOL_ACCEPTED.contains(&status) {
            return Err(format!("创建 WebDAV 目录 {segment} 失败: HTTP {status}"));
        }
    }
    Ok(())
}

/// 上传一个已落盘的音频文件。
///
/// 文件按字节读入内存后上传：单曲体积通常在几十 MB 以内，而并发下载
/// 数量本身有限，换来的是不需要额外的流式依赖。
pub async fn upload_file(
    client: &reqwest::Client,
    config: &WebdavConfig,
    local_path: &Path,
) -> Result<(), String> {
    if !is_configured(config) {
        return Ok(());
    }
    let filename = local_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "WebDAV 上传文件名无效".to_string())?;
    let remote_rel = remote_relative_path(&config.dir, filename)?;
    let base = parse_base_url(&config.url)?;
    let data = tokio::fs::read(local_path)
        .await
        .map_err(|error| format!("读取待上传文件失败: {error}"))?;

    ensure_directories(client, &base, config, &remote_rel).await?;

    let target = format!("{}/{remote_rel}", base.trim_end_matches('/'));
    let content_type = content_type_for(filename);
    let response = client
        .put(&target)
        .basic_auth(&config.username, Some(&config.password))
        .header(reqwest::header::CONTENT_TYPE, content_type)
        .body(data)
        .send()
        .await
        .map_err(|error| format!("上传到 WebDAV 失败: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let body = body.trim();
        let detail = if body.is_empty() {
            status.to_string()
        } else {
            format!("{status}: {body}")
        };
        return Err(format!("上传到 WebDAV 失败: {detail}"));
    }
    Ok(())
}

/// 音频扩展名对应的 MIME，与音频源下载时使用的一致。
fn content_type_for(filename: &str) -> &'static str {
    let ext = filename
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "flac" => "audio/flac",
        "ogg" | "opus" => "audio/ogg",
        "m4a" | "mp4" | "aac" => "audio/mp4",
        "wav" => "audio/wav",
        "wma" => "audio/x-ms-wma",
        _ => "audio/mpeg",
    }
}

/// 清洗单个路径段：去掉路径穿越与 Windows 非法字符。
fn safe_segment(segment: &str) -> String {
    let trimmed = segment.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return String::new();
    }
    crate::download::naming::sanitize_filename(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(url: &str, dir: &str, enabled: bool) -> WebdavConfig {
        WebdavConfig {
            enabled,
            url: url.to_string(),
            username: "user".to_string(),
            password: "pass".to_string(),
            dir: dir.to_string(),
        }
    }

    #[test]
    fn configuration_requires_both_switch_and_url() {
        assert!(is_configured(&config(
            "https://dav.example.com/dav",
            "",
            true
        )));
        assert!(!is_configured(&config(
            "https://dav.example.com/dav",
            "",
            false
        )));
        assert!(!is_configured(&config("   ", "", true)));
    }

    #[test]
    fn base_url_rejects_unsupported_schemes() {
        assert_eq!(
            parse_base_url("https://dav.example.com/dav/").unwrap(),
            "https://dav.example.com/dav"
        );
        assert!(parse_base_url("ftp://dav.example.com").is_err());
        assert!(parse_base_url("dav.example.com/dav").is_err());
        assert!(parse_base_url("https:///dav").is_err());
    }

    #[test]
    fn remote_path_keeps_template_subdirectories() {
        assert_eq!(
            remote_relative_path("voicefox", "周杰伦/叶惠美/晴天.flac").unwrap(),
            "voicefox/周杰伦/叶惠美/晴天.flac"
        );
        // 留空表示直接放在服务器根目录。
        assert_eq!(remote_relative_path("", "晴天.flac").unwrap(), "晴天.flac");
        assert_eq!(
            remote_relative_path("/voicefox/", "晴天.flac").unwrap(),
            "voicefox/晴天.flac"
        );
    }

    #[test]
    fn remote_path_drops_traversal_segments() {
        let path = remote_relative_path("voicefox", "../../etc/passwd").unwrap();
        assert_eq!(path, "voicefox/etc/passwd");
        assert!(remote_relative_path("", "../..").is_err());
    }

    #[test]
    fn content_type_matches_the_audio_extension() {
        assert_eq!(content_type_for("晴天.flac"), "audio/flac");
        assert_eq!(content_type_for("晴天.MP3"), "audio/mpeg");
        assert_eq!(content_type_for("晴天.m4a"), "audio/mp4");
        assert_eq!(content_type_for("无扩展名"), "audio/mpeg");
    }
}
