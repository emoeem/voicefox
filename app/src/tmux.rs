//! tmux 集成：把 client attach 事件通知到进程内

use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

/// 图形序列是否需要经 tmux passthrough，判据对齐 ratatui-image 的
/// detect_tmux_and_outer_protocol_from_env
pub fn is_passthrough() -> bool {
    is_passthrough_from(
        std::env::var_os("TMUX").is_some(),
        std::env::var("TERM").ok().as_deref(),
        std::env::var("TERM_PROGRAM").ok().as_deref(),
    )
}

/// 让 ratatui-image 在 `TERM=screen-*` 的常见 tmux 环境中启用 passthrough。
///
/// 必须在程序创建任何线程前调用，因为修改进程环境不是线程安全操作。
pub fn prepare_ratatui_image_environment() {
    let tmux_present = std::env::var_os("TMUX").is_some();
    let ratatui_image_detects_tmux = is_passthrough_from(
        false,
        std::env::var("TERM").ok().as_deref(),
        std::env::var("TERM_PROGRAM").ok().as_deref(),
    );
    if tmux_present && !ratatui_image_detects_tmux {
        // SAFETY: main() 在创建 Tokio runtime 和其他工作线程前调用本函数。
        unsafe {
            std::env::set_var("TERM_PROGRAM", "tmux");
        }
    }
}

fn is_passthrough_from(tmux_present: bool, term: Option<&str>, term_program: Option<&str>) -> bool {
    tmux_present
        || term.is_some_and(|term| term.starts_with("tmux"))
        || term_program == Some("tmux")
}

/// 经 passthrough 发出的查询能否得到外层终端的应答
///
/// allow-passthrough 选项需要 tmux 3.3 及以上，且 session 须有已连接的 client。
pub fn passthrough_available() -> bool {
    tmux(&["show", "-pvA", "allow-passthrough"])
        && tmux_output(&["display", "-p", "#{session_attached}"])
            .is_some_and(|attached| attached.trim().parse::<u32>().is_ok_and(|count| count > 0))
}

pub struct AttachWatcher {
    session: String,
    hook: String,
    channel: String,
    attached: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
}

impl AttachWatcher {
    /// 安装 hook 并启动监听线程，失败返回 None
    pub fn install() -> Option<Self> {
        if !is_passthrough() {
            return None;
        }
        let session = session_target()?;
        let pid = std::process::id();
        let hook = hook_name(pid);
        let channel = channel_name(pid);
        let command = format!("run-shell -b \"tmux wait-for -S {channel}\"");
        if !tmux(&["set-hook", "-t", &session, &hook, &command]) {
            tracing::warn!("install tmux {hook} failed, cover will not follow attach");
            return None;
        }

        let attached = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));
        let (flag, stop, watched) = (
            Arc::clone(&attached),
            Arc::clone(&shutdown),
            channel.clone(),
        );
        let spawned = thread::Builder::new()
            .name("voicefox-tmux-attach".to_string())
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    // wait-for 阻塞等待，不占用 CPU
                    if !tmux(&["wait-for", &watched]) {
                        tracing::debug!("tmux wait-for failed, stop watching for attach");
                        break;
                    }
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    flag.store(true, Ordering::Relaxed);
                }
            });
        if spawned.is_err() {
            tracing::warn!("spawn tmux attach watcher failed");
            tmux(&["set-hook", "-u", "-t", &session, &hook]);
            return None;
        }

        tracing::info!("watching tmux {hook} on session {session}");
        Some(Self {
            session,
            hook,
            channel,
            attached,
            shutdown,
        })
    }

    /// 上次调用之后是否有 client attach，读取后清零
    pub fn take_attached(&self) -> bool {
        self.attached.swap(false, Ordering::Relaxed)
    }
}

impl Drop for AttachWatcher {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        tmux(&["set-hook", "-u", "-t", &self.session, &self.hook]);
        // 监听线程仍阻塞在 wait-for 上，唤醒它以读取 shutdown 标志
        tmux(&["wait-for", "-S", &self.channel]);
    }
}

/// hook 名，以进程 pid 作数组下标
fn hook_name(pid: u32) -> String {
    format!("client-attached[{pid}]")
}

fn channel_name(pid: u32) -> String {
    format!("voicefox-attached-{pid}")
}

/// $TMUX 的第三段是 session id 编号，拼上 $ 即为 set-hook 的目标
fn session_target() -> Option<String> {
    session_target_from(&std::env::var("TMUX").ok()?)
}

fn session_target_from(tmux_env: &str) -> Option<String> {
    let id = tmux_env.split(',').nth(2)?;
    if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(format!("${id}"))
}

/// 执行一条 tmux 命令，返回是否成功
fn tmux(args: &[&str]) -> bool {
    Command::new("tmux")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// 执行一条 tmux 命令，返回成功时的标准输出
fn tmux_output(args: &[&str]) -> Option<String> {
    let output = Command::new("tmux")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::{is_passthrough_from, session_target_from};

    #[test]
    fn tmux_environment_variable_detects_screen_term_sessions() {
        assert!(is_passthrough_from(
            true,
            Some("screen-256color"),
            Some("foot")
        ));
        assert!(!is_passthrough_from(
            false,
            Some("screen-256color"),
            Some("foot")
        ));
    }

    #[test]
    fn the_session_target_comes_from_the_third_field_of_tmux_env() {
        assert_eq!(
            session_target_from("/tmp/tmux-1000/default,22518,0"),
            Some("$0".to_string())
        );
        assert_eq!(
            session_target_from("/tmp/tmux-1000/default,22518,17"),
            Some("$17".to_string())
        );
    }

    #[test]
    fn a_malformed_tmux_env_yields_no_target() {
        // 字段缺失、字段为空、字段非数字都不能拼出目标，否则 set-hook 会作用到别的 session
        assert_eq!(session_target_from("/tmp/socket,22518"), None);
        assert_eq!(session_target_from("/tmp/socket,22518,"), None);
        assert_eq!(session_target_from("/tmp/socket,22518,zero"), None);
        assert_eq!(session_target_from(""), None);
    }
}
