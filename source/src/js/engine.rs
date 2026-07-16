//! JS 引擎：通过 Node.js 子进程运行 lx-music user API 脚本
//!
//! 通信协议: stdin/stdout JSON Lines (每行一个 JSON 对象)
//!
//! 请求格式: {"type":"call","id":0,"action":"musicUrl","source":"kw","info":{...}}
//! 响应格式: {"id":0,"result":{...}} 或 {"id":0,"error":"..."}

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// JS 引擎：封装一个 Node.js 子进程运行用户音源脚本
pub struct JsEngine {
    /// Node.js 子进程
    _child: Mutex<Child>,
    /// stdin 写入端
    stdin: Mutex<ChildStdin>,
    /// stdout 读取端
    stdout: Mutex<BufReader<std::process::ChildStdout>>,
    /// 请求 ID 计数器
    req_id: AtomicU64,
    /// 当前脚本路径，用于子进程异常后的重启
    source_path: String,
    /// 脚本通过 EVENT_NAMES.inited 声明的能力
    init_info: Mutex<Option<serde_json::Value>>,
}

impl JsEngine {
    /// 创建 JS 引擎，启动 Node.js 子进程加载指定脚本
    ///
    /// * `source_path` - 用户音源 JS 文件的路径
    pub fn new(source_path: &str) -> Result<Self, String> {
        let wrapper_js = include_str!("wrapper.js");

        // 将 wrapper.js 写入临时文件
        let tmp_dir = std::env::temp_dir().join("lx-tui-js");
        std::fs::create_dir_all(&tmp_dir)
            .map_err(|e| format!("无法创建临时目录: {}", e))?;
        let wrapper_path = tmp_dir.join("wrapper.js");
        std::fs::write(&wrapper_path, wrapper_js)
            .map_err(|e| format!("无法写入 wrapper.js: {}", e))?;

        // 启动 Node.js 子进程
        let mut child = Command::new("node")
            .arg(wrapper_path.to_str().unwrap())
            .arg(source_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("启动 Node.js 失败（请确认已安装 Node.js）: {}", e))?;

        let stdin = child
            .stdin
            .take()
            .ok_or("无法获取子进程 stdin")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("无法获取子进程 stdout")?;

        let engine = Self {
            _child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(BufReader::new(stdout)),
            req_id: AtomicU64::new(0),
            source_path: source_path.to_string(),
            init_info: Mutex::new(None),
        };

        engine
            .ping()
            .map_err(|e| format!("JS 引擎启动失败: {}", e))?;
        engine
            .wait_initialized()
            .map_err(|e| format!("JS 音源初始化失败: {}", e))?;

        Ok(engine)
    }

    pub fn source_path(&self) -> &str {
        &self.source_path
    }

    pub fn supported_qualities(&self, source: &str) -> Vec<String> {
        self.init_info
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
            .and_then(|info| info["sources"][source]["qualitys"].as_array().cloned())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|quality| quality.as_str().map(str::to_string))
            .collect()
    }

    pub fn supports_source(&self, source: &str) -> bool {
        self.init_info
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
            .and_then(|info| info["sources"][source]["actions"].as_array().cloned())
            .is_some_and(|actions| {
                actions
                    .iter()
                    .any(|action| action.as_str() == Some("musicUrl"))
            })
    }

    /// Ping 子进程验证连接
    fn ping(&self) -> Result<(), String> {
        let id = self.req_id.fetch_add(1, Ordering::SeqCst);
        let cmd = serde_json::json!({ "type": "ping", "id": id });

        self.send_command(&cmd)?;
        self.wait_response(id, std::time::Duration::from_secs(5))
            .map(|_| ())
    }

    fn wait_initialized(&self) -> Result<(), String> {
        if self.init_info.lock().map_err(|e| e.to_string())?.is_some() {
            return Ok(());
        }

        let mut stdout = self
            .stdout
            .lock()
            .map_err(|e| format!("stdout lock error: {}", e))?;
        let mut line = String::new();
        loop {
            line.clear();
            let count = stdout
                .read_line(&mut line)
                .map_err(|e| format!("读取子进程输出失败: {}", e))?;
            if count == 0 {
                return Err("JS 引擎子进程已退出".to_string());
            }

            let response: serde_json::Value = match serde_json::from_str(line.trim()) {
                Ok(value) => value,
                Err(_) => continue,
            };
            if response["type"].as_str() == Some("initError") {
                return Err(response["error"]
                    .as_str()
                    .unwrap_or("未知初始化错误")
                    .to_string());
            }
            if response["type"].as_str() == Some("event")
                && response["event"].as_str() == Some("inited")
            {
                self.record_init(response["data"].clone())?;
                return Ok(());
            }
        }
    }

    fn record_init(&self, info: serde_json::Value) -> Result<(), String> {
        let sources = info
            .get("sources")
            .and_then(|value| value.as_object())
            .ok_or_else(|| "音源未声明 sources".to_string())?;
        if sources.is_empty() {
            return Err("音源未声明任何可用平台".to_string());
        }
        *self.init_info.lock().map_err(|e| e.to_string())? = Some(info);
        Ok(())
    }

    /// 调用 JS 处理器
    ///
    /// * `action` - 操作类型: "musicUrl", "lyric", "pic", "search"
    /// * `source` - 平台标识: "kw", "kg", "tx", "wy", "mg"
    /// * `info`  - 请求参数（JSON 对象）
    pub fn call_action(
        &self,
        action: &str,
        source: &str,
        info: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let id = self.req_id.fetch_add(1, Ordering::SeqCst);
        let cmd = serde_json::json!({
            "type": "call",
            "id": id,
            "action": action,
            "source": source,
            "info": info,
        });

        self.send_command(&cmd)?;
        self.wait_response(id, std::time::Duration::from_secs(16))
    }

    /// 发送 JSON 命令到子进程 stdin
    fn send_command(&self, cmd: &serde_json::Value) -> Result<(), String> {
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|e| format!("stdin lock error: {}", e))?;
        let line = serde_json::to_string(cmd)
            .map_err(|e| format!("序列化命令失败: {}", e))?;
        writeln!(stdin, "{}", line)
            .map_err(|e| format!("写入子进程失败: {}", e))?;
        Ok(())
    }

    /// 等待并匹配响应 ID
    fn wait_response(
        &self,
        expected_id: u64,
        timeout: std::time::Duration,
    ) -> Result<serde_json::Value, String> {
        let start = std::time::Instant::now();
        let mut stdout = self
            .stdout
            .lock()
            .map_err(|e| format!("stdout lock error: {}", e))?;

        let mut line = String::new();
        loop {
            if start.elapsed() > timeout {
                return Err("JS 引擎响应超时".to_string());
            }

            line.clear();
            let n = stdout
                .read_line(&mut line)
                .map_err(|e| format!("读取子进程输出失败: {}", e))?;

            if n == 0 {
                return Err("JS 引擎子进程已退出".to_string());
            }

            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            // 尝试解析为 JSON
            let resp: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue, // 跳过非 JSON 行
            };

            if resp.get("type").and_then(|v| v.as_str()) == Some("initError") {
                return Err(resp["error"]
                    .as_str()
                    .unwrap_or("JS 音源初始化失败")
                    .to_string());
            }

            // 检查是否是事件消息（非响应）
            if resp.get("type").and_then(|v| v.as_str()) == Some("event") {
                if resp.get("event").and_then(|v| v.as_str()) == Some("inited") {
                    self.record_init(resp["data"].clone())?;
                }
                continue;
            }

            // 匹配响应 ID
            if let Some(id) = resp.get("id").and_then(|v| v.as_u64()) {
                if id == expected_id {
                    if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
                        return Err(err.to_string());
                    }
                    return Ok(resp["result"].clone());
                }
                // 不匹配的响应跳过（可能是之前的响应）
            }
        }
    }
}

impl Drop for JsEngine {
    fn drop(&mut self) {
        // 尝试优雅关闭子进程
        if let Ok(mut child) = self._child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
