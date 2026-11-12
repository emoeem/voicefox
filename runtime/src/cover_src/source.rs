//! 封面的获取与本地缓存

use std::sync::OnceLock;

use lx_source::cover_cache;
use lx_source::http::{RETRY_ATTEMPTS, SendWithRetry};
use regex::Regex;
use reqwest::header::{ACCEPT, REFERER};

/// 清理进程异常退出后残留在缓存目录里的临时文件，并按缓存总量上限淘汰旧文件。
///
/// 目录、命名、校验与淘汰规则都在 [`cover_cache`] 里：本地文件的内嵌封面
/// 缓存共用同一份，避免两个模块各自维护一份 512 的预算。
pub async fn sweep_temp_files() {
    cover_cache::sweep().await;
}

/// 已就绪的封面
#[derive(Debug, Clone)]
pub struct CoverImage {
    pub path: String,
    /// 像素 宽/高
    pub aspect: f32,
}

/// 下载封面到本地缓存，返回缓存路径与像素宽高比
pub async fn download_and_cache(client: &reqwest::Client, url: &str) -> Result<CoverImage, String> {
    if !cover_cache::cache_dir().exists() {
        // async 上下文里避免阻塞 worker 的同步文件系统调用
        let _ = tokio::fs::create_dir_all(cover_cache::cache_dir()).await;
    }

    // 本地文件直接返回路径
    if is_local_path(url) {
        let path = url.strip_prefix("file://").unwrap_or(url);
        if !tokio::fs::try_exists(path).await.unwrap_or(false) {
            return Err("封面文件不存在".to_string());
        }
        let aspect = probe_aspect(path)
            .await
            .ok_or_else(|| "封面图片无法完整解码".to_string())?;
        return Ok(CoverImage {
            path: path.to_string(),
            aspect,
        });
    }

    // 远程文件：下载到缓存
    let url = validate_remote_url(url).map_err(|error| {
        tracing::debug!("cover url rejected: {url:?}: {error}");
        format!("封面地址无效: {}（{url}）", error)
    })?;

    let cache_path = cover_cache::remote_cache_path(&url);

    match probe_aspect(&cache_path).await {
        Some(aspect) => {
            return Ok(CoverImage {
                path: cache_path.to_string_lossy().to_string(),
                aspect,
            });
        }
        // 读不到宽高说明缓存文件已损坏，需要重新下载。
        // 也可能是 image crate default-formats 未包含的格式，但同样无法处理，因此视作损坏。
        None if cache_path.exists() => {
            tracing::debug!("cover cache {cache_path:?} is unreadable, downloading again");
            let _ = tokio::fs::remove_file(&cache_path).await;
        }
        None => {}
    }

    // HTTP 下载
    let mut request = client
        .get(&url)
        .header(ACCEPT, "image/webp,image/apng,image/*,*/*;q=0.8");
    if let Some(referer) = cover_referer(&url) {
        request = request.header(REFERER, referer);
    }
    let bytes = request
        .send_with_retry(RETRY_ATTEMPTS)
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .bytes()
        .await
        .map_err(|error| error.to_string())?;

    // 解码校验留在重试之外：重试只针对发送阶段的瞬时网络错误，
    // 4xx/5xx 与损坏图片立刻失败（不再白白重试三次）。
    let target = cache_path.clone();
    let (width, height) =
        tokio::task::spawn_blocking(move || cover_cache::store_bytes(&target, &bytes))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| format!("写入封面缓存失败: {error}"))?;
    let aspect = width as f32 / height as f32;

    Ok(CoverImage {
        path: cache_path.to_string_lossy().to_string(),
        aspect,
    })
}

/// 判断封面地址是否指向本地文件。
fn is_local_path(url: &str) -> bool {
    if url.starts_with("file://") {
        return true;
    }
    // 绝对路径的形式随平台而异，需要 is_absolute 判断。
    // 但以 `//` 开头的地址是省略协议的远程地址，在 windows 上会被解析成 UNC 路径，
    // 需要先行排除。本地音乐的 cover_url 指向缓存目录，而后者绝大多数情况下不在
    // UNC 路径上，因此排除是安全的。
    !url.starts_with("//") && std::path::Path::new(url).is_absolute()
}

/// 完整解码图片并取像素宽高比，失败返回 None（同步实现走共享缓存模块）。
pub async fn probe_aspect(path: impl AsRef<std::path::Path>) -> Option<f32> {
    let path = path.as_ref().to_path_buf();
    tokio::task::spawn_blocking(move || cover_cache::probe_aspect(&path))
        .await
        .ok()
        .flatten()
}

/// 规整音源返回的封面地址：去空白、补 `//` 前缀、剥掉包裹引号。
pub fn normalize_url(url: &str) -> String {
    let url = url.trim().trim_matches(['"', '\'']).trim();
    if url.starts_with("//") {
        format!("https:{url}")
    } else {
        url.to_string()
    }
}

/// 校验远程封面地址是否可能是一张图。
///
/// 音源经常返回残缺地址（例如只剩域名 `https://p`、`https://y.gtimg.cn`），
/// 直接请求只会拿到 400/403 或 HTML 首页，随后以「封面不可用」的面目出现，
/// 很难定位。这里提前挡掉，并把原始地址带进错误信息。
pub fn validate_remote_url(url: &str) -> Result<String, &'static str> {
    static HOST: OnceLock<Regex> = OnceLock::new();
    let url = normalize_url(url);
    let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    else {
        return Err("缺少 http(s) 前缀");
    };
    if url.chars().any(char::is_whitespace) {
        return Err("地址包含空白字符");
    }
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    // 只保留主机名部分，丢掉端口与可能的 userinfo
    let host = host.rsplit('@').next().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    if !HOST
        .get_or_init(|| {
            Regex::new(r"^(?:[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?\.)+[A-Za-z]{2,}$")
                .expect("valid cover host regex")
        })
        .is_match(host)
    {
        return Err("主机名不完整");
    }
    // 没有路径说明音源只给出了域名，不可能指向具体图片
    if path.trim_matches('/').is_empty() {
        return Err("只有域名、没有图片路径");
    }
    Ok(url)
}

/// 地址是否是一个可能指向图片的远程 URL（不发起网络请求）。
pub fn is_usable_remote_url(url: &str) -> bool {
    validate_remote_url(url).is_ok()
}

fn cover_referer(url: &str) -> Option<&'static str> {
    if url.contains("kuwo.cn") {
        Some("https://www.kuwo.cn/")
    } else if url.contains("kugou.com") {
        Some("https://www.kugou.com/")
    } else if url.contains("qq.com") {
        Some("https://y.qq.com/")
    } else if url.contains("music.163.com") || url.contains("126.net") {
        Some("https://music.163.com/")
    } else {
        None
    }
}
#[cfg(test)]
mod tests {
    use super::{is_local_path, is_usable_remote_url, validate_remote_url};

    #[test]
    fn local_paths_are_told_apart_from_remote_urls() {
        assert!(is_local_path("file:///tmp/cover.jpg"));
        #[cfg(unix)]
        assert!(is_local_path("/tmp/cover.jpg"));
        #[cfg(windows)]
        {
            assert!(is_local_path(r"C:\Users\me\cover.jpg"));
            assert!(is_local_path(r"\\server\share\cover.jpg"));
        }
        assert!(!is_local_path("//y.gtimg.cn/music/x.jpg"));
        assert!(!is_local_path("https://p1.music.126.net/x.jpg"));
        assert!(!is_local_path("cover.jpg"));
    }

    #[test]
    fn truncated_cover_urls_are_rejected_with_a_reason() {
        // 音源只回一个残缺主机名时，请求只会拿到 400/403 或 HTML 首页
        assert!(validate_remote_url("https://p").is_err());
        assert!(validate_remote_url("https://y.gtimg.cn").is_err());
        assert!(validate_remote_url("https://p1.music.126.net/").is_err());
        assert!(validate_remote_url("https://").is_err());
        assert!(validate_remote_url("p1.music.126.net/x.jpg").is_err());
    }

    #[test]
    fn well_formed_cover_urls_survive_normalization() {
        assert_eq!(
            validate_remote_url("//p1.music.126.net/abc/x.jpg").unwrap(),
            "https://p1.music.126.net/abc/x.jpg"
        );
        assert_eq!(
            validate_remote_url("  \"https://y.gtimg.cn/music/photo/T002.jpg\"  ").unwrap(),
            "https://y.gtimg.cn/music/photo/T002.jpg"
        );
        assert!(
            validate_remote_url("http://artistpicserver.kuwo.cn/pic.web?corp=kuwo&rid=1").is_ok()
        );
    }

    #[test]
    fn usability_probe_never_requests_the_network() {
        assert!(is_usable_remote_url("https://p1.music.126.net/a/b.jpg"));
        assert!(!is_usable_remote_url("https://p"));
        assert!(!is_usable_remote_url(""));
    }
}
