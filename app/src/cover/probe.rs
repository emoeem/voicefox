//! 在子进程中查询终端图形能力
//!
//! ratatui-image 的查询线程阻塞在 stdin 上，超时返回后仍会留存并抢走后续输入。
//! 查询放在子进程中执行，父进程按固定时限回收子进程，确保没有线程继续读取 stdin。

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ratatui_image::FontSize;
use ratatui_image::picker::{Picker, ProtocolType};

use super::render::parse_protocol;

/// 子进程的启动参数
const PROBE_ARG: &str = "--internal-cover-probe";

/// 查询的总时限，不随终端的零碎应答延长
const PROBE_TIMEOUT: Duration = Duration::from_millis(2500);

/// 检查子进程是否退出的间隔
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// 子进程返回的查询结果
#[derive(Debug)]
pub struct ProbeResult {
    pub protocol: ProtocolType,
    pub font_size: FontSize,
    /// 字号是否来自终端应答
    pub queried_font_size: bool,
}

/// 以查询子进程身份启动时执行查询并退出
///
/// 结果写入 stderr，stdin 与 stdout 留给终端查询使用。必须先于 CLI 解析调用。
pub fn run_if_probe() {
    if std::env::args().nth(1).as_deref() != Some(PROBE_ARG) {
        return;
    }
    let code = match Picker::from_query_stdio() {
        Ok(picker) => {
            let result = ProbeResult {
                protocol: picker.protocol_type(),
                font_size: picker.font_size(),
                queried_font_size: !picker.capabilities().is_empty(),
            };
            let mut stderr = std::io::stderr();
            let _ = writeln!(stderr, "{}", encode(&result));
            0
        }
        Err(error) => {
            let mut stderr = std::io::stderr();
            let _ = writeln!(stderr, "error {error}");
            1
        }
    };
    std::process::exit(code);
}

/// 启动查询子进程并等待结果，失败或超时返回 `None`
///
/// 调用时事件循环不得读取 stdin。
pub fn query() -> Option<ProbeResult> {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            tracing::info!("locate executable for cover probe failed: {error}");
            return None;
        }
    };
    // 子进程可能在修改终端模式后被终止
    let _mode = TerminalMode::save();
    let mut child = match Command::new(exe)
        .arg(PROBE_ARG)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            tracing::info!("spawn cover probe failed: {error}");
            return None;
        }
    };

    let deadline = Instant::now() + PROBE_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL_INTERVAL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                tracing::info!("cover probe timed out after {PROBE_TIMEOUT:?}");
                return None;
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                tracing::info!("wait for cover probe failed: {error}");
                return None;
            }
        }
    };

    let mut output = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut output);
    }
    let line = output.lines().last().unwrap_or_default();
    if !status.success() {
        tracing::info!("cover probe exited with {status}: {line}");
        return None;
    }
    let result = decode(line);
    if result.is_none() {
        tracing::info!("cover probe returned malformed output {output:?}");
    }
    result
}

fn encode(result: &ProbeResult) -> String {
    let protocol = match result.protocol {
        ProtocolType::Halfblocks => "halfblocks",
        ProtocolType::Sixel => "sixel",
        ProtocolType::Kitty => "kitty",
        ProtocolType::Iterm2 => "iterm2",
    };
    format!(
        "{protocol} {} {} {}",
        result.font_size.width,
        result.font_size.height,
        u8::from(result.queried_font_size)
    )
}

fn decode(line: &str) -> Option<ProbeResult> {
    let mut fields = line.split_whitespace();
    let protocol = parse_protocol(fields.next()?)?;
    let width = fields.next()?.parse().ok()?;
    let height = fields.next()?.parse().ok()?;
    let queried_font_size = match fields.next()? {
        "0" => false,
        "1" => true,
        _ => return None,
    };
    if fields.next().is_some() || width == 0 || height == 0 {
        return None;
    }
    Some(ProbeResult {
        protocol,
        font_size: FontSize::new(width, height),
        queried_font_size,
    })
}

/// 析构时恢复保存的 stdin 终端模式
struct TerminalMode {
    #[cfg(unix)]
    termios: Option<rustix::termios::Termios>,
    #[cfg(windows)]
    console_mode: Option<u32>,
}

#[cfg(unix)]
impl TerminalMode {
    fn save() -> Self {
        Self {
            termios: rustix::termios::tcgetattr(std::io::stdin()).ok(),
        }
    }
}

#[cfg(unix)]
impl Drop for TerminalMode {
    fn drop(&mut self) {
        use rustix::termios::{OptionalActions, tcsetattr};

        if let Some(termios) = &self.termios
            && let Err(error) = tcsetattr(std::io::stdin(), OptionalActions::Now, termios)
        {
            tracing::warn!("restore terminal mode after cover probe failed: {error}");
        }
    }
}

#[cfg(windows)]
impl TerminalMode {
    fn save() -> Self {
        use windows_sys::Win32::System::Console::{GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE};

        let mut mode = 0;
        // SAFETY: 句柄来自 GetStdHandle，mode 指向有效的局部变量。
        let saved = unsafe { GetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), &mut mode) } != 0;
        Self {
            console_mode: saved.then_some(mode),
        }
    }
}

#[cfg(windows)]
impl Drop for TerminalMode {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode};

        if let Some(mode) = self.console_mode {
            // SAFETY: 句柄来自 GetStdHandle，mode 为先前读取的控制台模式。
            if unsafe { SetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), mode) } == 0 {
                tracing::warn!("restore console mode after cover probe failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui_image::FontSize;
    use ratatui_image::picker::ProtocolType;

    use super::{ProbeResult, decode, encode};

    #[test]
    fn probe_results_round_trip() {
        for protocol in [
            ProtocolType::Halfblocks,
            ProtocolType::Sixel,
            ProtocolType::Kitty,
            ProtocolType::Iterm2,
        ] {
            let result = ProbeResult {
                protocol,
                font_size: FontSize::new(10, 22),
                queried_font_size: protocol == ProtocolType::Sixel,
            };
            let decoded = decode(&encode(&result)).expect("decodable");
            assert_eq!(decoded.protocol, result.protocol);
            assert_eq!(
                (decoded.font_size.width, decoded.font_size.height),
                (10, 22)
            );
            assert_eq!(decoded.queried_font_size, result.queried_font_size);
        }
    }

    #[test]
    fn malformed_probe_output_is_rejected() {
        for line in [
            "",
            "auto 10 22 1",
            "sixel 10 22",
            "sixel 0 22 1",
            "sixel 10 22 2",
            "sixel 10 22 1 extra",
        ] {
            assert!(decode(line).is_none(), "{line:?}");
        }
    }
}
