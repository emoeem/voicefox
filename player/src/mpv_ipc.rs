//! mpv 子进程 + JSON IPC 通信
//!
//! 方案：启动 mpv --input-ipc-server=/tmp/lx-tui-mpv-{pid}
//! 通过 Unix socket 发送 JSON 命令，解析 JSON 事件
//!
//! 参考：go-musicfox internal/player/mpv_player.go

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::mpsc;

/// mpv IPC 客户端——管理子进程 + Unix socket 通信
pub struct MpvIpc {
    /// 子进程句柄（mutex 包裹以支持 &self 操作）
    process: Mutex<std::process::Child>,
    /// Unix socket 路径
    socket_path: String,
    /// 命令连接（发送命令 + 读取响应）
    cmd_conn: Mutex<UnixStream>,
    /// mpv 事件接收端（end-file 等），take 后为 None
    event_rx: Mutex<Option<mpsc::UnboundedReceiver<MpvEvent>>>,
    /// 事件监听线程句柄（仅用于 Sync，不 join）
    _event_handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

/// mpv 事件
#[derive(Debug, Clone)]
pub enum MpvEvent {
    /// 播放结束
    EndFile,
}

/// mpv IPC 错误
#[derive(Debug, thiserror::Error)]
pub enum MpvError {
    #[error("mpv not found")]
    NotFound,
    #[error("IPC error: {0}")]
    Ipc(String),
}

impl MpvIpc {
    /// 启动 mpv 进程，建立 IPC 连接
    ///
    /// * `url` - 可选的初始播放 URL
    pub fn start(url: Option<&str>) -> Result<Self, MpvError> {
        let pid = std::process::id();
        let socket_path = format!("/tmp/lx-tui-mpv-{}.sock", pid);

        // 清理残留的 socket 文件
        let _ = std::fs::remove_file(&socket_path);

        // 构建 mpv 参数
        let mut args: Vec<String> = vec![
            "--no-video".into(),
            "--no-terminal".into(),
            format!("--input-ipc-server={}", socket_path),
            "--idle".into(),
            "--cache=yes".into(),
            "--volume=80".into(),
            "--audio-device=auto".into(),
        ];

        if let Some(u) = url {
            args.push(u.to_string());
        }

        // 启动 mpv（静默输出）
        let mut process = std::process::Command::new("mpv")
            .args(&args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    MpvError::NotFound
                } else {
                    MpvError::Ipc(format!("failed to start mpv: {}", e))
                }
            })?;

        // 轮询等待 socket 文件出现（最多 3 秒）
        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(3);
        loop {
            if std::path::Path::new(&socket_path).exists() {
                break;
            }
            if start.elapsed() >= timeout {
                let _ = process.kill();
                let _ = process.wait();
                return Err(MpvError::Ipc("timeout waiting for mpv socket".into()));
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        // 连接命令 socket
        let cmd_conn = UnixStream::connect(&socket_path)
            .map_err(|e| MpvError::Ipc(format!("failed to connect to mpv socket: {}", e)))?;
        cmd_conn
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|e| MpvError::Ipc(e.to_string()))?;
        cmd_conn
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|e| MpvError::Ipc(e.to_string()))?;

        // 创建事件通道
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        // 启动事件监听线程（独立的 socket 连接）
        let socket_path_clone = socket_path.clone();
        let event_handle = std::thread::spawn(move || {
            let conn = match UnixStream::connect(&socket_path_clone) {
                Ok(c) => c,
                Err(_) => return,
            };
            let mut reader = BufReader::new(conn);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let is_natural_end = serde_json::from_str::<serde_json::Value>(&line)
                            .ok()
                            .is_some_and(|event| {
                                event["event"].as_str() == Some("end-file")
                                    && event["reason"].as_str() == Some("eof")
                            });
                        if is_natural_end {
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
            event_rx: Mutex::new(Some(event_rx)),
            _event_handle: Mutex::new(Some(event_handle)),
        })
    }

    /// 发送 JSON 命令（不读响应）
    pub fn send_command(&self, cmd: &str) -> Result<(), MpvError> {
        let mut conn = self.cmd_conn.lock().unwrap();
        conn.set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|e| MpvError::Ipc(e.to_string()))?;
        conn.write_all(cmd.as_bytes())
            .map_err(|e| MpvError::Ipc(e.to_string()))?;
        conn.write_all(b"\n")
            .map_err(|e| MpvError::Ipc(e.to_string()))?;
        Ok(())
    }

    /// 获取 mpv 属性值，返回原始 JSON 响应字符串
    pub fn get_property(&self, name: &str) -> Result<String, MpvError> {
        let cmd = format!("{{\"command\": [\"get_property\", \"{}\"]}}", name);

        let mut conn = self.cmd_conn.lock().unwrap();
        conn.set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|e| MpvError::Ipc(e.to_string()))?;
        conn.write_all(cmd.as_bytes())
            .map_err(|e| MpvError::Ipc(e.to_string()))?;
        conn.write_all(b"\n")
            .map_err(|e| MpvError::Ipc(e.to_string()))?;

        conn.set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|e| MpvError::Ipc(e.to_string()))?;

        let mut reader = BufReader::new(&*conn);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|e| MpvError::Ipc(format!("read error: {}", e)))?;

        Ok(line.trim().to_string())
    }

    /// 取出事件接收端（仅可调用一次）
    pub fn event_receiver(&self) -> Option<mpsc::UnboundedReceiver<MpvEvent>> {
        self.event_rx.lock().unwrap().take()
    }

    /// 设置音量 (0-100)
    pub fn set_volume(&self, vol: u32) -> Result<(), MpvError> {
        let cmd = format!("{{\"command\": [\"set_property\", \"volume\", {}]}}", vol);
        self.send_command(&cmd)
    }

    /// 停止 mpv 进程并清理 socket
    pub fn stop(&self) {
        if let Ok(mut child) = self.process.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

impl Drop for MpvIpc {
    fn drop(&mut self) {
        self.stop();
    }
}
