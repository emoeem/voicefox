use crossterm::event::{KeyEvent, MouseEvent};
use std::time::Duration;

use crate::model::song::SongInfo;
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
    PlaySong {
        songs: Vec<SongInfo>,
        index: usize,
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
    RemoveSource(String),
    ScanLocalMusic {
        paths: Vec<String>,
        max_depth: u32,
    },
    None,
}

/// 通知消息
#[derive(Debug, Clone)]
pub struct Notification {
    pub level: NotificationLevel,
    pub message: String,
    pub created_at: chrono::DateTime<chrono::Local>,
}

impl Notification {
    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            level: NotificationLevel::Error,
            message: msg.into(),
            created_at: chrono::Local::now(),
        }
    }

    pub fn info(msg: impl Into<String>) -> Self {
        Self {
            level: NotificationLevel::Info,
            message: msg.into(),
            created_at: chrono::Local::now(),
        }
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
    Warn,
    Error,
}

/// 插入位置
#[derive(Debug, Clone)]
pub enum InsertPosition {
    Next,
    End,
}
