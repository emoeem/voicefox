use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lx_core::model::source::PlayerState;
use lx_core::traits::player::{Player, PlayerEvent};
use tokio::sync::{mpsc, watch};
use tracing::warn;

use crate::mpv_ipc::{MpvEvent, MpvIpc};

/// 播放引擎（mpv 子进程 + JSON IPC）
pub struct MpvEngine {
    /// mpv IPC 实例（Arc 支持跨任务共享）
    ipc: Mutex<Option<Arc<MpvIpc>>>,
    play_lock: Mutex<()>,
    state_tx: watch::Sender<PlayerState>,
    state_rx: watch::Receiver<PlayerState>,
    position_tx: watch::Sender<Duration>,
    position_rx: watch::Receiver<Duration>,
    duration_tx: watch::Sender<Duration>,
    duration_rx: watch::Receiver<Duration>,
    event_tx: mpsc::UnboundedSender<PlayerEvent>,
    event_rx: Mutex<Option<mpsc::UnboundedReceiver<PlayerEvent>>>,
    volume: AtomicU32,
    generation: Arc<AtomicU64>,
    pending_headers: Mutex<HashMap<u64, Vec<(String, String)>>>,
}

impl MpvEngine {
    pub fn new() -> Self {
        let (state_tx, state_rx) = watch::channel(PlayerState::Idle);
        let (position_tx, position_rx) = watch::channel(Duration::ZERO);
        let (duration_tx, duration_rx) = watch::channel(Duration::ZERO);
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        Self {
            ipc: Mutex::new(None),
            play_lock: Mutex::new(()),
            state_tx,
            state_rx,
            position_tx,
            position_rx,
            duration_tx,
            duration_rx,
            event_tx,
            event_rx: Mutex::new(Some(event_rx)),
            volume: AtomicU32::new(80),
            generation: Arc::new(AtomicU64::new(0)),
            pending_headers: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for MpvEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// 解析 mpv 响应的 `data` 字段为 f64
fn parse_mpv_data(resp: &str) -> Option<f64> {
    let v: serde_json::Value = serde_json::from_str(resp).ok()?;
    v.get("data")?.as_f64()
}

impl Player for MpvEngine {
    fn prepare(&self) -> u64 {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.state_tx.send(PlayerState::Loading);
        let _ = self.position_tx.send(Duration::ZERO);
        let _ = self.duration_tx.send(Duration::ZERO);
        generation
    }

    fn play(&self, url: &str, generation: u64) -> bool {
        let headers = self
            .pending_headers
            .lock()
            .unwrap()
            .remove(&generation)
            .unwrap_or_default();
        let _play_guard = self.play_lock.lock().unwrap();
        if self.generation.load(Ordering::SeqCst) != generation {
            return false;
        }

        // 停止旧 mpv
        let old_ipc = self.ipc.lock().unwrap().take();
        if let Some(old) = old_ipc {
            old.stop();
        }

        // 启动新 mpv
        match MpvIpc::start(None) {
            Ok(ipc) => {
                let ipc = Arc::new(ipc);
                if self.generation.load(Ordering::SeqCst) != generation {
                    ipc.stop();
                    return false;
                }

                // 取出事件接收端并启动事件监听任务
                let polling = Arc::new(AtomicBool::new(true));
                if let Some(ipc_event_rx) = ipc.event_receiver() {
                    let event_tx = self.event_tx.clone();
                    let state_tx = self.state_tx.clone();
                    let active_generation = Arc::clone(&self.generation);
                    let polling = Arc::clone(&polling);
                    tokio::spawn(async move {
                        let mut rx = ipc_event_rx;
                        while let Some(event) = rx.recv().await {
                            if active_generation.load(Ordering::SeqCst) != generation {
                                break;
                            }
                            match event {
                                MpvEvent::EndFile => {
                                    polling.store(false, Ordering::SeqCst);
                                    let _ = event_tx.send(PlayerEvent::Ended);
                                    let _ = state_tx.send(PlayerState::Stopped);
                                }
                                MpvEvent::Error(error) => {
                                    polling.store(false, Ordering::SeqCst);
                                    let _ = event_tx.send(PlayerEvent::Error(error));
                                    let _ = state_tx.send(PlayerState::Stopped);
                                }
                            }
                        }
                    });
                }

                // 启动进度轮询任务
                let ipc_clone = Arc::clone(&ipc);
                let position_tx = self.position_tx.clone();
                let duration_tx = self.duration_tx.clone();
                let active_generation = Arc::clone(&self.generation);
                let polling_task = Arc::clone(&polling);
                tokio::spawn(async move {
                    let mut ticker = tokio::time::interval(Duration::from_millis(50));
                    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    let mut tick = 0u32;
                    loop {
                        ticker.tick().await;
                        if active_generation.load(Ordering::SeqCst) != generation
                            || !polling_task.load(Ordering::SeqCst)
                        {
                            break;
                        }

                        // 查询 time-pos
                        let ipc = Arc::clone(&ipc_clone);
                        let pos_res =
                            tokio::task::spawn_blocking(move || ipc.get_property("time-pos"))
                                .await
                                .ok()
                                .and_then(Result::ok);

                        if let Some(ref pos_str) = pos_res
                            && let Some(secs) = parse_mpv_data(pos_str)
                        {
                            let _ = position_tx.send(Duration::from_secs_f64(secs));
                        }

                        // 总时长变化频率很低，每秒查询一次即可。
                        if tick.is_multiple_of(20) {
                            let ipc = Arc::clone(&ipc_clone);
                            let dur_res =
                                tokio::task::spawn_blocking(move || ipc.get_property("duration"))
                                    .await
                                    .ok()
                                    .and_then(Result::ok);
                            if let Some(ref dur_str) = dur_res
                                && let Some(secs) = parse_mpv_data(dur_str)
                            {
                                let _ = duration_tx.send(Duration::from_secs_f64(secs));
                            }
                        }
                        tick = tick.wrapping_add(1);
                    }
                });

                let _ = ipc.set_volume(self.volume());
                if !headers.is_empty() {
                    let fields = headers
                        .iter()
                        .filter(|(name, value)| {
                            !name.contains(['\r', '\n']) && !value.contains(['\r', '\n'])
                        })
                        .map(|(name, value)| format!("{name}: {value}"))
                        .collect::<Vec<_>>();
                    let header_command = serde_json::json!({
                        "command": ["set_property", "http-header-fields", fields]
                    })
                    .to_string();
                    if let Err(error) = ipc.send_command(&header_command) {
                        polling.store(false, Ordering::SeqCst);
                        ipc.stop();
                        let _ = self.state_tx.send(PlayerState::Stopped);
                        let _ = self.event_tx.send(PlayerEvent::Error(error.to_string()));
                        return false;
                    }
                }
                let load_command = serde_json::json!({
                    "command": ["loadfile", url, "replace"]
                })
                .to_string();
                if let Err(error) = ipc.send_command(&load_command) {
                    polling.store(false, Ordering::SeqCst);
                    ipc.stop();
                    let _ = self.state_tx.send(PlayerState::Stopped);
                    let _ = self.event_tx.send(PlayerEvent::Error(error.to_string()));
                    return false;
                }
                // 存入 ipc 并更新状态
                *self.ipc.lock().unwrap() = Some(ipc);
                let _ = self.state_tx.send(PlayerState::Playing);
                true
            }
            Err(e) => {
                if self.generation.load(Ordering::SeqCst) == generation {
                    let _ = self.state_tx.send(PlayerState::Stopped);
                    let _ = self.event_tx.send(PlayerEvent::Error(e.to_string()));
                }
                false
            }
        }
    }

    fn play_with_headers(&self, url: &str, generation: u64, headers: &[(String, String)]) -> bool {
        self.pending_headers
            .lock()
            .unwrap()
            .insert(generation, headers.to_vec());
        self.play(url, generation)
    }

    fn pause(&self) {
        let _ = self.state_tx.send(PlayerState::Paused);
        {
            let guard = self.ipc.lock().unwrap();
            if let Some(ref ipc) = *guard
                && let Err(e) =
                    ipc.send_command("{\"command\": [\"set_property\", \"pause\", true]}")
            {
                warn!("mpv pause failed: {}", e);
            }
        }
    }

    fn resume(&self) {
        let _ = self.state_tx.send(PlayerState::Playing);
        {
            let guard = self.ipc.lock().unwrap();
            if let Some(ref ipc) = *guard
                && let Err(e) =
                    ipc.send_command("{\"command\": [\"set_property\", \"pause\", false]}")
            {
                warn!("mpv resume failed: {}", e);
            }
        }
    }

    fn stop(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        {
            let guard = self.ipc.lock().unwrap();
            if let Some(ref ipc) = *guard
                && let Err(e) = ipc.send_command("{\"command\": [\"stop\"]}")
            {
                warn!("mpv stop failed: {}", e);
            }
        }
        let _ = self.state_tx.send(PlayerState::Stopped);
        let _ = self.position_tx.send(Duration::ZERO);
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
        // 先更新本地观察值，让进度条和歌词立即响应，再通知 mpv。
        let _ = self.position_tx.send(position);
        let cmd = format!(
            "{{\"command\": [\"set_property\", \"time-pos\", {}]}}",
            position.as_secs_f64()
        );
        {
            let guard = self.ipc.lock().unwrap();
            if let Some(ref ipc) = *guard
                && let Err(e) = ipc.send_command(&cmd)
            {
                warn!("mpv seek failed: {}", e);
            }
        }
    }

    fn state_watcher(&self) -> watch::Receiver<PlayerState> {
        self.state_rx.clone()
    }

    fn position_watcher(&self) -> watch::Receiver<Duration> {
        self.position_rx.clone()
    }

    fn duration_watcher(&self) -> watch::Receiver<Duration> {
        self.duration_rx.clone()
    }

    fn take_event_receiver(&self) -> Option<mpsc::UnboundedReceiver<PlayerEvent>> {
        self.event_rx.lock().unwrap().take()
    }

    fn volume(&self) -> u32 {
        self.volume.load(Ordering::Relaxed)
    }

    fn set_volume(&self, vol: u32) {
        let vol = vol.clamp(0, 100);
        self.volume.store(vol, Ordering::Relaxed);
        let cmd = format!("{{\"command\": [\"set_property\", \"volume\", {}]}}", vol);
        let guard = self.ipc.lock().unwrap();
        if let Some(ref ipc) = *guard
            && let Err(e) = ipc.send_command(&cmd)
        {
            warn!("mpv set_volume failed: {}", e);
        }
    }

    fn volume_up(&self, delta: u32) {
        let v = self.volume() + delta;
        self.set_volume(v);
    }

    fn volume_down(&self, delta: u32) {
        let v = self.volume().saturating_sub(delta);
        self.set_volume(v);
    }
}

impl Drop for MpvEngine {
    fn drop(&mut self) {
        let ipc = self.ipc.lock().unwrap().take();
        if let Some(ipc) = ipc {
            ipc.stop();
        }
    }
}
