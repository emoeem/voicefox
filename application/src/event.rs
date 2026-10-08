use std::time::Duration;

use lx_core::model::playlist::Playlist;
use lx_core::model::song::SongInfo;
use lx_core::model::source::{PlayerState, SourceId};

#[derive(Debug, Clone)]
pub enum ApplicationCommand {
    Search {
        keyword: String,
        source: Option<SourceId>,
    },
    SearchMore {
        keyword: String,
        page: u32,
        source: Option<SourceId>,
    },
    Play {
        songs: Vec<SongInfo>,
        index: usize,
    },
    PlayCurrent,
    Pause,
    Resume,
    Toggle,
    Stop,
    Seek(Duration),
    QueueAdd {
        song: SongInfo,
        next: bool,
    },
    QueueRemove {
        index: usize,
    },
    QueueClear,
    Next,
    Previous,
    PlaybackFailed {
        request_id: u64,
        error: String,
    },
    RestorePlayback {
        songs: Vec<SongInfo>,
        index: usize,
        position: Duration,
        paused: bool,
    },
    LoadLyrics {
        song: SongInfo,
    },
    PlaylistOpen {
        playlist: Playlist,
    },
    PlaylistSet(Vec<Playlist>),
}

#[derive(Debug, Clone)]
// PlaybackStarted 内联 SongInfo 是最大变体：事件只是经通道短暂流转的
// 短命对象，装箱要波及所有构造与匹配点，尺寸收益不值得。
#[allow(clippy::large_enum_variant)]
pub enum ApplicationEvent {
    StateChanged(Box<crate::ApplicationState>),
    SearchStarted {
        request_id: u64,
        keyword: String,
    },
    SearchCompleted {
        request_id: u64,
        keyword: String,
        page: u32,
        append: bool,
        items: Vec<SongInfo>,
        has_more: bool,
    },
    SearchFailed {
        request_id: u64,
        error: String,
    },
    PlaybackStarted {
        request_id: u64,
        song: SongInfo,
        source_index: Option<usize>,
    },
    PlaybackPaused,
    PlaybackResumed,
    PlaybackStopped,
    PlaybackEnded,
    PlaybackFailed {
        request_id: u64,
        error: String,
    },
    QueueChanged {
        queue: Vec<SongInfo>,
        index: usize,
    },
    LyricsChanged {
        song_id: String,
    },
    PlaylistChanged {
        playlists: Vec<Playlist>,
    },
    Notification {
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationEventKind {
    StateChanged,
    Search,
    Playback,
    Queue,
    Lyrics,
    Playlist,
    Notification,
}

impl ApplicationEvent {
    pub fn kind(&self) -> ApplicationEventKind {
        match self {
            Self::StateChanged(_) => ApplicationEventKind::StateChanged,
            Self::SearchStarted { .. }
            | Self::SearchCompleted { .. }
            | Self::SearchFailed { .. } => ApplicationEventKind::Search,
            Self::PlaybackStarted { .. }
            | Self::PlaybackPaused
            | Self::PlaybackResumed
            | Self::PlaybackStopped
            | Self::PlaybackEnded
            | Self::PlaybackFailed { .. } => ApplicationEventKind::Playback,
            Self::QueueChanged { .. } => ApplicationEventKind::Queue,
            Self::LyricsChanged { .. } => ApplicationEventKind::Lyrics,
            Self::PlaylistChanged { .. } => ApplicationEventKind::Playlist,
            Self::Notification { .. } => ApplicationEventKind::Notification,
        }
    }

    pub fn player_state(&self) -> Option<PlayerState> {
        match self {
            Self::PlaybackStarted { .. } => Some(PlayerState::Playing),
            Self::PlaybackPaused => Some(PlayerState::Paused),
            Self::PlaybackResumed => Some(PlayerState::Playing),
            Self::PlaybackStopped => Some(PlayerState::Stopped),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_kind_is_stable_for_frontend_routing() {
        let event = ApplicationEvent::PlaybackPaused;
        assert_eq!(event.kind(), ApplicationEventKind::Playback);
        assert_eq!(event.player_state(), Some(PlayerState::Paused));
    }

    #[test]
    fn queue_and_search_events_have_distinct_kinds() {
        assert_eq!(
            ApplicationEvent::QueueChanged {
                queue: vec![],
                index: 0
            }
            .kind(),
            ApplicationEventKind::Queue
        );
        assert_eq!(
            ApplicationEvent::SearchStarted {
                request_id: 1,
                keyword: "x".into()
            }
            .kind(),
            ApplicationEventKind::Search
        );
    }
}
