use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use reqwest::header::{ACCEPT, REFERER};

/// 封面状态
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoverState {
    Empty,
    Loading,
    Ready,
    Unavailable(String),
}

pub struct CoverService {
    client: reqwest::Client,
    image_path: RwLock<Option<String>>,
    state: RwLock<CoverState>,
    request_id: AtomicU64,
    /// 封面版本号，每次加载新封面时递增。display_kitty 只在新版本时传输图片。
    display_gen: AtomicU64,
    /// 已显示到终端的版本号
    displayed_gen: AtomicU64,
}

impl CoverService {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(12))
            .user_agent("voicefox/0.1")
            .build()
            .unwrap_or_default();
        Self {
            client,
            image_path: RwLock::new(None),
            state: RwLock::new(CoverState::Empty),
            request_id: AtomicU64::new(0),
            display_gen: AtomicU64::new(0),
            displayed_gen: AtomicU64::new(0),
        }
    }

    pub fn clear(&self) {
        self.request_id.fetch_add(1, Ordering::SeqCst);
        *self.image_path.write().unwrap() = None;
        *self.state.write().unwrap() = CoverState::Empty;
        // 清除终端中的图片
        self.clear_kitty_image();
    }

    fn clear_kitty_image(&self) {
        use std::io::Write;
        // 删除所有 Kitty 图片
        let _ = std::io::stdout().write_all(b"\x1b_Ga=d\x1b\\");
        let _ = std::io::stdout().flush();
    }

    pub fn has_image(&self) -> bool {
        self.image_path.read().unwrap().is_some()
    }

    pub fn state(&self) -> CoverState {
        self.state.read().unwrap().clone()
    }

    /// 获取当前封面路径
    pub fn image_path(&self) -> Option<String> {
        self.image_path.read().unwrap().clone()
    }

    pub async fn load(&self, url: Option<String>) -> Result<(), String> {
        let request_id = self.request_id.fetch_add(1, Ordering::SeqCst) + 1;
        *self.image_path.write().unwrap() = None;
        self.clear_kitty_image();

        let Some(url) = url
            .map(|url| normalize_url(&url))
            .filter(|url| !url.trim().is_empty())
        else {
            *self.state.write().unwrap() =
                CoverState::Unavailable("当前音源没有返回封面".to_string());
            return Ok(());
        };
        *self.state.write().unwrap() = CoverState::Loading;

        let mut last_error = "封面请求失败".to_string();
        let mut result_path: Option<String> = None;

        for attempt in 0..3 {
            if self.request_id.load(Ordering::SeqCst) != request_id {
                return Ok(());
            }
            match self.download_and_cache(&url).await {
                Ok(path) => {
                    result_path = Some(path);
                    break;
                }
                Err(error) => {
                    last_error = error;
                    if attempt < 2 {
                        tokio::time::sleep(Duration::from_millis(150 * (attempt + 1))).await;
                    }
                }
            }
        }

        if self.request_id.load(Ordering::SeqCst) == request_id {
            if let Some(ref path) = result_path {
                *self.image_path.write().unwrap() = Some(path.clone());
                *self.state.write().unwrap() = CoverState::Ready;
                self.display_gen.fetch_add(1, Ordering::SeqCst);
            } else {
                *self.state.write().unwrap() = CoverState::Unavailable(last_error.clone());
            }
        }

        match result_path {
            Some(_) => Ok(()),
            None => Err(last_error),
        }
    }

    /// 下载封面到本地缓存，返回缓存路径
    async fn download_and_cache(&self, url: &str) -> Result<String, String> {
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
            .join("voicefox")
            .join("covers");

        if !cache_dir.exists() {
            let _ = std::fs::create_dir_all(&cache_dir);
        }

        // 本地文件直接返回路径
        if url.starts_with('/') || url.starts_with("file://") {
            let path = url.strip_prefix("file://").unwrap_or(url);
            if std::path::Path::new(path).exists() {
                return Ok(path.to_string());
            }
            return Err("封面文件不存在".to_string());
        }

        // 远程文件：下载到缓存
        let hash = simple_hash(url.as_bytes());
        let cache_path = cache_dir.join(format!("{}.jpg", hash));

        if cache_path.exists() {
            return Ok(cache_path.to_string_lossy().to_string());
        }

        // HTTP 下载
        let mut request = self
            .client
            .get(url)
            .header(ACCEPT, "image/avif,image/webp,image/apng,image/*,*/*;q=0.8");
        if let Some(referer) = cover_referer(url) {
            request = request.header(REFERER, referer);
        }
        let bytes = request
            .send()
            .await
            .map_err(|error| error.to_string())?
            .error_for_status()
            .map_err(|error| error.to_string())?
            .bytes()
            .await
            .map_err(|error| error.to_string())?;

        let cache_path_clone = cache_path.clone();
        tokio::task::spawn_blocking(move || {
            std::fs::write(&cache_path_clone, &bytes).ok();
        })
        .await
        .ok();

        Ok(cache_path.to_string_lossy().to_string())
    }

    /// 在指定的终端区域显示封面（使用 Kitty 协议）
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        // 在 TUI 中留出空白区域（Kitty 图片会浮动在上方）
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(ratatui::style::Style::new().fg(ratatui::style::Color::DarkGray))
            .title("封面");
        block.render(area, buf);
    }

    /// 在终端中使用 Kitty 协议显示封面（必须在 terminal.draw() 之后调用）
    pub fn display_kitty(&self, area: Rect) {
        use std::io::Write;

        let current_gen = self.display_gen.load(Ordering::SeqCst);
        if current_gen == 0 || area.width == 0 || area.height == 0 {
            return;
        }
        // 如果版本没变，说明同一张图已经显示过了，跳过
        if current_gen == self.displayed_gen.load(Ordering::SeqCst) {
            return;
        }

        let path = match self.image_path.read().unwrap().clone() {
            Some(p) => p,
            None => return,
        };

        if !std::path::Path::new(&path).exists() {
            return;
        }

        // 先清除旧图片
        let _ = std::io::stdout().write_all(b"\x1b_Ga=d\x1b\\");

        // 用 viuer 在指定位置显示图片
        let config = viuer::Config {
            x: area.x as u16,
            y: area.y as i16,
            width: Some(area.width as u32),
            height: Some(area.height as u32),
            ..Default::default()
        };

        let _ = viuer::print_from_file(&path, &config);
        let _ = std::io::stdout().flush();

        self.displayed_gen.store(current_gen, Ordering::SeqCst);
    }

    /// 清除终端中的封面图片
    pub fn clear_display(&self) {
        use std::io::Write;
        self.displayed_gen.store(0, Ordering::SeqCst);
        let _ = std::io::stdout().write_all(b"\x1b_Ga=d\x1b\\");
        let _ = std::io::stdout().flush();
    }
}

fn normalize_url(url: &str) -> String {
    let url = url.trim();
    if url.starts_with("//") {
        format!("https:{url}")
    } else {
        url.to_string()
    }
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

fn simple_hash(data: &[u8]) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}
