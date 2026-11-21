use serde::{Deserialize, Deserializer, Serialize};

use super::source::{Quality, SourceId};
use crate::keybinding::KeybindingConfig;
use crate::traits::player::EqualizerBand;

pub const CURRENT_CONFIG_VERSION: u32 = 17;

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

/// 状态栏高度上限（行）。
///
/// 默认 1 行；上限 6 行 —— 把 `ui.status_bar_items` 全开时，一行放不下的段
/// 可以折到下面几行，而不是被收进「更多」。鼠标拖底栏顶边的把手即可在
/// 1..=6 之间调整（见 `pages::components::status_bar`）。
pub const STATUS_BAR_MAX_HEIGHT: u8 = 6;

/// 默认主题名：保持历史观感。
pub fn default_theme_name() -> String {
    "voicefox".to_string()
}

fn default_status_bar_height() -> u8 {
    1
}

fn default_page_step() -> usize {
    10
}

fn default_status_bar_items() -> Vec<StatusBarItem> {
    // 默认只保留用户播放时真正有用的信息；音源/JS 音源状态等诊断信息
    // 仍可在设置中手动打开，但不应该挤占每个页面的底部空间。
    vec![
        StatusBarItem::State,
        StatusBarItem::Song,
        StatusBarItem::Time,
        StatusBarItem::Volume,
        StatusBarItem::Queue,
    ]
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

/// 播放地址解析策略。
///
/// 只影响"这首歌去哪里拿播放地址"，不影响搜索默认音源（`source.default`）——
/// 两者混在一起会让用户分不清"换的是这首歌"还是"以后都用它"。
/// 「JS 音源还是内置音源先行」是另一个独立开关：[`SourceConfig::js_first`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SourcePolicy {
    /// 默认：内置原平台 → JS 音源（配置顺序）→ 跨平台严格匹配兜底。
    #[default]
    Auto,
    /// 跨平台兜底时，把指定平台的同曲排到候选最前；其余顺序不变。
    Prefer,
    /// 只用指定平台解析（声明支持该平台的 JS 音源仍参与），不做跨平台兜底。
    Only,
}

impl SourcePolicy {
    pub fn as_config(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Prefer => "prefer",
            Self::Only => "only",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "自动",
            Self::Prefer => "优先指定平台",
            Self::Only => "只用指定平台",
        }
    }
}

/// 「界面强调色跟随专辑封面」的强度档位。
///
/// 封面主色在发布前会归一化到鲜艳区间（见 app 的 cover::accent），
/// 档位只控制它混入 accent 的比例：轻微适合想保留主题个性的场景，
/// 明显是默认值——之前的版本提取色偏暗、混合后几乎无感，已归一化修复。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AccentFollowCover {
    /// 不跟随，accent 恒为主题/配置色。
    Off,
    /// 轻微：封面主色以较低比例（0.45）混入。
    Subtle,
    /// 明显：封面主色以较高比例（0.75）混入。
    #[default]
    Strong,
}

impl AccentFollowCover {
    pub fn as_config(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Subtle => "subtle",
            Self::Strong => "strong",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "关闭",
            Self::Subtle => "轻微",
            Self::Strong => "明显",
        }
    }

    /// 设置页循环的下一档。
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Subtle,
            Self::Subtle => Self::Strong,
            Self::Strong => Self::Off,
        }
    }
}

/// 宽松解析「封面主色跟随」：接受 kebab-case 字符串，也接受早期示例发布过的
/// 布尔值（`true` → `Strong`、`false` → `Off`），抄过旧示例配置的用户不会
/// 因类型变更而整份配置解析失败。
fn deserialize_accent_follow_cover<'de, D>(deserializer: D) -> Result<AccentFollowCover, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Bool(bool),
        Text(String),
    }
    match Raw::deserialize(deserializer) {
        Ok(Raw::Bool(true)) => Ok(AccentFollowCover::Strong),
        Ok(Raw::Bool(false)) => Ok(AccentFollowCover::Off),
        Ok(Raw::Text(text)) => match text.trim().to_ascii_lowercase().as_str() {
            "off" | "false" => Ok(AccentFollowCover::Off),
            "subtle" => Ok(AccentFollowCover::Subtle),
            "strong" | "true" => Ok(AccentFollowCover::Strong),
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["off", "subtle", "strong"],
            )),
        },
        Err(error) => Err(error),
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
    /// 解析策略；默认 `auto`。
    #[serde(default)]
    pub policy: SourcePolicy,
    /// 解析策略作用的平台；`auto` 时忽略。
    #[serde(default)]
    pub policy_platform: Option<SourceId>,
    /// 「JS 音源优先」：开启后播放地址先问 JS 音源（配置顺序），失败再回退
    /// 内置音源。默认关闭——单个挂死的 JS 音源曾让本可直接播放的歌一起变慢；
    /// 依赖 JS 音源解 VIP 曲目的用户可以打开这个开关恢复旧行为。
    #[serde(default)]
    pub js_first: bool,
    /// V1 双栈总闸（musid-dli 接入，opt-in）：false 时搜索/流地址/歌词四面
    /// 全部走内置旧路径。开启还需编译期 `musid-dli-adapter` feature 与按源
    /// 子 feature（`musid-dli-adapter-*`）；config 开而 feature 关 → 该面
    /// 静默走旧路径 + 启动时一次 warn。配置形状不随 feature 变化：
    /// 字段恒解析、恒默认 false（docs/MUSID_DLI_ADAPTER.md §11）。
    #[serde(default)]
    pub musid_stack: bool,
    /// 搜索面双栈（单源与聚合同规则；musid 影子源独立注册，不覆盖内置）。
    #[serde(default)]
    pub musid_search: bool,
    /// 流地址面双栈（内置槽位前插 musid 判定；js_first 语义不变）。
    #[serde(default)]
    pub musid_song_url: bool,
    /// 歌词面双栈（运行时另需编译期 `musid-dli-adapter-lyrics` 歌词转换
    /// 变体；缺 feature 时该面不生效 + 启动 warn）。
    #[serde(default)]
    pub musid_lyric: bool,
    /// parity 对账影子模式（恒纯观测）：musid 参与但恒展示旧路径结果
    /// （musid 产物一律不进用户路径），仅内存计数对照且不阻塞用户路径
    /// （搜索/歌词真双跑；流地址面只做失败侧对账，避免双倍平台配额）。
    #[serde(default)]
    pub musid_parity_probe: bool,
}

impl Default for SourceConfig {
    fn default() -> Self {
        Self {
            enabled: SourceId::default_enabled().to_vec(),
            default: SourceId::Kw,
            auto_toggle: true,
            js_sources: vec![],
            policy: SourcePolicy::Auto,
            policy_platform: None,
            js_first: false,
            musid_stack: false,
            musid_search: false,
            musid_song_url: false,
            musid_lyric: false,
            musid_parity_probe: false,
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
    /// 界面主题名。
    ///
    /// - `voicefox`（默认）：使用下面这套手工调好的 Catppuccin 槽位，观感与历史版本一致；
    /// - 其它取值：来自 `ratatui-themes` 的具名主题（kebab-case，如 `dracula` /
    ///   `tokyo-night` / `nord` / `catppuccin-mocha`），整份皮肤由该主题的语义色推导。
    ///
    /// 名字不合法时回退到 `voicefox`，不会让界面变成读不清的颜色。
    #[serde(default = "default_theme_name")]
    pub name: String,
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
            name: default_theme_name(),
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

/// 歌曲列表表格的列配置（可持久化，支持用户手动调整列宽和隐藏）。
///
/// 每个页面可以有一套独立的列配置，key 使用页面标识（如 `"queue"`、`"search"`）。
/// 用户未调整过的页面走 `song_table.rs` 里的默认档位公式，不会写入 Config。
///
/// `serde(default)` 不可省略：这一节是用户可手写的，缺字段（或将来新增字段）
/// 时若反序列化失败，会让整份 `Config` 解析失败并**直接导致程序启动失败**。
/// 非法取值（`min_width > max_width`、`width = 0`）由
/// `song_table::load_columns_for_page` 在读取时校正。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TableColumnConfig {
    pub key: String,
    pub label: String,
    pub visible: bool,
    pub width: u16,
    pub min_width: u16,
    pub max_width: u16,
}

impl Default for TableColumnConfig {
    fn default() -> Self {
        Self {
            key: String::new(),
            label: String::new(),
            visible: true,
            width: 8,
            min_width: 2,
            max_width: 64,
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
    /// PgUp/PgDn 键盘翻页步长（行数）。
    ///
    /// 与滚轮 `scroll_amount` 语义不同：滚轮是高频小步，翻页是低频大步，
    /// 所以分开两个字段。此前各页面硬编码 5/10/15 三种步长，已统一到这里。
    #[serde(default = "default_page_step")]
    pub page_step: usize,
    /// 界面强调色跟随专辑封面主色的档位。
    #[serde(default, deserialize_with = "deserialize_accent_follow_cover")]
    pub accent_follow_cover: AccentFollowCover,
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
    /// 底部状态栏高度（行数）：1..=6，默认 1。
    ///
    /// 默认 1 —— 默认只开 5 个字段，一行足够。把 `ui.status_bar_items` 全开、
    /// 想一眼看到全部可交互控件时再调大；也可以在界面上直接拖底栏顶边的
    /// 拖拽把手改（鼠标拖拽/落盘见 `pages::components::status_bar`）。
    /// 取值会被夹到 1..=[`STATUS_BAR_MAX_HEIGHT`]。
    #[serde(default = "default_status_bar_height")]
    pub status_bar_height: u8,
    /// 用户自定义的歌曲列表列配置，按页面 key 存储。
    /// 空 HashMap 表示所有页面都使用默认档位公式。
    #[serde(default)]
    pub table_columns: std::collections::HashMap<String, Vec<TableColumnConfig>>,
    /// 用户拖拽出的面板分隔比例，按页面 key 存储。
    ///
    /// 形如 `{"queue": {"wide_columns": 0.4, "narrow_queue": 0.6}}`。
    /// 用嵌套表而不是固定字段，是为了以后加 Pane 时不必再动配置结构；
    /// 缺省（或页面没拖过）时各页面走自己的内置默认比例。
    #[serde(default)]
    pub pane_ratios: std::collections::HashMap<String, std::collections::HashMap<String, f32>>,
    /// 频谱可视化：`off`（默认）或 `bars`。
    ///
    /// `bars` 采集系统输出监视流画柱状频谱（Linux，需要 pw-record 或
    /// parec）；运行时按快捷键切换并落盘到这里。
    #[serde(default)]
    pub visualizer: String,
    /// 频谱风格：`classic`（默认，cava-like 柱状）或 `modern`（更紧凑、降低间隙）。
    #[serde(default)]
    pub visualizer_style: String,
}

impl UiConfig {
    /// 频谱可视化是否开启（未知取值一律视为关闭）。
    pub fn visualizer_enabled(&self) -> bool {
        self.visualizer.eq_ignore_ascii_case("bars")
    }
}

impl UiConfig {
    /// 实际使用的状态栏行数（写坏配置也不会让布局失真）。
    pub fn status_bar_rows(&self) -> u16 {
        self.status_bar_height.clamp(1, STATUS_BAR_MAX_HEIGHT) as u16
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            enable_mouse: true,
            wrap_navigation: true,
            scroll_amount: 3,
            page_step: default_page_step(),
            accent_follow_cover: AccentFollowCover::default(),
            aggregate_search: true,
            sidebar_transparent: false,
            sidebar_style: None,
            show_cover: true,
            cover_protocol: "auto".to_string(),
            show_notifications: None,
            notification_timeout: None,
            max_fps: 20,
            status_bar_items: default_status_bar_items(),
            status_bar_height: default_status_bar_height(),
            table_columns: std::collections::HashMap::new(),
            visualizer: "off".to_string(),
            visualizer_style: "classic".to_string(),
            pane_ratios: std::collections::HashMap::new(),
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
    /// Windows 上接入 SMTC（System Media Transport Controls），
    /// 硬件媒体键与系统媒体浮层可控制播放。
    pub smtc: bool,
    /// macOS 上接入 Now Playing（MPRemoteCommandCenter），
    /// 触摸栏 / 耳机按键 / 控制中心可控制播放。
    #[serde(alias = "now_playing")]
    pub now_playing: bool,
    /// ListenBrainz 用户 token（ui 头像页右上角「Profile」里可见）。
    /// 留空表示不上报听歌记录；填写后开始播放 30 秒上报一次 listen。
    #[serde(default)]
    pub listenbrainz_token: String,
}

impl Default for IntegrationConfig {
    fn default() -> Self {
        Self {
            mpris: true,
            smtc: true,
            now_playing: true,
            listenbrainz_token: String::new(),
        }
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
    use super::{
        AccentFollowCover, Config, DownloadConfig, LocalMusicConfig, STATUS_BAR_MAX_HEIGHT,
        SourceConfig, SourceId, SourcePolicy, StatusBarItem, UiConfig, WebdavConfig,
    };

    #[test]
    fn ui_page_step_and_accent_follow_cover_have_sane_defaults() {
        let ui = UiConfig::default();
        assert_eq!(ui.page_step, 10, "PgUp/PgDn 翻页步长默认 10");
        assert_eq!(
            ui.accent_follow_cover,
            AccentFollowCover::Strong,
            "封面主色跟随默认「明显」档"
        );
        // 页面步长与滚轮步长语义不同，默认值不应相同（滚轮 3 / 翻页 10）。
        assert_ne!(ui.page_step, ui.scroll_amount);
    }

    #[test]
    fn accent_follow_cover_accepts_legacy_bools_and_strings() {
        let parse = |value: serde_json::Value| -> Result<UiConfig, serde_json::Error> {
            serde_json::from_value(serde_json::json!({ "accent_follow_cover": value }))
        };
        // 早期示例发布过布尔值。
        assert_eq!(
            parse(serde_json::json!(true)).unwrap().accent_follow_cover,
            AccentFollowCover::Strong
        );
        assert_eq!(
            parse(serde_json::json!(false)).unwrap().accent_follow_cover,
            AccentFollowCover::Off
        );
        // 新写法与大小写/空白宽容。
        assert_eq!(
            parse(serde_json::json!("strong"))
                .unwrap()
                .accent_follow_cover,
            AccentFollowCover::Strong
        );
        assert_eq!(
            parse(serde_json::json!(" Subtle "))
                .unwrap()
                .accent_follow_cover,
            AccentFollowCover::Subtle
        );
        assert_eq!(
            parse(serde_json::json!("off")).unwrap().accent_follow_cover,
            AccentFollowCover::Off
        );
        // 未知取值拒绝，避免静默回退成用户不想要的档位。
        assert!(parse(serde_json::json!("blazing")).is_err());
    }

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

    /// V1 双栈 opt-in 开关：默认全 false（不开 opt-in 时旧实现唯一生效），
    /// 且配置形状不随 feature 变化——字段恒解析（缺失/显式取值两种形态）。
    #[test]
    fn musid_stack_flags_default_off_and_parse_in_any_shape() {
        let default = SourceConfig::default();
        assert!(
            !default.musid_stack
                && !default.musid_search
                && !default.musid_song_url
                && !default.musid_lyric
                && !default.musid_parity_probe,
            "双栈总闸与四面开关必须全部默认 false"
        );

        // 旧配置完全不含 musid 字段：照常解析，默认 false。
        let legacy: SourceConfig = serde_json::from_value(serde_json::json!({
            "enabled": ["kw", "wy"],
            "default": "kw"
        }))
        .expect("旧配置解析");
        assert!(!legacy.musid_stack);

        // 显式开启（含 probe）：形状一致、逐字段落位。
        let enabled: SourceConfig = serde_json::from_value(serde_json::json!({
            "musid_stack": true,
            "musid_search": true,
            "musid_song_url": true,
            "musid_lyric": true,
            "musid_parity_probe": true
        }))
        .expect("musid 配置解析");
        assert!(
            enabled.musid_stack
                && enabled.musid_search
                && enabled.musid_song_url
                && enabled.musid_lyric
                && enabled.musid_parity_probe
        );

        // 序列化往返保真（设置页落盘路径）。
        let round_trip: SourceConfig =
            serde_json::from_value(serde_json::to_value(&enabled).expect("序列化")).expect("回读");
        assert_eq!(round_trip.musid_parity_probe, enabled.musid_parity_probe);
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

    /// 旧配置（没有 status_bar_height / policy 字段）必须照旧加载，
    /// 且新字段落到"零行为变化"的默认值上。
    #[test]
    fn new_status_bar_and_policy_fields_are_backward_compatible() {
        // 用 JSON 表达"旧配置文件里没有这两个字段"（core 不依赖 toml；
        // 真实 TOML 加载路径由 app 的配置加载测试覆盖）。
        let config: Config =
            serde_json::from_str("{\"ui\":{\"enable_mouse\":true}}").expect("旧配置应当能解析");
        assert_eq!(config.ui.status_bar_height, 1, "默认仍是一行");
        assert_eq!(config.ui.status_bar_rows(), 1);
        assert_eq!(
            config.source.policy,
            SourcePolicy::Auto,
            "默认必须等价于历史行为"
        );
        assert_eq!(config.source.policy_platform, None);
    }

    #[test]
    fn status_bar_height_is_clamped_to_a_sane_range() {
        let mut config = Config::default();
        config.ui.status_bar_height = 0;
        assert_eq!(config.ui.status_bar_rows(), 1, "0 行夹回 1 行");
        config.ui.status_bar_height = 2;
        assert_eq!(config.ui.status_bar_rows(), 2);
        config.ui.status_bar_height = STATUS_BAR_MAX_HEIGHT;
        assert_eq!(
            config.ui.status_bar_rows(),
            u16::from(STATUS_BAR_MAX_HEIGHT),
            "上限本身必须可用"
        );
        config.ui.status_bar_height = 99;
        assert_eq!(
            config.ui.status_bar_rows(),
            u16::from(STATUS_BAR_MAX_HEIGHT)
        );
        assert_eq!(STATUS_BAR_MAX_HEIGHT, 6, "上限就是拖拽能拖到的最大行数");
    }

    #[test]
    fn policies_round_trip_through_config_text() {
        let config: Config =
            serde_json::from_str("{\"source\":{\"policy\":\"only\",\"policy_platform\":\"wy\"}}")
                .expect("策略配置应当能解析");
        assert_eq!(config.source.policy, SourcePolicy::Only);
        assert_eq!(config.source.policy_platform, Some(SourceId::Wy));
        assert_eq!(config.source.policy.as_config(), "only");
        assert_eq!(SourcePolicy::Prefer.label(), "优先指定平台");
    }
}
