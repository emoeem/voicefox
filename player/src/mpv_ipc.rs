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

/// 平台相关的 socket 类型
#[cfg(unix)]
use std::os::unix::net::UnixStream as SocketStream;
#[cfg(windows)]
use interprocess::local_socket::Stream as SocketStream;

/// mpv IPC 客户端
pub struct MpvIpc {
    process: Mutex<std::process::Child>,
    socket_path: String,
    cmd_conn: Mutex<SocketStream>,
    query_conn: Mutex<BufReader<SocketStream>>,
    query_request_id: AtomicU64,
    event_rx: Mutex<Option<mpsc::UnboundedReceiver<MpvEvent>>>,
    _cmd_handle: Mutex<Option<std::thread::JoinHandle<()>>>,
    _event_handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

#[derive(Debug, Clone)]
pub enum MpvEvent {
    EndFile,
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
        { path.to_string() }
        #[cfg(windows)]
        { format!("\\\\.\\pipe\\{}", path) }
    }

    /// 连接本地 socket
    fn connect(path: &str) -> Result<SocketStream, String> {
        #[cfg(unix)]
        {
            std::os::unix::net::UnixStream::connect(path)
                .map_err(|e| format!("connect: {e}"))
        }
        #[cfg(windows)]
        {
            interprocess::local_socket::Stream::connect(path)
                .map_err(|e| format!("connect: {e}"))
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
        let cmd_conn = Self::connect(&socket_path)
            .map_err(|e| MpvError::Ipc(e))?;
        #[cfg(unix)]
        let _ = cmd_conn.set_write_timeout(Some(Duration::from_millis(150)));

        // 排空线程
        let cmd_reader = cmd_conn.try_clone()
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
        let query_conn = Self::connect(&socket_path)
            .map_err(|e| MpvError::Ipc(e))?;

        // 事件监听
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let sp_clone = socket_path.clone();
        let event_handle = std::thread::spawn(move || {
            let conn = match Self::connect(&sp_clone) {
                Ok(c) => c,
                Err(_) => return,
            };
            let mut reader = BufReader::new(conn);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if serde_json::from_str::<serde_json::Value>(&line).ok()
                            .is_some_and(|v| {
                                v["event"].as_str() == Some("end-file")
                                    && v["reason"].as_str() == Some("eof")
                            })
                        {
                            let _ = event_tx.send(MpvEvent::EndFile);
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            process: Mutex::new(process),
            socket_path,
            cmd_conn: Mutex::new(cmd_conn),
            query_conn: Mutex::new(BufReader::new(query_conn)),
            query_request_id: AtomicU64::new(0),
            event_rx: Mutex::new(Some(event_rx)),
            _cmd_handle: Mutex::new(Some(cmd_handle)),
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
        let request_id = self.query_request_id.fetch_add(1, Ordering::Relaxed) + 1;
        let cmd = serde_json::json!({
            "command": ["get_property", name],
            "request_id": request_id,
        }).to_string();

        let mut conn = self.query_conn.lock().unwrap();
        conn.get_mut().write_all(cmd.as_bytes())
            .and_then(|_| conn.get_mut().write_all(b"\n"))
            .and_then(|_| conn.get_mut().flush())
            .map_err(|e| MpvError::Ipc(e.to_string()))?;

        let deadline = Instant::now() + Duration::from_millis(500);
        let mut line = String::new();
        loop {
            if Instant::now() >= deadline {
                return Err(MpvError::Ipc(format!("timeout reading property {name}")));
            }
            line.clear();
            if conn.read_line(&mut line).is_err() || line.is_empty() {
                return Err(MpvError::Ipc("mpv query socket closed".to_string()));
            }
            if serde_json::from_str::<serde_json::Value>(&line).ok()
                .and_then(|v| v["request_id"].as_u64())
                == Some(request_id)
            {
                return Ok(line.trim().to_string());
            }
        }
    }

    pub fn event_receiver(&self) -> Option<mpsc::UnboundedReceiver<MpvEvent>> {
        self.event_rx.lock().unwrap().take()
    }

    pub fn set_volume(&self, vol: u32) -> Result<(), MpvError> {
        self.send_command(&format!(
            "{{\"command\": [\"set_property\", \"volume\", {}]}}", vol
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

impl Drop for MpvIpc {
    fn drop(&mut self) {
        self.stop();
    }
}
