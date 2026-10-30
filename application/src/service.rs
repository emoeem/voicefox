use std::future::Future;
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicU64, Ordering},
};

/// fire-and-forget spawn：任务 panic 时向用户发一条通知而不是静默消失。
///
/// 各任务内部的业务 Err 分支自己发 Notification；这里兜住 panic 那条路——
/// 此前歌词/搜索任务 panic 只有全局 hook 的一条日志，用户只看到功能静默失效。
fn spawn_reported<F>(events: broadcast::Sender<ApplicationEvent>, what: &'static str, future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    let task = tokio::spawn(future);
    tokio::spawn(async move {
        if let Err(join_error) = task.await
            && join_error.is_panic()
        {
            let _ = events.send(ApplicationEvent::Notification {
                message: format!("{what}处理异常退出，请重试"),
            });
        }
    });
}

use lx_core::model::source::PlayerState;
use lx_core::traits::player::PlayerBackend;
use lx_lyric::service::LyricService;
use lx_source::manager::SourceManager;
use tokio::sync::broadcast;

use crate::event::{ApplicationCommand, ApplicationEvent};
use crate::services::{
    LyricsService, PlaybackService, PlaylistService, QueueService, SearchService,
};
use crate::state::ApplicationState;

const EVENT_CAPACITY: usize = 256;

#[derive(Clone)]
pub struct ApplicationService {
    state: Arc<RwLock<ApplicationState>>,
    events: broadcast::Sender<ApplicationEvent>,
    playback: PlaybackService,
    search: SearchService,
    queue: QueueService,
    lyrics: LyricsService,
    playlists: PlaylistService,
    request_id: Arc<AtomicU64>,
    playback_request_id: Arc<AtomicU64>,
}

impl ApplicationService {
    pub fn new(
        player: Arc<dyn PlayerBackend>,
        sources: Arc<SourceManager>,
        lyrics: Arc<LyricService>,
        effects: Arc<dyn crate::services::PlaybackEffects>,
    ) -> Self {
        let playback = PlaybackService::new(Arc::clone(&player), Arc::clone(&sources), effects);
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        Self {
            state: Arc::new(RwLock::new(ApplicationState::default())),
            events,
            playback,
            search: SearchService::new(Arc::clone(&sources)),
            queue: QueueService::new(),
            lyrics: LyricsService::new(lyrics),
            playlists: PlaylistService::with_sources(Arc::clone(&sources)),
            request_id: Arc::new(AtomicU64::new(0)),
            playback_request_id: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn subscribe(&self) -> ApplicationEventStream {
        ApplicationEventStream {
            receiver: self.events.subscribe(),
        }
    }

    pub fn state(&self) -> ApplicationState {
        self.state.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn playback(&self) -> PlaybackService {
        self.playback.clone()
    }
    pub fn search(&self) -> SearchService {
        self.search.clone()
    }
    pub fn queue(&self) -> QueueService {
        self.queue.clone()
    }
    pub fn lyrics(&self) -> LyricsService {
        self.lyrics.clone()
    }
    pub fn playlists(&self) -> PlaylistService {
        self.playlists.clone()
    }

    pub async fn dispatch(&self, command: ApplicationCommand) -> Result<(), String> {
        match command {
            ApplicationCommand::Search { keyword, source } => {
                self.start_search(keyword, 1, source, false).await
            }
            ApplicationCommand::SearchMore {
                keyword,
                page,
                source,
            } => self.start_search(keyword, page, source, true).await,
            ApplicationCommand::Play { songs, index } => self.play(songs, index).await,
            ApplicationCommand::PlayCurrent => {
                let (songs, index) = self.queue.snapshot();
                self.play(songs, index).await
            }
            ApplicationCommand::Pause => {
                self.playback.pause();
                self.emit(ApplicationEvent::PlaybackPaused);
                Ok(())
            }
            ApplicationCommand::Resume => {
                self.playback.resume();
                self.emit(ApplicationEvent::PlaybackResumed);
                Ok(())
            }
            ApplicationCommand::Toggle => {
                self.playback.toggle();
                Ok(())
            }
            ApplicationCommand::Stop => {
                self.playback.stop();
                self.emit(ApplicationEvent::PlaybackStopped);
                Ok(())
            }
            ApplicationCommand::Seek(position) => {
                self.playback.seek(position);
                Ok(())
            }
            ApplicationCommand::QueueAdd { song, next } => {
                let was_empty = self.queue.snapshot().0.is_empty();
                self.queue.add(song.clone(), next);
                self.publish_queue();
                if was_empty {
                    let (songs, index) = self.queue.snapshot();
                    if let Some(current) = songs.get(index).cloned() {
                        self.play(songs, index).await?;
                        self.emit(ApplicationEvent::Notification {
                            message: format!("开始播放: {} - {}", current.name, current.singer),
                        });
                    }
                }
                Ok(())
            }
            ApplicationCommand::QueueRemove { index } => {
                self.queue.remove(index);
                self.publish_queue();
                Ok(())
            }
            ApplicationCommand::QueueClear => {
                self.queue.clear();
                self.publish_queue();
                Ok(())
            }
            ApplicationCommand::Next => {
                let (songs, index) = self.queue.snapshot();
                if songs.is_empty() {
                    return Ok(());
                }
                let next = (index + 1) % songs.len();
                self.play(songs, next).await
            }
            ApplicationCommand::Previous => {
                let (songs, index) = self.queue.snapshot();
                if songs.is_empty() {
                    return Ok(());
                }
                let len = songs.len();
                let previous = if index == 0 { len - 1 } else { index - 1 };
                self.play(songs, previous).await
            }
            ApplicationCommand::PlaybackFailed { request_id, error } => {
                if self.request_id.load(Ordering::Acquire) != request_id {
                    return Ok(());
                }
                let (songs, index) = self.queue.snapshot();
                if songs.len() > 1 {
                    let next = (index + 1) % songs.len();
                    self.emit(ApplicationEvent::Notification {
                        message: format!("{}；已跳过当前歌曲", error),
                    });
                    self.play(songs, next).await
                } else {
                    self.emit(ApplicationEvent::PlaybackFailed {
                        request_id,
                        error: error.clone(),
                    });
                    Err(error)
                }
            }
            ApplicationCommand::RestorePlayback {
                songs,
                index,
                position,
                paused,
            } => {
                let _ = (position, paused);
                self.play_with_restore(songs, index, position, paused).await
            }
            ApplicationCommand::LoadLyrics { song } => {
                let generation = self.state().playback.generation;
                let lyrics = self.lyrics.clone();
                let events = self.events.clone();
                let state = Arc::clone(&self.state);
                spawn_reported(events.clone(), "歌词加载", async move {
                    match lyrics.load(&song, generation).await {
                        Ok(value) => {
                            state.write().unwrap_or_else(|e| e.into_inner()).lyrics = Some(value);
                            let _ =
                                events.send(ApplicationEvent::LyricsChanged { song_id: song.id });
                        }
                        Err(error) => {
                            let _ = events.send(ApplicationEvent::Notification {
                                message: format!("歌词加载失败: {error}"),
                            });
                        }
                    }
                });
                Ok(())
            }
            ApplicationCommand::PlaylistOpen { playlist } => {
                self.playlists.upsert(playlist);
                self.publish_playlists();
                Ok(())
            }
            ApplicationCommand::PlaylistSet(playlists) => {
                self.playlists.replace(playlists);
                self.publish_playlists();
                Ok(())
            }
        }
    }

    async fn start_search(
        &self,
        keyword: String,
        page: u32,
        source: Option<lx_core::model::source::SourceId>,
        append: bool,
    ) -> Result<(), String> {
        let request_id = self.request_id.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            state.search.keyword = keyword.clone();
            state.search.source = source;
            state.search.page = page;
            state.search.request_id = request_id;
            if !append {
                state.search.items.clear();
            }
        }
        self.emit(ApplicationEvent::SearchStarted {
            request_id,
            keyword: keyword.clone(),
        });

        let search = self.search.clone();
        let events = self.events.clone();
        let state = Arc::clone(&self.state);
        spawn_reported(events.clone(), "搜索", async move {
            match search.search(&keyword, page, source).await {
                Ok((items, has_more)) => {
                    let mut current = state.write().unwrap_or_else(|e| e.into_inner());
                    if current.search.request_id != request_id {
                        return;
                    }
                    if append {
                        current.search.items.extend(items.clone());
                    } else {
                        current.search.items = items.clone();
                    }
                    current.search.has_more = has_more;
                    current.search.page = page;
                    let _ = events.send(ApplicationEvent::SearchCompleted {
                        request_id,
                        keyword: keyword.clone(),
                        page,
                        append,
                        items,
                        has_more,
                    });
                }
                Err(error) => {
                    if state
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .search
                        .request_id
                        != request_id
                    {
                        return;
                    }
                    let _ = events.send(ApplicationEvent::SearchFailed { request_id, error });
                }
            }
        });
        Ok(())
    }

    fn playback_quality(&self) -> lx_core::model::source::Quality {
        self.playback.quality()
    }
    fn auto_toggle(&self) -> bool {
        self.playback.auto_toggle()
    }

    async fn play_with_restore(
        &self,
        songs: Vec<lx_core::model::song::SongInfo>,
        index: usize,
        position: std::time::Duration,
        paused: bool,
    ) -> Result<(), String> {
        let request_id = self.playback_request_id.fetch_add(1, Ordering::SeqCst) + 1;
        let Some(song) = songs.get(index).cloned() else {
            return Err("播放索引超出队列范围".to_string());
        };
        self.queue.replace(songs, index);
        match self
            .playback
            .play(
                &song,
                self.playback_quality(),
                self.auto_toggle(),
                Some((position, paused)),
                false,
            )
            .await
        {
            Ok((generation, resolved_song)) => {
                let (queue, index) = self.queue.snapshot();
                let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
                state.queue = crate::state::QueueState {
                    songs: queue,
                    index,
                };
                state.playback.state = if paused {
                    PlayerState::Paused
                } else {
                    PlayerState::Playing
                };
                state.playback.song = Some(resolved_song.clone());
                state.playback.generation = generation;
                state.playback.volume = self.playback.volume();
                drop(state);
                self.emit(ApplicationEvent::PlaybackStarted {
                    request_id,
                    song: resolved_song,
                    source_index: self.playback.source_index(),
                });
                self.publish_queue();
                Ok(())
            }
            Err(error) => {
                self.emit(ApplicationEvent::PlaybackFailed {
                    request_id,
                    error: error.clone(),
                });
                Err(error)
            }
        }
    }

    async fn play(
        &self,
        songs: Vec<lx_core::model::song::SongInfo>,
        index: usize,
    ) -> Result<(), String> {
        let (songs, index, bili_note) = self.playback.expand_bili_parts(songs, index).await;
        if let Some(note) = bili_note {
            self.emit(ApplicationEvent::Notification { message: note });
        }
        let request_id = self.playback_request_id.fetch_add(1, Ordering::SeqCst) + 1;
        let Some(song) = songs.get(index).cloned() else {
            return Err("播放索引超出队列范围".to_string());
        };
        self.queue.replace(songs, index);
        match self
            .playback
            .play(
                &song,
                self.playback_quality(),
                self.auto_toggle(),
                None,
                true,
            )
            .await
        {
            Ok((generation, resolved_song)) => {
                let (queue, index) = self.queue.snapshot();
                {
                    let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
                    state.queue = crate::state::QueueState {
                        songs: queue,
                        index,
                    };
                    state.playback.state = PlayerState::Playing;
                    state.playback.song = Some(resolved_song.clone());
                    state.playback.generation = generation;
                    state.playback.volume = self.playback.volume();
                }
                self.emit(ApplicationEvent::PlaybackStarted {
                    request_id,
                    song: resolved_song.clone(),
                    source_index: self.playback.source_index(),
                });
                self.publish_queue();
                let lyrics = self.lyrics.clone();
                let events = self.events.clone();
                let state = Arc::clone(&self.state);
                spawn_reported(events.clone(), "歌词加载", async move {
                    match lyrics.load(&resolved_song, generation).await {
                        Ok(value) => {
                            state.write().unwrap_or_else(|e| e.into_inner()).lyrics = Some(value);
                            let _ = events.send(ApplicationEvent::LyricsChanged {
                                song_id: resolved_song.id.clone(),
                            });
                        }
                        Err(error) => {
                            let _ = events.send(ApplicationEvent::Notification {
                                message: format!("歌词加载失败: {error}"),
                            });
                        }
                    }
                });
                Ok(())
            }
            Err(error) => {
                self.emit(ApplicationEvent::PlaybackFailed {
                    request_id,
                    error: error.clone(),
                });
                Err(error)
            }
        }
    }

    fn publish_queue(&self) {
        let (songs, index) = self.queue.snapshot();
        self.state.write().unwrap_or_else(|e| e.into_inner()).queue = crate::state::QueueState {
            songs: songs.clone(),
            index,
        };
        self.emit(ApplicationEvent::QueueChanged {
            queue: songs,
            index,
        });
    }

    fn publish_playlists(&self) {
        let playlists = self.playlists.snapshot();
        self.state
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .playlists = playlists.clone();
        self.emit(ApplicationEvent::PlaylistChanged { playlists });
    }

    fn emit(&self, event: ApplicationEvent) {
        let is_state = matches!(event, ApplicationEvent::StateChanged(_));
        let _ = self.events.send(event);
        if !is_state {
            let snapshot = self.state();
            let _ = self
                .events
                .send(ApplicationEvent::StateChanged(Box::new(snapshot)));
        }
    }
}

pub struct ApplicationEventStream {
    receiver: broadcast::Receiver<ApplicationEvent>,
}

impl ApplicationEventStream {
    pub async fn recv(&mut self) -> Result<ApplicationEvent, ApplicationEventStreamError> {
        self.receiver
            .recv()
            .await
            .map_err(ApplicationEventStreamError::from)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApplicationEventStreamError {
    #[error("application event stream lagged by {0} events")]
    Lagged(u64),
    #[error("application event stream closed")]
    Closed,
}

impl From<broadcast::error::RecvError> for ApplicationEventStreamError {
    fn from(value: broadcast::error::RecvError) -> Self {
        match value {
            broadcast::error::RecvError::Lagged(value) => Self::Lagged(value),
            broadcast::error::RecvError::Closed => Self::Closed,
        }
    }
}
