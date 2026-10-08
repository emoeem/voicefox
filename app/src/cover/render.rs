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
    /// 按配置创建渲染器；仅在启用封面且协议为 auto 时探测终端
    ///
    /// 自动探测会发送查询序列并直接读取 stdin，只能在事件循环启动前调用一次。
    pub fn detect(cover_protocol: &str, cover_enabled: bool) -> Self {
        let configured = parse_protocol(cover_protocol);
        let picker = match (configured, cover_enabled) {
            (Some(protocol), _) => {
                let corrected = corrected_protocol(
                    protocol,
                    kitty_signal(
                        std::env::var("KITTY_WINDOW_ID").ok().as_deref(),
                        std::env::var("TERM").ok().as_deref(),
                    ),
                );
                if corrected != protocol {
                    tracing::warn!(
                        "ui.cover_protocol = {} 在本终端不被支持，已改用 {}",
                        protocol_label(protocol),
                        protocol_label(corrected)
                    );
                }
                picker_for_protocol(corrected)
            }
            (None, false) => Picker::halfblocks(),
            (None, true) => match protocol_from_env(
                std::env::var("KITTY_WINDOW_ID").ok().as_deref(),
                std::env::var("TERM").ok().as_deref(),
                std::env::var("TERM_PROGRAM").ok().as_deref(),
                // tmux 用 $TMUX、screen 用 $STY：它们会吞掉或**原样打印**图形序列
                std::env::var("TMUX")
                    .ok()
                    .or_else(|| std::env::var("STY").ok())
                    .as_deref(),
            ) {
                Some(protocol) => picker_for_protocol(protocol),
                None => {
                    // 环境无法**正向确认**终端支持图像协议 → 保守用半格。
                    //
                    // 这里刻意不再调用 `Picker::from_query_stdio*`：它要发查询并直接读 stdin，
                    // 对回显/中转极其敏感——0.3.19 及之前曾在 auto 分支发过查询，实测
                    // （120x36 裸 pty, TERM=xterm-256color）会得出 kitty，
                    // 强制 sixel 时单个封面载荷达 227157 字节、98.8% 是可打印 ASCII ——
                    // 一旦选中的协议终端其实不支持，这些字节会被当文本打印、刷满整屏。
                    // 误判的代价是"满屏花屏"，保守的代价只是封面用半格渲染，因此不确定就保守。
                    tracing::info!(
                        "cover protocol auto → halfblocks（终端未被正向识别；需要图像可显式设 ui.cover_protocol）"
                    );
                    Picker::halfblocks()
                }
            },
        };

        let mut renderer = Self::spawn(picker);
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
            // 第一次读取时保留 Picker 查询到的字号
            // capabilities 非空说明字号来自终端应答，ratatui-image 未文档化此关系
            None if !self.picker.capabilities().is_empty() => return false,
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

/// 解析配置里的 ui.cover_protocol，None 表示由终端探测决定
/// 环境是否明确是 kitty 终端。
fn kitty_signal(kitty_window_id: Option<&str>, term: Option<&str>) -> bool {
    kitty_window_id.is_some_and(|value| !value.trim().is_empty())
        || term.is_some_and(|value| value.to_ascii_lowercase().contains("kitty"))
}

/// 核对显式配置与环境是否**不可能成立**。
///
/// kitty 终端只实现自己的 kitty 图形协议：既不支持 sixel，**也不支持 iTerm2 的
/// `OSC 1337;File=`**（那是 iTerm2 / WezTerm 一类的扩展）。两种配错的表现不同：
///
/// - `sixel` 配错：整幅载荷被当文本打印（实测单个封面 227157 字节、98.8% 可打印
///   ASCII → 满屏 `?`）；
/// - `iterm2` 配错：kitty 直接**丢弃**该序列，封面区域什么都不画 —— 因为
///   `has_image` 为真、占位文字被跳过，用户只看到一个空框，最难自查。
///
/// 这两种组合都能由环境确定"配错了"，因此纠正为 kitty 协议并告警；其它组合一律
/// 尊重用户显式配置。
fn corrected_protocol(protocol: ProtocolType, in_kitty: bool) -> ProtocolType {
    match (protocol, in_kitty) {
        (ProtocolType::Sixel | ProtocolType::Iterm2, true) => ProtocolType::Kitty,
        (protocol, _) => protocol,
    }
}

/// 本终端上"哪些封面协议真的能画出图"的**唯一判定点**。
///
/// 之前只有"发送前纠正"（[`corrected_protocol`]）：配置写错会被静默换成可用协议，
/// 但设置页的 `Shift+P` 仍然把画不出来的选项摆在循环里，用户会一直选到空框。
/// 现在把环境探测结果收敛成这个类型，两处都从它取答案：
///
/// - **运行期**：`CoverRenderer::detect` 用它决定真正使用的协议（配错则纠正 + 告警）；
/// - **界面**：设置页的 `Shift+P` 循环按 [`Self::supported`] 跳过画不出的协议，
///   并在行内显示"配置值与生效值不一致"的原因。
///
/// 注意它**不发查询、不读 stdin**（只读环境变量），因此可以随时构造。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoverCapabilities {
    /// 固定顺序的候选（用于循环切换与展示）
    supported: &'static [ProtocolType],
    /// 本次实际生效的协议
    active: ProtocolType,
    /// 环境正向识别出的协议；`None` 表示认不出（半块字符兜底）。
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

    /// 只做环境探测（与 `detect` 同一口径），不做任何 IO。
    pub fn detect(active: ProtocolType) -> Self {
        let detected = protocol_from_env(
            std::env::var("KITTY_WINDOW_ID")
                .ok()
                .or_else(|| std::env::var("KITTY_PID").ok())
                .as_deref(),
            std::env::var("TERM").ok().as_deref(),
            std::env::var("TERM_PROGRAM").ok().as_deref(),
            std::env::var("TMUX")
                .ok()
                .or_else(|| std::env::var("STY").ok())
                .as_deref(),
        );
        Self::from_detected(detected, active)
    }

    /// 纯函数版本（可测）：`detected` 为环境正向识别出的协议，`None` 表示认不出。
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

/// 由环境变量**正向**识别终端支持的图像协议。
///
/// 只认能确定的情况：认不出来就返回 `None`，由调用方退到 `halfblocks`。
/// 抽成纯函数是为了能测（不依赖真实环境变量）。
fn protocol_from_env(
    kitty_window_id: Option<&str>,
    term: Option<&str>,
    term_program: Option<&str>,
    multiplexer: Option<&str>,
) -> Option<ProtocolType> {
    // 多路复用器优先级最高：`$TMUX`/`$STY` 会从外层终端**继承**（`KITTY_WINDOW_ID`、
    // `TERM_PROGRAM` 因此在 tmux/ssh 里也可能"假的为真"）。图形序列要穿过 tmux 需要
    // >=3.3 且 `allow-passthrough on`，screen 完全不支持，而这里无法确认 —— 一旦猜错，
    // 整套载荷会被原样当文本打印（实测 kitty 协议每帧重发，6 秒即约 1MB 垃圾）。
    // 代价对比：猜错=满屏花屏，保守=封面用半格渲染。所以有复用器就保守。
    if multiplexer.is_some_and(|value| !value.trim().is_empty()) {
        return None;
    }
    if kitty_signal(kitty_window_id, term) {
        return Some(ProtocolType::Kitty);
    }
    if let Some(term) = term {
        let term = term.to_ascii_lowercase();
        if term.contains("kitty") {
            return Some(ProtocolType::Kitty);
        }
        // foot / contour / mlterm / yaft 等以 TERM 直接声明 sixel
        if term.contains("sixel") || term.starts_with("foot") || term.starts_with("contour") {
            return Some(ProtocolType::Sixel);
        }
    }
    match term_program
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "iterm.app" => Some(ProtocolType::Iterm2),
        // WezTerm 实现了 kitty 图形协议
        "wezterm" => Some(ProtocolType::Kitty),
        _ => None,
    }
}

/// 把配置里的 `ui.cover_protocol` 文案解析成协议；`auto`/空/未知都返回 `None`。
///
/// 公开给设置页使用：`Shift+P` 循环要先知道"现在配的是哪个"，才能在本终端
/// 支持的子集里找下一个。
pub fn protocol_from_config(value: &str) -> Option<ProtocolType> {
    parse_protocol(value)
}

fn parse_protocol(value: &str) -> Option<ProtocolType> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "auto" => None,
        "kitty" => Some(ProtocolType::Kitty),
        "sixel" | "sixels" => Some(ProtocolType::Sixel),
        "iterm2" | "iterm" => Some(ProtocolType::Iterm2),
        "halfblocks" | "blocks" => Some(ProtocolType::Halfblocks),
        other => {
            tracing::warn!("unknown ui.cover_protocol {other:?}, falling back to auto");
            None
        }
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

    use super::{CoverRenderer, DecodeJob, parse_protocol, protocol_from_env, queue_decode};

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

    /// 协议识别必须是"正向"的：认不出来就返回 None（调用方退 halfblocks），
    /// 绝不能猜一个终端可能不支持的协议 —— 猜错的代价是整幅载荷被当文本刷屏。
    #[test]
    fn protocol_detection_is_positive_only() {
        use super::ProtocolType;

        let kitty = protocol_from_env(Some("1"), None, None, None);
        assert_eq!(
            kitty,
            Some(ProtocolType::Kitty),
            "KITTY_WINDOW_ID 是确定信号"
        );
        assert_eq!(
            protocol_from_env(None, Some("xterm-kitty"), None, None),
            Some(ProtocolType::Kitty)
        );
        assert_eq!(
            protocol_from_env(None, Some("foot"), None, None),
            Some(ProtocolType::Sixel),
            "foot 声明支持 sixel"
        );
        assert_eq!(
            protocol_from_env(None, Some("xterm-256color"), Some("iTerm.app"), None),
            Some(ProtocolType::Iterm2)
        );
        assert_eq!(
            protocol_from_env(None, Some("xterm-256color"), Some("WezTerm"), None),
            Some(ProtocolType::Kitty)
        );

        // 认不出来 → None（调用方退 halfblocks）
        assert_eq!(
            protocol_from_env(None, Some("xterm-256color"), None, None),
            None
        );
        assert_eq!(
            protocol_from_env(None, Some("linux"), Some("Apple_Terminal"), None),
            None
        );
        assert_eq!(protocol_from_env(None, None, None, None), None);
        assert_eq!(
            protocol_from_env(Some("   "), Some("dumb"), Some("Unknown"), None),
            None,
            "空白/未知信号不能当成支持"
        );

        // 关键回归：环境变量会穿透 tmux/ssh，复用器里发图形序列会被原样打印成文本
        assert_eq!(
            protocol_from_env(
                Some("1"),
                Some("xterm-kitty"),
                None,
                Some("/tmp/tmux-1000/default,123,0")
            ),
            None,
            "tmux 里不能发 kitty 图形（passthrough 无法确认）"
        );
        assert_eq!(
            protocol_from_env(
                None,
                Some("xterm-256color"),
                Some("WezTerm"),
                Some("/tmp/screen.123")
            ),
            None,
            "screen 里一律保守"
        );
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

    /// kitty 终端既不支持 sixel，也不支持 iTerm2 协议：显式配错时必须纠正。
    ///
    /// sixel 配错是"整幅载荷被当文本打印"（花屏），iTerm2 配错则是 kitty 静默丢弃
    /// 序列 —— 封面区只留一个空框，用户完全看不出是协议选错了。
    #[test]
    fn unsupported_protocols_are_corrected_on_kitty_terminals() {
        use super::{ProtocolType, corrected_protocol};

        for configured in [ProtocolType::Sixel, ProtocolType::Iterm2] {
            assert_eq!(
                corrected_protocol(configured, true),
                ProtocolType::Kitty,
                "kitty 下 {configured:?} 不可能成立，必须纠正"
            );
            assert_eq!(
                corrected_protocol(configured, false),
                configured,
                "非 kitty 终端尊重显式配置"
            );
        }
        // 其它组合一律不动
        for protocol in [ProtocolType::Kitty, ProtocolType::Halfblocks] {
            assert_eq!(corrected_protocol(protocol, true), protocol);
            assert_eq!(corrected_protocol(protocol, false), protocol);
        }
    }
}
