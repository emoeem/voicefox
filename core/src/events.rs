use crossterm::event::{KeyEvent, MouseEvent};
use std::sync::Arc;
use std::time::Duration;

use crate::model::config::SourcePolicy;
use crate::model::song::SongInfo;
use crate::model::source::Quality;
use crate::model::source::SourceHealth;
use crate::model::source::SourceId;

/// 终端输入事件
#[derive(Debug, Clone)]
pub enum InputEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
    Tick,
}

/// 页面标识
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageId {
    Main,
    Search,
    PlayQueue,
    Settings,
}

/// 应用操作（页面返回的结果）
#[derive(Debug, Clone)]
pub enum AppAction {
    Navigate(PageId),
    GoBack,
    Quit,
    /// 播放 / 暂停（底栏「播放状态」段与菜单共用）。
    TogglePlayPause,
    /// 精确设置播放模式；取值是配置里的字符串（`list-loop` / `single-loop` / …）。
    ///
    /// 用字符串而不是枚举，是因为 `PlayMode` 属于 app 层的播放列表实现，
    /// 这里只做"用户点了哪一个"的搬运，转换留在 app 侧。
    SetPlayMode(String),
    /// 精确设置音质偏好（只改偏好，不打断正在播放的歌）。
    SetQuality(Quality),
    /// 设置解析策略：`auto` / `prefer` / `only` + 目标平台。
    SetSourcePolicy {
        policy: SourcePolicy,
        platform: Option<SourceId>,
    },
    /// 重新加载配置里的全部 JS 音源（复用设置页保存时的那条路径）。
    ReloadJsSources,
    PlaySong {
        songs: Vec<SongInfo>,
        index: usize,
    },
    PlaySongAfterFailure {
        songs: Vec<SongInfo>,
        index: usize,
    },
    /// 从当前队列继续播放下一首。歌曲列表以 `Arc` 共享，
    /// 避免每次自动切歌都深拷贝整张队列。
    PlayFromQueue {
        songs: Arc<Vec<SongInfo>>,
        index: usize,
    },
    /// 播放失败跳过后的下一首，保留连续失败计数。
    PlayFromQueueAfterFailure {
        songs: Arc<Vec<SongInfo>>,
        index: usize,
    },
    /// Resolve a search-result Bilibili video before playback; multi-part videos open a picker.
    ResolveBiliParts {
        songs: Vec<SongInfo>,
        index: usize,
        request_id: u64,
    },
    RestorePlayback {
        songs: Vec<SongInfo>,
        index: usize,
        position: Duration,
        start_playback: bool,
        paused: bool,
    },
    AddToQueue {
        song: Box<SongInfo>,
        position: InsertPosition,
    },
    /// Toggle the favorite state of a selected song.  Keeping this as an
    /// application action lets pages without direct storage access (for
    /// example search) use the same behavior as the other song lists.
    ToggleFavoriteSong(Box<SongInfo>),
    /// 下载一首歌到本地下载目录（后台任务，进度在下载面板查看）。
    DownloadSong(Box<SongInfo>),
    RetrySong {
        song: Box<SongInfo>,
    },
    PlaybackFailed {
        request_id: u64,
        error: String,
    },
    Search {
        keyword: String,
        source: Option<SourceId>,
    },
    SearchMore {
        keyword: String,
        page: u32,
        source: Option<SourceId>,
    },
    ShowNotification(Notification),
    ImportSource(String),
    SourceImported {
        url: String,
        generation: u64,
    },
    SourceImportFailed {
        error: String,
        generation: u64,
    },
    CheckSourceHealth,
    SourceHealthChecked {
        results: Vec<SourceHealth>,
    },
    RemoveSource(String),
    RemoveHistory(Box<SongInfo>),
    ClearHistory,
    ScanLocalMusic {
        paths: Vec<String>,
        max_depth: u32,
        /// 用户主动重扫（添加/移除目录、按 r）时强制全量遍历；
        /// 自动触发（启动、进入页面、watcher）走目录签名快路径。
        force: bool,
    },
    /// 从设置页发起的外部歌单导入。文件解析与写盘都在后台任务中完成，
    /// 避免在 TUI 主循环里同步解析大歌单并反复写盘。
    ImportExternalPlaylist(String),
    /// 发起扫码登录。第二个字段是渠道（QQ 音乐有 QQ / 微信两条）。
    QrLogin(SourceId, crate::model::login::QrLoginKind),
    QrLogout(SourceId),
    QrLoginSuccess(SourceId),
    /// 与网易云账号进行双向增量同步（仅增加，不自动删除）。
    SyncNetease,
    /// 与 QQ 音乐账号进行双向增量同步（仅增加，不自动删除）。
    SyncQq,
    /// 把本地自建歌单推送到远端音源（写回，追加不删除）。
    PushLocalPlaylist {
        playlist_id: String,
    },
    /// 把本地收藏（红心）推送到远端音源的“我喜欢”（写回，追加不删除）。
    PushFavorites,
    /// 打开歌手详情页（从歌曲右键菜单进入）。
    ShowArtistDetails(Box<SongInfo>),
    /// 打开专辑详情页（从歌曲右键菜单或歌手页专辑列表进入）。
    ShowAlbumDetails(Box<crate::model::playlist::Album>),
    /// 用户提交某页面的列宽调整结果，需要持久化到 Config。
    CommitColumnResize {
        page_key: String,
        columns: Vec<crate::model::config::TableColumnConfig>,
    },
    /// 用户请求恢复某页面的默认列宽（删除该页面的 table_columns 配置）。
    ResetColumnWidths {
        page_key: String,
    },
    /// 用户请求恢复某页面的默认面板布局（删除该页面的 pane_ratios 配置）。
    ResetPaneLayout {
        page_key: String,
    },
    /// 键盘请求打开当前选中项的上下文菜单（无鼠标环境下的右键替代入口）。
    OpenContextMenu,
    /// 跳到「歌单 · 我的歌单」并定位到指定远端歌单（远程歌单窗口的回车动作）。
    OpenAccountPlaylist(String),
    /// 用户拖拽面板分隔条结束，需要把该比例持久化到 Config。
    CommitPaneRatio {
        page_key: String,
        ratio_key: String,
        ratio: f32,
    },
    None,
}

/// 通知消息
#[derive(Debug, Clone)]
pub struct Notification {
    pub level: NotificationLevel,
    pub title: Option<String>,
    pub message: String,
    pub icon: Option<String>,
    pub in_app: bool,
    pub desktop: bool,
    pub replace_previous: bool,
    pub action_label: Option<String>,
    pub action_url: Option<String>,
    pub created_at: chrono::DateTime<chrono::Local>,
}

impl Notification {
    fn new(level: NotificationLevel, message: impl Into<String>) -> Self {
        Self {
            level,
            title: None,
            message: message.into(),
            icon: None,
            in_app: true,
            desktop: true,
            replace_previous: false,
            action_label: None,
            action_url: None,
            created_at: chrono::Local::now(),
        }
    }

    pub fn error(msg: impl Into<String>) -> Self {
        Self::new(NotificationLevel::Error, msg)
    }

    pub fn info(msg: impl Into<String>) -> Self {
        Self::new(NotificationLevel::Info, msg)
    }

    pub fn success(msg: impl Into<String>) -> Self {
        Self::new(NotificationLevel::Success, msg)
    }

    pub fn warning(msg: impl Into<String>) -> Self {
        Self::new(NotificationLevel::Warn, msg)
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn with_icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    pub fn replacing_previous(mut self) -> Self {
        self.replace_previous = true;
        self
    }

    pub fn tui_only(mut self) -> Self {
        self.desktop = false;
        self
    }

    pub fn desktop_only(mut self) -> Self {
        self.in_app = false;
        self
    }

    pub fn with_action(mut self, label: impl Into<String>, url: impl Into<String>) -> Self {
        self.action_label = Some(label.into());
        self.action_url = Some(url.into());
        self
    }

    pub fn timestamp(&self) -> String {
        self.created_at.format("%H:%M:%S").to_string()
    }

    pub fn age(&self) -> Duration {
        chrono::Local::now()
            .signed_duration_since(self.created_at)
            .to_std()
            .unwrap_or_default()
    }

    pub fn is_expired(&self, lifetime: Duration) -> bool {
        self.age() >= lifetime
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationLevel {
    Info,
    Success,
    Warn,
    Error,
}

/// 插入位置
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertPosition {
    Next,
    End,
}
