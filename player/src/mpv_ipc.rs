//! mpv 子进程 + JSON IPC 通信
//!
//! Unix: 通过 Unix 域套接字通信
//! Windows: 通过命名管道通信（interprocess 库）
//!
//! 参考：go-musicfox internal/player/mpv_player.go

use std::io::{BufRead, BufReader, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

#[cfg(windows)]
use interprocess::TryClone as _;
#[cfg(windows)]
use interprocess::local_socket::Stream as SocketStream;
#[cfg(windows)]
use interprocess::local_socket::traits::Stream as _;
#[cfg(windows)]
use interprocess::local_socket::{GenericNamespaced, ToNsName as _};
/// 平台相关的 socket 类型
#[cfg(unix)]
use std::os::unix::net::UnixStream as SocketStream;

/// mpv IPC 客户端
pub struct MpvIpc {
    process: Mutex<std::process::Child>,
    #[cfg(unix)]
    socket_path: String,
    cmd_conn: Mutex<SocketStream>,
    query_conn: Mutex<SocketStream>,
    query_rx: Mutex<std::sync::mpsc::Receiver<String>>,
    query_lock: Mutex<()>,
    query_request_id: AtomicU64,
    event_rx: Mutex<Option<mpsc::UnboundedReceiver<MpvEvent>>>,
    _cmd_handle: Mutex<Option<std::thread::JoinHandle<()>>>,
    _query_handle: Mutex<Option<std::thread::JoinHandle<()>>>,
    _event_handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

#[derive(Debug, Clone)]
pub enum MpvEvent {
    EndFile,
    Error(String),
}

#[derive(Debug, thiserror::Error)]
pub enum MpvError {
    #[error("mpv not found")]
    NotFound,
    #[error("IPC error: {0}")]
    Ipc(String),
}

impl MpvIpc {
    /// 生成跨平台 socket 路径
    #[cfg(unix)]
    fn make_path(pid: u32) -> String {
        format!("/tmp/voicefox-mpv-{}.sock", pid)
    }
    #[cfg(windows)]
    fn make_path(pid: u32) -> String {
        format!("VoicefoxMpv{}", pid)
    }

    /// mpv 的 --input-ipc-server 参数值
    fn ipc_arg(path: &str) -> String {
        #[cfg(unix)]
        {
            path.to_string()
        }
        #[cfg(windows)]
        {
            format!("\\\\.\\pipe\\{}", path)
        }
    }

    /// 连接本地 socket
    fn connect(path: &str) -> Result<SocketStream, String> {
        #[cfg(unix)]
        {
            std::os::unix::net::UnixStream::connect(path).map_err(|e| format!("connect: {e}"))
        }
        #[cfg(windows)]
        {
            let name = path
                .to_ns_name::<GenericNamespaced>()
                .map_err(|e| format!("invalid pipe name: {e}"))?;
            interprocess::local_socket::Stream::connect(name).map_err(|e| format!("connect: {e}"))
        }
    }

    pub fn start(url: Option<&str>) -> Result<Self, MpvError> {
        let pid = std::process::id();
        let socket_path = Self::make_path(pid);

        #[cfg(unix)]
        let _ = std::fs::remove_file(&socket_path);

        let mut args: Vec<String> = vec![
            "--no-video".into(),
            "--no-terminal".into(),
            format!("--input-ipc-server={}", Self::ipc_arg(&socket_path)),
            "--idle".into(),
            "--cache=yes".into(),
            "--volume=80".into(),
            "--audio-device=auto".into(),
        ];
        if let Some(u) = url {
            args.push(u.to_string());
        }

        let mut process = std::process::Command::new("mpv")
            .args(&args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    MpvError::NotFound
                } else {
                    MpvError::Ipc(format!("start mpv: {}", e))
                }
            })?;

        // 等待 socket 就绪
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            #[cfg(unix)]
            let ready = std::path::Path::new(&socket_path).exists();
            #[cfg(windows)]
            let ready = Self::connect(&socket_path).is_ok();
            if ready {
                break;
            }
            if Instant::now() >= deadline {
                let _ = process.kill();
                let _ = process.wait();
                return Err(MpvError::Ipc("timeout waiting for mpv socket".into()));
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        // 连接命令 socket
        let cmd_conn = Self::connect(&socket_path).map_err(MpvError::Ipc)?;
        #[cfg(unix)]
        let _ = cmd_conn.set_write_timeout(Some(Duration::from_millis(150)));

        // 排空线程
        let cmd_reader = cmd_conn
            .try_clone()
            .map_err(|e| MpvError::Ipc(format!("clone: {e}")))?;
        let cmd_handle = std::thread::spawn(move || {
            let mut reader = BufReader::new(cmd_reader);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });

        // 查询 socket
        let query_conn = Self::connect(&socket_path).map_err(MpvError::Ipc)?;
        let query_reader = query_conn
            .try_clone()
            .map_err(|e| MpvError::Ipc(format!("clone query socket: {e}")))?;
        let (query_tx, query_rx) = std::sync::mpsc::channel();
        let query_handle = std::thread::spawn(move || {
            let mut reader = BufReader::new(query_reader);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if query_tx.send(line.trim().to_string()).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        // 事件监听
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let event_conn = Self::connect(&socket_path).map_err(MpvError::Ipc)?;
        let event_handle = std::thread::spawn(move || {
            let mut reader = BufReader::new(event_conn);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if let Some(event) = parse_mpv_event(&line) {
                            let _ = event_tx.send(event);
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            process: Mutex::new(process),
            #[cfg(unix)]
            socket_path,
            cmd_conn: Mutex::new(cmd_conn),
            query_conn: Mutex::new(query_conn),
            query_rx: Mutex::new(query_rx),
            query_lock: Mutex::new(()),
            query_request_id: AtomicU64::new(0),
            event_rx: Mutex::new(Some(event_rx)),
            _cmd_handle: Mutex::new(Some(cmd_handle)),
            _query_handle: Mutex::new(Some(query_handle)),
            _event_handle: Mutex::new(Some(event_handle)),
        })
    }

    pub fn send_command(&self, cmd: &str) -> Result<(), MpvError> {
        let mut conn = self.cmd_conn.lock().unwrap();
        conn.write_all(cmd.as_bytes())
            .and_then(|_| conn.write_all(b"\n"))
            .and_then(|_| conn.flush())
            .map_err(|e| MpvError::Ipc(e.to_string()))
    }

    pub fn get_property(&self, name: &str) -> Result<String, MpvError> {
        let _query_guard = self.query_lock.lock().unwrap();
        let request_id = self.query_request_id.fetch_add(1, Ordering::Relaxed) + 1;
        let cmd = serde_json::json!({
            "command": ["get_property", name],
            "request_id": request_id,
        })
        .to_string();

        {
            let mut conn = self.query_conn.lock().unwrap();
            conn.write_all(cmd.as_bytes())
                .and_then(|_| conn.write_all(b"\n"))
                .and_then(|_| conn.flush())
                .map_err(|e| MpvError::Ipc(e.to_string()))?;
        }

        let deadline = Instant::now() + Duration::from_millis(500);
        let responses = self.query_rx.lock().unwrap();
        loop {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(MpvError::Ipc(format!("timeout reading property {name}")));
            };
            let line = responses
                .recv_timeout(remaining)
                .map_err(|error| match error {
                    std::sync::mpsc::RecvTimeoutError::Timeout => {
                        MpvError::Ipc(format!("timeout reading property {name}"))
                    }
                    std::sync::mpsc::RecvTimeoutError::Disconnected => {
                        MpvError::Ipc("mpv query socket closed".to_string())
                    }
                })?;
            if serde_json::from_str::<serde_json::Value>(&line)
                .ok()
                .and_then(|v| v["request_id"].as_u64())
                == Some(request_id)
            {
                return Ok(line);
            }
        }
    }

    pub fn event_receiver(&self) -> Option<mpsc::UnboundedReceiver<MpvEvent>> {
        self.event_rx.lock().unwrap().take()
    }

    pub fn set_volume(&self, vol: u32) -> Result<(), MpvError> {
        self.send_command(&format!(
            "{{\"command\": [\"set_property\", \"volume\", {}]}}",
            vol
        ))
    }

    pub fn stop(&self) {
        if let Ok(mut child) = self.process.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

fn parse_mpv_event(line: &str) -> Option<MpvEvent> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    if value["event"].as_str() != Some("end-file") {
        return None;
    }
    match value["reason"].as_str() {
        Some("eof") => Some(MpvEvent::EndFile),
        Some("error") => Some(MpvEvent::Error(
            value["file_error"]
                .as_str()
                .unwrap_or("mpv 无法打开当前音频")
                .to_string(),
        )),
        _ => None,
    }
}

impl Drop for MpvIpc {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::{MpvEvent, parse_mpv_event};

    #[test]
    fn parses_end_file_errors() {
        let event = parse_mpv_event(
            r#"{"event":"end-file","reason":"error","file_error":"connection failed"}"#,
        );
        assert!(matches!(
            event,
            Some(MpvEvent::Error(message)) if message == "connection failed"
        ));
    }

    #[test]
    fn ignores_manual_stop_events() {
        assert!(parse_mpv_event(r#"{"event":"end-file","reason":"stop"}"#).is_none());
    }
}
