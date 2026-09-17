//! 封面

mod layout;
mod render;
mod source;

use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

pub use layout::CoverGeometry;
pub use render::CoverRenderer;
pub use source::sweep_temp_files;

use source::CoverImage;

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
    image: RwLock<Option<CoverImage>>,
    state: RwLock<CoverState>,
    request_id: AtomicU64,
}

impl CoverService {
    pub fn new(proxy_url: &str, timeout_secs: u64) -> Self {
        let mut builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs.clamp(1, 300)))
            .user_agent("voicefox/0.1");
        if !proxy_url.trim().is_empty()
            && let Ok(proxy) = reqwest::Proxy::all(proxy_url.trim())
        {
            builder = builder.proxy(proxy);
        }
        let client = builder.build().unwrap_or_default();
        Self {
            client,
            image: RwLock::new(None),
            state: RwLock::new(CoverState::Empty),
            request_id: AtomicU64::new(0),
        }
    }

    pub fn clear(&self) {
        self.request_id.fetch_add(1, Ordering::SeqCst);
        *self.image.write().unwrap_or_else(|e| e.into_inner()) = None;
        *self.state.write().unwrap_or_else(|e| e.into_inner()) = CoverState::Empty;
    }

    /// 当前封面的本地路径
    pub fn image_path(&self) -> Option<String> {
        self.image
            .read()
            .unwrap()
            .as_ref()
            .map(|image| image.path.clone())
    }

    /// 当前封面的像素宽高比
    pub fn image_aspect(&self) -> f32 {
        self.image
            .read()
            .unwrap()
            .as_ref()
            .map_or(layout::DEFAULT_IMAGE_ASPECT, |image| image.aspect)
    }

    pub fn state(&self) -> CoverState {
        self.state.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 把封面下载到本地缓存，不改变当前显示的封面
    pub async fn cache_path(&self, url: Option<String>) -> Result<Option<String>, String> {
        let Some(url) = url
            .map(|url| source::normalize_url(&url))
            .filter(|url| !url.trim().is_empty())
        else {
            return Ok(None);
        };
        source::download_and_cache(&self.client, &url)
            .await
            .map(|image| Some(image.path))
    }

    pub async fn load(&self, url: Option<String>) -> Result<(), String> {
        let request_id = self.request_id.fetch_add(1, Ordering::SeqCst) + 1;
        *self.image.write().unwrap_or_else(|e| e.into_inner()) = None;

        let Some(url) = url
            .map(|url| source::normalize_url(&url))
            .filter(|url| !url.trim().is_empty())
        else {
            *self.state.write().unwrap_or_else(|e| e.into_inner()) =
                CoverState::Unavailable("当前音源没有返回封面".to_string());
            return Ok(());
        };
        *self.state.write().unwrap_or_else(|e| e.into_inner()) = CoverState::Loading;

        let mut last_error = "封面请求失败".to_string();
        let mut result: Option<CoverImage> = None;

        for attempt in 0..3 {
            if self.request_id.load(Ordering::SeqCst) != request_id {
                return Ok(());
            }
            match source::download_and_cache(&self.client, &url).await {
                Ok(image) => {
                    result = Some(image);
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
            if result.is_some() {
                *self.image.write().unwrap_or_else(|e| e.into_inner()) = result.clone();
                *self.state.write().unwrap_or_else(|e| e.into_inner()) = CoverState::Ready;
            } else {
                *self.state.write().unwrap_or_else(|e| e.into_inner()) =
                    CoverState::Unavailable(last_error.clone());
            }
        }

        match result {
            Some(_) => Ok(()),
            None => Err(last_error),
        }
    }
}
