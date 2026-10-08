//! PCM 采集：读取系统默认输出的监视流。
//!
//! 监视流（monitor）混录的是本机正在播放的全部声音 —— cava 等终端可视化
//! 也是这个口径。采集用现成的命令行工具子进程，不引入音频后端依赖：
//!
//! 1. `parec`（pulseaudio-utils；PipeWire 桌面的 pipewire-pulse 同样提供）：
//!    pulse 协议层原生识别特殊源名 `@DEFAULT_MONITOR@`（cava 同款写法），
//!    由服务端解析为当前默认 sink 的监视流，无需查询设备名。注意反过来
//!    `pw-record --target @DEFAULT_MONITOR@` 是错的：pw-cat 不认识这个
//!    特殊名，WirePlumber 解析不到节点时会把流退回默认 *麦克风*；
//! 2. `pw-record`（PipeWire 自带，仅在系统里没有 parec 时兜底）：`--target`
//!    必须给默认 sink 的**数字节点 id**（先用 `pactl get-default-sink`
//!    查名字，再用 `pactl list short sinks` 换成 id）。给名字 —— 包括
//!    `<sink>.monitor` —— 会被静默忽略，流照样出数据但内容是默认麦克风。
//!
//! 两者都输出 s16le / 44.1kHz / 双声道的裸 PCM。

use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::spectrum::{Analyzer, RingBuffer, WINDOW_SIZE};

/// 采集参数（pw-record 与 parec 共用的口径）。
const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u32 = 2;
/// 计算节拍：20fps 的可视化足够顺滑，也把 FFT 开销压在主循环之外。
const COMPUTE_INTERVAL: Duration = Duration::from_millis(50);
/// 单次读取的原始字节数（s16le 立体声 → 512 帧）。
const READ_CHUNK_BYTES: usize = 512 * CHANNELS as usize * 2;

/// 采集子进程的包装。它本身不实现 Drop（会在克隆间互相误杀）：持有者
/// （采集线程、静默监测线程、[`super::Visualizer`]）显式 [`Self::kill`]，
/// 让阻塞在 stdout 上的读立刻结束。
#[derive(Clone)]
pub(super) struct CaptureProcess {
    child: Arc<Mutex<Child>>,
    /// 子进程 stderr 的最后若干字节。pulse 工具运行期间 stderr 静默，
    /// 失败原因全在这里 —— 不留着，断流就永远查不出原因。
    stderr_tail: Arc<Mutex<String>>,
}

impl CaptureProcess {
    pub(super) fn spawn(mut command: Command) -> std::io::Result<Self> {
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stderr_tail = Arc::new(Mutex::new(String::new()));
        if let Some(stderr) = child.stderr.take() {
            let tail = Arc::clone(&stderr_tail);
            // 排水线程：stdout 的读取节奏由采集数据决定，stderr 若无人
            // 排水，子进程在写满管道缓冲后会整根卡死。线程随 stderr 关闭
            // （子进程退出）自然结束。
            let drainer = std::thread::Builder::new()
                .name("voicefox-capture-stderr".to_string())
                .spawn(move || drain_stderr(stderr, tail));
            if drainer.is_err() {
                // 线程起不来：诊断信息丢弃，不影响采集本身。
                tracing::warn!("采集 stderr 排水线程启动失败，断流时将没有 stderr 详情");
            }
        }
        Ok(Self {
            child: Arc::new(Mutex::new(child)),
            stderr_tail,
        })
    }

    fn take_stdout(&self) -> Option<ChildStdout> {
        self.child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stdout
            .take()
    }

    pub(super) fn kill(&self) {
        let mut child = self
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = child.kill();
        let _ = child.wait();
    }

    /// 描述子进程的退出状态与 stderr 尾部（断流日志用）。
    ///
    /// 必须在 [`Self::kill`] 之前调用，否则状态会变成"被 SIGKILL"。
    /// 子进程刚退出时排水线程可能还没把 stderr 尾部落盘，此时给一次
    /// 100ms 的宽限再取。
    fn describe_exit(&self) -> String {
        let status = self
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .try_wait()
            .ok()
            .flatten();
        let mut stderr = self.stderr_tail();
        if status.is_some() && stderr.is_empty() {
            std::thread::sleep(Duration::from_millis(100));
            stderr = self.stderr_tail();
        }
        let stderr_note = if stderr.is_empty() {
            String::new()
        } else {
            format!("，stderr: {stderr}")
        };
        match status {
            Some(status) => format!("（退出={status}{stderr_note}）"),
            None => format!("（子进程仍在运行{stderr_note}）"),
        }
    }

    fn stderr_tail(&self) -> String {
        self.stderr_tail
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// 子进程是否已退出（测试看门狗用；顺手回收僵尸避免测试留 zombie）。
    #[cfg(test)]
    fn is_finished(&self) -> bool {
        self.child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .try_wait()
            .map(|status| status.is_some())
            .unwrap_or(true)
    }
}

/// 持续把子进程 stderr 读进一个有上限的环形尾巴；子进程退出后线程结束。
fn drain_stderr(mut stderr: ChildStderr, tail: Arc<Mutex<String>>) {
    use std::io::Read;
    let mut buffer = [0u8; 512];
    let mut latest: Vec<u8> = Vec::with_capacity(STDERR_TAIL_BYTES);
    loop {
        match stderr.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                latest.extend_from_slice(&buffer[..n]);
                if latest.len() > STDERR_TAIL_BYTES {
                    let overflow = latest.len() - STDERR_TAIL_BYTES;
                    latest.drain(..overflow);
                }
            }
        }
    }
    if let Ok(mut guard) = tail.lock() {
        *guard = String::from_utf8_lossy(&latest).trim().to_string();
    }
}

/// 断流日志里保留的 stderr 尾部长度。
const STDERR_TAIL_BYTES: usize = 1024;
/// 向 pulse 服务端请求的采集延迟（毫秒）。见 [`capture_command`]：不显式
/// 指定时服务端默认缓冲会让首字节迟到约 2 秒。
const LATENCY_MS: u32 = 20;

/// 组装采集命令。返回 `None` 表示系统里没有可用的采集工具。
pub fn capture_command() -> Option<Command> {
    // parec 优先：`@DEFAULT_MONITOR@` 由 pulse 服务端解析（零查询、始终
    // 跟随默认输出）。实测同参数的 pw-record 会把 @DEFAULT_MONITOR@ 当
    // 字面节点名，解析失败后退回默认麦克风 —— 用户反馈"可视化是麦克风
    // 不是桌面音频"的根源。
    //
    // `--latency-msec=20`：不指定时服务端给的默认缓冲很大，实测（PipeWire
    // 1.6.9 + pipewire-pulse）第一个字节要等 **2.02s**；显式要 20ms 后降到
    // 0.03s。差的这 2 秒正好落在"按下 w 之后"和"每次重连之后"——期间没有
    // 数据就画不出柱子，看着就像卡死。
    if which("parec") {
        let mut command = Command::new("parec");
        command
            .arg("-d")
            .arg("@DEFAULT_MONITOR@")
            .arg("--format=s16le")
            .arg(format!("--rate={SAMPLE_RATE}"))
            .arg(format!("--channels={CHANNELS}"))
            .arg(format!("--latency-msec={LATENCY_MS}"));
        return Some(command);
    }
    if which("pw-record") && which("pactl") {
        let Some(target) = pipewire_monitor_target() else {
            tracing::warn!("pw-record 兜底不可用：解析不到默认 sink 的 PipeWire 节点 id");
            return None;
        };
        let mut command = Command::new("pw-record");
        command
            .arg("--raw")
            .arg("--format")
            .arg("s16")
            .arg("--rate")
            .arg(SAMPLE_RATE.to_string())
            .arg("--channels")
            .arg(CHANNELS.to_string())
            .arg("--target")
            .arg(target)
            .arg("-");
        return Some(command);
    }
    None
}

/// `pw-record --target` 的取值：默认 sink 的 PipeWire **节点 id**。
///
/// 必须是数字 id，不能是设备名 —— 实测（PipeWire 1.6.9 + WirePlumber，
/// `pw-link -l` 看真实链路）传 `@DEFAULT_MONITOR@`、`<sink 名>`、甚至
/// `<sink 名>.monitor` 都会被忽略，流被接到默认麦克风
/// （`...usb-...:capture_MONO`）；只有 `--target <sink 的节点 id>` 会连到
/// `alsa_output...:monitor_FL/FR`。名字失效是静默的：流照样出数据，只是
/// 内容不是桌面音频，所以这里宁可不启动也不能退化成麦克风。
fn pipewire_monitor_target() -> Option<String> {
    let sink = Command::new("pactl")
        .arg("get-default-sink")
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|name| !name.is_empty())?;
    let sinks = Command::new("pactl")
        .args(["list", "short", "sinks"])
        .output()
        .ok()?;
    let sinks = String::from_utf8_lossy(&sinks.stdout);
    parse_sink_id(&sinks, &sink).map(str::to_string)
}

/// 从 `pactl list short sinks` 的输出里取指定 sink 的 id。每行是
/// `id  name  driver  spec  state`；pactl 用的 id 就是 PipeWire 全局节点
/// id，可以直接喂给 `pw-record --target`。
fn parse_sink_id<'a>(sinks: &'a str, sink: &str) -> Option<&'a str> {
    sinks.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let id = fields.next()?;
        (fields.next() == Some(sink)).then_some(id)
    })
}

fn which(program: &str) -> bool {
    // 采集命令用 PATH 解析即可；不缓存，可视化是低频开启操作。
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

/// 采集线程主体：读 PCM → 写环形缓冲 → 按 COMPUTE_INTERVAL 出帧。
///
/// 断流（子进程退出 / 读取失败）时会清掉旧帧并自动重启采集进程：设备切换、
/// PipeWire 重启都会让旧的监视流结束；如果就此收工，用户看到的就是一屏永远
/// 不动的柱子（"卡死"）。重启后槽位里始终是当前那枚进程，drop 杀掉它即可。
///
/// 连续失败按指数退避（0.5s → 1s → 2s → 4s，封顶 5s）：音频图抖动时
/// （设备切换、蓝牙 profile 漂移）会出连环断流，固定间隔的重连会在
/// WirePlumber 忙于重建链路时火上浇油，退避给它喘息窗口。
pub fn run_capture(
    current: Arc<Mutex<Option<CaptureProcess>>>,
    shutdown: Arc<AtomicBool>,
    snapshot: Arc<Mutex<Option<super::Snapshot>>>,
    last_data: LastDataMs,
) {
    run_capture_with(current, shutdown, snapshot, last_data, &system_spawn);
}

/// 真实环境下的采集进程来源：按 [`capture_command`] 组装并 spawn。
fn system_spawn() -> Option<CaptureProcess> {
    let command = capture_command()?;
    match CaptureProcess::spawn(command) {
        Ok(process) => Some(process),
        Err(error) => {
            tracing::warn!("频谱采集进程启动失败: {error}");
            None
        }
    }
}

/// [`run_capture`] 的主体，采集进程来源可注入 —— 重连路径（断流后重新
/// spawn）没有真实音频栈也要能测。
fn run_capture_with(
    current: Arc<Mutex<Option<CaptureProcess>>>,
    shutdown: Arc<AtomicBool>,
    snapshot: Arc<Mutex<Option<super::Snapshot>>>,
    last_data: LastDataMs,
    spawner: &dyn Fn() -> Option<CaptureProcess>,
) {
    mark_data(&last_data);
    let mut ring = RingBuffer::new(WINDOW_SIZE * 2);
    let mut analyzer = Analyzer::new(SAMPLE_RATE);
    let mut raw = vec![0u8; READ_CHUNK_BYTES];
    let mut mono = Vec::with_capacity(READ_CHUNK_BYTES / 2);
    let mut consecutive_failures: u32 = 0;

    loop {
        if shutdown.load(Ordering::Acquire) {
            return;
        }

        let process = current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let Some(process) = process else {
            if !spawn_capture(&current, &shutdown, &last_data, spawner) {
                consecutive_failures = consecutive_failures.saturating_add(1);
                sleep_briefly(&shutdown, restart_delay(consecutive_failures));
            }
            continue;
        };
        let Some(mut stdout) = process.take_stdout() else {
            process.kill();
            *current
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            continue;
        };
        let mut last_compute = Instant::now() - COMPUTE_INTERVAL;

        loop {
            if shutdown.load(Ordering::Acquire) {
                return;
            }
            use std::io::Read;
            let read = match stdout.read(&mut raw) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) => {
                    tracing::warn!("频谱采集读取失败: {error}");
                    break;
                }
            };
            // 有字节就喂看门狗：静音 sink 的监视流也会持续输出零样本，
            // 所以"有字节"等价于"流活着"，与是否真的出声无关。
            mark_data(&last_data);
            // 流真的在出数据：连续失败计数归零，退避回到最快档。
            consecutive_failures = 0;
            // s16le 交错立体声 → 单声道 f32（忽略落单的最后一个字节）。
            mono.clear();
            for frame in raw[..read.saturating_sub(read % 4)].chunks_exact(4) {
                let left = i16::from_le_bytes([frame[0], frame[1]]);
                let right = i16::from_le_bytes([frame[2], frame[3]]);
                mono.push((f32::from(left) + f32::from(right)) / (2.0 * f32::from(i16::MAX)));
            }
            ring.push(&mono);
            if last_compute.elapsed() >= COMPUTE_INTERVAL
                && let Some(samples) = ring.latest(WINDOW_SIZE)
            {
                let data = analyzer.analyze_data(&samples);
                let snapshot_value = super::Snapshot {
                    data,
                    peaks: analyzer.peaks().to_vec(),
                    updated_at: Instant::now(),
                };
                if let Ok(mut guard) = snapshot.try_lock() {
                    *guard = Some(snapshot_value);
                }
                last_compute = Instant::now();
            }
        }

        // 流断了：旧帧不能再当实时数据画。先问子进程自己是怎么退出的
        // （describe_exit 必须在 kill 之前，否则只会看到 SIGKILL），
        // 再杀掉旧进程重连。
        drop(stdout);
        let exit_note = process.describe_exit();
        if shutdown.load(Ordering::Acquire) {
            return;
        }
        tracing::info!("频谱采集流结束，尝试重连{exit_note}");
        *snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        process.kill();
        *current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        consecutive_failures = consecutive_failures.saturating_add(1);
        sleep_briefly(&shutdown, restart_delay(consecutive_failures));
    }
}

/// 断流重连间隔：给音频服务切换留时间，也避免失败时空转烧 CPU。
const RESTART_INTERVAL: Duration = Duration::from_millis(500);
/// 连续失败退避的封顶间隔：再失败也不把重连拖得过久，用户切回正常
/// 设备后最多等 5s 就能恢复。
const MAX_RESTART_INTERVAL: Duration = Duration::from_secs(5);

/// 第 `failures` 次连续失败后的重连间隔：0.5s 起步逐次翻倍，5s 封顶。
fn restart_delay(failures: u32) -> Duration {
    let doubles = failures.saturating_sub(1).min(4);
    RESTART_INTERVAL
        .saturating_mul(1u32 << doubles)
        .min(MAX_RESTART_INTERVAL)
}

/// 静默监测口径。监视流"连上了但没有字节"有两种可能：
/// - 本机确实没有声音（sink 挂起、输出设备静默）—— 这是"安静"不是"坏了"，
///   应该把待机舞台画出来，而不是让整个叠加层消失（那才是用户眼里的
///   "按了没反应"）；
/// - 流从未被 WirePlumber 链接（策略竞争、目标设备异常）—— 这种流永远不会
///   自愈，必须重连一次给链接策略新的机会。
///
/// 所以静默的处理分两档：超过 [`IDLE_PUBLISH_AFTER`] 开始发布全零待机帧；
/// 超过 [`IDLE_RECONNECT_AFTER`] 仍无字节才判定未链接，杀掉子进程强制重连。
pub(super) const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(500);
const IDLE_PUBLISH_AFTER: Duration = Duration::from_millis(1500);
const IDLE_RECONNECT_AFTER: Duration = Duration::from_secs(10);

/// 最近一次收到采集字节的时间戳（UNIX 毫秒）。采集线程写、监测线程读。
pub(super) type LastDataMs = Arc<AtomicU64>;

pub(super) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

fn mark_data(last_data: &LastDataMs) {
    last_data.store(now_ms(), Ordering::Release);
}

/// 静默监测线程主体：周期检查 [`LastDataMs`]，按静默时长执行两档动作。
/// 间隔与阈值可注入，测试用极小值。
pub(super) fn run_silence_watch(
    current: Arc<Mutex<Option<CaptureProcess>>>,
    shutdown: Arc<AtomicBool>,
    last_data: LastDataMs,
    snapshot: Arc<Mutex<Option<super::Snapshot>>>,
    interval: Duration,
    publish_after: Duration,
    reconnect_after: Duration,
) {
    let mut was_idle = false;
    loop {
        if shutdown.load(Ordering::Acquire) {
            return;
        }
        std::thread::sleep(interval);
        if shutdown.load(Ordering::Acquire) {
            return;
        }
        let silent_for = now_ms().saturating_sub(last_data.load(Ordering::Acquire));
        if silent_for < publish_after.as_millis() as u64 {
            if was_idle {
                tracing::info!("频谱采集流恢复数据");
                was_idle = false;
            }
            continue;
        }
        if !was_idle {
            tracing::info!("频谱采集流已连接但静默（无字节）：本机没有声音在播，按待机舞台绘制");
            was_idle = true;
        }
        publish_idle_frame(&snapshot);
        if silent_for >= reconnect_after.as_millis() as u64
            && let Some(process) = current
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
        {
            tracing::info!(
                "频谱采集流静默超过 {}s，判定流未被链接，强制重连",
                reconnect_after.as_secs()
            );
            process.kill();
        }
    }
}

/// 发布一帧全零的待机频谱：柱子全平，渲染层画出待机基线，叠加层不消失。
fn publish_idle_frame(snapshot: &Arc<Mutex<Option<super::Snapshot>>>) {
    if let Ok(mut guard) = snapshot.try_lock() {
        *guard = Some(super::Snapshot {
            data: super::VisualizerData {
                spectrum: vec![0.0; super::spectrum::BAND_COUNT],
                ..super::VisualizerData::default()
            },
            peaks: vec![0.0; super::spectrum::BAND_COUNT],
            updated_at: std::time::Instant::now(),
        });
    }
}

/// 用默认参数起静默监测线程（真实参数在 [`IDLE_POLL_INTERVAL`] 等常量里）。
pub(super) fn spawn_silence_watch(
    current: Arc<Mutex<Option<CaptureProcess>>>,
    shutdown: Arc<AtomicBool>,
    last_data: LastDataMs,
    snapshot: Arc<Mutex<Option<super::Snapshot>>>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("voicefox-visualizer-idle".to_string())
        .spawn(move || {
            run_silence_watch(
                current,
                shutdown,
                last_data,
                snapshot,
                IDLE_POLL_INTERVAL,
                IDLE_PUBLISH_AFTER,
                IDLE_RECONNECT_AFTER,
            )
        })
}

/// 尝试把一枚新的采集进程放进槽位；返回 false 表示这次没成功（下轮再试）。
fn spawn_capture(
    current: &Arc<Mutex<Option<CaptureProcess>>>,
    shutdown: &AtomicBool,
    last_data: &LastDataMs,
    spawner: &dyn Fn() -> Option<CaptureProcess>,
) -> bool {
    if shutdown.load(Ordering::Acquire) {
        return true;
    }
    let Some(process) = spawner() else {
        return false;
    };
    // 先喂看门狗再放入槽位：避免看门狗在两步之间读到"新进程 +
    // 旧时间戳"的组合，刚 spawn 就被误杀。
    mark_data(last_data);
    *current
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(process);
    true
}

/// 以 50ms 为粒度小睡到下一次重连；期间响应 shutdown，避免关掉可视化时
/// drop 还要等满整个退避间隔。
fn sleep_briefly(shutdown: &AtomicBool, delay: Duration) {
    let mut waited = Duration::ZERO;
    while waited < delay {
        if shutdown.load(Ordering::Acquire) {
            return;
        }
        let step = Duration::from_millis(50).min(delay - waited);
        std::thread::sleep(step);
        waited += step;
    }
}

#[cfg(test)]
mod backoff_tests {
    use super::*;

    #[test]
    fn restart_delay_doubles_then_caps() {
        assert_eq!(restart_delay(0), RESTART_INTERVAL);
        assert_eq!(restart_delay(1), Duration::from_millis(500));
        assert_eq!(restart_delay(2), Duration::from_millis(1000));
        assert_eq!(restart_delay(3), Duration::from_millis(2000));
        assert_eq!(restart_delay(4), Duration::from_millis(4000));
        assert_eq!(restart_delay(5), MAX_RESTART_INTERVAL);
        assert_eq!(restart_delay(99), MAX_RESTART_INTERVAL);
    }

    #[test]
    fn sleep_briefly_wakes_early_on_shutdown() {
        let shutdown = Arc::new(AtomicBool::new(false));
        let handle = {
            let shutdown = Arc::clone(&shutdown);
            std::thread::spawn(move || {
                let start = std::time::Instant::now();
                sleep_briefly(&shutdown, Duration::from_secs(10));
                start.elapsed()
            })
        };
        std::thread::sleep(Duration::from_millis(120));
        shutdown.store(true, Ordering::Release);
        let elapsed = handle.join().unwrap();
        assert!(
            elapsed < Duration::from_secs(1),
            "shutdown 应立刻唤醒退避睡眠"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_command_prefers_parec_on_pulse_systems() {
        // 本测试只断言命令构造的形状；没有 parec/pw-record 的环境允许 None。
        let Some(command) = capture_command() else {
            return;
        };
        let program = command.get_program().to_string_lossy().to_string();
        let parec_installed = which("parec");
        if parec_installed {
            assert!(
                program.ends_with("parec"),
                "parec 存在时应优先（pulse 层认 @DEFAULT_MONITOR@），实际选中 {program}"
            );
            // 不显式请求小延迟时，服务端默认缓冲会让首字节迟到约 2 秒
            // （见 capture_command 的注释），按下 w 后先空窗两秒。
            let args: Vec<String> = command
                .get_args()
                .map(|arg| arg.to_string_lossy().to_string())
                .collect();
            assert!(
                args.iter().any(|arg| arg.starts_with("--latency-msec=")),
                "parec 必须显式请求低延迟，实际参数 {args:?}"
            );
        } else {
            assert!(program.ends_with("pw-record"));
        }
    }

    #[test]
    fn parse_sink_id_matches_only_the_named_sink() {
        let sinks = "60\talsa_output.old\tPipeWire\ts32le 2ch 48000Hz\tSUSPENDED\n\
                     61\talsa_output.pci-0000_36_00.6.analog-stereo\tPipeWire\ts32le 2ch 48000Hz\tRUNNING\n";
        assert_eq!(
            parse_sink_id(sinks, "alsa_output.pci-0000_36_00.6.analog-stereo"),
            Some("61")
        );
        assert_eq!(parse_sink_id(sinks, "alsa_output.old"), Some("60"));
        assert_eq!(parse_sink_id(sinks, "alsa_output.gone"), None);
    }

    /// pw-record 兜底的目标必须是数字节点 id：传设备名会被静默忽略并退回
    /// 默认麦克风（见 [`pipewire_monitor_target`]），等于把可视化又接回麦上。
    #[test]
    fn pw_record_fallback_targets_a_node_id_not_a_sink_name() {
        let Some(target) = pipewire_monitor_target() else {
            // 机器上没有 pactl/默认 sink：兜底路径本来就不会被选中。
            return;
        };
        assert!(
            !target.is_empty() && target.chars().all(|c| c.is_ascii_digit()),
            "pw-record 兜底目标应是节点 id，实际是 {target}"
        );
    }

    #[test]
    fn silence_watch_publishes_idle_frames_when_stream_is_silent() {
        let current: Arc<Mutex<Option<CaptureProcess>>> = Arc::new(Mutex::new(None));
        let shutdown = Arc::new(AtomicBool::new(false));
        let snapshot: Arc<Mutex<Option<super::super::Snapshot>>> = Arc::new(Mutex::new(None));
        // 时间戳回拨一分钟：模拟"流连上了但一直没出字节"（sink 挂起）。
        let last_data: LastDataMs = Arc::new(AtomicU64::new(now_ms().saturating_sub(60_000)));
        let watcher = {
            let current = Arc::clone(&current);
            let shutdown = Arc::clone(&shutdown);
            let last_data = Arc::clone(&last_data);
            let snapshot = Arc::clone(&snapshot);
            std::thread::spawn(move || {
                run_silence_watch(
                    current,
                    shutdown,
                    last_data,
                    snapshot,
                    Duration::from_millis(10),
                    Duration::from_millis(50),
                    Duration::from_secs(60),
                )
            })
        };
        std::thread::sleep(Duration::from_millis(200));
        shutdown.store(true, Ordering::Release);
        watcher.join().unwrap();

        let frame = snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .expect("静默期应发布待机帧");
        assert_eq!(
            frame.data.spectrum.len(),
            super::super::spectrum::BAND_COUNT
        );
        assert!(
            frame.data.spectrum.iter().all(|level| *level == 0.0),
            "待机帧的频谱应全平"
        );
    }

    #[test]
    fn silence_watch_does_not_publish_while_data_flows() {
        let current: Arc<Mutex<Option<CaptureProcess>>> = Arc::new(Mutex::new(None));
        let shutdown = Arc::new(AtomicBool::new(false));
        let snapshot: Arc<Mutex<Option<super::super::Snapshot>>> = Arc::new(Mutex::new(None));
        // 流持续出数据：观察期内不断刷新 last_data，监测线程不该插手快照。
        let last_data: LastDataMs = Arc::new(AtomicU64::new(now_ms()));
        let stop_feeding = Arc::new(AtomicBool::new(false));
        let feeder = {
            let last_data = Arc::clone(&last_data);
            let stop_feeding = Arc::clone(&stop_feeding);
            std::thread::spawn(move || {
                while !stop_feeding.load(Ordering::Acquire) {
                    mark_data(&last_data);
                    std::thread::sleep(Duration::from_millis(5));
                }
            })
        };
        let watcher = {
            let current = Arc::clone(&current);
            let shutdown = Arc::clone(&shutdown);
            let last_data = Arc::clone(&last_data);
            let snapshot = Arc::clone(&snapshot);
            std::thread::spawn(move || {
                run_silence_watch(
                    current,
                    shutdown,
                    last_data,
                    snapshot,
                    Duration::from_millis(10),
                    Duration::from_millis(50),
                    Duration::from_secs(60),
                )
            })
        };
        std::thread::sleep(Duration::from_millis(150));
        stop_feeding.store(true, Ordering::Release);
        feeder.join().unwrap();
        shutdown.store(true, Ordering::Release);
        watcher.join().unwrap();

        assert!(
            snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_none(),
            "数据正常流动时不应发布待机帧"
        );
    }

    #[test]
    fn silence_watch_reconnects_after_prolonged_silence() {
        let mut command = Command::new("sh");
        command.args(["-c", "exec sleep 60"]);
        let process = CaptureProcess::spawn(command).unwrap();
        let current = Arc::new(Mutex::new(Some(process)));
        let shutdown = Arc::new(AtomicBool::new(false));
        let snapshot: Arc<Mutex<Option<super::super::Snapshot>>> = Arc::new(Mutex::new(None));
        // 静默远超重连阈值：监测线程应杀掉进程，交还重连逻辑。
        let last_data: LastDataMs = Arc::new(AtomicU64::new(now_ms().saturating_sub(60_000)));
        let watcher = {
            let current = Arc::clone(&current);
            let shutdown = Arc::clone(&shutdown);
            let last_data = Arc::clone(&last_data);
            let snapshot = Arc::clone(&snapshot);
            std::thread::spawn(move || {
                run_silence_watch(
                    current,
                    shutdown,
                    last_data,
                    snapshot,
                    Duration::from_millis(10),
                    Duration::from_millis(50),
                    Duration::from_millis(80),
                )
            })
        };
        std::thread::sleep(Duration::from_millis(200));
        shutdown.store(true, Ordering::Release);
        watcher.join().unwrap();

        let guard = current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let process = guard
            .as_ref()
            .expect("槽位里的进程应仍存在（重生由采集线程负责）");
        assert!(process.is_finished(), "长期静默的流应被强制重连（SIGKILL）");
    }

    #[cfg(unix)]
    #[test]
    fn killing_capture_process_unblocks_stdout_read() {
        let mut command = Command::new("sh");
        command.args(["-c", "exec sleep 60"]);
        let process = CaptureProcess::spawn(command).unwrap();
        let mut stdout = process.take_stdout().unwrap();
        let reader = std::thread::spawn(move || {
            use std::io::Read;
            let mut byte = [0u8; 1];
            stdout.read(&mut byte)
        });

        process.kill();

        assert_eq!(reader.join().unwrap().unwrap(), 0);
    }

    #[test]
    fn read_chunk_covers_at_least_one_window_per_second() {
        // 44.1kHz × 512 帧/读 → 每秒约 86 次读，50ms 节拍下窗口填充绰绰有余。
        let chunks_per_second = SAMPLE_RATE as f32 / 512.0;
        assert!(chunks_per_second > 1.0 / COMPUTE_INTERVAL.as_secs_f32());
    }

    /// 断流后必须自动重连：监视流结束（设备切换、PipeWire 重启）时若不再
    /// 起新进程，用户看到的就是一屏永远不动的柱子 —— 这正是"卡死"的另一半。
    /// 用一个"吐一小段 PCM 就退出"的假采集器（不需要真实音频栈，CI 可跑），
    /// 断言 run_capture 在 EOF 后真的又 spawn 了采集进程。
    #[cfg(unix)]
    #[test]
    fn run_capture_respawns_after_stream_ends() {
        let spawns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let spawner = {
            let spawns = Arc::clone(&spawns);
            move || {
                spawns.fetch_add(1, Ordering::AcqRel);
                let mut command = Command::new("sh");
                // 一小段 PCM 后自然退出，模拟监视流结束。
                command.args(["-c", "head -c 8192 /dev/zero"]);
                CaptureProcess::spawn(command).ok()
            }
        };
        let current: Arc<Mutex<Option<CaptureProcess>>> = Arc::new(Mutex::new(None));
        let shutdown = Arc::new(AtomicBool::new(false));
        let snapshot: Arc<Mutex<Option<super::super::Snapshot>>> = Arc::new(Mutex::new(None));
        let last_data: LastDataMs = Arc::new(AtomicU64::new(now_ms()));
        let worker = {
            let current = Arc::clone(&current);
            let shutdown = Arc::clone(&shutdown);
            let snapshot = Arc::clone(&snapshot);
            let last_data = Arc::clone(&last_data);
            std::thread::spawn(move || {
                run_capture_with(current, shutdown, snapshot, last_data, &spawner)
            })
        };

        let deadline = Instant::now() + Duration::from_secs(5);
        while spawns.load(Ordering::Acquire) < 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        shutdown.store(true, Ordering::Release);
        if let Some(process) = current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            process.kill();
        }
        worker.join().unwrap();

        assert!(
            spawns.load(Ordering::Acquire) >= 2,
            "流结束后应自动重连（重新 spawn 采集进程），否则柱子永远停在一帧上"
        );
    }

    /// 端到端冒烟：真实起 pw-record 采 2 秒系统音频，断言产出了一帧合法频谱。
    /// 静音环境下柱子全 0 也算通过 —— 验证的是"链路通"，不是"有声"。
    #[test]
    #[ignore = "需要系统音频栈（pipewire/pulse），CI 无设备"]
    fn capture_stream_produces_valid_spectrum_frame() {
        use std::sync::Mutex;

        let Some(command) = capture_command() else {
            panic!("本机应提供 pw-record 或 parec");
        };
        let shutdown = Arc::new(AtomicBool::new(false));
        let snapshot = Arc::new(Mutex::new(None));
        let last_data: LastDataMs = Arc::new(AtomicU64::new(now_ms()));
        let current = Arc::new(Mutex::new(Some(CaptureProcess::spawn(command).unwrap())));
        let worker_current = Arc::clone(&current);
        let worker = {
            let shutdown = Arc::clone(&shutdown);
            let snapshot = Arc::clone(&snapshot);
            let last_data = Arc::clone(&last_data);
            std::thread::spawn(move || run_capture(worker_current, shutdown, snapshot, last_data))
        };
        std::thread::sleep(Duration::from_secs(2));
        shutdown.store(true, Ordering::Release);
        if let Some(process) = current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            process.kill();
        }
        let _ = worker.join();

        let frame = snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .expect("capture should publish a frame within 2s");
        assert_eq!(
            frame.data.spectrum.len(),
            super::super::spectrum::BAND_COUNT
        );
        assert_eq!(frame.peaks.len(), super::super::spectrum::BAND_COUNT);
        let frame_peak = frame
            .data
            .spectrum
            .iter()
            .fold(0.0f32, |acc, level| acc.max(*level));
        println!("频谱帧峰值电平 = {frame_peak:.3}（静音环境应接近 0）");
        assert!(
            frame
                .data
                .spectrum
                .iter()
                .chain(frame.peaks.iter())
                .all(|level| (0.0..=1.0).contains(level))
        );
    }
}
