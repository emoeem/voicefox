use clap::Parser;

/// voicefox: Rust TUI 版 lx-music-desktop
#[derive(Parser, Debug)]
#[command(name = "voicefox", version, about)]
pub struct Cli {
    /// 配置文件路径
    #[arg(short, long, default_value = "")]
    pub config: String,

    /// 日志级别
    #[arg(short, long, default_value = "info")]
    pub log_level: String,
}

impl Cli {
    pub fn parse() -> Self {
        <Self as Parser>::parse()
    }
}
