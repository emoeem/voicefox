use async_trait::async_trait;
use lx_core::model::config::Config;
use lx_core::model::song::SongInfo;
use lx_core::model::source::{PlayerState, Quality};
use lx_core::traits::player::PlayerBackend;
use lx_lyric::service::LyricService;
use lx_source::manager::SourceManager;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use voicefox_application::{ApplicationService, PlaybackEffects};

mod cover;
pub mod storage;
pub use cover::{CoverService, CoverState, is_usable_remote_url, sweep_temp_files};
pub use storage::{SavedPlayerState, Storage};
pub use voicefox_application::resolve_cover_url;

struct RuntimePlaybackEffects {
    storage: Arc<Storage>,
    cover: Arc<CoverService>,
    config: Arc<RwLock<Config>>,
}
#[async_trait]
impl PlaybackEffects for RuntimePlaybackEffects {
    fn existing_download(&self, song: &SongInfo) -> Option<PathBuf> {
        let c = self.config.read().unwrap_or_else(|e| e.into_inner());
        let dir = if c.download.dir.trim().is_empty() {
            dirs::download_dir().unwrap_or_else(|| PathBuf::from("."))
        } else {
            expand_home(&c.download.dir)
        };
        let stem = sanitize_filename(&format!("{} - {}", song.name, song.singer));
        ["mp3", "flac", "m4a", "aac", "ogg", "opus", "wav"]
            .iter()
            .map(|e| dir.join(format!("{stem}.{e}")))
            .find(|p| p.is_file())
    }
    fn record_history(&self, song: &SongInfo) {
        let n = self
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .player
            .history_limit;
        self.storage.add_history(song, n)
    }
    fn prepare_cover(&self, song: &SongInfo, show: bool) -> Option<String> {
        if show
            && self
                .config
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .ui
                .show_cover
        {
            song.cover_url.clone()
        } else {
            self.cover.clear();
            None
        }
    }
    async fn load_cover(&self, url: Option<String>) -> Result<(), String> {
        self.cover.load(url).await
    }
    async fn cache_cover(&self, url: Option<String>) -> Option<String> {
        self.cover.cache_path(url).await.ok().flatten()
    }
    fn cover_url_is_usable(&self, url: &str) -> bool {
        is_usable_remote_url(url)
    }
    fn album_cover_notification(&self) -> bool {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .notification
            .album_cover
    }
    fn track_change_notification(&self) -> bool {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .notification
            .track_change
    }
    fn fade_in_ms(&self) -> u64 {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .player
            .fade_in_ms
    }
    fn quality(&self) -> Quality {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .player
            .quality
    }
    fn auto_toggle(&self) -> bool {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .source
            .auto_toggle
    }
}

pub struct ApplicationRuntime {
    application: ApplicationService,
    pub config: Arc<RwLock<Config>>,
    pub config_path: PathBuf,
    pub storage: Arc<Storage>,
    pub cover: Arc<CoverService>,
    pub player: Arc<dyn PlayerBackend>,
    pub sources: Arc<SourceManager>,
    pub lyrics: Arc<LyricService>,
}
impl ApplicationRuntime {
    pub fn from_parts(
        config: Config,
        config_path: PathBuf,
        player: Arc<dyn PlayerBackend>,
    ) -> anyhow::Result<Self> {
        lx_source::configure_network(&config.network.proxy_url, config.network.timeout);
        player.set_volume(config.player.volume);
        player.set_playback_speed(config.player.playback_speed);
        player.set_audio_output_device(&config.player.audio_device);
        let sources = Arc::new(SourceManager::new(
            config.source.default,
            &config.source.enabled,
        ));
        let lyrics = Arc::new(LyricService::new(Arc::new(
            lx_lyric::fetcher::SourceLyricFetcher::new(Arc::clone(&sources)),
        )));
        lyrics.set_translation_enabled(config.lyric.show_translation);
        lyrics.set_yrc_enabled(config.lyric.show_yrc);
        lyrics.set_offset_ms(config.lyric.offset);
        let storage = Arc::new(Storage::new());
        storage.trim_history(config.player.history_limit);
        let cover = Arc::new(CoverService::new(
            &config.network.proxy_url,
            config.network.timeout,
        ));
        let shared = Arc::new(RwLock::new(config));
        let effects: Arc<dyn PlaybackEffects> = Arc::new(RuntimePlaybackEffects {
            storage: Arc::clone(&storage),
            cover: Arc::clone(&cover),
            config: Arc::clone(&shared),
        });
        let application = ApplicationService::new(
            Arc::clone(&player),
            Arc::clone(&sources),
            Arc::clone(&lyrics),
            effects,
        );
        Ok(Self {
            application,
            config: shared,
            config_path,
            storage,
            cover,
            player,
            sources,
            lyrics,
        })
    }
    #[cfg(feature = "desktop-mpv")]
    pub fn desktop(config: Config, path: PathBuf) -> anyhow::Result<Self> {
        let p: Arc<dyn PlayerBackend> = Arc::new(lx_player::engine::MpvEngine::new()?);
        Self::from_parts(config, path, p)
    }
    pub fn application(&self) -> ApplicationService {
        self.application.clone()
    }
    pub fn state(&self) -> voicefox_application::ApplicationState {
        self.application.state()
    }
    pub fn player_state(&self) -> PlayerState {
        *self.player.state_watcher().borrow()
    }
}
pub fn default_config_path() -> PathBuf {
    directories::ProjectDirs::from("", "", "voicefox")
        .map(|project| project.config_dir().to_path_buf())
        .unwrap_or_else(|| {
            dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("voicefox")
        })
        .join("config.toml")
}
pub fn load_config() -> anyhow::Result<(Config, PathBuf)> {
    let p = default_config_path();
    if !p.is_file() {
        return Ok((Config::default(), p));
    }
    let text = std::fs::read_to_string(&p)?;
    Ok((toml::from_str(&text).unwrap_or_default(), p))
}
fn expand_home(v: &str) -> PathBuf {
    if let Some(r) = v.strip_prefix("~/")
        && let Some(h) = dirs::home_dir()
    {
        return h.join(r);
    }
    PathBuf::from(v)
}
fn sanitize_filename(v: &str) -> String {
    let mut s = v
        .chars()
        .map(|c| if "/\\:*?\"<>|".contains(c) { '_' } else { c })
        .collect::<String>();
    while s.ends_with('.') || s.ends_with(' ') {
        s.pop();
    }
    if s.is_empty() { "unknown".into() } else { s }
}
