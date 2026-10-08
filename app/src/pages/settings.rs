//! 设置页面：支持 JS 音源 URL 或本地路径导入/删除

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lx_core::events::AppAction;
use lx_core::keybinding::{Action, KeybindingResolver};
use lx_core::model::config::{SourcePolicy, StatusBarItem};
use lx_core::model::login::QrLoginKind;
use lx_core::model::source::{Quality, SourceId};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};

use crate::fmt::{format_bytes, format_duration};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};
use unicode_width::UnicodeWidthStr;

use crate::context::AppContext;
use crate::pages::components::context_menu::{
    MenuAction, MenuItem, MenuOutcome, SongContextMenu, StatusBarMenuAction, submenu,
};
use crate::pages::components::hit_test::{PANEL_BORDERS, panel_inner};
use crate::pages::components::remote_collections::{
    RemoteCollectionsOutcome, RemoteCollectionsWindow,
};
use crate::pages::components::splitter::{
    DividerHit, GUTTER, SplitAxis, Splitter, clamp_extent, clamp_ratio, divider_line,
    split_with_gutter,
};
use crate::pages::components::text::pad_display;
use crate::playlist::mode::PlayMode;

/// 删除类操作（音源 / 本地目录）二次确认的窗口时长
const DELETE_CONFIRM_WINDOW: Duration = Duration::from_secs(5);

/// 检查 JS 音源是否已缓存到本地
fn is_source_cached(url: &str) -> bool {
    lx_source::js::loader::is_source_cached(url)
}

/// 压缩音源地址用于一行的展示栏（超长时保留开头 + `...`）。
///
/// 与 [`truncate_display`] / [`truncate_width`] 刻意不同：这里用三个点而不是
/// 省略号 `…`，且**不补齐**（返回值宽度可变）。它是历史展示格式，改动会波及
/// 多处快照式断言，因此保留现状，只在文档里写清区别。
fn shorten_source(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    let visible_chars = max_chars.saturating_sub(3);
    format!(
        "{}...",
        value.chars().take(visible_chars).collect::<String>()
    )
}

/// 把 `value` 压进**恰好** `max_chars` 显示列：超宽截断加省略号，然后补空格。
///
/// 调用点都靠它对齐（名称列 + `· N 首` 同一行、固定 18 列的本地文件行），
/// 所以输出宽度必须恒定。旧实现只在"没超宽"的分支补空格，截断分支直接返回，
/// 于是 `truncate_display("晴晴晴", 4)` 只有 3 列（`"晴…"`）——宽字符边界上
/// 整行会向左串一列。现在统一走 [`truncate_width`] + [`pad_display`]。
fn truncate_display(value: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    pad_display(
        crate::pages::components::text::truncate_width(value, max_chars).as_ref(),
        max_chars,
    )
}

/// 方向键 / 激活键当前归谁。
///
/// 设置页现在只有一个面板：分类栏（宽屏）/ 分类列表（窄屏）+ 分类内容。
/// `Options` 持有分类里的**设置行光标**；其余四个变体持有分类内容里
/// **内嵌的管理列表**（原来那排下方面板的内容，见 `embedded_list_for`）。
///
/// 原来的 `next()`（`s` 循环五个区域）随"下方管理面板排"一起删除：
/// 进入分类就能看到该分类的内嵌列表，用 ↑/↓ 或鼠标点击即可把焦点交给它。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsFocus {
    /// 设置行光标（分类栏 + 设置项）。
    Options,
    /// 音源分类内嵌的 JS 音源列表。
    JsSources,
    /// 数据与本地库分类内嵌的本地音乐目录列表。
    LocalPaths,
    /// 界面分类内嵌的状态栏字段列表。
    StatusBar,
    /// 账号与扫码分类内嵌的扫码登录列表。
    QrLogin,
}

/// 窄屏（< 100 列）单栏视图的两个层级。
///
/// 窄屏放不下"左分类栏 + 右设置项"，因此先只显示分类列表，
/// 选中分类按 Enter 进入后显示该分类的设置项，Esc 返回分类列表。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NarrowPane {
    Categories,
    Rows,
}

/// 设置行的稳定标识。构造时按顺序分配，键盘光标与鼠标命中共用同一个 id，
/// 分类过滤只按 `SettingsRowMeta::category` 做，不再依赖下标区间。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct SettingsRowId(u32);

/// 设置行的交互类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsRowKind {
    /// 布尔开关：Enter 激活、Space 快速切换。
    Toggle,
    /// 多值枚举：Enter 打开取值菜单（当前值打 ✓）或直接推进档位。
    ///
    /// 候选值能列全的（主题 / 默认音源 / 解析策略 / 均衡器 / 状态栏字段 / 音质 /
    /// 播放模式 / 音源开关 / 歌词偏移）走取值菜单；数值档位（超时 / FPS / 分片…）
    /// 走 `RowPlan::Direct` 里的 `next_*` 推进。两种都不再依赖行内快捷键。
    Enum,
    /// 一次性动作：Enter 触发。
    Action,
    /// 需要文本输入的动作：Enter 打开输入浮层。
    Input,
    /// 纯说明 / 只读展示：可选中，但 Enter 不触发任何业务动作。
    Info,
}

/// 一条设置行的元数据。
///
/// `category` 与 `plan` 都在**每一行的构造点显式声明**（不再用
/// `option_indices` 这样的下标区间推断，也不再靠解析键位字符串推断激活方式，
/// 插入一行不会让分类与激活方式错位）；`key` 只是键位列的显示文案。
#[derive(Debug, Clone, PartialEq, Eq)]
struct SettingsRowMeta {
    id: SettingsRowId,
    label: String,
    /// 键位列显示文案：删掉逐行快捷键后一律是 `Enter`（封面协议那行显示真实组合键）。
    key: String,
    category: SettingsCategory,
    kind: SettingsRowKind,
    /// 激活计划：键盘 `Enter`/`Space` 与鼠标点击**共用**的唯一决策点。
    plan: RowPlan,
}

/// 渲染时记录的"行矩形 + 行 id"。
///
/// 渲染 Rect 与命中 Rect 共用同一次计算（见 `render_setting_rows`），
/// 鼠标命中只查这份账本，不存在硬编码坐标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SettingsRowHit {
    rect: Rect,
    id: SettingsRowId,
}

/// 内嵌管理列表的一条条目命中账本。
///
/// `rect` 是整行的矩形（点它 = 选中该条）；`checkbox` 是行内勾选框的矩形
/// （只有状态栏字段列表有，点它 = 切换开关）。两者都来自渲染时的同一次排版。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EmbeddedRowHit {
    rect: Rect,
    checkbox: Option<Rect>,
    index: usize,
}

impl EmbeddedRowHit {
    /// 指针是否落在这一行上。
    fn contains(&self, position: Position) -> bool {
        self.rect.contains(position)
    }

    /// 指针是否落在这一行的勾选框上。
    fn checkbox_at(&self, position: Position) -> bool {
        self.checkbox
            .is_some_and(|checkbox| checkbox.contains(position))
    }
}

/// 内嵌管理列表的命中账本：条目行 + 命令按钮。
///
/// 一次只有一个内嵌列表可见，因此四个列表共用这一份账本；
/// 鼠标命中只查它，不存在硬编码坐标。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct EmbeddedHits {
    rows: Vec<EmbeddedRowHit>,
    commands: Vec<(Rect, char)>,
}

impl EmbeddedHits {
    /// 命中条目行。
    fn row_at(&self, position: Position) -> Option<EmbeddedRowHit> {
        self.rows.iter().find(|hit| hit.contains(position)).copied()
    }

    /// 命中命令行按钮。
    fn command_at(&self, position: Position) -> Option<char> {
        self.commands
            .iter()
            .find(|(rect, _)| rect.contains(position))
            .map(|(_, key)| *key)
    }
}

/// 设置行的**直接动作**：不再依赖任何行内快捷键。
///
/// 每个变体只描述"这一行该做什么"，执行统一在
/// [`SettingsPage::apply_direct_action`] 里复用既有 `AppAction` / `update_config`
/// 业务分支。逐行快捷键删掉之后，行激活（`Enter` / `Space` / 鼠标点击）就是
/// 这些设置项**唯一**的入口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsRowDirectAction {
    // ── 界面 ──
    ToggleMouse,
    ToggleAggregateSearch,
    ToggleWrapNavigation,
    ToggleShowCover,
    ToggleRememberPlaybackState,
    CycleNetworkTimeout,
    CycleCoverProtocol,
    CycleMaxFps,
    CyclePageStep,
    CycleAccentFollowCover,
    ToggleTrackChangeNotification,
    // ── 播放 ──
    CyclePlaybackSpeed,
    EditAudioDevice,
    CycleReplayGainMode,
    CycleReplayGainPreamp,
    CycleChannelMode,
    CycleBalance,
    ToggleReplayGainClip,
    CycleFadeInDuration,
    CycleFadeOutDuration,
    RunFadeIn,
    RunFadeOut,
    SetAbLoopStart,
    SetAbLoopEnd,
    ClearAbLoop,
    CycleHistoryLimit,
    // ── 音源与歌词 ──
    ToggleAutoSource,
    ToggleLyricTranslation,
    ToggleLyricYrc,
    EditProxy,
    ReloadJsSources,
    CheckSourceHealth,
    // ── 账号与扫码 ──
    QrLoginSelected,
    SyncNetease,
    SyncQq,
    ImportExternalPlaylist,
    // ── 通知与集成 ──
    CycleScrollAmount,
    ToggleMpris,
    ToggleInAppNotification,
    CycleInAppTimeout,
    ToggleDesktopNotification,
    ToggleNotificationAlbumCover,
    // ── 下载 ──
    EditDownloadDir,
    CycleDownloadQuality,
    EditFilenameTemplate,
    ToggleMultipart,
    CycleMultipartMinSize,
    CycleDownloadConcurrency,
    CycleConcurrentSongs,
    CycleMaxRetries,
    ToggleVerifySize,
    ToggleSkipExisting,
    ToggleWriteTags,
    ToggleEmbedCover,
    ToggleSaveLyric,
    // ── 数据与本地库 ──
    CycleScanDepth,
    ExportData,
    ImportData,
    ClearCoverCache,
    ClearRemoteCache,
}

/// 枚举行的取值菜单来源。
///
/// 每个变体对应一条设置行的候选取值集合；选项定义在 `enum_menu`（纯数据），
/// 选中后由设置页自己写配置或派发既有 `AppAction`，不依赖"循环键"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsEnumPicker {
    Quality,
    PlayMode,
    Theme,
    DefaultSource,
    SourcePolicy,
    Equalizer,
    StatusBarItems,
    /// 歌词整体偏移（毫秒）。
    LyricOffset,
    /// 在线音源的启用开关（菜单里每项各自打 ✓）。
    EnabledSources,
}

impl SettingsEnumPicker {
    /// 取值动作 `MenuAction::SettingChoice` 里携带的设置行标识。
    ///
    /// 这些字符串只在本文件内产生与消费，是"行 → 配置字段"的唯一契约。
    fn row_id(self) -> &'static str {
        match self {
            Self::Quality => SETTING_ROW_QUALITY,
            Self::PlayMode => SETTING_ROW_PLAY_MODE,
            Self::Theme => SETTING_ROW_THEME,
            Self::DefaultSource => SETTING_ROW_DEFAULT_SOURCE,
            Self::SourcePolicy => SETTING_ROW_SOURCE_POLICY,
            Self::Equalizer => SETTING_ROW_EQUALIZER,
            Self::StatusBarItems => SETTING_ROW_STATUS_BAR_ITEM,
            Self::LyricOffset => SETTING_ROW_LYRIC_OFFSET,
            Self::EnabledSources => SETTING_ROW_SOURCE_ENABLED,
        }
    }
}

/// 行激活计划（纯数据，便于测试）。
///
/// **没有 `Key` 变体**：设置页不再"回灌按键"，每一行都必须显式声明自己要
/// 打开哪个取值菜单、或直接执行哪个动作。被删掉的 80 多个行内快捷键因此
/// 不可能再从行表里偷偷回来。
#[derive(Debug, Clone, PartialEq, Eq)]
enum RowPlan {
    /// 打开取值菜单（枚举）。
    Menu(SettingsEnumPicker),
    /// 直接动作：由设置页调用既有 `AppAction` / 写配置。
    Direct(SettingsRowDirectAction),
    /// 纯说明行：不触发任何业务动作。
    Inert,
}

impl RowPlan {
    /// 键位列的显示文案。
    ///
    /// 删掉逐行快捷键之后，设置行只能靠 `Enter`（或鼠标点击）激活，
    /// 因此这里一律显示 `Enter`，不再广告任何已删除的字母键。
    fn key_hint(&self) -> &'static str {
        match self {
            Self::Menu(_) | Self::Direct(_) => DEFAULT_ACTIVATION_KEY,
            Self::Inert => "-",
        }
    }
}

/// 行构造点使用的颜色组合，避免 builder 方法参数过多。
#[derive(Debug, Clone, Copy)]
struct RowPalette {
    accent: Color,
    muted: Color,
}

/// 一行设置项：元数据 + 渲染行。行 id 由 builder 分配。
struct SettingsRow {
    meta: SettingsRowMeta,
    line: Line<'static>,
}

/// 设置行构造器。
///
/// 这是设置行唯一的构造点：`setting_line` / `setting_value_line` / `setting_row`
/// 三个渲染构造点都从这里过一遍，顺带把标签、分类、类型、激活计划记成元数据。
#[derive(Default)]
struct SettingsRows {
    rows: Vec<SettingsRow>,
}

impl SettingsRows {
    fn new() -> Self {
        Self { rows: Vec::new() }
    }

    fn push(
        &mut self,
        category: SettingsCategory,
        kind: SettingsRowKind,
        label: &str,
        key: &str,
        plan: RowPlan,
        line: Line<'static>,
    ) -> SettingsRowId {
        let id = SettingsRowId(self.rows.len() as u32);
        self.rows.push(SettingsRow {
            meta: SettingsRowMeta {
                id,
                label: label.to_string(),
                key: key.to_string(),
                category,
                kind,
                plan,
            },
            line,
        });
        id
    }

    /// 布尔开关行（Enter / Space / 点击都能切换）。
    fn toggle(
        &mut self,
        category: SettingsCategory,
        label: &str,
        plan: RowPlan,
        value: bool,
        palette: RowPalette,
    ) -> SettingsRowId {
        let key = plan.key_hint();
        let line = setting_line(label, value, key, palette.accent, palette.muted);
        self.push(category, SettingsRowKind::Toggle, label, key, plan, line)
    }

    /// 取值行（枚举 / 动作 / 输入 / 只读说明）。键位列由激活计划决定。
    fn value(
        &mut self,
        category: SettingsCategory,
        kind: SettingsRowKind,
        label: &str,
        plan: RowPlan,
        value: &str,
        palette: RowPalette,
    ) -> SettingsRowId {
        let key = plan.key_hint();
        let line = if kind == SettingsRowKind::Action {
            setting_action_line(label, value, key, palette.accent, palette.muted)
        } else {
            setting_value_line(label, value, key, palette.accent, palette.muted)
        };
        self.push(category, kind, label, key, plan, line)
    }

    /// 键位列需要显示**真实组合键**的行。
    ///
    /// 只有「封面协议」用它：`Shift+P` 是设置页刻意保留的页面级组合键
    /// （见 `COVER_PROTOCOL_ROW_KEY`），键位列必须把真正生效的键写出来。
    #[allow(clippy::too_many_arguments)]
    fn value_with_key_hint(
        &mut self,
        category: SettingsCategory,
        kind: SettingsRowKind,
        label: &str,
        plan: RowPlan,
        key_hint: &str,
        value: &str,
        palette: RowPalette,
    ) -> SettingsRowId {
        let line = setting_value_line(label, value, key_hint, palette.accent, palette.muted);
        self.push(category, kind, label, key_hint, plan, line)
    }

    /// 当前分类的可见行（显示顺序 = 构造顺序）。
    fn rows_in(&self, category: SettingsCategory) -> Vec<&SettingsRow> {
        self.rows
            .iter()
            .filter(|row| row.meta.category == category)
            .collect()
    }

    /// 所有行的元数据（键盘光标按分类过滤）。
    fn metas(&self) -> Vec<SettingsRowMeta> {
        self.rows.iter().map(|row| row.meta.clone()).collect()
    }
}

/// 分类栏的显示顺序（窄屏单栏视图共用同一顺序）。
const SETTINGS_CATEGORIES: [SettingsCategory; 7] = [
    SettingsCategory::Interface,
    SettingsCategory::Playback,
    SettingsCategory::Sources,
    SettingsCategory::Accounts,
    SettingsCategory::Integration,
    SettingsCategory::Download,
    SettingsCategory::Data,
];

/// 宽屏（≥ 100 列）才在设置项面板左侧显示分类栏。
const CATEGORY_SIDEBAR_MIN_WIDTH: u16 = 100;
/// 分类栏宽度（含左右边框的整块宽度）。
const CATEGORY_SIDEBAR_WIDTH: u16 = 18;
/// 分类栏宽度的夹取范围（列，含左右边框）。
///
/// 下界 12 列 = 10 列可用内容：加上边框后仍放得下 `▶ ` 与最长的分类名
/// （「数据与本地库」14 列时会截断，但不会被挤成 0 列）。
const CATEGORY_SIDEBAR_WIDTH_MIN: u16 = 12;
const CATEGORY_SIDEBAR_WIDTH_MAX: u16 = 40;
/// 宽屏左右分栏后，右侧内容区至少要保留的列数（已扣掉 1 列 gutter）。
///
/// 24 列 = 设置行的键位列 + 标签列 + 一个取值列：小终端上宁可把分类栏压到
/// 下界，也不能把内容挤成 0 宽（0 宽会让分界线一起消失）。
const CONTENT_MIN_WIDTH: u16 = 24;

/// `MenuAction::SettingChoice::row` 用的设置行标识。
///
/// 只有本文件会构造和解释这些字符串；取值（`value`）的标识在各行的
/// 选项表里声明（主题用皮肤名、音源用 `SourceId::as_str`、音质用标签…）。
const SETTING_ROW_QUALITY: &str = "quality";
const SETTING_ROW_PLAY_MODE: &str = "play-mode";
const SETTING_ROW_THEME: &str = "theme";
const SETTING_ROW_DEFAULT_SOURCE: &str = "default-source";
const SETTING_ROW_SOURCE_POLICY: &str = "source-policy";
/// 解析策略菜单里"目标平台"子菜单的行标识（只写 `policy_platform`）。
const SETTING_ROW_SOURCE_POLICY_PLATFORM: &str = "source-policy-platform";
const SETTING_ROW_EQUALIZER: &str = "equalizer";
/// 状态栏字段是"每个字段一个独立开关"，因此一行对应多个取值。
const SETTING_ROW_STATUS_BAR_ITEM: &str = "status-bar-item";
/// 歌词整体偏移（毫秒）：Enter 打开取值菜单。
const SETTING_ROW_LYRIC_OFFSET: &str = "lyric-offset";
/// 在线音源启用开关：一行对应多个取值（每个音源一个开关）。
const SETTING_ROW_SOURCE_ENABLED: &str = "source-enabled";

/// 取值菜单里可选的音质档位（与 `next_quality` 的循环顺序一致）。
const QUALITY_CHOICES: [Quality; 4] = [
    Quality::Low128,
    Quality::High320,
    Quality::Flac,
    Quality::Flac24,
];

/// 取值菜单里可选的播放模式（与 `PlayMode::next_mode` 的循环顺序一致）。
const PLAY_MODE_CHOICES: [PlayMode; 5] = [
    PlayMode::ListLoop,
    PlayMode::SingleLoop,
    PlayMode::Random,
    PlayMode::List,
    PlayMode::None,
];

/// 取值菜单里可选的解析策略（顺序与循环键理解一致：自动 → 优先 → 只用）。
const SOURCE_POLICY_CHOICES: [SourcePolicy; 3] =
    [SourcePolicy::Auto, SourcePolicy::Prefer, SourcePolicy::Only];

/// 取值菜单里可选的歌词偏移档位（毫秒）。
///
/// 旧版只能靠 `[` / `]` 一次 ±100 ms 试：删掉行内快捷键后改由菜单直接选，
/// 并且**双向可达**（含负值），不会出现"只能往一个方向调"。
const LYRIC_OFFSET_CHOICES: [i32; 7] = [-500, -300, -100, 0, 100, 300, 500];

/// 下载设置里的文本输入目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DownloadInputTarget {
    /// 下载目录
    Dir,
    /// 文件名模板
    Template,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsCategory {
    Interface,
    Playback,
    Sources,
    Accounts,
    Integration,
    Download,
    Data,
}

impl SettingsCategory {
    fn next(self) -> Self {
        match self {
            Self::Interface => Self::Playback,
            Self::Playback => Self::Sources,
            Self::Sources => Self::Accounts,
            Self::Accounts => Self::Integration,
            Self::Integration => Self::Download,
            Self::Download => Self::Data,
            Self::Data => Self::Interface,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Interface => Self::Data,
            Self::Playback => Self::Interface,
            Self::Sources => Self::Playback,
            Self::Accounts => Self::Sources,
            Self::Integration => Self::Accounts,
            Self::Download => Self::Integration,
            Self::Data => Self::Download,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Interface => "界面",
            Self::Playback => "播放",
            Self::Sources => "音源与歌词",
            Self::Accounts => "账号与扫码",
            Self::Integration => "通知与集成",
            Self::Download => "下载",
            Self::Data => "数据与本地库",
        }
    }
}

pub struct SettingsPage {
    /// 本终端能用哪些封面协议（启动时探测一次，`Shift+P` 循环据它跳过画不出的项）。
    cover_capabilities: crate::cover::CoverCapabilities,
    /// 输入中的 JS 源 URL 或本地路径
    pub input_url: String,
    /// 是否在输入模式
    pub input_mode: bool,
    /// 导入状态消息
    pub status_msg: Option<String>,
    /// 状态消息的设置时间；渲染 3 秒后自动消失，不再永久压住面板最后一行。
    pub status_msg_at: Option<Instant>,
    /// JS 源列表的选中索引
    pub selected_source: usize,
    /// 本地音乐路径输入
    pub local_path_input: String,
    /// 本地音乐路径输入模式
    pub local_path_mode: bool,
    /// 本地路径列表选中索引
    pub selected_local_path: usize,
    /// 代理地址输入
    pub proxy_input: String,
    /// 代理地址输入模式
    pub proxy_input_mode: bool,
    /// 音频输出设备输入模式
    pub audio_device_input: String,
    pub audio_device_input_mode: bool,
    /// 外部歌单文件输入模式。
    pub playlist_import_input: String,
    pub playlist_import_mode: bool,
    /// 下载目录 / 文件名模板输入模式。
    download_input: String,
    download_input_target: Option<DownloadInputTarget>,
    /// 扫码登录入口当前指向的音源
    qr_login_source_index: usize,
    /// 状态栏字段列表的选中索引
    pub selected_status_item: usize,
    /// 状态栏字段列表的滚动位置
    status_item_scroll: usize,
    /// 状态栏拖拽当前所在的字段行，避免同一行重复触发重排。
    status_drag_target: Option<usize>,
    /// 分类内容里内嵌管理列表的高度份额（占内容区高度，可持久化到 Config）。
    embedded_ratio: f32,
    /// 内嵌列表是否按"用户拖过的比例"给高度（而不是"够用就好"）。
    ///
    /// 首次拖动横向分界线的那一刻就置位，配置里存过 `settings.embedded` 也置位。
    /// 少了它，列表只有两三条时高度会被"够用就好"顶死，拖拽把手纹丝不动。
    embedded_ratio_fixed: bool,
    /// 宽屏分类栏宽度（列，含左右边框；可持久化到 Config）。
    categories_width: u16,
    /// 分类栏 / 内嵌列表两条分界线的拖拽会话。
    splitter: Splitter<SettingsResizeTarget>,
    /// 鼠标悬停中的分界线（只影响高亮）。
    hover_divider: Option<SettingsResizeTarget>,
    /// 最近一次渲染出来的分栏几何：悬停与拖拽抓取都只认它，
    /// 因此"画出来的线"与"抓得到的线"是同一份矩形。
    last_panes: SettingsPanes,
    /// 当前聚焦区域
    focus: SettingsFocus,
    category: SettingsCategory,
    /// 窄屏单栏视图当前层级（宽屏不使用）。
    narrow_pane: NarrowPane,
    /// 设置行的键盘光标（当前分类内）。
    row_cursor: Option<SettingsRowId>,
    /// 鼠标悬停的设置行（只影响高亮，不改键盘光标）。
    hover_row: Option<SettingsRowId>,
    /// 全部设置行的元数据（每次渲染重建；键盘光标按分类过滤它）。
    row_metas: Vec<SettingsRowMeta>,
    /// 可见设置行的矩形账本（渲染时写入，鼠标命中只查它）。
    row_hits: Vec<SettingsRowHit>,
    /// 分类栏每行的矩形账本（渲染时写入，鼠标命中只查它）。
    category_hits: Vec<(Rect, SettingsCategory)>,
    /// 当前分类内嵌管理列表的命中账本（渲染时写入，鼠标命中只查它）。
    ///
    /// 一次只有一个内嵌列表可见，因此四个列表共用这一份账本。
    embedded_hits: EmbeddedHits,
    /// 当前内嵌管理列表的整块矩形（渲染时写入）。
    ///
    /// 点/滚面板里的空白处（标题、命令行以外的行）也把焦点交给这个列表。
    embedded_area: Option<Rect>,
    /// 最近一次渲染收到的页面矩形：窄屏判定与菜单边界都取它。
    last_area: Rect,
    /// 设置页自己持有的枚举取值菜单（不占用主循环的 `song_menu` 槽位）。
    menu: Option<SongContextMenu>,
    /// 「网易云远程歌单」独立窗口；`Some` 时模态独占按键与鼠标。
    remote_window: Option<RemoteCollectionsWindow>,
    /// 打开窗口时的远程缓存 generation：刷新完成后要就地更新窗口内容。
    remote_window_generation: u64,
    /// 删除 JS 音源的武装时刻：首次按 d 只武装，窗口内再按一次才删除
    delete_source_armed: Option<Instant>,
    /// 删除本地目录的武装时刻，机制同上
    delete_local_path_armed: Option<Instant>,
    /// 「清除封面缓存」行的值文案（进入数据分类 / 清理后刷新一次）。
    cover_cache_label: String,
    /// 「清除网易云歌单缓存」行的值文案。
    remote_cache_label: String,
}

/// 构造设置页的全部设置行。
///
/// **每一行在这里显式声明自己的分类与激活计划**（禁止用下标区间或键位字符串
/// 推断），键盘 `Enter`/`Space` 与鼠标点击读的是同一份元数据。
///
/// 新增设置行只需在这里加一行；分类栏、键盘光标、鼠标命中、面板高度
/// 全部从行元数据推导，不会再出现"加了行但点错位置"。
/// 「界面主题」这一行的**唯一构造点**（生产代码与测试共用同一份）。
///
/// 值里带 `›`：让用户一眼看出"这一行能展开选主题"，而不是一个只读的当前值。
/// 激活计划是 `RowPlan::Menu(Theme)`，键位列显示 `Enter`，
/// 因此鼠标点击、`Enter`、`Space` 三条路都会走到 `SettingsEnumPicker::Theme`。
///
/// 位置刻意放在「界面」分类的**第一行**：上一版它埋在"音源与歌词"里，
/// 用户要换主题根本找不到入口。
fn theme_picker_row(
    rows: &mut SettingsRows,
    config: &lx_core::model::config::Config,
    palette: RowPalette,
) -> SettingsRowId {
    rows.value(
        SettingsCategory::Interface,
        SettingsRowKind::Enum,
        "界面主题",
        RowPlan::Menu(SettingsEnumPicker::Theme),
        &format!("{} ›", crate::theme::skin_label(&config.theme.name)),
        palette,
    )
}

/// `build_settings_rows` 真正读到的运行时值。
///
/// 刻意不塞整个 `AppContext`：行表构造只依赖这几个值，于是测试可以在没有
/// 播放器 / 音源管理器 / 下载管理器的环境里验证**真实行集**。生产路径与测试
/// 走的是同一个 `build_settings_rows`，不存在第二份"测试专用行表"。
#[derive(Debug, Clone, PartialEq, Eq)]
struct RowInputs {
    /// 当前播放模式（它是播放列表的运行时状态，配置里的只是持久化副本）。
    play_mode: PlayMode,
    /// 已设置的 A-B 循环：`(起点文案, 终点文案)`；未设置时 `None`。
    ab_loop: Option<(String, String)>,
    /// 「扫码登录」行的值：各在线音源的登录状态摘要。
    qr_login_label: String,
    /// 当前下载目录（显示文案）。
    download_dir: String,
    /// 封面缓存行显示文案；生产路径由设置页按需刷新，避免每帧扫盘。
    cover_cache_label: String,
    /// 网易云远程缓存行显示文案。
    remote_cache_label: String,
}

impl RowInputs {
    /// 生产入口：只从 `AppContext` 抽取行表需要的那几个值。
    fn from_context(ctx: &AppContext) -> Self {
        let ab_loop = ctx
            .player
            .ab_loop()
            .map(|points| (format_duration(points.start), format_duration(points.end)));
        let qr_login_label = {
            let login_sources: Vec<SourceId> = SourceId::all_online()
                .iter()
                .copied()
                .filter(|source| ctx.source_manager.capabilities(*source).qr_login)
                .collect();
            if login_sources.is_empty() {
                "无可用音源".to_string()
            } else {
                login_sources
                    .iter()
                    .map(|source| {
                        let status = if ctx.source_manager.is_logged_in(*source) {
                            "✓"
                        } else {
                            "○"
                        };
                        format!("{}{}", status, source.display_name())
                    })
                    .collect::<Vec<_>>()
                    .join("  ")
            }
        };
        Self {
            play_mode: ctx.playlist.mode(),
            ab_loop,
            qr_login_label,
            download_dir: ctx.downloads.download_dir().display().to_string(),
            cover_cache_label: "计算中…".to_string(),
            remote_cache_label: "计算中…".to_string(),
        }
    }
}

fn build_settings_rows(
    rows: &mut SettingsRows,
    config: &lx_core::model::config::Config,
    inputs: &RowInputs,
    palette: RowPalette,
    cover_capabilities: crate::cover::CoverCapabilities,
) {
    use SettingsCategory as C;
    use SettingsRowDirectAction as D;
    use SettingsRowKind as K;

    // 每一行都显式声明自己的**激活计划**：
    // - `RowPlan::Menu(...)`：Enter / Space / 点击打开取值菜单（候选值可枚举）；
    // - `RowPlan::Direct(...)`：Enter / Space / 点击直接执行（开关、循环档位、
    //   输入浮层、一次性动作），执行时复用既有 `AppAction` / 写配置分支。
    //
    // 这里**没有**"按键回灌"这一档：行内快捷键已全部删除，键位列一律显示
    // `Enter`（唯一例外是刻意保留的页面级组合键，见 `COVER_PROTOCOL_ROW_KEY`）。

    // ── 界面 ──
    // 主题入口放第一行：这是"我要换主题"时第一个该看到的东西。
    theme_picker_row(rows, config, palette);
    rows.toggle(
        C::Interface,
        "鼠标控制",
        RowPlan::Direct(D::ToggleMouse),
        config.ui.enable_mouse,
        palette,
    );
    rows.toggle(
        C::Interface,
        "聚合搜索",
        RowPlan::Direct(D::ToggleAggregateSearch),
        config.ui.aggregate_search,
        palette,
    );
    rows.toggle(
        C::Interface,
        "循环导航",
        RowPlan::Direct(D::ToggleWrapNavigation),
        config.ui.wrap_navigation,
        palette,
    );
    rows.toggle(
        C::Interface,
        "封面显示",
        RowPlan::Direct(D::ToggleShowCover),
        config.ui.show_cover,
        palette,
    );
    rows.value(
        C::Interface,
        K::Enum,
        "封面主色跟随",
        RowPlan::Direct(D::CycleAccentFollowCover),
        config.ui.accent_follow_cover.label(),
        palette,
    );
    rows.toggle(
        C::Interface,
        "保留播放状态",
        RowPlan::Direct(D::ToggleRememberPlaybackState),
        config.player.remember_playback_state,
        palette,
    );
    rows.value(
        C::Interface,
        K::Enum,
        "网络超时",
        RowPlan::Direct(D::CycleNetworkTimeout),
        &format!("{} 秒", config.network.timeout),
        palette,
    );
    // 「封面协议」是唯一保留页面级组合键的设置行：裸 `P` 被「账号与扫码」占用，
    // 真正生效的是 `Shift+P`，因此键位列必须显示它（见 `is_cover_protocol_key`）。
    rows.value_with_key_hint(
        C::Interface,
        K::Enum,
        "封面协议",
        RowPlan::Direct(D::CycleCoverProtocol),
        COVER_PROTOCOL_ROW_KEY,
        &cover_protocol_display(&config.ui.cover_protocol, cover_capabilities),
        palette,
    );
    rows.value(
        C::Interface,
        K::Enum,
        "最大 FPS",
        RowPlan::Direct(D::CycleMaxFps),
        &config.ui.max_fps.to_string(),
        palette,
    );
    rows.value(
        C::Interface,
        K::Enum,
        "翻页步长",
        RowPlan::Direct(D::CyclePageStep),
        &format!("{} 行", config.ui.page_step),
        palette,
    );
    rows.toggle(
        C::Interface,
        "切歌通知",
        RowPlan::Direct(D::ToggleTrackChangeNotification),
        config.notification.track_change,
        palette,
    );
    // 状态栏字段是"多个独立开关"，菜单里每项各自打 ✓ / ○，选中即切换。
    rows.value(
        C::Interface,
        K::Enum,
        "状态栏字段",
        RowPlan::Menu(SettingsEnumPicker::StatusBarItems),
        &format!(
            "{}/{} 项",
            config.ui.status_bar_items.len(),
            StatusBarItem::ALL.len()
        ),
        palette,
    );

    // ── 播放 ──
    rows.value(
        C::Playback,
        K::Enum,
        "播放音质",
        RowPlan::Menu(SettingsEnumPicker::Quality),
        config.player.quality.label(),
        palette,
    );
    rows.value(
        C::Playback,
        K::Enum,
        "播放速度",
        RowPlan::Direct(D::CyclePlaybackSpeed),
        &format!("{:.2}x", config.player.playback_speed),
        palette,
    );
    rows.value(
        C::Playback,
        K::Input,
        "音频设备",
        RowPlan::Direct(D::EditAudioDevice),
        &config.player.audio_device,
        palette,
    );
    rows.value(
        C::Playback,
        K::Enum,
        "ReplayGain",
        RowPlan::Direct(D::CycleReplayGainMode),
        &config.player.replaygain_mode,
        palette,
    );
    rows.value(
        C::Playback,
        K::Enum,
        "RG 预放大",
        RowPlan::Direct(D::CycleReplayGainPreamp),
        &format!("{:+.1} dB", config.player.replaygain_preamp),
        palette,
    );
    rows.value(
        C::Playback,
        K::Enum,
        "声道模式",
        RowPlan::Direct(D::CycleChannelMode),
        &config.player.channel_mode,
        palette,
    );
    rows.value(
        C::Playback,
        K::Enum,
        "左右平衡",
        RowPlan::Direct(D::CycleBalance),
        &format!("{:+.2}", config.player.balance),
        palette,
    );
    rows.toggle(
        C::Playback,
        "ReplayGain 削波保护",
        RowPlan::Direct(D::ToggleReplayGainClip),
        config.player.replaygain_clip,
        palette,
    );
    rows.value(
        C::Playback,
        K::Enum,
        "淡入时长",
        RowPlan::Direct(D::CycleFadeInDuration),
        &fade_label(config.player.fade_in_ms),
        palette,
    );
    rows.value(
        C::Playback,
        K::Enum,
        "淡出时长",
        RowPlan::Direct(D::CycleFadeOutDuration),
        &fade_label(config.player.fade_out_ms),
        palette,
    );
    rows.value(
        C::Playback,
        K::Enum,
        "均衡器",
        RowPlan::Menu(SettingsEnumPicker::Equalizer),
        crate::context::equalizer_label(&config.player.equalizer_bands),
        palette,
    );
    rows.value(
        C::Playback,
        K::Action,
        "淡入当前歌曲",
        RowPlan::Direct(D::RunFadeIn),
        "执行",
        palette,
    );
    rows.value(
        C::Playback,
        K::Action,
        "淡出当前歌曲",
        RowPlan::Direct(D::RunFadeOut),
        "执行",
        palette,
    );
    let ab_loop_set = inputs.ab_loop.is_some();
    let ab_start_label = inputs
        .ab_loop
        .as_ref()
        .map(|(start, _)| start.clone())
        .unwrap_or_else(|| "未设置".to_string());
    let ab_end_label = inputs
        .ab_loop
        .as_ref()
        .map(|(_, end)| end.clone())
        .unwrap_or_else(|| "未设置".to_string());
    rows.value(
        C::Playback,
        K::Action,
        "A-B 循环起点",
        RowPlan::Direct(D::SetAbLoopStart),
        &ab_start_label,
        palette,
    );
    rows.value(
        C::Playback,
        K::Action,
        "A-B 循环终点",
        RowPlan::Direct(D::SetAbLoopEnd),
        &ab_end_label,
        palette,
    );
    rows.value(
        C::Playback,
        K::Action,
        "清除 A-B",
        RowPlan::Direct(D::ClearAbLoop),
        if ab_loop_set { "执行" } else { "未设置" },
        palette,
    );
    rows.value(
        C::Playback,
        K::Enum,
        "播放模式",
        RowPlan::Menu(SettingsEnumPicker::PlayMode),
        inputs.play_mode.label(),
        palette,
    );
    rows.value(
        C::Playback,
        K::Enum,
        "历史上限",
        RowPlan::Direct(D::CycleHistoryLimit),
        &config.player.history_limit.to_string(),
        palette,
    );

    // ── 音源与歌词 ──
    rows.value(
        C::Sources,
        K::Enum,
        "默认音源",
        RowPlan::Menu(SettingsEnumPicker::DefaultSource),
        config.source.default.as_str(),
        palette,
    );
    rows.toggle(
        C::Sources,
        "自动换源",
        RowPlan::Direct(D::ToggleAutoSource),
        config.source.auto_toggle,
        palette,
    );
    rows.value(
        C::Sources,
        K::Enum,
        "解析策略",
        RowPlan::Menu(SettingsEnumPicker::SourcePolicy),
        &source_policy_label(config.source.policy, config.source.policy_platform),
        palette,
    );
    // 每个在线音源的启用开关：菜单里每项各自打 ✓，选中即切换。
    rows.value(
        C::Sources,
        K::Enum,
        "音源开关",
        RowPlan::Menu(SettingsEnumPicker::EnabledSources),
        &format!(
            "{}/{} 开启",
            config.source.enabled.len(),
            SourceId::all_online().len()
        ),
        palette,
    );
    rows.toggle(
        C::Sources,
        "歌词翻译",
        RowPlan::Direct(D::ToggleLyricTranslation),
        config.lyric.show_translation,
        palette,
    );
    rows.toggle(
        C::Sources,
        "逐字歌词",
        RowPlan::Direct(D::ToggleLyricYrc),
        config.lyric.show_yrc,
        palette,
    );
    rows.value(
        C::Sources,
        K::Enum,
        "歌词偏移",
        RowPlan::Menu(SettingsEnumPicker::LyricOffset),
        &format!("{:+} ms", config.lyric.offset),
        palette,
    );
    let proxy_label = if config.network.proxy_url.is_empty() {
        "未设置".to_string()
    } else {
        shorten_source(&config.network.proxy_url, 18)
    };
    rows.value(
        C::Sources,
        K::Input,
        "网络代理",
        RowPlan::Direct(D::EditProxy),
        &proxy_label,
        palette,
    );
    // 重新加载 JS 音源此前只有状态栏菜单入口：现在这一行就是它的入口。
    rows.value(
        C::Sources,
        K::Action,
        "重新加载 JS 音源",
        RowPlan::Direct(D::ReloadJsSources),
        "执行",
        palette,
    );
    rows.value(
        C::Sources,
        K::Action,
        "音源体检",
        RowPlan::Direct(D::CheckSourceHealth),
        "检测",
        palette,
    );

    // ── 账号与扫码 ──
    rows.value(
        C::Accounts,
        K::Action,
        "扫码登录",
        RowPlan::Direct(D::QrLoginSelected),
        &inputs.qr_login_label,
        palette,
    );
    rows.value(
        C::Accounts,
        K::Action,
        "刷新远程歌单",
        RowPlan::Direct(D::SyncNetease),
        "双向增量",
        palette,
    );
    rows.value(
        C::Accounts,
        K::Action,
        "QQ 音乐同步",
        RowPlan::Direct(D::SyncQq),
        "双向增量",
        palette,
    );
    // 「导入外部歌单」归数据分类：它是文件导入动作，和账号无关。

    // ── 通知与集成 ──
    rows.value(
        C::Integration,
        K::Enum,
        "滚动步长",
        RowPlan::Direct(D::CycleScrollAmount),
        &config.ui.scroll_amount.to_string(),
        palette,
    );
    rows.toggle(
        C::Integration,
        "MPRIS",
        RowPlan::Direct(D::ToggleMpris),
        config.integration.mpris,
        palette,
    );
    // TUI 通知原来是"一个键管两个字段"（`o` 开关 / `O` 秒数）。删掉行内快捷键后
    // 拆成两行：两件事各自 Enter 可达，语义反而更清楚。
    rows.toggle(
        C::Integration,
        "TUI 通知",
        RowPlan::Direct(D::ToggleInAppNotification),
        config.notification.in_app,
        palette,
    );
    rows.value(
        C::Integration,
        K::Enum,
        "TUI 通知时长",
        RowPlan::Direct(D::CycleInAppTimeout),
        &format!("{} 秒", config.notification.in_app_timeout.clamp(1, 60)),
        palette,
    );
    rows.toggle(
        C::Integration,
        "桌面通知",
        RowPlan::Direct(D::ToggleDesktopNotification),
        config.notification.enable,
        palette,
    );
    rows.toggle(
        C::Integration,
        "通知封面",
        RowPlan::Direct(D::ToggleNotificationAlbumCover),
        config.notification.album_cover,
        palette,
    );

    // ── 下载 ──
    rows.value(
        C::Download,
        K::Input,
        "下载目录",
        RowPlan::Direct(D::EditDownloadDir),
        &shorten_source(&inputs.download_dir, 22),
        palette,
    );
    rows.value(
        C::Download,
        K::Enum,
        "下载音质",
        RowPlan::Direct(D::CycleDownloadQuality),
        &config
            .download
            .quality
            .map(|quality| quality.label().to_string())
            .unwrap_or_else(|| format!("跟随播放 ({})", config.player.quality.label())),
        palette,
    );
    rows.value(
        C::Download,
        K::Input,
        "文件名模板",
        RowPlan::Direct(D::EditFilenameTemplate),
        &shorten_source(&config.download.filename_template, 22),
        palette,
    );
    rows.toggle(
        C::Download,
        "多线程分片",
        RowPlan::Direct(D::ToggleMultipart),
        config.download.multipart,
        palette,
    );
    rows.value(
        C::Download,
        K::Enum,
        "分片阈值",
        RowPlan::Direct(D::CycleMultipartMinSize),
        &format!("{} MB", config.download.multipart_min_size_mb),
        palette,
    );
    rows.value(
        C::Download,
        K::Enum,
        "分片并发",
        RowPlan::Direct(D::CycleDownloadConcurrency),
        &config.download.concurrency.to_string(),
        palette,
    );
    rows.value(
        C::Download,
        K::Enum,
        "同时下载",
        RowPlan::Direct(D::CycleConcurrentSongs),
        &format!("{} 首", config.download.concurrent_songs),
        palette,
    );
    rows.value(
        C::Download,
        K::Enum,
        "失败重试",
        RowPlan::Direct(D::CycleMaxRetries),
        &format!("{} 次", config.download.max_retries),
        palette,
    );
    rows.toggle(
        C::Download,
        "校验文件大小",
        RowPlan::Direct(D::ToggleVerifySize),
        config.download.verify_size,
        palette,
    );
    rows.toggle(
        C::Download,
        "跳过已下载",
        RowPlan::Direct(D::ToggleSkipExisting),
        config.download.skip_existing,
        palette,
    );
    rows.toggle(
        C::Download,
        "写入标签",
        RowPlan::Direct(D::ToggleWriteTags),
        config.download.write_tags,
        palette,
    );
    rows.toggle(
        C::Download,
        "嵌入封面",
        RowPlan::Direct(D::ToggleEmbedCover),
        config.download.embed_cover,
        palette,
    );
    rows.toggle(
        C::Download,
        "保存歌词",
        RowPlan::Direct(D::ToggleSaveLyric),
        config.download.save_lyric,
        palette,
    );

    // ── 数据与本地库 ──
    let scan_depth_label = if config.local_music.max_depth == 0 {
        "不限".to_string()
    } else {
        config.local_music.max_depth.to_string()
    };
    rows.value(
        C::Data,
        K::Enum,
        "扫描深度",
        RowPlan::Direct(D::CycleScanDepth),
        &scan_depth_label,
        palette,
    );
    rows.value(
        C::Data,
        K::Action,
        "导出数据",
        RowPlan::Direct(D::ExportData),
        "voicefox-export.json",
        palette,
    );
    rows.value(
        C::Data,
        K::Action,
        "导入数据",
        RowPlan::Direct(D::ImportData),
        "voicefox-export.json",
        palette,
    );
    rows.value(
        C::Data,
        K::Action,
        "导入外部歌单",
        RowPlan::Direct(D::ImportExternalPlaylist),
        "M3U/JSON",
        palette,
    );
    rows.value(
        C::Data,
        K::Action,
        "清除封面缓存",
        RowPlan::Direct(D::ClearCoverCache),
        &inputs.cover_cache_label,
        palette,
    );
    rows.value(
        C::Data,
        K::Action,
        "清除网易云歌单缓存",
        RowPlan::Direct(D::ClearRemoteCache),
        &inputs.remote_cache_label,
        palette,
    );
}

impl SettingsPage {
    /// 设置状态消息并记录时间；渲染侧超过 [`STATUS_MSG_TIMEOUT`] 自动隐藏。
    pub fn set_status(&mut self, message: impl Into<String>) {
        self.status_msg = Some(message.into());
        self.status_msg_at = Some(Instant::now());
    }

    /// 清除状态消息。
    pub fn clear_status(&mut self) {
        self.status_msg = None;
        self.status_msg_at = None;
    }

    /// 手动保存配置后设置「已保存 / 失败」状态消息（统一走 [`Self::set_status`] 计时）。
    pub fn set_saved_status(&mut self, result: anyhow::Result<()>) {
        self.set_status(match result {
            Ok(()) => "设置已保存".to_string(),
            Err(error) => format!("保存设置失败: {error}"),
        });
    }
    /// 检查是否有任何输入模式激活（JS 源输入或本地路径输入）
    pub fn any_input_active(&self) -> bool {
        self.input_mode
            || self.local_path_mode
            || self.proxy_input_mode
            || self.audio_device_input_mode
            || self.playlist_import_mode
            || self.download_input_target.is_some()
    }

    /// 判断按键是否由设置页独占。
    ///
    /// 精简之后设置页只吃三类键：分类 / 光标导航键、`Enter` / `Space` 激活键、
    /// 以及少数几个刻意保留的页面级键（`p` / `P` / `Shift+P` 与内嵌列表的
    /// 命令行操作键）。**清单之外一键不吃**，因此不会出现"按键没反应但也不传
    /// 下去"的死键（见 `owns_char_key`）。
    /// 设置页是否吃这个字符键（`consumes_key` 与 `handle_input` 共用同一条口径）。
    ///
    /// - `p` / `P`：主题循环 / 账号面板（`Shift+P` 是封面协议，都在 `p`/`P` 上）；
    /// - 内嵌管理列表的命令行操作键：只有**对应列表拿到焦点**时才吃，
    ///   否则会把它们从全局快捷键那里抢走却什么也不做。
    fn owns_char_key(&self, character: char) -> bool {
        if !SETTINGS_PAGE_CHAR_KEYS.contains(&character) {
            return false;
        }
        match character {
            'p' | 'P' => true,
            // `v`：查看远程歌单窗口（账号分类的只读浏览入口）
            'v' | 'V' => true,
            _ => embedded_command(self.focus, character).is_some(),
        }
    }

    pub fn consumes_key(&self, key: &KeyEvent, resolver: &KeybindingResolver) -> bool {
        // 远程歌单窗口 / 取值菜单都是模态的：打开期间全部按键先给它们，
        // 否则 `q`（全局退出）会在窗口还开着的时候直接退出程序。
        if self.remote_window.is_some() || self.menu.is_some() {
            return true;
        }
        // Bare number keys are reserved for navigation (1-8 select sidebar
        // tabs). Even a stale or intentionally custom settings binding must
        // not make tab switching stop while the settings page is open.
        if key.modifiers == KeyModifiers::NONE && matches!(key.code, KeyCode::Char('0'..='9')) {
            return false;
        }
        if resolver
            .resolve_page("settings", key)
            .is_some_and(settings_action_is_page_owned)
        {
            return true;
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return false;
        }
        if self.focus == SettingsFocus::StatusBar
            && (matches!(
                (key.modifiers, key.code),
                (KeyModifiers::NONE, KeyCode::Enter | KeyCode::Char(' '))
            ) || matches!(
                (key.modifiers, key.code),
                (KeyModifiers::SHIFT, KeyCode::Left | KeyCode::Right)
            ))
        {
            return true;
        }
        // 设置项面板同样把 Enter / Space 当作自己的激活键：
        // 否则全局 Space（播放/暂停）会抢在行激活之前触发。
        if self.focus == SettingsFocus::Options
            && matches!(
                (key.modifiers, key.code),
                (KeyModifiers::NONE, KeyCode::Enter | KeyCode::Char(' '))
            )
        {
            return true;
        }
        match key.code {
            KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right => true,
            KeyCode::Char(character) => self.owns_char_key(character),
            _ => false,
        }
    }

    pub fn new() -> Self {
        Self {
            cover_capabilities: crate::cover::CoverCapabilities::from_detected(
                None,
                crate::cover::ProtocolType::Halfblocks,
            ),
            input_url: String::new(),
            input_mode: false,
            status_msg: None,
            status_msg_at: None,
            selected_source: 0,
            local_path_input: String::new(),
            local_path_mode: false,
            selected_local_path: 0,
            proxy_input: String::new(),
            proxy_input_mode: false,
            audio_device_input: String::new(),
            audio_device_input_mode: false,
            playlist_import_input: String::new(),
            playlist_import_mode: false,
            download_input: String::new(),
            download_input_target: None,
            qr_login_source_index: 0,
            selected_status_item: 0,
            status_item_scroll: 0,
            status_drag_target: None,
            embedded_ratio: EMBEDDED_RATIO_DEFAULT,
            embedded_ratio_fixed: false,
            categories_width: CATEGORY_SIDEBAR_WIDTH,
            splitter: Splitter::default(),
            hover_divider: None,
            last_panes: SettingsPanes::empty(),
            focus: SettingsFocus::Options,
            category: SettingsCategory::Interface,
            narrow_pane: NarrowPane::Categories,
            row_cursor: None,
            hover_row: None,
            row_metas: Vec::new(),
            row_hits: Vec::new(),
            category_hits: Vec::new(),
            embedded_hits: EmbeddedHits::default(),
            embedded_area: None,
            last_area: Rect::default(),
            menu: None,
            remote_window: None,
            remote_window_generation: 0,
            delete_source_armed: None,
            delete_local_path_armed: None,
            cover_cache_label: "计算中…".to_string(),
            remote_cache_label: "计算中…".to_string(),
        }
    }

    /// 注入启动时探测到的封面协议能力（`Ctrl+G` 之外的封面入口都据它判断）。
    pub fn set_cover_capabilities(&mut self, capabilities: crate::cover::CoverCapabilities) {
        self.cover_capabilities = capabilities;
    }

    /// 扫码登录入口列表。
    ///
    /// 支持自带扫码渠道的音源各一条；声明了 `wechat_login` 的音源再补一条微信
    /// 入口（目前只有 QQ 音乐，issue #43 的诉求）。条目顺序就是界面行顺序。
    fn qr_login_sources(&self, ctx: &AppContext) -> Vec<(SourceId, QrLoginKind)> {
        let mut entries = Vec::new();
        for source in SourceId::all_online().iter().copied() {
            let capabilities = ctx.source_manager.capabilities(source);
            if capabilities.qr_login {
                entries.push((source, QrLoginKind::Standard));
            }
            if capabilities.wechat_login {
                entries.push((source, QrLoginKind::WeChat));
            }
        }
        entries
    }

    pub fn handle_input(
        &mut self,
        key: KeyEvent,
        ctx: &AppContext,
        resolver: &KeybindingResolver,
    ) -> AppAction {
        // 「远程歌单」窗口是模态的：打开期间它独占按键（Esc 关窗、Enter 跳转）。
        if self.remote_window.is_some() {
            let outcome = self
                .remote_window
                .as_mut()
                .map(|window| window.handle_key(key));
            return match outcome {
                Some(RemoteCollectionsOutcome::Open(id)) => {
                    self.remote_window = None;
                    AppAction::OpenAccountPlaylist(id)
                }
                Some(RemoteCollectionsOutcome::Closed) => {
                    self.remote_window = None;
                    AppAction::None
                }
                _ => AppAction::None,
            };
        }
        // 枚举取值菜单是模态的：打开期间它独占按键。
        if let Some(action) = self.handle_menu_key(&key, ctx, resolver) {
            return action;
        }
        if self.proxy_input_mode {
            return self.handle_proxy_input(key, ctx);
        }
        if self.audio_device_input_mode {
            return self.handle_audio_device_input(key, ctx);
        }
        if self.playlist_import_mode {
            return self.handle_playlist_import_input(key, ctx);
        }
        if self.download_input_target.is_some() {
            return self.handle_download_input(key, ctx);
        }
        if self.local_path_mode {
            return self.handle_local_path_input(key, ctx);
        }
        if self.input_mode {
            match (key.modifiers, key.code) {
                (KeyModifiers::NONE, KeyCode::Esc) => {
                    self.input_mode = false;
                    self.input_url.clear();
                    return AppAction::None;
                }
                (KeyModifiers::NONE, KeyCode::Enter) => {
                    if !self.input_url.trim().is_empty() {
                        let url = self.input_url.trim().to_string();
                        self.input_mode = false;
                        self.input_url.clear();
                        self.set_status("正在添加音源...".to_string());
                        return AppAction::ImportSource(url);
                    }
                    return AppAction::None;
                }
                (modifiers, KeyCode::Char(c))
                    if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.input_url.push(c);
                }
                (KeyModifiers::NONE, KeyCode::Backspace) => {
                    self.input_url.pop();
                }
                _ => {}
            }
        } else {
            // 设置页**只保留分类 / 导航 / 激活键**：设置项本身一律靠 `Enter`
            // （`Space` 对开关与取值行等价）或鼠标点击激活，不再有逐行字母快捷键。
            // 这里剩下的每一个键都有明确归属 —— 不会出现"按下去没反应、
            // 也不传给全局"的死键。

            // `P`（裸 `'P'`）→ 进入「账号与扫码」分类；已经在该分类里时改为
            // 对当前选中音源执行扫码登录 / 退出登录。
            //
            // 小写 `p` 必须留给下面的「界面主题」循环分支。
            if is_accounts_panel_key(&key) {
                if self.category == SettingsCategory::Accounts {
                    let login_sources = self.qr_login_sources(ctx);
                    if login_sources.is_empty() {
                        self.set_status("当前没有支持扫码登录的音源".to_string());
                    } else {
                        self.qr_login_source_index %= login_sources.len();
                        let (source, kind) = login_sources[self.qr_login_source_index];
                        return self.qr_login_toggle(source, kind, ctx);
                    }
                } else {
                    self.set_category(SettingsCategory::Accounts);
                }
                return AppAction::None;
            }

            // 「封面协议」的循环键：`Shift+P`（裸 `P` 已被上面的账号面板入口占用，
            // 两个键的分工见 `is_cover_protocol_key` / `is_accounts_panel_key`）。
            if is_cover_protocol_key(&key) {
                return self.apply_direct_action(SettingsRowDirectAction::CycleCoverProtocol, ctx);
            }

            // 界面主题（p = palette）：在 `voicefox`（默认）与主题库的具名主题之间
            // 循环。颜色每次渲染都从配置解析，所以按一下立刻整界面换肤。
            if key.modifiers == KeyModifiers::NONE && key.code == KeyCode::Char('p') {
                self.update_config(ctx, |config| {
                    config.theme.name = crate::theme::next_skin_name(&config.theme.name);
                });
                return AppAction::None;
            }

            // `v`：查看网易云远程歌单窗口（账号分类下的只读浏览入口）。
            // 缓存还没拉过时也打开，把"要先去刷新"的引导写在窗口里。
            if key.modifiers == KeyModifiers::NONE
                && matches!(key.code, KeyCode::Char('v') | KeyCode::Char('V'))
            {
                self.open_remote_collections_window();
                return AppAction::None;
            }

            if key.modifiers == KeyModifiers::NONE && key.code == KeyCode::Left {
                self.set_category(self.category.previous());
                return AppAction::None;
            }
            if key.modifiers == KeyModifiers::NONE && key.code == KeyCode::Right {
                self.set_category(self.category.next());
                return AppAction::None;
            }

            // 当前列表区域的按键优先处理（内嵌管理列表自己的 ↑/↓ 与命令行操作键）。
            if self.focus == SettingsFocus::LocalPaths
                && let Some(action) = self.handle_local_keys(key, ctx, resolver)
            {
                return action;
            }
            if self.focus == SettingsFocus::JsSources
                && let Some(action) = self.handle_js_source_keys(key, ctx, resolver)
            {
                return action;
            }
            if self.focus == SettingsFocus::StatusBar
                && let Some(action) = self.handle_status_bar_keys(key, ctx, resolver)
            {
                return action;
            }

            // 扫码列表聚焦时 ↑/↓ 归它（第一条继续 ↑ 把焦点交还设置行光标），
            // `Enter` 对选中音源执行登录 / 退出登录。
            if self.focus == SettingsFocus::QrLogin {
                let login_sources = self.qr_login_sources(ctx);
                if !login_sources.is_empty() {
                    match (key.modifiers, key.code) {
                        (KeyModifiers::NONE, KeyCode::Up) => {
                            if self.qr_login_source_index == 0 {
                                self.leave_embedded_list();
                            } else {
                                self.qr_login_source_index -= 1;
                            }
                            return AppAction::None;
                        }
                        (KeyModifiers::NONE, KeyCode::Down) => {
                            self.qr_login_source_index =
                                (self.qr_login_source_index + 1) % login_sources.len();
                            return AppAction::None;
                        }
                        (KeyModifiers::NONE, KeyCode::Enter) => {
                            let (source, kind) =
                                login_sources[self.qr_login_source_index % login_sources.len()];
                            return self.qr_login_toggle(source, kind, ctx);
                        }
                        _ => {}
                    }
                }
            }

            // 设置项面板的按键。放在所有"子列表"处理之后：JS 音源 / 本地目录 /
            // 状态栏 / 扫码聚焦时 ↑/↓ 与 Enter 已经由它们消费，设置行光标让位
            // （见 `list_owns_direction_keys`）。
            if let Some(action) = self.handle_settings_row_keys(&key, ctx) {
                return action;
            }

            // `Esc` 取消删除类操作（音源 / 本地目录）的武装状态。
            // 窄屏的"返回分类列表"由 `handle_settings_row_keys` 处理。
            if matches!(
                (key.modifiers, key.code),
                (KeyModifiers::NONE, KeyCode::Esc)
            ) {
                self.delete_source_armed = None;
                self.delete_local_path_armed = None;
            }
        }
        AppAction::None
    }

    // ── 设置行光标 / 行激活 ────────────────────────────────────────────────

    /// 宽屏判定：≥ `CATEGORY_SIDEBAR_MIN_WIDTH` 列才左右分栏。
    /// 输入处理拿不到页面矩形，因此读最近一次渲染记下的 `last_area`。
    fn wide_layout(&self) -> bool {
        category_sidebar_visible(self.last_area.width)
    }

    /// 切换分类：焦点回到设置行光标。
    ///
    /// 内嵌管理列表只跟随它所属的分类出现（`embedded_list_for`），
    /// 因此换分类时必须把焦点交还 `Options`，否则会出现"列表不可见却占着方向键"。
    fn set_category(&mut self, category: SettingsCategory) {
        if category != self.category {
            self.category = category;
            self.clear_status();
        }
        if category == SettingsCategory::Data {
            // 缓存体积不走每帧渲染：切进数据分类时统计一次。
            self.refresh_cache_stats();
        }
        self.focus = SettingsFocus::Options;
    }

    /// 设置行光标 ↓ 已经停在分类最后一行时，再按一次 ↓ 进入内嵌管理列表。
    ///
    /// 删除 `s` 循环后，这是键盘进入内嵌列表的唯一入口（鼠标点击也能进入）。
    fn enter_embedded_list_if_at_last_row(&mut self) {
        let Some(list) = embedded_list_for(self.category) else {
            return;
        };
        let ids = category_row_ids(&self.row_metas, self.category);
        if ids.last() == self.row_cursor.as_ref() {
            self.focus = list;
        }
    }

    /// 内嵌列表第一条继续 ↑：焦点交还设置行光标（停在分类最后一行）。
    fn leave_embedded_list(&mut self) {
        self.focus = SettingsFocus::Options;
        let ids = category_row_ids(&self.row_metas, self.category);
        if let Some(last) = ids.last() {
            self.row_cursor = Some(*last);
        }
    }

    /// 设置行光标当前指向的元数据。
    fn current_row_meta(&self) -> Option<&SettingsRowMeta> {
        let cursor = self.row_cursor?;
        self.row_metas.iter().find(|meta| meta.id == cursor)
    }

    /// 设置项面板的按键：↑/↓ 移动光标、Enter 激活；Space 对开关是快速切换、
    /// 对取值行（如「界面主题」）等同于 Enter（见 `row_activates_with_space`）。
    ///
    /// 返回 `None` 表示这个键不归设置项面板（继续走原有分支）。
    fn handle_settings_row_keys(&mut self, key: &KeyEvent, ctx: &AppContext) -> Option<AppAction> {
        // 让位判断：子列表（JS 音源 / 本地目录 / 状态栏 / 扫码）聚焦时，
        // 方向键与激活键归它们，设置行光标不抢。
        if list_owns_direction_keys(self.focus) {
            return None;
        }
        // 窄屏单栏：先分类列表，Enter 进入后才操作设置项。
        if !self.wide_layout() {
            match self.narrow_pane {
                NarrowPane::Categories => return self.handle_narrow_category_keys(key),
                NarrowPane::Rows => {
                    if matches!(
                        (key.modifiers, key.code),
                        (KeyModifiers::NONE, KeyCode::Esc)
                    ) {
                        self.narrow_pane = NarrowPane::Categories;
                        return Some(AppAction::None);
                    }
                }
            }
        }
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Up) => {
                self.move_row_cursor(false);
                Some(AppAction::None)
            }
            (KeyModifiers::NONE, KeyCode::Down) => {
                self.move_row_cursor(true);
                // ↓ 到底后再按一次：焦点进入分类内容里的内嵌管理列表。
                self.enter_embedded_list_if_at_last_row();
                Some(AppAction::None)
            }
            (KeyModifiers::NONE, KeyCode::Enter) => Some(self.activate_cursor_row(ctx)),
            // Space 对布尔开关 = 快速切换，对枚举行 = 打开取值菜单（例如「界面主题」）。
            // 输入类行不行：空格会把输入浮层误开；说明行不行：它没有可执行的设置。
            (KeyModifiers::NONE, KeyCode::Char(' ')) => {
                let activatable = self
                    .current_row_meta()
                    .is_some_and(|meta| row_activates_with_space(meta.kind));
                if activatable {
                    Some(self.activate_cursor_row(ctx))
                } else {
                    Some(AppAction::None)
                }
            }
            _ => None,
        }
    }

    /// 窄屏分类列表：↑/↓ 换分类，Enter 进入该分类的设置项。
    fn handle_narrow_category_keys(&mut self, key: &KeyEvent) -> Option<AppAction> {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Up) => {
                self.set_category(self.category.previous());
                Some(AppAction::None)
            }
            (KeyModifiers::NONE, KeyCode::Down) => {
                self.set_category(self.category.next());
                Some(AppAction::None)
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                self.narrow_pane = NarrowPane::Rows;
                Some(AppAction::None)
            }
            _ => None,
        }
    }

    /// ↑/↓ 移动行光标（让位判断在 `navigate_row_cursor` 里，与测试同源）。
    fn move_row_cursor(&mut self, forward: bool) {
        self.row_cursor = navigate_row_cursor(
            self.focus,
            &self.row_metas,
            self.category,
            self.row_cursor,
            forward,
        );
    }

    fn activate_cursor_row(&mut self, ctx: &AppContext) -> AppAction {
        match self.row_cursor {
            Some(id) => self.activate_settings_row(id, ctx),
            None => AppAction::None,
        }
    }

    /// 键盘与鼠标**共用**的行激活入口。
    ///
    /// 激活方式由行元数据里的 `RowPlan` 决定：打开取值菜单，或直接执行动作。
    /// 设置页里没有"回灌按键"这条路，因此删掉的行内快捷键不可能再被触发。
    fn activate_settings_row(&mut self, id: SettingsRowId, ctx: &AppContext) -> AppAction {
        let Some(meta) = self.row_metas.iter().find(|meta| meta.id == id).cloned() else {
            return AppAction::None;
        };
        match plan_row_activation(&meta) {
            RowPlan::Menu(picker) => {
                self.open_enum_menu(picker, ctx);
                AppAction::None
            }
            RowPlan::Direct(action) => self.apply_direct_action(action, ctx),
            RowPlan::Inert => {
                self.set_status(format!(
                    "{}：这一行只用于显示，没有可执行的设置",
                    meta.label
                ));
                AppAction::None
            }
        }
    }

    /// 直接动作的唯一执行点：复用既有 `AppAction` / `update_config` 业务分支。
    ///
    /// 这些代码原先散在 `handle_input` 的逐行快捷键分支里；删键时它们是**平移**
    /// 过来的，业务语义（写哪些字段、什么时候落盘、要不要同步运行时）保持一致。
    fn apply_direct_action(
        &mut self,
        action: SettingsRowDirectAction,
        ctx: &AppContext,
    ) -> AppAction {
        use SettingsRowDirectAction as D;
        match action {
            // ── 界面 ──
            D::ToggleMouse => self.flip_config(ctx, |config| {
                config.ui.enable_mouse = !config.ui.enable_mouse;
            }),
            D::ToggleAggregateSearch => self.flip_config(ctx, |config| {
                config.ui.aggregate_search = !config.ui.aggregate_search;
            }),
            D::ToggleWrapNavigation => self.flip_config(ctx, |config| {
                config.ui.wrap_navigation = !config.ui.wrap_navigation;
            }),
            D::ToggleShowCover => {
                self.update_config(ctx, |config| {
                    config.ui.show_cover = !config.ui.show_cover;
                });
                if !ctx
                    .config
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .ui
                    .show_cover
                {
                    ctx.cover_service.clear();
                }
                AppAction::None
            }
            D::ToggleRememberPlaybackState => {
                let enabled = {
                    let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                    config.player.remember_playback_state = !config.player.remember_playback_state;
                    let enabled = config.player.remember_playback_state;
                    let result = crate::config::loader::save(&config, &ctx.config_path);
                    self.set_status(match result {
                        Ok(()) => "设置已保存".to_string(),
                        Err(error) => format!("保存设置失败: {}", error),
                    });
                    enabled
                };
                let result = if enabled {
                    ctx.persist_playback_session()
                } else {
                    ctx.storage.clear_playback_session()
                };
                if let Err(error) = result {
                    self.set_status(format!("播放状态设置已更新，但会话保存失败: {error}"));
                }
                AppAction::None
            }
            D::CycleNetworkTimeout => {
                let (proxy, timeout) = {
                    let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                    config.network.timeout = next_network_timeout(config.network.timeout);
                    let values = (config.network.proxy_url.clone(), config.network.timeout);
                    self.set_saved_status(crate::config::loader::save(&config, &ctx.config_path));
                    values
                };
                lx_source::configure_network(&proxy, timeout);
                AppAction::None
            }
            D::CycleCoverProtocol => {
                // 只在本终端**画得出来**的协议之间循环：kitty 下不再把
                // sixel / iterm2 摆给用户选（选了也只会得到空框）。
                let capabilities = self.cover_capabilities;
                self.update_config(ctx, |config| {
                    let current = crate::cover::protocol_from_config(&config.ui.cover_protocol)
                        .unwrap_or(capabilities.active());
                    config.ui.cover_protocol =
                        crate::cover::protocol_label(capabilities.next_supported(current))
                            .to_string();
                });
                if self.status_msg.as_deref() == Some("设置已保存") {
                    self.set_status("封面协议已保存，下次启动生效".to_string());
                }
                AppAction::None
            }
            D::CycleMaxFps => {
                self.update_config(ctx, |config| {
                    config.ui.max_fps = next_fps(config.ui.max_fps);
                });
                if self.status_msg.as_deref() == Some("设置已保存") {
                    self.set_status("刷新率已保存，下次启动生效".to_string());
                }
                AppAction::None
            }
            D::CyclePageStep => {
                self.update_config(ctx, |config| {
                    config.ui.page_step = next_page_step(config.ui.page_step);
                });
                AppAction::None
            }
            D::CycleAccentFollowCover => {
                // 即时生效：theme::accent 每帧读配置，无需任何运行时同步。
                self.update_config(ctx, |config| {
                    config.ui.accent_follow_cover = config.ui.accent_follow_cover.next();
                });
                if self.status_msg.as_deref() == Some("设置已保存") {
                    let mode = {
                        let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
                        config.ui.accent_follow_cover
                    };
                    self.set_status(format!("封面主色跟随：{}", mode.label()));
                }
                AppAction::None
            }
            D::ToggleTrackChangeNotification => self.flip_config(ctx, |config| {
                config.notification.track_change = !config.notification.track_change;
            }),

            // ── 播放 ──
            D::CyclePlaybackSpeed => {
                self.set_status(ctx.cycle_playback_speed());
                AppAction::None
            }
            D::EditAudioDevice => {
                self.audio_device_input = ctx
                    .config
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .player
                    .audio_device
                    .clone();
                self.audio_device_input_mode = true;
                self.set_status("输入 libmpv 音频设备名，Enter 保存".to_string());
                AppAction::None
            }
            D::CycleReplayGainMode => {
                self.set_status(ctx.cycle_replaygain_mode());
                AppAction::None
            }
            D::CycleReplayGainPreamp => {
                self.set_status(ctx.cycle_replaygain_preamp());
                AppAction::None
            }
            D::CycleChannelMode => {
                self.set_status(ctx.cycle_channel_mode());
                AppAction::None
            }
            D::CycleBalance => {
                self.set_status(ctx.cycle_balance());
                AppAction::None
            }
            D::ToggleReplayGainClip => {
                self.set_status(ctx.toggle_replaygain_clip());
                AppAction::None
            }
            D::CycleFadeInDuration => {
                let duration = {
                    let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                    config.player.fade_in_ms = next_fade_duration(config.player.fade_in_ms);
                    let value = config.player.fade_in_ms;
                    self.set_saved_status(crate::config::loader::save(&config, &ctx.config_path));
                    value
                };
                self.set_status(format!("淡入: {}", fade_label(duration)));
                AppAction::None
            }
            D::CycleFadeOutDuration => {
                let duration = {
                    let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                    config.player.fade_out_ms = next_fade_duration(config.player.fade_out_ms);
                    let value = config.player.fade_out_ms;
                    self.set_saved_status(crate::config::loader::save(&config, &ctx.config_path));
                    value
                };
                self.set_status(format!("淡出: {}", fade_label(duration)));
                AppAction::None
            }
            D::RunFadeIn => {
                self.set_status(ctx.fade_in_now());
                AppAction::None
            }
            D::RunFadeOut => {
                self.set_status(ctx.fade_out_now());
                AppAction::None
            }
            D::SetAbLoopStart => {
                self.set_status(ctx.set_ab_loop_start_now());
                AppAction::None
            }
            D::SetAbLoopEnd => {
                self.set_status(ctx.set_ab_loop_end_now());
                AppAction::None
            }
            D::ClearAbLoop => {
                self.set_status(ctx.clear_ab_loop());
                AppAction::None
            }
            D::CycleHistoryLimit => {
                let limit = {
                    let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                    config.player.history_limit = next_history_limit(config.player.history_limit);
                    let limit = config.player.history_limit;
                    let result = crate::config::loader::save(&config, &ctx.config_path);
                    self.set_status(match result {
                        Ok(()) => format!("历史上限: {limit}"),
                        Err(error) => format!("保存设置失败: {error}"),
                    });
                    limit
                };
                ctx.storage.trim_history(limit);
                AppAction::None
            }

            // ── 音源与歌词 ──
            D::ToggleAutoSource => self.flip_config(ctx, |config| {
                config.source.auto_toggle = !config.source.auto_toggle;
            }),
            D::ToggleLyricTranslation => {
                let enabled = {
                    let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                    config.lyric.show_translation = !config.lyric.show_translation;
                    let enabled = config.lyric.show_translation;
                    self.set_saved_status(crate::config::loader::save(&config, &ctx.config_path));
                    enabled
                };
                ctx.lyric_service.set_translation_enabled(enabled);
                AppAction::None
            }
            D::ToggleLyricYrc => {
                let enabled = {
                    let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                    config.lyric.show_yrc = !config.lyric.show_yrc;
                    let enabled = config.lyric.show_yrc;
                    self.set_saved_status(crate::config::loader::save(&config, &ctx.config_path));
                    enabled
                };
                ctx.lyric_service.set_yrc_enabled(enabled);
                AppAction::None
            }
            D::EditProxy => {
                self.proxy_input = ctx
                    .config
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .network
                    .proxy_url
                    .clone();
                self.proxy_input_mode = true;
                self.clear_status();
                AppAction::None
            }
            D::ReloadJsSources => AppAction::ReloadJsSources,
            D::CheckSourceHealth => {
                self.set_status("正在检测音源…".to_string());
                AppAction::CheckSourceHealth
            }

            // ── 账号与扫码 ──
            D::QrLoginSelected => {
                let login_sources = self.qr_login_sources(ctx);
                if login_sources.is_empty() {
                    self.set_status("当前没有支持扫码登录的音源".to_string());
                    return AppAction::None;
                }
                self.qr_login_source_index %= login_sources.len();
                let (source, kind) = login_sources[self.qr_login_source_index];
                self.qr_login_toggle(source, kind, ctx)
            }
            D::SyncNetease => AppAction::SyncNetease,
            D::SyncQq => AppAction::SyncQq,
            D::ImportExternalPlaylist => {
                self.playlist_import_input.clear();
                self.playlist_import_mode = true;
                self.set_status("输入 M3U/LX Music/网易云歌单路径，Enter 导入".to_string());
                AppAction::None
            }

            // ── 通知与集成 ──
            D::CycleScrollAmount => self.flip_config(ctx, |config| {
                config.ui.scroll_amount = next_scroll_amount(config.ui.scroll_amount);
            }),
            D::ToggleMpris => {
                self.update_config(ctx, |config| {
                    config.integration.mpris = !config.integration.mpris;
                });
                if self.status_msg.as_deref() == Some("设置已保存") {
                    self.set_status("MPRIS 设置已保存，下次启动生效".to_string());
                }
                AppAction::None
            }
            D::ToggleInAppNotification => self.flip_config(ctx, |config| {
                config.notification.in_app = !config.notification.in_app;
            }),
            D::CycleInAppTimeout => self.flip_config(ctx, |config| {
                config.notification.in_app_timeout = match config.notification.in_app_timeout {
                    0..=2 => 4,
                    3..=4 => 6,
                    5..=6 => 8,
                    _ => 2,
                };
            }),
            D::ToggleDesktopNotification => self.flip_config(ctx, |config| {
                config.notification.enable = !config.notification.enable;
            }),
            D::ToggleNotificationAlbumCover => self.flip_config(ctx, |config| {
                config.notification.album_cover = !config.notification.album_cover;
            }),

            // ── 下载 ──
            D::EditDownloadDir => {
                self.download_input = ctx.downloads.download_dir().display().to_string();
                self.download_input_target = Some(DownloadInputTarget::Dir);
                self.set_status("输入下载目录，Enter 保存，Esc 取消".to_string());
                AppAction::None
            }
            D::CycleDownloadQuality => self.flip_config(ctx, |config| {
                config.download.quality = match config.download.quality {
                    None => Some(config.player.quality),
                    Some(Quality::Low128) => Some(Quality::High320),
                    Some(Quality::High320) => Some(Quality::Flac),
                    Some(Quality::Flac) => Some(Quality::Flac24),
                    Some(Quality::Flac24) => None,
                };
            }),
            D::EditFilenameTemplate => {
                self.download_input = ctx
                    .config
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .download
                    .filename_template
                    .clone();
                self.download_input_target = Some(DownloadInputTarget::Template);
                self.set_status(
                    "输入文件名模板，支持 {name} {singer} {album} {source} {quality}".to_string(),
                );
                AppAction::None
            }
            D::ToggleMultipart => self.flip_config(ctx, |config| {
                config.download.multipart = !config.download.multipart;
            }),
            D::CycleMultipartMinSize => self.flip_config(ctx, |config| {
                config.download.multipart_min_size_mb = next_step(
                    &[1, 2, 5, 10, 20, 50],
                    config.download.multipart_min_size_mb,
                );
            }),
            D::CycleDownloadConcurrency => self.flip_config(ctx, |config| {
                config.download.concurrency =
                    next_step(&[1, 2, 4, 8, 16], config.download.concurrency as u64) as usize;
            }),
            D::CycleConcurrentSongs => self.flip_config(ctx, |config| {
                config.download.concurrent_songs =
                    next_step(&[1, 2, 3, 4], config.download.concurrent_songs as u64) as usize;
            }),
            D::CycleMaxRetries => self.flip_config(ctx, |config| {
                config.download.max_retries =
                    next_step(&[0, 1, 2, 3, 5], config.download.max_retries as u64) as u32;
            }),
            D::ToggleVerifySize => self.flip_config(ctx, |config| {
                config.download.verify_size = !config.download.verify_size;
            }),
            D::ToggleSkipExisting => self.flip_config(ctx, |config| {
                config.download.skip_existing = !config.download.skip_existing;
            }),
            D::ToggleWriteTags => self.flip_config(ctx, |config| {
                config.download.write_tags = !config.download.write_tags;
            }),
            D::ToggleEmbedCover => self.flip_config(ctx, |config| {
                config.download.embed_cover = !config.download.embed_cover;
            }),
            D::ToggleSaveLyric => self.flip_config(ctx, |config| {
                config.download.save_lyric = !config.download.save_lyric;
            }),

            // ── 数据与本地库 ──
            D::CycleScanDepth => self.flip_config(ctx, |config| {
                config.local_music.max_depth = next_scan_depth(config.local_music.max_depth);
            }),
            D::ExportData => {
                self.set_status(match ctx.storage.export_default() {
                    Ok(path) => format!("数据已导出: {}", path.display()),
                    Err(error) => format!("数据导出失败: {error}"),
                });
                AppAction::None
            }
            D::ImportData => {
                self.set_status(match ctx.storage.import_default() {
                    Ok(path) => format!("数据已导入，原数据备份于: {}", path.display()),
                    Err(error) => format!("数据导入失败: {error}"),
                });
                AppAction::None
            }
            D::ClearCoverCache => {
                let message = match lx_source::cover_cache::clear_cache() {
                    Ok((files, bytes)) => {
                        format!(
                            "已清理封面缓存：{} 个文件，释放 {}",
                            files,
                            format_bytes(bytes)
                        )
                    }
                    Err(error) => format!("清理封面缓存失败: {error}"),
                };
                self.refresh_cache_stats();
                self.set_status(message);
                AppAction::None
            }
            D::ClearRemoteCache => {
                let message = match crate::remote_cache::clear() {
                    Ok(()) => format!(
                        "已清除网易云歌单镜像（{}），下次同步会自动重建",
                        format_bytes(crate::remote_cache::cache_file_size())
                    ),
                    Err(error) => format!("清除网易云歌单缓存失败: {error}"),
                };
                self.refresh_cache_stats();
                self.set_status(message);
                AppAction::None
            }
        }
    }

    /// 重新统计缓存体积（只在进入数据分类 / 清理后调用，避免每帧扫盘）。
    pub fn refresh_cache_stats(&mut self) {
        let (files, bytes) = lx_source::cover_cache::cache_stats();
        self.cover_cache_label = format!("{} 个 · {}", files, format_bytes(bytes));
        self.remote_cache_label = format!(
            "{} · 歌单镜像",
            format_bytes(crate::remote_cache::cache_file_size())
        );
    }

    /// `update_config` + `AppAction::None`：省掉几十个 `{ ...; AppAction::None }`。
    fn flip_config(
        &mut self,
        ctx: &AppContext,
        update: impl FnOnce(&mut lx_core::model::config::Config),
    ) -> AppAction {
        self.update_config(ctx, update);
        AppAction::None
    }

    /// 打开枚举取值菜单：菜单内容是 `enum_menu` 的纯数据视图（当前值打 ✓）。
    ///
    /// 音质 / 播放模式选中后映射回既有 `AppAction`；其余设置项由设置页
    /// 自己写配置（`apply_setting_choice`），都不新增业务语义。
    fn open_enum_menu(&mut self, picker: SettingsEnumPicker, ctx: &AppContext) {
        let origin = self.menu_origin();
        let play_mode = ctx.playlist.mode();
        let menu = {
            let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
            enum_menu(picker, &config, play_mode)
        };
        let title = menu.title.clone();
        let items = menu.into_items(picker.row_id());
        self.menu = Some(SongContextMenu::from_status_items(origin, title, items));
    }

    /// 菜单出现的位置：贴着当前行左上角，行不可见时退回页面左上角。
    fn menu_origin(&self) -> Position {
        self.row_hits
            .iter()
            .find(|hit| Some(hit.id) == self.row_cursor)
            .map(|hit| Position::new(hit.rect.x, hit.rect.y))
            .unwrap_or_else(|| Position::new(self.last_area.x, self.last_area.y))
    }

    /// 模态菜单的按键处理。返回 `None` 表示当前没有菜单。
    fn handle_menu_key(
        &mut self,
        key: &KeyEvent,
        ctx: &AppContext,
        resolver: &KeybindingResolver,
    ) -> Option<AppAction> {
        let bounds = self.last_area;
        let outcome = self
            .menu
            .as_mut()?
            .handle_key(key, resolver, "settings", bounds);
        Some(self.finish_menu_outcome(outcome, ctx))
    }

    /// 菜单结果 → AppAction（动作直接复用现有业务分支）。
    fn finish_menu_outcome(&mut self, outcome: MenuOutcome, ctx: &AppContext) -> AppAction {
        match outcome {
            MenuOutcome::None => AppAction::None,
            MenuOutcome::Close => {
                self.menu = None;
                AppAction::None
            }
            MenuOutcome::Action(action) => {
                self.menu = None;
                self.dispatch_menu_action(action, ctx)
            }
        }
    }

    /// 分派一条菜单动作。
    ///
    /// - `SettingChoice`：设置页自己解释。音质 / 播放模式映射回既有 `AppAction`，
    ///   其余"行 + 取值"直接写配置（`apply_setting_choice`）并即时生效；
    /// - 其余动作（`ReloadJsSources` 等）：仍走 `settings_menu_app_action`。
    fn dispatch_menu_action(&mut self, action: MenuAction, ctx: &AppContext) -> AppAction {
        match action {
            MenuAction::SettingChoice { row, value } => match row.as_str() {
                SETTING_ROW_QUALITY => match quality_by_label(&value) {
                    Some(quality) => AppAction::SetQuality(quality),
                    None => {
                        self.set_status(choice_refusal_message(&row).to_string());
                        AppAction::None
                    }
                },
                SETTING_ROW_PLAY_MODE => AppAction::SetPlayMode(value),
                _ => self.apply_setting_choice_action(&row, &value, ctx),
            },
            action => match settings_menu_app_action(action) {
                Some(app_action) => app_action,
                None => {
                    self.set_status(choice_refusal_message("").to_string());
                    AppAction::None
                }
            },
        }
    }

    /// 把菜单里选中的"设置行 + 取值"写进配置并立即生效。
    ///
    /// 写盘沿用设置页既有的"枚举即时生效、无需重启"语义；需要运行时同步的几行
    /// （均衡器 → 播放器、歌词偏移 → 歌词服务、默认音源 / 音源开关 → 音源管理器）
    /// 在落盘后按原先循环键的同一路径同步，保证与旧行为一致。
    fn apply_setting_choice_action(
        &mut self,
        row: &str,
        value: &str,
        ctx: &AppContext,
    ) -> AppAction {
        let (message, bands, lyric_offset, source_preferences, save_result) = {
            let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
            let Some(message) = apply_setting_choice(&mut config, row, value) else {
                self.set_status(choice_refusal_message(row).to_string());
                return AppAction::None;
            };
            let bands = config.player.equalizer_bands.clone();
            let lyric_offset = config.lyric.offset;
            let source_preferences = (config.source.default, config.source.enabled.clone());
            let save_result = crate::config::loader::save(&config, &ctx.config_path);
            (
                message,
                bands,
                lyric_offset,
                source_preferences,
                save_result,
            )
        };
        if row == SETTING_ROW_EQUALIZER {
            ctx.player.set_equalizer_bands(&bands);
        }
        if row == SETTING_ROW_LYRIC_OFFSET {
            ctx.lyric_service.set_offset_ms(lyric_offset);
        }
        if matches!(row, SETTING_ROW_DEFAULT_SOURCE | SETTING_ROW_SOURCE_ENABLED) {
            ctx.source_manager
                .update_source_preferences(source_preferences.0, &source_preferences.1);
        }
        self.set_status(match save_result {
            Ok(()) => message,
            Err(error) => format!("{message}（但保存失败: {error}）"),
        });
        AppAction::None
    }

    fn handle_proxy_input(&mut self, key: KeyEvent, ctx: &AppContext) -> AppAction {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc) => {
                self.proxy_input_mode = false;
                self.proxy_input.clear();
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                let proxy = self.proxy_input.trim().to_string();
                let timeout = {
                    let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                    config.network.proxy_url = proxy.clone();
                    let timeout = config.network.timeout;
                    self.set_saved_status(crate::config::loader::save(&config, &ctx.config_path));
                    timeout
                };
                lx_source::configure_network(&proxy, timeout);
                self.proxy_input_mode = false;
                self.proxy_input.clear();
            }
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                self.proxy_input.pop();
            }
            (modifiers, KeyCode::Char(c))
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.proxy_input.push(c);
            }
            _ => {}
        }
        AppAction::None
    }

    fn handle_audio_device_input(&mut self, key: KeyEvent, ctx: &AppContext) -> AppAction {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc) => {
                self.audio_device_input_mode = false;
                self.audio_device_input.clear();
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                let device = self.audio_device_input.trim().to_string();
                if !device.is_empty() {
                    self.set_status(ctx.set_audio_output_device(&device));
                }
                self.audio_device_input_mode = false;
                self.audio_device_input.clear();
            }
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                self.audio_device_input.pop();
            }
            (modifiers, KeyCode::Char(c))
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && c != '\0' =>
            {
                self.audio_device_input.push(c);
            }
            _ => {}
        }
        AppAction::None
    }

    /// 下载目录 / 文件名模板的文本输入。
    fn handle_download_input(&mut self, key: KeyEvent, ctx: &AppContext) -> AppAction {
        let Some(target) = self.download_input_target else {
            return AppAction::None;
        };
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc) => {
                self.download_input_target = None;
                self.download_input.clear();
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                let value = self.download_input.trim().to_string();
                self.download_input_target = None;
                self.download_input.clear();
                match target {
                    DownloadInputTarget::Dir => {
                        let dir = crate::download::naming::resolve_download_dir(&value);
                        self.update_config(ctx, |config| {
                            config.download.dir = value.clone();
                        });
                        self.set_status(format!("下载目录: {}", dir.display()));
                    }
                    DownloadInputTarget::Template => {
                        if value.is_empty() {
                            return AppAction::None;
                        }
                        self.update_config(ctx, |config| {
                            config.download.filename_template = value.clone();
                        });
                        self.set_status(format!("文件名模板: {value}"));
                    }
                }
            }
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                self.download_input.pop();
            }
            (modifiers, KeyCode::Char(c))
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && c != '\0' =>
            {
                self.download_input.push(c);
            }
            _ => {}
        }
        AppAction::None
    }

    fn handle_playlist_import_input(&mut self, key: KeyEvent, _ctx: &AppContext) -> AppAction {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc) => {
                self.playlist_import_mode = false;
                self.playlist_import_input.clear();
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                let path = self.playlist_import_input.trim().to_string();
                self.playlist_import_mode = false;
                self.playlist_import_input.clear();
                if path.is_empty() {
                    return AppAction::None;
                }
                // 导入在后台任务中完成（解析大歌单 + 一次性写盘），
                // 完成后通过通知汇报结果，避免阻塞 TUI 主循环。
                return AppAction::ImportExternalPlaylist(path);
            }
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                self.playlist_import_input.pop();
            }
            (modifiers, KeyCode::Char(c))
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.playlist_import_input.push(c);
            }
            _ => {}
        }
        AppAction::None
    }

    /// 对某一个音源执行"扫码登录 / 退出登录"的切换（纯语义，便于回归测试）。
    ///
    /// 删掉 `b`（退出登录）之后，登录态切换只剩这一条路：设置行、扫码列表的
    /// `Enter`、鼠标点击列表条目都走它，因此**退出登录仍然可达**。
    fn qr_login_toggle(
        &mut self,
        source: SourceId,
        kind: QrLoginKind,
        ctx: &AppContext,
    ) -> AppAction {
        qr_login_action(ctx.source_manager.is_logged_in(source), source, kind)
    }

    /// 处理本地音乐路径输入模式
    fn handle_local_path_input(&mut self, key: KeyEvent, ctx: &AppContext) -> AppAction {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc) => {
                self.local_path_mode = false;
                self.local_path_input.clear();
                AppAction::None
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                if !self.local_path_input.trim().is_empty() {
                    let path = self.local_path_input.trim().to_string();
                    let save_result = {
                        let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                        if !config.local_music.paths.contains(&path) {
                            config.local_music.paths.push(path.clone());
                            config.local_music.enabled = true;
                        }
                        crate::config::loader::save(&config, &ctx.config_path)
                    };
                    let (paths, max_depth) = {
                        let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
                        (
                            config.local_music.paths.clone(),
                            config.local_music.max_depth,
                        )
                    };
                    self.local_path_mode = false;
                    self.local_path_input.clear();
                    if let Err(error) = save_result {
                        self.set_status(format!("目录已添加，但保存失败: {}", error));
                    } else {
                        self.set_status("正在扫描本地音乐...".to_string());
                    }
                    return AppAction::ScanLocalMusic {
                        paths,
                        max_depth,
                        force: true,
                    };
                }
                AppAction::None
            }
            (modifiers, KeyCode::Char(c))
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.local_path_input.push(c);
                AppAction::None
            }
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                self.local_path_input.pop();
                AppAction::None
            }
            _ => AppAction::None,
        }
    }

    /// 处理 JS 音源列表的按键（该列表聚焦时）。
    ///
    /// 方向键与配置的列表导航键（默认 `k`/`j`）移动光标；命令行操作键
    /// （`a` 添加 / `d` 移除 / `h` 检测）走 `embedded_command` ——
    /// 与鼠标点击命令行按钮**同一条**路径。
    fn handle_js_source_keys(
        &mut self,
        key: KeyEvent,
        ctx: &AppContext,
        resolver: &KeybindingResolver,
    ) -> Option<AppAction> {
        let len = ctx
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .source
            .js_sources
            .len();

        if let Some(action) = resolver.resolve_page("settings", &key) {
            match action {
                Action::ListSelectUp => {
                    if self.selected_source == 0 {
                        self.leave_embedded_list();
                    } else {
                        self.selected_source -= 1;
                    }
                    return Some(AppAction::None);
                }
                Action::ListSelectDown => {
                    if self.selected_source + 1 < len {
                        self.selected_source += 1;
                    }
                    return Some(AppAction::None);
                }
                _ => {}
            }
        }

        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Up) => {
                if self.selected_source == 0 {
                    self.leave_embedded_list();
                } else {
                    self.selected_source -= 1;
                }
                Some(AppAction::None)
            }
            (KeyModifiers::NONE, KeyCode::Down) => {
                if self.selected_source + 1 < len {
                    self.selected_source += 1;
                }
                Some(AppAction::None)
            }
            (KeyModifiers::NONE, KeyCode::Char(character)) => {
                let command = embedded_command(SettingsFocus::JsSources, character)?;
                Some(self.run_embedded_command(command, ctx))
            }
            _ => None,
        }
    }

    /// 处理本地音乐区域的按键（非输入模式）。
    ///
    /// 与 JS 音源列表同构：方向键 / 列表导航键移动光标，命令行操作键
    /// （`a` 添加目录 / `d` 移除 / `r` 重新扫描）走 `embedded_command`。
    fn handle_local_keys(
        &mut self,
        key: KeyEvent,
        ctx: &AppContext,
        resolver: &KeybindingResolver,
    ) -> Option<AppAction> {
        let len = ctx
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .local_music
            .paths
            .len();

        if let Some(action) = resolver.resolve_page("settings", &key) {
            match action {
                Action::ListSelectUp => {
                    if self.selected_local_path == 0 {
                        self.leave_embedded_list();
                    } else {
                        self.selected_local_path -= 1;
                    }
                    return Some(AppAction::None);
                }
                Action::ListSelectDown => {
                    if self.selected_local_path + 1 < len {
                        self.selected_local_path += 1;
                    }
                    return Some(AppAction::None);
                }
                _ => {}
            }
        }

        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Up) => {
                if self.selected_local_path == 0 {
                    self.leave_embedded_list();
                } else {
                    self.selected_local_path -= 1;
                }
                Some(AppAction::None)
            }
            (KeyModifiers::NONE, KeyCode::Down) => {
                if self.selected_local_path + 1 < len {
                    self.selected_local_path += 1;
                }
                Some(AppAction::None)
            }
            (KeyModifiers::NONE, KeyCode::Char(character)) => {
                let command = embedded_command(SettingsFocus::LocalPaths, character)?;
                Some(self.run_embedded_command(command, ctx))
            }
            _ => None,
        }
    }

    /// 内嵌管理列表命令的唯一执行点：键盘别名与鼠标点击命令行按钮共用。
    fn run_embedded_command(&mut self, command: EmbeddedCommand, ctx: &AppContext) -> AppAction {
        use EmbeddedCommand as E;
        match command {
            E::AddJsSource => {
                self.input_mode = true;
                self.clear_status();
                AppAction::None
            }
            E::RemoveJsSource => {
                let sources = ctx
                    .config
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .source
                    .js_sources
                    .clone();
                if sources.is_empty() || self.selected_source >= sources.len() {
                    return AppAction::None;
                }
                // 二次确认：首次只武装并提示，窗口内再按一次才真正删除
                let now = Instant::now();
                let confirmed = matches!(
                    self.delete_source_armed,
                    Some(armed_at) if now.duration_since(armed_at) <= DELETE_CONFIRM_WINDOW
                );
                if !confirmed {
                    self.delete_source_armed = Some(now);
                    self.set_status("再按一次确认删除该音源，Esc 取消".to_string());
                    return AppAction::None;
                }
                self.delete_source_armed = None;
                let url = sources[self.selected_source].clone();
                self.set_status("已移除音源".to_string());
                if self.selected_source >= sources.len().saturating_sub(1) {
                    self.selected_source = self.selected_source.saturating_sub(1);
                }
                AppAction::RemoveSource(url)
            }
            E::CheckSourceHealth => {
                self.set_status("正在检测音源…".to_string());
                AppAction::CheckSourceHealth
            }
            E::AddLocalPath => {
                self.local_path_mode = true;
                self.local_path_input.clear();
                self.clear_status();
                AppAction::None
            }
            E::RemoveLocalPath => {
                let paths = ctx
                    .config
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .local_music
                    .paths
                    .clone();
                if paths.is_empty() || self.selected_local_path >= paths.len() {
                    return AppAction::None;
                }
                let now = Instant::now();
                let confirmed = matches!(
                    self.delete_local_path_armed,
                    Some(armed_at) if now.duration_since(armed_at) <= DELETE_CONFIRM_WINDOW
                );
                if !confirmed {
                    self.delete_local_path_armed = Some(now);
                    self.set_status("再按一次确认删除该本地目录，Esc 取消".to_string());
                    return AppAction::None;
                }
                self.delete_local_path_armed = None;
                let removed = paths[self.selected_local_path].clone();
                let save_result = {
                    let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                    config.local_music.paths.retain(|p| p != &removed);
                    config.local_music.enabled = !config.local_music.paths.is_empty();
                    crate::config::loader::save(&config, &ctx.config_path)
                };
                if self.selected_local_path >= paths.len().saturating_sub(1) {
                    self.selected_local_path = self.selected_local_path.saturating_sub(1);
                }
                let (remaining, max_depth) = {
                    let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
                    (
                        config.local_music.paths.clone(),
                        config.local_music.max_depth,
                    )
                };
                self.set_status(match save_result {
                    Ok(()) => format!("已移除 {}，正在重新扫描...", removed),
                    Err(error) => format!("已移除，但保存失败: {}", error),
                });
                AppAction::ScanLocalMusic {
                    paths: remaining,
                    max_depth,
                    force: true,
                }
            }
            E::RescanLocalMusic => {
                let (paths, max_depth) = {
                    let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
                    (
                        config.local_music.paths.clone(),
                        config.local_music.max_depth,
                    )
                };
                self.set_status("正在扫描本地音乐...".to_string());
                AppAction::ScanLocalMusic {
                    paths,
                    max_depth,
                    force: true,
                }
            }
        }
    }

    fn handle_status_bar_keys(
        &mut self,
        key: KeyEvent,
        ctx: &AppContext,
        resolver: &KeybindingResolver,
    ) -> Option<AppAction> {
        let item_count = StatusBarItem::ALL.len();
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Enter | KeyCode::Char(' ')) => {
                self.toggle_status_bar_item(ctx);
                return Some(AppAction::None);
            }
            (KeyModifiers::SHIFT, KeyCode::Left | KeyCode::Up) => {
                self.move_status_bar_item(ctx, -1);
                return Some(AppAction::None);
            }
            (KeyModifiers::SHIFT, KeyCode::Right | KeyCode::Down) => {
                self.move_status_bar_item(ctx, 1);
                return Some(AppAction::None);
            }
            (KeyModifiers::NONE, KeyCode::Up) => {
                if self.selected_status_item == 0 {
                    self.leave_embedded_list();
                } else {
                    self.selected_status_item -= 1;
                }
                return Some(AppAction::None);
            }
            (KeyModifiers::NONE, KeyCode::Down) => {
                self.selected_status_item =
                    (self.selected_status_item + 1).min(item_count.saturating_sub(1));
                return Some(AppAction::None);
            }
            _ => {}
        }

        if let Some(action) = resolver.resolve_page("settings", &key) {
            match action {
                Action::ListSelectUp => {
                    if self.selected_status_item == 0 {
                        self.leave_embedded_list();
                    } else {
                        self.selected_status_item -= 1;
                    }
                    return Some(AppAction::None);
                }
                Action::ListSelectDown => {
                    self.selected_status_item =
                        (self.selected_status_item + 1).min(item_count.saturating_sub(1));
                    return Some(AppAction::None);
                }
                _ => {}
            }
        }
        None
    }

    fn toggle_status_bar_item(&mut self, ctx: &AppContext) {
        let item = StatusBarItem::ALL[self.selected_status_item % StatusBarItem::ALL.len()];
        let (enabled, result) = {
            let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
            if config.ui.status_bar_items.contains(&item) {
                config
                    .ui
                    .status_bar_items
                    .retain(|candidate| *candidate != item);
                let result = crate::config::loader::save(&config, &ctx.config_path);
                (false, result)
            } else {
                config.ui.status_bar_items.push(item);
                let result = crate::config::loader::save(&config, &ctx.config_path);
                (true, result)
            }
        };
        self.set_status(match result {
            Ok(()) => format!(
                "状态栏“{}”已{}",
                status_bar_item_label(item),
                if enabled { "显示" } else { "隐藏" }
            ),
            Err(error) => format!("状态栏已更新，但保存失败: {error}"),
        });
    }

    fn move_status_bar_item(&mut self, ctx: &AppContext, direction: isize) {
        let item = StatusBarItem::ALL[self.selected_status_item % StatusBarItem::ALL.len()];
        let (position, result) = {
            let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
            let Some(index) = config
                .ui
                .status_bar_items
                .iter()
                .position(|candidate| *candidate == item)
            else {
                self.set_status("请先启用这个状态栏字段".to_string());
                return;
            };
            let new_index = if direction < 0 {
                index.saturating_sub(1)
            } else {
                (index + 1).min(config.ui.status_bar_items.len().saturating_sub(1))
            };
            if new_index == index {
                return;
            }
            config.ui.status_bar_items.swap(index, new_index);
            let result = crate::config::loader::save(&config, &ctx.config_path);
            (new_index + 1, result)
        };
        self.set_status(match result {
            Ok(()) => format!(
                "状态栏“{}”已移到第 {position} 位",
                status_bar_item_label(item)
            ),
            Err(error) => format!("状态栏顺序已更新，但保存失败: {error}"),
        });
    }

    fn move_status_bar_item_to(&mut self, ctx: &AppContext, target_index: usize) {
        let item = StatusBarItem::ALL[self.selected_status_item % StatusBarItem::ALL.len()];
        let Some(&target) = StatusBarItem::ALL.get(target_index) else {
            return;
        };
        if item == target {
            return;
        }

        let (position, result) = {
            let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
            if !config.ui.status_bar_items.contains(&item) {
                self.set_status("请先启用这个状态栏字段".to_string());
                return;
            }
            let Some(position) =
                reorder_status_bar_items(&mut config.ui.status_bar_items, item, target)
            else {
                // Disabled fields have no display-order position to drop on.
                return;
            };
            let result = crate::config::loader::save(&config, &ctx.config_path);
            (position + 1, result)
        };
        self.set_status(match result {
            Ok(()) => format!(
                "状态栏“{}”已移到第 {position} 位",
                status_bar_item_label(item)
            ),
            Err(error) => format!("状态栏顺序已更新，但保存失败: {error}"),
        });
    }

    /// 渲染"账号与扫码"分类里内嵌的扫码登录列表，返回命中账本。
    ///
    /// `login_sources` 是支持扫码登录的音源（= `qr_login_sources`），与
    /// `qr_login_source_index` 一一对应，因此列表条目可以直接按点击选中。
    /// 真正的二维码仍然是既有浮层（`AppAction::QrLogin`），这里只画入口列表。
    fn render_qr_login_panel(
        &self,
        area: Rect,
        buf: &mut Buffer,
        ctx: &AppContext,
        accent: Color,
        muted: Color,
        login_sources: &[(SourceId, QrLoginKind)],
    ) -> EmbeddedHits {
        let block = Block::default()
            .borders(PANEL_BORDERS)
            .border_style(Style::new().fg(if self.focus == SettingsFocus::QrLogin {
                accent
            } else {
                crate::theme::border(ctx)
            }))
            .title(" 扫码登录 · ↑/↓选择 · Enter/P 登录或退出 · v 查看远程歌单窗口 ");
        let inner = panel_inner(area);
        block.render(area, buf);
        let mut hits = EmbeddedHits::default();

        if inner.height == 0 {
            return hits;
        }
        let selected_qr = login_sources
            .get(self.qr_login_source_index % login_sources.len().max(1))
            .copied();
        let mut row = inner.y;
        // 一个音源可能有两个入口（QQ / 微信），计数按音源去重，
        // 否则「支持扫码 / 已登录」会跟着渠道数量一起翻倍。
        let mut supported: Vec<SourceId> = Vec::new();
        for (source, _) in login_sources {
            if !supported.contains(source) {
                supported.push(*source);
            }
        }
        let summary = format!(
            " 支持扫码: {}  · 已登录: {}",
            supported.len(),
            supported
                .iter()
                .filter(|source| source_session_valid(**source, ctx))
                .count(),
        );
        Paragraph::new(Line::from(Span::styled(summary, Style::new().fg(muted))))
            .render(Rect::new(inner.x, row, inner.width, 1), buf);
        row = row.saturating_add(1);

        for (index, (source, kind)) in login_sources.iter().enumerate() {
            if row >= inner.bottom() {
                break;
            }
            let (status, status_color) = source_login_display(*source, ctx);
            let qr_selected = selected_qr == Some((*source, *kind));
            let style = if qr_selected {
                Style::new()
                    .fg(crate::theme::selection_fg(ctx))
                    .bg(accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(crate::theme::text(ctx))
            };
            // 同一音源的第二条渠道（微信）要在名字后标出来，否则两行长得一样。
            let label = match kind.badge() {
                Some(badge) => format!(" {:<12} ", format!("{}（{badge}）", source.display_name())),
                None => format!(" {:<12} ", source.display_name()),
            };
            let line = Line::from(vec![
                Span::styled(label, style),
                Span::styled(status, Style::new().fg(status_color)),
            ]);
            let rect = Rect::new(inner.x, row, inner.width, 1);
            hits.rows
                .push(render_embedded_row(buf, rect, index, line, None));
            row = row.saturating_add(1);
        }

        // 网易云远程集合是播放数据，不写入本地歌单；必须在设置里明确可见。
        // 渲染路径不拷贝整个缓存，只取计数与当前可见行的文本。
        let (cached_total, cached_normal, cached_favorites) = crate::remote_cache::summary_counts();
        if row < inner.bottom() {
            Paragraph::new(Line::from(vec![
                Span::styled(
                    " 网易云远程歌单",
                    Style::new().fg(accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(
                        "  · 已缓存 {cached_total} 个（普通 {cached_normal} / 红心 {cached_favorites}）  · 选中「刷新远程歌单」行按 Enter 刷新"
                    ),
                    Style::new().fg(muted),
                ),
            ]))
            .render(Rect::new(inner.x, row, inner.width, 1), buf);
            row = row.saturating_add(1);
        }

        if row < inner.bottom() {
            if cached_total == 0 {
                Paragraph::new(" 尚未读取网易云远程歌单。登录后选中「刷新远程歌单」行按 Enter。")
                    .style(Style::new().fg(crate::theme::yellow(ctx)))
                    .render(Rect::new(inner.x, row, inner.width, 1), buf);
            } else {
                let room = inner.bottom().saturating_sub(row) as usize;
                let width = inner.width.saturating_sub(20) as usize;
                let lines = crate::remote_cache::with_netease(|collections| {
                    collections
                        .iter()
                        .take(room)
                        .map(|collection| {
                            let kind = if matches!(
                                collection.kind,
                                lx_core::sync::SyncCollectionKind::Favorites
                            ) {
                                "红心"
                            } else {
                                "歌单"
                            };
                            format!(
                                " {}  {}  · {} 首",
                                kind,
                                truncate_display(&collection.name, width),
                                collection.songs.len()
                            )
                        })
                        .collect::<Vec<_>>()
                });
                for line in lines {
                    Paragraph::new(Line::from(Span::styled(
                        line,
                        Style::new().fg(crate::theme::text(ctx)),
                    )))
                    .render(Rect::new(inner.x, row, inner.width, 1), buf);
                    row = row.saturating_add(1);
                }
            }
        }
        hits
    }

    /// 渲染"音源与歌词"分类里内嵌的 JS 音源列表，返回命中账本。
    ///
    /// 内容 = 原来的下方面板：命令行（添加/移除/检测）、加载状态、音源体检结果、
    /// 每条音源的状态与地址；此外按 `AppContext::js_source_status` 补上每项的
    /// 加载 ✓/✗ 与失败原因（"哪个坏了、为什么"从状态栏搬到了这里）。
    fn render_js_source_list(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        ctx: &AppContext,
        sources: &[String],
        accent: Color,
        muted: Color,
    ) -> EmbeddedHits {
        let block = Block::default()
            .borders(PANEL_BORDERS)
            .border_style(Style::new().fg(if self.focus == SettingsFocus::JsSources {
                accent
            } else {
                crate::theme::border(ctx)
            }))
            .title(" JS 音源 [a添加/d移除/h检测] ");
        let inner = panel_inner(area);
        block.render(area, buf);
        let mut hits = EmbeddedHits::default();
        if inner.height == 0 {
            return hits;
        }

        // 命令行：标签与命中矩形由同一次排版产生。
        let loaded_sources = ctx.source_manager.js_source_count();
        let source_state = if loaded_sources > 0 {
            (
                format!("{loaded_sources} 个音源已就绪，按列表顺序解析"),
                crate::theme::green(ctx),
            )
        } else if sources.is_empty() {
            ("尚未导入 JS 音源".to_string(), crate::theme::yellow(ctx))
        } else {
            ("加载中或加载失败".to_string(), crate::theme::yellow(ctx))
        };
        hits.commands = render_command_row(
            inner,
            buf,
            JS_SOURCE_COMMANDS,
            (&source_state.0, source_state.1),
            muted,
        );

        // 第二行：音源检测中 / 逐项加载结果 / 音源体检摘要。
        if inner.height > 1 {
            let checking = ctx
                .source_health_checking
                .load(std::sync::atomic::Ordering::Relaxed);
            let status = ctx.js_source_status();
            let health = ctx.source_health.read().unwrap_or_else(|e| e.into_inner());
            // 优先级：正在体检 → 体检结果 → 逐项加载结果 → 尚未体检。
            // 体检是用户显式按 h 触发的，它的结果不能被加载摘要盖掉。
            let text = if checking {
                " 音源检测中…".to_string()
            } else if !health.is_empty() {
                let summary = health
                    .iter()
                    .map(|item| {
                        format!(
                            "{} {}{}ms",
                            item.name,
                            if item.ok { "✓" } else { "✗" },
                            item.latency_ms
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("  ");
                format!(" 检测结果: {summary}")
            } else if status.total > 0 {
                // 逐项加载状态：坏的是哪个、为什么（行内放不下时这里兜底）。
                let failed = status
                    .failures
                    .iter()
                    .map(|failure| format!("{}: {}", failure.name, failure.reason))
                    .collect::<Vec<_>>()
                    .join("  ✗ ");
                format!(
                    " 音源加载 {}/{} 成功{}",
                    status.loaded,
                    status.total,
                    if failed.is_empty() {
                        String::new()
                    } else {
                        format!("  ✗ {failed}")
                    }
                )
            } else {
                " 尚未检测音源".to_string()
            };
            Paragraph::new(Line::from(Span::styled(
                truncate_display(&text, inner.width as usize),
                Style::new().fg(if checking {
                    crate::theme::yellow(ctx)
                } else {
                    muted
                }),
            )))
            .render(Rect::new(inner.x, inner.y + 1, inner.width, 1), buf);
        }

        // 条目区：命令行 + 状态行之后，最后一行留给状态消息。
        let capacity = inner.height.saturating_sub(3) as usize;
        let first_row = inner.y.saturating_add(2);
        if sources.is_empty() {
            if capacity > 0 {
                Paragraph::new(Line::from(Span::styled(
                    " (无，按 a 添加音源 URL)",
                    Style::new().fg(crate::theme::yellow(ctx)),
                )))
                .render(Rect::new(inner.x, first_row, inner.width, 1), buf);
            }
            return hits;
        }
        self.selected_source = self.selected_source.min(sources.len().saturating_sub(1));
        let start = list_window_start(self.selected_source, sources.len(), capacity);
        let status = ctx.js_source_status();
        let max_url_chars = inner.width.saturating_sub(36) as usize;
        for (row, (index, url)) in sources
            .iter()
            .enumerate()
            .skip(start)
            .take(capacity)
            .enumerate()
        {
            let loaded = ctx.source_manager.js_source_name_for_origin(url);
            let failure = status
                .failures
                .iter()
                .find(|failure| failure.origin == *url);
            let (mark, mark_color, name) = match (&loaded, failure) {
                (Some(name), _) => ("✓", crate::theme::green(ctx), name.clone()),
                (None, Some(failure)) => (
                    "✗",
                    crate::theme::red(ctx),
                    format!("{}（{}）", failure.name, failure.reason),
                ),
                (None, None) => ("✗", crate::theme::yellow(ctx), "未加载".to_string()),
            };
            let selected = index == self.selected_source;
            let base = embedded_row_style(selected, ctx, accent);
            let file_state = if is_source_cached(url) {
                "cached"
            } else {
                "download"
            };
            let line = Line::from(vec![
                Span::styled(
                    format!(" {mark} {file_state:<8} "),
                    if selected {
                        base
                    } else {
                        Style::new().fg(mark_color)
                    },
                ),
                Span::styled(
                    truncate_display(&name, 18),
                    if selected {
                        base
                    } else {
                        Style::new().fg(crate::theme::text(ctx))
                    },
                ),
                Span::styled(
                    format!(" {}", shorten_source(url, max_url_chars.max(8))),
                    if selected {
                        base
                    } else {
                        Style::new().fg(muted)
                    },
                ),
            ]);
            let rect = Rect::new(inner.x, first_row + row as u16, inner.width, 1);
            hits.rows
                .push(render_embedded_row(buf, rect, index, line, None));
        }
        hits
    }

    /// 渲染"数据与本地库"分类里内嵌的本地音乐目录列表，返回命中账本。
    ///
    /// 本地音乐目录原本跟状态栏挤在下方管理区里；`Data`（数据与本地库）
    /// 是 7 个分类里唯一与"本地音乐库"同义的分类，因此并进这里。
    fn render_local_path_list(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        ctx: &AppContext,
        paths: &[String],
        accent: Color,
        muted: Color,
    ) -> EmbeddedHits {
        let block = Block::default()
            .borders(PANEL_BORDERS)
            .border_style(Style::new().fg(if self.focus == SettingsFocus::LocalPaths {
                accent
            } else {
                crate::theme::border(ctx)
            }))
            .title(" 本地音乐目录 [a添加/d移除/r重新扫描] ");
        let inner = panel_inner(area);
        block.render(area, buf);
        let mut hits = EmbeddedHits::default();
        if inner.height == 0 {
            return hits;
        }
        hits.commands = render_command_row(inner, buf, LOCAL_PATH_COMMANDS, ("", muted), muted);

        let capacity = inner.height.saturating_sub(3) as usize;
        let first_row = inner.y.saturating_add(2);
        if paths.is_empty() {
            if capacity > 0 {
                Paragraph::new(Line::from(Span::styled(
                    " (无，按 a 添加音乐目录)",
                    Style::new().fg(crate::theme::yellow(ctx)),
                )))
                .render(Rect::new(inner.x, first_row, inner.width, 1), buf);
            }
        } else {
            self.selected_local_path = self.selected_local_path.min(paths.len().saturating_sub(1));
            let start = list_window_start(self.selected_local_path, paths.len(), capacity);
            for (row, (index, path)) in paths
                .iter()
                .enumerate()
                .skip(start)
                .take(capacity)
                .enumerate()
            {
                let line = Line::from(Span::styled(
                    format!(" {path}"),
                    embedded_row_style(index == self.selected_local_path, ctx, accent),
                ));
                let rect = Rect::new(inner.x, first_row + row as u16, inner.width, 1);
                hits.rows
                    .push(render_embedded_row(buf, rect, index, line, None));
            }
        }

        // 底部显示歌曲数（最后一行留给状态消息，与其它内嵌列表一致）。
        if inner.height > 1 {
            let footer_y = inner.bottom().saturating_sub(1);
            if footer_y > inner.y {
                Paragraph::new(Line::from(Span::styled(
                    format!(
                        " 共 {} 首歌曲",
                        ctx.source_manager.local_source().song_count()
                    ),
                    Style::new().fg(muted),
                )))
                .render(Rect::new(inner.x, footer_y, inner.width, 1), buf);
            }
        }
        hits
    }

    /// 渲染"界面"分类里内嵌的状态栏字段列表，返回命中账本。
    ///
    /// 勾选框在渲染时就把自己的矩形记进账本（`checkbox`），
    /// 鼠标点勾选框 = 切换开关，点行的其它位置 = 拖拽排序。
    fn render_status_item_list(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        ctx: &AppContext,
        config: &lx_core::model::config::Config,
        accent: Color,
    ) -> EmbeddedHits {
        let block = Block::default()
            .borders(PANEL_BORDERS)
            .border_style(Style::new().fg(if self.focus == SettingsFocus::StatusBar {
                accent
            } else {
                crate::theme::border(ctx)
            }))
            .title(" 状态栏字段 [Enter/Space勾选 · Shift+↑↓排序] ");
        let inner = panel_inner(area);
        block.render(area, buf);
        let mut hits = EmbeddedHits::default();

        // 最后一行留给状态消息。
        let capacity = inner.height.saturating_sub(1) as usize;
        self.selected_status_item = self
            .selected_status_item
            .min(StatusBarItem::ALL.len().saturating_sub(1));
        if self.selected_status_item < self.status_item_scroll {
            self.status_item_scroll = self.selected_status_item;
        } else if capacity > 0 && self.selected_status_item >= self.status_item_scroll + capacity {
            self.status_item_scroll = self.selected_status_item + 1 - capacity;
        }
        self.status_item_scroll = self
            .status_item_scroll
            .min(StatusBarItem::ALL.len().saturating_sub(capacity.max(1)));

        for (row, (index, item)) in StatusBarItem::ALL
            .iter()
            .enumerate()
            .skip(self.status_item_scroll)
            .take(capacity)
            .enumerate()
        {
            let order = config
                .ui
                .status_bar_items
                .iter()
                .position(|candidate| candidate == item)
                .map(|position| position + 1);
            let text = format!(
                " [{}] {:>2}  {}",
                if order.is_some() { "x" } else { " " },
                order.map_or_else(|| "-".to_string(), |value| value.to_string()),
                status_bar_item_label(*item)
            );
            let rect = Rect::new(inner.x, inner.y + row as u16, inner.width, 1);
            hits.rows.push(render_embedded_row(
                buf,
                rect,
                index,
                Line::from(Span::styled(
                    text,
                    embedded_row_style(index == self.selected_status_item, ctx, accent),
                )),
                Some(STATUS_BAR_CHECKBOX_WIDTH),
            ));
        }
        hits
    }

    fn update_config(
        &mut self,
        ctx: &AppContext,
        update: impl FnOnce(&mut lx_core::model::config::Config),
    ) {
        let result = {
            let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
            update(&mut config);
            // 下载目录/分片等参数变化后立即生效，无需重启。
            ctx.downloads.sync_config(&config);
            crate::config::loader::save(&config, &ctx.config_path)
        };
        self.set_status(match result {
            Ok(()) => "设置已保存".to_string(),
            Err(error) => format!("保存设置失败: {}", error),
        });
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        // 本帧的页面矩形：窄屏判定与设置页菜单边界都读它（输入处理拿不到 area）。
        self.last_area = area;
        self.sync_remote_window();
        let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
        let sources = &config.source.js_sources;
        let local_paths = &config.local_music.paths;
        let accent = crate::theme::accent(ctx);
        let muted = crate::theme::muted(ctx);

        // 1. 构造设置行：每一行显式声明分类。
        let palette = RowPalette { accent, muted };
        let mut rows = SettingsRows::new();
        let mut inputs = RowInputs::from_context(ctx);
        inputs.cover_cache_label = self.cover_cache_label.clone();
        inputs.remote_cache_label = self.remote_cache_label.clone();
        build_settings_rows(
            &mut rows,
            &config,
            &inputs,
            palette,
            self.cover_capabilities,
        );

        // 2. 布局：整个设置页只有一个面板 ——
        //    分类栏（宽屏）/ 分类列表（窄屏）+ 该分类的内容。
        //    分类栏与内容、设置行与内嵌列表之间各留 1 格 gutter 给分界线：
        //    分界线占自己那一格，不压任何面板的内容或边框。
        let wide = category_sidebar_visible(area.width);
        let narrow_categories = !wide && self.narrow_pane == NarrowPane::Categories;
        let show_rows = wide || !narrow_categories;
        let embedded = embedded_list_for(self.category);
        let embedded_fixed = self.embedded_ratio_is_fixed();
        let embedded_needed = if show_rows {
            embedded.map(|list| {
                let items = embedded_items(
                    list,
                    sources.len(),
                    local_paths.len(),
                    self.qr_login_sources(ctx).len(),
                );
                embedded_needed_rows_for(list, items, embedded_fixed)
            })
        } else {
            None
        };
        let panes = settings_panes(
            area,
            wide,
            show_rows,
            embedded_needed,
            self.effective_embedded_ratio(),
            self.effective_categories_width(),
        );
        // 渲染与命中同源：悬停/拖拽抓取都只认这一份矩形。
        self.last_panes = panes;

        // 3. 记录元数据账本并让行光标合法（换分类后落到新分类第一行）。
        self.row_metas = rows.metas();
        self.row_cursor = ensure_row_cursor(&self.row_metas, self.category, self.row_cursor);
        let visible_rows = rows.rows_in(self.category);

        // 4. 面板标题里的操作提示按当前视图给出。
        let hint = if narrow_categories {
            "↑/↓分类 · Enter进入 · ←/→切换"
        } else if panes.embedded.is_some() {
            "←/→分类 · ↑/↓选择 · Enter确认 · ↓到底进入列表"
        } else {
            "←/→分类 · ↑/↓选择 · Enter确认"
        };
        // 主题入口所在的分类：标题里直接带上"主题: <当前主题>"。
        // 用户不必猜"换主题的选项埋在哪个分类里"，标题上就能看见。
        let theme_hint = if visible_rows
            .iter()
            .any(|row| row.meta.plan == RowPlan::Menu(SettingsEnumPicker::Theme))
        {
            format!(
                " · 主题: {} (p 循环)",
                crate::theme::skin_label(&config.theme.name)
            )
        } else {
            String::new()
        };
        let options_block = Block::default()
            .borders(PANEL_BORDERS)
            .border_style(Style::new().fg(if self.focus == SettingsFocus::Options {
                accent
            } else {
                crate::theme::border(ctx)
            }))
            .title(format!(
                " 设置 · {}  [{hint}{theme_hint}] ",
                self.category.label()
            ));
        options_block.render(area, buf);

        // 5. 分类栏 / 分类列表 + 设置行：渲染 Rect 与命中 Rect 同源。
        //    分类栏与右侧内容面板同一套边框 + 标题（内区由 `panel_inner` 扣掉，
        //    因此命中账本不含边框那一列）。
        self.category_hits = match panes.categories {
            Some(category_area) => render_setting_categories(
                &SETTINGS_CATEGORIES,
                category_area,
                buf,
                self.category,
                accent,
                crate::theme::selection_fg(ctx),
                muted,
                if narrow_categories {
                    accent
                } else {
                    crate::theme::border(ctx)
                },
            ),
            None => Vec::new(),
        };
        self.row_hits = match panes.rows {
            Some(rows_area) => render_setting_rows(
                &visible_rows,
                rows_area,
                buf,
                self.row_cursor,
                self.hover_row,
                accent,
                crate::theme::selection_fg(ctx),
            ),
            None => Vec::new(),
        };

        // 6. 分类内容里内嵌的管理列表：原来那四个下方面板各自并进它所属的分类。
        //    一次只有一个可见，因此渲染时写入同一份命中账本 `embedded_hits`。
        self.embedded_area = panes.embedded;
        self.embedded_hits = match (embedded, panes.embedded) {
            (Some(SettingsFocus::JsSources), Some(list_area)) => {
                self.render_js_source_list(list_area, buf, ctx, sources, accent, muted)
            }
            (Some(SettingsFocus::LocalPaths), Some(list_area)) => {
                self.render_local_path_list(list_area, buf, ctx, local_paths, accent, muted)
            }
            (Some(SettingsFocus::StatusBar), Some(list_area)) => {
                self.render_status_item_list(list_area, buf, ctx, &config, accent)
            }
            (Some(SettingsFocus::QrLogin), Some(list_area)) => {
                let login_sources = self.qr_login_sources(ctx);
                self.render_qr_login_panel(list_area, buf, ctx, accent, muted, &login_sources)
            }
            _ => EmbeddedHits::default(),
        };

        // 6.5 两条分界线：坐标与拖拽命中同一份 `panes`，画在自己那一格 gutter 上。
        self.render_resize_dividers(&panes, buf, accent, crate::theme::surface1(ctx));

        // 7. 状态消息画在当前聚焦区域的底部。
        let focused_inner = if self.focus == SettingsFocus::Options {
            panes.rows
        } else {
            panes.embedded
        }
        .unwrap_or_else(|| panel_inner(area));
        // 状态消息 3 秒后自动消失：此前会一直压住面板最后一行，直到下次操作。
        let status_msg_visible = self
            .status_msg_at
            .is_some_and(|at| at.elapsed() < STATUS_MSG_TIMEOUT);
        if let Some(ref msg) = self.status_msg
            && status_msg_visible
            && focused_inner.height > 1
        {
            Paragraph::new(Line::from(Span::styled(
                format!(" {}", msg),
                Style::new().fg(crate::theme::yellow(ctx)),
            )))
            .render(
                Rect::new(
                    focused_inner.x,
                    focused_inner.bottom().saturating_sub(1),
                    focused_inner.width,
                    1,
                ),
                buf,
            );
        }

        // ── 输入浮层 ──
        //
        // 六个单行输入共用同一个渲染函数：既去掉重复代码，也保证光标（输入法
        // 候选框的定位依据）在每一处都落在同一个位置——文本插入点。
        if self.local_path_mode {
            render_input_overlay(
                area,
                buf,
                ctx,
                "输入本地音乐目录路径",
                &self.local_path_input,
            );
        }

        if self.input_mode {
            render_input_overlay(
                area,
                buf,
                ctx,
                "输入 JS 音源 URL 或本地路径",
                &self.input_url,
            );
        }

        if self.proxy_input_mode {
            render_input_overlay(
                area,
                buf,
                ctx,
                "输入代理地址，留空表示关闭",
                &self.proxy_input,
            );
        }

        if self.audio_device_input_mode {
            render_input_overlay(
                area,
                buf,
                ctx,
                "输入 libmpv 音频设备名，Enter 保存",
                &self.audio_device_input,
            );
        }

        if self.playlist_import_mode {
            render_input_overlay(
                area,
                buf,
                ctx,
                "输入 M3U/LX Music/网易云歌单路径，Enter 导入",
                &self.playlist_import_input,
            );
        }

        // 下载目录 / 文件名模板此前只写 status_msg、没有可见输入框：
        // 用户看不到自己打了什么，输入法的候选框也无处可依附。
        if let Some(target) = self.download_input_target {
            let title = match target {
                DownloadInputTarget::Dir => "输入下载目录，Enter 保存",
                DownloadInputTarget::Template => "输入文件名模板，Enter 保存",
            };
            render_input_overlay(area, buf, ctx, title, &self.download_input);
        }

        // 枚举取值菜单画在最上层（设置页自己持有，不占用主循环的 `song_menu` 槽位）。
        if let Some(menu) = self.menu.as_ref() {
            menu.render(area, buf, ctx);
        }

        // 远程歌单窗口比菜单更上层：它打开时菜单不可能同时开着。
        if let Some(window) = self.remote_window.as_mut() {
            window.render(area, buf, ctx);
        }
    }

    /// 打开「网易云远程歌单」窗口（用当前缓存内容）。
    ///
    /// 允许缓存为空时打开：窗口里会给出"还没有缓存任何远程歌单"的引导，
    /// 比按了没反应更好。
    pub fn open_remote_collections_window(&mut self) {
        let collections = crate::pages::components::remote_collections::account_collections();
        self.remote_window_generation = crate::remote_cache::generation();
        match self.remote_window.as_mut() {
            Some(window) => window.refresh(collections),
            None => {
                self.remote_window = Some(RemoteCollectionsWindow::new(collections));
            }
        }
    }

    /// 窗口开着时，远程刷新完成（缓存 generation 变化）要就地更新列表，
    /// 否则用户得关掉再开才看得到刚拉回来的歌单。
    fn sync_remote_window(&mut self) {
        if self.remote_window.is_none() {
            return;
        }
        let generation = crate::remote_cache::generation();
        if generation == self.remote_window_generation {
            return;
        }
        self.remote_window_generation = generation;
        let collections = crate::pages::components::remote_collections::account_collections();
        if let Some(window) = self.remote_window.as_mut() {
            window.refresh(collections);
        }
    }

    /// 兜底取消进行中的拖拽会话（两条分界线 + 状态栏条目拖拽排序）。
    ///
    /// 触发场景与其它页面一致：鼠标在内容区外松开、键盘切页、终端 resize。
    pub fn abort_drag_sessions(&mut self) {
        self.status_drag_target = None;
        self.splitter.cancel();
        self.hover_divider = None;
    }

    /// 页面在 `ui.pane_ratios` 里的 key。
    pub fn pane_page_key(&self) -> &'static str {
        SETTINGS_PAGE_KEY
    }

    /// 指针移动：更新分界线悬停高亮，返回高亮是否变化（主循环据此决定重绘）。
    ///
    /// 主循环的鼠标分支里 `Moved` 只服务底栏（更早 `continue`），页面收不到它，
    /// 因此设置页的分界线悬停单独走这一根线（见 `run_app` 的鼠标分支）。
    /// 命中只用最近一次**画出来**的 [`SettingsPanes`]。
    pub fn update_divider_hover(&mut self, position: Position) -> bool {
        let hovered = self
            .last_area
            .contains(position)
            .then(|| self.last_panes.resize_target_at(position))
            .flatten();
        if hovered == self.hover_divider {
            return false;
        }
        self.hover_divider = hovered;
        true
    }

    /// 从 Config 恢复用户拖出来的分类栏宽度与内嵌列表高度份额（构造后调用一次）。
    /// 把分类栏宽度与内嵌列表高度恢复成内置默认（与 `apply_pane_ratios` 对称）。
    ///
    /// `embedded_ratio_fixed` 也要复位：它表示"用户拖过、高度按比例给足"，
    /// 复位后重新回到"够用就好"的自动高度。
    pub fn reset_pane_ratios(&mut self) {
        self.embedded_ratio = EMBEDDED_RATIO_DEFAULT;
        self.embedded_ratio_fixed = false;
        self.categories_width = CATEGORY_SIDEBAR_WIDTH;
        self.splitter.cancel();
    }

    pub fn apply_pane_ratios(&mut self, ratios: &std::collections::HashMap<String, f32>) {
        if let Some(value) = ratios.get(SETTINGS_EMBEDDED_RATIO_KEY).copied() {
            self.embedded_ratio = clamp_ratio(value, EMBEDDED_RATIO_MIN, EMBEDDED_RATIO_MAX);
            // 存过值 = 用户拖过：高度按比例给足，不再"够用就好"。
            self.embedded_ratio_fixed = true;
        }
        if let Some(value) = ratios.get(SETTINGS_CATEGORIES_RATIO_KEY).copied() {
            self.categories_width = columns_from_value(value);
        }
    }

    /// 帧内生效的分类栏宽度（拖拽预览优先于已提交值）。
    fn effective_categories_width(&self) -> u16 {
        columns_from_value(self.splitter.effective(
            &SettingsResizeTarget::CategoriesWidth,
            self.categories_width as f32,
        ))
    }

    /// 帧内生效的内嵌列表高度份额（拖拽预览优先于已提交值）。
    fn effective_embedded_ratio(&self) -> f32 {
        clamp_ratio(
            self.splitter
                .effective(&SettingsResizeTarget::EmbeddedHeight, self.embedded_ratio),
            EMBEDDED_RATIO_MIN,
            EMBEDDED_RATIO_MAX,
        )
    }

    /// 内嵌列表是否按比例给足高度（用户拖过、或正在拖这条线）。
    fn embedded_ratio_is_fixed(&self) -> bool {
        self.embedded_ratio_fixed
            || self.splitter.dragging() == Some(&SettingsResizeTarget::EmbeddedHeight)
    }

    /// 按下分界线：开始一次拖拽会话。
    ///
    /// 内嵌列表此前可能一直"够用就好"（实际高度小于比例给出的份额）。第一次抓
    /// 这条线时先把比例对齐到**当前**分界线所在的位置：否则会话一开始（高度改
    /// 按比例给足）那条线就会自己跳一下。
    fn begin_resize(
        &mut self,
        panes: &SettingsPanes,
        target: SettingsResizeTarget,
        position: Position,
    ) {
        if target == SettingsResizeTarget::EmbeddedHeight
            && !self.embedded_ratio_is_fixed()
            && let Some(ratio) = panes.embedded_ratio_at(position.y)
        {
            self.embedded_ratio = ratio;
        }
        self.splitter
            .begin(target, self.committed_resize_value(target));
    }

    /// 该分界线已提交的值（开始拖拽时取初值）。
    fn committed_resize_value(&self, target: SettingsResizeTarget) -> f32 {
        match target {
            SettingsResizeTarget::CategoriesWidth => self.categories_width as f32,
            SettingsResizeTarget::EmbeddedHeight => self.embedded_ratio,
        }
    }

    /// 拖拽中：指针 → 预览值（只改内存里的预览，不落盘）。
    fn update_resize_preview(
        &mut self,
        panes: &SettingsPanes,
        target: SettingsResizeTarget,
        position: Position,
    ) {
        let preview = match target {
            SettingsResizeTarget::CategoriesWidth => panes
                .categories_width_at(position.x)
                .map(|width| width as f32),
            SettingsResizeTarget::EmbeddedHeight => panes.embedded_ratio_at(position.y),
        };
        if let Some(value) = preview {
            self.splitter.drag(value);
        }
    }

    /// 鼠标抬起：结束会话、写内存，返回要落盘的 `(ratio_key, value)`。
    fn commit_resize(&mut self) -> Option<(&'static str, f32)> {
        let (target, value) = self.splitter.commit()?;
        Some(match target {
            SettingsResizeTarget::CategoriesWidth => {
                let width = columns_from_value(value);
                self.categories_width = width;
                (SETTINGS_CATEGORIES_RATIO_KEY, width as f32)
            }
            SettingsResizeTarget::EmbeddedHeight => {
                let ratio = clamp_ratio(value, EMBEDDED_RATIO_MIN, EMBEDDED_RATIO_MAX);
                self.embedded_ratio = ratio;
                // 拖过一次之后，高度就按用户给的比例走。
                self.embedded_ratio_fixed = true;
                (SETTINGS_EMBEDDED_RATIO_KEY, ratio)
            }
        })
    }

    /// 鼠标抬起 → 落盘动作（沿用 `CommitPaneRatio` 的写法）。
    fn commit_resize_action(&mut self) -> AppAction {
        match self.commit_resize() {
            Some((ratio_key, ratio)) => AppAction::CommitPaneRatio {
                page_key: SETTINGS_PAGE_KEY.to_string(),
                ratio_key: ratio_key.to_string(),
                ratio,
            },
            None => AppAction::None,
        }
    }

    /// 画两条分界线：坐标来自 [`SettingsPanes::dividers`]，与拖拽命中同一份。
    ///
    /// 悬停或正在拖拽的那条线用 `surface1` 底 + BOLD 加亮；空间不足时分界线
    /// 根本不存在，这里自然什么都不画（绝不覆盖内容）。
    fn render_resize_dividers(
        &self,
        panes: &SettingsPanes,
        buf: &mut Buffer,
        accent: Color,
        highlight_bg: Color,
    ) {
        for (target, hit) in panes.dividers() {
            let highlighted =
                self.splitter.dragging() == Some(&target) || self.hover_divider == Some(target);
            let style = if highlighted {
                Style::new()
                    .fg(accent)
                    .bg(highlight_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(accent)
            };
            match hit.axis {
                SplitAxis::Vertical => {
                    for y in hit.span.0..hit.span.1 {
                        buf.set_string(hit.divider, y, "│", style);
                    }
                }
                SplitAxis::Horizontal => {
                    for x in hit.span.0..hit.span.1 {
                        buf.set_string(x, hit.divider, "─", style);
                    }
                }
            }
        }
    }

    /// 鼠标在分类栏上的唯一入口：切分类（窄屏同时进入该分类的设置项）。
    fn click_category(&mut self, category: SettingsCategory) {
        self.set_category(category);
        if !self.wide_layout() {
            self.narrow_pane = NarrowPane::Rows;
        }
    }

    /// 选中内嵌列表的第 `index` 条（鼠标点击 / 滚动共用）。
    ///
    /// 焦点一并交给该列表：内嵌列表只有在它所属的分类里才可见，
    /// 因此"选中"与"获得方向键"是同一件事。
    fn select_embedded_row(&mut self, index: usize) {
        match embedded_list_for(self.category) {
            Some(SettingsFocus::JsSources) => {
                self.focus = SettingsFocus::JsSources;
                self.selected_source = index;
            }
            Some(SettingsFocus::LocalPaths) => {
                self.focus = SettingsFocus::LocalPaths;
                self.selected_local_path = index;
            }
            Some(SettingsFocus::StatusBar) => {
                self.focus = SettingsFocus::StatusBar;
                self.selected_status_item = index;
            }
            Some(SettingsFocus::QrLogin) => {
                self.focus = SettingsFocus::QrLogin;
                self.qr_login_source_index = index;
            }
            Some(SettingsFocus::Options) | None => {}
        }
    }

    /// 内嵌列表当前的选中项（没有内嵌列表时为 0）。
    fn embedded_selection(&self) -> usize {
        match embedded_list_for(self.category) {
            Some(SettingsFocus::JsSources) => self.selected_source,
            Some(SettingsFocus::LocalPaths) => self.selected_local_path,
            Some(SettingsFocus::StatusBar) => self.selected_status_item,
            Some(SettingsFocus::QrLogin) => self.qr_login_source_index,
            Some(SettingsFocus::Options) | None => 0,
        }
    }

    /// 内嵌列表的条目数（滚动用；渲染与命中共用 `embedded_items` 的取值口径）。
    fn embedded_len(&self, ctx: &AppContext) -> usize {
        let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
        let Some(list) = embedded_list_for(self.category) else {
            return 0;
        };
        embedded_items(
            list,
            config.source.js_sources.len(),
            config.local_music.paths.len(),
            self.qr_login_sources(ctx).len(),
        )
    }

    /// 鼠标点击内嵌列表条目：选中它，右键/勾选框做该列表的"直接操作"。
    fn click_embedded_row(
        &mut self,
        hit: EmbeddedRowHit,
        position: Position,
        right_click: bool,
        ctx: &AppContext,
    ) -> AppAction {
        self.select_embedded_row(hit.index);
        match embedded_list_for(self.category) {
            // 右键音源 / 本地目录 = 移除它（与旧面板一致，仍走二次确认分支）。
            Some(SettingsFocus::JsSources) if right_click => {
                self.run_embedded_command(EmbeddedCommand::RemoveJsSource, ctx)
            }
            Some(SettingsFocus::LocalPaths) if right_click => {
                self.run_embedded_command(EmbeddedCommand::RemoveLocalPath, ctx)
            }
            Some(SettingsFocus::StatusBar) => {
                // 勾选框矩形来自渲染账本；点勾选框（或右键）即切换，点行的其它位置开始拖拽排序。
                if right_click || hit.checkbox_at(position) {
                    self.toggle_status_bar_item(ctx);
                } else {
                    self.status_drag_target = Some(hit.index);
                }
                AppAction::None
            }
            _ => AppAction::None,
        }
    }

    /// 鼠标在内嵌列表里滚动：焦点交给该列表并移动它的选中项。
    fn scroll_embedded_list(&mut self, forward: bool, ctx: &AppContext) {
        let len = self.embedded_len(ctx);
        let next = step_index(self.embedded_selection(), len, forward);
        self.select_embedded_row(next);
    }

    /// `resolver` 与其它页面保持同一签名：设置页的鼠标路径已经不需要页面级
    /// 键位了（命令行按钮改走 `EmbeddedCommand`），因此这里不使用它。
    pub fn handle_mouse(
        &mut self,
        event: MouseEvent,
        area: Rect,
        ctx: &AppContext,
        _resolver: &KeybindingResolver,
    ) -> AppAction {
        // 只有"按下"才算用户主动离开输入态：否则鼠标一移动就会退出输入模式，
        // 后续按键转入全局/选项键位分发（曾经因此误触"保留播放状态"开关）。
        if self.any_input_active() && matches!(event.kind, MouseEventKind::Down(_)) {
            self.input_mode = false;
            self.local_path_mode = false;
            self.proxy_input_mode = false;
            self.audio_device_input_mode = false;
            self.playlist_import_mode = false;
            self.download_input_target = None;
        }
        // 远程歌单窗口也是模态的：打开期间鼠标只在窗口内生效，
        // 点窗口外等价于关窗（与取值菜单一致）。
        if self.remote_window.is_some() {
            let popup = RemoteCollectionsWindow::window_rect(self.last_area);
            let inside = popup.contains((event.column, event.row).into());
            if !inside && matches!(event.kind, MouseEventKind::Down(_)) {
                self.remote_window = None;
            }
            return AppAction::None;
        }
        // 设置页自己的枚举取值菜单是模态的：打开期间鼠标事件先给它。
        if self.menu.is_some() {
            let bounds = self.last_area;
            let outcome = self
                .menu
                .as_mut()
                .expect("menu checked above")
                .handle_mouse(event, bounds);
            return self.finish_menu_outcome(outcome, ctx);
        }

        let position = Position::new(event.column, event.row);
        // 分界线几何只认最近一次**画出来**的那一份（渲染与命中同源）。
        let panes = self.last_panes;

        // 拖拽优先：先判拖拽状态，再派发点击；落在分界线那一格上的事件也不穿透。
        match settings_mouse_dispatch(
            self.splitter.dragging().copied(),
            &panes.dividers(),
            event.kind,
            position,
        ) {
            SettingsMouseDispatch::BeginResize(target) => {
                self.begin_resize(&panes, target, position);
                return AppAction::None;
            }
            SettingsMouseDispatch::PreviewResize(target) => {
                self.update_resize_preview(&panes, target, position);
                return AppAction::None;
            }
            SettingsMouseDispatch::CommitResize => return self.commit_resize_action(),
            SettingsMouseDispatch::Swallow => return AppAction::None,
            SettingsMouseDispatch::Dispatch => {}
        }

        match event.kind {
            MouseEventKind::Moved => {
                // 悬停高亮：与点击一样只查渲染时记下的行矩形账本。
                self.hover_row = row_hit_at(&self.row_hits, position);
                self.hover_divider = panes.resize_target_at(position);
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let forward = matches!(event.kind, MouseEventKind::ScrollDown);
                if self
                    .embedded_area
                    .is_some_and(|embedded| embedded.contains(position))
                {
                    // 内嵌列表整块区域：滚轮翻它的条目（焦点一并交给它）。
                    self.scroll_embedded_list(forward, ctx);
                } else if area.contains(position) {
                    // 面板内的其余区域：滚轮 = 设置行光标（宽屏 / 已进入分类），
                    // 窄屏分类列表视图下 = 换分类。
                    self.focus = SettingsFocus::Options;
                    if self.wide_layout() || self.narrow_pane == NarrowPane::Rows {
                        self.move_row_cursor(forward);
                    } else {
                        self.set_category(if forward {
                            self.category.next()
                        } else {
                            self.category.previous()
                        });
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                // 状态栏字段拖拽排序：只认渲染时记下的条目账本。
                if let Some(hit) = self.embedded_hits.row_at(position)
                    && embedded_list_for(self.category) == Some(SettingsFocus::StatusBar)
                {
                    self.focus = SettingsFocus::StatusBar;
                    if self.status_drag_target.is_some()
                        && self.status_drag_target != Some(hit.index)
                    {
                        self.move_status_bar_item_to(ctx, hit.index);
                        self.status_drag_target = Some(hit.index);
                    }
                }
            }
            MouseEventKind::Down(button)
                if matches!(button, MouseButton::Left | MouseButton::Right) =>
            {
                let right_click = button == MouseButton::Right;
                self.status_drag_target = None;
                if !right_click {
                    // 设置行命中只查渲染时记下的矩形账本（渲染 Rect 与命中 Rect 同源）。
                    if let Some(id) = row_hit_at(&self.row_hits, position) {
                        self.focus = SettingsFocus::Options;
                        self.row_cursor = Some(id);
                        return self.activate_settings_row(id, ctx);
                    }
                    // 分类栏点击：切分类（窄屏单栏同时进入该分类的设置项）。
                    if let Some(category) = category_hit_at(&self.category_hits, position) {
                        self.click_category(category);
                        return AppAction::None;
                    }
                } else if !self.wide_layout()
                    && self.narrow_pane == NarrowPane::Rows
                    && area.contains(position)
                {
                    // 窄屏单栏没有 Esc 的鼠标等价物：右键 = 返回分类列表。
                    self.focus = SettingsFocus::Options;
                    self.narrow_pane = NarrowPane::Categories;
                    return AppAction::None;
                }

                // 内嵌管理列表：命令按钮与条目都只查渲染时记下的账本。
                // 按钮字符经 `embedded_command` 变成操作 —— 与列表聚焦时按同一个
                // 字母键走的是**同一条**路径（见 `run_embedded_command`）。
                if let Some(key) = self.embedded_hits.command_at(position)
                    && let Some(list) = embedded_list_for(self.category)
                    && let Some(command) = embedded_command(list, key)
                {
                    return self.run_embedded_command(command, ctx);
                }
                if let Some(hit) = self.embedded_hits.row_at(position) {
                    return self.click_embedded_row(hit, position, right_click, ctx);
                }
                // 点内嵌面板的空白处（标题 / 命令行以外的行）：把焦点交给它。
                if self
                    .embedded_area
                    .is_some_and(|embedded| embedded.contains(position))
                {
                    let selection = self.embedded_selection();
                    self.select_embedded_row(selection);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.status_drag_target = None;
            }
            _ => {}
        }
        AppAction::None
    }
}

/// 渲染一个居中的单行输入浮层，并把终端光标钉在文本插入点。
///
/// 输入法的候选框跟随**终端光标**。ratatui 差分渲染下若不显式设置
/// `Frame::cursor_position`，光标只会被隐藏、停在"本帧最后一个变化的单元格"上，
/// 候选框就会在输入框和状态栏之间来回跳（issue #42）。所以每个输入浮层
/// 都要登记插入点，主循环据此设置光标位置（见 `ui_cursor`）。
fn render_input_overlay(
    area: Rect,
    buf: &mut ratatui::buffer::Buffer,
    ctx: &AppContext,
    title: &str,
    text: &str,
) {
    use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};

    let width = area.width.saturating_sub(4).min(74);
    let input_area = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(3) / 2,
        width,
        3.min(area.height),
    );
    Clear.render(input_area, buf);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(crate::theme::green(ctx)))
        .title(title);
    let inner = block.inner(input_area);
    block.render(input_area, buf);
    Paragraph::new(Line::from(text)).render(inner, buf);
    // 光标由终端绘制，因此这里不再拼软件光标字符。
    crate::ui_cursor::request_after(inner, "", text);
}

fn list_window_start(selected: usize, len: usize, rows: usize) -> usize {
    if len == 0 || rows == 0 {
        0
    } else {
        selected.min(len - 1).saturating_sub(rows.saturating_sub(1))
    }
}

/// 命令行排版：整行文本 + 每个命令的命中矩形（**同一次排版**产生）。
///
/// 渲染与鼠标命中都从这里取，因此标签改了不会出现"画在这里、点在那里"。
/// 每个命令形如 `[a] 添加`，命中矩形正好覆盖它的标签宽度（行首有一个空格）。
fn command_row_layout(inner: Rect, commands: &[(&str, char)]) -> (String, Vec<(Rect, char)>) {
    let row = Rect::new(inner.x, inner.y, inner.width, 1);
    let mut text = String::from(" ");
    let mut hits = Vec::new();
    let mut x = inner.x.saturating_add(1);
    for (label, key) in commands {
        let width = UnicodeWidthStr::width(*label) as u16;
        if let Some(rect) = clip_to_row(Rect::new(x, inner.y, width, 1), row) {
            hits.push((rect, *key));
        }
        text.push_str(label);
        text.push_str("  ");
        x = x.saturating_add(width).saturating_add(2);
    }
    (text, hits)
}

/// 把行内矩形夹回它所在的行（窄终端下标签可能排到行外）。
fn clip_to_row(rect: Rect, row: Rect) -> Option<Rect> {
    let clipped = rect.intersection(row);
    (clipped.width > 0).then_some(clipped)
}

/// 渲染内嵌列表的命令行：命令用 `muted`，后缀（加载状态等）用它自己的颜色。
///
/// 返回命中账本；绘制坐标与账本由 `command_row_layout` 一次算出。
fn render_command_row(
    inner: Rect,
    buf: &mut Buffer,
    commands: &[(&str, char)],
    suffix: (&str, Color),
    muted: Color,
) -> Vec<(Rect, char)> {
    let (text, hits) = command_row_layout(inner, commands);
    Paragraph::new(Line::from(vec![
        Span::styled(text, Style::new().fg(muted)),
        Span::styled(suffix.0.to_string(), Style::new().fg(suffix.1)),
    ]))
    .render(Rect::new(inner.x, inner.y, inner.width, 1), buf);
    hits
}

/// 渲染内嵌列表的一条条目行，并返回它的命中账本。
fn render_embedded_row(
    buf: &mut Buffer,
    rect: Rect,
    index: usize,
    line: Line<'static>,
    checkbox_width: Option<u16>,
) -> EmbeddedRowHit {
    Paragraph::new(line).render(rect, buf);
    EmbeddedRowHit {
        rect,
        checkbox: checkbox_width.map(|width| Rect::new(rect.x, rect.y, width.min(rect.width), 1)),
        index,
    }
}

/// 内嵌列表条目的整行选中样式（与设置行、其它列表一致）。
fn embedded_row_style(selected: bool, ctx: &AppContext, accent: Color) -> Style {
    if selected {
        Style::new()
            .fg(crate::theme::selection_fg(ctx))
            .bg(accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(crate::theme::text(ctx))
    }
}

/// 列表选中项按 `forward` 前进一步（到头即停）。
fn step_index(selected: usize, len: usize, forward: bool) -> usize {
    if len == 0 {
        return 0;
    }
    if forward {
        (selected + 1).min(len - 1)
    } else {
        selected.saturating_sub(1)
    }
}

/// 设置页仍然独占的页面级动作：只有列表导航（内嵌管理列表的 ↑/↓）。
///
/// 逐行快捷键连同它们的 `Settings*` 页面绑定一起删除了：那些动作即便还留在
/// 用户配置里，也不再被设置页拦截（设置页会把它让给全局分发），因此不会出现
/// "被吃掉却没人处理"的死键。
fn settings_action_is_page_owned(action: Action) -> bool {
    matches!(action, Action::ListSelectUp | Action::ListSelectDown)
}

fn status_bar_item_label(item: StatusBarItem) -> &'static str {
    match item {
        StatusBarItem::State => "播放状态",
        StatusBarItem::Source => "当前音源",
        StatusBarItem::Sort => "页面排序",
        StatusBarItem::Song => "歌曲名称",
        StatusBarItem::Time => "播放时间",
        StatusBarItem::Volume => "音量",
        StatusBarItem::PlayMode => "播放模式",
        StatusBarItem::Quality => "音质",
        StatusBarItem::Queue => "队列位置",
        StatusBarItem::JsSourceState => "JS 音源状态",
    }
}

fn reorder_status_bar_items(
    items: &mut Vec<StatusBarItem>,
    item: StatusBarItem,
    target: StatusBarItem,
) -> Option<usize> {
    let item_position = items.iter().position(|candidate| *candidate == item)?;
    let target_position = items.iter().position(|candidate| *candidate == target)?;
    if item_position == target_position {
        return Some(item_position);
    }
    items.remove(item_position);
    let insertion = target_position.min(items.len());
    items.insert(insertion, item);
    Some(insertion)
}

/// 在更新配置项后更新这些常量!
///
/// 最长按键提示的显示宽度 (组合键在界面中使用 C/S/A 缩写)
const KEY_COLUMN_WIDTH: usize = 7;

/// 绝大多数设置行的激活键：键位列对它只留占位符，不逐行重复。
const DEFAULT_ACTIVATION_KEY: &str = "Enter";

/// 最长标签的显示宽度 (当前为 保留播放状态)
const LABEL_COLUMN_WIDTH: usize = 12;

/// 组装一行设置项：按键提示、标签与取值分别占固定宽度的列。
///
/// 键位列只在**这一行有专属快捷键**时才写出来（当前只有「封面协议」的
/// `Shift+P`）：每行都印一遍 `[Enter]` 只是噪声，"Enter 激活"这条共性已经写在
/// 面板标题里（见 `SettingsPage::render`）。
fn setting_row(label: &str, value: Span<'static>, key: &str, muted: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!(" {} ", pad_display(&key_label(key), KEY_COLUMN_WIDTH)),
            Style::new().fg(muted),
        ),
        Span::raw(pad_display(label, LABEL_COLUMN_WIDTH)),
        Span::raw(" "),
        value,
    ])
}

/// 键位列的显示文本。
///
/// 删掉逐行快捷键之后，绝大多数行的激活键就是 `Enter`（`RowPlan::key_hint`
/// 原样返回 `Enter`），键位列对它只留一个安静的圆点，不再逐行重复广告同一个键；
/// 只有刻意保留的页面级组合键会走到这里做缩写（`Shift+P` → `S+P`）。
/// 空键位（只读说明行）显示为 `-`。
fn compact_key_label(key: &str) -> String {
    if key.is_empty() {
        return "-".to_string();
    }
    if key == DEFAULT_ACTIVATION_KEY {
        return String::new();
    }
    key.replace("Ctrl+", "C+")
        .replace("Shift+", "S+")
        .replace("Alt+", "A+")
}

/// 键位列单元格：Enter 激活的行只留一个安静的圆点（不带方括号——方括号
/// 会被读成"可输入的字段"，整列都是时视觉噪音很大）；刻意保留的组合键
/// 才用 `[S+P]` 这样的方括号形式标出"这不是 Enter"。
fn key_label(key: &str) -> String {
    let compact = compact_key_label(key);
    if compact.is_empty() {
        "·".to_string()
    } else {
        format!("[{compact}]")
    }
}

fn setting_line(label: &str, value: bool, key: &str, accent: Color, muted: Color) -> Line<'static> {
    setting_row(
        label,
        Span::styled(
            if value { "✓" } else { "○" },
            Style::new().fg(if value { accent } else { muted }),
        ),
        key,
        muted,
    )
}

fn setting_value_line(
    label: &str,
    value: &str,
    key: &str,
    accent: Color,
    muted: Color,
) -> Line<'static> {
    setting_row(
        label,
        Span::styled(value.to_string(), Style::new().fg(accent)),
        key,
        muted,
    )
}

/// 一次性动作行使用轻量的 `›` 前缀，与可编辑的当前值区分开。
///
/// 不把动作做成按钮：设置页仍保持统一的整行 Enter / 鼠标激活模型，
/// 这里只增加一个视觉语义锚点。
fn setting_action_line(
    label: &str,
    value: &str,
    key: &str,
    accent: Color,
    _muted: Color,
) -> Line<'static> {
    setting_row(
        label,
        Span::styled(format!("› {value}"), Style::new().fg(accent)),
        key,
        _muted,
    )
}

// ── 设置行的纯逻辑（不依赖 AppContext，便于回归测试）────────────────────

/// 「封面协议」这一行的键位列文案：**真正生效**的组合键。
///
/// 裸 `P` 归「账号与扫码」面板（见 `is_accounts_panel_key`），封面协议只能靠
/// `Shift+P`。键位列以前写的是 `P`，用户照着按下去只会跳到账号分类 ——
/// 于是"封面协议根本改不了"。这里把显示与生效绑到同一个常量上。
const COVER_PROTOCOL_ROW_KEY: &str = "Shift+P";

/// `P` 与 `p` 在设置页的分工（纯函数，便于回归测试）。
///
/// - `P`（裸 `'P'`）→ 进入「账号与扫码」分类；已经在该分类里时改为发起
///   当前音源的扫码登录。
/// - `p`（裸小写）→ **不**属于这里，留给「界面主题」循环键（`next_skin_name`）。
///
/// 上一版把 `p` 一起吃掉，KEYBINDINGS 里写的"设置页按 `p` 换主题"实际按不出来。
///
/// 必须保持"只认无修饰的 `'P'`"：`Shift+P`（`Char('P')` + SHIFT，或
/// `Char('p')` + SHIFT）是「封面协议」的循环键，抢过来会让那一行变成死键。
fn is_accounts_panel_key(key: &KeyEvent) -> bool {
    key.modifiers.is_empty() && key.code == KeyCode::Char('P')
}

/// 「封面协议」这一行真正生效的键（与 `COVER_PROTOCOL_ROW_KEY` 同一件事）。
///
/// 终端可能把 `Shift+P` 报成 `Char('P')` + SHIFT，也可能报成 `Char('p')` + SHIFT，
/// 两种都要认；裸 `P`（账号面板）与裸 `p`（主题循环）都不属于它。
fn is_cover_protocol_key(key: &KeyEvent) -> bool {
    key.modifiers == KeyModifiers::SHIFT && matches!(key.code, KeyCode::Char('P' | 'p'))
}

/// Enter / `Space` 都能激活的行。
///
/// - `Toggle`：`Space` 是快速切换（历史行为）；
/// - `Enum`：`Space` 等同于 `Enter` —— 打开取值菜单。主题菜单因此
///   鼠标点击 / `Enter` / `Space` 三种方式都能开，不会出现"这个键没反应"。
///
/// `Input` 与 `Action` 保持 Enter 独占：空格会把输入浮层误开。
/// `Info` 是纯说明行，激活它也只会得到一句"没有可执行的设置"。
fn row_activates_with_space(kind: SettingsRowKind) -> bool {
    matches!(kind, SettingsRowKind::Toggle | SettingsRowKind::Enum)
}

/// 宽屏（≥ 100 列）才左右分栏：左分类栏 + 右设置项。
fn category_sidebar_visible(width: u16) -> bool {
    width >= CATEGORY_SIDEBAR_MIN_WIDTH
}

/// 内嵌管理列表的一条命令行操作。
///
/// 渲染出来的命令行按钮（`JS_SOURCE_COMMANDS` / `LOCAL_PATH_COMMANDS`）、列表
/// 聚焦时的字母键、以及鼠标点击按钮三条路都汇聚到它，因此"看得见的按钮"
/// 与"真正执行的操作"永远同源（见 `embedded_command`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmbeddedCommand {
    /// JS 音源：添加（打开 URL 输入浮层）。
    AddJsSource,
    /// JS 音源：移除当前选中项（二次确认）。
    RemoveJsSource,
    /// JS 音源：音源体检。
    CheckSourceHealth,
    /// 本地目录：添加。
    AddLocalPath,
    /// 本地目录：移除当前选中项（二次确认）。
    RemoveLocalPath,
    /// 本地目录：重新扫描。
    RescanLocalMusic,
}

/// 命令行按钮字符 → 操作（列表 + 字符 → 唯一操作）。
///
/// 这是"渲染的命令行标签"和"实际执行的命令"之间**唯一**的映射表：
/// 命令行按钮的命中字符必须能在这里查到操作，否则那个按钮就是摆设。
fn embedded_command(list: SettingsFocus, key: char) -> Option<EmbeddedCommand> {
    match (list, key) {
        (SettingsFocus::JsSources, 'a') => Some(EmbeddedCommand::AddJsSource),
        (SettingsFocus::JsSources, 'd') => Some(EmbeddedCommand::RemoveJsSource),
        (SettingsFocus::JsSources, 'h') => Some(EmbeddedCommand::CheckSourceHealth),
        (SettingsFocus::LocalPaths, 'a') => Some(EmbeddedCommand::AddLocalPath),
        (SettingsFocus::LocalPaths, 'd') => Some(EmbeddedCommand::RemoveLocalPath),
        (SettingsFocus::LocalPaths, 'r') => Some(EmbeddedCommand::RescanLocalMusic),
        _ => None,
    }
}

/// 让位判断：这些子列表聚焦时自己消费 ↑/↓ 与 Enter，设置行光标必须让位。
///
/// 与 `SettingsPage::consumes_key` 的焦点判断同源：JS 音源、本地目录、
/// 状态栏字段、扫码登录四个列表都有自己的方向键语义。
fn list_owns_direction_keys(focus: SettingsFocus) -> bool {
    matches!(
        focus,
        SettingsFocus::JsSources
            | SettingsFocus::LocalPaths
            | SettingsFocus::StatusBar
            | SettingsFocus::QrLogin
    )
}

/// 行激活计划：**唯一**的激活决策点，键盘与鼠标共用。
///
/// 计划在设置行的构造点就显式声明好了（`SettingsRowMeta::plan`），这里只做
/// 两件事：只读说明行一律 `Inert`（绝不触发业务动作），其余原样返回。
/// 设置页里已经不存在"把键位字符串解析成按键再回灌"的路径。
fn plan_row_activation(meta: &SettingsRowMeta) -> RowPlan {
    if meta.kind == SettingsRowKind::Info {
        return RowPlan::Inert;
    }
    meta.plan.clone()
}

/// 菜单结果 → 现有 `AppAction`。
///
/// 只映射设置页自己构造的三种取值动作，其余一律不执行（`None`），
/// 因此不会把"不属于这个菜单"的动作误派发出去。
fn settings_menu_app_action(action: MenuAction) -> Option<AppAction> {
    match action {
        MenuAction::StatusBar(StatusBarMenuAction::SetQuality(quality)) => {
            Some(AppAction::SetQuality(quality))
        }
        MenuAction::StatusBar(StatusBarMenuAction::SetPlayMode(value)) => {
            Some(AppAction::SetPlayMode(value))
        }
        MenuAction::StatusBar(StatusBarMenuAction::ReloadJsSources) => {
            Some(AppAction::ReloadJsSources)
        }
        _ => None,
    }
}

/// 菜单项文本：当前值前面打 ✓。
fn enum_menu_label(selected: bool, label: &str) -> String {
    format!("{} {label}", if selected { "✓" } else { " " })
}

/// 取值菜单的一条选项（纯数据）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct EnumChoice {
    /// 写配置 / 派发动作时用的稳定取值标识（不随显示文案变化）。
    value: String,
    /// 菜单里显示的文案（不含 ✓）。
    label: String,
    /// 是否落在当前值上（菜单里打 ✓）。
    selected: bool,
}

/// 一行枚举设置的取值菜单内容。
///
/// 这是**纯数据视图**：只看配置与静态选项表，不依赖 `AppContext`，
/// 因此测试可以直接拿 `Config::default()` 断言"项数 / 顺序 / ✓ 落在哪一项"。
#[derive(Debug, Clone, PartialEq, Eq)]
struct EnumMenu {
    title: String,
    choices: Vec<EnumChoice>,
    /// 「解析策略」专有的"目标平台"子菜单；其余行为 `None`。
    platform: Option<Vec<EnumChoice>>,
}

impl EnumMenu {
    fn new(title: impl Into<String>, choices: Vec<EnumChoice>) -> Self {
        Self {
            title: title.into(),
            choices,
            platform: None,
        }
    }

    /// 组装成通用菜单条目：每个取值都携带 `SettingChoice { row, value }`。
    ///
    /// 选中后由设置页自己解释（见 `apply_setting_choice`），主循环不认识这些动作。
    fn into_items(self, row: &str) -> Vec<MenuItem> {
        let mut items: Vec<MenuItem> = self
            .choices
            .into_iter()
            .map(|choice| {
                MenuItem::new(
                    enum_menu_label(choice.selected, &choice.label),
                    setting_choice(row, choice.value),
                )
            })
            .collect();
        if let Some(platform) = self.platform {
            items.push(MenuItem::new(
                "目标平台...",
                submenu(
                    " 目标平台 ",
                    platform
                        .into_iter()
                        .map(|choice| {
                            MenuItem::new(
                                enum_menu_label(choice.selected, &choice.label),
                                setting_choice(SETTING_ROW_SOURCE_POLICY_PLATFORM, choice.value),
                            )
                        })
                        .collect(),
                ),
            ));
        }
        items
    }
}

/// 构造一个"设置项取值"动作。
fn setting_choice(row: &str, value: impl Into<String>) -> MenuAction {
    MenuAction::SettingChoice {
        row: row.to_string(),
        value: value.into(),
    }
}

/// 在线音源选项（默认音源与解析策略的目标平台共用）。
fn source_choices(current: Option<SourceId>) -> Vec<EnumChoice> {
    SourceId::all_online()
        .iter()
        .map(|source| EnumChoice {
            value: source.as_str().to_string(),
            label: source.display_name().to_string(),
            selected: current == Some(*source),
        })
        .collect()
}

/// 构造某个枚举行的取值菜单（纯函数）。
///
/// `play_mode` 是运行时的当前播放模式（它是播放列表的运行时状态，
/// 配置里的 `player.play_mode` 只是它的持久化副本），由调用方传入。
fn enum_menu(
    picker: SettingsEnumPicker,
    config: &lx_core::model::config::Config,
    play_mode: PlayMode,
) -> EnumMenu {
    match picker {
        // 音质的取值标识就是它的显示标签（`QUALITY_CHOICES` 内唯一），
        // 派发时用 `quality_by_label` 还原成档位。
        SettingsEnumPicker::Quality => EnumMenu::new(
            " 播放音质 ",
            QUALITY_CHOICES
                .iter()
                .map(|quality| EnumChoice {
                    value: quality.label().to_string(),
                    label: quality.label().to_string(),
                    selected: *quality == config.player.quality,
                })
                .collect(),
        ),
        SettingsEnumPicker::PlayMode => EnumMenu::new(
            " 播放模式 ",
            PLAY_MODE_CHOICES
                .iter()
                .map(|mode| EnumChoice {
                    value: mode.as_config().to_string(),
                    label: mode.label().to_string(),
                    selected: *mode == play_mode,
                })
                .collect(),
        ),
        SettingsEnumPicker::Theme => EnumMenu::new(
            " 界面主题 ",
            crate::theme::skin_names()
                .into_iter()
                .map(|name| EnumChoice {
                    label: crate::theme::skin_label(&name),
                    selected: name == config.theme.name,
                    value: name,
                })
                .collect(),
        ),
        SettingsEnumPicker::DefaultSource => {
            EnumMenu::new(" 默认音源 ", source_choices(Some(config.source.default)))
        }
        SettingsEnumPicker::SourcePolicy => EnumMenu {
            title: " 解析策略 ".to_string(),
            choices: SOURCE_POLICY_CHOICES
                .iter()
                .map(|policy| EnumChoice {
                    value: policy.as_config().to_string(),
                    label: policy.label().to_string(),
                    selected: *policy == config.source.policy,
                })
                .collect(),
            platform: Some(source_choices(config.source.policy_platform)),
        },
        // 取值与频段都来自 `crate::context::EQUALIZER_PRESETS`（唯一定义），
        // 标签也由同一张表经 `equalizer_label` 得到：本文件不再声明任何预设。
        SettingsEnumPicker::Equalizer => EnumMenu::new(
            " 均衡器 ",
            crate::context::EQUALIZER_CHOICE_VALUES
                .iter()
                .filter_map(|value| {
                    let bands = crate::context::equalizer_preset_bands(value)?;
                    Some(EnumChoice {
                        value: (*value).to_string(),
                        label: crate::context::equalizer_label(&bands).to_string(),
                        selected: config.player.equalizer_bands == bands,
                    })
                })
                .collect(),
        ),
        SettingsEnumPicker::StatusBarItems => EnumMenu::new(
            " 状态栏字段 ",
            StatusBarItem::ALL
                .iter()
                .map(|item| EnumChoice {
                    value: status_bar_item_value(*item).to_string(),
                    label: status_bar_item_label(*item).to_string(),
                    selected: config.ui.status_bar_items.contains(item),
                })
                .collect(),
        ),
        // 歌词偏移：旧版靠 `[` / `]` 一次 ±100 ms，删键后直接给一列档位，
        // 正负都能一步到位（不会出现"只能往一个方向调"）。
        SettingsEnumPicker::LyricOffset => EnumMenu::new(
            " 歌词偏移 ",
            LYRIC_OFFSET_CHOICES
                .iter()
                .map(|offset| EnumChoice {
                    value: offset.to_string(),
                    label: format!("{offset:+} ms"),
                    selected: config.lyric.offset == *offset,
                })
                .collect(),
        ),
        // 音源开关：每个在线音源一个开关，菜单里逐项打 ✓ / ○。
        SettingsEnumPicker::EnabledSources => EnumMenu::new(
            " 音源开关 ",
            SourceId::all_online()
                .iter()
                .map(|source| EnumChoice {
                    value: source.as_str().to_string(),
                    label: source.display_name().to_string(),
                    selected: config.source.enabled.contains(source),
                })
                .collect(),
        ),
    }
}

/// 扫码登录列表 / 「扫码登录」行上按 `Enter`（或点击条目）的语义（纯函数）。
///
/// 未登录 → 登录（按条目自己的渠道），已登录 → 退出登录（退出登录与渠道无关，
/// 因为两条渠道写的是同一份登录态）。删掉 `b`（退出登录）之后，退出登录只剩
/// 这一条路，因此必须由它有明确分支。
fn qr_login_action(logged_in: bool, source: SourceId, kind: QrLoginKind) -> AppAction {
    if logged_in {
        AppAction::QrLogout(source)
    } else {
        AppAction::QrLogin(source, kind)
    }
}

/// 会话是否仍可用于需要登录态的功能（账号列表的「已登录」计数口径）。
///
/// 网易云区分「凭据过期」：cookie 还在但接口已返回「需要登录」时不再算
/// 已登录，否则过期会话会一直显示 ✓。
fn source_session_valid(source: SourceId, ctx: &AppContext) -> bool {
    if source == SourceId::Wy {
        return lx_source::wy::session::login_health()
            == lx_source::wy::session::LoginHealth::LoggedIn;
    }
    ctx.source_manager.is_logged_in(source)
}

/// 账号列表的状态文案与配色。
///
/// 网易云额外显示账号昵称（登录/会话验证时保存）与「已失效」；其它音源
/// 只能看本地有没有凭据。
fn source_login_display(source: SourceId, ctx: &AppContext) -> (String, Color) {
    if source == SourceId::Wy {
        return match lx_source::wy::session::login_health() {
            lx_source::wy::session::LoginHealth::LoggedIn => {
                match lx_source::wy::session::account_display() {
                    Some(name) => (format!("✓ 已登录 · {name}"), crate::theme::green(ctx)),
                    None => ("✓ 已登录".to_string(), crate::theme::green(ctx)),
                }
            }
            lx_source::wy::session::LoginHealth::Expired => {
                ("✗ 已失效，请重新扫码".to_string(), crate::theme::red(ctx))
            }
            lx_source::wy::session::LoginHealth::NotLoggedIn => {
                ("○ 可扫码 / 未登录".to_string(), crate::theme::yellow(ctx))
            }
        };
    }
    if ctx.source_manager.is_logged_in(source) {
        ("✓ 已登录".to_string(), crate::theme::green(ctx))
    } else {
        ("○ 可扫码 / 未登录".to_string(), crate::theme::yellow(ctx))
    }
}

/// 在线音源的稳定取值标识 → 音源（与菜单里的 `SourceId::as_str` 同源）。
fn online_source(value: &str) -> Option<SourceId> {
    SourceId::all_online()
        .iter()
        .copied()
        .find(|source| source.as_str() == value)
}

/// 音质取值标签 → 档位（音质菜单的取值标识就是它的显示标签）。
fn quality_by_label(label: &str) -> Option<Quality> {
    QUALITY_CHOICES
        .iter()
        .copied()
        .find(|quality| quality.label() == label)
}

/// 状态栏字段的稳定取值标识（与 `StatusBarItem` 的 serde 名一致，便于对照配置）。
fn status_bar_item_value(item: StatusBarItem) -> &'static str {
    match item {
        StatusBarItem::State => "state",
        StatusBarItem::Source => "source",
        StatusBarItem::Sort => "sort",
        StatusBarItem::Song => "song",
        StatusBarItem::Time => "time",
        StatusBarItem::Volume => "volume",
        StatusBarItem::PlayMode => "play-mode",
        StatusBarItem::Quality => "quality",
        StatusBarItem::Queue => "queue",
        StatusBarItem::JsSourceState => "js-source-state",
    }
}

/// 菜单取值写不进配置时的提示。个别行有更具体的原因。
fn choice_refusal_message(row: &str) -> &'static str {
    match row {
        SETTING_ROW_SOURCE_ENABLED => "至少需要保留一个在线音源",
        _ => "这个取值暂时无法从设置页写入",
    }
}

/// 设置行上显示的解析策略摘要（`auto` 时目标平台不生效，不显示）。
fn source_policy_label(policy: SourcePolicy, platform: Option<SourceId>) -> String {
    match (policy, platform) {
        (SourcePolicy::Auto, _) | (_, None) => policy.label().to_string(),
        (_, Some(platform)) => format!("{} · {}", policy.label(), platform.display_name()),
    }
}

/// 把"设置行 + 取值"写进配置：**纯函数**（不碰 `AppContext`，也不落盘）。
///
/// 返回给用户看的状态文案；`None` 表示这一对"行 / 取值"不受支持
/// （防御性返回：菜单不可能产生这种组合，但绝不静默写坏配置）。
///
/// 音质 / 播放模式不在这里：它们是运行时动作，走既有
/// `AppAction::SetQuality` / `AppAction::SetPlayMode`。
fn apply_setting_choice(
    config: &mut lx_core::model::config::Config,
    row: &str,
    value: &str,
) -> Option<String> {
    match row {
        SETTING_ROW_THEME => {
            // 只接受 `skin_names()` 里的名字：否则配置里会残留未知主题，
            // 设置页显示"Voicefox"却与配置字符串不一致。
            if !crate::theme::skin_names().iter().any(|name| name == value) {
                return None;
            }
            config.theme.name = value.to_string();
            Some(format!("主题: {}", crate::theme::skin_label(value)))
        }
        SETTING_ROW_DEFAULT_SOURCE => {
            let source = online_source(value)?;
            config.source.default = source;
            Some(format!("默认音源: {}", source.as_str()))
        }
        SETTING_ROW_SOURCE_POLICY => {
            let policy = match value {
                "auto" => SourcePolicy::Auto,
                "prefer" => SourcePolicy::Prefer,
                "only" => SourcePolicy::Only,
                _ => return None,
            };
            config.source.policy = policy;
            Some(format!(
                "解析策略: {}",
                source_policy_label(policy, config.source.policy_platform)
            ))
        }
        SETTING_ROW_SOURCE_POLICY_PLATFORM => {
            let source = online_source(value)?;
            config.source.policy_platform = Some(source);
            Some(format!("目标平台: {}", source.display_name()))
        }
        SETTING_ROW_EQUALIZER => {
            let bands = crate::context::equalizer_preset_bands(value)?;
            let label = crate::context::equalizer_label(&bands).to_string();
            config.player.equalizer_bands = bands;
            Some(format!("均衡器: {label}"))
        }
        // 状态栏字段是"每项一个开关"：写入同一个 `config.ui.status_bar_items`，
        // 与原 `toggle_status_bar_item`（状态栏面板 Enter）语义完全一致。
        SETTING_ROW_LYRIC_OFFSET => {
            let offset: i32 = value.parse().ok()?;
            if !LYRIC_OFFSET_CHOICES.contains(&offset) {
                return None;
            }
            config.lyric.offset = offset;
            Some(format!("歌词偏移: {offset:+} ms"))
        }
        // 音源开关：与原 `toggle_selected_source`（`y` 选音源 + `K` 切换）一致的
        // 语义 —— 至少保留一个在线音源，关掉默认音源时自动改指第一个启用的。
        SETTING_ROW_SOURCE_ENABLED => {
            let source = online_source(value)?;
            if config.source.enabled.contains(&source) {
                // 最后一个音源不能关：返回 `None` 让调用方给出明确提示。
                if config.source.enabled.len() <= 1 {
                    return None;
                }
                config.source.enabled.retain(|item| *item != source);
                if config.source.default == source {
                    config.source.default = config.source.enabled[0];
                }
                Some(format!("{}音源 已禁用", source.as_str()))
            } else {
                config.source.enabled.push(source);
                config.source.enabled.sort_by_key(|item| {
                    SourceId::all_online()
                        .iter()
                        .position(|candidate| candidate == item)
                        .unwrap_or(usize::MAX)
                });
                Some(format!("{}音源 已启用", source.as_str()))
            }
        }
        SETTING_ROW_STATUS_BAR_ITEM => {
            let item = StatusBarItem::ALL
                .iter()
                .copied()
                .find(|item| status_bar_item_value(*item) == value)?;
            let label = status_bar_item_label(item);
            if config.ui.status_bar_items.contains(&item) {
                config
                    .ui
                    .status_bar_items
                    .retain(|candidate| *candidate != item);
                Some(format!("状态栏“{label}”已隐藏"))
            } else {
                config.ui.status_bar_items.push(item);
                Some(format!("状态栏“{label}”已显示"))
            }
        }
        _ => None,
    }
}

/// 设置行在显示顺序中的槽位 → 矩形。
///
/// 多列布局按"行优先"填充（0 号槽位在第 1 列第 1 行…），
/// 与 `setting_option_columns` 划分出来的列区同源。
/// 把可操作设置行的值推到行尾（label 贴左、值贴右，中间留白）。
///
/// 只处理 `setting_row` 的固定结构 `[键位, label, 间隙, 值…]`，且**说明行
/// （Info）不参与**——说明文字是句子，贴右会破坏阅读流。行宽放不下时不动。
fn right_align_setting_value(line: &mut Line<'static>, row_width: u16, kind: SettingsRowKind) {
    if matches!(kind, SettingsRowKind::Info) || line.spans.len() < 4 || row_width == 0 {
        return;
    }
    let used: usize = line
        .spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum();
    let padding = usize::from(row_width).saturating_sub(used);
    if padding > 1 {
        line.spans.insert(2, Span::raw(" ".repeat(padding)));
    }
}

fn setting_row_rect(columns: &[Rect], column_count: usize, slot: usize) -> Rect {
    let column_count = column_count.max(1);
    let column = slot % column_count;
    let row = slot / column_count;
    let area = columns[column];
    Rect::new(area.x, area.y.saturating_add(row as u16), area.width, 1)
}

/// 命中账本 → 行 id。鼠标只会用这一个入口。
fn row_hit_at(hits: &[SettingsRowHit], position: Position) -> Option<SettingsRowId> {
    hits.iter()
        .find(|hit| hit.rect.contains(position))
        .map(|hit| hit.id)
}

/// 命中账本 → 分类。分类栏每行同样在渲染时记录矩形。
fn category_hit_at(
    hits: &[(Rect, SettingsCategory)],
    position: Position,
) -> Option<SettingsCategory> {
    hits.iter()
        .find(|(rect, _)| rect.contains(position))
        .map(|(_, category)| *category)
}

/// 分类内的行 id（显示顺序 = 构造顺序）。
fn category_row_ids(metas: &[SettingsRowMeta], category: SettingsCategory) -> Vec<SettingsRowId> {
    metas
        .iter()
        .filter(|meta| meta.category == category)
        .map(|meta| meta.id)
        .collect()
}

/// ↑/↓ 的目标行：夹在分类首尾之间（与列表导航一致的"到头就停"）。
fn step_row_selection(
    metas: &[SettingsRowMeta],
    category: SettingsCategory,
    current: Option<SettingsRowId>,
    forward: bool,
) -> Option<SettingsRowId> {
    let ids = category_row_ids(metas, category);
    if ids.is_empty() {
        return None;
    }
    let Some(current) = current else {
        return Some(if forward { ids[0] } else { ids[ids.len() - 1] });
    };
    let Some(position) = ids.iter().position(|id| *id == current) else {
        return Some(ids[0]);
    };
    let next = if forward {
        (position + 1).min(ids.len() - 1)
    } else {
        position.saturating_sub(1)
    };
    Some(ids[next])
}

/// ↑/↓ 的行光标结果：**子列表聚焦时原样返回（让位）**，否则移动光标。
///
/// 这是"JS 音源 / 本地目录列表聚焦时 ↑/↓ 不被设置行导航抢走"的唯一判定点。
fn navigate_row_cursor(
    focus: SettingsFocus,
    metas: &[SettingsRowMeta],
    category: SettingsCategory,
    cursor: Option<SettingsRowId>,
    forward: bool,
) -> Option<SettingsRowId> {
    if list_owns_direction_keys(focus) {
        return cursor;
    }
    step_row_selection(metas, category, cursor, forward)
}

/// 光标合法化：换分类后光标指向新分类的第一行，始终有可见的"当前行"。
fn ensure_row_cursor(
    metas: &[SettingsRowMeta],
    category: SettingsCategory,
    cursor: Option<SettingsRowId>,
) -> Option<SettingsRowId> {
    let ids = category_row_ids(metas, category);
    match cursor {
        Some(id) if ids.contains(&id) => Some(id),
        _ => ids.first().copied(),
    }
}

/// 行列表的滚动窗口起点：保证光标在窗口内，且窗口不越界。
fn row_window_start(selected: Option<usize>, len: usize, capacity: usize) -> usize {
    if capacity == 0 || len <= capacity {
        return 0;
    }
    let selected = selected.unwrap_or(0).min(len - 1);
    let start = if selected >= capacity {
        selected + 1 - capacity
    } else {
        0
    };
    start.min(len - capacity)
}

/// 把"当前行"画得一眼可见：前导空格换成 `▶`，并整行加选中底色。
/// 鼠标悬停行也加 `▶`（不加底色），与键盘光标区分开。
fn highlight_setting_line(
    line: &mut Line<'static>,
    current: bool,
    hovered: bool,
    accent: Color,
    selection_fg: Color,
) {
    if !current && !hovered {
        return;
    }
    if current {
        // 与其它列表的选中样式一致：统一前景色 + 强调底色。
        line.style = line.style.bg(accent);
        for span in &mut line.spans {
            span.style = Style::new().fg(selection_fg).bg(accent);
        }
    }
    if let Some(first) = line.spans.first_mut() {
        let content = first.content.to_string();
        if let Some(rest) = content.strip_prefix(' ') {
            // 前导空格正好占一列：换成 ▶ 不改变各列对齐。
            first.content = format!("▶{rest}").into();
            first.style = first
                .style
                .fg(if current { selection_fg } else { accent })
                .add_modifier(Modifier::BOLD);
        }
    }
}

/// 渲染当前分类的可见设置行，**并把每行的矩形记进命中账本**。
///
/// 渲染与命中用的是同一份 `rect`（本函数返回值即 `SettingsRowHit`），
/// 因此不可能出现"画在这里、点在那里"的错位。
fn render_setting_rows(
    rows: &[&SettingsRow],
    area: Rect,
    buf: &mut Buffer,
    selected: Option<SettingsRowId>,
    hovered: Option<SettingsRowId>,
    accent: Color,
    selection_fg: Color,
) -> Vec<SettingsRowHit> {
    let mut hits = Vec::new();
    if area.width == 0 || area.height == 0 || rows.is_empty() {
        return hits;
    }
    let column_count = setting_option_column_count(area.width);
    let columns = setting_option_columns(area);
    let capacity = area.height as usize * column_count;
    let selected_slot = selected.and_then(|id| rows.iter().position(|row| row.meta.id == id));
    let scroll = row_window_start(selected_slot, rows.len(), capacity);
    for (slot, row) in rows.iter().enumerate().skip(scroll).take(capacity) {
        let rect = setting_row_rect(&columns, column_count, slot - scroll);
        let mut line = row.line.clone();
        // 双列布局下每列本来就不宽，值贴右会被截断；只在单列（宽面板）时启用。
        let align_width = if column_count == 1 { rect.width } else { 0 };
        right_align_setting_value(&mut line, align_width, row.meta.kind);
        highlight_setting_line(
            &mut line,
            Some(row.meta.id) == selected,
            hovered == Some(row.meta.id),
            accent,
            selection_fg,
        );
        Paragraph::new(line).render(rect, buf);
        hits.push(SettingsRowHit {
            rect,
            id: row.meta.id,
        });
    }
    hits
}

/// 渲染分类栏（宽屏左栏 / 窄屏单栏）：**面板边框 + 标题 + 每行矩形账本**。
///
/// `area` 是含边框的整块矩形（来自同一份 [`SettingsPanes`]），内区一律走
/// `panel_inner`：命中账本因此天然扣掉左右边框，既不会出现"边框那一列还能
/// 选中"，也不会出现"最后一列选不中"。
#[allow(clippy::too_many_arguments)]
fn render_setting_categories(
    categories: &[SettingsCategory],
    area: Rect,
    buf: &mut Buffer,
    selected: SettingsCategory,
    accent: Color,
    selection_fg: Color,
    muted: Color,
    border: Color,
) -> Vec<(Rect, SettingsCategory)> {
    Block::default()
        .borders(PANEL_BORDERS)
        .border_style(Style::new().fg(border))
        .title(CATEGORY_PANE_TITLE)
        .render(area, buf);

    let inner = panel_inner(area);
    let mut hits = Vec::new();
    if inner.width == 0 || inner.height == 0 {
        return hits;
    }
    for (row, category) in categories.iter().enumerate() {
        if row as u16 >= inner.height {
            break;
        }
        let rect = Rect::new(inner.x, inner.y.saturating_add(row as u16), inner.width, 1);
        let current = *category == selected;
        let style = if current {
            Style::new()
                .fg(selection_fg)
                .bg(accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(muted)
        };
        Paragraph::new(Line::from(Span::styled(
            format!("{} {}", if current { "▶" } else { " " }, category.label()),
            style,
        )))
        .render(rect, buf);
        hits.push((rect, *category));
    }
    hits
}

/// 翻页步长档位循环：5 → 10 → 15 → 20 → 5；不在档位上的自定义值按区间
/// 映射到下一档（与 `next_scroll_amount` 同一套写法）。
pub(crate) fn next_page_step(current: usize) -> usize {
    match current {
        0..=5 => 10,
        6..=10 => 15,
        11..=15 => 20,
        _ => 5,
    }
}

pub(crate) fn next_quality(quality: Quality) -> Quality {
    match quality {
        Quality::Low128 => Quality::High320,
        Quality::High320 => Quality::Flac,
        Quality::Flac => Quality::Flac24,
        Quality::Flac24 => Quality::Low128,
    }
}

/// 在一组候选值里循环取值；当前值不在候选里时回到第一个。
fn next_step(values: &[u64], current: u64) -> u64 {
    match values.iter().position(|value| *value == current) {
        Some(index) => values[(index + 1) % values.len()],
        None => values[0],
    }
}

fn next_history_limit(limit: usize) -> usize {
    match limit {
        0..=25 => 50,
        26..=50 => 100,
        51..=100 => 200,
        101..=200 => 500,
        _ => 25,
    }
}

fn next_network_timeout(timeout: u64) -> u64 {
    match timeout {
        0..=5 => 10,
        6..=10 => 15,
        11..=15 => 30,
        16..=30 => 60,
        _ => 5,
    }
}

/// 封面协议行的显示值：**生效协议放最前**。
///
/// 此前以配置值开头——旧版 Shift+P 会把 iterm2 写进配置、运行期又纠正成
/// kitty 渲染，行首的 "iterm2" 让 kitty 用户误以为探测错了终端。现在：
///
/// - `auto` 且探测成功 → `auto（生效 kitty）`；
/// - `auto` 未识别 → `auto（未识别终端，生效 halfblocks）`；
/// - 配置值与生效值不一致 → `kitty（配置 iterm2 在本终端画不出，已纠正）`；
/// - 一致 → 原样显示配置值。
fn cover_protocol_display(
    configured: &str,
    capabilities: crate::cover::CoverCapabilities,
) -> String {
    let active = crate::cover::protocol_label(capabilities.active());
    match crate::cover::protocol_from_config(configured) {
        None => match capabilities.detected() {
            Some(detected) if detected == capabilities.active() => {
                format!("auto（生效 {active}）")
            }
            _ => format!("auto（未识别终端，生效 {active}）"),
        },
        Some(configured_protocol) if configured_protocol == capabilities.active() => {
            configured.to_string()
        }
        Some(configured_protocol) => format!(
            "{active}（配置 {} 在本终端画不出，已纠正）",
            crate::cover::protocol_label(configured_protocol)
        ),
    }
}

fn next_fps(fps: u32) -> u32 {
    match fps {
        0..=10 => 20,
        11..=20 => 30,
        21..=30 => 60,
        _ => 10,
    }
}

fn next_scroll_amount(amount: usize) -> usize {
    match amount {
        0..=1 => 3,
        2..=3 => 5,
        4..=5 => 10,
        _ => 1,
    }
}

fn next_scan_depth(depth: u32) -> u32 {
    match depth {
        0 => 1,
        1 => 2,
        2 => 4,
        3..=4 => 8,
        5..=8 => 16,
        _ => 0,
    }
}

fn next_fade_duration(value: u64) -> u64 {
    match value {
        0 => 250,
        1..=250 => 500,
        251..=500 => 1_000,
        501..=1_000 => 2_000,
        _ => 0,
    }
}

fn fade_label(value: u64) -> String {
    if value == 0 {
        "关闭".to_string()
    } else if value.is_multiple_of(1_000) {
        format!("{} 秒", value / 1_000)
    } else {
        format!("{} ms", value)
    }
}

const TWO_COLUMN_OPTIONS_MIN_WIDTH: u16 = 36;
const THREE_COLUMN_OPTIONS_MIN_WIDTH: u16 = 72;
/// 页面在 `ui.pane_ratios` 里的 key。
/// 状态消息展示时长；超时自动消失，不再永久遮住面板最后一行。
const STATUS_MSG_TIMEOUT: Duration = Duration::from_secs(3);

const SETTINGS_PAGE_KEY: &str = "settings";
/// 内嵌管理列表高度份额在 `settings` 这一页里的 ratio key。
const SETTINGS_EMBEDDED_RATIO_KEY: &str = "embedded";
/// 分类栏宽度在 `settings` 这一页里的 key。
///
/// 与 `embedded` 同表，但存的是**列数**（f32）而不是比例：分类栏是定宽面板，
/// 存比例会让同一个值在 120 列与 200 列终端上给出两种宽度。
const SETTINGS_CATEGORIES_RATIO_KEY: &str = "categories";
/// 分类栏 / 分类列表面板的标题。
const CATEGORY_PANE_TITLE: &str = " 分类 ";
/// 内嵌管理列表默认占分类内容区的高度份额。
const EMBEDDED_RATIO_DEFAULT: f32 = 0.5;
const EMBEDDED_RATIO_MIN: f32 = 0.2;
const EMBEDDED_RATIO_MAX: f32 = 0.8;
/// 内嵌管理列表至少要有这么多行（顶部条 + 命令行 + 一行条目 + 一行状态）。
const EMBEDDED_MIN_HEIGHT: u16 = 5;
/// 分类内容里设置行区至少保留的行数（内嵌列表不能把它挤没）。
const ROWS_MIN_HEIGHT: u16 = 4;
/// JS 音源列表的命令行标签（渲染与命中同源）。
const JS_SOURCE_COMMANDS: &[(&str, char)] =
    &[("[a] 添加", 'a'), ("[d] 移除", 'd'), ("[h] 检测", 'h')];
/// 本地音乐目录列表的命令行标签。
const LOCAL_PATH_COMMANDS: &[(&str, char)] = &[
    ("[a] 添加目录", 'a'),
    ("[d] 移除", 'd'),
    ("[r] 重新扫描", 'r'),
];
/// 状态栏字段行里勾选框 `[x]` 的显示宽度（点它就是切换开关）。
const STATUS_BAR_CHECKBOX_WIDTH: u16 = 5;

/// 设置页在非输入模式下**仍然独占**的字符键（"本页吃掉的键"清单）。
///
/// 逐行快捷键已全部删除：设置项只能靠 `Enter` / `Space` / 鼠标激活。
/// 剩下的只有：
/// - `p`（主题循环）/ `P`（账号与扫码；`Shift+P` 是封面协议，见 `is_cover_protocol_key`）；
/// - 内嵌管理列表的命令行操作键（`a`/`d`/`h` JS 音源，`a`/`d`/`r` 本地目录），
///   只有**对应列表获得焦点**时才生效 —— 见 `owns_char_key` 与 `embedded_command`。
///
/// 列表导航键（默认 `k`/`j`）来自页面级绑定，由 `consumes_key` 查表解析，
/// 不列在这里。
const SETTINGS_PAGE_CHAR_KEYS: &[char] = &['a', 'd', 'h', 'r', 'p', 'P', 'v', 'V'];

fn setting_option_column_count(width: u16) -> usize {
    if width >= THREE_COLUMN_OPTIONS_MIN_WIDTH {
        3
    } else if width >= TWO_COLUMN_OPTIONS_MIN_WIDTH {
        2
    } else {
        1
    }
}

fn setting_option_columns(area: Rect) -> std::rc::Rc<[Rect]> {
    let count = setting_option_column_count(area.width);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints((0..count).map(|_| Constraint::Ratio(1, count as u32)))
        .split(area)
}

/// 分类内容里内嵌的管理列表（原来那排下方面板的内容并进了各自的分类）。
///
/// 这是"哪个分类吸收了原来哪个下方面板"的唯一声明点：
/// - 音源与歌词 → JS 音源列表
/// - 界面       → 状态栏字段（勾选 + 排序）
/// - 账号与扫码 → 扫码登录
/// - 数据与本地库 → 本地音乐目录
fn embedded_list_for(category: SettingsCategory) -> Option<SettingsFocus> {
    match category {
        SettingsCategory::Sources => Some(SettingsFocus::JsSources),
        SettingsCategory::Interface => Some(SettingsFocus::StatusBar),
        SettingsCategory::Accounts => Some(SettingsFocus::QrLogin),
        SettingsCategory::Data => Some(SettingsFocus::LocalPaths),
        SettingsCategory::Playback | SettingsCategory::Integration | SettingsCategory::Download => {
            None
        }
    }
}

/// 内嵌列表的条目数。渲染与鼠标命中必须给出同一份，
/// 因此三份"外部数量"由调用方传入，只有状态栏字段是固定长度。
fn embedded_items(
    list: SettingsFocus,
    js_sources: usize,
    local_paths: usize,
    qr_sources: usize,
) -> usize {
    match list {
        SettingsFocus::JsSources => js_sources,
        SettingsFocus::LocalPaths => local_paths,
        SettingsFocus::StatusBar => StatusBarItem::ALL.len(),
        SettingsFocus::QrLogin => qr_sources,
        SettingsFocus::Options => 0,
    }
}

/// 内嵌列表需要的内容行数（不含它自己的顶部条）。
fn embedded_needed_rows(list: SettingsFocus, items: usize) -> u16 {
    let chrome = match list {
        // 摘要行 + 远程歌单标题 + 计数 / 提示行
        SettingsFocus::QrLogin => 6,
        // 命令行 + 加载/检测状态行 + 底部状态或计数行
        _ => 3,
    };
    (items as u16).saturating_add(chrome)
}

/// 内嵌列表"需要多少行"：用户没拖过时按实际条目数（够用就好），
/// 拖过横向分界线（或配置里存过比例）之后表达为"能给多少要多少"。
///
/// 少了后半段，列表只有两三条时高度被 `needed` 顶死在 [`EMBEDDED_MIN_HEIGHT`]，
/// 分界线怎么拖都纹丝不动 —— 拖拽把手等于摆设。
fn embedded_needed_rows_for(list: SettingsFocus, items: usize, fixed: bool) -> u16 {
    if fixed {
        u16::MAX
    } else {
        embedded_needed_rows(list, items)
    }
}

/// 内嵌管理列表的高度：够用就好，最多占内容区 `ratio` 的份额，
/// 并保证设置行区至少留下 [`ROWS_MIN_HEIGHT`] 行（设置行不能没有可见空间）。
fn embedded_height(needed: u16, content_height: u16, ratio: f32) -> u16 {
    if content_height == 0 {
        return 0;
    }
    let reserve = ROWS_MIN_HEIGHT.min(content_height / 2);
    let cap = content_height.saturating_sub(reserve).max(1);
    let by_ratio = ((content_height as f32)
        * clamp_ratio(ratio, EMBEDDED_RATIO_MIN, EMBEDDED_RATIO_MAX))
    .round() as u16;
    let limit = by_ratio.clamp(1, cap);
    let floor = EMBEDDED_MIN_HEIGHT.min(limit);
    needed.clamp(floor, limit)
}

/// 设置页内部可拖拽的两条分界线（`Splitter<T>` 的 `T`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsResizeTarget {
    /// 宽屏：分类栏与内容之间的**纵向**分界线，拖动改变分类栏宽度。
    CategoriesWidth,
    /// 设置行区与内嵌列表之间的**横向**分界线，拖动改变内嵌列表高度。
    EmbeddedHeight,
}

/// 设置页的分区：分类栏/分类列表、分类内容里的设置行区、内嵌管理列表区，
/// 外加两条分界线自己的 gutter。
///
/// **渲染与鼠标命中共用**这个结构：矩形与分界线都由 [`settings_panes`] 一次算出
/// （`panel_inner` 的内缩只在那里出现），因此不可能出现"画在这里、点在那里"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SettingsPanes {
    /// 整个页面内区（`panel_inner(area)`）：所有矩形的根。
    inner: Rect,
    /// 宽屏左侧分类栏 / 窄屏分类列表（窄屏进入分类后为 `None`）。
    ///
    /// 这是**含边框**的整块矩形；它的内容区一律走 `panel_inner`。
    categories: Option<Rect>,
    /// 分类内容区（设置行区 + 1 行 gutter + 内嵌列表区）。
    content: Option<Rect>,
    /// 分类内容里的设置行区（窄屏分类列表视图为 `None`）。
    rows: Option<Rect>,
    /// 当前分类内嵌的管理列表区（该分类没有内嵌列表时为 `None`）。
    embedded: Option<Rect>,
    /// 分类栏与内容之间的纵向分界线；空间不足（`clamp_extent` 退化）时为
    /// `None` —— 此时既不画也不可拖，绝不覆盖内容。
    category_divider: Option<DividerHit>,
    /// 设置行区与内嵌列表之间的横向分界线；退化时为 `None`。
    embedded_divider: Option<DividerHit>,
}

impl SettingsPanes {
    /// 还没渲染过时的空几何：没有任何分界线，也就没有可拖的东西。
    fn empty() -> Self {
        Self {
            inner: Rect::default(),
            categories: None,
            content: None,
            rows: None,
            embedded: None,
            category_divider: None,
            embedded_divider: None,
        }
    }

    /// 本帧存在的分界线。渲染与拖拽命中都只从这里取，坐标不可能分叉。
    fn dividers(&self) -> Vec<(SettingsResizeTarget, DividerHit)> {
        let mut dividers = Vec::new();
        if let Some(hit) = self.category_divider {
            dividers.push((SettingsResizeTarget::CategoriesWidth, hit));
        }
        if let Some(hit) = self.embedded_divider {
            dividers.push((SettingsResizeTarget::EmbeddedHeight, hit));
        }
        dividers
    }

    /// 分界线命中：指针是否正落在某条线**自己那一格**上。
    fn resize_target_at(&self, position: Position) -> Option<SettingsResizeTarget> {
        divider_target_at(&self.dividers(), position)
    }

    /// 拖拽映射：指针列 → 分类栏宽度（列），已按当前可用宽度夹好。
    fn categories_width_at(&self, pointer: u16) -> Option<u16> {
        // 没有这条分界线就没有可拖的东西（退化时抓到的是"另一条"或什么都不抓）。
        self.category_divider?;
        let categories = self.categories?;
        let (min, max) = categories_width_limits(self.inner.width.saturating_sub(GUTTER));
        Some(clamp_extent(pointer.saturating_sub(categories.x), min, max))
    }

    /// 拖拽映射：指针行 → 内嵌列表高度份额，已夹到
    /// [`EMBEDDED_RATIO_MIN`]..=[`EMBEDDED_RATIO_MAX`]。
    fn embedded_ratio_at(&self, pointer: u16) -> Option<f32> {
        self.embedded_divider?;
        Some(embedded_ratio_from_pointer(self.content?, pointer))
    }
}

/// 分界线命中：只认分界线自己那一格（gutter 不与任何面板内容/边框重叠）。
///
/// 刻意不用 `DividerHit::matches` 的 ±`GRAB_RADIUS` 容差：设置页的分界线紧贴
/// 可点击的内容（设置行的第一列与最后一行、分类栏的边框列），带容差会把那些
/// 格子从点击里抢走。gutter 是独立的一格，精确命中即可。
fn divider_target_at(
    dividers: &[(SettingsResizeTarget, DividerHit)],
    position: Position,
) -> Option<SettingsResizeTarget> {
    dividers.iter().find_map(|(target, hit)| {
        let on_line = match hit.axis {
            SplitAxis::Vertical => position.x == hit.divider,
            SplitAxis::Horizontal => position.y == hit.divider,
        };
        let within_span = match hit.axis {
            SplitAxis::Vertical => position.y >= hit.span.0 && position.y < hit.span.1,
            SplitAxis::Horizontal => position.x >= hit.span.0 && position.x < hit.span.1,
        };
        (on_line && within_span).then_some(*target)
    })
}

/// 鼠标事件在设置页的分派意图（纯函数，便于回归"拖拽优先"的顺序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsMouseDispatch {
    /// 按下分界线那一格：开始一次拖拽会话。
    BeginResize(SettingsResizeTarget),
    /// 拖拽中：更新预览（只改内存，不落盘）。
    PreviewResize(SettingsResizeTarget),
    /// 拖拽中松开左键：结束会话并落盘。
    CommitResize,
    /// 拖拽中、或落在分界线那一格上的其它事件：吃掉，
    /// 不派发给设置行/分类，也不换分类、不动光标。
    Swallow,
    /// 普通派发（设置行 / 分类 / 内嵌列表）。
    Dispatch,
}

/// **唯一**的鼠标分派决策点：先判拖拽状态，再派发点击。
///
/// 拖拽会话一旦开始，除左键拖动/抬起外的一切事件都被吃掉：压在设置行或分类
/// 上的按下、滚轮都不会触发点击或换分类（`Swallow`）。
fn settings_mouse_dispatch(
    dragging: Option<SettingsResizeTarget>,
    dividers: &[(SettingsResizeTarget, DividerHit)],
    kind: MouseEventKind,
    position: Position,
) -> SettingsMouseDispatch {
    if let Some(target) = dragging {
        return match kind {
            MouseEventKind::Drag(MouseButton::Left) => SettingsMouseDispatch::PreviewResize(target),
            MouseEventKind::Up(MouseButton::Left) => SettingsMouseDispatch::CommitResize,
            _ => SettingsMouseDispatch::Swallow,
        };
    }
    let Some(target) = divider_target_at(dividers, position) else {
        return SettingsMouseDispatch::Dispatch;
    };
    match kind {
        MouseEventKind::Down(MouseButton::Left) => SettingsMouseDispatch::BeginResize(target),
        // 分界线那一格上的滚轮 / 右键 / 移动一律不穿透到下面的行。
        _ => SettingsMouseDispatch::Swallow,
    }
}

/// 分类栏宽度的夹取范围（`usable` = 分栏方向可用列数，已扣掉 gutter）。
///
/// 上界同时受 [`CATEGORY_SIDEBAR_WIDTH_MAX`] 与"给内容区留下
/// [`CONTENT_MIN_WIDTH`] 列"两条约束；`usable` 不够时分栏退化，
/// 但**不 panic**（`clamp_extent` 的语义：min > max 时取 min）。
fn categories_width_limits(usable: u16) -> (u16, u16) {
    let max = CATEGORY_SIDEBAR_WIDTH_MAX.min(usable.saturating_sub(CONTENT_MIN_WIDTH));
    (CATEGORY_SIDEBAR_WIDTH_MIN.min(max), max)
}

/// `pane_ratios` 里的 f32 → 分类栏列数。
///
/// 分类栏是定宽面板（不是比例），因此 `settings.categories` 这一格存的是列数
/// 本身；坏值（NaN / 负数 / 越界）一律夹回 [`CATEGORY_SIDEBAR_WIDTH_MIN`]
/// ..=[`CATEGORY_SIDEBAR_WIDTH_MAX`] 列。
fn columns_from_value(value: f32) -> u16 {
    if !value.is_finite() {
        return CATEGORY_SIDEBAR_WIDTH_MIN;
    }
    clamp_extent(
        value.round().max(0.0) as u16,
        CATEGORY_SIDEBAR_WIDTH_MIN,
        CATEGORY_SIDEBAR_WIDTH_MAX,
    )
}

/// 拖拽映射（纯函数）：指针行 → 内嵌列表高度份额。
///
/// 内嵌列表贴着内容区底边，因此从底边往上量：指针越靠上，列表越大。
/// 退化输入（内容区放不下 gutter）返回默认份额，不产生 NaN。
fn embedded_ratio_from_pointer(content: Rect, pointer: u16) -> f32 {
    let usable = content.height.saturating_sub(GUTTER);
    if usable == 0 {
        return EMBEDDED_RATIO_DEFAULT;
    }
    let embedded_rows = content
        .bottom()
        .saturating_sub(GUTTER)
        .saturating_sub(pointer);
    clamp_ratio(
        embedded_rows as f32 / usable as f32,
        EMBEDDED_RATIO_MIN,
        EMBEDDED_RATIO_MAX,
    )
}

/// 分出分类栏/分类列表、设置行区与内嵌管理列表区，以及两条分界线。
///
/// `embedded_needed` 是内嵌列表需要的内容行数；`None` 表示该分类没有内嵌列表。
/// `categories_width` 是用户拖出来的分类栏宽度（列，含边框），这里再按当前
/// 可用宽度夹一次。
///
/// 分栏一律走 [`split_with_gutter`]，分界线坐标一律走 [`divider_line`]：
/// 分界线占**它自己**的 1 格 gutter，不压任何面板的内容或边框。
fn settings_panes(
    area: Rect,
    wide: bool,
    show_rows: bool,
    embedded_needed: Option<u16>,
    embedded_ratio: f32,
    categories_width: u16,
) -> SettingsPanes {
    let inner = panel_inner(area);
    let (categories, content, category_divider) = if wide {
        let (min, max) = categories_width_limits(inner.width.saturating_sub(GUTTER));
        let width = clamp_extent(categories_width, min, max);
        let (categories, content) = split_with_gutter(inner, SplitAxis::Vertical, width);
        // 两块都画得出内容时分界线才存在；退化时返回 None（不画也不可拖）。
        let divider = (categories.width > 0 && content.width > 0).then(|| {
            DividerHit::new(
                SplitAxis::Vertical,
                divider_line(categories, SplitAxis::Vertical),
                (inner.y, inner.bottom()),
            )
        });
        (Some(categories), Some(content), divider)
    } else if show_rows {
        (None, Some(inner), None)
    } else {
        (Some(inner), None, None)
    };

    let Some(content) = content else {
        return SettingsPanes {
            inner,
            categories,
            content: None,
            rows: None,
            embedded: None,
            category_divider,
            embedded_divider: None,
        };
    };

    let Some(needed) = embedded_needed else {
        return SettingsPanes {
            inner,
            categories,
            content: Some(content),
            rows: Some(content),
            embedded: None,
            category_divider,
            embedded_divider: None,
        };
    };

    // 内容区上下分栏：横向分界线占 1 行 gutter，两块只能分 `usable` 行。
    // 比例仍按内容区高度算，拖拽时指针落在哪一行线就跟到哪一行。
    let usable = content.height.saturating_sub(GUTTER);
    let height = embedded_height(needed, usable, embedded_ratio);
    let rows_height = usable.saturating_sub(height);
    let (rows, embedded) = split_with_gutter(content, SplitAxis::Horizontal, rows_height);
    let embedded_divider = (rows.height > 0 && embedded.height > 0).then(|| {
        DividerHit::new(
            SplitAxis::Horizontal,
            divider_line(rows, SplitAxis::Horizontal),
            (content.x, content.right()),
        )
    });
    SettingsPanes {
        inner,
        categories,
        content: Some(content),
        rows: Some(rows),
        embedded: (height > 0).then_some(embedded),
        category_divider,
        embedded_divider,
    }
}

#[cfg(test)]
mod tests {
    use ratatui::layout::{Position, Rect};
    use ratatui::style::Color;
    use unicode_width::UnicodeWidthStr;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
    use lx_core::events::AppAction;
    use lx_core::keybinding::{Action, KeybindingConfig, KeybindingResolver};
    use lx_core::model::config::{Config, SourcePolicy, StatusBarItem};
    use lx_core::model::login::QrLoginKind;
    use lx_core::model::source::{Quality, SourceId};

    use crate::pages::components::context_menu::{MenuAction, StatusBarMenuAction};
    use crate::pages::components::remote_collections::RemoteCollectionsWindow;
    use crate::pages::components::splitter::{GUTTER, SplitAxis, clamp_extent, divider_line};
    use crate::playlist::mode::PlayMode;

    use super::{
        CATEGORY_SIDEBAR_WIDTH, CATEGORY_SIDEBAR_WIDTH_MAX, CATEGORY_SIDEBAR_WIDTH_MIN,
        COVER_PROTOCOL_ROW_KEY, EMBEDDED_MIN_HEIGHT, EMBEDDED_RATIO_DEFAULT, EMBEDDED_RATIO_MAX,
        EMBEDDED_RATIO_MIN, EmbeddedCommand, EmbeddedHits, EnumMenu, JS_SOURCE_COMMANDS,
        KEY_COLUMN_WIDTH, LABEL_COLUMN_WIDTH, LOCAL_PATH_COMMANDS, LYRIC_OFFSET_CHOICES,
        NarrowPane, ROWS_MIN_HEIGHT, RowInputs, RowPalette, RowPlan, SETTING_ROW_DEFAULT_SOURCE,
        SETTING_ROW_EQUALIZER, SETTING_ROW_LYRIC_OFFSET, SETTING_ROW_SOURCE_ENABLED,
        SETTING_ROW_SOURCE_POLICY, SETTING_ROW_SOURCE_POLICY_PLATFORM, SETTING_ROW_STATUS_BAR_ITEM,
        SETTING_ROW_THEME, SETTINGS_CATEGORIES, SETTINGS_CATEGORIES_RATIO_KEY,
        SETTINGS_EMBEDDED_RATIO_KEY, SETTINGS_PAGE_CHAR_KEYS, STATUS_BAR_CHECKBOX_WIDTH,
        SettingsCategory, SettingsEnumPicker, SettingsFocus, SettingsMouseDispatch, SettingsPage,
        SettingsResizeTarget, SettingsRowDirectAction, SettingsRowHit, SettingsRowId,
        SettingsRowKind, SettingsRowMeta, SettingsRows, apply_setting_choice, build_settings_rows,
        categories_width_limits, category_hit_at, category_row_ids, category_sidebar_visible,
        choice_refusal_message, columns_from_value, command_row_layout, compact_key_label,
        cover_protocol_display, embedded_command, embedded_height, embedded_items,
        embedded_list_for, embedded_needed_rows, embedded_needed_rows_for,
        embedded_ratio_from_pointer, ensure_row_cursor, enum_menu, enum_menu_label,
        is_accounts_panel_key, is_cover_protocol_key, key_label, list_owns_direction_keys,
        navigate_row_cursor, next_page_step, panel_inner, plan_row_activation, qr_login_action,
        quality_by_label, render_command_row, render_embedded_row, render_setting_categories,
        render_setting_rows, reorder_status_bar_items, row_activates_with_space, row_hit_at,
        row_window_start, setting_line, setting_option_column_count, setting_option_columns,
        setting_row_rect, setting_value_line, settings_menu_app_action, settings_mouse_dispatch,
        settings_panes, shorten_source, step_index, step_row_selection, theme_picker_row,
        truncate_display,
    };
    use ratatui::buffer::Buffer;
    use ratatui::text::Line;

    /// 各设置项取值统一起始的列号
    const VALUE_COLUMN: usize = 1 + KEY_COLUMN_WIDTH + 1 + LABEL_COLUMN_WIDTH + 1;

    /// 新的布局形态：整个设置页只有一个面板，分类内容 = 设置行区 + 内嵌管理列表区。
    ///
    /// 宽屏 = 左分类栏 + 右内容；窄屏分类视图 = 单栏分类列表；
    /// 窄屏进入分类 = 单栏内容（设置行 + 内嵌列表）。
    #[test]
    fn settings_panel_fills_the_page_and_splits_rows_from_embedded_list() {
        let wide_area = Rect::new(0, 0, 160, 40);
        let wide = settings_panes(
            wide_area,
            true,
            true,
            Some(12),
            EMBEDDED_RATIO_DEFAULT,
            CATEGORY_SIDEBAR_WIDTH,
        );
        let inner = panel_inner(wide_area);
        let categories = wide.categories.expect("宽屏必须有分类栏");
        let rows = wide.rows.expect("宽屏必须显示设置行");
        let embedded = wide.embedded.expect("有内嵌列表时必须有列表区");

        assert_eq!(categories.width, CATEGORY_SIDEBAR_WIDTH, "分类栏宽度固定");
        assert_eq!(categories.x, inner.x);
        // 分类栏与内容之间隔着 1 列 gutter（分界线占自己那一格，不压内容）。
        assert_eq!(
            rows.x,
            inner.x + categories.width + GUTTER,
            "设置行区在分类栏右侧，中间隔着分界线的 gutter"
        );
        assert_eq!(rows.width, inner.width - categories.width - GUTTER);
        assert_eq!(embedded.x, rows.x);
        assert_eq!(embedded.width, rows.width);
        // 设置行区 + 分界线 gutter + 内嵌列表区正好铺满内容区。
        assert_eq!(rows.height + embedded.height + GUTTER, inner.height);
        assert_eq!(embedded.bottom(), inner.bottom());
        assert!(rows.height > 0, "设置行区必须看得见");

        // 窄屏：分类列表视图只有分类栏；进入分类后只有内容（设置行 + 内嵌列表）。
        let narrow_area = Rect::new(0, 0, 80, 24);
        let list_view = settings_panes(
            narrow_area,
            false,
            false,
            Some(9),
            0.5,
            CATEGORY_SIDEBAR_WIDTH,
        );
        assert!(list_view.rows.is_none());
        assert!(list_view.embedded.is_none());
        assert_eq!(list_view.categories, Some(panel_inner(narrow_area)));

        let rows_view = settings_panes(
            narrow_area,
            false,
            true,
            Some(9),
            0.5,
            CATEGORY_SIDEBAR_WIDTH,
        );
        assert!(
            rows_view.categories.is_none(),
            "窄屏进入分类后不再显示分类栏"
        );
        let rows = rows_view.rows.expect("进入分类后必须显示设置行");
        let embedded = rows_view.embedded.expect("内嵌列表必须可见");
        assert_eq!(rows.width, panel_inner(narrow_area).width);
        assert_eq!(
            rows.height + embedded.height + GUTTER,
            panel_inner(narrow_area).height
        );
        // 四周全框：inner = (1,1,78,22)，故内嵌列表底边 = 23
        assert_eq!(embedded.bottom(), 23);
    }

    /// 分类内容里没有内嵌列表时（播放 / 通知与集成 / 下载），设置行区拿走整块内容区。
    #[test]
    fn categories_without_an_embedded_list_give_every_row_to_the_settings() {
        for category in [
            SettingsCategory::Playback,
            SettingsCategory::Integration,
            SettingsCategory::Download,
        ] {
            assert_eq!(
                embedded_list_for(category),
                None,
                "{category:?} 不该有内嵌列表"
            );
            let area = Rect::new(0, 0, 120, 30);
            let panes = settings_panes(area, true, true, None, 0.5, CATEGORY_SIDEBAR_WIDTH);
            let inner = panel_inner(area);
            assert_eq!(
                panes.rows,
                Some(Rect::new(
                    inner.x + CATEGORY_SIDEBAR_WIDTH + GUTTER,
                    inner.y,
                    inner.width - CATEGORY_SIDEBAR_WIDTH - GUTTER,
                    inner.height,
                )),
                "没有内嵌列表的分类：设置行区拿走整块内容区"
            );
            assert!(panes.embedded.is_none());
        }
    }

    /// 内嵌列表高度：够用就好、受 ratio 约束、且永远给设置行区留出空间。
    #[test]
    fn embedded_height_uses_the_ratio_and_reserves_row_space() {
        // 条目多时按 ratio 封顶
        assert_eq!(embedded_height(30, 20, 0.5), 10);
        assert_eq!(embedded_height(30, 20, EMBEDDED_RATIO_MIN), 4);
        assert_eq!(embedded_height(30, 20, EMBEDDED_RATIO_MAX), 16);
        // ratio 越界会被夹回区间内
        assert_eq!(
            embedded_height(30, 20, 5.0),
            embedded_height(30, 20, EMBEDDED_RATIO_MAX)
        );
        // 条目少时只要够用的高度
        assert_eq!(embedded_height(4, 20, 0.5), EMBEDDED_MIN_HEIGHT);
        // 设置行区至少留下 ROWS_MIN_HEIGHT 行
        for content_height in [6u16, 8, 10, 24, 40] {
            let height = embedded_height(200, content_height, EMBEDDED_RATIO_MAX);
            assert!(
                height <= content_height,
                "height {height} 不能超过内容区 {content_height}"
            );
            let rows = content_height - height;
            assert!(
                rows >= ROWS_MIN_HEIGHT.min(content_height / 2),
                "内容区 {content_height}：设置行区只剩 {rows} 行"
            );
        }
        // 高度为 0 的内容区不 panic
        assert_eq!(embedded_height(10, 0, 0.5), 0);
    }

    /// 用户拖过之后，内嵌列表不再"够用就好"：高度由比例给足，
    /// 否则列表只有两三条时分界线怎么拖都不动。
    #[test]
    fn a_dragged_embedded_ratio_stops_hugging_the_item_count() {
        // 没拖过：needed 很小 → 只用够用的高度（旧的"够用就好"）
        assert_eq!(
            embedded_needed_rows_for(SettingsFocus::JsSources, 1, false),
            4
        );
        assert_eq!(
            embedded_height(4, 27, EMBEDDED_RATIO_DEFAULT),
            EMBEDDED_MIN_HEIGHT
        );
        // 拖过：需求退化成"能给多少要多少"，高度完全由比例决定
        assert_eq!(
            embedded_needed_rows_for(SettingsFocus::JsSources, 1, true),
            u16::MAX
        );
        let dragged = embedded_height(u16::MAX, 27, EMBEDDED_RATIO_DEFAULT);
        assert!(dragged > EMBEDDED_MIN_HEIGHT, "拖过之后必须吃满比例");
        assert_eq!(dragged, embedded_height(200, 27, EMBEDDED_RATIO_DEFAULT));
    }

    /// 两条分界线各自**独占一格** gutter：既不在任何面板的矩形里，
    /// 也不压任何面板的边框（横向宽屏 + 横向窄屏都验一遍）。
    #[test]
    fn dividers_own_a_gutter_cell_and_never_overlap_a_pane() {
        let area = Rect::new(0, 0, 160, 40);
        let panes = settings_panes(area, true, true, Some(12), 0.5, CATEGORY_SIDEBAR_WIDTH);
        let inner = panel_inner(area);
        let categories = panes.categories.expect("宽屏分类栏");
        let rows = panes.rows.expect("宽屏设置行区");
        let embedded = panes.embedded.expect("内嵌列表区");

        let dividers = panes.dividers();
        assert_eq!(dividers.len(), 2, "宽屏必须有纵横两条分界线");
        let vertical = *dividers
            .iter()
            .find(|(target, _)| *target == SettingsResizeTarget::CategoriesWidth)
            .map(|(_, hit)| hit)
            .expect("宽屏必须有纵向分界线");
        let horizontal = *dividers
            .iter()
            .find(|(target, _)| *target == SettingsResizeTarget::EmbeddedHeight)
            .map(|(_, hit)| hit)
            .expect("有内嵌列表就必须有横向分界线");

        // 纵线：就是分类栏右侧那一列，且不与任何面板重叠（连边框也不压）。
        assert_eq!(vertical.axis, SplitAxis::Vertical);
        assert_eq!(
            vertical.divider,
            divider_line(categories, SplitAxis::Vertical)
        );
        assert_eq!(vertical.divider, categories.right());
        assert!(!categories.contains(Position::new(vertical.divider, inner.y)));
        assert!(panel_inner(categories).right() <= vertical.divider);
        assert_eq!(
            rows.x,
            vertical.divider + GUTTER,
            "内容区紧跟在 gutter 之后"
        );
        assert!(panel_inner(rows).x > vertical.divider);
        assert_eq!(vertical.span, (inner.y, inner.bottom()));

        // 横线：就是设置行区下方那一行，列表区从它的下一行开始。
        assert_eq!(horizontal.axis, SplitAxis::Horizontal);
        assert_eq!(
            horizontal.divider,
            divider_line(rows, SplitAxis::Horizontal)
        );
        assert_eq!(horizontal.divider, rows.bottom());
        assert!(!rows.contains(Position::new(rows.x, horizontal.divider)));
        assert!(panel_inner(rows).bottom() <= horizontal.divider);
        assert_eq!(
            embedded.y,
            horizontal.divider + GUTTER,
            "内嵌列表区紧跟在 gutter 之后"
        );
        assert!(panel_inner(embedded).y > horizontal.divider);
        assert_eq!(horizontal.span, (rows.x, rows.right()));

        // 两块 + gutter 恰好用满分栏方向，没有一格被重复占用。
        assert_eq!(categories.width + GUTTER + rows.width, inner.width);
        assert_eq!(rows.height + GUTTER + embedded.height, inner.height);

        // 窄屏只有横向那一条；窄屏分类列表视图一条都没有。
        let narrow = Rect::new(0, 0, 80, 24);
        let narrow_rows_view =
            settings_panes(narrow, false, true, Some(9), 0.5, CATEGORY_SIDEBAR_WIDTH);
        let narrow_dividers = narrow_rows_view.dividers();
        assert_eq!(narrow_dividers.len(), 1);
        assert_eq!(narrow_dividers[0].0, SettingsResizeTarget::EmbeddedHeight);
        assert_eq!(
            narrow_dividers[0].1.divider,
            narrow_rows_view.rows.expect("窄屏设置行区").bottom()
        );
        let narrow_list_view =
            settings_panes(narrow, false, false, Some(9), 0.5, CATEGORY_SIDEBAR_WIDTH);
        assert!(narrow_list_view.dividers().is_empty());
    }

    /// 空间不够时分界线退化：不画（`dividers` 为空）也不可拖（命中 None），
    /// 绝不回头去借面板的最后一行/列。
    #[test]
    fn a_squeezed_layout_drops_the_divider_instead_of_eating_a_pane_row() {
        // 宽屏但列数不够分栏：分类栏被夹到 0 列 → 没有可拖的纵线。
        let squeezed_category_pane =
            settings_panes(Rect::new(0, 0, 20, 10), true, true, None, 0.5, u16::MAX);
        assert!(
            squeezed_category_pane.category_divider.is_none(),
            "内容区 0 列时纵线必须不存在"
        );
        assert!(squeezed_category_pane.dividers().is_empty());
        assert!(
            squeezed_category_pane.categories_width_at(0).is_none()
                && squeezed_category_pane
                    .resize_target_at(Position::new(0, 0))
                    .is_none(),
            "没有分界线就不能拖"
        );

        // 内容区只剩 1 行：放不下"1 行 gutter + 两块"，横向分界线同样不存在。
        let squeezed_rows = settings_panes(
            Rect::new(0, 0, 120, 3),
            true,
            true,
            Some(9),
            0.5,
            CATEGORY_SIDEBAR_WIDTH,
        );
        assert!(
            squeezed_rows.embedded_divider.is_none(),
            "内容区放不下 gutter 时必须没有横线"
        );
        assert!(squeezed_rows.embedded.is_none() || squeezed_rows.embedded.unwrap().height == 0);
    }

    /// 渲染出来的分界线与拖拽命中**同一行/列**（同源断言）。
    #[test]
    fn rendered_dividers_sit_on_the_same_cell_that_grabs_them() {
        let area = Rect::new(0, 0, 160, 40);
        let panes = settings_panes(area, true, true, Some(12), 0.5, CATEGORY_SIDEBAR_WIDTH);
        let page = SettingsPage::new();
        let mut buf = Buffer::empty(area);
        page.render_resize_dividers(&panes, &mut buf, Color::Reset, Color::Reset);

        for (target, hit) in panes.dividers() {
            let (symbol, cells): (&str, Vec<(u16, u16)>) = match hit.axis {
                SplitAxis::Vertical => (
                    "│",
                    (hit.span.0..hit.span.1).map(|y| (hit.divider, y)).collect(),
                ),
                SplitAxis::Horizontal => (
                    "─",
                    (hit.span.0..hit.span.1).map(|x| (x, hit.divider)).collect(),
                ),
            };
            assert!(!cells.is_empty());
            for (x, y) in cells {
                assert_eq!(
                    buf[(x, y)].symbol(),
                    symbol,
                    "分界线必须画在 divider_line 给出的那一格 ({x},{y})"
                );
                // 画出来的每一格都能抓住它自己 —— 渲染与命中同源。
                assert_eq!(
                    panes.resize_target_at(Position::new(x, y)),
                    Some(target),
                    "({x},{y}) 画了线却抓不住"
                );
            }
        }

        // 反向：贴着分界线的那一格内容**不算**分界线（精确到格，不抢内容列/行）。
        let vertical = panes
            .dividers()
            .into_iter()
            .find(|(target, _)| *target == SettingsResizeTarget::CategoriesWidth)
            .expect("纵向分界线")
            .1;
        assert_eq!(
            panes.resize_target_at(Position::new(vertical.divider + GUTTER, vertical.span.0)),
            None,
            "内容区第一列不能被判成分界线"
        );
        let horizontal = panes
            .dividers()
            .into_iter()
            .find(|(target, _)| *target == SettingsResizeTarget::EmbeddedHeight)
            .expect("横向分界线")
            .1;
        assert_eq!(
            panes.resize_target_at(Position::new(horizontal.span.0, horizontal.divider - 1)),
            None,
            "设置行区最后一行不能被判成分界线"
        );

        // 悬停高亮：指针移到线上会亮，离开就灭（主循环据此决定重绘）。
        let mut page = SettingsPage::new();
        page.last_area = area;
        page.last_panes = panes;
        assert!(page.update_divider_hover(Position::new(vertical.divider, vertical.span.0)));
        assert_eq!(
            page.hover_divider,
            Some(SettingsResizeTarget::CategoriesWidth)
        );
        assert!(!page.update_divider_hover(Position::new(vertical.divider, vertical.span.0)));
        assert!(page.update_divider_hover(Position::new(area.x, area.y)));
        assert_eq!(page.hover_divider, None);
    }

    /// 指针位置 → 比例/宽度的纯映射：上下越界与退化输入都被夹好。
    #[test]
    fn resize_pointer_mapping_clamps_and_survives_degenerate_input() {
        let area = Rect::new(0, 0, 160, 40);
        let panes = settings_panes(area, true, true, Some(12), 0.5, CATEGORY_SIDEBAR_WIDTH);
        let categories = panes.categories.expect("分类栏");
        let content = panes.content.expect("内容区");

        // 指针在最左 / 最右都能夹回 [MIN, MAX] 列。
        assert_eq!(
            panes.categories_width_at(0),
            Some(CATEGORY_SIDEBAR_WIDTH_MIN)
        );
        assert_eq!(
            panes.categories_width_at(u16::MAX),
            Some(CATEGORY_SIDEBAR_WIDTH_MAX)
        );
        // 区间内原样生效。
        assert_eq!(
            panes.categories_width_at(categories.x + 25),
            Some(25),
            "指针列 → 分类栏宽度"
        );
        // 窄屏（没有纵线）不给宽度。
        let narrow = settings_panes(
            Rect::new(0, 0, 80, 24),
            false,
            true,
            Some(9),
            0.5,
            CATEGORY_SIDEBAR_WIDTH,
        );
        assert_eq!(narrow.categories_width_at(10), None);

        // 落盘值 → 列数：坏值与越界值都夹回区间。
        assert_eq!(columns_from_value(f32::NAN), CATEGORY_SIDEBAR_WIDTH_MIN);
        assert_eq!(columns_from_value(-5.0), CATEGORY_SIDEBAR_WIDTH_MIN);
        assert_eq!(columns_from_value(0.0), CATEGORY_SIDEBAR_WIDTH_MIN);
        assert_eq!(columns_from_value(1.0e9), CATEGORY_SIDEBAR_WIDTH_MAX);
        assert_eq!(columns_from_value(18.4), 18);
        assert_eq!(columns_from_value(18.5), 19);
        assert_eq!(columns_from_value(30.0), 30);

        // 指针行 → 比例：分界线越靠上，内嵌列表越大；贴顶 = MAX，贴底/越界 = MIN。
        assert_eq!(
            embedded_ratio_from_pointer(content, content.y),
            EMBEDDED_RATIO_MAX
        );
        assert_eq!(
            embedded_ratio_from_pointer(content, content.bottom().saturating_sub(1)),
            EMBEDDED_RATIO_MIN,
            "指针落在内容区最后一行时内嵌列表没有可用行"
        );
        assert_eq!(
            embedded_ratio_from_pointer(content, content.bottom()),
            EMBEDDED_RATIO_MIN
        );
        assert_eq!(
            embedded_ratio_from_pointer(content, u16::MAX),
            EMBEDDED_RATIO_MIN
        );
        // 中间的指针给出中间的比例，且单调（越往上越大）。
        let middle = embedded_ratio_from_pointer(content, content.y + content.height / 2);
        assert!(
            middle > EMBEDDED_RATIO_MIN && middle < EMBEDDED_RATIO_MAX,
            "中间位置: {middle}"
        );
        assert!(embedded_ratio_from_pointer(content, content.y + 4) > middle);
        assert_eq!(panes.embedded_ratio_at(content.y), Some(EMBEDDED_RATIO_MAX));
        assert_eq!(
            panes.embedded_ratio_at(content.bottom()),
            Some(EMBEDDED_RATIO_MIN)
        );
        // 退化输入（内容区放不下 gutter）不产生 NaN，也不 panic。
        assert_eq!(
            embedded_ratio_from_pointer(Rect::new(0, 0, 10, 0), 0),
            EMBEDDED_RATIO_DEFAULT
        );
        assert_eq!(
            embedded_ratio_from_pointer(Rect::new(0, 0, 10, 1), 0),
            EMBEDDED_RATIO_DEFAULT
        );
        // 没有内嵌列表的分类不给比例。
        let no_list = settings_panes(area, true, true, None, 0.5, CATEGORY_SIDEBAR_WIDTH);
        assert_eq!(no_list.embedded_ratio_at(content.y), None);
    }

    /// 拖动横向分界线 → 三块矩形随之变化，且**内容最后一行仍可命中**
    /// （回归"分割线压住面板最后一格，最后一行点不到"）。
    #[test]
    fn dragging_the_embedded_divider_moves_the_panes_and_keeps_the_last_row_hittable() {
        let area = Rect::new(0, 0, 120, 30);
        let before = settings_panes(area, true, true, Some(20), 0.5, CATEGORY_SIDEBAR_WIDTH);
        let mut page = SettingsPage::new();
        page.last_panes = before;

        let divider = before.embedded_divider.expect("横向分界线").divider;
        let target = before
            .resize_target_at(Position::new(before.rows.expect("设置行区").x, divider))
            .expect("分界线那一格必须抓得住");
        assert_eq!(target, SettingsResizeTarget::EmbeddedHeight);

        // 第一次抓线：比例先对齐到线现在所在的位置，抓的那一下不能自己跳。
        page.begin_resize(
            &before,
            target,
            Position::new(before.rows.expect("设置行区").x, divider),
        );
        assert!(page.splitter.is_dragging());
        let grabbed = settings_panes(
            area,
            true,
            true,
            Some(20),
            page.effective_embedded_ratio(),
            page.effective_categories_width(),
        );
        assert_eq!(
            grabbed.embedded.expect("内嵌列表").height,
            before.embedded.expect("内嵌列表").height,
            "抓起分界线的那一帧内嵌列表不能跳"
        );
        assert_eq!(
            grabbed.embedded_divider.expect("横向分界线").divider,
            divider,
            "抓起来之后分界线还在原来那一行"
        );

        // 往上拖 5 行：内嵌列表变大、设置行区变小。
        page.update_resize_preview(
            &before,
            target,
            Position::new(before.rows.expect("设置行区").x, divider - 5),
        );
        let after = settings_panes(
            area,
            true,
            true,
            Some(20),
            page.effective_embedded_ratio(),
            page.effective_categories_width(),
        );
        assert!(
            after.embedded.expect("内嵌列表").height > before.embedded.expect("内嵌列表").height,
            "往上拖必须把内嵌列表拖大"
        );
        assert!(
            after.rows.expect("设置行区").height < before.rows.expect("设置行区").height,
            "设置行区必须相应变小"
        );
        assert_eq!(
            after.rows.expect("设置行区").height
                + after.embedded.expect("内嵌列表").height
                + GUTTER,
            panel_inner(area).height,
            "两块 + gutter 始终铺满内容区"
        );

        // 内容最后一行：渲染出来必须仍能被点中，而 gutter 那一行不属于任何行。
        let rows_area = after.rows.expect("设置行区");
        let capacity = rows_area.height as usize * setting_option_column_count(rows_area.width);
        let palette = test_palette();
        let mut rows = SettingsRows::new();
        for index in 0..capacity {
            rows.toggle(
                SettingsCategory::Interface,
                "行",
                test_plan(index),
                true,
                palette,
            );
        }
        let visible = rows.rows_in(SettingsCategory::Interface);
        let mut buf = Buffer::empty(area);
        let hits = render_setting_rows(
            &visible,
            rows_area,
            &mut buf,
            None,
            None,
            Color::Reset,
            Color::Reset,
        );
        assert_eq!(hits.len(), capacity);
        let last_row = hits
            .iter()
            .max_by_key(|hit| hit.rect.y)
            .expect("最后一行必须渲染出来");
        assert_eq!(
            last_row.rect.bottom(),
            divider_line(rows_area, SplitAxis::Horizontal),
            "最后一行紧贴在分界线上方"
        );
        assert_eq!(
            row_hit_at(&hits, Position::new(last_row.rect.x, last_row.rect.y)),
            Some(last_row.id),
            "内容最后一行必须可命中"
        );
        let horizontal = after.embedded_divider.expect("横向分界线");
        assert_eq!(
            row_hit_at(&hits, Position::new(rows_area.x, horizontal.divider)),
            None,
            "分界线那一行不能穿透成设置行"
        );

        // 兜底取消（终端 resize / 切页 / 鼠标在内容区外松开）：会话清干净，不落盘。
        page.abort_drag_sessions();
        assert!(!page.splitter.is_dragging());
        assert_eq!(page.hover_divider, None);
        assert_eq!(page.commit_resize(), None, "取消之后没有可提交的值");
    }

    /// 拖拽期间压在设置行 / 分类上的按下、滚轮都被吃掉：
    /// 不触发点击、不换分类、也不动光标。
    #[test]
    fn a_drag_session_swallows_clicks_and_wheel_over_rows_and_categories() {
        let area = Rect::new(0, 0, 160, 40);
        let panes = settings_panes(area, true, true, Some(12), 0.5, CATEGORY_SIDEBAR_WIDTH);
        let dividers = panes.dividers();
        let rows_area = panes.rows.expect("设置行区");
        let categories = panes.categories.expect("分类栏");

        // 造一份真实账本：这两个位置确实各自压在设置行与分类上。
        let palette = test_palette();
        let mut rows = SettingsRows::new();
        for index in 0..rows_area.height as usize {
            rows.toggle(
                SettingsCategory::Interface,
                "行",
                test_plan(index),
                true,
                palette,
            );
        }
        let visible = rows.rows_in(SettingsCategory::Interface);
        let mut buf = Buffer::empty(area);
        let row_hits = render_setting_rows(
            &visible,
            rows_area,
            &mut buf,
            None,
            None,
            Color::Reset,
            Color::Reset,
        );
        let category_hits = render_setting_categories(
            &SETTINGS_CATEGORIES,
            categories,
            &mut buf,
            SettingsCategory::Interface,
            Color::Reset,
            Color::Reset,
            Color::Reset,
            Color::Reset,
        );
        let on_row = Position::new(rows_area.x + 3, rows_area.y + 1);
        let on_category =
            Position::new(panel_inner(categories).x + 2, panel_inner(categories).y + 1);
        assert!(
            row_hit_at(&row_hits, on_row).is_some(),
            "位置必须压在一行上"
        );
        assert!(
            category_hit_at(&category_hits, on_category).is_some(),
            "位置必须压在一个分类上"
        );

        // 拖拽中：这些位置上的按下/滚轮/移动全被吃掉，绝不派发。
        for position in [on_row, on_category] {
            for kind in [
                MouseEventKind::Down(MouseButton::Left),
                MouseEventKind::Down(MouseButton::Right),
                MouseEventKind::ScrollUp,
                MouseEventKind::ScrollDown,
                MouseEventKind::Moved,
            ] {
                assert_eq!(
                    settings_mouse_dispatch(
                        Some(SettingsResizeTarget::EmbeddedHeight),
                        &dividers,
                        kind,
                        position,
                    ),
                    SettingsMouseDispatch::Swallow,
                    "拖拽中 {kind:?} @ {position:?} 必须被吃掉"
                );
            }
        }
        // 拖拽中只有左键拖动与抬起有语义。
        assert_eq!(
            settings_mouse_dispatch(
                Some(SettingsResizeTarget::EmbeddedHeight),
                &dividers,
                MouseEventKind::Drag(MouseButton::Left),
                on_row,
            ),
            SettingsMouseDispatch::PreviewResize(SettingsResizeTarget::EmbeddedHeight)
        );
        assert_eq!(
            settings_mouse_dispatch(
                Some(SettingsResizeTarget::EmbeddedHeight),
                &dividers,
                MouseEventKind::Up(MouseButton::Left),
                on_row,
            ),
            SettingsMouseDispatch::CommitResize
        );

        // 不在拖拽时同样的位置照常派发（回归：别把正常点击一起吞掉）。
        assert_eq!(
            settings_mouse_dispatch(
                None,
                &dividers,
                MouseEventKind::Down(MouseButton::Left),
                on_row,
            ),
            SettingsMouseDispatch::Dispatch
        );
        assert_eq!(
            settings_mouse_dispatch(None, &dividers, MouseEventKind::ScrollDown, on_category,),
            SettingsMouseDispatch::Dispatch
        );

        // 分界线那一格：左键 = 开始拖拽，滚轮/右键 = 不穿透到下面的行。
        let on_divider = Position::new(
            rows_area.x,
            panes.embedded_divider.expect("横向分界线").divider,
        );
        assert_eq!(
            settings_mouse_dispatch(
                None,
                &dividers,
                MouseEventKind::Down(MouseButton::Left),
                on_divider
            ),
            SettingsMouseDispatch::BeginResize(SettingsResizeTarget::EmbeddedHeight)
        );
        for kind in [
            MouseEventKind::ScrollUp,
            MouseEventKind::ScrollDown,
            MouseEventKind::Down(MouseButton::Right),
            MouseEventKind::Moved,
        ] {
            assert_eq!(
                settings_mouse_dispatch(None, &dividers, kind, on_divider),
                SettingsMouseDispatch::Swallow,
                "分界线那一格上的 {kind:?} 不能穿透"
            );
        }
    }

    /// 松开左键落盘：横向写 `settings.embedded`（比例），
    /// 纵向写 `settings.categories`（列数），且内存值立刻生效。
    #[test]
    fn releasing_a_divider_commits_the_settings_pane_ratio_key() {
        let area = Rect::new(0, 0, 160, 40);
        let panes = settings_panes(area, true, true, Some(12), 0.5, CATEGORY_SIDEBAR_WIDTH);
        let mut page = SettingsPage::new();
        page.last_panes = panes;

        // ── 横向：指针行 → 比例 ──
        let target = SettingsResizeTarget::EmbeddedHeight;
        page.splitter
            .begin(target, page.committed_resize_value(target));
        let divider = panes.embedded_divider.expect("横向分界线").divider;
        page.update_resize_preview(
            &panes,
            target,
            Position::new(panes.rows.expect("设置行区").x, divider - 8),
        );
        let (key, ratio) = page.commit_resize().expect("拖拽中松开必须提交");
        assert_eq!(key, SETTINGS_EMBEDDED_RATIO_KEY);
        assert_eq!(ratio, page.embedded_ratio, "落盘值 = 内存值");
        assert!(
            page.embedded_ratio > EMBEDDED_RATIO_DEFAULT,
            "往上拖之后比例变大"
        );
        assert!(page.embedded_ratio_fixed, "拖过之后高度按比例给足");
        assert!(!page.splitter.is_dragging(), "提交后会话必须结束");
        assert_eq!(page.commit_resize(), None, "没有第二次可提交的值");

        // ── 纵向：指针列 → 列数 ──
        let target = SettingsResizeTarget::CategoriesWidth;
        page.splitter
            .begin(target, page.committed_resize_value(target));
        page.update_resize_preview(
            &panes,
            target,
            Position::new(panes.inner.x + 30, area.y + 2),
        );
        let (key, value) = page.commit_resize().expect("拖拽中松开必须提交");
        assert_eq!(key, SETTINGS_CATEGORIES_RATIO_KEY);
        assert_eq!(value, 30.0);
        assert_eq!(page.categories_width, 30);
        let after = settings_panes(
            area,
            true,
            true,
            Some(12),
            page.effective_embedded_ratio(),
            page.effective_categories_width(),
        );
        assert_eq!(
            after.categories.expect("分类栏").width,
            30,
            "落盘的宽度必须真的改变分栏"
        );

        // 越界指针被夹回范围后再落盘。
        page.splitter
            .begin(target, page.committed_resize_value(target));
        page.update_resize_preview(&panes, target, Position::new(0, area.y));
        let (_, clamped) = page.commit_resize().expect("拖拽中松开必须提交");
        assert_eq!(clamped, CATEGORY_SIDEBAR_WIDTH_MIN as f32);
        page.splitter
            .begin(target, page.committed_resize_value(target));
        page.update_resize_preview(&panes, target, Position::new(u16::MAX, area.y));
        let (_, clamped) = page.commit_resize().expect("拖拽中松开必须提交");
        assert_eq!(clamped, CATEGORY_SIDEBAR_WIDTH_MAX as f32);

        // 取消拖拽不落盘。
        page.splitter
            .begin(target, page.committed_resize_value(target));
        page.update_resize_preview(&panes, target, Position::new(panes.inner.x + 20, area.y));
        page.splitter.cancel();
        assert_eq!(page.commit_resize(), None);
        assert_eq!(page.categories_width, CATEGORY_SIDEBAR_WIDTH_MAX);

        // 配置里的值（含坏值）恢复进内存。
        let mut restored = SettingsPage::new();
        restored.apply_pane_ratios(&std::collections::HashMap::from([
            (SETTINGS_EMBEDDED_RATIO_KEY.to_string(), 0.8),
            (SETTINGS_CATEGORIES_RATIO_KEY.to_string(), 33.0),
        ]));
        assert_eq!(restored.embedded_ratio, EMBEDDED_RATIO_MAX);
        assert_eq!(restored.categories_width, 33);
        assert!(restored.embedded_ratio_fixed);
        restored.apply_pane_ratios(&std::collections::HashMap::from([(
            SETTINGS_CATEGORIES_RATIO_KEY.to_string(),
            f32::NAN,
        )]));
        assert_eq!(restored.categories_width, CATEGORY_SIDEBAR_WIDTH_MIN);
    }

    /// 分类栏的命中账本与渲染列一致：内区扣掉左右边框，宽度被夹到范围内。
    #[test]
    fn category_hits_follow_the_clamped_width_and_step_over_the_border() {
        let area = Rect::new(0, 0, 140, 30);
        for requested in [0u16, CATEGORY_SIDEBAR_WIDTH_MIN, 26, u16::MAX] {
            let panes = settings_panes(area, true, true, None, 0.5, requested);
            let categories = panes.categories.expect("宽屏分类栏");
            let (min, max) = categories_width_limits(panes.inner.width.saturating_sub(GUTTER));
            assert_eq!(
                categories.width,
                clamp_extent(requested, min, max),
                "分类栏宽度必须被夹到范围里"
            );
            assert!(
                (CATEGORY_SIDEBAR_WIDTH_MIN..=CATEGORY_SIDEBAR_WIDTH_MAX)
                    .contains(&categories.width),
                "夹取范围就是 12..=40 列"
            );

            let mut buf = Buffer::empty(area);
            let hits = render_setting_categories(
                &SETTINGS_CATEGORIES,
                categories,
                &mut buf,
                SettingsCategory::Interface,
                Color::Reset,
                Color::Reset,
                Color::Reset,
                Color::Reset,
            );
            let inner = panel_inner(categories);
            assert_eq!(hits.len(), SETTINGS_CATEGORIES.len(), "7 个分类都画得下");
            for (rect, category) in &hits {
                // 命中几何 = 内区（同一份矩形扣掉边框），不是整块矩形。
                assert_eq!(rect.x, inner.x);
                assert_eq!(rect.width, inner.width);
                assert_eq!(
                    category_hit_at(&hits, Position::new(rect.x, rect.y)),
                    Some(*category)
                );
                // 左右边框那两列都不属于任何分类（"边框那一列还能选中"回归）。
                assert_eq!(
                    category_hit_at(&hits, Position::new(rect.x - 1, rect.y)),
                    None,
                    "左边框列不能选中分类"
                );
                assert_eq!(
                    category_hit_at(&hits, Position::new(rect.right(), rect.y)),
                    None,
                    "右边框列不能选中分类"
                );
                // 内区最后一列必须能选中同一行（"最后一列选不中"回归）。
                assert_eq!(
                    category_hit_at(&hits, Position::new(rect.right() - 1, rect.y)),
                    Some(*category)
                );
            }
            // 面板自己画了边框 + 「 分类 」标题，且列数与命中账本同源。
            assert_eq!(buf[(categories.x, categories.y)].symbol(), "┌");
            assert_eq!(buf[(categories.right() - 1, categories.y)].symbol(), "┐");
            assert_eq!(buf[(categories.x, categories.bottom() - 1)].symbol(), "└");
            let title: String = (categories.x..categories.right())
                .map(|x| buf[(x, categories.y)].symbol().to_string())
                .collect();
            assert!(
                title.replace(' ', "").contains("分类"),
                "分类栏标题: {title:?}"
            );
            assert_eq!(title.chars().next(), Some('┌'), "分类栏顶边");
        }

        // 窄屏分类列表同样有边框，命中同样从内区开始。
        let narrow = Rect::new(0, 0, 80, 24);
        let panes = settings_panes(narrow, false, false, None, 0.5, CATEGORY_SIDEBAR_WIDTH);
        let categories = panes.categories.expect("窄屏分类列表");
        let mut buf = Buffer::empty(narrow);
        let hits = render_setting_categories(
            &SETTINGS_CATEGORIES,
            categories,
            &mut buf,
            SettingsCategory::Interface,
            Color::Reset,
            Color::Reset,
            Color::Reset,
            Color::Reset,
        );
        let inner = panel_inner(categories);
        assert_eq!(buf[(categories.x, categories.y)].symbol(), "┌");
        assert_eq!(hits[0].0.x, inner.x);
        assert_eq!(hits[0].0.width, inner.width);

        // 小终端（宽屏阈值以下也走窄屏）不会把内容挤成 0 宽。
        let tiny = settings_panes(Rect::new(0, 0, 24, 8), false, true, Some(9), 0.5, u16::MAX);
        assert!(tiny.rows.expect("设置行区").width > 0);
        assert_eq!(
            tiny.rows.expect("设置行区").width,
            panel_inner(Rect::new(0, 0, 24, 8)).width
        );
    }

    /// 命令行账本与渲染文本同源：命中矩形正好覆盖渲染出来的标签列。
    #[test]
    fn command_hit_targets_match_the_rendered_labels() {
        let inner = panel_inner(Rect::new(10, 20, 50, 8));
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 30));
        let commands = [("[a] 添加", 'a'), ("[d] 移除", 'd')];
        let hits = render_command_row(
            inner,
            &mut buf,
            &commands,
            (" 3 个音源", Color::Reset),
            Color::Reset,
        );

        // 宽字符占两列、后一列是空填充，因此比较前统一去掉空白。
        let text: String = (inner.x..inner.x + 14)
            .map(|x| buf[(x, inner.y)].symbol().to_string())
            .collect();
        assert!(
            text.replace(' ', "").starts_with("[a]添加"),
            "渲染文本: {text:?}"
        );
        assert_eq!(hits.len(), 2);
        for (rect, key) in &hits {
            // 命中矩形内的字符必须属于这个标签
            let label: String = (rect.x..rect.right())
                .map(|x| buf[(x, rect.y)].symbol().to_string())
                .collect();
            assert!(
                label.replace(' ', "").starts_with(&format!("[{key}]")),
                "{key} 的命中矩形覆盖了 {label:?}"
            );
            assert_eq!(
                EmbeddedHits {
                    rows: Vec::new(),
                    commands: hits.clone(),
                }
                .command_at(Position::new(rect.x, rect.y)),
                Some(*key)
            );
        }
        // 行首空格与下一行都不命中
        let empty = EmbeddedHits::default();
        assert_eq!(empty.command_at(Position::new(inner.x, inner.y)), None);
        assert_eq!(
            EmbeddedHits {
                rows: Vec::new(),
                commands: hits,
            }
            .command_at(Position::new(inner.x + 1, inner.y + 1)),
            None
        );
        // 布局函数与命中账本必须一致
        let (rendered, layout_hits) = command_row_layout(inner, &commands);
        assert!(rendered.starts_with(" [a] 添加  [d] 移除"));
        assert_eq!(layout_hits.len(), 2);
    }

    /// 远程歌单窗口打开时，设置页必须独占全部按键：否则 `q` 会在窗口还开着
    /// 的时候直接退出程序，`p` 会在背后换主题。
    #[test]
    fn the_remote_collections_window_takes_over_the_keyboard() {
        let resolver = KeybindingResolver::from_config(&KeybindingConfig::default());
        let mut page = SettingsPage::new();
        assert!(page.remote_window.is_none(), "默认不开窗");

        page.remote_window = Some(RemoteCollectionsWindow::new(Vec::new()));
        for code in [
            KeyCode::Char('q'),
            KeyCode::Char('p'),
            KeyCode::Char('v'),
            KeyCode::Esc,
            KeyCode::Enter,
            KeyCode::Down,
        ] {
            let event = KeyEvent::new(code, KeyModifiers::NONE);
            assert!(
                page.consumes_key(&event, &resolver),
                "{code:?} 必须交给远程歌单窗口，不能被全局快捷键抢走"
            );
        }
    }

    /// `v` 是设置页**新增**的页面级键（查看远程歌单窗口），必须被本页吃掉。
    #[test]
    fn v_is_owned_by_the_settings_page() {
        let resolver = KeybindingResolver::from_config(&KeybindingConfig::default());
        let page = SettingsPage::new();
        for code in [KeyCode::Char('v'), KeyCode::Char('V')] {
            let event = KeyEvent::new(code, KeyModifiers::NONE);
            assert!(page.consumes_key(&event, &resolver), "{code:?} 应归设置页");
        }
    }

    /// `truncate_display` 的输出宽度必须**恒定**：调用点靠它做列对齐。
    ///
    /// 旧实现在截断分支直接返回，`truncate_display("晴晴晴", 4)` 只有 3 列，
    /// 宽字符边界上整行会向左串一列。
    #[test]
    fn truncate_display_always_fills_the_requested_width() {
        for (value, width) in [
            ("晴天", 6),
            ("晴晴晴", 6),
            ("晴晴晴", 4),
            ("a very long english name", 18),
            ("", 5),
            ("晴天", 0),
        ] {
            assert_eq!(
                UnicodeWidthStr::width(truncate_display(value, width).as_str()),
                width,
                "{value:?} 压到 {width} 列时宽度必须正好是 {width}"
            );
        }

        // 超宽才加省略号；没超宽原样补齐
        assert_eq!(truncate_display("abc", 5), "abc  ");
        assert!(truncate_display("abcdef", 5).ends_with('…'));
    }

    /// 恢复默认布局必须同时复位"用户拖过"的标记：否则高度仍按比例给足，
    /// 看起来还是老的布局。
    #[test]
    fn reset_pane_ratios_restores_defaults_and_clears_the_dragged_flag() {
        let mut page = SettingsPage::new();
        page.apply_pane_ratios(&std::collections::HashMap::from([
            (SETTINGS_EMBEDDED_RATIO_KEY.to_string(), 0.8_f32),
            (SETTINGS_CATEGORIES_RATIO_KEY.to_string(), 30_f32),
        ]));
        assert!(page.embedded_ratio_fixed, "拖过之后高度按比例给足");
        assert_ne!(page.embedded_ratio, EMBEDDED_RATIO_DEFAULT);
        assert_ne!(page.categories_width, CATEGORY_SIDEBAR_WIDTH);

        page.reset_pane_ratios();

        assert_eq!(page.embedded_ratio, EMBEDDED_RATIO_DEFAULT);
        assert!(
            !page.embedded_ratio_fixed,
            "复位后回到「够用就好」的自动高度"
        );
        assert_eq!(page.categories_width, CATEGORY_SIDEBAR_WIDTH);
        assert!(!page.splitter.is_dragging());
    }

    /// 测试用配色（不参与任何断言）。
    fn test_palette() -> RowPalette {
        RowPalette {
            accent: Color::Reset,
            muted: Color::Reset,
        }
    }

    /// 合成行的激活计划：行表测试只关心分类 / 光标，不关心具体动作类型。
    fn test_plan(index: usize) -> RowPlan {
        const PLANS: [RowPlan; 4] = [
            RowPlan::Direct(SettingsRowDirectAction::ToggleMouse),
            RowPlan::Direct(SettingsRowDirectAction::ToggleAggregateSearch),
            RowPlan::Direct(SettingsRowDirectAction::ToggleWrapNavigation),
            RowPlan::Menu(SettingsEnumPicker::Theme),
        ];
        PLANS[index % PLANS.len()].clone()
    }

    /// 行表构造的测试输入：真实行集不需要播放器 / 音源管理器 / 下载管理器。
    fn test_row_inputs() -> RowInputs {
        RowInputs {
            cover_cache_label: "0 个 · 0 B".to_string(),
            remote_cache_label: "0 B · 歌单镜像".to_string(),
            play_mode: PlayMode::ListLoop,
            ab_loop: None,
            qr_login_label: "○样例音源".to_string(),
            download_dir: "/home/user/Music/voicefox".to_string(),
        }
    }

    /// **真实设置行集**：与生产 `render` 调用同一个 `build_settings_rows`。
    ///
    /// 所有"行集"断言都以它为基础：不再拿合成的"第一行/第二行"顶替，
    /// 而是直接检查用户真正看到的那张表。
    fn real_settings_rows(config: &Config) -> SettingsRows {
        let mut rows = SettingsRows::new();
        build_settings_rows(
            &mut rows,
            config,
            &test_row_inputs(),
            test_palette(),
            crate::cover::CoverCapabilities::from_detected(
                None,
                crate::cover::ProtocolType::Halfblocks,
            ),
        );
        rows
    }

    /// 某个分类里的真实设置项标签（按显示顺序）。
    fn labels_in(rows: &SettingsRows, category: SettingsCategory) -> Vec<String> {
        rows.rows_in(category)
            .iter()
            .map(|row| row.meta.label.clone())
            .collect()
    }

    #[test]
    fn accent_follow_cover_and_page_step_rows_live_in_the_interface_category() {
        let rows = real_settings_rows(&Config::default());
        for label in ["封面主色跟随", "翻页步长"] {
            let meta = rows
                .metas()
                .into_iter()
                .find(|meta| meta.label == label)
                .unwrap_or_else(|| panic!("真实行集里必须有「{label}」"));
            assert_eq!(meta.category, SettingsCategory::Interface, "{label}");
            assert_eq!(meta.kind, SettingsRowKind::Enum, "{label}");
        }
        let accent = rows
            .metas()
            .into_iter()
            .find(|meta| meta.label == "封面主色跟随")
            .expect("checked above");
        assert_eq!(
            plan_row_activation(&accent),
            RowPlan::Direct(SettingsRowDirectAction::CycleAccentFollowCover),
        );
        let page_step = rows
            .metas()
            .into_iter()
            .find(|meta| meta.label == "翻页步长")
            .expect("checked above");
        assert_eq!(
            plan_row_activation(&page_step),
            RowPlan::Direct(SettingsRowDirectAction::CyclePageStep),
        );
        // 档位推进函数本身。
        assert_eq!(next_page_step(5), 10);
        assert_eq!(next_page_step(10), 15);
        assert_eq!(next_page_step(15), 20);
        assert_eq!(next_page_step(20), 5);
        assert_eq!(next_page_step(7), 15, "自定义值按区间映射");
        // 强档位循环：关闭 → 轻微 → 明显 → 关闭。
        use lx_core::model::config::AccentFollowCover;
        assert_eq!(AccentFollowCover::Off.next(), AccentFollowCover::Subtle);
        assert_eq!(AccentFollowCover::Subtle.next(), AccentFollowCover::Strong);
        assert_eq!(AccentFollowCover::Strong.next(), AccentFollowCover::Off);
    }

    #[test]
    fn cover_protocol_row_leads_with_the_active_protocol() {
        use crate::cover::{CoverCapabilities, ProtocolType};
        let kitty =
            CoverCapabilities::from_detected(Some(ProtocolType::Kitty), ProtocolType::Kitty);
        let corrected =
            CoverCapabilities::from_detected(Some(ProtocolType::Kitty), ProtocolType::Kitty);
        let unknown = CoverCapabilities::from_detected(None, ProtocolType::Halfblocks);

        // auto：生效协议亮出来。
        assert_eq!(cover_protocol_display("auto", kitty), "auto（生效 kitty）");
        assert_eq!(
            cover_protocol_display("auto", unknown),
            "auto（未识别终端，生效 halfblocks）"
        );
        // 配置与生效一致：原样显示。
        assert_eq!(cover_protocol_display("kitty", kitty), "kitty");
        // 旧配置残留 iterm2：行首是生效协议，配置值退居括号说明。
        assert_eq!(
            cover_protocol_display("iterm2", corrected),
            "kitty（配置 iterm2 在本终端画不出，已纠正）"
        );
    }

    /// **真实行集**里每一行都属于 `SETTINGS_CATEGORIES` 里的**恰好一个**分类。
    ///
    /// 这条测试在重构时被删掉过，替代品只测合成的"第一行/第二行"，等于真实行集
    /// 失去了保护。这里按元数据**显式分类**（绝不用下标推断）恢复等价断言：
    /// 没有重复 id、没有空分类（空分类 = 该分类的设置全部丢失）。
    #[test]
    fn every_setting_option_belongs_to_exactly_one_category() {
        let rows = real_settings_rows(&Config::default());
        let metas = rows.metas();
        assert!(!metas.is_empty(), "真实行集不能是空的");

        // 1. 每个分类都必须有行：空分类说明那个分类的设置全丢了
        for category in SETTINGS_CATEGORIES {
            let listed = rows.rows_in(category);
            assert!(
                !listed.is_empty(),
                "「{}」分类一行设置都没有（重构把行弄丢了）",
                category.label()
            );
            for row in &listed {
                assert_eq!(
                    row.meta.category, category,
                    "“{}”被分类过滤带进了别的分类",
                    row.meta.label
                );
            }
        }

        // 2. 真实行集里不允许出现"不属于任何分类"的行
        for meta in &metas {
            assert!(
                SETTINGS_CATEGORIES.contains(&meta.category),
                "“{}”的分类 {:?} 不在分类表里",
                meta.label,
                meta.category
            );
        }

        // 3. 分类并集 == 真实行集，每一行只出现一次（没有重复 id / 漏行）
        let mut seen: Vec<SettingsRowId> = Vec::new();
        for category in SETTINGS_CATEGORIES {
            for row in rows.rows_in(category) {
                assert!(
                    !seen.contains(&row.meta.id),
                    "行 id {:?}（“{}”）同时属于多个分类",
                    row.meta.id,
                    row.meta.label
                );
                seen.push(row.meta.id);
            }
        }
        assert_eq!(seen.len(), metas.len(), "分类并集必须正好覆盖全部真实行");

        // 4. 分类栏用的 `category_row_ids` 与真实行表同源（渲染与光标同一条口径）
        for category in SETTINGS_CATEGORIES {
            let ids: Vec<SettingsRowId> = rows
                .rows_in(category)
                .iter()
                .map(|row| row.meta.id)
                .collect();
            assert_eq!(category_row_ids(&metas, category), ids, "行序必须一致");
        }
        assert_eq!(SETTINGS_CATEGORIES.len(), 7, "分类数量是契约的一部分");
    }

    /// 下载分类必须暴露**全部**下载相关设置项。
    ///
    /// 逐项列出当前真实存在的标签：少一项就说明"下载设置里再也找不到那个开关"。
    #[test]
    fn download_category_exposes_every_download_option() {
        let rows = real_settings_rows(&Config::default());
        let labels = labels_in(&rows, SettingsCategory::Download);
        let expected = [
            "下载目录",
            "下载音质",
            "文件名模板",
            "多线程分片",
            "分片阈值",
            "分片并发",
            "同时下载",
            "失败重试",
            "校验文件大小",
            "跳过已下载",
            "写入标签",
            "嵌入封面",
            "保存歌词",
        ];
        assert_eq!(
            labels, expected,
            "下载分类的设置项必须一个不少（顺序 = 构造顺序）"
        );

        // 每一项都能真的被激活，不是只读摆设
        for row in rows.rows_in(SettingsCategory::Download) {
            assert_ne!(
                row.meta.kind,
                SettingsRowKind::Info,
                "“{}”是只读行",
                row.meta.label
            );
            assert_ne!(
                plan_row_activation(&row.meta),
                RowPlan::Inert,
                "“{}”没有激活计划（Enter 按下去没反应）",
                row.meta.label
            );
        }

        // 同一条设置只能有一个归属分类
        for category in SETTINGS_CATEGORIES {
            if category == SettingsCategory::Download {
                continue;
            }
            let other = labels_in(&rows, category);
            for label in expected {
                assert!(
                    !other.contains(&label.to_string()),
                    "“{label}”同时出现在「{}」和「下载」里",
                    category.label()
                );
            }
        }
    }

    /// 关键设置项清单：重构不能把行弄丢（对真实行集逐项断言存在）。
    #[test]
    fn key_settings_options_stay_in_the_real_row_set() {
        let rows = real_settings_rows(&Config::default());
        let expected: [(SettingsCategory, &[&str]); 7] = [
            (
                SettingsCategory::Interface,
                &[
                    "界面主题",
                    "鼠标控制",
                    "聚合搜索",
                    "循环导航",
                    "封面显示",
                    "保留播放状态",
                    "网络超时",
                    "封面协议",
                    "最大 FPS",
                    "切歌通知",
                    "状态栏字段",
                ],
            ),
            (
                SettingsCategory::Playback,
                &[
                    "播放音质",
                    "播放速度",
                    "音频设备",
                    "ReplayGain",
                    "RG 预放大",
                    "声道模式",
                    "左右平衡",
                    "ReplayGain 削波保护",
                    "淡入时长",
                    "淡出时长",
                    "均衡器",
                    "淡入当前歌曲",
                    "淡出当前歌曲",
                    "A-B 循环起点",
                    "A-B 循环终点",
                    "清除 A-B",
                    "播放模式",
                    "历史上限",
                ],
            ),
            (
                SettingsCategory::Sources,
                &[
                    "默认音源",
                    "自动换源",
                    "解析策略",
                    "音源开关",
                    "歌词翻译",
                    "逐字歌词",
                    "歌词偏移",
                    "网络代理",
                    "重新加载 JS 音源",
                    "音源体检",
                ],
            ),
            (
                SettingsCategory::Accounts,
                &["扫码登录", "刷新远程歌单", "QQ 音乐同步"],
            ),
            (
                SettingsCategory::Integration,
                &["滚动步长", "MPRIS", "TUI 通知", "桌面通知", "通知封面"],
            ),
            (
                SettingsCategory::Download,
                &["下载目录", "下载音质", "文件名模板", "失败重试"],
            ),
            (
                SettingsCategory::Data,
                &["扫描深度", "导出数据", "导入数据"],
            ),
        ];

        for (category, labels) in expected {
            let listed = labels_in(&rows, category);
            for label in labels {
                assert!(
                    listed.contains(&label.to_string()),
                    "「{}」分类里缺少关键设置项「{label}」（重构丢了行）",
                    category.label()
                );
            }
        }
        // 清单里的分类就是全部分类：不允许有没被清单覆盖的分类
        for category in SETTINGS_CATEGORIES {
            assert!(
                expected.iter().any(|(listed, _)| *listed == category),
                "「{}」没有出现在关键项清单里",
                category.label()
            );
        }
    }

    /// 每一行都在构造点显式声明分类：过滤只按元数据做，
    /// 在中间插一行也不会让其它分类的行错位（回归 `option_indices` 那种下标区间推断）。
    #[test]
    fn category_filter_keeps_every_row_in_its_own_category() {
        let palette = test_palette();
        let mut rows = SettingsRows::new();
        for category in SETTINGS_CATEGORIES {
            rows.toggle(category, "第一行", test_plan(0), true, palette);
            rows.toggle(category, "第二行", test_plan(1), false, palette);
        }
        let inserted = rows.toggle(
            SettingsCategory::Playback,
            "插入行",
            test_plan(2),
            true,
            palette,
        );
        let metas = rows.metas();

        for category in SETTINGS_CATEGORIES {
            let listed: Vec<SettingsRowId> = rows
                .rows_in(category)
                .iter()
                .map(|row| row.meta.id)
                .collect();
            assert_eq!(listed, category_row_ids(&metas, category), "行序必须一致");
            assert_eq!(
                rows.rows_in(category).len(),
                listed.len(),
                "行数必须来自元数据"
            );
            for id in &listed {
                let meta = metas.iter().find(|meta| meta.id == *id).unwrap();
                assert_eq!(meta.category, category, "分类过滤不能把别的行带进来");
            }
        }

        let playback: Vec<SettingsRowId> = rows
            .rows_in(SettingsCategory::Playback)
            .iter()
            .map(|row| row.meta.id)
            .collect();
        assert_eq!(playback.len(), 3);
        assert!(playback.contains(&inserted));
        for category in SETTINGS_CATEGORIES {
            if category == SettingsCategory::Playback {
                continue;
            }
            assert_eq!(
                rows.rows_in(category).len(),
                2,
                "{:?} 的行数不该因为别的分类插了一行而变化",
                category.label()
            );
        }
    }

    /// 光标在分类内移动、到头即停；换分类后落到新分类的第一行。
    #[test]
    fn row_cursor_stays_inside_the_current_category() {
        let palette = test_palette();
        let mut rows = SettingsRows::new();
        let first = rows.toggle(
            SettingsCategory::Interface,
            "a",
            test_plan(0),
            true,
            palette,
        );
        let second = rows.toggle(
            SettingsCategory::Interface,
            "b",
            test_plan(1),
            true,
            palette,
        );
        let third = rows.toggle(
            SettingsCategory::Interface,
            "c",
            test_plan(2),
            true,
            palette,
        );
        let other = rows.toggle(SettingsCategory::Playback, "x", test_plan(3), true, palette);
        let metas = rows.metas();

        assert_eq!(
            ensure_row_cursor(&metas, SettingsCategory::Interface, None),
            Some(first)
        );
        assert_eq!(
            ensure_row_cursor(&metas, SettingsCategory::Interface, Some(other)),
            Some(first),
            "光标不属于本分类时必须回到第一行"
        );
        assert_eq!(
            step_row_selection(&metas, SettingsCategory::Interface, Some(first), true),
            Some(second)
        );
        assert_eq!(
            step_row_selection(&metas, SettingsCategory::Interface, Some(third), true),
            Some(third),
            "末行继续 ↓ 停在末行"
        );
        assert_eq!(
            step_row_selection(&metas, SettingsCategory::Interface, Some(first), false),
            Some(first),
            "首行继续 ↑ 停在首行"
        );
        assert_eq!(
            step_row_selection(&metas, SettingsCategory::Playback, None, true),
            Some(other)
        );
    }

    #[test]
    fn shortens_unicode_source_path_on_character_boundaries() {
        let path = "/home/user/音乐音源/这是一个很长的第三方音源脚本文件名/latest.js";
        let shortened = shorten_source(path, 24);

        assert_eq!(shortened.chars().count(), 24);
        assert!(shortened.ends_with("..."));
    }

    #[test]
    fn setting_rows_align_values_on_a_shared_column() {
        let accent = Color::Reset;
        let muted = Color::Reset;
        let rows = [
            setting_line("MPRIS", true, "i", accent, muted),
            setting_line("保留播放状态", false, "e", accent, muted),
            setting_value_line("最大 FPS", "30", "f", accent, muted),
            setting_value_line("歌词偏移", "+0 ms", "[/]", accent, muted),
            setting_value_line("音源开关", "kw 开启", "k/K", accent, muted),
        ];

        for row in rows {
            let prefix: String = row.spans[..row.spans.len() - 1]
                .iter()
                .map(|span| span.content.as_ref())
                .collect();

            assert_eq!(UnicodeWidthStr::width(prefix.as_str()), VALUE_COLUMN);
        }
    }

    /// 设置页**只**独占 `SETTINGS_PAGE_CHAR_KEYS` 里那几个字符键，别无其它。
    ///
    /// 清单是"本页吃掉的键"的唯一来源，两个方向都要成立：
    /// - 清单里的键在其生效焦点下必须被独占（否则会被全局快捷键抢先处理）；
    /// - 清单外的字母键一律让出去（不出现"吃掉却不处理"的死键）。
    #[test]
    fn settings_page_owns_every_char_key_it_advertises() {
        let resolver = KeybindingResolver::from_config(&KeybindingConfig::default());

        // 清单本身：主题 / 账号面板 / 远程歌单窗口 + 内嵌列表的操作键
        assert_eq!(
            SETTINGS_PAGE_CHAR_KEYS,
            &['a', 'd', 'h', 'r', 'p', 'P', 'v', 'V'],
            "设置页吃掉的字符键清单（多一个都算死键）"
        );

        // `p` / `P`：任何焦点下都归设置页（主题循环 / 账号面板 / Shift+P 封面协议）
        for focus in [
            SettingsFocus::Options,
            SettingsFocus::JsSources,
            SettingsFocus::LocalPaths,
            SettingsFocus::StatusBar,
            SettingsFocus::QrLogin,
        ] {
            let mut page = SettingsPage::new();
            page.focus = focus;
            for character in ['p', 'P'] {
                let event = KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE);
                assert!(
                    page.consumes_key(&event, &resolver),
                    "{focus:?} 下 {character} 必须由设置页独占"
                );
            }
        }

        // 内嵌列表的操作键：只有对应列表获得焦点时才独占
        for (focus, character) in [
            (SettingsFocus::JsSources, 'a'),
            (SettingsFocus::JsSources, 'd'),
            (SettingsFocus::JsSources, 'h'),
            (SettingsFocus::LocalPaths, 'a'),
            (SettingsFocus::LocalPaths, 'd'),
            (SettingsFocus::LocalPaths, 'r'),
        ] {
            let mut page = SettingsPage::new();
            page.focus = focus;
            let event = KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE);
            assert!(
                page.consumes_key(&event, &resolver),
                "{focus:?} 下 {character} 必须由设置页独占"
            );
        }

        // 行光标导航别名：默认 `k` / `j`（来自页面级绑定，仍归设置页）
        let page = SettingsPage::new();
        for character in ['k', 'j'] {
            let event = KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE);
            assert!(
                page.consumes_key(&event, &resolver),
                "默认导航键 {character} 必须由设置页独占"
            );
        }

        // 清单外的字母键（含旧版那些行内快捷键）一个都不能吃
        let page = SettingsPage::new();
        for character in 'a'..='z' {
            if SETTINGS_PAGE_CHAR_KEYS.contains(&character) || matches!(character, 'k' | 'j') {
                continue;
            }
            let event = KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE);
            assert!(
                !page.consumes_key(&event, &resolver),
                "{character} 不在清单里，不该被设置页吃掉"
            );
        }
        // 列表操作键在别的焦点下也不能被吃掉（否则就是死键）
        for (focus, character) in [
            (SettingsFocus::Options, 'a'),
            (SettingsFocus::Options, 'd'),
            (SettingsFocus::Options, 'r'),
            (SettingsFocus::Options, 'h'),
            (SettingsFocus::StatusBar, 'a'),
            (SettingsFocus::QrLogin, 'h'),
        ] {
            let mut page = SettingsPage::new();
            page.focus = focus;
            let event = KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE);
            assert!(
                !page.consumes_key(&event, &resolver),
                "{focus:?} 下 {character} 不该被设置页吃掉"
            );
        }
    }

    #[test]
    fn tab_number_keys_remain_available_on_the_settings_page() {
        let resolver = KeybindingResolver::from_config(&KeybindingConfig::default());
        let page = SettingsPage::new();

        for key in '0'..='9' {
            assert!(!page.consumes_key(
                &KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
                &resolver
            ));
        }
    }

    #[test]
    fn tab_number_keys_remain_reserved_with_custom_settings_bindings() {
        let mut config = KeybindingConfig::default();
        config
            .pages
            .get_mut("settings")
            .unwrap()
            .insert(Action::SettingsCyclePlaybackSpeed, "1".to_string());
        let resolver = KeybindingResolver::from_config(&config);
        let page = SettingsPage::new();

        for key in '1'..='8' {
            assert!(!page.consumes_key(
                &KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
                &resolver
            ));
        }
    }

    #[test]
    fn settings_page_owns_the_configured_list_navigation_keys() {
        let mut config = KeybindingConfig::default();
        config
            .pages
            .get_mut("settings")
            .unwrap()
            .insert(Action::ListSelectUp, "h".to_string());
        let resolver = KeybindingResolver::from_config(&config);
        let page = SettingsPage::new();

        let rebound = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE);
        let released = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE);

        assert!(page.consumes_key(&rebound, &resolver));
        // 'k' 不再是导航键，也不是选项键，应交还给全局
        assert!(!page.consumes_key(&released, &resolver));
    }

    /// 旧版逐行动作键（`Settings*`）即便还留在用户配置里，也不再被设置页吃掉。
    ///
    /// 这些动作已经随行内快捷键一起删除：拦下来只会变成"按下去没反应"的死键，
    /// 因此必须把它们让给全局分发（清单之外一键不吃）。
    #[test]
    fn legacy_settings_action_bindings_are_released_to_the_global_dispatcher() {
        let mut config = KeybindingConfig::default();
        config
            .pages
            .get_mut("settings")
            .unwrap()
            .insert(Action::SettingsCyclePlaybackSpeed, "Ctrl+1".to_string());
        let resolver = KeybindingResolver::from_config(&config);
        let page = SettingsPage::new();

        let rebound = KeyEvent::new(KeyCode::Char('1'), KeyModifiers::CONTROL);
        let plain = KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE);

        assert!(
            !page.consumes_key(&rebound, &resolver),
            "已删除的行内动作不该再抢这个组合键"
        );
        assert!(
            !page.consumes_key(&plain, &resolver),
            "数字键始终留给页面导航"
        );
    }

    #[test]
    fn settings_page_leaves_playback_and_navigation_keys_global() {
        let resolver = KeybindingResolver::from_config(&KeybindingConfig::default());
        let mut page = SettingsPage::new();
        // 设置项面板聚焦时会独占 Enter/Space（见 `options_focus_owns_enter_and_space`），
        // 这里验证其它区域照旧把全局键让出去。
        page.focus = SettingsFocus::JsSources;
        let global = [
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char(','), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('.'), KeyModifiers::NONE),
        ];

        for event in global {
            assert!(
                !page.consumes_key(&event, &resolver),
                "{:?} 不应被设置页独占",
                event.code
            );
        }
    }

    #[test]
    fn status_bar_focus_owns_toggle_and_reorder_keys() {
        let resolver = KeybindingResolver::from_config(&KeybindingConfig::default());
        let mut page = SettingsPage::new();
        let space = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
        let shift_left = KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT);

        page.focus = SettingsFocus::JsSources;
        assert!(!page.consumes_key(&space, &resolver));
        page.focus = SettingsFocus::StatusBar;
        assert!(page.consumes_key(&space, &resolver));
        assert!(page.consumes_key(&shift_left, &resolver));
    }

    /// 四个"有内容"的分类各自吸收一个原来的下方面板；其余分类没有内嵌列表。
    ///
    /// 这就是"原来按 `s` 循环可达的功能现在按分类进入即可"的声明点。
    #[test]
    fn every_merged_panel_lives_in_exactly_one_category() {
        assert_eq!(
            embedded_list_for(SettingsCategory::Sources),
            Some(SettingsFocus::JsSources),
            "JS 音源列表并进音源与歌词"
        );
        assert_eq!(
            embedded_list_for(SettingsCategory::Interface),
            Some(SettingsFocus::StatusBar),
            "状态栏字段（勾选 + 排序）并进界面"
        );
        assert_eq!(
            embedded_list_for(SettingsCategory::Accounts),
            Some(SettingsFocus::QrLogin),
            "扫码登录并进账号与扫码"
        );
        assert_eq!(
            embedded_list_for(SettingsCategory::Data),
            Some(SettingsFocus::LocalPaths),
            "本地音乐目录并进数据与本地库"
        );
        // 每个内嵌列表只有一个归属分类（分类列表里不能出现两份）
        let owners: Vec<SettingsCategory> = SETTINGS_CATEGORIES
            .iter()
            .copied()
            .filter(|category| embedded_list_for(*category).is_some())
            .collect();
        assert_eq!(owners.len(), 4, "只有一个分类吸收一个面板：{owners:?}");

        // 四个列表聚焦时方向键都归它们（`list_owns_direction_keys` 不变）
        for focus in [
            SettingsFocus::JsSources,
            SettingsFocus::LocalPaths,
            SettingsFocus::StatusBar,
            SettingsFocus::QrLogin,
        ] {
            assert!(list_owns_direction_keys(focus));
        }
        assert!(!list_owns_direction_keys(SettingsFocus::Options));

        // 内嵌列表的条目数由分类决定，四类都要有正确的条目来源
        assert_eq!(
            embedded_items(SettingsFocus::StatusBar, 0, 0, 0),
            StatusBarItem::ALL.len()
        );
        assert_eq!(embedded_items(SettingsFocus::JsSources, 3, 9, 9), 3);
        assert_eq!(embedded_items(SettingsFocus::LocalPaths, 3, 9, 9), 9);
        assert_eq!(embedded_items(SettingsFocus::QrLogin, 3, 9, 2), 2);
        assert_eq!(embedded_needed_rows(SettingsFocus::JsSources, 3), 6);
        assert!(embedded_needed_rows(SettingsFocus::QrLogin, 0) >= EMBEDDED_MIN_HEIGHT);
    }

    /// 内嵌列表条目：渲染时记下的矩形，任意列都能命中它自己那一行。
    #[test]
    fn every_embedded_list_row_is_hit_by_its_own_row() {
        let inner = Rect::new(3, 5, 60, 12);
        let mut buf = Buffer::empty(Rect::new(0, 0, 70, 20));
        let mut hits = EmbeddedHits::default();
        for index in 0..8 {
            let rect = Rect::new(inner.x, inner.y + index as u16, inner.width, 1);
            hits.rows.push(render_embedded_row(
                &mut buf,
                rect,
                index,
                Line::from(format!(" 第 {index} 条")),
                (index == 2).then_some(STATUS_BAR_CHECKBOX_WIDTH),
            ));
        }

        for hit in &hits.rows {
            for x in [
                hit.rect.x,
                hit.rect.x + hit.rect.width / 2,
                hit.rect.right() - 1,
            ] {
                assert_eq!(
                    hits.row_at(Position::new(x, hit.rect.y))
                        .map(|hit| hit.index),
                    Some(hit.index),
                    "第 {} 条在自己的矩形内必须命中自己",
                    hit.index
                );
            }
            assert_ne!(
                hits.row_at(Position::new(hit.rect.x, hit.rect.y + 1))
                    .map(|hit| hit.index),
                Some(hit.index),
                "下一条的位置不能命中这一条"
            );
        }
        // 列表之外不命中
        assert_eq!(hits.row_at(Position::new(inner.x, inner.y + 20)), None);
        // 勾选框矩形同样来自账本：只有记了勾选框的行按它命中
        let checkbox_hit = hits.row_at(Position::new(inner.x, inner.y + 2)).unwrap();
        assert_eq!(
            checkbox_hit.checkbox,
            Some(Rect::new(
                inner.x,
                inner.y + 2,
                STATUS_BAR_CHECKBOX_WIDTH,
                1
            ))
        );
        assert!(checkbox_hit.checkbox_at(Position::new(inner.x, inner.y + 2)));
        assert!(!checkbox_hit.checkbox_at(Position::new(inner.x, inner.y + 3)));
        assert!(
            !hits
                .row_at(Position::new(inner.x, inner.y + 3))
                .unwrap()
                .checkbox_at(Position::new(inner.x, inner.y + 3))
        );
    }

    /// 多列布局按行优先填充：0/1 号槽位同一行、不同列，第 2 行从 3 号槽位开始。
    #[test]
    fn setting_row_rect_follows_the_column_layout() {
        let area = Rect::new(0, 0, 60, 24);
        let columns = setting_option_columns(area);
        let count = setting_option_column_count(area.width);
        assert_eq!(count, 2);

        let first = setting_row_rect(&columns, count, 0);
        let second = setting_row_rect(&columns, count, 1);
        let third = setting_row_rect(&columns, count, 2);
        assert_eq!(first.y, second.y, "0/1 号槽位在同一行");
        assert!(second.x > first.x, "1 号槽位在下一列");
        assert_eq!(third.y, first.y + 1);
        assert_eq!(third.x, first.x, "行优先：第 2 行回到第一列");

        let wide = Rect::new(0, 0, 90, 24);
        let columns = setting_option_columns(wide);
        let count = setting_option_column_count(wide.width);
        assert_eq!(count, 3);
        assert_eq!(
            setting_row_rect(&columns, count, 2).y,
            setting_row_rect(&columns, count, 0).y
        );
        assert!(setting_row_rect(&columns, count, 2).x > setting_row_rect(&columns, count, 1).x);
    }

    /// 内嵌列表的命令行按钮：账本矩形必须与渲染出来的 `[a] …` 逐字对齐。
    #[test]
    fn embedded_command_hits_cover_the_rendered_labels() {
        let inner = panel_inner(Rect::new(10, 20, 50, 8));
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 30));
        let hits = render_command_row(
            inner,
            &mut buf,
            LOCAL_PATH_COMMANDS,
            ("", Color::Reset),
            Color::Reset,
        );
        assert_eq!(hits.len(), LOCAL_PATH_COMMANDS.len());

        for (command, (rect, key)) in LOCAL_PATH_COMMANDS.iter().zip(hits.iter()) {
            assert_eq!(command.1, *key);
            let label: String = (rect.x..rect.right())
                .map(|x| buf[(x, rect.y)].symbol().to_string())
                .collect();
            assert_eq!(
                label.replace(' ', ""),
                command.0.replace(' ', ""),
                "命中矩形必须覆盖渲染文本"
            );
        }
        // 每个命令都有自己的按钮，且命令之间不重叠
        for pair in hits.windows(2) {
            assert!(pair[0].0.right() <= pair[1].0.x, "命令按钮不能重叠");
        }
        // 行首空格不是按钮
        assert!(
            !hits
                .iter()
                .any(|(rect, _)| rect.contains(Position::new(inner.x, inner.y))),
            "行首空格不该被当成按钮"
        );
    }

    #[test]
    fn status_bar_drag_reorders_once_at_the_target_position() {
        let mut items = vec![
            StatusBarItem::State,
            StatusBarItem::Source,
            StatusBarItem::Sort,
            StatusBarItem::Song,
        ];

        assert_eq!(
            reorder_status_bar_items(&mut items, StatusBarItem::State, StatusBarItem::Sort),
            Some(2)
        );
        assert_eq!(
            items,
            vec![
                StatusBarItem::Source,
                StatusBarItem::Sort,
                StatusBarItem::State,
                StatusBarItem::Song,
            ]
        );
        assert_eq!(
            reorder_status_bar_items(&mut items, StatusBarItem::Song, StatusBarItem::Source),
            Some(0)
        );
        assert_eq!(items[0], StatusBarItem::Song);
    }
    // ── Phase 1 重构回归测试 ──────────────────────────────────────────────

    /// 行 Rect 与命中一致：渲染时记下的矩形，任意列的横坐标都能命中它自己那一行。
    #[test]
    fn every_rendered_row_rect_is_hit_by_its_own_row() {
        let palette = test_palette();
        let mut rows = SettingsRows::new();
        for index in 0..9 {
            rows.toggle(
                SettingsCategory::Interface,
                "行",
                test_plan(index),
                index % 2 == 0,
                palette,
            );
        }
        let visible = rows.rows_in(SettingsCategory::Interface);
        let area = Rect::new(3, 2, 90, 12);
        let mut buf = Buffer::empty(area);
        let selected = visible.first().map(|row| row.meta.id);
        let hits = render_setting_rows(
            &visible,
            area,
            &mut buf,
            selected,
            None,
            Color::Reset,
            Color::Reset,
        );

        assert_eq!(hits.len(), 9, "9 行都必须留下命中矩形");
        let unique: std::collections::BTreeSet<SettingsRowId> =
            hits.iter().map(|hit| hit.id).collect();
        assert_eq!(unique.len(), 9, "每行一个独立 id");

        for hit in &hits {
            for x in [
                hit.rect.x,
                hit.rect.x + hit.rect.width / 2,
                hit.rect.right() - 1,
            ] {
                assert_eq!(
                    row_hit_at(&hits, Position::new(x, hit.rect.y)),
                    Some(hit.id),
                    "第 {:?} 行在自己的矩形内必须命中自己",
                    hit.id
                );
            }
            // 下一行的位置不能命中这一行
            assert_ne!(
                row_hit_at(&hits, Position::new(hit.rect.x, hit.rect.y + 1)),
                Some(hit.id)
            );
        }
        // 矩形之外不命中
        assert_eq!(row_hit_at(&hits, Position::new(area.x, area.y + 20)), None);
    }

    /// `Enter` 激活：行激活完全由**显式声明的计划**决定。
    ///
    /// 旧版在这里验证"解析键位字符串 → 回灌 `handle_input`"。现在 `RowPlan`
    /// 里已经没有 `Key` 变体：任何一行都不可能再偷偷依赖被删掉的行内快捷键；
    /// `Info` 行仍然一律 `Inert`（绝不触发业务动作）。
    #[test]
    fn enter_activation_follows_the_declared_plan_not_a_replayed_key() {
        let meta = |plan: RowPlan, kind: SettingsRowKind| SettingsRowMeta {
            id: SettingsRowId(7),
            label: "测试行".to_string(),
            key: plan.key_hint().to_string(),
            category: SettingsCategory::Playback,
            kind,
            plan,
        };

        // 取值行：计划就是它的取值菜单
        assert_eq!(
            plan_row_activation(&meta(
                RowPlan::Menu(SettingsEnumPicker::Theme),
                SettingsRowKind::Enum
            )),
            RowPlan::Menu(SettingsEnumPicker::Theme)
        );
        // 直接动作行：计划就是那个动作
        assert_eq!(
            plan_row_activation(&meta(
                RowPlan::Direct(SettingsRowDirectAction::ReloadJsSources),
                SettingsRowKind::Action
            )),
            RowPlan::Direct(SettingsRowDirectAction::ReloadJsSources)
        );
        // 开关行：同样是直接动作，键位列只写 Enter
        let toggle = meta(
            RowPlan::Direct(SettingsRowDirectAction::ToggleMouse),
            SettingsRowKind::Toggle,
        );
        assert_eq!(
            plan_row_activation(&toggle),
            RowPlan::Direct(SettingsRowDirectAction::ToggleMouse)
        );
        assert_eq!(compact_key_label(&toggle.key), "");
        assert_eq!(key_label(&toggle.key), "·");

        // Info 行：绝不触发任何业务动作（哪怕声明了计划）
        assert_eq!(
            plan_row_activation(&meta(
                RowPlan::Direct(SettingsRowDirectAction::ToggleMouse),
                SettingsRowKind::Info
            )),
            RowPlan::Inert
        );
        assert_eq!(
            compact_key_label(&meta(RowPlan::Inert, SettingsRowKind::Info).key),
            "-"
        );
    }

    /// 鼠标命中 → 同一个 id → 与键盘**同一条**激活路径。
    #[test]
    fn mouse_and_keyboard_activation_share_one_path() {
        let palette = test_palette();
        let mut rows = SettingsRows::new();
        let toggle = rows.toggle(
            SettingsCategory::Interface,
            "鼠标控制",
            RowPlan::Direct(SettingsRowDirectAction::ToggleMouse),
            false,
            palette,
        );
        rows.toggle(
            SettingsCategory::Interface,
            "聚合搜索",
            RowPlan::Direct(SettingsRowDirectAction::ToggleAggregateSearch),
            false,
            palette,
        );

        let area = Rect::new(0, 0, 90, 12);
        let visible = rows.rows_in(SettingsCategory::Interface);
        let columns = setting_option_columns(area);
        let column_count = setting_option_column_count(area.width);
        let hits: Vec<SettingsRowHit> = visible
            .iter()
            .enumerate()
            .map(|(slot, row)| SettingsRowHit {
                rect: setting_row_rect(&columns, column_count, slot),
                id: row.meta.id,
            })
            .collect();

        let hit = hits[0];
        let clicked = row_hit_at(&hits, Position::new(hit.rect.x + 1, hit.rect.y))
            .expect("点击行矩形必须命中");
        assert_eq!(clicked, toggle, "命中 id 必须与渲染行一致");

        let metas = rows.metas();
        let keyboard_meta = metas.iter().find(|meta| meta.id == toggle).unwrap();
        let mouse_meta = metas.iter().find(|meta| meta.id == clicked).unwrap();

        // 键盘 Enter/Space 与鼠标点击读的是同一份计划，因此不可能分叉
        assert_eq!(
            plan_row_activation(keyboard_meta),
            plan_row_activation(mouse_meta),
            "键盘与鼠标必须走同一个激活计划"
        );
        assert_eq!(
            plan_row_activation(keyboard_meta),
            RowPlan::Direct(SettingsRowDirectAction::ToggleMouse),
            "「鼠标控制」这一行的激活计划必须是直接切换，而不是回灌某个按键"
        );
    }

    /// 被删掉的行内快捷键不再被设置页吃掉（不会出现"没反应但也不传下去"的死键）。
    ///
    /// 精简后设置页只吃分类 / 导航 / 激活键与少数几个刻意保留的页面级键；
    /// 逐个断言被删的字母键与功能键都**交还**给全局分发。
    #[test]
    fn removed_row_shortcuts_are_not_swallowed_anymore() {
        let resolver = KeybindingResolver::from_config(&KeybindingConfig::default());
        let page = SettingsPage::new();

        // 旧版逐行字母快捷键：全部必须让出去（`v`/`V` 是**新**加的远程歌单窗口键，
        // 不在"已删除"之列，见 `SETTINGS_PAGE_CHAR_KEYS`）。
        let removed = [
            't', 'g', 'w', 'c', 'e', 'N', 'f', 'R', 'Q', 'H', 'u', 'y', 'K', 'T', 'Y', '[', ']',
            'n', 'm', 'z', 'i', 'o', 'O', 'x', 'X', 'S', 'F', 'M', 'B', 'W', 'A', 'E', 'U', 'L',
            'J', 'G', 'I', 'D', 'b', 'h', 'r', 'a', 'd',
        ];
        for character in removed {
            let event = KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE);
            // 设置项面板聚焦（默认）：这些键都不再归设置页
            assert!(
                !page.consumes_key(&event, &resolver),
                "{character} 已经删掉了，不该再被设置页吃掉"
            );
        }

        // 功能键：旧版 F1~F10 / Shift+F1~F8 是设置页的行内快捷键，现在也必须让出去
        let mut removed_keys: Vec<KeyEvent> = (1..=10)
            .map(|number| KeyEvent::new(KeyCode::F(number), KeyModifiers::NONE))
            .collect();
        removed_keys
            .extend((1..=8).map(|number| KeyEvent::new(KeyCode::F(number), KeyModifiers::SHIFT)));
        for event in removed_keys {
            assert!(
                !page.consumes_key(&event, &resolver),
                "{:?} 已经删掉了，不该再被设置页吃掉",
                event.code
            );
        }

        // 保留的页面级键仍然必须被设置页独占（否则会被全局快捷键抢先处理）
        for (focus, character) in [
            (SettingsFocus::Options, 'p'),
            (SettingsFocus::Options, 'P'),
            (SettingsFocus::JsSources, 'a'),
            (SettingsFocus::JsSources, 'd'),
            (SettingsFocus::JsSources, 'h'),
            (SettingsFocus::LocalPaths, 'a'),
            (SettingsFocus::LocalPaths, 'd'),
            (SettingsFocus::LocalPaths, 'r'),
        ] {
            let mut page = SettingsPage::new();
            page.focus = focus;
            let event = KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE);
            assert!(
                page.consumes_key(&event, &resolver),
                "{focus:?} 下 {character} 仍然由设置页独占"
            );
        }
        // `Shift+P`（封面协议）也要独占
        let mut page = SettingsPage::new();
        page.focus = SettingsFocus::Options;
        assert!(page.consumes_key(
            &KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT),
            &resolver
        ));
    }

    /// 宽屏（≥100 列，左分类栏 + 右设置项）与窄屏（单栏）都要"画得出就点得中"。
    #[test]
    fn wide_and_narrow_layouts_both_produce_hittable_rows() {
        let palette = test_palette();
        for width in [120u16, 80u16] {
            let area = Rect::new(0, 0, width, 24);
            let wide = category_sidebar_visible(width);
            assert_eq!(wide, width >= 100, "100 列是左右分栏的阈值");

            // 窄屏单栏时先只看分类列表；宽屏本来就左右并排。
            let list_view = settings_panes(area, wide, false, Some(9), 0.5, CATEGORY_SIDEBAR_WIDTH);
            assert!(list_view.categories.is_some(), "分类栏/分类列表必须可见");
            if wide {
                assert!(list_view.rows.is_some(), "宽屏分类栏与设置项并排");
            } else {
                assert!(list_view.rows.is_none(), "单栏分类视图不显示设置行");
            }

            // 进入分类后（宽屏则本来就并排）显示设置行
            let panes = settings_panes(area, wide, true, Some(9), 0.5, CATEGORY_SIDEBAR_WIDTH);
            let rows_area = panes.rows.expect("设置行区域必须存在");
            let mut rows = SettingsRows::new();
            for index in 0..9 {
                rows.toggle(
                    SettingsCategory::Interface,
                    "行",
                    test_plan(index),
                    index % 2 == 0,
                    palette,
                );
            }
            let visible = rows.rows_in(SettingsCategory::Interface);
            let mut buf = Buffer::empty(area);
            let hits = render_setting_rows(
                &visible,
                rows_area,
                &mut buf,
                None,
                None,
                Color::Reset,
                Color::Reset,
            );
            assert_eq!(hits.len(), 9, "width {width}：9 行都必须可命中");
            for hit in &hits {
                assert_eq!(
                    row_hit_at(&hits, Position::new(hit.rect.x, hit.rect.y)),
                    Some(hit.id),
                    "width {width}：行矩形与命中必须一致"
                );
            }

            match panes.categories {
                Some(category_area) => {
                    assert!(category_area.width > 0 && category_area.height > 0);
                    let mut buf = Buffer::empty(area);
                    let category_hits = render_setting_categories(
                        &SETTINGS_CATEGORIES,
                        category_area,
                        &mut buf,
                        SettingsCategory::Playback,
                        Color::Reset,
                        Color::Reset,
                        Color::Reset,
                        Color::Reset,
                    );
                    assert_eq!(category_hits.len(), SETTINGS_CATEGORIES.len());
                    for (rect, category) in &category_hits {
                        assert_eq!(
                            category_hit_at(&category_hits, Position::new(rect.x, rect.y)),
                            Some(*category),
                            "width {width}：分类栏每行必须命中自己"
                        );
                    }
                    // 分类栏与设置行不重叠：点分类不会误触设置项
                    for hit in &hits {
                        assert_eq!(
                            category_hit_at(&category_hits, Position::new(hit.rect.x, hit.rect.y)),
                            None,
                            "设置行不能被判成分类栏"
                        );
                    }
                }
                None => assert!(!wide, "只有窄屏单栏才没有分类栏"),
            }
        }
    }

    /// 鼠标点分类栏：账本矩形 → `category_hit_at` → `click_category`，
    /// 宽屏只切分类，窄屏同时进入该分类的设置项。
    #[test]
    fn mouse_click_on_a_category_switches_the_category() {
        let area = Rect::new(0, 0, 120, 30);
        let mut page = SettingsPage::new();
        page.last_area = area;
        let panes = settings_panes(area, true, true, Some(9), 0.5, CATEGORY_SIDEBAR_WIDTH);
        let mut buf = Buffer::empty(area);
        let hits = render_setting_categories(
            &SETTINGS_CATEGORIES,
            panes.categories.expect("宽屏分类栏"),
            &mut buf,
            SettingsCategory::Interface,
            Color::Reset,
            Color::Reset,
            Color::Reset,
            Color::Reset,
        );
        let rect = *hits
            .iter()
            .find(|(_, category)| *category == SettingsCategory::Download)
            .map(|(rect, _)| rect)
            .expect("分类栏必须含下载分类");
        // 只用渲染时记下的矩形推算点击位置，不硬编码坐标
        let clicked = category_hit_at(&hits, Position::new(rect.x, rect.y));
        assert_eq!(clicked, Some(SettingsCategory::Download));
        page.click_category(clicked.unwrap());
        assert_eq!(page.category, SettingsCategory::Download);
        assert_eq!(page.focus, SettingsFocus::Options, "点分类后焦点归设置行");
        assert_eq!(page.narrow_pane, NarrowPane::Categories, "宽屏不改窄屏层级");

        // 窄屏：点分类 = 进入该分类的设置项
        let narrow = Rect::new(0, 0, 80, 24);
        let mut page = SettingsPage::new();
        page.last_area = narrow;
        page.narrow_pane = NarrowPane::Categories;
        let panes = settings_panes(narrow, false, false, None, 0.5, CATEGORY_SIDEBAR_WIDTH);
        let mut buf = Buffer::empty(narrow);
        let hits = render_setting_categories(
            &SETTINGS_CATEGORIES,
            panes.categories.expect("窄屏分类列表"),
            &mut buf,
            SettingsCategory::Interface,
            Color::Reset,
            Color::Reset,
            Color::Reset,
            Color::Reset,
        );
        let rect = *hits
            .iter()
            .find(|(_, category)| *category == SettingsCategory::Sources)
            .map(|(rect, _)| rect)
            .unwrap();
        page.click_category(category_hit_at(&hits, Position::new(rect.x, rect.y)).unwrap());
        assert_eq!(page.category, SettingsCategory::Sources);
        assert_eq!(page.narrow_pane, NarrowPane::Rows, "窄屏点分类即进入设置项");
    }

    /// 内嵌列表的键盘交接：设置行光标 ↓ 到底进入列表，列表第一条 ↑ 交还光标。
    ///
    /// 这是删除 `s` 循环后键盘进入内嵌列表的唯一入口（鼠标点击也能进入）。
    #[test]
    fn arrow_keys_hand_the_focus_between_rows_and_the_embedded_list() {
        let palette = test_palette();
        let mut rows = SettingsRows::new();
        rows.toggle(
            SettingsCategory::Interface,
            "第一行",
            test_plan(0),
            true,
            palette,
        );
        let last = rows.toggle(
            SettingsCategory::Interface,
            "最后一行",
            test_plan(1),
            true,
            palette,
        );
        let mut page = SettingsPage::new();
        page.category = SettingsCategory::Interface;
        page.row_metas = rows.metas();

        // 还没到底：焦点留在设置行
        page.focus = SettingsFocus::Options;
        page.row_cursor = Some(last);
        page.enter_embedded_list_if_at_last_row();
        assert_eq!(page.focus, SettingsFocus::StatusBar, "↓ 到底进入状态栏列表");

        // 列表第一条继续 ↑：焦点回设置行，并停在最后一行
        page.leave_embedded_list();
        assert_eq!(page.focus, SettingsFocus::Options);
        assert_eq!(page.row_cursor, Some(last));

        // 没有内嵌列表的分类：↓ 到底也不会把焦点交出去
        page.category = SettingsCategory::Download;
        page.focus = SettingsFocus::Options;
        page.enter_embedded_list_if_at_last_row();
        assert_eq!(page.focus, SettingsFocus::Options);

        // 换分类必须把焦点收回设置行，否则会出现"列表不可见却占着方向键"
        page.focus = SettingsFocus::StatusBar;
        page.set_category(SettingsCategory::Data);
        assert_eq!(page.focus, SettingsFocus::Options);
    }

    /// 内嵌列表条目可点中并选中：账本命中 → `select_embedded_row`。
    ///
    /// 四个列表各验一次：点哪一条就选中哪一条，焦点同时交给该列表。
    #[test]
    fn clicking_an_embedded_row_selects_it() {
        for (category, focus, index) in [
            (SettingsCategory::Sources, SettingsFocus::JsSources, 2),
            (SettingsCategory::Interface, SettingsFocus::StatusBar, 5),
            (SettingsCategory::Accounts, SettingsFocus::QrLogin, 1),
            (SettingsCategory::Data, SettingsFocus::LocalPaths, 3),
        ] {
            let mut page = SettingsPage::new();
            page.category = category;
            // 命中位置由渲染账本推出来（渲染 Rect 与命中 Rect 同源）。
            let inner = Rect::new(0, 2, 60, 10);
            let mut buf = Buffer::empty(Rect::new(0, 0, 60, 20));
            let mut hits = EmbeddedHits::default();
            for slot in 0..6 {
                hits.rows.push(render_embedded_row(
                    &mut buf,
                    Rect::new(inner.x, inner.y + slot as u16, inner.width, 1),
                    slot,
                    Line::from(format!(" 第 {slot} 条")),
                    (focus == SettingsFocus::StatusBar).then_some(STATUS_BAR_CHECKBOX_WIDTH),
                ));
            }
            let hit = hits
                .row_at(Position::new(inner.x + 1, inner.y + index as u16))
                .expect("账本必须命中被点击的条目");
            assert_eq!(hit.index, index);
            page.select_embedded_row(hit.index);
            assert_eq!(page.focus, focus, "{category:?}：点条目后焦点归该列表");
            assert_eq!(
                page.embedded_selection(),
                index,
                "{category:?}：选中的必须是点中的那一条"
            );
        }

        // 没有内嵌列表的分类：点击不会把焦点交给任何列表
        let mut page = SettingsPage::new();
        page.category = SettingsCategory::Download;
        page.select_embedded_row(3);
        assert_eq!(page.focus, SettingsFocus::Options);
        assert_eq!(page.embedded_selection(), 0);
    }

    /// 内嵌列表在它自己的分类里才出现，且列表选中项按 `step_index` 前进/到头即停。
    #[test]
    fn embedded_lists_only_render_in_their_own_category() {
        assert!(
            settings_panes(
                Rect::new(0, 0, 120, 30),
                true,
                true,
                None,
                0.5,
                CATEGORY_SIDEBAR_WIDTH
            )
            .embedded
            .is_none()
        );
        for (category, focus) in [
            (SettingsCategory::Sources, SettingsFocus::JsSources),
            (SettingsCategory::Interface, SettingsFocus::StatusBar),
            (SettingsCategory::Accounts, SettingsFocus::QrLogin),
            (SettingsCategory::Data, SettingsFocus::LocalPaths),
        ] {
            assert_eq!(embedded_list_for(category), Some(focus));
            let needed = embedded_needed_rows(focus, embedded_items(focus, 3, 3, 3));
            let panes = settings_panes(
                Rect::new(0, 0, 120, 30),
                true,
                true,
                Some(needed),
                0.5,
                CATEGORY_SIDEBAR_WIDTH,
            );
            assert!(panes.embedded.is_some(), "{category:?} 必须显示内嵌列表");
        }

        assert_eq!(step_index(0, 5, true), 1);
        assert_eq!(step_index(4, 5, true), 4, "末条继续 ↓ 停在末条");
        assert_eq!(step_index(0, 5, false), 0, "首条继续 ↑ 停在首条");
        assert_eq!(step_index(3, 0, true), 0, "空列表不越界");
    }

    /// 最重要的回归：JS 音源 / 本地目录列表聚焦时 ↑/↓ 归它们，设置行光标让位。
    #[test]
    fn focused_sublists_keep_the_direction_keys() {
        assert!(list_owns_direction_keys(SettingsFocus::JsSources));
        assert!(list_owns_direction_keys(SettingsFocus::LocalPaths));
        assert!(list_owns_direction_keys(SettingsFocus::StatusBar));
        assert!(list_owns_direction_keys(SettingsFocus::QrLogin));
        assert!(!list_owns_direction_keys(SettingsFocus::Options));

        let palette = test_palette();
        let mut rows = SettingsRows::new();
        let first = rows.toggle(
            SettingsCategory::Interface,
            "a",
            test_plan(0),
            true,
            palette,
        );
        let second = rows.toggle(
            SettingsCategory::Interface,
            "b",
            test_plan(1),
            true,
            palette,
        );
        let metas = rows.metas();

        for focus in [
            SettingsFocus::JsSources,
            SettingsFocus::LocalPaths,
            SettingsFocus::StatusBar,
            SettingsFocus::QrLogin,
        ] {
            assert_eq!(
                navigate_row_cursor(
                    focus,
                    &metas,
                    SettingsCategory::Interface,
                    Some(first),
                    true
                ),
                Some(first),
                "{focus:?} 聚焦时设置行光标必须让位"
            );
        }

        assert_eq!(
            navigate_row_cursor(
                SettingsFocus::Options,
                &metas,
                SettingsCategory::Interface,
                Some(first),
                true
            ),
            Some(second),
            "设置项面板聚焦时 ↑/↓ 才移动行光标"
        );

        // 滚动窗口跟着光标，光标永远可见
        assert_eq!(row_window_start(Some(0), 30, 6), 0);
        assert_eq!(row_window_start(Some(29), 30, 6), 24);
        assert_eq!(row_window_start(Some(7), 30, 6), 2);
        assert_eq!(row_window_start(Some(3), 4, 6), 0, "装得下就不滚动");
    }

    /// 当前行画 ▶、hover 行也画 ▶，且两者都不破坏各列对齐（▶ 占的正是原来的前导空格）。
    #[test]
    fn current_and_hovered_rows_are_marked_without_breaking_alignment() {
        let palette = test_palette();
        let mut rows = SettingsRows::new();
        let selected = rows.toggle(
            SettingsCategory::Interface,
            "鼠标控制",
            test_plan(1),
            true,
            palette,
        );
        let hovered = rows.toggle(
            SettingsCategory::Interface,
            "聚合搜索",
            test_plan(1),
            false,
            palette,
        );
        rows.toggle(
            SettingsCategory::Interface,
            "循环导航",
            test_plan(1),
            false,
            palette,
        );
        let visible = rows.rows_in(SettingsCategory::Interface);
        let area = Rect::new(0, 0, 60, 6);
        let mut buf = Buffer::empty(area);
        let hits = render_setting_rows(
            &visible,
            area,
            &mut buf,
            Some(selected),
            Some(hovered),
            Color::Reset,
            Color::Reset,
        );

        let text_at = |rect: Rect| -> String {
            (rect.x..rect.right())
                .map(|x| buf[(x, rect.y)].symbol().to_string())
                .collect()
        };
        let current = text_at(hits[0].rect);
        let hover = text_at(hits[1].rect);
        let plain = text_at(hits[2].rect);

        assert!(current.starts_with("▶"), "当前行必须有 ▶ 前缀：{current:?}");
        assert!(hover.starts_with("▶"), "hover 行必须有 ▶ 前缀：{hover:?}");
        assert!(plain.starts_with(' '), "普通行保持前导空格：{plain:?}");
        assert_eq!(
            UnicodeWidthStr::width(current.as_str()),
            UnicodeWidthStr::width(plain.as_str()),
            "标记不能改变行的显示宽度（各列必须对齐）"
        );
    }

    /// 枚举取值菜单：当前值打勾，选中项映射回现有 AppAction（不新增业务语义）。
    #[test]
    fn enum_menu_marks_the_current_value_and_reuses_existing_actions() {
        assert_eq!(enum_menu_label(true, "FLAC"), "✓ FLAC");
        assert_eq!(enum_menu_label(false, "FLAC"), "  FLAC");

        let quality = settings_menu_app_action(MenuAction::StatusBar(
            StatusBarMenuAction::SetQuality(Quality::Flac),
        ));
        assert!(matches!(
            quality,
            Some(AppAction::SetQuality(Quality::Flac))
        ));

        let mode = settings_menu_app_action(MenuAction::StatusBar(
            StatusBarMenuAction::SetPlayMode("random".to_string()),
        ));
        match mode {
            Some(AppAction::SetPlayMode(value)) => assert_eq!(value, "random"),
            other => panic!("播放模式菜单必须映射成 AppAction::SetPlayMode，实际 {other:?}"),
        }

        let reload =
            settings_menu_app_action(MenuAction::StatusBar(StatusBarMenuAction::ReloadJsSources));
        assert!(matches!(reload, Some(AppAction::ReloadJsSources)));

        // 不属于设置页菜单的动作一律不派发
        assert!(settings_menu_app_action(MenuAction::Back).is_none());
        assert!(
            settings_menu_app_action(MenuAction::SettingChoice {
                row: SETTING_ROW_THEME.to_string(),
                value: "voicefox".to_string(),
            })
            .is_none()
        );
    }

    /// 取某个菜单里打 ✓ 的取值（单选菜单用）。
    fn menu_selected(menu: &EnumMenu) -> Option<&str> {
        menu.choices
            .iter()
            .find(|choice| choice.selected)
            .map(|choice| choice.value.as_str())
    }

    /// 取某个菜单的取值顺序（断言"项数 + 顺序"用）。
    fn menu_values(menu: &EnumMenu) -> Vec<&str> {
        menu.choices
            .iter()
            .map(|choice| choice.value.as_str())
            .collect()
    }

    /// 每个接入的枚举行：取值数量 / 顺序 / ✓ 落在当前值上，全部来自纯数据视图。
    #[test]
    fn every_enum_menu_lists_its_choices_in_order_with_the_current_value_marked() {
        let config = Config::default();
        let skins = crate::theme::skin_names();
        let sources: Vec<&str> = SourceId::all_online()
            .iter()
            .map(SourceId::as_str)
            .collect();

        // 播放音质：与 QUALITY_CHOICES（循环顺序）一致，默认 320K
        let quality = enum_menu(SettingsEnumPicker::Quality, &config, PlayMode::ListLoop);
        assert_eq!(quality.title, " 播放音质 ");
        assert_eq!(menu_values(&quality), ["128K", "320K", "FLAC", "Hi-Res"]);
        assert_eq!(menu_selected(&quality), Some("320K"));

        // 播放模式：当前值来自运行时模式，而不是配置里的持久化副本
        let mode = enum_menu(SettingsEnumPicker::PlayMode, &config, PlayMode::Random);
        assert_eq!(mode.title, " 播放模式 ");
        assert_eq!(
            menu_values(&mode),
            ["list-loop", "single-loop", "random", "list", "none"]
        );
        assert_eq!(menu_selected(&mode), Some("random"));

        // 界面主题：顺序 = skin_names()，默认 voicefox 且标签走 skin_label
        let theme = enum_menu(SettingsEnumPicker::Theme, &config, PlayMode::ListLoop);
        assert_eq!(theme.title, " 界面主题 ");
        assert_eq!(
            menu_values(&theme),
            skins.iter().map(String::as_str).collect::<Vec<_>>()
        );
        assert_eq!(menu_selected(&theme), Some(crate::theme::SKIN_VOICEFOX));
        assert_eq!(theme.choices[0].label, "Voicefox");
        assert_eq!(theme.choices.len(), skins.len(), "每个皮肤一项，不重不漏");

        // 默认音源：顺序 = SourceId::all_online()，默认 kw
        let source = enum_menu(
            SettingsEnumPicker::DefaultSource,
            &config,
            PlayMode::ListLoop,
        );
        assert_eq!(source.title, " 默认音源 ");
        assert_eq!(menu_values(&source), sources);
        assert_eq!(menu_selected(&source), Some("kw"));

        // 解析策略：auto / prefer / only + "目标平台"子菜单（默认无目标平台）
        let policy = enum_menu(
            SettingsEnumPicker::SourcePolicy,
            &config,
            PlayMode::ListLoop,
        );
        assert_eq!(policy.title, " 解析策略 ");
        assert_eq!(menu_values(&policy), ["auto", "prefer", "only"]);
        assert_eq!(menu_selected(&policy), Some("auto"));
        let platform = policy
            .platform
            .as_ref()
            .expect("解析策略菜单必须带目标平台子菜单");
        assert_eq!(
            platform
                .iter()
                .map(|choice| choice.value.as_str())
                .collect::<Vec<_>>(),
            sources
        );
        assert!(
            platform.iter().all(|choice| !choice.selected),
            "默认没有目标平台，不应有任何 ✓"
        );

        // 均衡器：关闭 / 低音增强 / 人声，标签与 equalizer_label 同源
        let equalizer = enum_menu(SettingsEnumPicker::Equalizer, &config, PlayMode::ListLoop);
        assert_eq!(equalizer.title, " 均衡器 ");
        assert_eq!(menu_values(&equalizer), ["off", "bass", "vocal"]);
        assert_eq!(
            equalizer
                .choices
                .iter()
                .map(|choice| choice.label.as_str())
                .collect::<Vec<_>>(),
            ["关闭", "低音增强", "人声"]
        );
        assert_eq!(menu_selected(&equalizer), Some("off"));

        // 状态栏字段：多项各自打勾（默认 5/10 项），顺序 = StatusBarItem::ALL
        let items = enum_menu(
            SettingsEnumPicker::StatusBarItems,
            &config,
            PlayMode::ListLoop,
        );
        assert_eq!(items.title, " 状态栏字段 ");
        assert_eq!(
            menu_values(&items),
            [
                "state",
                "source",
                "sort",
                "song",
                "time",
                "volume",
                "play-mode",
                "quality",
                "queue",
                "js-source-state",
            ]
        );
        let enabled: Vec<&str> = items
            .choices
            .iter()
            .filter(|choice| choice.selected)
            .map(|choice| choice.value.as_str())
            .collect();
        assert_eq!(enabled, ["state", "song", "time", "volume", "queue"]);
    }

    /// 均衡器菜单的每一项都来自 `crate::context` 的**唯一**预设表。
    ///
    /// 上一版设置页为了让枚举能列预设，把频段常量又抄了一份（并写着"必须同步"）。
    /// 这条把菜单与 `EQUALIZER_PRESETS` 按 取值标识 / 标签 / 频段 逐项比对：
    /// 任何一边偷偷改掉，测试都会红。
    #[test]
    fn equalizer_menu_reads_the_single_preset_table_in_context() {
        let config = Config::default();
        let menu = enum_menu(SettingsEnumPicker::Equalizer, &config, PlayMode::ListLoop);

        // 取值顺序与 `next_equalizer_preset` 的循环一致：关闭 → 预设 1 → 预设 2
        assert_eq!(menu_values(&menu), crate::context::EQUALIZER_CHOICE_VALUES);
        assert_eq!(menu_values(&menu), ["off", "bass", "vocal"]);
        assert_eq!(
            crate::context::EQUALIZER_CHOICE_VALUES.len(),
            crate::context::EQUALIZER_PRESETS.len() + 1,
            "取值表 = 关闭 + 唯一定义里的全部预设"
        );

        // "关闭"是空频段，标签由 same-source 的 equalizer_label 得到
        let off = &menu.choices[0];
        assert_eq!(off.value, "off");
        assert!(
            crate::context::equalizer_preset_bands("off")
                .expect("关闭必须有定义")
                .is_empty()
        );
        assert_eq!(
            crate::context::equalizer_label(
                &crate::context::equalizer_preset_bands("off").unwrap()
            ),
            "关闭"
        );

        // 逐项比对：菜单里第 N 个预设 == 唯一定义里的第 N 项（标识 / 标签 / 频段）
        for (choice, (id, label, bands)) in menu
            .choices
            .iter()
            .skip(1)
            .zip(crate::context::EQUALIZER_PRESETS)
        {
            assert_eq!(choice.value, id, "取值标识必须与唯一定义一致");
            assert_eq!(choice.label, label, "标签必须与唯一定义一致");
            assert_eq!(
                crate::context::equalizer_preset_bands(id).expect("预设必须有频段"),
                bands.to_vec(),
                "{id} 的频段必须与唯一定义逐段相等"
            );
            // 标签也确实由同一张表给出（而不是别处抄的字符串）
            assert_eq!(crate::context::equalizer_label(&bands), label);
            // 每一项都是真实的 `EqualizerBand` 频段，不是空表
            assert_eq!(bands.len(), 6);
        }
        assert_eq!(
            menu.choices.len() - 1,
            crate::context::EQUALIZER_PRESETS.len()
        );
    }

    /// ✓ 必须跟着当前配置走，而不是永远落在第一项。
    #[test]
    fn enum_menu_checkmark_tracks_the_current_config() {
        let mut config = Config::default();
        config.player.quality = Quality::Flac;
        config.theme.name = "tokyo-night".to_string();
        config.source.default = SourceId::Wy;
        config.source.policy = SourcePolicy::Only;
        config.source.policy_platform = Some(SourceId::Kg);
        config.player.equalizer_bands = crate::context::equalizer_preset_bands("vocal").unwrap();
        config
            .ui
            .status_bar_items
            .retain(|item| *item != StatusBarItem::State);
        config.ui.status_bar_items.push(StatusBarItem::Source);

        assert_eq!(
            menu_selected(&enum_menu(
                SettingsEnumPicker::Quality,
                &config,
                PlayMode::ListLoop
            )),
            Some("FLAC")
        );
        assert_eq!(
            menu_selected(&enum_menu(
                SettingsEnumPicker::Theme,
                &config,
                PlayMode::ListLoop
            )),
            Some("tokyo-night")
        );
        assert_eq!(
            menu_selected(&enum_menu(
                SettingsEnumPicker::DefaultSource,
                &config,
                PlayMode::ListLoop
            )),
            Some("wy")
        );
        assert_eq!(
            menu_selected(&enum_menu(
                SettingsEnumPicker::SourcePolicy,
                &config,
                PlayMode::ListLoop
            )),
            Some("only")
        );
        assert_eq!(
            menu_selected(&enum_menu(
                SettingsEnumPicker::Equalizer,
                &config,
                PlayMode::ListLoop
            )),
            Some("vocal")
        );
        let policy = enum_menu(
            SettingsEnumPicker::SourcePolicy,
            &config,
            PlayMode::ListLoop,
        );
        let platform = policy.platform.as_ref().unwrap();
        assert_eq!(
            platform
                .iter()
                .find(|choice| choice.selected)
                .map(|choice| choice.value.as_str()),
            Some("kg")
        );
        // 播放模式的 ✓ 跟运行时模式走
        assert_eq!(
            menu_selected(&enum_menu(
                SettingsEnumPicker::PlayMode,
                &config,
                PlayMode::None
            )),
            Some("none")
        );
    }

    // ────────────── 主题入口：找得到、点得动 ──────────────

    /// 「界面主题」这一行：在「界面」分类里、值是"当前主题 + ›"、键位列是 Enter。
    ///
    /// 用生产代码的唯一构造点 `theme_picker_row` 建行，测的就是用户真看到的那一行。
    #[test]
    fn the_theme_row_is_a_visible_picker_in_the_interface_category() {
        let mut config = Config::default();
        config.theme.name = "tokyo-night".to_string();
        let palette = test_palette();
        let mut rows = SettingsRows::new();
        let id = theme_picker_row(&mut rows, &config, palette);
        let metas = rows.metas();
        let meta = metas.iter().find(|meta| meta.id == id).expect("行必须存在");

        assert_eq!(meta.label, "界面主题");
        assert_eq!(
            meta.category,
            SettingsCategory::Interface,
            "换主题的入口必须在「界面」分类里，不能埋在音源/数据里"
        );
        assert_eq!(meta.kind, SettingsRowKind::Enum);
        assert_eq!(
            plan_row_activation(meta),
            RowPlan::Menu(SettingsEnumPicker::Theme),
            "激活这一行 = 打开主题菜单"
        );

        // 值渲染成"Tokyo Night ›"：带箭头才看得出能展开；键位列显示 Enter
        let visible = rows.rows_in(SettingsCategory::Interface);
        let line: String = visible[0]
            .line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(
            line.contains("Tokyo Night ›"),
            "主题行的值必须显示当前主题 + 展开箭头（实际 {line:?}）"
        );
        // Enter 激活这条共性写在面板标题里，行内只留安静的键位圆点
        assert!(
            line.contains(" · "),
            "激活行的键位列应留安静圆点而不是逐行重复 Enter（实际 {line:?}）"
        );
    }

    /// 删除「主题强调色」之后：主题相关的设置行只剩这一条。
    ///
    /// 上一版还有一条只读的「主题强调色」（`theme.accent`，摆在"数据与本地库"里、
    /// 键位列显示 `-`）：它只对内置 `voicefox` 生效，点下去没有任何反应，
    /// 用户因此以为"设置里的主题点不了"。现在它连同 `K::Info` 的只读行一起删了。
    #[test]
    fn the_theme_section_has_no_inert_info_row_left() {
        let mut rows = SettingsRows::new();
        let ids = [theme_picker_row(
            &mut rows,
            &Config::default(),
            test_palette(),
        )];
        let metas = rows.metas();
        assert_eq!(ids.len(), 1, "主题相关只应有「界面主题」一行");

        // 没有任何"标题带主题、又是 Info（点了没反应）"的行
        let info_rows: Vec<&str> = metas
            .iter()
            .filter(|meta| meta.label.contains("主题") && meta.kind == SettingsRowKind::Info)
            .map(|meta| meta.label.as_str())
            .collect();
        assert!(
            info_rows.is_empty(),
            "不该留下会误导用户的主题 Info 行: {info_rows:?}"
        );
        // 该构造点里每一行都必须有可执行语义（有键位 / 是取值菜单 / 是开关）
        for meta in &metas {
            assert!(
                meta.kind != SettingsRowKind::Info || !meta.key.is_empty(),
                "{} 是只读行却没写清用途",
                meta.label
            );
        }
    }

    /// 鼠标点击「界面主题」行 → 走真实渲染账本 → 打开主题菜单。
    ///
    /// `handle_mouse` 需要 `AppContext`（单测环境里没有），但它的关键一步
    /// ——「渲染账本命中 → 行激活计划」——可以在这里原样验证：账本矩形来自
    /// `render_setting_rows`（与生产同一函数），激活走 `activate_row_with`
    /// （`activate_settings_row` 里 `RowActivation::Menu → open_enum_menu` 的上一步）。
    #[test]
    fn clicking_the_theme_row_opens_the_theme_menu() {
        let palette = test_palette();
        let config = Config::default();
        let mut rows = SettingsRows::new();
        let theme_id = theme_picker_row(&mut rows, &config, palette);
        let visible = rows.rows_in(SettingsCategory::Interface);

        let area = Rect::new(0, 0, 120, 24);
        let panes = settings_panes(area, true, true, None, 0.5, CATEGORY_SIDEBAR_WIDTH);
        let rows_area = panes.rows.expect("宽屏必须有设置行区");
        let mut buf = Buffer::empty(area);
        let hits = render_setting_rows(
            &visible,
            rows_area,
            &mut buf,
            None,
            None,
            Color::Reset,
            Color::Reset,
        );

        // 只用渲染出来的矩形推算点击位置，不硬编码坐标
        let rect = hits
            .iter()
            .find(|hit| hit.id == theme_id)
            .map(|hit| hit.rect)
            .expect("主题行必须在命中账本里");
        let clicked = row_hit_at(&hits, Position::new(rect.x, rect.y));
        assert_eq!(clicked, Some(theme_id), "点主题行必须命中它自己");

        let meta = rows
            .metas()
            .into_iter()
            .find(|meta| meta.id == theme_id)
            .expect("行元数据必须存在");
        assert_eq!(
            plan_row_activation(&meta),
            RowPlan::Menu(SettingsEnumPicker::Theme),
            "点这一行必须打开主题菜单，而不是回灌某个按键"
        );

        // 主题菜单：项数 = skin_names()，且恰好一项打 ✓
        let menu = enum_menu(SettingsEnumPicker::Theme, &config, PlayMode::ListLoop);
        assert_eq!(menu.choices.len(), crate::theme::skin_names().len());
        let checked: Vec<&str> = menu
            .choices
            .iter()
            .filter(|choice| choice.selected)
            .map(|choice| choice.value.as_str())
            .collect();
        assert_eq!(checked, [crate::theme::SKIN_VOICEFOX], "只该有一项打 ✓");
    }

    /// `Enter` 与 `Space` 都能激活枚举行（主题菜单三种激活方式都能开）。
    #[test]
    fn enter_and_space_both_activate_enum_rows() {
        assert!(
            row_activates_with_space(SettingsRowKind::Enum),
            "主题等取值行"
        );
        assert!(
            row_activates_with_space(SettingsRowKind::Toggle),
            "布尔开关"
        );
        assert!(
            !row_activates_with_space(SettingsRowKind::Input),
            "输入行不能被空格误开"
        );
        assert!(
            !row_activates_with_space(SettingsRowKind::Info),
            "说明行没有可执行的设置"
        );

        // Enter 与 Space 走的是同一个激活计划
        let mut rows = SettingsRows::new();
        let id = theme_picker_row(&mut rows, &Config::default(), test_palette());
        let meta = rows.metas().into_iter().find(|meta| meta.id == id).unwrap();
        assert_eq!(
            plan_row_activation(&meta),
            RowPlan::Menu(SettingsEnumPicker::Theme)
        );
    }

    /// 「封面协议」行的键位列必须显示**真正生效**的组合键 `Shift+P`。
    ///
    /// 上一版这一行显示 `P`：裸 `P` 实际被「账号与扫码」面板占用，用户照着键位
    /// 列按下去只会跳到账号分类 —— "封面协议根本改不了"。这条把"显示出来的键"
    /// 与"真的会触发封面协议的键"绑在一起断言。
    #[test]
    fn the_cover_protocol_row_advertises_the_key_that_really_cycles_it() {
        // 显示成项目既有的组合键风格（`compact_key_label` 把 Shift+ 缩写成 S+）
        assert_eq!(COVER_PROTOCOL_ROW_KEY, "Shift+P");
        assert_eq!(compact_key_label(COVER_PROTOCOL_ROW_KEY), "S+P");
        assert!(
            UnicodeWidthStr::width(
                format!("[{}]", compact_key_label(COVER_PROTOCOL_ROW_KEY)).as_str()
            ) <= KEY_COLUMN_WIDTH,
            "键位列宽度必须放得下这个组合键"
        );

        // 键位列显示的键 == 真正生效的键：终端对 Shift+P 的两种报法都要能触发
        for shown in [
            KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::SHIFT),
        ] {
            assert!(
                is_cover_protocol_key(&shown),
                "键位列显示的键必须真的循环封面协议"
            );
            assert!(
                !is_accounts_panel_key(&shown),
                "显示出来的键不能是「账号与扫码」面板的入口键"
            );
        }

        // 真实行集里的「封面协议」行：键位列确实写着这个组合键，
        // 而激活计划是直接动作（不再是"回灌按键"）。
        let rows = real_settings_rows(&Config::default());
        let cover = rows
            .metas()
            .into_iter()
            .find(|meta| meta.label == "封面协议")
            .expect("真实行集里必须有「封面协议」");
        assert_eq!(cover.key, COVER_PROTOCOL_ROW_KEY);
        assert_eq!(cover.category, SettingsCategory::Interface);
        assert_eq!(
            plan_row_activation(&cover),
            RowPlan::Direct(SettingsRowDirectAction::CycleCoverProtocol),
            "这一行的激活计划必须直接循环封面协议"
        );

        // 反向：账号面板键与主题循环键都不归封面协议
        assert!(!is_cover_protocol_key(&KeyEvent::new(
            KeyCode::Char('P'),
            KeyModifiers::NONE
        )));
        assert!(!is_cover_protocol_key(&KeyEvent::new(
            KeyCode::Char('p'),
            KeyModifiers::NONE
        )));
        assert!(!is_cover_protocol_key(&KeyEvent::new(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL
        )));
        // 终端对 Shift+P 的两种报法都要认
        assert!(is_cover_protocol_key(&KeyEvent::new(
            KeyCode::Char('p'),
            KeyModifiers::SHIFT
        )));
    }

    /// `P` = 账号与扫码面板，裸 `p` = 界面主题循环。
    ///
    /// 上一版 `p` 也被账号面板吃掉，KEYBINDINGS 里写的"设置页按 `p` 换主题"
    /// 实际按不出来；这条把它们的分工钉死，同时保住 `Shift+P`（封面协议循环）。
    #[test]
    fn p_cycles_the_theme_while_shift_p_opens_the_accounts_panel() {
        let none = KeyModifiers::NONE;
        let p = KeyEvent::new(KeyCode::Char('p'), none);
        let upper = KeyEvent::new(KeyCode::Char('P'), none);
        let upper_shift = KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT);
        let lower_shift = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::SHIFT);

        assert!(!is_accounts_panel_key(&p), "裸 p 必须是主题循环");
        assert!(is_accounts_panel_key(&upper), "裸 P = 账号与扫码");
        // Shift+P 是「封面协议」的循环键：不能被账号面板抢走，否则那一行成死键
        assert!(!is_accounts_panel_key(&upper_shift), "Shift+P 归封面协议");
        assert!(!is_accounts_panel_key(&lower_shift), "Shift+p 归封面协议");
        assert!(
            !is_accounts_panel_key(&KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            "组合键不归账号面板"
        );

        // 裸 `p` 确实会推进主题名（与旧的循环键同一条路径）
        let names = crate::theme::skin_names();
        let next = crate::theme::next_skin_name(crate::theme::SKIN_VOICEFOX);
        assert_ne!(next, crate::theme::SKIN_VOICEFOX, "p 必须真的换主题");
        assert!(names.contains(&next));
    }

    /// 菜单项真的携带 `SettingChoice { row, value }`，且解析策略的平台是通用子菜单。
    #[test]
    fn enum_menu_items_carry_setting_choice_actions() {
        let config = Config::default();

        let theme = enum_menu(SettingsEnumPicker::Theme, &config, PlayMode::ListLoop);
        let skin_count = theme.choices.len();
        let items = theme.into_items(SETTING_ROW_THEME);
        assert_eq!(items.len(), skin_count);
        assert_eq!(items[0].label(), "✓ Voicefox");
        assert_eq!(
            items[0].action(),
            &MenuAction::SettingChoice {
                row: SETTING_ROW_THEME.to_string(),
                value: crate::theme::SKIN_VOICEFOX.to_string(),
            }
        );
        let second = crate::theme::skin_label(&crate::theme::skin_names()[1]);
        assert_eq!(items[1].label(), format!("  {second}"));
        assert_eq!(
            items[1].action(),
            &MenuAction::SettingChoice {
                row: SETTING_ROW_THEME.to_string(),
                value: crate::theme::skin_names()[1].clone(),
            }
        );

        // 状态栏字段：每项一个开关，动作都指向同一行、不同取值
        let items = enum_menu(
            SettingsEnumPicker::StatusBarItems,
            &config,
            PlayMode::ListLoop,
        )
        .into_items(SETTING_ROW_STATUS_BAR_ITEM);
        assert_eq!(items.len(), StatusBarItem::ALL.len());
        assert_eq!(items[0].label(), "✓ 播放状态");
        assert_eq!(
            items[1].action(),
            &MenuAction::SettingChoice {
                row: SETTING_ROW_STATUS_BAR_ITEM.to_string(),
                value: "source".to_string(),
            }
        );

        // 解析策略：3 条策略 + 1 条"目标平台..."子菜单（子菜单自带返回上级）
        let items = enum_menu(
            SettingsEnumPicker::SourcePolicy,
            &config,
            PlayMode::ListLoop,
        )
        .into_items(SETTING_ROW_SOURCE_POLICY);
        assert_eq!(items.len(), 4, "3 条策略 + 目标平台子菜单");
        assert_eq!(
            items[1].action(),
            &MenuAction::SettingChoice {
                row: SETTING_ROW_SOURCE_POLICY.to_string(),
                value: "prefer".to_string(),
            }
        );
        match items[3].action() {
            MenuAction::Submenu {
                title,
                items: platform,
            } => {
                assert!(title.contains("目标平台"), "子菜单标题: {title}");
                assert_eq!(
                    platform.len(),
                    SourceId::all_online().len() + 1,
                    "子菜单末尾自动补一条返回上级"
                );
                assert_eq!(platform.last().unwrap().label(), "← 返回上级");
                assert_eq!(
                    platform[0].action(),
                    &MenuAction::SettingChoice {
                        row: SETTING_ROW_SOURCE_POLICY_PLATFORM.to_string(),
                        value: SourceId::Kw.as_str().to_string(),
                    }
                );
            }
            other => panic!("最后一项必须是目标平台子菜单，实际 {other:?}"),
        }
    }

    /// "行 + 取值 → 配置变更"是纯函数：直接对 `Config::default()` 跑，逐字段断言。
    #[test]
    fn setting_choices_write_the_same_config_fields_as_the_cycle_keys() {
        let mut config = Config::default();

        // 界面主题 → theme.name（与原 'p' 循环键同一个字段）
        let message = apply_setting_choice(&mut config, SETTING_ROW_THEME, "tokyo-night")
            .expect("合法主题必须写入");
        assert_eq!(config.theme.name, "tokyo-night");
        assert_eq!(message, "主题: Tokyo Night");
        // 未知主题不写配置（避免配置与界面显示不一致）
        assert!(apply_setting_choice(&mut config, SETTING_ROW_THEME, "不存在的主题").is_none());
        assert_eq!(config.theme.name, "tokyo-night");

        // 默认音源 → source.default（与原 'v' 循环键同一个字段）
        assert!(apply_setting_choice(&mut config, SETTING_ROW_DEFAULT_SOURCE, "wy").is_some());
        assert_eq!(config.source.default, SourceId::Wy);
        assert!(apply_setting_choice(&mut config, SETTING_ROW_DEFAULT_SOURCE, "nope").is_none());
        assert_eq!(config.source.default, SourceId::Wy);

        // 解析策略 → source.policy
        assert!(apply_setting_choice(&mut config, SETTING_ROW_SOURCE_POLICY, "prefer").is_some());
        assert_eq!(config.source.policy, SourcePolicy::Prefer);
        assert!(apply_setting_choice(&mut config, SETTING_ROW_SOURCE_POLICY, "only").is_some());
        assert_eq!(config.source.policy, SourcePolicy::Only);
        assert!(apply_setting_choice(&mut config, SETTING_ROW_SOURCE_POLICY, "auto").is_some());
        assert_eq!(config.source.policy, SourcePolicy::Auto);
        assert!(apply_setting_choice(&mut config, SETTING_ROW_SOURCE_POLICY, "bogus").is_none());

        // 目标平台 → source.policy_platform
        assert!(
            apply_setting_choice(&mut config, SETTING_ROW_SOURCE_POLICY_PLATFORM, "kg").is_some()
        );
        assert_eq!(config.source.policy_platform, Some(SourceId::Kg));

        // 均衡器 → player.equalizer_bands（与原 F10 循环键同一个字段）
        assert!(apply_setting_choice(&mut config, SETTING_ROW_EQUALIZER, "bass").is_some());
        assert_eq!(
            config.player.equalizer_bands,
            crate::context::equalizer_preset_bands("bass").unwrap()
        );
        assert_eq!(
            crate::context::equalizer_label(&config.player.equalizer_bands),
            "低音增强"
        );
        assert!(apply_setting_choice(&mut config, SETTING_ROW_EQUALIZER, "vocal").is_some());
        assert_eq!(
            crate::context::equalizer_label(&config.player.equalizer_bands),
            "人声"
        );
        assert!(apply_setting_choice(&mut config, SETTING_ROW_EQUALIZER, "off").is_some());
        assert!(config.player.equalizer_bands.is_empty());
        assert_eq!(
            crate::context::equalizer_label(&config.player.equalizer_bands),
            "关闭"
        );
        assert!(apply_setting_choice(&mut config, SETTING_ROW_EQUALIZER, "bogus").is_none());

        // 状态栏字段 → ui.status_bar_items：同一个字段的开启 / 关闭
        let before = config.ui.status_bar_items.clone();
        assert!(before.contains(&StatusBarItem::Song), "默认应显示歌曲名称");
        let message = apply_setting_choice(&mut config, SETTING_ROW_STATUS_BAR_ITEM, "song")
            .expect("已启用的字段必须能关闭");
        assert_eq!(message, "状态栏“歌曲名称”已隐藏");
        assert!(!config.ui.status_bar_items.contains(&StatusBarItem::Song));
        assert_eq!(config.ui.status_bar_items.len(), before.len() - 1);
        let message = apply_setting_choice(&mut config, SETTING_ROW_STATUS_BAR_ITEM, "song")
            .expect("未启用的字段必须能打开");
        assert_eq!(message, "状态栏“歌曲名称”已显示");
        assert!(config.ui.status_bar_items.contains(&StatusBarItem::Song));
        // 重新打开是 push 到末尾（与原 `toggle_status_bar_item` 一致），
        // 所以只比较集合，不比较顺序。
        assert_eq!(config.ui.status_bar_items.len(), before.len());
        assert!(
            before
                .iter()
                .all(|item| config.ui.status_bar_items.contains(item)),
            "开启后集合必须与原来一致"
        );
        assert!(apply_setting_choice(&mut config, SETTING_ROW_STATUS_BAR_ITEM, "bogus").is_none());

        // 未知行一律不写配置
        assert!(apply_setting_choice(&mut config, "unknown-row", "x").is_none());
    }

    /// 音质菜单的取值标识就是它的显示标签，派发前必须能还原成同一档位。
    #[test]
    fn quality_choice_values_round_trip_through_their_labels() {
        for (label, quality) in [
            ("128K", Quality::Low128),
            ("320K", Quality::High320),
            ("FLAC", Quality::Flac),
            ("Hi-Res", Quality::Flac24),
        ] {
            assert_eq!(quality_by_label(label), Some(quality), "{label}");
        }
        assert_eq!(quality_by_label("不存在的档位"), None);
    }

    /// 每一行的激活计划只有两种：取值菜单 或 直接动作（没有"回灌按键"）。
    ///
    /// 这条测试对**真实行集**跑：枚举行接菜单，其余行接直接动作，
    /// 并且每一条计划都必须能被 `Enter` 真正执行。
    #[test]
    fn rows_plan_to_either_a_picker_or_a_direct_action() {
        let rows = real_settings_rows(&Config::default());

        // 取值菜单行：计划里的 picker 必须是这一行的菜单
        for (label, picker) in [
            ("界面主题", SettingsEnumPicker::Theme),
            ("状态栏字段", SettingsEnumPicker::StatusBarItems),
            ("播放音质", SettingsEnumPicker::Quality),
            ("均衡器", SettingsEnumPicker::Equalizer),
            ("播放模式", SettingsEnumPicker::PlayMode),
            ("默认音源", SettingsEnumPicker::DefaultSource),
            ("解析策略", SettingsEnumPicker::SourcePolicy),
            ("音源开关", SettingsEnumPicker::EnabledSources),
            ("歌词偏移", SettingsEnumPicker::LyricOffset),
        ] {
            let meta = rows
                .metas()
                .into_iter()
                .find(|meta| meta.label == label)
                .unwrap_or_else(|| panic!("真实行集里必须有「{label}」"));
            assert_eq!(
                plan_row_activation(&meta),
                RowPlan::Menu(picker),
                "「{label}」必须接取值菜单"
            );
        }

        // 直接动作行：开关 / 动作 / 输入行的计划都必须是 `Direct`
        for (label, expected) in [
            ("鼠标控制", SettingsRowDirectAction::ToggleMouse),
            (
                "保留播放状态",
                SettingsRowDirectAction::ToggleRememberPlaybackState,
            ),
            ("音频设备", SettingsRowDirectAction::EditAudioDevice),
            ("播放速度", SettingsRowDirectAction::CyclePlaybackSpeed),
            ("自动换源", SettingsRowDirectAction::ToggleAutoSource),
            ("网络代理", SettingsRowDirectAction::EditProxy),
            ("重新加载 JS 音源", SettingsRowDirectAction::ReloadJsSources),
            ("扫码登录", SettingsRowDirectAction::QrLoginSelected),
            (
                "导入外部歌单",
                SettingsRowDirectAction::ImportExternalPlaylist,
            ),
            ("下载目录", SettingsRowDirectAction::EditDownloadDir),
            ("导出数据", SettingsRowDirectAction::ExportData),
        ] {
            let meta = rows
                .metas()
                .into_iter()
                .find(|meta| meta.label == label)
                .unwrap_or_else(|| panic!("真实行集里必须有「{label}」"));
            assert_eq!(
                plan_row_activation(&meta),
                RowPlan::Direct(expected),
                "「{label}」必须直接执行动作"
            );
        }

        // 真实行集里没有一行是只读的（否则删除快捷键后它就再也点不动了）
        for meta in rows.metas() {
            assert_ne!(
                plan_row_activation(&meta),
                RowPlan::Inert,
                "「{}」删键后没有任何激活方式",
                meta.label
            );
        }
    }

    /// **强不变量**：真实行集里没有任何一行依赖行内快捷键。
    ///
    /// `RowPlan` 已经没有 `Key` 变体 —— "计划是 `Key(_)`"在类型上就不可能。
    /// 这条把结论钉在**真实行集**上，并额外断言键位列不再广告任何已删除的键
    /// （只剩 `Enter`，唯一例外是刻意保留的 `Shift+P` 封面协议行）。
    #[test]
    fn no_real_settings_row_depends_on_a_per_row_shortcut() {
        let rows = real_settings_rows(&Config::default());
        let mut enter_rows = 0;
        for meta in rows.metas() {
            assert!(
                matches!(
                    plan_row_activation(&meta),
                    RowPlan::Menu(_) | RowPlan::Direct(_)
                ),
                "「{}」的激活计划里不该有按键回灌",
                meta.label
            );
            if meta.key == COVER_PROTOCOL_ROW_KEY {
                // 唯一保留的页面级组合键：显示 `S+P`，真的按得出来
                assert_eq!(compact_key_label(&meta.key), "S+P");
                assert_eq!(meta.label, "封面协议");
                continue;
            }
            assert_eq!(
                compact_key_label(&meta.key),
                "",
                "「{}」的键位列还在广告已删除的快捷键",
                meta.label
            );
            enter_rows += 1;
        }
        assert!(
            enter_rows >= 60,
            "真实行集不该缩水（当前只有 {enter_rows} 行靠 Enter 激活）"
        );

        // 被删掉的键族一个都不该出现在任何一行的键位列里
        let removed_chars = [
            't', 'g', 'w', 'c', 'e', 'f', 'x', 'z', 'i', 'o', 'k', 'y', 'b', 'h', 'n', 'm', 'S',
            'F', 'M', 'V', 'W', 'A', 'E', 'U', 'L', 'J', 'G', 'I', 'D', 'T', 'Y', 'R', 'Q', 'H',
            'N', 'K', 'X', 'O', 'P',
        ];
        for meta in rows.metas() {
            for removed in removed_chars {
                assert_ne!(
                    meta.key,
                    removed.to_string(),
                    "「{}」还在广告已删除的键 {removed}",
                    meta.label
                );
            }
            assert!(
                !meta.key.starts_with("F"),
                "「{}」还在广告已删除的功能键 {}",
                meta.label,
                meta.key
            );
            assert!(
                !meta.key.contains("Shift+F"),
                "「{}」还在广告已删除的组合功能键 {}",
                meta.label,
                meta.key
            );
        }
    }

    /// 内嵌管理列表删键后仍然可用：鼠标点击 / ↑↓ / Enter 都能完成原操作。
    ///
    /// 逐项覆盖增 / 删 / 切换 / 排序 / 登录，并且"渲染出来的命令行按钮"
    /// 与"真正执行的操作"必须一一对应（见 `embedded_command`）。
    #[test]
    fn embedded_lists_stay_reachable_through_mouse_and_enter() {
        // ① 增 / 删 / 重扫 / 检测：命令行按钮字符 → 操作，渲染与执行同源
        for (list, commands) in [
            (SettingsFocus::JsSources, JS_SOURCE_COMMANDS),
            (SettingsFocus::LocalPaths, LOCAL_PATH_COMMANDS),
        ] {
            for (label, character) in commands {
                assert!(
                    embedded_command(list, *character).is_some(),
                    "{list:?} 的命令行按钮「{label}」没有对应操作（点了没反应）"
                );
            }
        }
        assert_eq!(
            embedded_command(SettingsFocus::JsSources, 'a'),
            Some(EmbeddedCommand::AddJsSource)
        );
        assert_eq!(
            embedded_command(SettingsFocus::JsSources, 'h'),
            Some(EmbeddedCommand::CheckSourceHealth)
        );
        assert_eq!(
            embedded_command(SettingsFocus::LocalPaths, 'r'),
            Some(EmbeddedCommand::RescanLocalMusic)
        );
        // 命令行按钮的鼠标命中矩形同样来自渲染账本
        let inner = panel_inner(Rect::new(4, 6, 60, 8));
        let mut buf = Buffer::empty(Rect::new(0, 0, 70, 20));
        let hits = render_command_row(
            inner,
            &mut buf,
            LOCAL_PATH_COMMANDS,
            (" 2 个目录", Color::Reset),
            Color::Reset,
        );
        assert_eq!(hits.len(), LOCAL_PATH_COMMANDS.len());
        for (rect, character) in &hits {
            assert!(
                embedded_command(SettingsFocus::LocalPaths, *character).is_some(),
                "点「{character}」按钮必须有操作"
            );
            assert_eq!(
                EmbeddedHits {
                    rows: Vec::new(),
                    commands: hits.clone(),
                }
                .command_at(Position::new(rect.x, rect.y)),
                Some(*character)
            );
        }

        // ② ↑↓：四条列表都用同一套步进逻辑（0 → 1 → 末尾停住）
        for len in [1usize, 3, 10] {
            assert_eq!(step_index(0, len, true), usize::from(len > 1));
            assert_eq!(step_index(len - 1, len, true), len - 1, "末项继续 ↓ 停住");
            assert_eq!(step_index(0, len, false), 0, "首项继续 ↑ 停住");
        }

        // ③ 状态栏字段的"切换"与"排序"：Enter 走后者的纯函数路径
        let mut config = Config::default();
        let before = config.ui.status_bar_items.clone();
        assert!(before.contains(&StatusBarItem::Song));
        apply_setting_choice(&mut config, SETTING_ROW_STATUS_BAR_ITEM, "song")
            .expect("Enter 必须能切换状态栏字段");
        assert!(!config.ui.status_bar_items.contains(&StatusBarItem::Song));
        apply_setting_choice(&mut config, SETTING_ROW_STATUS_BAR_ITEM, "song").unwrap();
        assert!(config.ui.status_bar_items.contains(&StatusBarItem::Song));

        let mut items = config.ui.status_bar_items.clone();
        let moved = reorder_status_bar_items(&mut items, StatusBarItem::Song, StatusBarItem::State);
        assert_eq!(moved, Some(0), "状态栏字段必须能左右重排");
        assert_eq!(items[0], StatusBarItem::Song);
        assert_eq!(
            items.len(),
            config.ui.status_bar_items.len(),
            "重排不增不减"
        );

        // ④ 扫码登录：Enter / 点击条目的语义（未登录 → 登录，已登录 → 退出登录）
        assert!(matches!(
            qr_login_action(false, SourceId::Wy, QrLoginKind::Standard),
            AppAction::QrLogin(SourceId::Wy, QrLoginKind::Standard)
        ));
        assert!(
            matches!(
                qr_login_action(true, SourceId::Wy, QrLoginKind::Standard),
                AppAction::QrLogout(SourceId::Wy)
            ),
            "删掉 b 之后，退出登录必须靠这一条路"
        );
        // 微信入口点下去必须带微信渠道，否则用户还是被送去 QQ 扫码（issue #43）。
        // 退出登录则与渠道无关：两条渠道写的是同一份登录态。
        assert!(matches!(
            qr_login_action(false, SourceId::Tx, QrLoginKind::WeChat),
            AppAction::QrLogin(SourceId::Tx, QrLoginKind::WeChat)
        ));
        assert!(matches!(
            qr_login_action(true, SourceId::Tx, QrLoginKind::WeChat),
            AppAction::QrLogout(SourceId::Tx)
        ));
    }

    /// 新增的两个取值菜单（音源开关 / 歌词偏移）端到端接上了纯配置写入。
    #[test]
    fn the_new_pickers_write_the_same_config_fields_as_the_deleted_keys() {
        let mut config = Config::default();
        let enabled_before = config.source.enabled.clone();

        // 音源开关：菜单逐项 ✓，选中即切换（与原 `y` 选音源 + `K` 切换一致）
        let menu = enum_menu(
            SettingsEnumPicker::EnabledSources,
            &config,
            PlayMode::ListLoop,
        );
        assert_eq!(menu.title, " 音源开关 ");
        assert_eq!(
            menu.choices.len(),
            SourceId::all_online().len(),
            "每个在线音源一个开关"
        );
        let enabled: Vec<&str> = menu
            .choices
            .iter()
            .filter(|choice| choice.selected)
            .map(|choice| choice.value.as_str())
            .collect();
        assert_eq!(
            enabled.len(),
            enabled_before
                .iter()
                .filter(|source| SourceId::all_online().contains(source))
                .count(),
            "菜单里打 ✓ 的必须正好是已启用的在线音源"
        );

        // 关闭一个已启用的音源
        let target = config.source.enabled[1];
        let message =
            apply_setting_choice(&mut config, SETTING_ROW_SOURCE_ENABLED, target.as_str())
                .expect("已启用的音源必须能关闭");
        assert_eq!(message, format!("{}音源 已禁用", target.as_str()));
        assert!(!config.source.enabled.contains(&target));
        // 再打开：顺序回到 `all_online()` 的顺序
        apply_setting_choice(&mut config, SETTING_ROW_SOURCE_ENABLED, target.as_str()).unwrap();
        assert!(config.source.enabled.contains(&target));

        // 最后一个音源不能关掉，并且要有明确提示
        let mut single = Config::default();
        single.source.enabled = vec![SourceId::Kw];
        single.source.default = SourceId::Kw;
        assert!(
            apply_setting_choice(&mut single, SETTING_ROW_SOURCE_ENABLED, "kw").is_none(),
            "最后一个在线音源不能关"
        );
        assert_eq!(
            config.source.enabled, enabled_before,
            "开关一来一回不能增减音源"
        );
        assert_eq!(
            choice_refusal_message(SETTING_ROW_SOURCE_ENABLED),
            "至少需要保留一个在线音源"
        );
        // 关掉默认音源时，默认音源自动改指第一个启用的音源
        let mut config = Config::default();
        let default = config.source.default;
        apply_setting_choice(&mut config, SETTING_ROW_SOURCE_ENABLED, default.as_str()).unwrap();
        assert_ne!(config.source.default, default);
        assert!(config.source.enabled.contains(&config.source.default));
        // 未知音源不写配置
        assert!(apply_setting_choice(&mut config, SETTING_ROW_SOURCE_ENABLED, "nope").is_none());

        // 歌词偏移：档位表 = 菜单项，正负都一步可达
        let mut config = Config::default();
        let menu = enum_menu(SettingsEnumPicker::LyricOffset, &config, PlayMode::ListLoop);
        assert_eq!(menu.title, " 歌词偏移 ");
        assert_eq!(
            menu.choices
                .iter()
                .map(|choice| choice.value.parse::<i32>().unwrap())
                .collect::<Vec<_>>(),
            LYRIC_OFFSET_CHOICES.to_vec()
        );
        assert!(menu.choices.iter().any(|choice| choice.selected));
        assert!(
            menu.choices
                .iter()
                .any(|choice| choice.value.starts_with('-')),
            "偏移菜单必须包含负值（旧版 `[` / `]` 双向可达）"
        );
        for offset in LYRIC_OFFSET_CHOICES {
            let message =
                apply_setting_choice(&mut config, SETTING_ROW_LYRIC_OFFSET, &offset.to_string())
                    .expect("档位必须能写入");
            assert_eq!(config.lyric.offset, offset);
            assert_eq!(message, format!("歌词偏移: {offset:+} ms"));
        }
        // 表外的值不写配置（防御性）
        assert!(apply_setting_choice(&mut config, SETTING_ROW_LYRIC_OFFSET, "12345").is_none());
        assert!(apply_setting_choice(&mut config, SETTING_ROW_LYRIC_OFFSET, "abc").is_none());
        assert_eq!(config.lyric.offset, *LYRIC_OFFSET_CHOICES.last().unwrap());
    }

    /// 设置项面板聚焦时 Enter / Space 归设置页（否则会被全局播放键抢走）。
    #[test]
    fn options_focus_owns_enter_and_space() {
        let resolver = KeybindingResolver::from_config(&KeybindingConfig::default());
        let mut page = SettingsPage::new();
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let space = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);

        assert_eq!(page.focus, SettingsFocus::Options, "默认聚焦设置项面板");
        assert!(page.consumes_key(&enter, &resolver));
        assert!(page.consumes_key(&space, &resolver));

        page.focus = SettingsFocus::JsSources;
        assert!(!page.consumes_key(&enter, &resolver));
        assert!(!page.consumes_key(&space, &resolver));

        page.focus = SettingsFocus::Options;
        page.narrow_pane = NarrowPane::Rows;
        assert_eq!(page.narrow_pane, NarrowPane::Rows);
    }
}
