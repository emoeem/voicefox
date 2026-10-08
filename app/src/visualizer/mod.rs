//! 频谱可视化：系统音频监视器采集 PCM → FFT → 半块柱状渲染。
//!
//! 数据链路完全在 [`self`] 模块内闭环，主循环只做三件事：
//! 开关时 [`Visualizer::start`] / drop、每帧 [`Visualizer::frame`] 取最新
//! 一帧、把内容区交给 [`render`]。采集与 FFT 都在专属线程上，
//! 主循环每帧只读一份 96 个 f32 的快照。
//!
//! 口径说明：采集的是系统输出监视流（cava 同款）——画的是电脑正在出声的
//! 混音，voicefox 自己不出声时柱子是平的。这是终端播放器可视化的
//! 惯例取舍，也因此在 mpv 之外零侵入。

mod capture;
mod render;
mod spectrum;

pub use render::{PaletteCache, render_data};

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// 超过这个时间没有新帧，就认为采集链路已经断了：宁可让叠加层消失，
/// 也不能把最后一帧当作"实时频谱"一直画在屏幕上（用户看到的就是卡死）。
const STALE_AFTER: Duration = Duration::from_millis(1000);

/// 一帧可视化数据。分析层不依赖任何 UI。
pub use spectrum::VisualizerData;

#[derive(Debug, Clone, Default)]
pub struct Frame {
    pub data: VisualizerData,
    pub peaks: Vec<f32>,
}

/// 采集线程发布的最新快照。
#[derive(Debug, Clone)]
pub struct Snapshot {
    data: VisualizerData,
    peaks: Vec<f32>,
    /// 这一帧的发布时间。Instant 没有 Default，因此这里不派生 Default
    /// （快照容器本身就是 Option&lt;Snapshot&gt;，用不到默认值）。
    updated_at: Instant,
}

impl Snapshot {
    fn frame(&self) -> Option<Frame> {
        if self.data.spectrum.is_empty() || self.updated_at.elapsed() > STALE_AFTER {
            return None;
        }
        Some(Frame {
            data: self.data.clone(),
            peaks: self.peaks.clone(),
        })
    }
}

/// 运行中的可视化（采集线程 + 静默监测线程 + 最新帧）。
pub struct Visualizer {
    snapshot: Arc<Mutex<Option<Snapshot>>>,
    shutdown: Arc<AtomicBool>,
    /// 当前正在读取的采集子进程。采集线程断流后会在这里换成新进程，
    /// drop / 静默监测也照这个槽位杀——否则重连后旧槽位会留下孤儿进程。
    current: Arc<Mutex<Option<capture::CaptureProcess>>>,
    worker: Option<JoinHandle<()>>,
    idle_monitor: Option<JoinHandle<()>>,
}

impl Visualizer {
    /// 启动采集线程。系统里找不到 `parec` / `pw-record` 时返回 Err。
    pub fn start() -> anyhow::Result<Self> {
        let Some(command) = capture::capture_command() else {
            anyhow::bail!(
                "未找到 parec 或 pw-record，无法采集系统音频（需要 pulseaudio-utils 或 pipewire）"
            );
        };
        let snapshot = Arc::new(Mutex::new(None));
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_snapshot = Arc::clone(&snapshot);
        let worker_shutdown = Arc::clone(&shutdown);
        // 采集线程与静默监测线程必须共享同一个时钟：监测线程读的是
        // "最近一次收到字节"的时刻。一旦各持一份，监测线程看到的永远是
        // 冻结的启动时刻，会把每个健康的采集进程当成静默流杀掉。
        let last_data: capture::LastDataMs = Arc::new(AtomicU64::new(capture::now_ms()));
        let worker_last_data = Arc::clone(&last_data);
        let current = Arc::new(Mutex::new(Some(capture::CaptureProcess::spawn(command)?)));
        let worker_current = Arc::clone(&current);
        let worker = std::thread::Builder::new()
            .name("voicefox-visualizer".to_string())
            .spawn(move || {
                capture::run_capture(
                    worker_current,
                    worker_shutdown,
                    worker_snapshot,
                    worker_last_data,
                )
            });
        let worker = match worker {
            Ok(worker) => worker,
            Err(error) => {
                if let Some(capture) = current
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
                {
                    capture.kill();
                }
                return Err(error.into());
            }
        };
        let monitor_shutdown = Arc::clone(&shutdown);
        let monitor_current = Arc::clone(&current);
        let monitor_snapshot = Arc::clone(&snapshot);
        let monitor_last_data = Arc::clone(&last_data);
        let idle_monitor = capture::spawn_silence_watch(
            monitor_current,
            monitor_shutdown,
            monitor_last_data,
            monitor_snapshot,
        );
        // 静默监测起不来只损失"待机舞台 / 静默自愈"能力，采集本身照常工作。
        let idle_monitor = match idle_monitor {
            Ok(idle_monitor) => Some(idle_monitor),
            Err(error) => {
                tracing::warn!("可视化静默监测线程启动失败: {error}");
                None
            }
        };
        Ok(Self {
            snapshot,
            shutdown,
            current,
            worker: Some(worker),
            idle_monitor,
        })
    }

    /// 最近一帧；采集刚开始（窗口未填满）时为 None。
    pub fn frame(&self) -> Option<Frame> {
        self.snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(Snapshot::frame)
    }
}

impl Drop for Visualizer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        // 杀当前这枚子进程即可：采集线程可能已经重连过多次，槽位里始终是
        // 正在读的那一枚；杀掉它就能让阻塞的 stdout.read 立刻返回。
        if let Some(capture) = self
            .current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            capture.kill();
        }
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            tracing::warn!("频谱采集线程异常退出");
        }
        if let Some(idle_monitor) = self.idle_monitor.take()
            && idle_monitor.join().is_err()
        {
            tracing::warn!("可视化静默监测线程异常退出");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_is_none_before_first_snapshot() {
        // 不启动采集线程（避免测试依赖系统音频栈），只验证快照语义。
        let handle = Visualizer {
            snapshot: Arc::new(Mutex::new(None)),
            shutdown: Arc::new(AtomicBool::new(false)),
            current: Arc::new(Mutex::new(None)),
            worker: None,
            idle_monitor: None,
        };
        assert!(handle.frame().is_none());
        *handle
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Snapshot {
            data: VisualizerData {
                spectrum: vec![0.5; 4],
                ..VisualizerData::default()
            },
            peaks: vec![0.6; 4],
            updated_at: Instant::now(),
        });
        let frame = handle.frame().unwrap();
        assert_eq!(frame.data.spectrum.len(), 4);
        assert_eq!(frame.peaks.len(), 4);
    }
}
