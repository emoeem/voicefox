use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::source::{Quality, SourceId};

/// 播放器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerConfig {
    pub engine: String,
    pub quality: Quality,
    pub volume: u32,
    pub play_mode: String,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            engine: "mpv".to_string(),
            quality: Quality::High320,
            volume: 80,
            play_mode: "list-loop".to_string(),
        }
    }
}

/// 音源配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceConfig {
    pub enabled: Vec<SourceId>,
    pub default: SourceId,
    pub auto_toggle: bool,
    /// JS 音源脚本 URL 或本地路径列表（lx-music user API 协议）
    #[serde(default)]
    pub js_sources: Vec<String>,
}

impl Default for SourceConfig {
    fn default() -> Self {
        Self {
            enabled: vec![SourceId::Kw],
            default: SourceId::Kw,
            auto_toggle: true,
            js_sources: vec![],
        }
    }
}

/// 歌词配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LyricConfig {
    pub show_translation: bool,
    pub show_yrc: bool,
    pub offset: i32,
}

impl Default for LyricConfig {
    fn default() -> Self {
        Self {
            show_translation: true,
            show_yrc: true,
            offset: 0,
        }
    }
}

/// 网络配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub proxy_url: String,
    pub timeout: u64,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            proxy_url: String::new(),
            timeout: 15,
        }
    }
}

/// 主题配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    pub use_dark: bool,
    pub accent: String,
    pub text: String,
    pub muted: String,
    pub border: String,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            use_dark: true,
            accent: "cyan".to_string(),
            text: "white".to_string(),
            muted: "dark_gray".to_string(),
            border: "cyan".to_string(),
        }
    }
}

/// TUI 交互配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub enable_mouse: bool,
    pub wrap_navigation: bool,
    pub scroll_amount: usize,
    pub aggregate_search: bool,
    pub show_cover: bool,
    pub max_fps: u32,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            enable_mouse: true,
            wrap_navigation: true,
            scroll_amount: 3,
            aggregate_search: true,
            show_cover: true,
            max_fps: 20,
        }
    }
}

/// 应用完整配置
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    pub player: PlayerConfig,
    pub source: SourceConfig,
    pub lyric: LyricConfig,
    pub network: NetworkConfig,
    pub theme: ThemeConfig,
    #[serde(default)]
    pub ui: UiConfig,
    /// 自定义快捷键
    pub keybindings: HashMap<String, String>,
}
