//! 封面在终端里的实际绘制

use std::collections::VecDeque;
use std::sync::mpsc::{
    Receiver, Sender, SyncSender, TryRecvError, TrySendError, channel, sync_channel,
};
use std::sync::{Arc, Mutex};
use std::thread;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui_image::errors::Errors;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::thread::{ResizeRequest, ResizeResponse, ThreadProtocol};
use ratatui_image::{FilterType, FontSize, Resize, ResizeEncodeRender};

/// 封面缩放到封面框的尺寸，放大与缩小都按 Triangle 采样
const RESIZE: Resize = Resize::Scale(Some(FilterType::Triangle));

/// 解码后的最长边上限
///
/// 封面在终端里最终会缩放回单元格尺寸，1024px 的解码缓冲（约 4MB RGBA）
/// 超出实际需要；640px 在常见终端尺寸下画质无感知差异，峰值内存降为约 1/4。
const MAX_DECODED_EDGE: u32 = 640;

/// 解码后的封面图缓存容量（按封面路径计）。
///
/// 切回近期播放过的歌曲时直接复用解码结果，避免反复解码带来的
/// CPU 开销与瞬时大块分配；容量固定，不会无限增长。
const DECODED_COVER_CACHE_CAP: usize = 8;

/// 主线程发给解码线程的请求
struct DecodeJob {
    path: String,
    /// 构造 protocol 用的 Picker，携带当前的终端字号
    picker: Picker,
    /// 请求序号
    id: u64,
}

/// 后台线程返回给主线程的结果
enum Done {
    Loaded {
        id: u64,
        protocol: Option<Box<StatefulProtocol>>,
    },
    Resized(Box<Result<ResizeResponse, Errors>>),
}

pub struct CoverRenderer {
    picker: Picker,
    /// 字号是否来自终端应答
    queried_font_size: bool,
    enabled: bool,
    /// 最新的解码请求；新请求会覆盖尚未开始的旧请求
    decode_pending: Arc<Mutex<Option<DecodeJob>>>,
    /// 唤醒解码线程的有界通道
    decode_wake_tx: SyncSender<()>,
    /// 后台线程结果的接收端
    done_rx: Receiver<Done>,
    /// 封面的图形协议状态，内部为空表示后台线程尚未返回结果
    protocol: ThreadProtocol,
    /// 当前是否有封面可以显示
    has_image: bool,
    /// 已经开始解码的路径
    loaded: Option<String>,
    /// 上一次从 ioctl 读到的单元格尺寸
    probed: Option<FontSize>,
    /// 解码请求序号
    request_id: u64,
}

impl CoverRenderer {
    /// 按配置创建渲染器，仅在启用封面且协议为 auto 时探测终端
    ///
    /// 自动探测会发送查询序列并直接读取 stdin，只能在事件循环启动前调用一次。
    pub fn detect(cover_protocol: &str, cover_enabled: bool) -> Self {
        let (picker, queried_font_size) = match (parse_protocol(cover_protocol), cover_enabled) {
            (Some(protocol), _) => (picker_for_protocol(protocol), false),
            (None, false) => (Picker::halfblocks(), false),
            (None, true) => {
                if !is_auto(cover_protocol) {
                    tracing::warn!(
                        "unknown ui.cover_protocol {cover_protocol:?}, falling back to auto"
                    );
                }
                query_picker()
            }
        };

        let mut renderer = Self::spawn(picker);
        renderer.queried_font_size = queried_font_size;
        // 记录 ioctl 的基准读数
        renderer.refresh_font_size();
        tracing::info!(
            "cover protocol {:?}, font size {:?}",
            renderer.picker.protocol_type(),
            renderer.picker.font_size()
        );
        renderer
    }

    fn spawn(picker: Picker) -> Self {
        let decode_pending = Arc::new(Mutex::new(None));
        let (decode_wake_tx, decode_wake_rx) = sync_channel::<()>(1);
        let (encode_tx, encode_rx) = channel::<ResizeRequest>();
        let (done_tx, done_rx) = channel::<Done>();
        let enabled = spawn_workers(
            decode_wake_rx,
            Arc::clone(&decode_pending),
            encode_rx,
            done_tx,
        );

        Self {
            protocol: ThreadProtocol::new(encode_tx, None),
            queried_font_size: false,
            picker,
            enabled,
            decode_pending,
            decode_wake_tx,
            done_rx,
            has_image: false,
            loaded: None,
            probed: None,
            request_id: 0,
        }
    }

    /// 终端单元格的像素尺寸
    pub fn font_size(&self) -> FontSize {
        self.picker.font_size()
    }

    /// 本次实际生效的封面协议（设置页据此显示"配置值 vs 生效值"）。
    pub fn protocol_type(&self) -> ProtocolType {
        self.picker.protocol_type()
    }

    /// 把渲染器同步到给定的封面路径，路径变化时才向后台线程发起解码
    pub fn sync(&mut self, path: Option<&str>) {
        if !self.enabled {
            return;
        }
        match path {
            None => {
                if self.loaded.is_none() {
                    return;
                }
                self.loaded = None;
                self.request_id += 1;
                self.protocol.empty_protocol();
                self.has_image = false;
            }
            Some(path) => {
                if self.loaded.as_deref() == Some(path) {
                    return;
                }
                self.loaded = Some(path.to_string());
                self.dispatch_decode(path.to_string());
            }
        }
    }

    /// 发起一次解码，同时清空当前的封面
    fn dispatch_decode(&mut self, path: String) {
        self.request_id += 1;
        self.protocol.empty_protocol();
        let job = DecodeJob {
            path,
            picker: self.picker.clone(),
            id: self.request_id,
        };
        self.has_image = queue_decode(&self.decode_pending, &self.decode_wake_tx, job);
    }

    /// 强制重新传输当前封面
    pub fn force_reload(&mut self) {
        if !self.enabled || self.picker.protocol_type() == ProtocolType::Halfblocks {
            return;
        }
        if let Some(path) = self.loaded.clone() {
            self.dispatch_decode(path);
        }
    }

    /// 重新读取终端的单元格像素尺寸，尺寸变化时按新尺寸重新解码当前封面
    pub fn refresh_font_size(&mut self) -> bool {
        if !self.enabled {
            return false;
        }
        let Some(font_size) = probe_font_size() else {
            return false;
        };
        match self.probed.replace(font_size) {
            // 单元格尺寸与上次读到的一致
            Some(previous)
                if previous.width == font_size.width && previous.height == font_size.height =>
            {
                return false;
            }
            // 第一次读取时保留终端应答的字号
            None if self.queried_font_size => return false,
            _ => {}
        }
        tracing::debug!("cell size is now {font_size:?}");

        // Picker 没有单独设置字号的接口，重建一个并沿用探测到的协议
        #[allow(deprecated)]
        let mut picker = Picker::from_fontsize(font_size);
        picker.set_protocol_type(self.picker.protocol_type());
        self.picker = picker;

        if let Some(path) = self.loaded.clone() {
            self.dispatch_decode(path);
        }
        true
    }

    /// 收取后台线程返回的结果，返回是否需要重绘
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.done_rx.try_recv() {
                Ok(Done::Loaded { id, protocol }) => {
                    // id 与当前请求不一致说明封面已经更换
                    if id != self.request_id {
                        continue;
                    }
                    match protocol {
                        Some(protocol) => self.protocol.replace_protocol(*protocol),
                        None => {
                            self.protocol.empty_protocol();
                            self.has_image = false;
                        }
                    }
                    changed = true;
                }
                Ok(Done::Resized(result)) => match *result {
                    // 过期的结果由 ThreadProtocol 按 id 丢弃
                    Ok(response) => changed |= self.protocol.update_resized_protocol(response),
                    Err(error) => {
                        tracing::debug!("cover encode failed: {error}");
                        self.protocol.empty_protocol();
                        self.has_image = false;
                        changed = true;
                    }
                },
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        changed
    }

    /// 绘制封面，返回 false 表示未绘制
    pub fn render(&mut self, area: Rect, buf: &mut Buffer) -> bool {
        if !self.has_image || area.width == 0 || area.height == 0 {
            return false;
        }
        if let Some(size) = self.protocol.needs_resize(&RESIZE, area.into()) {
            self.protocol.resize_encode(&RESIZE, size);
        }
        self.protocol.render(area, buf);
        true
    }
}

/// 覆盖尚未开始的解码请求，并保证唤醒信号最多积压一个
fn queue_decode(
    pending: &Mutex<Option<DecodeJob>>,
    wake_tx: &SyncSender<()>,
    job: DecodeJob,
) -> bool {
    let Ok(mut pending) = pending.lock() else {
        return false;
    };
    *pending = Some(job);
    drop(pending);

    match wake_tx.try_send(()) {
        Ok(()) | Err(TrySendError::Full(())) => true,
        Err(TrySendError::Disconnected(())) => false,
    }
}

/// 启动解码线程与编码线程，返回两个线程是否都启动成功
fn spawn_workers(
    decode_wake_rx: Receiver<()>,
    decode_pending: Arc<Mutex<Option<DecodeJob>>>,
    encode_rx: Receiver<ResizeRequest>,
    done_tx: Sender<Done>,
) -> bool {
    let decode_tx = done_tx.clone();
    let decode = thread::Builder::new()
        .name("voicefox-cover-decode".to_string())
        .spawn(move || {
            let mut decoded_cache: VecDeque<(String, Arc<image::DynamicImage>)> = VecDeque::new();
            for () in decode_wake_rx {
                let Some(job) = decode_pending
                    .lock()
                    .ok()
                    .and_then(|mut pending| pending.take())
                else {
                    continue;
                };
                let image = decode_cached(&job.path, &mut decoded_cache);
                // 顺手提取主色并发布（后台线程，不阻塞渲染）：
                // 「界面强调色跟随封面」的数据源，见 cover::accent。
                let accent = image
                    .as_ref()
                    .and_then(|image| super::accent::dominant_color(image));
                super::accent::publish(&job.path, accent);
                let protocol =
                    image.map(|image| Box::new(job.picker.new_resize_protocol((*image).clone())));
                if decode_tx
                    .send(Done::Loaded {
                        id: job.id,
                        protocol,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

    let encode = thread::Builder::new()
        .name("voicefox-cover-encode".to_string())
        .spawn(move || {
            for request in encode_rx {
                if done_tx
                    .send(Done::Resized(Box::new(request.resize_encode())))
                    .is_err()
                {
                    break;
                }
            }
        });

    match (decode, encode) {
        (Ok(_), Ok(_)) => true,
        _ => {
            tracing::warn!("spawn cover worker threads failed, cover rendering disabled");
            false
        }
    }
}

/// 从终端窗口的像素尺寸和行列数反推单元格大小
fn probe_font_size() -> Option<FontSize> {
    let size = crossterm::terminal::window_size().ok()?;
    if size.width == 0 || size.height == 0 || size.columns == 0 || size.rows == 0 {
        return None;
    }
    Some(FontSize::new(
        size.width / size.columns,
        size.height / size.rows,
    ))
}

/// 使用配置指定的协议构造 Picker，不发送任何终端查询序列
fn picker_for_protocol(protocol: ProtocolType) -> Picker {
    let font_size = probe_font_size().unwrap_or_else(|| FontSize::new(10, 20));
    #[allow(deprecated)]
    let mut picker = Picker::from_fontsize(font_size);
    picker.set_protocol_type(protocol);
    picker
}

/// 向终端查询图形能力并构造 Picker，同时返回字号是否来自终端应答
fn query_picker() -> (Picker, bool) {
    // tmux 中的查询经 passthrough 发给外层终端，外层终端无法应答时跳过查询以免等满时限
    if crate::tmux::is_passthrough() && !crate::tmux::passthrough_available() {
        tracing::info!("tmux passthrough unavailable, skip cover protocol query");
        return (Picker::halfblocks(), false);
    }
    let (picker, queried_font_size) = match super::probe::query() {
        Some(result) => {
            // 在本进程重建 Picker，tmux 包装由构造时的环境检测决定
            #[allow(deprecated)]
            let mut picker = Picker::from_fontsize(result.font_size);
            picker.set_protocol_type(result.protocol);
            (picker, result.queried_font_size)
        }
        None => (Picker::halfblocks(), false),
    };
    if picker.protocol_type() == ProtocolType::Halfblocks {
        tracing::info!(
            "no graphics protocol detected, set ui.cover_protocol to kitty / sixel / iterm2 to force one"
        );
    }
    (picker, queried_font_size)
}

fn decode(path: &str) -> Option<image::DynamicImage> {
    match image::ImageReader::open(path)
        .and_then(|reader| reader.with_guessed_format())
        .map_err(|error| error.to_string())
        .and_then(|reader| reader.decode().map_err(|error| error.to_string()))
    {
        Ok(image) => Some(shrink(image)),
        Err(error) => {
            tracing::debug!("decode cover {path} failed: {error}");
            None
        }
    }
}

/// 带 LRU 缓存的封面解码：命中缓存时直接复用已解码图像并把条目
/// 移到队尾，未命中时解码并插入，超过容量时淘汰最久未用的条目。
fn decode_cached(
    path: &str,
    cache: &mut VecDeque<(String, Arc<image::DynamicImage>)>,
) -> Option<Arc<image::DynamicImage>> {
    if let Some(position) = cache.iter().position(|(cached, _)| cached == path) {
        let (cached_path, image) = cache.remove(position).expect("position is valid");
        cache.push_back((cached_path, Arc::clone(&image)));
        return Some(image);
    }
    let image = Arc::new(decode(path)?);
    cache.push_back((path.to_string(), image.clone()));
    while cache.len() > DECODED_COVER_CACHE_CAP {
        cache.pop_front();
    }
    Some(image)
}

/// 把最长边超过 [`MAX_DECODED_EDGE`] 的图按比例缩到上限
fn shrink(image: image::DynamicImage) -> image::DynamicImage {
    if image.width() <= MAX_DECODED_EDGE && image.height() <= MAX_DECODED_EDGE {
        return image;
    }
    image.resize(MAX_DECODED_EDGE, MAX_DECODED_EDGE, FilterType::Triangle)
}

/// 本终端上能画出图的封面协议
///
/// 由启动时确定的协议推出，设置页的 `Shift+P` 循环据此跳过画不出的协议，
/// 并在行内显示生效的协议。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoverCapabilities {
    /// 固定顺序的候选（用于循环切换与展示）
    supported: &'static [ProtocolType],
    /// 本次实际生效的协议
    active: ProtocolType,
    /// 探测到的图形协议，`None` 表示未探测或只能用半格
    detected: Option<ProtocolType>,
}

impl CoverCapabilities {
    /// 循环切换的固定顺序；`filtered` 是"本终端支持"的子集。
    const CYCLE: [ProtocolType; 4] = [
        ProtocolType::Kitty,
        ProtocolType::Sixel,
        ProtocolType::Iterm2,
        ProtocolType::Halfblocks,
    ];

    /// 由启动时确定的协议推出能力，未经探测时不限制可选的协议
    pub fn detect(active: ProtocolType, probed: bool) -> Self {
        if !probed {
            return Self {
                supported: &Self::CYCLE,
                active,
                detected: None,
            };
        }
        let detected = (active != ProtocolType::Halfblocks).then_some(active);
        Self::from_detected(detected, active)
    }

    /// `detected` 为探测到的图形协议，`None` 表示只能用半格
    pub fn from_detected(detected: Option<ProtocolType>, active: ProtocolType) -> Self {
        // 认不出终端时只有 halfblocks 敢保证画得出来（不发送任何图形序列）。
        let supported: &'static [ProtocolType] = match detected {
            Some(ProtocolType::Kitty) => &[ProtocolType::Kitty, ProtocolType::Halfblocks],
            Some(ProtocolType::Sixel) => &[ProtocolType::Sixel, ProtocolType::Halfblocks],
            Some(ProtocolType::Iterm2) => &[ProtocolType::Iterm2, ProtocolType::Halfblocks],
            _ => &[ProtocolType::Halfblocks],
        };
        Self {
            supported,
            active,
            detected,
        }
    }

    pub fn active(&self) -> ProtocolType {
        self.active
    }

    /// 环境探测结果（`None` = 未识别）；设置行展示"auto 生效成什么"用。
    pub fn detected(&self) -> Option<ProtocolType> {
        self.detected
    }

    #[cfg(test)]
    pub fn supported(&self) -> &'static [ProtocolType] {
        self.supported
    }

    /// 这个协议在本终端能否画出图。
    pub fn can_render(&self, protocol: ProtocolType) -> bool {
        self.supported.contains(&protocol)
    }

    /// 按固定顺序找到下一个**能画出来**的协议。
    pub fn next_supported(&self, current: ProtocolType) -> ProtocolType {
        let start = Self::CYCLE
            .iter()
            .position(|candidate| *candidate == current)
            .map_or(0, |index| index + 1);
        Self::CYCLE
            .iter()
            .cycle()
            .skip(start)
            .take(Self::CYCLE.len())
            .find(|candidate| self.can_render(**candidate))
            .copied()
            // 理论上不可能走到：`supported` 至少含 Halfblocks
            .unwrap_or(ProtocolType::Halfblocks)
    }

    /// 配置值与生效值不一致时返回原因（测试断言用；设置行改用
    /// `cover_protocol_display` 的"生效协议优先"格式）。
    #[cfg(test)]
    pub fn correction_note(&self) -> Option<String> {
        (!self.can_render(self.active)).then(|| {
            let names: Vec<&str> = self
                .supported()
                .iter()
                .copied()
                .map(protocol_label)
                .collect();
            format!(
                "本终端不支持该协议，已改用 {}；可选 {}",
                protocol_label(self.active),
                names.join(" / ")
            )
        })
    }

    /// 能力表的展示文案（仅测试断言用）。
    #[cfg(test)]
    pub fn supported_labels(&self) -> Vec<&'static str> {
        self.supported()
            .iter()
            .copied()
            .map(protocol_label)
            .collect()
    }
}

/// 协议的中文短名（设置行展示用）。
pub fn protocol_label(protocol: ProtocolType) -> &'static str {
    match protocol {
        ProtocolType::Kitty => "kitty",
        ProtocolType::Sixel => "sixel",
        ProtocolType::Iterm2 => "iterm2",
        ProtocolType::Halfblocks => "halfblocks",
    }
}

/// 把配置里的 `ui.cover_protocol` 文案解析成协议；`auto`/空/未知都返回 `None`。
///
/// 公开给设置页使用：`Shift+P` 循环要先知道"现在配的是哪个"，才能在本终端
/// 支持的子集里找下一个。
pub fn protocol_from_config(value: &str) -> Option<ProtocolType> {
    parse_protocol(value)
}

/// 配置里的 `ui.cover_protocol` 是否表示由探测决定协议
fn is_auto(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "" | "auto")
}

/// 解析配置里的 `ui.cover_protocol`，`None` 表示由探测决定
pub(super) fn parse_protocol(value: &str) -> Option<ProtocolType> {
    match value.trim().to_ascii_lowercase().as_str() {
        "kitty" => Some(ProtocolType::Kitty),
        "sixel" | "sixels" => Some(ProtocolType::Sixel),
        "iterm2" | "iterm" => Some(ProtocolType::Iterm2),
        "halfblocks" | "blocks" => Some(ProtocolType::Halfblocks),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Color;
    use ratatui_image::FontSize;
    use ratatui_image::picker::{Picker, ProtocolType};

    use super::{CoverRenderer, DecodeJob, parse_protocol, queue_decode};

    const AREA: Rect = Rect {
        x: 0,
        y: 0,
        width: 8,
        height: 4,
    };

    fn renderer() -> CoverRenderer {
        CoverRenderer::spawn(Picker::halfblocks())
    }

    /// 采用会向终端传输图片的协议的渲染器
    fn kitty_renderer() -> CoverRenderer {
        #[allow(deprecated)]
        let mut picker = Picker::from_fontsize(FontSize::new(8, 16));
        picker.set_protocol_type(ProtocolType::Kitty);
        CoverRenderer::spawn(picker)
    }

    /// 写入一张 16x16 的纯色 PNG，返回路径
    fn write_image(name: &str, color: [u8; 3]) -> String {
        write_sized_image(name, 16, 16, color)
    }

    /// 写入一张指定尺寸的纯色 PNG，返回路径
    fn write_sized_image(name: &str, width: u32, height: u32, color: [u8; 3]) -> String {
        let path = std::env::temp_dir().join(format!("voicefox-cover-{name}.png"));
        let pixel = image::Rgb(color);
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(width, height, pixel))
            .save_with_format(&path, image::ImageFormat::Png)
            .unwrap();
        path.to_string_lossy().to_string()
    }

    /// 轮询至后台完成解码与编码，返回绘制出的 buffer
    fn settle(renderer: &mut CoverRenderer) -> Buffer {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            renderer.poll();
            let mut buf = Buffer::empty(AREA);
            assert!(renderer.render(AREA, &mut buf), "应有封面可以显示");
            if buf != Buffer::empty(AREA) {
                return buf;
            }
            assert!(Instant::now() < deadline, "后台线程应已经完成");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// 画面左上角的颜色
    fn corner(buf: &Buffer) -> (Color, Color) {
        let cell = buf.cell((AREA.x, AREA.y)).unwrap();
        (cell.fg, cell.bg)
    }

    #[test]
    fn cover_is_decoded_off_thread_and_then_rendered() {
        let red = write_image("red", [255, 0, 0]);
        let mut renderer = renderer();

        // 尚无封面
        let mut buf = Buffer::empty(AREA);
        assert!(!renderer.render(AREA, &mut buf));

        renderer.sync(Some(&red));
        // 后台仍在解码：封面框由渲染器绘制，这一帧留白
        let mut buf = Buffer::empty(AREA);
        assert!(renderer.render(AREA, &mut buf));
        assert_eq!(buf, Buffer::empty(AREA));

        let buf = settle(&mut renderer);
        assert_eq!(corner(&buf), (Color::Rgb(255, 0, 0), Color::Rgb(255, 0, 0)));
    }

    #[test]
    fn a_cover_swapped_mid_decode_beats_the_stale_one() {
        let red = write_image("stale", [255, 0, 0]);
        let green = write_image("fresh", [0, 255, 0]);
        let mut renderer = renderer();

        // 两次 sync 之间不 poll，红色的结果返回时序号已经过期，必须被丢弃
        renderer.sync(Some(&red));
        renderer.sync(Some(&green));

        let buf = settle(&mut renderer);
        assert_eq!(corner(&buf), (Color::Rgb(0, 255, 0), Color::Rgb(0, 255, 0)));
    }

    #[test]
    fn an_undecodable_cover_stops_rendering() {
        let mut renderer = renderer();
        renderer.sync(Some("/voicefox/does/not/exist.png"));

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            renderer.poll();
            let mut buf = Buffer::empty(AREA);
            if !renderer.render(AREA, &mut buf) {
                return;
            }
            assert!(Instant::now() < deadline, "解码失败后应没有封面可以显示");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn an_oversized_cover_is_shrunk_when_decoded() {
        let huge = write_sized_image("huge", 3000, 1500, [0, 0, 255]);
        let image = super::decode(&huge).unwrap();
        assert_eq!(
            (image.width(), image.height()),
            (640, 320),
            "最长边应缩到上限，比例应保持"
        );

        let small = write_sized_image("small", 300, 150, [0, 0, 255]);
        let image = super::decode(&small).unwrap();
        assert_eq!(
            (image.width(), image.height()),
            (300, 150),
            "上限以内的图应原样保留"
        );
    }

    #[test]
    fn a_forced_reload_hands_the_current_cover_to_the_workers_again() {
        let blue = write_image("forced", [0, 0, 255]);
        let mut renderer = kitty_renderer();
        renderer.sync(Some(&blue));
        settle(&mut renderer);

        let before = renderer.request_id;
        renderer.force_reload();
        assert!(renderer.request_id > before, "重传应重解码");
        settle(&mut renderer);
    }

    #[test]
    fn a_forced_reload_without_a_cover_does_nothing() {
        let mut renderer = kitty_renderer();
        renderer.force_reload();
        assert_eq!(renderer.request_id, 0, "没有封面时不应响应");
    }

    #[test]
    fn halfblocks_ignores_a_forced_reload() {
        let red = write_image("halfblocks-forced", [255, 0, 0]);
        let mut renderer = renderer();
        renderer.sync(Some(&red));
        settle(&mut renderer);

        let before = renderer.request_id;
        renderer.force_reload();
        assert_eq!(renderer.request_id, before);
    }

    #[test]
    fn protocol_config_is_parsed_leniently() {
        assert_eq!(parse_protocol("auto"), None);
        assert_eq!(parse_protocol(""), None);
        assert_eq!(parse_protocol("  Kitty "), Some(ProtocolType::Kitty));
        assert_eq!(parse_protocol("SIXEL"), Some(ProtocolType::Sixel));
        assert_eq!(parse_protocol("iterm"), Some(ProtocolType::Iterm2));
        assert_eq!(parse_protocol("halfblocks"), Some(ProtocolType::Halfblocks));
        // 拼写错误退回 auto
        assert_eq!(parse_protocol("kity"), None);
    }

    #[test]
    fn an_explicit_protocol_is_selected_without_terminal_capabilities() {
        let picker = super::picker_for_protocol(ProtocolType::Kitty);
        assert_eq!(picker.protocol_type(), ProtocolType::Kitty);
        assert!(picker.capabilities().is_empty());
    }

    #[test]
    fn pending_decode_requests_are_coalesced_to_the_latest_one() {
        let pending = std::sync::Mutex::new(None);
        let (wake_tx, _wake_rx) = std::sync::mpsc::sync_channel(1);
        let picker = Picker::halfblocks();

        assert!(queue_decode(
            &pending,
            &wake_tx,
            DecodeJob {
                path: "old.png".to_string(),
                picker: picker.clone(),
                id: 1,
            },
        ));
        assert!(queue_decode(
            &pending,
            &wake_tx,
            DecodeJob {
                path: "latest.png".to_string(),
                picker,
                id: 2,
            },
        ));

        let pending = pending.lock().unwrap_or_else(|e| e.into_inner());
        let latest = pending.as_ref().unwrap();
        assert_eq!(latest.path, "latest.png");
        assert_eq!(latest.id, 2);
    }

    /// 能力表决定 `Shift+P` 循环里出现哪些协议：画不出的不再摆给用户。
    ///
    /// 这条把"终端能力"与"设置页能选什么"钉在一起 —— 之前两者各写一套，
    /// kitty 上仍能循环到 sixel / iterm2，选完只会得到一个空框。
    #[test]
    fn capabilities_drive_the_protocol_cycle() {
        use super::{CoverCapabilities, ProtocolType};

        // kitty 终端：只认自己的协议 + 保底半格
        let kitty =
            CoverCapabilities::from_detected(Some(ProtocolType::Kitty), ProtocolType::Kitty);
        assert_eq!(kitty.supported_labels(), ["kitty", "halfblocks"]);
        assert!(kitty.can_render(ProtocolType::Kitty));
        assert!(!kitty.can_render(ProtocolType::Sixel));
        assert!(!kitty.can_render(ProtocolType::Iterm2));
        assert_eq!(
            kitty.next_supported(ProtocolType::Kitty),
            ProtocolType::Halfblocks,
            "kitty 下应跳过 sixel / iterm2 直接到 halfblocks"
        );
        assert_eq!(
            kitty.next_supported(ProtocolType::Halfblocks),
            ProtocolType::Kitty,
            "循环要能绕回来"
        );
        // 配错时给出可读原因（含"可选哪些"）
        let corrected =
            CoverCapabilities::from_detected(Some(ProtocolType::Kitty), ProtocolType::Iterm2);
        assert!(!corrected.can_render(ProtocolType::Iterm2));
        let note = corrected.correction_note().expect("配错必须有说明");
        assert!(note.contains("iterm2") && note.contains("kitty"), "{note}");
        // 配置与生效一致时不该有噪声
        assert!(kitty.correction_note().is_none());

        // iTerm2 终端：iterm2 有效，kitty 协议无效
        let iterm =
            CoverCapabilities::from_detected(Some(ProtocolType::Iterm2), ProtocolType::Iterm2);
        assert_eq!(iterm.supported_labels(), ["iterm2", "halfblocks"]);
        assert_eq!(
            iterm.next_supported(ProtocolType::Iterm2),
            ProtocolType::Halfblocks
        );
        assert!(!iterm.can_render(ProtocolType::Sixel));

        // 认不出终端：只有 halfblocks 敢保证画得出来
        let unknown = CoverCapabilities::from_detected(None, ProtocolType::Halfblocks);
        assert_eq!(unknown.supported_labels(), ["halfblocks"]);
        assert_eq!(
            unknown.next_supported(ProtocolType::Halfblocks),
            ProtocolType::Halfblocks
        );
        assert!(unknown.correction_note().is_none(), "halfblocks 一定能画");

        // 显式配置未经探测，不限制可选协议
        let unprobed = CoverCapabilities::detect(ProtocolType::Sixel, false);
        assert_eq!(
            unprobed.supported_labels(),
            ["kitty", "sixel", "iterm2", "halfblocks"]
        );
        assert_eq!(
            CoverCapabilities::detect(ProtocolType::Kitty, true).supported_labels(),
            ["kitty", "halfblocks"]
        );
        assert_eq!(
            CoverCapabilities::detect(ProtocolType::Halfblocks, true).supported_labels(),
            ["halfblocks"]
        );
    }

    /// `protocol_from_config` 是设置页循环的入口：`auto`/空/未知都要能识别。
    #[test]
    fn protocol_config_parsing_round_trips() {
        use super::{ProtocolType, protocol_from_config, protocol_label};

        assert_eq!(protocol_from_config("kitty"), Some(ProtocolType::Kitty));
        assert_eq!(protocol_from_config("Iterm2"), Some(ProtocolType::Iterm2));
        assert_eq!(protocol_from_config("sixel"), Some(ProtocolType::Sixel));
        assert_eq!(
            protocol_from_config("halfblocks"),
            Some(ProtocolType::Halfblocks)
        );
        assert_eq!(protocol_from_config("auto"), None);
        assert_eq!(protocol_from_config(""), None);
        assert_eq!(protocol_from_config("nonsense"), None);

        // 循环写回配置用的是 label，必须能被解析回来
        for protocol in [
            ProtocolType::Kitty,
            ProtocolType::Sixel,
            ProtocolType::Iterm2,
            ProtocolType::Halfblocks,
        ] {
            assert_eq!(
                protocol_from_config(protocol_label(protocol)),
                Some(protocol)
            );
        }
    }
}
