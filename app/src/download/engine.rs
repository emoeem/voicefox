//! 下载引擎：单线程流式 + 多线程分片，附带重试与完整性校验。
//!
//! 行为参考 MusicBot-Go 的 `bot/download/{service,multipart}.go`：
//! - 先用 HEAD / Range 探测源是否支持分片以及文件总大小；
//! - 大文件且支持 Range 时分片并发下载，否则回退单连接流式下载；
//! - 音源注入的备用 CDN（`DownloadRequest::candidate_urls`）与网易云
//!   m8→m7 节点改写一起作为候选地址，主地址失败时依次尝试；
//! - 音源声明 `max_chunk_size` 时走严格有界 Range 分片：不探测 HEAD、
//!   不发 plain GET、也不回退单连接（googlevideo 这类 CDN 只认有界 Range）；
//! - 落盘字节数与音源声明大小不一致视为完整性错误，不重试；
//! - 网络类错误按指数退避重试。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lx_core::model::config::DownloadConfig;
use reqwest::header::{
    ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, HeaderMap, HeaderName, HeaderValue, RANGE,
};
use tokio::io::AsyncWriteExt;

pub const USER_AGENT: &str = "voicefox/0.1";

/// 下载请求。`dest` 为最终文件名，临时文件写在同目录下。
#[derive(Debug, Clone)]
pub struct DownloadRequest {
    pub url: String,
    /// 音源要求的附加请求头（如 Referer / User-Agent）。
    pub headers: Vec<(String, String)>,
    pub dest: PathBuf,
    /// 音源声明的大小，用于完整性校验。
    pub expected_size: Option<u64>,
    /// 声明大小本身不可靠（部分源少报若干字节）时只校验「不短于声明值」。
    pub size_is_advisory: bool,
    /// 音源提供的 MD5，下载完成后用于二次校验。空字符串表示不校验。
    pub md5: Option<String>,
    /// 音源注入的备用 CDN 地址（如咪咕同一音质的多份文件），主地址失败时依次尝试。
    pub candidate_urls: Vec<String>,
    /// 音源要求「严格有界 Range 分片」时给出单次请求上限（字节），`0` 表示不启用。
    /// googlevideo（YouTube Music）这类 CDN 会拒绝 HEAD、plain GET 与超上限的
    /// Range，只能按不超过该值的有界 Range 分片抓取。
    pub max_chunk_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadOptions {
    /// 单个文件的分片并发数。
    pub concurrency: usize,
    /// 是否启用分片下载。
    pub multipart: bool,
    /// 小于该体积的文件不分片。
    pub multipart_min_size: u64,
    /// 网络类失败的最大重试次数。
    pub max_retries: u32,
    /// 是否校验落盘字节数。
    pub verify_size: bool,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            concurrency: 4,
            multipart: true,
            multipart_min_size: 5 * 1024 * 1024,
            max_retries: 3,
            verify_size: true,
        }
    }
}

impl DownloadOptions {
    /// 从用户配置构造下载选项，并对越界值做收敛。
    pub fn from_config(config: &DownloadConfig) -> Self {
        Self {
            concurrency: config.concurrency.clamp(1, 16),
            multipart: config.multipart,
            multipart_min_size: config.multipart_min_size_bytes(),
            max_retries: config.max_retries.min(10),
            verify_size: config.verify_size,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("下载已取消")]
    Cancelled,
    #[error("音源不支持 Range 分片")]
    RangesUnsupported,
    #[error("网络错误: {0}")]
    Network(String),
    #[error("文件完整性校验失败: {0}")]
    Integrity(String),
    #[error("写入文件失败: {0}")]
    Io(String),
    #[error("HTTP {status}")]
    Http { status: u16 },
    #[error("{0}")]
    Other(String),
}

impl DownloadError {
    /// 网络类、5xx 错误可以重试；完整性与取消不需要重试。
    pub fn is_retryable(&self) -> bool {
        match self {
            DownloadError::Network(_) | DownloadError::Io(_) => true,
            // 分片不可用属于「换个策略重试」：由调用方回退到单连接下载。
            DownloadError::RangesUnsupported => true,
            DownloadError::Http { status } => *status >= 500 || *status == 408 || *status == 429,
            DownloadError::Cancelled | DownloadError::Integrity(_) | DownloadError::Other(_) => {
                false
            }
        }
    }
}

/// 下载进度，UI 每帧读取快照，不阻塞下载任务。
#[derive(Debug, Default)]
pub struct DownloadProgress {
    downloaded: AtomicU64,
    total: AtomicU64,
    cancel: AtomicBool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressSnapshot {
    pub downloaded: u64,
    pub total: u64,
    pub cancelled: bool,
}

impl ProgressSnapshot {
    /// 进度比例；总大小未知时返回 `None`。
    pub fn ratio(&self) -> Option<f64> {
        if self.total == 0 {
            return None;
        }
        Some((self.downloaded as f64 / self.total as f64).clamp(0.0, 1.0))
    }
}

impl DownloadProgress {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn add(&self, bytes: u64) {
        if bytes > 0 {
            self.downloaded.fetch_add(bytes, Ordering::Relaxed);
        }
    }

    fn sub(&self, bytes: u64) {
        if bytes == 0 {
            return;
        }
        // 分片重试时回退已计入的字节，避免进度虚高。
        let mut current = self.downloaded.load(Ordering::Relaxed);
        loop {
            let next = current.saturating_sub(bytes);
            match self.downloaded.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
    }

    fn set_total(&self, total: u64) {
        self.total.store(total, Ordering::Relaxed);
    }

    /// 换候选地址前把已计入的字节清零。
    fn reset(&self) {
        self.downloaded.store(0, Ordering::Relaxed);
        self.total.store(0, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> ProgressSnapshot {
        ProgressSnapshot {
            downloaded: self.downloaded.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            cancelled: self.cancel.load(Ordering::Relaxed),
        }
    }

    /// 请求取消：下载循环在下一个检查点退出。
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn check_cancelled(&self) -> Result<(), DownloadError> {
        if self.is_cancelled() {
            Err(DownloadError::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct SourceProbe {
    /// 文件总大小，未知时为 `None`。
    total: Option<u64>,
    /// 源是否支持 Range 分片。
    ranges: bool,
}

pub struct DownloadEngine {
    client: reqwest::Client,
    options: DownloadOptions,
}

impl DownloadEngine {
    /// 与下载共用代理/超时配置的 HTTP 客户端，供 WebDAV 上传等附加步骤复用。
    pub(crate) fn client(&self) -> &reqwest::Client {
        &self.client
    }

    pub fn new(proxy_url: &str, timeout_secs: u64, options: DownloadOptions) -> Self {
        let timeout = Duration::from_secs(timeout_secs.clamp(1, 600));
        let mut builder = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(timeout_secs.clamp(1, 30)))
            .pool_idle_timeout(Duration::from_secs(120))
            .pool_max_idle_per_host(16)
            .tcp_keepalive(Duration::from_secs(30))
            .user_agent(USER_AGENT);
        if !proxy_url.trim().is_empty()
            && let Ok(proxy) = reqwest::Proxy::all(proxy_url.trim())
        {
            builder = builder.proxy(proxy);
        }
        Self {
            client: builder.build().unwrap_or_default(),
            options,
        }
    }

    /// 下载小文件（封面等）到内存，带体积上限。
    pub async fn fetch_bytes(
        &self,
        url: &str,
        headers: &[(String, String)],
        limit: u64,
    ) -> Result<Vec<u8>, DownloadError> {
        let mut response = apply_headers(self.client.get(url), headers)
            .send()
            .await
            .map_err(|error| DownloadError::Network(error.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(DownloadError::Http {
                status: status.as_u16(),
            });
        }
        if let Some(length) = response.content_length()
            && length > limit
        {
            return Err(DownloadError::Other(format!(
                "响应体积 {length} 超过上限 {limit}"
            )));
        }
        let mut buffer = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| DownloadError::Network(error.to_string()))?
        {
            buffer.extend_from_slice(&chunk);
            if buffer.len() as u64 > limit {
                return Err(DownloadError::Other(format!("响应体积超过上限 {limit}")));
            }
        }
        Ok(buffer)
    }

    /// 下载到 `req.dest`。先写同目录临时文件，成功后原子改名，
    /// 避免中断时留下半截文件冒充成品。
    pub async fn download(
        &self,
        req: &DownloadRequest,
        progress: &Arc<DownloadProgress>,
    ) -> Result<u64, DownloadError> {
        if req.url.trim().is_empty() {
            return Err(DownloadError::Other("缺少下载地址".to_string()));
        }
        if let Some(parent) = req.dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| DownloadError::Io(error.to_string()))?;
        }

        let part_path = staging_path(&req.dest);
        // 参考 MusicBot-Go 的候选地址机制：同一个资源可能有多个可用地址
        // （音源注入的备用 CDN，或网易云 CDN 的 m8/m801/m804/m704 常常 403，
        // 需要换成 m7/m701）。
        let candidates = candidate_download_urls(&req.url, &req.candidate_urls);
        let mut last_error = DownloadError::Other("没有可用的下载地址".to_string());
        for (index, url) in candidates.iter().enumerate() {
            if index > 0 {
                tracing::debug!(
                    "download retrying with candidate url: {} (for {})",
                    url,
                    req.dest.display()
                );
                progress.reset();
            }

            // 严格分片音源：不探测、不降级，每个请求都必须是有界 Range。
            let outcome = if req.max_chunk_size > 0 {
                match req.expected_size.filter(|&total| total > 0) {
                    Some(total) => {
                        progress.set_total(total);
                        self.download_chunked(req, url, total, &part_path, progress)
                            .await
                    }
                    None => Err(DownloadError::Other(
                        "严格分片下载需要音源声明的文件大小".to_string(),
                    )),
                }
            } else {
                let probe = self.probe(url, &req.headers).await;
                let total = probe.total.or(req.expected_size);
                progress.set_total(total.unwrap_or(0));
                self.download_to(req, url, probe, total, &part_path, progress)
                    .await
            };

            match outcome {
                Ok(written) => {
                    if self.options.verify_size
                        && let Some(expected) = req.expected_size
                        && let Err(error) = verify_size(expected, written, req.size_is_advisory)
                    {
                        let _ = tokio::fs::remove_file(&part_path).await;
                        last_error = error;
                        continue;
                    }
                    if let Some(expected_md5) = &req.md5
                        && let Err(error) = verify_md5(&part_path, expected_md5).await
                    {
                        let _ = tokio::fs::remove_file(&part_path).await;
                        last_error = error;
                        continue;
                    }
                    tokio::fs::rename(&part_path, &req.dest)
                        .await
                        .map_err(|error| {
                            let _ = std::fs::remove_file(&part_path);
                            DownloadError::Io(error.to_string())
                        })?;
                    return Ok(written);
                }
                Err(error) => {
                    let _ = tokio::fs::remove_file(&part_path).await;
                    last_error = error;
                }
            }
        }
        Err(last_error)
    }

    async fn download_to(
        &self,
        req: &DownloadRequest,
        url: &str,
        probe: SourceProbe,
        total: Option<u64>,
        dest: &Path,
        progress: &Arc<DownloadProgress>,
    ) -> Result<u64, DownloadError> {
        let multipart_ready = self.options.multipart
            && probe.ranges
            && total.is_some_and(|total| total >= self.options.multipart_min_size);
        if multipart_ready {
            match self
                .download_multipart(req, url, total.unwrap_or_default(), dest, progress)
                .await
            {
                Ok(written) => return Ok(written),
                Err(error) if error.is_retryable() => {
                    tracing::debug!(
                        "multipart download failed for {}, falling back to single connection: {}",
                        req.dest.display(),
                        error
                    );
                    let _ = tokio::fs::remove_file(dest).await;
                }
                Err(error) => return Err(error),
            }
        }
        self.download_single(req, url, dest, progress).await
    }

    /// 探测源能力：优先 HEAD，失败再用 `Range: bytes=0-0` 兜底。
    async fn probe(&self, url: &str, headers: &[(String, String)]) -> SourceProbe {
        if let Ok(response) = apply_headers(self.client.head(url), headers).send().await
            && response.status().is_success()
        {
            let headers = response.headers().clone();
            let total = content_length(&headers).or_else(|| response.content_length());
            if total.is_some() {
                return SourceProbe {
                    total,
                    ranges: supports_ranges(&headers),
                };
            }
        }

        // HEAD 拿不到长度时用 GET + `Range: bytes=0-0`：响应体只有 1 字节，
        // 却能从 Content-Range 同时确认总长度与分片能力。
        match apply_headers(self.client.get(url), headers)
            .header(RANGE, "bytes=0-0")
            .send()
            .await
        {
            Ok(response) if response.status().as_u16() == 206 => {
                let total = response
                    .headers()
                    .get(CONTENT_RANGE)
                    .and_then(|value| value.to_str().ok())
                    .and_then(parse_content_range_total);
                SourceProbe {
                    total,
                    ranges: true,
                }
            }
            Ok(response) if response.status().is_success() => SourceProbe {
                total: response.content_length(),
                ranges: false,
            },
            _ => SourceProbe::default(),
        }
    }

    async fn download_single(
        &self,
        req: &DownloadRequest,
        url: &str,
        dest: &Path,
        progress: &Arc<DownloadProgress>,
    ) -> Result<u64, DownloadError> {
        let attempts = self.options.max_retries.max(1);
        let mut last_error = DownloadError::Other("下载失败".to_string());
        for attempt in 0..attempts {
            if attempt > 0 {
                sleep_before_retry(attempt).await;
            }
            progress.check_cancelled()?;
            match self.single_attempt(req, url, dest, progress).await {
                Ok(written) => return Ok(written),
                Err(error) if error.is_retryable() && attempt + 1 < attempts => {
                    tracing::debug!(
                        "download attempt {} for {} failed: {}",
                        attempt + 1,
                        req.dest.display(),
                        error
                    );
                    last_error = error;
                    let _ = tokio::fs::remove_file(dest).await;
                    progress.sub(progress.snapshot().downloaded);
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error)
    }

    async fn single_attempt(
        &self,
        req: &DownloadRequest,
        url: &str,
        dest: &Path,
        progress: &Arc<DownloadProgress>,
    ) -> Result<u64, DownloadError> {
        let mut response = apply_headers(self.client.get(url), &req.headers)
            .send()
            .await
            .map_err(|error| DownloadError::Network(error.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(DownloadError::Http {
                status: status.as_u16(),
            });
        }
        let declared_length =
            content_length(response.headers()).or_else(|| response.content_length());
        let mut file = tokio::fs::File::create(dest)
            .await
            .map_err(|error| DownloadError::Io(error.to_string()))?;
        let mut written: u64 = 0;
        loop {
            progress.check_cancelled()?;
            let chunk = response
                .chunk()
                .await
                .map_err(|error| DownloadError::Network(error.to_string()))?;
            let Some(chunk) = chunk else { break };
            file.write_all(&chunk)
                .await
                .map_err(|error| DownloadError::Io(error.to_string()))?;
            written += chunk.len() as u64;
            progress.add(chunk.len() as u64);
        }
        file.flush()
            .await
            .map_err(|error| DownloadError::Io(error.to_string()))?;
        drop(file);

        if self.options.verify_size
            && let Some(declared) = declared_length
        {
            verify_size(declared, written, req.size_is_advisory)?;
        }
        if let Some(expected_md5) = &req.md5 {
            verify_md5(dest, expected_md5).await?;
        }
        Ok(written)
    }

    async fn download_multipart(
        &self,
        req: &DownloadRequest,
        url: &str,
        total: u64,
        dest: &Path,
        progress: &Arc<DownloadProgress>,
    ) -> Result<u64, DownloadError> {
        let parts = plan_ranges(total, self.options.concurrency);
        if parts.len() <= 1 {
            return self.download_single(req, url, dest, progress).await;
        }
        self.download_ranges(req, url, parts, total, dest, progress)
            .await
    }

    /// 严格有界 Range 分片：音源声明 `max_chunk_size` 时必须走这条路径。
    ///
    /// 与普通分片的区别是「上限优先」：宁可多切几片也不发超过上限的 Range，
    /// 而且失败时不回退单连接（plain GET 在 googlevideo 上必定 403）。
    async fn download_chunked(
        &self,
        req: &DownloadRequest,
        url: &str,
        total: u64,
        dest: &Path,
        progress: &Arc<DownloadProgress>,
    ) -> Result<u64, DownloadError> {
        let parts = plan_bounded_ranges(total, self.options.concurrency, req.max_chunk_size);
        if parts.is_empty() {
            return Err(DownloadError::Other(
                "严格分片下载需要有效的分片上限".to_string(),
            ));
        }
        self.download_ranges(req, url, parts, total, dest, progress)
            .await
    }

    /// 按给定区间并发下载并写入同一个文件，最后核对总长度与 MD5。
    async fn download_ranges(
        &self,
        req: &DownloadRequest,
        url: &str,
        parts: Vec<(u64, u64)>,
        total: u64,
        dest: &Path,
        progress: &Arc<DownloadProgress>,
    ) -> Result<u64, DownloadError> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(dest)
            .map_err(|error| DownloadError::Io(error.to_string()))?;
        file.set_len(total)
            .map_err(|error| DownloadError::Io(error.to_string()))?;
        let file = Arc::new(file);

        let attempts = self.options.max_retries.max(1);
        let mut tasks = tokio::task::JoinSet::new();
        for (start, end) in parts {
            tasks.spawn(download_part(
                self.client.clone(),
                url.to_string(),
                req.headers.clone(),
                start,
                end,
                Arc::clone(&file),
                Arc::clone(progress),
                attempts,
            ));
        }

        let mut first_error: Option<DownloadError> = None;
        while let Some(result) = tasks.join_next().await {
            let error = match result {
                Ok(Ok(())) => continue,
                Ok(Err(error)) => error,
                Err(join_error) => DownloadError::Other(join_error.to_string()),
            };
            if first_error.is_none() {
                first_error = Some(error);
                // 出错后不再等待剩余分片，直接收敛。
                tasks.abort_all();
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        drop(file);

        let written = progress.snapshot().downloaded;
        if self.options.verify_size && written != total {
            return Err(DownloadError::Integrity(format!(
                "分片合并后大小不一致: 实际 {written} 字节，预期 {total} 字节"
            )));
        }
        if let Some(expected_md5) = &req.md5 {
            verify_md5(dest, expected_md5).await?;
        }
        Ok(written)
    }
}

/// 单分片下载：独立请求 + 独立重试，按字节偏移写入共享文件。
#[allow(clippy::too_many_arguments)]
async fn download_part(
    client: reqwest::Client,
    url: String,
    headers: Vec<(String, String)>,
    start: u64,
    end: u64,
    file: Arc<std::fs::File>,
    progress: Arc<DownloadProgress>,
    attempts: u32,
) -> Result<(), DownloadError> {
    let expected = end - start + 1;
    let mut last_error = DownloadError::Other("分片下载失败".to_string());
    for attempt in 0..attempts {
        if attempt > 0 {
            sleep_before_retry(attempt).await;
        }
        progress.check_cancelled()?;
        let request =
            apply_headers(client.get(&url), &headers).header(RANGE, format!("bytes={start}-{end}"));
        match part_attempt(&progress, file.as_ref(), request, start, expected).await {
            Ok(()) => return Ok(()),
            // 音源声称支持 Range 却返回整份内容：立即回退单连接，不重试。
            Err(error @ DownloadError::RangesUnsupported) => return Err(error),
            Err(error) if error.is_retryable() && attempt + 1 < attempts => {
                tracing::debug!("part {start}-{end} attempt {} failed: {error}", attempt + 1);
                last_error = error;
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error)
}

async fn part_attempt(
    progress: &Arc<DownloadProgress>,
    file: &std::fs::File,
    request: reqwest::RequestBuilder,
    start: u64,
    expected: u64,
) -> Result<(), DownloadError> {
    let mut response = request
        .send()
        .await
        .map_err(|error| DownloadError::Network(error.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(DownloadError::Http {
            status: status.as_u16(),
        });
    }
    // 请求了 Range 却收到 200：说明源忽略了 Range，分片结果不可信。
    if status.as_u16() == 200 && start > 0 {
        return Err(DownloadError::RangesUnsupported);
    }
    let mut offset = start;
    let mut part_written: u64 = 0;
    let result = loop {
        if let Err(error) = progress.check_cancelled() {
            break Err(error);
        }
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break Ok(()),
            Err(error) => break Err(DownloadError::Network(error.to_string())),
        };
        if let Err(error) = write_at(file, offset, &chunk) {
            break Err(DownloadError::Io(error));
        }
        offset += chunk.len() as u64;
        part_written += chunk.len() as u64;
        progress.add(chunk.len() as u64);
    };
    if let Err(error) = result {
        // 重试前置零：撤回本分片已计入的字节，下一次尝试从该偏移重写。
        progress.sub(part_written);
        return Err(error);
    }
    if part_written != expected {
        progress.sub(part_written);
        return Err(DownloadError::Integrity(format!(
            "分片 {start} 字节数不符: 实际 {part_written}，预期 {expected}"
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn write_at(file: &std::fs::File, offset: u64, data: &[u8]) -> Result<(), String> {
    use std::os::unix::fs::FileExt;
    file.write_at(data, offset)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(windows)]
fn write_at(file: &std::fs::File, offset: u64, data: &[u8]) -> Result<(), String> {
    use std::os::windows::fs::FileExt;
    file.seek_write(data, offset)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(not(any(unix, windows)))]
fn write_at(_file: &std::fs::File, _offset: u64, _data: &[u8]) -> Result<(), String> {
    Err("当前平台不支持分片写入".to_string())
}

fn apply_headers(
    mut request: reqwest::RequestBuilder,
    headers: &[(String, String)],
) -> reqwest::RequestBuilder {
    for (name, value) in headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            request = request.header(name, value);
        }
    }
    request
}

/// 完整性校验：默认要求字节数完全一致，声明值不可靠时只拒绝偏短。
fn verify_size(expected: u64, actual: u64, advisory: bool) -> Result<(), DownloadError> {
    if expected == 0 {
        return Ok(());
    }
    let ok = if advisory {
        actual >= expected
    } else {
        actual == expected
    };
    if ok {
        return Ok(());
    }
    if advisory {
        Err(DownloadError::Integrity(format!(
            "文件偏短: 实际 {actual} 字节，预期至少 {expected} 字节"
        )))
    } else {
        Err(DownloadError::Integrity(format!(
            "文件大小不符: 实际 {actual} 字节，预期 {expected} 字节"
        )))
    }
}

/// 计算文件 MD5 并与预期值比对。预期值为空时跳过。
async fn verify_md5(path: &std::path::Path, expected: &str) -> Result<(), DownloadError> {
    if expected.is_empty() {
        return Ok(());
    }
    use md5::{Digest, Md5};
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| DownloadError::Io(e.to_string()))?;
    let mut hasher = Md5::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        use tokio::io::AsyncReadExt;
        let n = file
            .read(&mut buf)
            .await
            .map_err(|e| DownloadError::Io(e.to_string()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let actual = format!("{:x}", hasher.finalize());
    if actual == expected.to_ascii_lowercase() {
        Ok(())
    } else {
        Err(DownloadError::Integrity(format!(
            "MD5 不匹配: 实际 {actual}, 预期 {expected}"
        )))
    }
}

/// 把总长度按并发数切成连续区间，最后一片吃掉余数。
///
/// 分片粒度不低于 1MB：小文件即使配置了高并发也只发一个请求，
/// 避免为几百 KB 的文件打出十几个 Range 请求。
fn plan_ranges(total: u64, concurrency: usize) -> Vec<(u64, u64)> {
    if total == 0 {
        return Vec::new();
    }
    const MIN_PART_SIZE: u64 = 1024 * 1024;
    let max_useful = (total / MIN_PART_SIZE).max(1);
    let workers = (concurrency.clamp(1, 16) as u64).min(max_useful);
    let chunk = total.div_ceil(workers);
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < total {
        let end = (start + chunk - 1).min(total - 1);
        ranges.push((start, end));
        start = end + 1;
    }
    ranges
}

/// 严格有界分片：每片不超过 `max_chunk`，片数由并发数决定但允许超出。
///
/// googlevideo 会对超过单 IP 上限的 Range 直接 403，因此这里上限优先：
/// 先按并发数均分，均分结果超过上限就退回上限，宁可多发几次请求。
fn plan_bounded_ranges(total: u64, concurrency: usize, max_chunk: u64) -> Vec<(u64, u64)> {
    if total == 0 || max_chunk == 0 {
        return Vec::new();
    }
    let workers = concurrency.clamp(1, 16) as u64;
    let mut part_size = total / workers;
    if part_size == 0 || part_size > max_chunk {
        part_size = max_chunk;
    }
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < total {
        let end = (start + part_size - 1).min(total - 1);
        ranges.push((start, end));
        start = end + 1;
    }
    ranges
}

fn supports_ranges(headers: &HeaderMap) -> bool {
    headers
        .get(ACCEPT_RANGES)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("bytes"))
}

fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
}

/// 解析 `Content-Range: bytes 0-0/12345` 中的总长度。
fn parse_content_range_total(value: &str) -> Option<u64> {
    value.rsplit('/').next()?.trim().parse::<u64>().ok()
}

/// 同一资源的候选下载地址，按尝试顺序返回。
///
/// 顺序：音源主地址 → 音源注入的备用 CDN → 各自的网易云节点改写。
/// 网易云 CDN 的 m8/m801/m804/m704 节点经常返回 403，MusicBot-Go 的做法是
/// 统一改写为对应的 m7/m701 节点，这里作为最后一道兜底。
pub fn candidate_download_urls(primary: &str, extra: &[String]) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();
    let mut push = |value: &str| {
        let value = value.trim();
        if value.is_empty() || candidates.iter().any(|existing| existing == value) {
            return;
        }
        candidates.push(value.to_string());
        let rewritten = rewrite_netease_host(value);
        if rewritten != value && !candidates.contains(&rewritten) {
            candidates.push(rewritten);
        }
    };
    push(primary);
    for url in extra {
        push(url);
    }
    candidates
}

/// 参照 MusicBot-Go `neteaseHostReplacer` 的节点替换表。
pub fn rewrite_netease_host(url: &str) -> String {
    const REPLACEMENTS: [(&str, &str); 4] = [
        ("m801.", "m701."),
        ("m804.", "m701."),
        ("m704.", "m701."),
        ("m8.", "m7."),
    ];
    let (scheme, rest) = match url.split_once("://") {
        Some((scheme, rest)) => (Some(scheme), rest),
        None => (None, url),
    };
    let (host, tail) = match rest.find('/') {
        Some(index) => rest.split_at(index),
        None => (rest, ""),
    };
    let rewritten = REPLACEMENTS
        .iter()
        .find_map(|(from, to)| host.starts_with(from).then(|| host.replacen(from, to, 1)));
    match (rewritten, scheme) {
        (Some(host), Some(scheme)) => format!("{scheme}://{host}{tail}"),
        (Some(host), None) => format!("{host}{tail}"),
        (None, _) => url.to_string(),
    }
}

fn staging_path(dest: &Path) -> PathBuf {
    let name = dest
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".to_string());
    dest.with_file_name(format!(".{name}.part"))
}

/// 指数退避 + 抖动，避免多个分片同时重试。
async fn sleep_before_retry(attempt: u32) {
    let backoff = 400u64.saturating_mul(1 << attempt.min(5)).min(8_000);
    let jitter = (retry_jitter_seed() % 250) as u64;
    tokio::time::sleep(Duration::from_millis(backoff + jitter)).await;
}

fn retry_jitter_seed() -> u128 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos() as u128)
        .unwrap_or(0);
    nanos.wrapping_mul(31).wrapping_add(seq as u128)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_cover_the_whole_file_without_gaps() {
        let total = 10 * 1024 * 1024 + 7;
        let ranges = plan_ranges(total, 4);

        assert_eq!(ranges.len(), 4);
        assert_eq!(ranges.first().unwrap().0, 0);
        assert_eq!(ranges.last().unwrap().1, total - 1);
        for pair in ranges.windows(2) {
            assert_eq!(pair[0].1 + 1, pair[1].0, "分片之间不允许有空隙");
        }
        let covered: u64 = ranges.iter().map(|(start, end)| end - start + 1).sum();
        assert_eq!(covered, total);
    }

    #[test]
    fn tiny_files_stay_single_part() {
        assert_eq!(plan_ranges(1024, 8), vec![(0, 1023)]);
    }

    #[test]
    fn bounded_ranges_never_exceed_the_configured_cap() {
        let total = 5 * 1024 * 1024 + 3;
        let cap = 1024 * 1024;
        let ranges = plan_bounded_ranges(total, 4, cap);

        assert!(ranges.iter().all(|(start, end)| end - start < cap));
        assert_eq!(ranges.first().unwrap().0, 0);
        assert_eq!(ranges.last().unwrap().1, total - 1);
        for pair in ranges.windows(2) {
            assert_eq!(pair[0].1 + 1, pair[1].0, "分片之间不允许有空隙");
        }
        let covered: u64 = ranges.iter().map(|(start, end)| end - start + 1).sum();
        assert_eq!(covered, total);
    }

    #[test]
    fn bounded_ranges_use_the_cap_when_workers_cannot_cover_the_file() {
        // 并发 4 但上限很小：片数会超过并发数，而不是发大 Request。
        let ranges = plan_bounded_ranges(1000, 4, 100);
        assert_eq!(ranges.len(), 10);
        assert_eq!(ranges.first().unwrap(), &(0, 99));
        assert_eq!(ranges.last().unwrap(), &(900, 999));
    }

    #[test]
    fn bounded_ranges_are_empty_without_a_cap_or_size() {
        assert!(plan_bounded_ranges(0, 4, 1024).is_empty());
        assert!(plan_bounded_ranges(1024, 4, 0).is_empty());
    }

    #[test]
    fn size_verification_rejects_truncated_downloads() {
        assert!(verify_size(2048, 2048, false).is_ok());
        let error = verify_size(2048, 1024, false).unwrap_err();
        assert!(matches!(error, DownloadError::Integrity(_)));
    }

    #[test]
    fn advisory_size_allows_longer_but_not_shorter_files() {
        assert!(verify_size(2048, 2050, true).is_ok());
        assert!(verify_size(2048, 4096, true).is_ok());
        assert!(verify_size(2048, 1000, true).is_err());
    }

    #[test]
    fn content_range_parser_reads_total_length() {
        assert_eq!(parse_content_range_total("bytes 0-0/12345"), Some(12345));
        assert_eq!(parse_content_range_total("bytes */12345"), Some(12345));
        assert_eq!(parse_content_range_total("bytes 0-0/*"), None);
    }

    #[test]
    fn staging_file_stays_next_to_the_destination() {
        let dest = PathBuf::from("/music/voicefox/周杰伦 - 晴天.mp3");
        assert_eq!(
            staging_path(&dest),
            PathBuf::from("/music/voicefox/.周杰伦 - 晴天.mp3.part")
        );
    }

    #[test]
    fn netease_nodes_are_rewritten_to_the_m7_family() {
        assert_eq!(
            rewrite_netease_host("http://m8.music.126.net/a/b.mp3?id=1"),
            "http://m7.music.126.net/a/b.mp3?id=1"
        );
        assert_eq!(
            rewrite_netease_host("https://m801.music.126.net/x.flac"),
            "https://m701.music.126.net/x.flac"
        );
        assert_eq!(
            rewrite_netease_host("https://m804.music.126.net/x.flac"),
            "https://m701.music.126.net/x.flac"
        );
        // 其它音源地址保持原样。
        assert_eq!(
            rewrite_netease_host("https://cdn.example.com/m8.song.flac"),
            "https://cdn.example.com/m8.song.flac"
        );
    }

    #[test]
    fn candidates_put_the_original_url_first() {
        assert_eq!(
            candidate_download_urls("http://m8.music.126.net/a.mp3", &[]),
            vec![
                "http://m8.music.126.net/a.mp3".to_string(),
                "http://m7.music.126.net/a.mp3".to_string()
            ]
        );
        assert_eq!(
            candidate_download_urls("https://cdn.example.com/a.flac", &[]),
            vec!["https://cdn.example.com/a.flac".to_string()]
        );
        assert!(candidate_download_urls("   ", &[]).is_empty());
    }

    #[test]
    fn platform_candidates_are_kept_in_order_and_deduplicated() {
        let extra = vec![
            "http://cdn2.example.com/a.flac".to_string(),
            // 与主地址重复、以及空串都必须被过滤掉。
            "http://cdn1.example.com/a.flac".to_string(),
            "   ".to_string(),
            // 备用地址同样享受网易云节点改写兜底。
            "http://m801.music.126.net/b.mp3".to_string(),
        ];
        assert_eq!(
            candidate_download_urls("http://cdn1.example.com/a.flac", &extra),
            vec![
                "http://cdn1.example.com/a.flac".to_string(),
                "http://cdn2.example.com/a.flac".to_string(),
                "http://m801.music.126.net/b.mp3".to_string(),
                "http://m701.music.126.net/b.mp3".to_string(),
            ]
        );
    }

    #[test]
    fn integrity_errors_are_not_retried() {
        assert!(!DownloadError::Integrity("bad".to_string()).is_retryable());
        assert!(!DownloadError::Cancelled.is_retryable());
        assert!(DownloadError::Network("timeout".to_string()).is_retryable());
        assert!(DownloadError::Http { status: 503 }.is_retryable());
        assert!(!DownloadError::Http { status: 404 }.is_retryable());
    }

    #[test]
    fn progress_ratio_handles_unknown_totals() {
        let progress = DownloadProgress::default();
        assert_eq!(progress.snapshot().ratio(), None);
        progress.set_total(200);
        progress.add(50);
        assert_eq!(progress.snapshot().ratio(), Some(0.25));
        progress.sub(200);
        assert_eq!(progress.snapshot().downloaded, 0);
    }

    use crate::download::test_support::{RangeMode, fake_audio, spawn_test_server, temp_dir};

    #[tokio::test]
    async fn multipart_download_reassembles_exact_bytes() {
        let body = fake_audio(3 * 1024 * 1024);
        let Some(server) = spawn_test_server(body.clone(), None, RangeMode::Supported).await else {
            return;
        };
        let url = server.audio_url;
        let dir = temp_dir("multipart");
        let dest = dir.join("song.flac");
        let engine = DownloadEngine::new(
            "",
            30,
            DownloadOptions {
                concurrency: 4,
                multipart: true,
                multipart_min_size: 1024,
                max_retries: 2,
                verify_size: true,
            },
        );
        let progress = DownloadProgress::new();
        let request = DownloadRequest {
            url,
            headers: Vec::new(),
            dest: dest.clone(),
            expected_size: Some(body.len() as u64),
            size_is_advisory: false,
            md5: None,
            candidate_urls: Vec::new(),
            max_chunk_size: 0,
        };

        let written = engine.download(&request, &progress).await.unwrap();

        assert_eq!(written as usize, body.len());
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert!(!staging_path(&dest).exists(), "临时文件必须被清理");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn single_connection_download_works_without_range_support() {
        let body = fake_audio(512 * 1024);
        let Some(server) = spawn_test_server(body.clone(), None, RangeMode::Unsupported).await
        else {
            return;
        };
        let url = server.audio_url;
        let dir = temp_dir("single");
        let dest = dir.join("song.flac");
        let engine = DownloadEngine::new("", 30, DownloadOptions::default());
        let progress = DownloadProgress::new();
        let request = DownloadRequest {
            url,
            headers: Vec::new(),
            dest: dest.clone(),
            expected_size: Some(body.len() as u64),
            size_is_advisory: false,
            md5: None,
            candidate_urls: Vec::new(),
            max_chunk_size: 0,
        };

        let written = engine.download(&request, &progress).await.unwrap();

        assert_eq!(written as usize, body.len());
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 源声称 Accept-Ranges 却对 Range 请求返回整份内容时，必须回退单连接下载
    /// 而不是把整份文件写到分片偏移上导致损坏。
    #[tokio::test]
    async fn multipart_falls_back_when_range_is_ignored() {
        let body = fake_audio(2 * 1024 * 1024);
        let Some(server) = spawn_test_server(body.clone(), None, RangeMode::Lying).await else {
            return;
        };
        let url = server.audio_url;
        let dir = temp_dir("lying-range");
        let dest = dir.join("song.flac");
        let engine = DownloadEngine::new(
            "",
            30,
            DownloadOptions {
                concurrency: 4,
                multipart: true,
                multipart_min_size: 1024,
                max_retries: 1,
                verify_size: true,
            },
        );
        let progress = DownloadProgress::new();
        let request = DownloadRequest {
            url,
            headers: Vec::new(),
            dest: dest.clone(),
            expected_size: Some(body.len() as u64),
            size_is_advisory: false,
            md5: None,
            candidate_urls: Vec::new(),
            max_chunk_size: 0,
        };

        let written = engine.download(&request, &progress).await.unwrap();

        assert_eq!(written as usize, body.len());
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn truncated_download_is_rejected_and_leaves_no_file() {
        let body = fake_audio(256 * 1024);
        let Some(server) = spawn_test_server(body.clone(), None, RangeMode::Supported).await else {
            return;
        };
        let url = server.audio_url;
        let dir = temp_dir("truncated");
        let dest = dir.join("song.flac");
        let engine = DownloadEngine::new("", 30, DownloadOptions::default());
        let progress = DownloadProgress::new();
        // 音源声明的大小比实际内容大：必须判为完整性失败而不是静默成功。
        let request = DownloadRequest {
            url,
            headers: Vec::new(),
            dest: dest.clone(),
            expected_size: Some(body.len() as u64 + 4096),
            size_is_advisory: false,
            md5: None,
            candidate_urls: Vec::new(),
            max_chunk_size: 0,
        };

        let error = engine.download(&request, &progress).await.unwrap_err();

        assert!(matches!(error, DownloadError::Integrity(_)), "got {error}");
        assert!(!dest.exists());
        assert!(!staging_path(&dest).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn strict_chunked_download_uses_bounded_ranges_only() {
        let cap = 128 * 1024;
        let body = fake_audio(1024 * 1024 + 7);
        let Some(server) = spawn_test_server(body.clone(), None, RangeMode::BoundedOnly(cap)).await
        else {
            return;
        };
        let dir = temp_dir("chunked");
        let dest = dir.join("song.flac");
        let engine = DownloadEngine::new("", 30, DownloadOptions::default());
        let progress = DownloadProgress::new();
        let request = DownloadRequest {
            url: server.audio_url,
            headers: Vec::new(),
            dest: dest.clone(),
            expected_size: Some(body.len() as u64),
            size_is_advisory: false,
            md5: None,
            candidate_urls: Vec::new(),
            max_chunk_size: cap as u64,
        };

        // 服务端拒绝 HEAD 与不带 Range 的 GET，只有严格有界分片能拿到内容。
        let written = engine.download(&request, &progress).await.unwrap();

        assert_eq!(written as usize, body.len());
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert!(!staging_path(&dest).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn strict_chunked_download_requires_a_declared_size() {
        let body = fake_audio(256 * 1024);
        let Some(server) = spawn_test_server(body, None, RangeMode::BoundedOnly(64 * 1024)).await
        else {
            return;
        };
        let dir = temp_dir("chunked-no-size");
        let dest = dir.join("song.flac");
        let engine = DownloadEngine::new("", 30, DownloadOptions::default());
        let progress = DownloadProgress::new();
        let request = DownloadRequest {
            url: server.audio_url,
            headers: Vec::new(),
            dest: dest.clone(),
            expected_size: None,
            size_is_advisory: false,
            md5: None,
            candidate_urls: Vec::new(),
            max_chunk_size: 64 * 1024,
        };

        let error = engine.download(&request, &progress).await.unwrap_err();

        // 没有大小就算不出有界 Range，绝不能退回 plain GET（那是这些 CDN 的 403 来源）。
        assert!(matches!(error, DownloadError::Other(_)), "got {error}");
        assert!(!dest.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn strict_chunked_download_never_falls_back_to_plain_get() {
        // 服务端只放行 ≤64KB 的 Range，而音源声明 1MB 上限：
        // 分片会被 403，此时必须整体失败，而不是改用 plain GET 悄悄成功。
        let body = fake_audio(512 * 1024);
        let Some(server) = spawn_test_server(body, None, RangeMode::BoundedOnly(64 * 1024)).await
        else {
            return;
        };
        let dir = temp_dir("chunked-too-big");
        let dest = dir.join("song.flac");
        let engine = DownloadEngine::new("", 30, DownloadOptions::default());
        let progress = DownloadProgress::new();
        let request = DownloadRequest {
            url: server.audio_url,
            headers: Vec::new(),
            dest: dest.clone(),
            expected_size: Some(512 * 1024),
            size_is_advisory: false,
            md5: None,
            candidate_urls: Vec::new(),
            max_chunk_size: 1024 * 1024,
        };

        let error = engine.download(&request, &progress).await.unwrap_err();

        assert!(
            matches!(error, DownloadError::Http { status: 403 }),
            "got {error}"
        );
        assert!(!dest.exists());
        assert!(!staging_path(&dest).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn platform_candidate_url_is_used_when_the_primary_fails() {
        let body = fake_audio(512 * 1024);
        let Some(server) = spawn_test_server(body.clone(), None, RangeMode::Supported).await else {
            return;
        };
        let dir = temp_dir("candidate");
        let dest = dir.join("song.flac");
        let engine = DownloadEngine::new("", 30, DownloadOptions::default());
        let progress = DownloadProgress::new();
        let request = DownloadRequest {
            // 主地址固定 403，音源注入的备用地址可用。
            url: server.denied_url.clone(),
            headers: Vec::new(),
            dest: dest.clone(),
            expected_size: Some(body.len() as u64),
            size_is_advisory: false,
            md5: None,
            candidate_urls: vec![server.audio_url.clone()],
            max_chunk_size: 0,
        };

        let written = engine.download(&request, &progress).await.unwrap();

        assert_eq!(written as usize, body.len());
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn cancelled_download_stops_before_writing() {
        let body = fake_audio(1024 * 1024);
        let Some(server) = spawn_test_server(body, None, RangeMode::Supported).await else {
            return;
        };
        let url = server.audio_url;
        let dir = temp_dir("cancelled");
        let dest = dir.join("song.flac");
        let engine = DownloadEngine::new("", 30, DownloadOptions::default());
        let progress = DownloadProgress::new();
        progress.cancel();
        let request = DownloadRequest {
            url,
            headers: Vec::new(),
            dest: dest.clone(),
            expected_size: None,
            size_is_advisory: false,
            md5: None,
            candidate_urls: Vec::new(),
            max_chunk_size: 0,
        };

        let error = engine.download(&request, &progress).await.unwrap_err();

        assert!(matches!(error, DownloadError::Cancelled));
        assert!(!dest.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
