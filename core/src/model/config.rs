use serde::{Deserialize, Deserializer, Serialize};

use super::source::{Quality, SourceId};
use crate::keybinding::KeybindingConfig;
use crate::traits::player::EqualizerBand;

pub const CURRENT_CONFIG_VERSION: u32 = 16;

/// 侧边栏背景样式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SidebarBg {
    Transparent,
    Mantle,
    Surface0,
    Surface1,
    Base,
}

impl SidebarBg {
    pub fn label(self) -> &'static str {
        match self {
            Self::Transparent => "透明（跟随终端）",
            Self::Mantle => "Mantle（默认深色）",
            Self::Surface0 => "Surface0（稍亮）",
            Self::Surface1 => "Surface1（更亮）",
            Self::Base => "Base",
        }
    }

    pub fn cycle_next(self) -> Self {
        match self {
            Self::Transparent => Self::Mantle,
            Self::Mantle => Self::Surface0,
            Self::Surface0 => Self::Surface1,
            Self::Surface1 => Self::Base,
            Self::Base => Self::Transparent,
        }
    }
}

/// 可显示在底部状态栏中的内容。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StatusBarItem {
    State,
    Source,
    Sort,
    Song,
    Time,
    Volume,
    PlayMode,
    Quality,
    Queue,
    JsSourceState,
}

impl StatusBarItem {
    pub const ALL: [Self; 10] = [
        Self::State,
        Self::Source,
        Self::Sort,
        Self::Song,
        Self::Time,
        Self::Volume,
        Self::PlayMode,
        Self::Quality,
        Self::Queue,
        Self::JsSourceState,
    ];
}

fn default_status_bar_items() -> Vec<StatusBarItem> {
    StatusBarItem::ALL.to_vec()
}

fn deserialize_status_bar_items<'de, D>(deserializer: D) -> Result<Vec<StatusBarItem>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<String>::deserialize(deserializer)?;
    let mut items = values
        .into_iter()
        .filter_map(|value| {
            serde_json::from_value::<StatusBarItem>(serde_json::Value::String(value)).ok()
        })
        .collect();
    sanitize_status_bar_items(&mut items);
    Ok(items)
}

/// 去除状态栏配置中的重复字段，同时保留用户指定的顺序。
pub fn sanitize_status_bar_items(items: &mut Vec<StatusBarItem>) -> bool {
    let original_len = items.len();
    let mut seen = std::collections::HashSet::new();
    items.retain(|item| seen.insert(*item));
    items.len() != original_len
}

/// 播放器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayerConfig {
    pub engine: String,
    pub quality: Quality,
    pub volume: u32,
    /// 播放速度倍率，正常速度为 1.0。
    pub playback_speed: f64,
    /// libmpv 音频输出设备，`auto` 使用系统默认设备。
    pub audio_device: String,
    /// ReplayGain 模式：off、track 或 album。
    pub replaygain_mode: String,
    /// ReplayGain 预放大（分贝）。
    pub replaygain_preamp: f64,
    /// 声道模式：auto、stereo、mono、left 或 right。
    pub channel_mode: String,
    /// 左右声道平衡，-1 为全左，1 为全右。
    pub balance: f64,
    /// 是否在 ReplayGain 预放大后限制削波。
    pub replaygain_clip: bool,
    /// 持久化的均衡器频段；空数组表示关闭均衡器。
    #[serde(default)]
    pub equalizer_bands: Vec<EqualizerBand>,
    /// 新曲目开始时的淡入时长（毫秒），0 表示关闭。
    pub fade_in_ms: u64,
    /// 曲目结束前的淡出时长（毫秒），0 表示关闭。
    pub fade_out_ms: u64,
    pub play_mode: String,
    pub remember_playback_state: bool,
    pub history_limit: usize,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            engine: "mpv".to_string(),
            quality: Quality::High320,
            volume: 80,
            playback_speed: 1.0,
            audio_device: "auto".to_string(),
            replaygain_mode: "off".to_string(),
            replaygain_preamp: 0.0,
            channel_mode: "auto".to_string(),
            balance: 0.0,
            replaygain_clip: false,
            equalizer_bands: Vec::new(),
            fade_in_ms: 0,
            fade_out_ms: 0,
            play_mode: "list-loop".to_string(),
            remember_playback_state: true,
            history_limit: 100,
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
            enabled: SourceId::default_enabled().to_vec(),
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
    /// 侧边导航是否使用终端原生背景，便于与透明终端主题融合。
    pub sidebar_transparent: bool,
    /// 侧边栏背景样式（优先于 sidebar_transparent）。
    #[serde(default)]
    pub sidebar_style: Option<SidebarBg>,
    pub show_cover: bool,
    /// 封面渲染协议：auto / kitty / sixel / iterm2 / halfblocks。
    /// auto 表示由终端探测决定，探测不准时可以指定具体协议。
    pub cover_protocol: String,
    /// 旧版本通知配置，仅用于迁移，不再写入新配置。
    #[serde(default, skip_serializing)]
    pub show_notifications: Option<bool>,
    /// 旧版本通知停留时间，仅用于迁移，不再写入新配置。
    #[serde(default, skip_serializing)]
    pub notification_timeout: Option<u64>,
    pub max_fps: u32,
    /// 底部状态栏中启用的字段，数组顺序即显示顺序。
    #[serde(
        default = "default_status_bar_items",
        deserialize_with = "deserialize_status_bar_items"
    )]
    pub status_bar_items: Vec<StatusBarItem>,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            enable_mouse: true,
            wrap_navigation: true,
            scroll_amount: 3,
            aggregate_search: true,
            sidebar_transparent: false,
            sidebar_style: None,
            show_cover: true,
            cover_protocol: "auto".to_string(),
            show_notifications: None,
            notification_timeout: None,
            max_fps: 20,
            status_bar_items: default_status_bar_items(),
        }
    }
}

/// 通知配置。
///
/// 字段同时接受 camelCase 和 snake_case，生成的配置使用与 go-musicfox
/// 一致的 camelCase，便于用户迁移已有配置和理解跨项目设置。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationConfig {
    pub enable: bool,
    #[serde(rename = "inApp", alias = "in_app")]
    pub in_app: bool,
    #[serde(rename = "inAppTimeout", alias = "in_app_timeout")]
    pub in_app_timeout: u64,
    #[serde(rename = "albumCover", alias = "album_cover")]
    pub album_cover: bool,
    #[serde(
        rename = "trackChange",
        alias = "track_change",
        alias = "notifyOnTrackChange"
    )]
    pub track_change: bool,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            enable: true,
            in_app: true,
            in_app_timeout: 4,
            album_cover: true,
            track_change: true,
        }
    }
}

/// 外部桌面集成配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IntegrationConfig {
    /// Linux 上注册标准 MPRIS 服务，Waybar 可直接识别和控制。
    pub mpris: bool,
}

impl Default for IntegrationConfig {
    fn default() -> Self {
        Self { mpris: true }
    }
}

/// 本地音乐配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalMusicConfig {
    pub enabled: bool,
    /// 音乐目录路径列表
    pub paths: Vec<String>,
    /// 扫描深度，0 为不限制
    pub max_depth: u32,
}

impl Default for LocalMusicConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            paths: Vec::new(),
            max_depth: 0,
        }
    }
}

/// 音乐下载配置
///
/// 下载逻辑参考 MusicBot-Go 的 `bot/download`：探测源是否支持 Range，
/// 大文件走多线程分片，落盘后校验字节数，网络类失败按指数退避重试。
///
/// `webdav` 子配置参考 go-music-dl 的 `core/webdav.go`：下载成功后把
/// 文件同步到远端，失败只作为警告，本地文件仍然保留。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DownloadConfig {
    /// 下载目录。留空时使用 `~/Music/voicefox`（没有音乐目录则退回 `~/Downloads/voicefox`）。
    pub dir: String,
    /// 下载音质。`None` 表示跟随播放音质。
    pub quality: Option<Quality>,
    /// 文件名模板，支持 `{name}` `{singer}` `{album}` `{source}` `{quality}`；
    /// 扩展名按实际音频格式自动追加。
    pub filename_template: String,
    /// 单个文件的分片并发数。
    pub concurrency: usize,
    /// 同时下载的歌曲数量。
    pub concurrent_songs: usize,
    /// 是否启用多线程分片下载。
    pub multipart: bool,
    /// 文件体积不小于该值（MB）时才分片下载。
    pub multipart_min_size_mb: u64,
    /// 网络类失败的最大重试次数。
    pub max_retries: u32,
    /// 校验实际落盘字节数与音源声明大小是否一致。
    pub verify_size: bool,
    /// 目标文件已存在时跳过下载。
    pub skip_existing: bool,
    /// 写入标题、歌手、专辑标签。
    pub write_tags: bool,
    /// 把封面嵌入音频标签。
    pub embed_cover: bool,
    /// 保存歌词：同时写出 `.lrc` 文件并内嵌到音频标签。
    pub save_lyric: bool,
    /// WebDAV 同步设置。
    pub webdav: WebdavConfig,
    /// 播放时自动缓存：把正在播放的歌曲存到本地下载目录。
    pub auto_cache_on_play: bool,
    /// 播放满该秒数后才开始缓存，避免刚切歌就白下一次。
    pub auto_cache_after_secs: u64,
}

/// WebDAV 同步配置。
///
/// 地址、账号、远端目录都可以留空；`enabled` 为真且 `url` 非空时才会上传。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WebdavConfig {
    pub enabled: bool,
    /// 服务地址，例如 `https://dav.example.com/remote.php/dav/files/user/`。
    pub url: String,
    pub username: String,
    pub password: String,
    /// 远端目录，留空表示直接放在服务地址对应的根目录下。
    pub dir: String,
}

impl Default for WebdavConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            username: String::new(),
            password: String::new(),
            dir: "voicefox".to_string(),
        }
    }
}

impl WebdavConfig {
    /// 从「可能带账号信息的地址」写入配置。
    ///
    /// 设置页只提供一个输入框，用户可以直接粘贴
    /// `https://user:pass@host/dav/`；这里把账号密码拆出来单独保存，
    /// 后续上传时再交给 Basic Auth，避免把密码留在 URL 里被日志打印。
    pub fn apply_url_input(&mut self, value: &str) {
        let value = value.trim();
        let Some((scheme, rest)) = value.split_once("://") else {
            self.url = value.to_string();
            return;
        };
        let authority_end = rest.find('/').unwrap_or(rest.len());
        let (authority, path) = rest.split_at(authority_end);
        match authority.rsplit_once('@') {
            Some((credentials, host)) => {
                let (user, password) = credentials.split_once(':').unwrap_or((credentials, ""));
                self.username = user.trim().to_string();
                self.password = password.trim().to_string();
                self.url = format!("{scheme}://{host}{path}");
            }
            None => {
                self.url = value.to_string();
            }
        }
    }

    /// 展示用地址：即便配置里残留了 `user:pass@`，也不会把密码显示出来。
    pub fn display_url(&self) -> String {
        let url = self.url.trim();
        let Some((scheme, rest)) = url.split_once("://") else {
            return url.to_string();
        };
        let authority_end = rest.find('/').unwrap_or(rest.len());
        let (authority, path) = rest.split_at(authority_end);
        match authority.rsplit_once('@') {
            Some((_, host)) => format!("{scheme}://{host}{path}"),
            None => url.to_string(),
        }
    }
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            dir: String::new(),
            quality: None,
            filename_template: "{singer} - {name}".to_string(),
            concurrency: 4,
            concurrent_songs: 2,
            multipart: true,
            multipart_min_size_mb: 5,
            max_retries: 3,
            verify_size: true,
            skip_existing: true,
            write_tags: true,
            embed_cover: true,
            save_lyric: true,
            webdav: WebdavConfig::default(),
            auto_cache_on_play: false,
            auto_cache_after_secs: 30,
        }
    }
}

impl DownloadConfig {
    /// 分片最小体积（字节），供下载引擎直接使用。
    pub fn multipart_min_size_bytes(&self) -> u64 {
        self.multipart_min_size_mb.saturating_mul(1024 * 1024)
    }
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
    #[serde(default)]
    pub download: DownloadConfig,
    #[serde(default)]
    pub keybindings: KeybindingConfig,
    #[serde(default)]
    pub notification: NotificationConfig,
    #[serde(default)]
    pub integration: IntegrationConfig,
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
            download: DownloadConfig::default(),
            keybindings: KeybindingConfig::default(),
            notification: NotificationConfig::default(),
            integration: IntegrationConfig::default(),
        }
    }
}

fn legacy_config_version() -> u32 {
    0
}

#[cfg(test)]
mod tests {
    use super::{DownloadConfig, LocalMusicConfig, StatusBarItem, UiConfig, WebdavConfig};

    #[test]
    fn webdav_defaults_are_disabled_and_parse_from_partial_toml() {
        let config: WebdavConfig =
            serde_json::from_value(serde_json::json!({ "url": "https://dav.example.com/dav" }))
                .unwrap();
        assert!(!config.enabled);
        assert_eq!(config.url, "https://dav.example.com/dav");
        assert!(config.username.is_empty());
        assert_eq!(config.dir, "voicefox");
        // 老配置文件里没有 webdav 段时用默认值。
        let download: DownloadConfig =
            serde_json::from_value(serde_json::json!({ "dir": "/music" })).unwrap();
        assert_eq!(download.webdav, WebdavConfig::default());
    }

    #[test]
    fn webdav_url_input_splits_embedded_credentials() {
        let mut config = WebdavConfig::default();
        config.apply_url_input("https://user:p%40ss@dav.example.com/remote.php/dav/");
        assert_eq!(config.url, "https://dav.example.com/remote.php/dav/");
        assert_eq!(config.username, "user");
        assert_eq!(config.password, "p%40ss");
        // 展示时不会再出现密码。
        assert_eq!(
            config.display_url(),
            "https://dav.example.com/remote.php/dav/"
        );

        // 不带账号的地址只改地址，已有账号保持不变。
        config.apply_url_input("https://dav.example.com/other");
        assert_eq!(config.url, "https://dav.example.com/other");
        assert_eq!(config.username, "user");
    }

    #[test]
    fn webdav_display_url_hides_legacy_credentials() {
        let config = WebdavConfig {
            url: "https://user:secret@dav.example.com/dav".to_string(),
            ..WebdavConfig::default()
        };
        assert_eq!(config.display_url(), "https://dav.example.com/dav");
    }

    #[test]
    fn missing_download_section_uses_defaults() {
        let config: crate::model::config::Config =
            serde_json::from_value(serde_json::json!({ "player": { "volume": 50 } })).unwrap();

        assert_eq!(config.download, DownloadConfig::default());
        assert_eq!(config.download.filename_template, "{singer} - {name}");
        assert_eq!(config.download.multipart_min_size_bytes(), 5 * 1024 * 1024);
        assert!(config.download.quality.is_none());
    }

    #[test]
    fn partial_download_section_keeps_other_defaults() {
        let config: crate::model::config::Config = serde_json::from_value(serde_json::json!({
            "download": { "dir": "/music", "concurrency": 8 }
        }))
        .unwrap();

        assert_eq!(config.download.dir, "/music");
        assert_eq!(config.download.concurrency, 8);
        assert_eq!(config.download.concurrent_songs, 2);
        assert!(config.download.verify_size);
    }

    #[test]
    fn legacy_local_music_config_remains_enabled() {
        let config: LocalMusicConfig = serde_json::from_value(serde_json::json!({
            "paths": ["/music"],
            "max_depth": 4
        }))
        .unwrap();

        assert!(config.enabled);
        assert_eq!(config.paths, vec!["/music"]);
        assert_eq!(config.max_depth, 4);
    }

    #[test]
    fn explicit_local_music_disable_is_preserved() {
        let config: LocalMusicConfig = serde_json::from_value(serde_json::json!({
            "enabled": false,
            "paths": ["/music"]
        }))
        .unwrap();

        assert!(!config.enabled);
    }

    #[test]
    fn status_bar_items_ignore_unknown_values_and_duplicates() {
        let config: UiConfig = serde_json::from_value(serde_json::json!({
            "status_bar_items": ["source", "unknown", "song", "source"]
        }))
        .unwrap();

        assert_eq!(
            config.status_bar_items,
            vec![StatusBarItem::Source, StatusBarItem::Song]
        );
    }

    #[test]
    fn status_bar_items_preserve_an_explicit_empty_list() {
        let config: UiConfig = serde_json::from_value(serde_json::json!({
            "status_bar_items": []
        }))
        .unwrap();

        assert!(config.status_bar_items.is_empty());
    }

    #[test]
    fn status_bar_items_use_documented_config_names() {
        let config = UiConfig {
            status_bar_items: vec![StatusBarItem::PlayMode, StatusBarItem::JsSourceState],
            ..UiConfig::default()
        };

        let value = serde_json::to_value(config).unwrap();

        assert_eq!(
            value["status_bar_items"],
            serde_json::json!(["play-mode", "js-source-state"])
        );
    }
}
