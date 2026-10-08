//! 采集探针：复现应用的采集环境（PTY stdin + stdout 管道 + stderr 丢弃），
//! 打印每次断流的时间点与子进程退出状态，用于诊断"采集流反复断开"。
//! 用法：cargo run -p voicefox-app --example capture_probe [-- --raw]
//!
//! --raw 会先把终端切到 raw 模式（与 TUI 相同）再起采集。

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn capture_command() -> Option<Command> {
    // 与 app/src/visualizer/capture.rs::capture_command 保持一致
    if std::path::Path::new("/usr/bin/parec").exists() || which("parec") {
        let mut command = Command::new("parec");
        command
            .arg("-d")
            .arg("@DEFAULT_MONITOR@")
            .arg("--format=s16le")
            .arg("--rate=44100")
            .arg("--channels=2");
        return Some(command);
    }
    if which("pw-record") && which("pactl") {
        let sink = Command::new("pactl")
            .arg("get-default-sink")
            .output()
            .ok()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .filter(|name| !name.is_empty())?;
        // `--target` 只认数字节点 id：传 sink 名会被静默忽略并接回麦克风。
        let sinks = Command::new("pactl")
            .args(["list", "short", "sinks"])
            .output()
            .ok()?;
        let target = String::from_utf8_lossy(&sinks.stdout)
            .lines()
            .find_map(|line| {
                let mut fields = line.split_whitespace();
                let id = fields.next()?;
                (fields.next() == Some(sink.as_str())).then_some(id.to_string())
            })?;
        let mut command = Command::new("pw-record");
        command
            .arg("--raw")
            .arg("--format")
            .arg("s16")
            .arg("--rate")
            .arg("44100")
            .arg("--channels")
            .arg("2")
            .arg("--target")
            .arg(target)
            .arg("-");
        return Some(command);
    }
    None
}

fn which(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

fn main() {
    let raw_mode = std::env::args().any(|arg| arg == "--raw");
    if raw_mode {
        match crossterm::terminal::enable_raw_mode() {
            Ok(()) => println!("[probe] raw mode ON"),
            Err(error) => println!("[probe] raw mode FAILED: {error}"),
        }
    }
    let Some(command) = capture_command() else {
        eprintln!("[probe] no capture command available");
        return;
    };
    let mut command = command;
    command.stdout(Stdio::piped()).stderr(Stdio::null());
    println!("[probe] spawning: {:?}", command);
    let start = Instant::now();
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            eprintln!("[probe] spawn failed: {error}");
            return;
        }
    };
    let mut stdout = child.stdout.take().unwrap();
    let start_reader = start;
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut raw = vec![0u8; 2048];
        let mut total = 0usize;
        let mut next_report = 0usize;
        loop {
            match stdout.read(&mut raw) {
                Ok(0) => {
                    println!(
                        "[probe] EOF at {:.3}s (total {total} bytes)",
                        start_reader.elapsed().as_secs_f32()
                    );
                    break;
                }
                Ok(n) => {
                    total += n;
                    if total >= next_report {
                        println!(
                            "[probe] t={:.3}s total={total}",
                            start_reader.elapsed().as_secs_f32()
                        );
                        next_report += 100_000;
                    }
                }
                Err(error) => {
                    println!(
                        "[probe] read error at {:.3}s: {error} (total {total})",
                        start_reader.elapsed().as_secs_f32()
                    );
                    break;
                }
            }
        }
        total
    });
    std::thread::sleep(Duration::from_secs(6));
    let status = child.try_wait();
    match status {
        Ok(None) => {
            println!("[probe] after 6s: child STILL RUNNING (stream alive)");
            let _ = child.kill();
            let _ = child.wait();
        }
        Ok(Some(status)) => println!("[probe] after 6s: child EXITED early: {status}"),
        Err(error) => println!("[probe] try_wait error: {error}"),
    }
    let _ = reader.join();
    if raw_mode {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}
