use serde::{Deserialize, Serialize};

use super::source::{Quality, SourceId};

pub const CURRENT_CONFIG_VERSION: u32 = 2;

/// 播放器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
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
#[serde(default)]
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
            enabled: SourceId::all_online().to_vec(),
            default: SourceId::Kw,
            auto_toggle: true,
            js_sources: vec![],
        }
    }
}

/// 歌词配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
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
#[serde(default)]
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
    /// 兼容旧配置的主强调色。
    pub accent: String,
    pub text: String,
    /// 兼容旧配置的次要文字色。
    pub muted: String,
    /// 兼容旧配置的边框色。
    pub border: String,
    pub rosewater: String,
    pub flamingo: String,
    pub pink: String,
    pub mauve: String,
    pub red: String,
    pub maroon: String,
    pub peach: String,
    pub yellow: String,
    pub green: String,
    pub teal: String,
    pub sky: String,
    pub sapphire: String,
    pub blue: String,
    pub lavender: String,
    pub subtext_1: String,
    pub subtext_0: String,
    pub overlay_2: String,
    pub overlay_1: String,
    pub overlay_0: String,
    pub surface_2: String,
    pub surface_1: String,
    pub surface_0: String,
    pub base: String,
    pub mantle: String,
    pub crust: String,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            use_dark: true,
            accent: "#cba6f7".to_string(),
            text: "#cdd6f4".to_string(),
            muted: "#a6adc8".to_string(),
            border: "#585b70".to_string(),
            rosewater: "#f5e0dc".to_string(),
            flamingo: "#f2cdcd".to_string(),
            pink: "#f5c2e7".to_string(),
            mauve: "#cba6f7".to_string(),
            red: "#f38ba8".to_string(),
            maroon: "#eba0ac".to_string(),
            peach: "#fab387".to_string(),
            yellow: "#f9e2af".to_string(),
            green: "#a6e3a1".to_string(),
            teal: "#94e2d5".to_string(),
            sky: "#89dceb".to_string(),
            sapphire: "#74c7ec".to_string(),
            blue: "#89b4fa".to_string(),
            lavender: "#b4befe".to_string(),
            subtext_1: "#bac2de".to_string(),
            subtext_0: "#a6adc8".to_string(),
            overlay_2: "#9399b2".to_string(),
            overlay_1: "#7f849c".to_string(),
            overlay_0: "#6c7086".to_string(),
            surface_2: "#585b70".to_string(),
            surface_1: "#45475a".to_string(),
            surface_0: "#313244".to_string(),
            base: "#1e1e2e".to_string(),
            mantle: "#181825".to_string(),
            crust: "#11111b".to_string(),
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

/// 本地音乐配置
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalMusicConfig {
    pub enabled: bool,
    /// 音乐目录路径列表
    pub paths: Vec<String>,
    /// 扫描深度，0 为不限制
    pub max_depth: u32,
}

/// 应用完整配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    #[serde(default = "legacy_config_version")]
    pub version: u32,
    pub player: PlayerConfig,
    pub source: SourceConfig,
    pub lyric: LyricConfig,
    pub network: NetworkConfig,
    pub theme: ThemeConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub local_music: LocalMusicConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CURRENT_CONFIG_VERSION,
            player: PlayerConfig::default(),
            source: SourceConfig::default(),
            lyric: LyricConfig::default(),
            network: NetworkConfig::default(),
            theme: ThemeConfig::default(),
            ui: UiConfig::default(),
            local_music: LocalMusicConfig::default(),
        }
    }
}

fn legacy_config_version() -> u32 {
    0
}
