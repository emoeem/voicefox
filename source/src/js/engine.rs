//! JS 引擎：通过 Node.js 子进程运行 lx-music user API 脚本
//!
//! 通信协议: stdin/stdout JSON Lines (每行一个 JSON 对象)
//!
//! 请求格式: {"type":"call","id":0,"action":"musicUrl","source":"kw","info":{...}}
//! 响应格式: {"id":0,"result":{...}} 或 {"id":0,"error":"..."}

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 诊断信息里保留的子进程 stderr 尾部长度（字节）。
const STDERR_TAIL_BYTES: usize = 8 * 1024;

/// JS 引擎：封装一个 Node.js 子进程运行用户音源脚本
pub struct JsEngine {
    /// Node.js 子进程
    child: Mutex<Option<Child>>,
    /// stdin 写入端
    stdin: Mutex<Option<ChildStdin>>,
    /// stdout 由专用线程持续读取，调用方通过通道进行可超时等待。
    output_rx: Mutex<std::sync::mpsc::Receiver<String>>,
    _reader_handle: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// 请求 ID 计数器
    req_id: AtomicU64,
    /// 当前脚本路径，用于子进程异常后的重启
    source_path: String,
    /// 脚本通过 EVENT_NAMES.inited 声明的能力
    init_info: Mutex<Option<serde_json::Value>>,
    /// 子进程 stderr 的最后若干字节，崩溃原因只在这里（界面上报错要带上）。
    stderr_tail: Arc<Mutex<String>>,
}

impl JsEngine {
    /// 创建 JS 引擎，启动 Node.js 子进程加载指定脚本
    ///
    /// * `source_path` - 用户音源 JS 文件的路径
    pub fn new(source_path: &str) -> Result<Self, String> {
        // 将 wrapper.js 写入当前用户缓存目录的私有子目录，避免多用户共用
        // /tmp 时出现符号链接或目录抢占竞态。
        let wrapper_dir = dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("voicefox")
            .join("js");
        std::fs::create_dir_all(&wrapper_dir)
            .map_err(|e| format!("无法创建 JS 引擎缓存目录: {}", e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&wrapper_dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("无法设置 JS 引擎缓存目录权限: {}", e))?;
        }
        let wrapper_path = install_wrapper(&wrapper_dir, include_str!("wrapper.js"))?;

        let wrapper_path_string = wrapper_path
            .to_str()
            .ok_or_else(|| "wrapper.js 路径包含无效字符".to_string())?;
        if !node_supports_permission_mode() {
            return Err("当前 Node.js 不支持权限隔离模式，请升级 Node.js".to_string());
        }

        let mut command = Command::new("node");
        configure_background_command(&mut command);
        // 清空子进程环境变量，仅按白名单恢复启动所必需的项。wrapper.js 只用
        // Node 内置模块，不依赖其他环境变量；NODE_OPTIONS / NODE_PATH 仍然
        // 故意不保留，防止外部注入。
        command.env_clear();
        for key in child_env_keys() {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command.arg("--permission");
        if node_supports_flag("--allow-net") {
            command.arg("--allow-net");
        }
        command
            .arg(format!(
                "--allow-fs-read={}",
                permission_path(wrapper_path_string)
            ))
            .arg(format!("--allow-fs-read={}", permission_path(source_path)))
            .arg("--disable-proto=throw");
        // 脚本常用 __dirname 相对读取同目录的资源文件，只授权单个文件会让
        // 这类脚本初始化时 ERR_ACCESS_DENIED；目录授权必须是带结尾斜杠的
        // 前缀，Node 的权限树才认。
        if let Some(source_dir) = readable_source_dir(source_path) {
            command.arg(format!("--allow-fs-read={source_dir}"));
        }
        let mut child = command
            .arg(wrapper_path_string)
            .arg(source_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("启动 Node.js 失败（请确认已安装 Node.js）: {}", e))?;

        let stdin = child.stdin.take().ok_or("无法获取子进程 stdin")?;
        let stdout = child.stdout.take().ok_or("无法获取子进程 stdout")?;
        let stderr = child.stderr.take().ok_or("无法获取子进程 stderr")?;

        // 后台线程读取 stderr：边逐行输出到 tracing（避免管道堵塞），边把
        // 尾部留进环形缓冲 —— 子进程启动即崩溃时（Windows 缺 SystemRoot、
        // 脚本语法错误等）原因全在这里，界面上报错必须带上。
        let stderr_tail = Arc::new(Mutex::new(String::new()));
        {
            let stderr_tail = Arc::clone(&stderr_tail);
            std::thread::spawn(move || {
                let mut stderr = BufReader::new(stderr);
                let mut line = String::new();
                let mut tail: Vec<u8> = Vec::with_capacity(STDERR_TAIL_BYTES);
                loop {
                    line.clear();
                    match stderr.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            tail.extend_from_slice(line.as_bytes());
                            if tail.len() > STDERR_TAIL_BYTES {
                                let overflow = tail.len() - STDERR_TAIL_BYTES;
                                tail.drain(..overflow);
                            }
                            let trimmed = line.trim();
                            if !trimmed.is_empty() {
                                tracing::debug!("JS engine stderr: {trimmed}");
                            }
                        }
                    }
                }
                if let Ok(mut guard) = stderr_tail.lock() {
                    *guard = String::from_utf8_lossy(&tail).trim().to_string();
                }
            });
        }

        let (output_tx, output_rx) = std::sync::mpsc::channel();
        let reader_handle = std::thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match stdout.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if output_tx.send(line.trim().to_string()).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        let engine = Self {
            child: Mutex::new(Some(child)),
            stdin: Mutex::new(Some(stdin)),
            output_rx: Mutex::new(output_rx),
            _reader_handle: Mutex::new(Some(reader_handle)),
            req_id: AtomicU64::new(0),
            source_path: source_path.to_string(),
            init_info: Mutex::new(None),
            stderr_tail,
        };

        engine
            .ping()
            .map_err(|e| format!("JS 引擎启动失败: {e}{}", engine.exit_diagnostics()))?;
        engine
            .wait_initialized()
            .map_err(|e| format!("JS 音源初始化失败: {e}{}", engine.exit_diagnostics()))?;

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
        self.wait_response(id, Duration::from_secs(5)).map(|_| ())
    }

    fn wait_initialized(&self) -> Result<(), String> {
        if self.init_info.lock().map_err(|e| e.to_string())?.is_some() {
            return Ok(());
        }

        let deadline = Instant::now() + Duration::from_secs(16);
        loop {
            let line = self.recv_output(deadline, "JS 音源初始化超时")?;
            let response: serde_json::Value = match serde_json::from_str(&line) {
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
        self.wait_response(id, Duration::from_secs(16))
    }

    /// 发送 JSON 命令到子进程 stdin
    fn send_command(&self, cmd: &serde_json::Value) -> Result<(), String> {
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|e| format!("stdin lock error: {}", e))?;
        let stdin = stdin
            .as_mut()
            .ok_or_else(|| "JS 引擎子进程已关闭".to_string())?;
        let line = serde_json::to_string(cmd).map_err(|e| format!("序列化命令失败: {}", e))?;
        writeln!(stdin, "{}", line).map_err(|e| format!("写入子进程失败: {}", e))?;
        Ok(())
    }

    /// 等待并匹配响应 ID
    fn wait_response(
        &self,
        expected_id: u64,
        timeout: Duration,
    ) -> Result<serde_json::Value, String> {
        let deadline = Instant::now() + timeout;
        loop {
            let line = self.recv_output(deadline, "JS 引擎响应超时")?;
            if line.is_empty() {
                continue;
            }

            // 尝试解析为 JSON
            let resp: serde_json::Value = match serde_json::from_str(&line) {
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
            if let Some(id) = resp.get("id").and_then(|v| v.as_u64())
                && id == expected_id
            {
                if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
                    return Err(err.to_string());
                }
                return Ok(resp["result"].clone());
            }
            // 不匹配的响应跳过（可能是之前的响应）
        }
    }

    fn recv_output(&self, deadline: Instant, timeout_message: &str) -> Result<String, String> {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err(timeout_message.to_string());
        };
        self.output_rx
            .lock()
            .map_err(|e| format!("stdout channel lock error: {e}"))?
            .recv_timeout(remaining)
            .map_err(|error| match error {
                std::sync::mpsc::RecvTimeoutError::Timeout => timeout_message.to_string(),
                std::sync::mpsc::RecvTimeoutError::Disconnected => {
                    format!("JS 引擎子进程已退出{}", self.exit_diagnostics())
                }
            })
    }

    /// 子进程退出码与 stderr 尾部，拼在错误消息里给用户看真正的原因。
    ///
    /// 进程还活着（例如刚超时）时只带 stderr；已经退出时再带上退出状态。
    fn exit_diagnostics(&self) -> String {
        let stderr = self
            .stderr_tail
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let status = self.child.lock().ok().and_then(|mut child| {
            child
                .as_mut()
                .and_then(|child| child.try_wait().ok())
                .flatten()
        });
        let stderr_note = if stderr.is_empty() {
            String::new()
        } else {
            format!("，Node 输出: {stderr}")
        };
        // 尾部截断标记，避免"原因看起来是完整的但开头没了"造成误读。
        let stderr_note = if stderr.len() >= STDERR_TAIL_BYTES {
            format!("…{stderr_note}")
        } else {
            stderr_note
        };
        match status {
            Some(status) => format!("（退出状态: {status}{stderr_note}）"),
            None if stderr_note.is_empty() => String::new(),
            None => format!("（子进程仍在运行{stderr_note}）"),
        }
    }
}

/// 子进程环境变量白名单。
///
/// Linux 上 PATH/HOME 足够。Windows 的 node.exe 还要 `SystemRoot` 才能初始化
/// SXS / CryptoAPI / Winsock：没有它进程立刻退出，而 `CREATE_NO_WINDOW` 又把
/// 系统报错框藏掉了，表现成一句空的「子进程已退出」（issue #44）。
/// 列表里的键互不为大小写变体——Windows 环境块大小写不敏感，重复键会让
/// `CreateProcess` 失败。
fn child_env_keys() -> &'static [&'static str] {
    if cfg!(windows) {
        &[
            "SystemRoot",
            "SystemDrive",
            "TEMP",
            "TMP",
            "PATHEXT",
            "ComSpec",
            "APPDATA",
            "LOCALAPPDATA",
            "USERPROFILE",
            "HOMEDRIVE",
            "HOMEPATH",
            "PATH",
        ]
    } else {
        &["PATH", "HOME"]
    }
}

/// 将 wrapper 脚本原子写入 `dir` 并返回其路径。
fn install_wrapper(dir: &std::path::Path, content: &str) -> Result<std::path::PathBuf, String> {
    let path = dir.join("wrapper.js");
    let is_installed = || std::fs::read(&path).is_ok_and(|existing| existing == content.as_bytes());
    if is_installed() {
        return Ok(path);
    }

    // 临时文件以独占方式创建，重名时换用下一个序号。
    static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);
    let (temp, mut file) = loop {
        let temp = dir.join(format!(
            ".wrapper-{}-{}.tmp",
            std::process::id(),
            TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
        {
            Ok(file) => break (temp, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("无法写入 wrapper.js: {error}")),
        }
    };
    let written = file.write_all(content.as_bytes());
    drop(file);
    match written.and_then(|()| std::fs::rename(&temp, &path)) {
        Ok(()) => Ok(path),
        Err(error) => {
            let _ = std::fs::remove_file(&temp);
            // 目标内容与待写入内容一致时可直接使用。
            if is_installed() {
                Ok(path)
            } else {
                Err(format!("无法写入 wrapper.js: {error}"))
            }
        }
    }
}

/// `--allow-fs-read` 的路径口径：Windows 下统一成正斜杠。
///
/// Node 的权限树按路径前缀节点匹配，反斜杠路径（尤其作为目录前缀授权时）
/// 经常对不上实际读取路径，`readFileSync` 直接 `ERR_ACCESS_DENIED`。
fn permission_path(path: &str) -> String {
    #[cfg(windows)]
    {
        path.replace('\\', "/")
    }
    #[cfg(not(windows))]
    {
        path.to_string()
    }
}

/// 音源脚本所在目录的授权前缀（含结尾斜杠）。
fn readable_source_dir(source_path: &str) -> Option<String> {
    let dir = std::path::Path::new(source_path).parent()?;
    let dir = dir.to_str()?;
    if dir.is_empty() {
        return None;
    }
    let mut normalized = permission_path(dir);
    if !normalized.ends_with('/') {
        normalized.push('/');
    }
    Some(normalized)
}

impl Drop for JsEngine {
    fn drop(&mut self) {
        // 先关闭 stdin 触发 wrapper 的退出处理，再终止仍存活的进程。
        // wait 和 stdout 线程回收放到独立线程，避免 Windows 上删除音源或退出 TUI 时阻塞。
        if let Ok(mut stdin) = self.stdin.lock() {
            stdin.take();
        }
        let child = self.child.lock().ok().and_then(|mut child| child.take());
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = std::thread::Builder::new()
                .name("voicefox-js-reaper".to_string())
                .spawn(move || {
                    let _ = child.wait();
                });
        }
        if let Ok(mut handle) = self._reader_handle.lock() {
            handle.take();
        }
    }
}

fn node_supports_permission_mode() -> bool {
    node_supports_flag("--permission")
}

fn node_supports_flag(flag: &str) -> bool {
    static HELP: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    HELP.get_or_init(|| {
        let mut command = Command::new("node");
        configure_background_command(&mut command);
        command
            .arg("--help")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
    })
    .as_deref()
    .is_some_and(|help| help.contains(flag))
}

fn configure_background_command(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

#[cfg(test)]
mod tests {
    use super::{child_env_keys, install_wrapper, permission_path, readable_source_dir};

    /// 每个测试独占的空目录。
    fn wrapper_test_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("voicefox-wrapper-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn wrapper_install_writes_full_content_without_leftovers() {
        let dir = wrapper_test_dir("fresh");
        let content = include_str!("wrapper.js");

        let path = install_wrapper(&dir, content).unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(entries, ["wrapper.js"], "目录中残留临时文件");
    }

    #[test]
    fn unchanged_wrapper_is_not_rewritten() {
        let dir = wrapper_test_dir("unchanged");
        let content = include_str!("wrapper.js");
        let path = install_wrapper(&dir, content).unwrap();
        let past =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(past)
            .unwrap();

        install_wrapper(&dir, content).unwrap();

        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(modified, past);
    }

    #[test]
    #[cfg(unix)]
    fn stale_wrapper_is_replaced_not_rewritten_in_place() {
        use std::io::Read;

        let dir = wrapper_test_dir("stale");
        let content = include_str!("wrapper.js");
        let path = dir.join("wrapper.js");
        std::fs::write(&path, "// stale").unwrap();
        // 保持旧文件打开，模拟正在读取它的 node 进程。
        let mut reader = std::fs::File::open(&path).unwrap();

        assert_eq!(install_wrapper(&dir, content).unwrap(), path);

        // 已打开的句柄仍读到完整的旧内容，说明目标文件经整体替换。
        let mut old = String::new();
        reader.read_to_string(&mut old).unwrap();
        let new = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(old, "// stale");
        assert_eq!(new, content);
    }

    #[test]
    fn wrapper_disables_dynamic_string_code_generation() {
        let wrapper = include_str!("wrapper.js");
        assert!(wrapper.contains("strings: false"));
        assert!(!wrapper.contains("strings: true"));
    }

    #[test]
    fn env_whitelist_has_no_case_insensitive_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for key in child_env_keys() {
            assert!(
                seen.insert(key.to_ascii_lowercase()),
                "Windows 环境块大小写不敏感，白名单里的 {key} 会造成重复键"
            );
        }
    }

    #[test]
    #[cfg(windows)]
    fn windows_env_whitelist_keeps_node_requirements() {
        let keys = child_env_keys();
        for required in ["SystemRoot", "PATH", "TEMP", "USERPROFILE"] {
            assert!(keys.contains(&required), "Windows 白名单缺 {required}");
        }
        assert!(
            !keys.contains(&"NODE_OPTIONS") && !keys.contains(&"NODE_PATH"),
            "注入面变量必须继续排除"
        );
    }

    #[test]
    fn fs_read_paths_are_normalized_for_the_permission_tree() {
        // POSIX 路径原样返回（反斜杠在 Unix 是合法文件名字符，不能动）。
        assert_eq!(
            permission_path("/tmp/voicefox/wrapper.js"),
            "/tmp/voicefox/wrapper.js"
        );
    }

    #[test]
    #[cfg(windows)]
    fn windows_paths_use_forward_slashes() {
        assert_eq!(
            permission_path("C:\\Users\\u\\AppData\\Local\\voicefox\\js\\wrapper.js"),
            "C:/Users/u/AppData/Local/voicefox/js/wrapper.js"
        );
    }

    #[test]
    fn source_dir_grant_is_a_trailing_slash_prefix() {
        assert_eq!(
            readable_source_dir("/home/u/sources/juhe.js").as_deref(),
            Some("/home/u/sources/")
        );
        // 相对文件名没有可授权的目录
        assert_eq!(readable_source_dir("juhe.js"), None);
    }

    #[test]
    #[cfg(windows)]
    fn source_dir_grant_normalizes_separators() {
        assert_eq!(
            readable_source_dir("C:\\sources\\juhe.js").as_deref(),
            Some("C:/sources/")
        );
    }
}
