use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use image::RgbaImage;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use reqwest::header::{ACCEPT, REFERER};

#[derive(Debug, Clone, Copy)]
struct CoverCell {
    foreground: Color,
    background: Color,
}

#[derive(Debug)]
struct RenderCache {
    width: u16,
    height: u16,
    cells: Vec<CoverCell>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoverState {
    Empty,
    Loading,
    Ready,
    Unavailable(String),
}

pub struct CoverService {
    client: reqwest::Client,
    image: RwLock<Option<Arc<RgbaImage>>>,
    state: RwLock<CoverState>,
    cache: Mutex<Option<RenderCache>>,
    request_id: AtomicU64,
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
            image: RwLock::new(None),
            state: RwLock::new(CoverState::Empty),
            cache: Mutex::new(None),
            request_id: AtomicU64::new(0),
        }
    }

    pub fn clear(&self) {
        self.request_id.fetch_add(1, Ordering::SeqCst);
        *self.image.write().unwrap() = None;
        *self.state.write().unwrap() = CoverState::Empty;
        *self.cache.lock().unwrap() = None;
    }

    pub fn has_image(&self) -> bool {
        self.image.read().unwrap().is_some()
    }

    pub fn state(&self) -> CoverState {
        self.state.read().unwrap().clone()
    }

    pub async fn load(&self, url: Option<String>) -> Result<(), String> {
        let request_id = self.request_id.fetch_add(1, Ordering::SeqCst) + 1;
        *self.image.write().unwrap() = None;
        *self.cache.lock().unwrap() = None;

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
        for attempt in 0..3 {
            if self.request_id.load(Ordering::SeqCst) != request_id {
                return Ok(());
            }

            match self.download_and_decode(&url).await {
                Ok(image) => {
                    if self.request_id.load(Ordering::SeqCst) == request_id {
                        *self.image.write().unwrap() = Some(Arc::new(image));
                        *self.state.write().unwrap() = CoverState::Ready;
                        *self.cache.lock().unwrap() = None;
                    }
                    return Ok(());
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
            *self.state.write().unwrap() = CoverState::Unavailable(last_error.clone());
        }
        Err(last_error)
    }

    async fn download_and_decode(&self, url: &str) -> Result<RgbaImage, String> {
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
        tokio::task::spawn_blocking(move || {
            image::load_from_memory(&bytes)
                .map(|image| image.to_rgba8())
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| error.to_string())?
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let Some(image) = self.image.read().unwrap().as_ref().map(Arc::clone) else {
            return;
        };

        let mut cache = self.cache.lock().unwrap();
        let needs_resize = cache
            .as_ref()
            .is_none_or(|cache| cache.width != area.width || cache.height != area.height);
        if needs_resize {
            *cache = Some(build_cache(&image, area.width, area.height));
        }
        let Some(cache) = cache.as_ref() else {
            return;
        };

        for y in 0..area.height {
            for x in 0..area.width {
                let cell = cache.cells[y as usize * area.width as usize + x as usize];
                buf[(area.x + x, area.y + y)]
                    .set_symbol("▀")
                    .set_fg(cell.foreground)
                    .set_bg(cell.background);
            }
        }
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

fn build_cache(image: &RgbaImage, width: u16, height: u16) -> RenderCache {
    let target_width = u32::from(width);
    let target_height = u32::from(height).saturating_mul(2);
    let scale = (target_width as f64 / image.width().max(1) as f64)
        .min(target_height as f64 / image.height().max(1) as f64);
    let resized_width =
        ((image.width() as f64 * scale).round() as u32).clamp(1, target_width.max(1));
    let resized_height =
        ((image.height() as f64 * scale).round() as u32).clamp(1, target_height.max(1));
    let resized = image::imageops::resize(
        image,
        resized_width,
        resized_height,
        image::imageops::FilterType::Triangle,
    );
    let offset_x = target_width.saturating_sub(resized_width) / 2;
    let offset_y = target_height.saturating_sub(resized_height) / 2;
    let fallback = [18, 20, 25, 255];

    let mut cells = Vec::with_capacity(width as usize * height as usize);
    for cell_y in 0..u32::from(height) {
        for cell_x in 0..target_width {
            let top = pixel_at(
                &resized,
                cell_x,
                cell_y.saturating_mul(2),
                offset_x,
                offset_y,
                fallback,
            );
            let bottom = pixel_at(
                &resized,
                cell_x,
                cell_y.saturating_mul(2).saturating_add(1),
                offset_x,
                offset_y,
                fallback,
            );
            cells.push(CoverCell {
                foreground: Color::Rgb(top[0], top[1], top[2]),
                background: Color::Rgb(bottom[0], bottom[1], bottom[2]),
            });
        }
    }
    RenderCache {
        width,
        height,
        cells,
    }
}

fn pixel_at(
    image: &RgbaImage,
    target_x: u32,
    target_y: u32,
    offset_x: u32,
    offset_y: u32,
    fallback: [u8; 4],
) -> [u8; 4] {
    if target_x < offset_x || target_y < offset_y {
        return fallback;
    }
    let x = target_x - offset_x;
    let y = target_y - offset_y;
    if x < image.width() && y < image.height() {
        image.get_pixel(x, y).0
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::{build_cache, cover_referer, normalize_url};
    use image::{Rgba, RgbaImage};

    #[test]
    fn cover_cache_matches_terminal_cell_dimensions() {
        let image = RgbaImage::from_pixel(16, 16, Rgba([20, 40, 60, 255]));

        let cache = build_cache(&image, 12, 7);

        assert_eq!(cache.width, 12);
        assert_eq!(cache.height, 7);
        assert_eq!(cache.cells.len(), 84);
    }

    #[test]
    fn normalizes_protocol_relative_cover_urls() {
        assert_eq!(
            normalize_url("//example.com/cover.webp"),
            "https://example.com/cover.webp"
        );
    }

    #[test]
    fn adds_platform_referer_for_known_cover_hosts() {
        assert_eq!(
            cover_referer("https://img1.kuwo.cn/star/albumcover.jpg"),
            Some("https://www.kuwo.cn/")
        );
    }
}
