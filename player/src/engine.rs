use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::Context;
use libmpv2::events::{Event, PropertyData};
use libmpv2::{Error, Format, Mpv, mpv_end_file_reason, mpv_error};
use lx_core::model::source::PlayerState;
use lx_core::traits::player::{
    AbLoop, AudioInfo, ChannelMode, EqualizerBand, Player, PlayerEvent, ReplayGainMode,
};
use tokio::sync::{mpsc, watch};
use tracing::{debug, warn};

const TIME_POS_OBSERVER: u64 = 1;
const AUDIO_PTS_OBSERVER: u64 = 2;
const DURATION_OBSERVER: u64 = 3;
const PAUSE_OBSERVER: u64 = 4;
const BUFFERING_OBSERVER: u64 = 5;
const AUDIO_CODEC_OBSERVER: u64 = 6;
const AUDIO_BITRATE_OBSERVER: u64 = 7;
const AUDIO_SAMPLE_RATE_OBSERVER: u64 = 8;

const DEFAULT_PLAYBACK_SPEED: f64 = 1.0;
const MIN_PLAYBACK_SPEED: f64 = 0.01;
const MAX_PLAYBACK_SPEED: f64 = 100.0;
const DEFAULT_AUDIO_DEVICE: &str = "auto";
const DEFAULT_REPLAYGAIN_PREAMP: f64 = 0.0;
const MIN_REPLAYGAIN_PREAMP: f64 = -150.0;
const MAX_REPLAYGAIN_PREAMP: f64 = 150.0;
const EQ_MIN_FREQUENCY_HZ: f64 = 1.0;
const EQ_MAX_FREQUENCY_HZ: f64 = 96_000.0;
const EQ_MIN_GAIN_DB: f64 = -150.0;
const EQ_MAX_GAIN_DB: f64 = 150.0;
const EQ_FILTER_LABEL: &str = "@voicefox-eq";
const BALANCE_FILTER_LABEL: &str = "@voicefox-balance";

struct EventLoopContext {
    state_tx: watch::Sender<PlayerState>,
    position_tx: watch::Sender<Duration>,
    audible_position_tx: watch::Sender<Duration>,
    duration_tx: watch::Sender<Duration>,
    audio_info_tx: watch::Sender<AudioInfo>,
    event_tx: mpsc::UnboundedSender<PlayerEvent>,
    generation: Arc<AtomicU64>,
    /// 最近一次 loadfile 下发时的代次：StartFile 事件据此归因，
    /// 而不是读取处理时刻的 engine generation（快速切歌时会把
    /// 旧文件的 StartFile 错记到新代次上）。
    load_generation: Arc<AtomicU64>,
    paused: Arc<AtomicBool>,
    /// 当前 loadfile 是否已到达 FileLoaded；事件线程与控制线程共享，
    /// 供 seek 判断“加载中暂停”场景（mpv 还不能接受 time-pos）。
    media_loaded: Arc<AtomicBool>,
    pending_seek: Arc<Mutex<Option<(u64, Duration)>>>,
    shutdown: Arc<AtomicBool>,
}

/// 基于 libmpv 的进程内播放引擎。
pub struct MpvEngine {
    mpv: Arc<Mpv>,
    play_lock: Mutex<()>,
    state_tx: watch::Sender<PlayerState>,
    state_rx: watch::Receiver<PlayerState>,
    position_tx: watch::Sender<Duration>,
    position_rx: watch::Receiver<Duration>,
    audible_position_tx: watch::Sender<Duration>,
    audible_position_rx: watch::Receiver<Duration>,
    duration_tx: watch::Sender<Duration>,
    duration_rx: watch::Receiver<Duration>,
    audio_info_rx: watch::Receiver<AudioInfo>,
    event_tx: mpsc::UnboundedSender<PlayerEvent>,
    event_rx: Mutex<Option<mpsc::UnboundedReceiver<PlayerEvent>>>,
    volume: AtomicU32,
    playback_speed: AtomicU64,
    audio_device: Mutex<String>,
    replaygain_mode: AtomicU8,
    replaygain_preamp: AtomicU64,
    replaygain_clip: AtomicBool,
    channel_mode: AtomicU8,
    balance: AtomicU64,
    ab_loop: Mutex<Option<AbLoop>>,
    equalizer_bands: Mutex<Vec<EqualizerBand>>,
    fade_generation: Arc<AtomicU64>,
    generation: Arc<AtomicU64>,
    load_generation: Arc<AtomicU64>,
    paused: Arc<AtomicBool>,
    media_loaded: Arc<AtomicBool>,
    pending_seek: Arc<Mutex<Option<(u64, Duration)>>>,
    shutdown: Arc<AtomicBool>,
    event_thread: Mutex<Option<JoinHandle<()>>>,
}

impl MpvEngine {
    pub fn new() -> anyhow::Result<Self> {
        let mpv = Arc::new(
            Mpv::with_initializer(|init| {
                // 与用户的全局 mpv 配置隔离：不加载 ~/.config/mpv/mpv.conf 和
                // scripts。那是面向视频播放的重型配置（GPU 着色器、脚本、ICC、
                // pipewire 等），与 voicefox 的 vo=null 音乐播放无关，却会让
                // 进程内 libmpv 在网络流播放时多占用数百 MB（实测同一份配置下
                // HTTP 播放 RSS 从约 76MB 涨到 568MB）。
                init.set_option("config", false)?;
                // mpv defines this option only when built with a scripting
                // backend. Its absence already means scripts cannot load.
                match init.set_option("load-scripts", false) {
                    Err(Error::Raw(mpv_error::OptionNotFound)) => {
                        debug!("libmpv built without scripting support");
                    }
                    result => result?,
                }
                init.set_option("vo", "null")?;
                init.set_option("cache", "yes")?;
                init.set_option("audio-client-name", "voicefox")?;
                // Keep the audio output alive across compatible file changes so
                // consecutive tracks can start without an avoidable gap.
                if let Err(error) = init.set_option("gapless-audio", "yes") {
                    warn!("libmpv gapless-audio option unavailable: {error}");
                }
                // 限制网络播放的内存缓冲。mpv 默认会把 demuxer 预读上限设到
                // 150MiB，在线歌曲连续播放时进程内 RSS 会随缓存水位升高且
                // allocator 不会立刻归还；音乐场景 32MiB 预读足够，长期播放
                // 内存保持有界。
                if let Err(error) = init.set_option("demuxer-max-bytes", 32 * 1024 * 1024) {
                    warn!("libmpv demuxer-max-bytes option unavailable: {error}");
                }
                if let Err(error) = init.set_option("demuxer-max-back-bytes", 4 * 1024 * 1024) {
                    warn!("libmpv demuxer-max-back-bytes option unavailable: {error}");
                }
                if let Err(error) = init.set_option("cache-size", 16 * 1024) {
                    warn!("libmpv cache-size option unavailable: {error}");
                }
                Ok(())
            })
            .context("初始化 libmpv 失败")?,
        );
        mpv.set_property("volume", 80.0_f64)
            .context("设置 libmpv 初始音量失败")?;

        // These controls are available as regular mpv properties. Keep startup
        // resilient when a system ships an older libmpv with a missing optional
        // property; subsequent user changes still report a useful warning.
        if let Err(error) = mpv.set_property("speed", DEFAULT_PLAYBACK_SPEED) {
            warn!("libmpv speed property unavailable: {error}");
        }
        if let Err(error) = mpv.set_property("audio-device", DEFAULT_AUDIO_DEVICE) {
            warn!("libmpv audio-device property unavailable: {error}");
        }
        if let Err(error) =
            mpv.set_property("replaygain", replaygain_mpv_value(ReplayGainMode::Off))
        {
            warn!("libmpv replaygain property unavailable: {error}");
        }
        if let Err(error) = mpv.set_property("replaygain-preamp", DEFAULT_REPLAYGAIN_PREAMP) {
            warn!("libmpv replaygain-preamp property unavailable: {error}");
        }
        if let Err(error) = mpv.set_property("replaygain-clip", false) {
            warn!("libmpv replaygain-clip property unavailable: {error}");
        }
        if let Err(error) = mpv.set_property("audio-channels", channel_mpv_value(ChannelMode::Auto))
        {
            warn!("libmpv audio-channels property unavailable: {error}");
        }

        mpv.disable_deprecated_events()
            .context("配置 libmpv 事件失败")?;
        mpv.observe_property("time-pos", Format::Double, TIME_POS_OBSERVER)
            .context("监听 libmpv 播放进度失败")?;
        mpv.observe_property("audio-pts", Format::Double, AUDIO_PTS_OBSERVER)
            .context("监听 libmpv 音频进度失败")?;
        mpv.observe_property("duration", Format::Double, DURATION_OBSERVER)
            .context("监听 libmpv 音频时长失败")?;
        mpv.observe_property("pause", Format::Flag, PAUSE_OBSERVER)
            .context("监听 libmpv 暂停状态失败")?;
        mpv.observe_property("cache-buffering-state", Format::Double, BUFFERING_OBSERVER)
            .context("监听 libmpv 缓冲状态失败")?;
        mpv.observe_property("audio-codec-name", Format::String, AUDIO_CODEC_OBSERVER)
            .context("监听 libmpv 音频编码失败")?;
        mpv.observe_property("audio-bitrate", Format::Double, AUDIO_BITRATE_OBSERVER)
            .context("监听 libmpv 音频码率失败")?;
        mpv.observe_property(
            "audio-samplerate",
            Format::Int64,
            AUDIO_SAMPLE_RATE_OBSERVER,
        )
        .context("监听 libmpv 音频采样率失败")?;

        let (state_tx, state_rx) = watch::channel(PlayerState::Idle);
        let (position_tx, position_rx) = watch::channel(Duration::ZERO);
        let (audible_position_tx, audible_position_rx) = watch::channel(Duration::ZERO);
        let (duration_tx, duration_rx) = watch::channel(Duration::ZERO);
        let (audio_info_tx, audio_info_rx) = watch::channel(AudioInfo::default());
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let generation = Arc::new(AtomicU64::new(0));
        let paused = Arc::new(AtomicBool::new(false));
        let media_loaded = Arc::new(AtomicBool::new(false));
        let pending_seek = Arc::new(Mutex::new(None));
        let shutdown = Arc::new(AtomicBool::new(false));
        let fade_generation = Arc::new(AtomicU64::new(0));
        let load_generation = Arc::new(AtomicU64::new(0));

        let event_context = EventLoopContext {
            state_tx: state_tx.clone(),
            position_tx: position_tx.clone(),
            audible_position_tx: audible_position_tx.clone(),
            duration_tx: duration_tx.clone(),
            audio_info_tx: audio_info_tx.clone(),
            event_tx: event_tx.clone(),
            generation: Arc::clone(&generation),
            load_generation: Arc::clone(&load_generation),
            paused: Arc::clone(&paused),
            media_loaded: Arc::clone(&media_loaded),
            pending_seek: Arc::clone(&pending_seek),
            shutdown: Arc::clone(&shutdown),
        };
        let event_mpv = Arc::clone(&mpv);
        let event_thread = thread::Builder::new()
            .name("voicefox-libmpv-events".to_string())
            .spawn(move || run_event_loop(event_mpv, event_context))
            .context("启动 libmpv 事件线程失败")?;

        Ok(Self {
            mpv,
            play_lock: Mutex::new(()),
            state_tx,
            state_rx,
            position_tx,
            position_rx,
            audible_position_tx,
            audible_position_rx,
            duration_tx,
            duration_rx,
            audio_info_rx,
            event_tx,
            event_rx: Mutex::new(Some(event_rx)),
            volume: AtomicU32::new(80),
            playback_speed: AtomicU64::new(DEFAULT_PLAYBACK_SPEED.to_bits()),
            audio_device: Mutex::new(DEFAULT_AUDIO_DEVICE.to_string()),
            replaygain_mode: AtomicU8::new(replaygain_mode_code(ReplayGainMode::Off)),
            replaygain_preamp: AtomicU64::new(DEFAULT_REPLAYGAIN_PREAMP.to_bits()),
            replaygain_clip: AtomicBool::new(false),
            channel_mode: AtomicU8::new(channel_mode_code(ChannelMode::Auto)),
            balance: AtomicU64::new(0.0_f64.to_bits()),
            ab_loop: Mutex::new(None),
            equalizer_bands: Mutex::new(Vec::new()),
            fade_generation,
            generation,
            load_generation,
            paused,
            media_loaded,
            pending_seek,
            shutdown,
            event_thread: Mutex::new(Some(event_thread)),
        })
    }

    fn play_inner(&self, url: &str, generation: u64, headers: &[(String, String)]) -> bool {
        let _play_guard = self.play_lock.lock().unwrap_or_else(|e| e.into_inner());
        if self.generation.load(Ordering::SeqCst) != generation {
            return false;
        }

        let header_fields = format_http_header_fields(headers);
        let result = self
            .mpv
            .set_property("http-header-fields", header_fields)
            .and_then(|()| self.mpv.set_property("pause", false))
            .and_then(|()| {
                // loadfile 下发即记录代次：事件线程用该值归因 StartFile，
                // 避免读取处理时刻的 engine generation。
                self.load_generation.store(generation, Ordering::SeqCst);
                self.mpv.command("loadfile", &[url, "replace"])
            });

        if let Err(error) = result {
            if self.generation.load(Ordering::SeqCst) == generation {
                let _ = self.state_tx.send(PlayerState::Stopped);
                let _ = self.event_tx.send(PlayerEvent::Error {
                    generation,
                    message: error.to_string(),
                });
            }
            return false;
        }

        true
    }

    fn clear_ab_loop_locked(&self) {
        let result = self
            .mpv
            .set_property("ab-loop-a", "no")
            .and_then(|()| self.mpv.set_property("ab-loop-b", "no"));
        if let Err(error) = result {
            warn!("libmpv clear A-B loop failed: {error}");
        }
        *self.ab_loop.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    fn cancel_fade_internal(&self) {
        self.fade_generation.fetch_add(1, Ordering::AcqRel);
        // 淡出被取消时 mpv 的实际音量可能停在中间值（典型是 0）。
        // 恢复为逻辑音量，否则 fade_out>0 且 fade_in==0 的配置下
        // 第二首起会永久静音（UI 仍显示原音量）。
        let _ = self
            .mpv
            .set_property("volume", f64::from(self.volume.load(Ordering::Relaxed)));
    }

    fn fade_to(&self, from: f64, to: f64, duration: Duration) {
        let token = self.fade_generation.fetch_add(1, Ordering::AcqRel) + 1;
        let mpv = Arc::clone(&self.mpv);
        let fade_generation = Arc::clone(&self.fade_generation);
        let shutdown = Arc::clone(&self.shutdown);
        let duration = duration.min(Duration::from_secs(60));
        let _ = thread::Builder::new()
            .name("voicefox-volume-fade".to_string())
            .spawn(move || {
                if duration.is_zero() {
                    if !shutdown.load(Ordering::Acquire)
                        && fade_generation.load(Ordering::Acquire) == token
                    {
                        let _ = mpv.set_property("volume", to);
                    }
                    return;
                }
                let step = Duration::from_millis(20);
                let steps = (duration.as_millis() / step.as_millis()).max(1) as u32;
                for index in 0..=steps {
                    if shutdown.load(Ordering::Acquire)
                        || fade_generation.load(Ordering::Acquire) != token
                    {
                        return;
                    }
                    let progress = f64::from(index) / f64::from(steps);
                    let value = from + (to - from) * progress;
                    let _ = mpv.set_property("volume", value.clamp(0.0, 100.0));
                    if index != steps {
                        thread::sleep(step);
                    }
                }
            });
    }
}

fn run_event_loop(event_client: Arc<Mpv>, context: EventLoopContext) {
    let mut current_file_generation = None;
    let mut last_media_position = None;
    let mut has_audio_position = false;

    while !context.shutdown.load(Ordering::SeqCst) {
        let Some(event) = event_client.wait_event(0.1) else {
            continue;
        };

        match event {
            Ok(Event::StartFile) => {
                let generation = context.load_generation.load(Ordering::SeqCst);
                if generation != context.generation.load(Ordering::SeqCst) {
                    // 该文件所属的 loadfile 已被更新的 prepare/stop 取代：
                    // 整个文件生命周期（含后续 FileLoaded/EndFile）一并忽略。
                    current_file_generation = None;
                    last_media_position = None;
                    has_audio_position = false;
                    continue;
                }
                current_file_generation = Some(generation);
                last_media_position = None;
                has_audio_position = false;
                context.media_loaded.store(false, Ordering::SeqCst);
                let _ = context.audio_info_tx.send(AudioInfo::default());
                if generation != 0 {
                    let _ = context.state_tx.send(loading_state(&context.paused));
                }
            }
            Ok(Event::FileLoaded) => {
                if let Some(generation) =
                    current_generation(current_file_generation, &context.generation)
                {
                    context.media_loaded.store(true, Ordering::SeqCst);
                    apply_pending_seek(&event_client, generation, &context.pending_seek);
                    let _ = context.state_tx.send(loading_state(&context.paused));
                }
            }
            Ok(Event::PlaybackRestart) => {
                if let Some(generation) =
                    current_generation(current_file_generation, &context.generation)
                {
                    apply_pending_seek(&event_client, generation, &context.pending_seek);
                    let state = if context.paused.load(Ordering::SeqCst) {
                        PlayerState::Paused
                    } else {
                        PlayerState::Playing
                    };
                    let _ = context.state_tx.send(state);
                    let _ = context.event_tx.send(PlayerEvent::Playing { generation });
                }
            }
            Ok(Event::EndFile(reason)) => {
                let Some(generation) =
                    current_generation(current_file_generation, &context.generation)
                else {
                    continue;
                };
                let _ = context.audio_info_tx.send(AudioInfo::default());

                match reason {
                    mpv_end_file_reason::Eof => {
                        let _ = context.state_tx.send(PlayerState::Stopped);
                        let _ = context.event_tx.send(PlayerEvent::Ended { generation });
                    }
                    mpv_end_file_reason::Error => {
                        let _ = context.state_tx.send(PlayerState::Stopped);
                        let _ = context.event_tx.send(PlayerEvent::Error {
                            generation,
                            message: "libmpv 无法播放当前音频".to_string(),
                        });
                    }
                    _ => {}
                }
                current_file_generation = None;
            }
            Ok(Event::PropertyChange { name, change, .. }) => {
                if !event_is_current(current_file_generation, &context.generation) {
                    continue;
                }
                let generation = current_file_generation.unwrap_or_default();

                match (name, change) {
                    ("time-pos", PropertyData::Double(seconds)) => {
                        if let Some(position) = duration_from_mpv_seconds(seconds) {
                            last_media_position = Some(position);
                            let _ = context.position_tx.send(position);
                            if !has_audio_position {
                                let _ = context.audible_position_tx.send(position);
                            }
                        }
                    }
                    ("audio-pts", PropertyData::Double(seconds)) => {
                        if let Some(position) = duration_from_mpv_seconds(seconds) {
                            has_audio_position = true;
                            let _ = context.audible_position_tx.send(position);
                        } else if let Some(position) = last_media_position {
                            has_audio_position = false;
                            let _ = context.audible_position_tx.send(position);
                        }
                    }
                    ("duration", PropertyData::Double(seconds)) => {
                        if let Some(duration) = duration_from_mpv_seconds(seconds) {
                            let _ = context.duration_tx.send(duration);
                        }
                    }
                    ("audio-codec-name", PropertyData::Str(codec)) => {
                        context.audio_info_tx.send_modify(|info| {
                            info.codec = (!codec.trim().is_empty()).then(|| codec.to_string());
                        });
                    }
                    ("audio-bitrate", PropertyData::Double(bitrate)) => {
                        if bitrate.is_finite() && bitrate > 0.0 {
                            context.audio_info_tx.send_modify(|info| {
                                info.bitrate_kbps = Some((bitrate / 1000.0).round() as u32);
                            });
                        }
                    }
                    ("audio-samplerate", PropertyData::Int64(sample_rate)) => {
                        if let Ok(sample_rate) = u32::try_from(sample_rate) {
                            context.audio_info_tx.send_modify(|info| {
                                info.sample_rate_hz = Some(sample_rate);
                            });
                        }
                    }
                    ("pause", PropertyData::Flag(paused)) => {
                        context.paused.store(paused, Ordering::SeqCst);
                        if paused {
                            let _ = context.state_tx.send(PlayerState::Paused);
                        }
                    }
                    ("cache-buffering-state", PropertyData::Double(percent)) => {
                        if !percent.is_finite() {
                            continue;
                        }
                        let percent = (percent.clamp(0.0, 100.0) / 100.0).clamp(0.0, 1.0);
                        if percent < 1.0 && !context.paused.load(Ordering::SeqCst) {
                            let _ = context.state_tx.send(PlayerState::Loading);
                        }
                        let _ = context.event_tx.send(PlayerEvent::Buffering {
                            generation,
                            percent,
                        });
                    }
                    _ => {}
                }
            }
            Ok(Event::QueueOverflow) => {
                warn!("libmpv event queue overflow");
            }
            Ok(Event::Shutdown) => {
                if !context.shutdown.load(Ordering::SeqCst)
                    && let Some(generation) =
                        current_generation(current_file_generation, &context.generation)
                {
                    let _ = context.state_tx.send(PlayerState::Stopped);
                    let _ = context.event_tx.send(PlayerEvent::Error {
                        generation,
                        message: "libmpv 播放核心意外退出".to_string(),
                    });
                }
                break;
            }
            Ok(_) => {}
            Err(error) => {
                if let Some(generation) =
                    current_generation(current_file_generation, &context.generation)
                {
                    let _ = context.state_tx.send(PlayerState::Stopped);
                    let _ = context.event_tx.send(PlayerEvent::Error {
                        generation,
                        message: error.to_string(),
                    });
                } else {
                    warn!("stale libmpv event error: {error}");
                }
                current_file_generation = None;
            }
        }
    }
}

fn loading_state(paused: &AtomicBool) -> PlayerState {
    if paused.load(Ordering::SeqCst) {
        PlayerState::Paused
    } else {
        PlayerState::Loading
    }
}

fn apply_pending_seek(
    event_client: &Mpv,
    generation: u64,
    pending_seek: &Mutex<Option<(u64, Duration)>>,
) {
    let position = take_pending_seek(pending_seek, generation);

    if let Some(position) = position
        && let Err(error) = event_client.set_property("time-pos", position.as_secs_f64())
    {
        warn!("libmpv deferred seek failed: {error}");
        *pending_seek.lock().unwrap_or_else(|e| e.into_inner()) = Some((generation, position));
    }
}

fn take_pending_seek(
    pending_seek: &Mutex<Option<(u64, Duration)>>,
    generation: u64,
) -> Option<Duration> {
    let mut pending = pending_seek.lock().unwrap_or_else(|e| e.into_inner());
    match *pending {
        Some((pending_generation, position)) if pending_generation == generation => {
            pending.take();
            Some(position)
        }
        Some((pending_generation, _)) if pending_generation < generation => {
            pending.take();
            None
        }
        _ => None,
    }
}

fn current_generation(event_generation: Option<u64>, active_generation: &AtomicU64) -> Option<u64> {
    let event_generation = event_generation?;
    (event_generation != 0 && event_generation == active_generation.load(Ordering::SeqCst))
        .then_some(event_generation)
}

fn event_is_current(event_generation: Option<u64>, active_generation: &AtomicU64) -> bool {
    current_generation(event_generation, active_generation).is_some()
}

fn duration_from_mpv_seconds(seconds: f64) -> Option<Duration> {
    seconds
        .is_finite()
        .then(|| Duration::from_secs_f64(seconds.max(0.0)))
}

fn format_http_header_fields(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .filter(|(name, value)| {
            !name.is_empty() && !name.contains(['\r', '\n']) && !value.contains(['\r', '\n'])
        })
        .map(|(name, value)| format!("{name}: {value}"))
        .map(|field| field.replace('\\', "\\\\").replace(',', "\\,"))
        .collect::<Vec<_>>()
        .join(",")
}

fn clamp_playback_speed(speed: f64) -> Option<f64> {
    speed
        .is_finite()
        .then(|| speed.clamp(MIN_PLAYBACK_SPEED, MAX_PLAYBACK_SPEED))
}

fn clamp_replaygain_preamp(db: f64) -> Option<f64> {
    db.is_finite()
        .then(|| db.clamp(MIN_REPLAYGAIN_PREAMP, MAX_REPLAYGAIN_PREAMP))
}

fn clamp_balance(balance: f64) -> Option<f64> {
    balance.is_finite().then(|| balance.clamp(-1.0, 1.0))
}

fn replaygain_mode_code(mode: ReplayGainMode) -> u8 {
    match mode {
        ReplayGainMode::Off => 0,
        ReplayGainMode::Track => 1,
        ReplayGainMode::Album => 2,
    }
}

fn replaygain_mode_from_code(code: u8) -> ReplayGainMode {
    match code {
        1 => ReplayGainMode::Track,
        2 => ReplayGainMode::Album,
        _ => ReplayGainMode::Off,
    }
}

fn replaygain_mpv_value(mode: ReplayGainMode) -> &'static str {
    match mode {
        ReplayGainMode::Off => "no",
        ReplayGainMode::Track => "track",
        ReplayGainMode::Album => "album",
    }
}

fn channel_mode_code(mode: ChannelMode) -> u8 {
    match mode {
        ChannelMode::Auto => 0,
        ChannelMode::Stereo => 1,
        ChannelMode::Mono => 2,
        ChannelMode::Left => 3,
        ChannelMode::Right => 4,
    }
}

fn channel_mode_from_code(code: u8) -> ChannelMode {
    match code {
        1 => ChannelMode::Stereo,
        2 => ChannelMode::Mono,
        3 => ChannelMode::Left,
        4 => ChannelMode::Right,
        _ => ChannelMode::Auto,
    }
}

fn channel_mpv_value(mode: ChannelMode) -> &'static str {
    match mode {
        ChannelMode::Auto => "auto-safe",
        ChannelMode::Stereo => "stereo",
        ChannelMode::Mono => "mono",
        // A one-speaker layout is the most direct way to select one side and
        // lets mpv perform the required conversion for the current source.
        ChannelMode::Left => "fl",
        ChannelMode::Right => "fr",
    }
}

fn valid_equalizer_band(band: EqualizerBand) -> bool {
    band.frequency_hz.is_finite()
        && band.gain_db.is_finite()
        && (EQ_MIN_FREQUENCY_HZ..=EQ_MAX_FREQUENCY_HZ).contains(&band.frequency_hz)
        && (EQ_MIN_GAIN_DB..=EQ_MAX_GAIN_DB).contains(&band.gain_db)
}

/// Build a labelled lavfi graph for Voicefox's equalizer settings.
///
/// The label lets us replace/remove only our own filter and preserve filters
/// configured by the caller or inserted automatically by mpv.
fn format_equalizer_filter(bands: &[EqualizerBand]) -> Option<String> {
    let valid_bands = bands
        .iter()
        .copied()
        .filter(|band| valid_equalizer_band(*band))
        .collect::<Vec<_>>();
    if valid_bands.is_empty() {
        return None;
    }

    let graph = valid_bands
        .iter()
        .map(|band| format!("equalizer=f={:.3}:g={:.3}", band.frequency_hz, band.gain_db))
        .collect::<Vec<_>>()
        .join(",");
    Some(format!("{EQ_FILTER_LABEL}:lavfi=[{graph}]"))
}

fn format_balance_filter(balance: f64) -> Option<String> {
    let balance = clamp_balance(balance)?;
    if balance.abs() < f64::EPSILON {
        return None;
    }

    // Keep the selected side at unity gain and attenuate only the opposite
    // side, which matches the usual balance control semantics.
    let left_gain = if balance > 0.0 { 1.0 - balance } else { 1.0 };
    let right_gain = if balance < 0.0 { 1.0 + balance } else { 1.0 };
    Some(format!(
        "{BALANCE_FILTER_LABEL}:lavfi=[pan=stereo|c0={left_gain:.6}*c0|c1={right_gain:.6}*c1]"
    ))
}

fn normalize_ab_loop(loop_points: AbLoop) -> Option<AbLoop> {
    AbLoop::new(loop_points.start, loop_points.end)
}

impl Player for MpvEngine {
    fn prepare(&self) -> u64 {
        let _play_guard = self.play_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.cancel_fade_internal();
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.paused.store(false, Ordering::SeqCst);
        self.media_loaded.store(false, Ordering::SeqCst);
        *self.pending_seek.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.clear_ab_loop_locked();
        let _ = self.state_tx.send(PlayerState::Loading);
        let _ = self.position_tx.send(Duration::ZERO);
        let _ = self.audible_position_tx.send(Duration::ZERO);
        let _ = self.duration_tx.send(Duration::ZERO);
        generation
    }

    fn play(&self, url: &str, generation: u64) -> bool {
        self.play_inner(url, generation, &[])
    }

    fn play_with_headers(&self, url: &str, generation: u64, headers: &[(String, String)]) -> bool {
        self.play_inner(url, generation, headers)
    }

    fn pause(&self) {
        if !matches!(
            *self.state_rx.borrow(),
            PlayerState::Playing | PlayerState::Loading
        ) {
            return;
        }
        if let Err(error) = self.mpv.set_property("pause", true) {
            warn!("libmpv pause failed: {error}");
            return;
        }
        self.paused.store(true, Ordering::SeqCst);
        let _ = self.state_tx.send(PlayerState::Paused);
    }

    fn resume(&self) {
        if *self.state_rx.borrow() != PlayerState::Paused {
            return;
        }
        if let Err(error) = self.mpv.set_property("pause", false) {
            warn!("libmpv resume failed: {error}");
            return;
        }
        self.paused.store(false, Ordering::SeqCst);
        let _ = self.state_tx.send(PlayerState::Playing);
    }

    fn stop(&self) {
        let _play_guard = self.play_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.cancel_fade_internal();
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.paused.store(false, Ordering::SeqCst);
        self.media_loaded.store(false, Ordering::SeqCst);
        *self.pending_seek.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.clear_ab_loop_locked();
        if let Err(error) = self.mpv.command("stop", &[]) {
            warn!("libmpv stop failed: {error}");
        }
        let _ = self.state_tx.send(PlayerState::Stopped);
        let _ = self.position_tx.send(Duration::ZERO);
        let _ = self.audible_position_tx.send(Duration::ZERO);
        let _ = self.duration_tx.send(Duration::ZERO);
    }

    fn toggle(&self) {
        let state = *self.state_rx.borrow();
        match state {
            PlayerState::Playing => self.pause(),
            PlayerState::Paused => self.resume(),
            _ => {}
        }
    }

    fn seek(&self, position: Duration) {
        let duration = *self.duration_rx.borrow();
        let position = if duration.is_zero() {
            position
        } else {
            position.min(duration)
        };
        let state = *self.state_rx.borrow();
        if state == PlayerState::Loading
            || (state == PlayerState::Paused
                && self.paused.load(Ordering::SeqCst)
                && !self.media_loaded.load(Ordering::SeqCst))
        {
            // 文件尚未加载完成（含“加载中已暂停”，此时状态是 Paused 但
            // mpv 还没 FileLoaded）：只记录待定 seek，进度条先行展示目标
            // 位置，加载完成后由事件循环应用。
            let generation = self.generation.load(Ordering::SeqCst);
            *self.pending_seek.lock().unwrap_or_else(|e| e.into_inner()) =
                Some((generation, position));
            let _ = self.position_tx.send(position);
            let _ = self.audible_position_tx.send(position);
            return;
        }
        // 淡出窗口内 seek 说明曲目不会自然结束，取消淡出并恢复音量，
        // 避免本曲剩余部分静音。
        self.cancel_fade_internal();
        if let Err(error) = self.mpv.set_property("time-pos", position.as_secs_f64()) {
            warn!("libmpv seek failed: {error}");
            return;
        }
        let _ = self.position_tx.send(position);
        let _ = self.audible_position_tx.send(position);
    }

    fn state_watcher(&self) -> watch::Receiver<PlayerState> {
        self.state_rx.clone()
    }

    fn position_watcher(&self) -> watch::Receiver<Duration> {
        self.position_rx.clone()
    }

    fn audible_position_watcher(&self) -> watch::Receiver<Duration> {
        self.audible_position_rx.clone()
    }

    fn duration_watcher(&self) -> watch::Receiver<Duration> {
        self.duration_rx.clone()
    }

    fn audio_info_watcher(&self) -> watch::Receiver<AudioInfo> {
        self.audio_info_rx.clone()
    }

    fn take_event_receiver(&self) -> Option<mpsc::UnboundedReceiver<PlayerEvent>> {
        self.event_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    fn volume(&self) -> u32 {
        self.volume.load(Ordering::Relaxed)
    }

    fn set_volume(&self, volume: u32) {
        // 先取消进行中的淡入/淡出，避免淡出线程随后把音量改回去
        self.cancel_fade_internal();
        let volume = volume.clamp(0, 100);
        if let Err(error) = self.mpv.set_property("volume", f64::from(volume)) {
            warn!("libmpv set_volume failed: {error}");
            return;
        }
        self.volume.store(volume, Ordering::Relaxed);
    }

    fn volume_up(&self, delta: u32) {
        self.set_volume(self.volume().saturating_add(delta));
    }

    fn volume_down(&self, delta: u32) {
        self.set_volume(self.volume().saturating_sub(delta));
    }

    fn playback_speed(&self) -> f64 {
        f64::from_bits(self.playback_speed.load(Ordering::Relaxed))
    }

    fn set_playback_speed(&self, speed: f64) {
        let Some(speed) = clamp_playback_speed(speed) else {
            warn!("ignoring invalid libmpv playback speed: {speed}");
            return;
        };
        if let Err(error) = self.mpv.set_property("speed", speed) {
            warn!("libmpv set_playback_speed failed: {error}");
            return;
        }
        self.playback_speed
            .store(speed.to_bits(), Ordering::Relaxed);
    }

    fn audio_output_device(&self) -> String {
        self.audio_device
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn set_audio_output_device(&self, device: &str) {
        // libmpv's CString conversion rejects NUL bytes. Reject them here so
        // a malformed value cannot leave our cached state out of sync; an empty
        // value is treated as the portable default device.
        if device.contains('\0') {
            warn!("ignoring invalid libmpv audio device name");
            return;
        }
        let device = if device.is_empty() {
            DEFAULT_AUDIO_DEVICE
        } else {
            device
        };
        let _play_guard = self.play_lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(error) = self.mpv.set_property("audio-device", device) {
            warn!("libmpv set_audio_output_device failed: {error}");
            return;
        }
        *self.audio_device.lock().unwrap_or_else(|e| e.into_inner()) = device.to_string();
    }

    fn replaygain_mode(&self) -> ReplayGainMode {
        replaygain_mode_from_code(self.replaygain_mode.load(Ordering::Relaxed))
    }

    fn set_replaygain_mode(&self, mode: ReplayGainMode) {
        if let Err(error) = self
            .mpv
            .set_property("replaygain", replaygain_mpv_value(mode))
        {
            warn!("libmpv set_replaygain_mode failed: {error}");
            return;
        }
        self.replaygain_mode
            .store(replaygain_mode_code(mode), Ordering::Relaxed);
    }

    fn replaygain_preamp(&self) -> f64 {
        f64::from_bits(self.replaygain_preamp.load(Ordering::Relaxed))
    }

    fn set_replaygain_preamp(&self, db: f64) {
        let Some(db) = clamp_replaygain_preamp(db) else {
            warn!("ignoring invalid ReplayGain preamp: {db}");
            return;
        };
        if let Err(error) = self.mpv.set_property("replaygain-preamp", db) {
            warn!("libmpv set_replaygain_preamp failed: {error}");
            return;
        }
        self.replaygain_preamp
            .store(db.to_bits(), Ordering::Relaxed);
    }

    fn replaygain_clip(&self) -> bool {
        self.replaygain_clip.load(Ordering::Relaxed)
    }

    fn set_replaygain_clip(&self, clip: bool) {
        if let Err(error) = self.mpv.set_property("replaygain-clip", clip) {
            warn!("libmpv set_replaygain_clip failed: {error}");
            return;
        }
        self.replaygain_clip.store(clip, Ordering::Relaxed);
    }

    fn channel_mode(&self) -> ChannelMode {
        channel_mode_from_code(self.channel_mode.load(Ordering::Relaxed))
    }

    fn set_channel_mode(&self, mode: ChannelMode) {
        if let Err(error) = self
            .mpv
            .set_property("audio-channels", channel_mpv_value(mode))
        {
            warn!("libmpv set_channel_mode failed: {error}");
            return;
        }
        self.channel_mode
            .store(channel_mode_code(mode), Ordering::Relaxed);
    }

    fn balance(&self) -> f64 {
        f64::from_bits(self.balance.load(Ordering::Relaxed))
    }

    fn set_balance(&self, balance: f64) {
        let Some(balance) = clamp_balance(balance) else {
            warn!("ignoring invalid channel balance: {balance}");
            return;
        };

        let _play_guard = self.play_lock.lock().unwrap_or_else(|e| e.into_inner());
        // Adding an existing label replaces that filter atomically. Clearing
        // removes only Voicefox's filter and leaves other filters untouched.
        let result = match format_balance_filter(balance) {
            Some(filter) => self.mpv.command("af", &["add", &filter]),
            None => self.mpv.command("af", &["remove", BALANCE_FILTER_LABEL]),
        };
        if let Err(error) = result {
            warn!("libmpv set_balance failed: {error}");
            return;
        }
        self.balance.store(balance.to_bits(), Ordering::Relaxed);
    }

    fn ab_loop(&self) -> Option<AbLoop> {
        *self.ab_loop.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_ab_loop(&self, loop_points: Option<AbLoop>) {
        let loop_points = match loop_points {
            Some(points) => {
                let Some(points) = normalize_ab_loop(points) else {
                    warn!("ignoring invalid A-B loop range");
                    return;
                };
                Some(points)
            }
            None => None,
        };

        let _play_guard = self.play_lock.lock().unwrap_or_else(|e| e.into_inner());
        let result = match loop_points {
            Some(points) => self
                .mpv
                .set_property("ab-loop-a", points.start.as_secs_f64())
                .and_then(|()| self.mpv.set_property("ab-loop-b", points.end.as_secs_f64())),
            None => self
                .mpv
                .set_property("ab-loop-a", "no")
                .and_then(|()| self.mpv.set_property("ab-loop-b", "no")),
        };

        if let Err(error) = result {
            warn!("libmpv set_ab_loop failed: {error}");
            return;
        }
        *self.ab_loop.lock().unwrap_or_else(|e| e.into_inner()) = loop_points;
    }

    fn equalizer_bands(&self) -> Vec<EqualizerBand> {
        self.equalizer_bands
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn set_equalizer_bands(&self, bands: &[EqualizerBand]) {
        if bands.iter().any(|band| !valid_equalizer_band(*band)) {
            warn!("ignoring invalid equalizer band");
            return;
        }

        let _play_guard = self.play_lock.lock().unwrap_or_else(|e| e.into_inner());
        // Adding an existing label replaces that filter atomically in mpv.
        // Clearing removes only our label and leaves caller/automatic filters.
        let result = match format_equalizer_filter(bands) {
            Some(filter) => self.mpv.command("af", &["add", &filter]),
            None => self.mpv.command("af", &["remove", EQ_FILTER_LABEL]),
        };
        if let Err(error) = result {
            warn!("libmpv set_equalizer_bands failed: {error}");
            return;
        }
        *self
            .equalizer_bands
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = bands.to_vec();
    }

    fn fade_in(&self, duration: Duration) {
        let target = self.volume() as f64;
        self.fade_to(0.0, target, duration);
    }

    fn fade_out(&self, duration: Duration) {
        let from = self.volume() as f64;
        self.fade_to(from, 0.0, duration);
    }

    fn cancel_fade(&self) {
        self.cancel_fade_internal();
    }
}

impl Drop for MpvEngine {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.cancel_fade_internal();
        let _ = self.mpv.command("stop", &[]);
        if let Some(event_thread) = self
            .event_thread
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            && event_thread.join().is_err()
        {
            warn!("libmpv event thread panicked");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use lx_core::model::source::PlayerState;

    use super::{
        channel_mpv_value, clamp_balance, clamp_playback_speed, clamp_replaygain_preamp,
        duration_from_mpv_seconds, format_balance_filter, format_equalizer_filter,
        format_http_header_fields, loading_state, replaygain_mpv_value, take_pending_seek,
    };
    use lx_core::traits::player::{ChannelMode, EqualizerBand, ReplayGainMode};

    #[test]
    fn invalid_mpv_times_are_handled_safely() {
        assert_eq!(duration_from_mpv_seconds(-0.001), Some(Duration::ZERO));
        assert_eq!(duration_from_mpv_seconds(f64::NAN), None);
        assert_eq!(duration_from_mpv_seconds(f64::INFINITY), None);
    }

    #[test]
    fn positive_mpv_times_keep_fractional_seconds() {
        assert_eq!(
            duration_from_mpv_seconds(1.25),
            Some(Duration::from_millis(1_250))
        );
    }

    #[test]
    fn headers_are_sanitized_and_escaped_for_mpv_string_lists() {
        let headers = vec![
            ("Referer".to_string(), "https://example.com/a,b".to_string()),
            ("X-Bad\nHeader".to_string(), "ignored".to_string()),
            ("Cookie".to_string(), r"a=b\c".to_string()),
        ];

        assert_eq!(
            format_http_header_fields(&headers),
            r"Referer: https://example.com/a\,b,Cookie: a=b\\c"
        );
    }

    #[test]
    fn empty_headers_clear_previous_mpv_headers() {
        assert_eq!(format_http_header_fields(&[]), "");
    }

    #[test]
    fn loading_events_preserve_a_pending_pause() {
        let paused = AtomicBool::new(false);
        assert_eq!(loading_state(&paused), PlayerState::Loading);

        paused.store(true, Ordering::SeqCst);
        assert_eq!(loading_state(&paused), PlayerState::Paused);
    }

    #[test]
    fn pending_seek_is_applied_only_to_its_generation() {
        let pending = Mutex::new(Some((4, Duration::from_secs(12))));

        assert_eq!(take_pending_seek(&pending, 3), None);
        assert_eq!(
            take_pending_seek(&pending, 4),
            Some(Duration::from_secs(12))
        );
        assert_eq!(take_pending_seek(&pending, 4), None);
    }

    #[test]
    fn stale_pending_seek_is_discarded() {
        let pending = Mutex::new(Some((4, Duration::from_secs(12))));

        assert_eq!(take_pending_seek(&pending, 5), None);
        assert!(pending.lock().unwrap_or_else(|e| e.into_inner()).is_none());
    }

    #[test]
    fn playback_speed_is_clamped_to_mpv_range() {
        assert_eq!(clamp_playback_speed(f64::NAN), None);
        assert_eq!(clamp_playback_speed(-1.0), Some(0.01));
        assert_eq!(clamp_playback_speed(1.25), Some(1.25));
        assert_eq!(clamp_playback_speed(101.0), Some(100.0));
    }

    #[test]
    fn replaygain_and_channel_modes_map_to_mpv_values() {
        assert_eq!(replaygain_mpv_value(ReplayGainMode::Off), "no");
        assert_eq!(replaygain_mpv_value(ReplayGainMode::Track), "track");
        assert_eq!(replaygain_mpv_value(ReplayGainMode::Album), "album");
        assert_eq!(channel_mpv_value(ChannelMode::Auto), "auto-safe");
        assert_eq!(channel_mpv_value(ChannelMode::Stereo), "stereo");
        assert_eq!(channel_mpv_value(ChannelMode::Mono), "mono");
        assert_eq!(channel_mpv_value(ChannelMode::Left), "fl");
        assert_eq!(channel_mpv_value(ChannelMode::Right), "fr");
    }

    #[test]
    fn equalizer_filter_is_labelled_and_rejects_invalid_bands() {
        let bands = [
            EqualizerBand::new(100.0, 3.0),
            EqualizerBand::new(1_000.0, -2.5),
        ];
        assert_eq!(
            format_equalizer_filter(&bands).as_deref(),
            Some("@voicefox-eq:lavfi=[equalizer=f=100.000:g=3.000,equalizer=f=1000.000:g=-2.500]")
        );
        assert!(format_equalizer_filter(&[EqualizerBand::new(0.0, 1.0)]).is_none());
        assert!(format_equalizer_filter(&[EqualizerBand::new(100.0, f64::NAN)]).is_none());
    }

    #[test]
    fn replaygain_preamp_is_clamped_and_rejects_non_finite_values() {
        assert_eq!(clamp_replaygain_preamp(f64::NAN), None);
        assert_eq!(clamp_replaygain_preamp(-200.0), Some(-150.0));
        assert_eq!(clamp_replaygain_preamp(2.5), Some(2.5));
        assert_eq!(clamp_replaygain_preamp(200.0), Some(150.0));
    }

    #[test]
    fn balance_is_clamped_and_only_attenuates_the_opposite_side() {
        assert_eq!(clamp_balance(f64::NAN), None);
        assert_eq!(clamp_balance(-2.0), Some(-1.0));
        assert_eq!(clamp_balance(0.25), Some(0.25));
        assert_eq!(format_balance_filter(0.0), None);
        assert_eq!(
            format_balance_filter(0.5).as_deref(),
            Some("@voicefox-balance:lavfi=[pan=stereo|c0=0.500000*c0|c1=1.000000*c1]")
        );
        assert_eq!(
            format_balance_filter(-1.0).as_deref(),
            Some("@voicefox-balance:lavfi=[pan=stereo|c0=1.000000*c0|c1=0.000000*c1]")
        );
    }
}
