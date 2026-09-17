//! 音源管理器：注册、调度、换源匹配
//!
//! 对标 lx-music src/renderer/utils/musicSdk/index.js

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use lx_core::model::leaderboard::LeaderboardInfo;
use lx_core::model::lyric::LyricData;
use lx_core::model::playlist::Playlist;
use lx_core::model::playlist::PlaylistCategory;
use lx_core::model::playlist::{Album, Artist};
use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceHealth, SourceId};
use lx_core::traits::source::{
    FetchError, MusicSource, ParsedLink, SearchError, SearchResult, SongUrl, SourceCapabilities,
};

use crate::apple::AppleSource;
use crate::bili::BiliSource;
use crate::fivesing::FivesingSource;
use crate::jamendo::JamendoSource;
use crate::joox::JooxSource;
use crate::kg::KgSource;
use crate::kw::KwSource;
use crate::local::LocalSource;
use crate::mg::MgSource;
use crate::qianqian::QianqianSource;
use crate::soda::SodaSource;
use crate::tx::TxSource;
use crate::wy::WySource;

struct JsSourceState {
    generation: u64,
    sources: Vec<JsSourceEntry>,
}

struct JsSourceEntry {
    origin: String,
    source: Arc<dyn MusicSource>,
}

/// 音源管理器
pub struct SourceManager {
    sources: HashMap<SourceId, Arc<dyn MusicSource>>,
    /// JS 自定义音源，按配置顺序依次用于解析，并共同参与聚合搜索。
    js_sources: std::sync::RwLock<JsSourceState>,
    /// 本地音乐源（单独存储以便调用扫描等特有方法）
    local_source: Arc<LocalSource>,
    /// 哔哩哔哩音源（单独存储以便调用登录等特有方法）
    bili_source: Arc<BiliSource>,
    default: std::sync::RwLock<SourceId>,
    enabled: std::sync::RwLock<HashSet<SourceId>>,
    /// 歌词"确认无词"负缓存（上次确认时间），避免纯音乐/无词歌曲
    /// 每次播放都触发 JS 音源 + 全源聚合补全的长耗时请求。
    /// 键为 (来源，JS 平台标记，歌曲 id)。
    lyric_negative_cache: std::sync::Mutex<HashMap<(SourceId, Option<String>, String), Instant>>,
}

/// 判断是否为真本地文件歌曲：JS 音源的搜索结果同样标记为 `SourceId::Local`，
/// 但额外带有 `extra["source"]` 平台标记，两者据此区分。
fn is_local_file_song(song: &SongInfo) -> bool {
    song.source == SourceId::Local && !song.extra.contains_key("source")
}

/// JS 音源搜索结果对应的平台内置音源（其 `id` 即该平台的歌曲 id），
/// JS 音源解析失败时可用它回退。
fn builtin_source_for_js(song: &SongInfo) -> Option<SourceId> {
    if song.source != SourceId::Local {
        return None;
    }
    match song.extra.get("source").map(String::as_str) {
        Some("kw") => Some(SourceId::Kw),
        Some("kg") => Some(SourceId::Kg),
        Some("tx") => Some(SourceId::Tx),
        Some("wy") => Some(SourceId::Wy),
        Some("mg") => Some(SourceId::Mg),
        _ => None,
    }
}

impl SourceManager {
    pub fn new(default: SourceId, enabled: &[SourceId]) -> Self {
        let local_source = Arc::new(LocalSource::new());
        let bili_source = Arc::new(BiliSource::new());
        let mut manager = Self {
            sources: HashMap::new(),
            js_sources: std::sync::RwLock::new(JsSourceState {
                generation: 0,
                sources: Vec::new(),
            }),
            local_source: Arc::clone(&local_source),
            bili_source: Arc::clone(&bili_source),
            default: std::sync::RwLock::new(default),
            enabled: std::sync::RwLock::new(enabled.iter().copied().collect()),
            lyric_negative_cache: std::sync::Mutex::new(HashMap::new()),
        };
        // 注册内置音源
        manager.register(Arc::new(KwSource::new()));
        manager.register(Arc::new(KgSource::new()));
        manager.register(Arc::new(MgSource::new()));
        manager.register(Arc::new(TxSource::new()));
        manager.register(Arc::new(WySource::new()));
        manager.register(Arc::new(QianqianSource::new()));
        manager.register(Arc::new(JooxSource::new()));
        manager.register(Arc::new(FivesingSource::new()));
        manager.register(Arc::new(JamendoSource::new()));
        manager.register(Arc::new(AppleSource::new()));
        manager.register(Arc::new(SodaSource::new()));
        manager.register(bili_source);
        // 注册本地音源
        manager.register(local_source);
        manager
    }

    pub fn register(&mut self, source: Arc<dyn MusicSource>) {
        self.sources.insert(source.id(), source);
    }

    /// 开始一次 JS 音源请求。代次和当前音源受同一把锁保护，
    /// 避免旧任务在检查代次后跨过删除或新导入操作写回。
    pub fn begin_js_source_request(&self, clear_current: bool) -> u64 {
        let mut state = self.js_sources.write().unwrap_or_else(|e| e.into_inner());
        state.generation = state.generation.wrapping_add(1);
        if clear_current {
            state.sources.clear();
        }
        state.generation
    }

    pub fn is_js_source_request_current(&self, generation: u64) -> bool {
        self.js_sources
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .generation
            == generation
    }

    pub fn set_js_source_if_current(&self, generation: u64, source: Arc<dyn MusicSource>) -> bool {
        self.set_js_sources_if_current(generation, vec![source])
    }

    pub fn set_js_sources_if_current(
        &self,
        generation: u64,
        sources: Vec<Arc<dyn MusicSource>>,
    ) -> bool {
        self.set_named_js_sources_if_current(
            generation,
            sources
                .into_iter()
                .map(|source| (String::new(), source))
                .collect(),
        )
    }

    pub fn set_named_js_sources_if_current(
        &self,
        generation: u64,
        sources: Vec<(String, Arc<dyn MusicSource>)>,
    ) -> bool {
        let mut state = self.js_sources.write().unwrap_or_else(|e| e.into_inner());
        if state.generation != generation {
            return false;
        }
        state.sources = sources
            .into_iter()
            .map(|(origin, source)| JsSourceEntry { origin, source })
            .collect();
        true
    }

    pub fn insert_js_source_if_current(
        &self,
        generation: u64,
        source: Arc<dyn MusicSource>,
    ) -> bool {
        let mut state = self.js_sources.write().unwrap_or_else(|e| e.into_inner());
        if state.generation != generation {
            return false;
        }
        state.sources.insert(
            0,
            JsSourceEntry {
                origin: String::new(),
                source,
            },
        );
        true
    }

    pub fn clear_js_source_if_current(&self, generation: u64) -> bool {
        let mut state = self.js_sources.write().unwrap_or_else(|e| e.into_inner());
        if state.generation != generation {
            return false;
        }
        state.sources.clear();
        true
    }

    /// 检查是否有 JS 音源
    pub fn has_js_source(&self) -> bool {
        self.js_source_count() > 0
    }

    pub fn js_source_count(&self) -> usize {
        self.js_sources
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .sources
            .len()
    }

    fn js_sources(&self) -> Vec<Arc<dyn MusicSource>> {
        self.js_sources
            .read()
            .unwrap()
            .sources
            .iter()
            .map(|entry| Arc::clone(&entry.source))
            .collect()
    }

    pub fn js_source_names(&self) -> Vec<String> {
        self.js_sources
            .read()
            .unwrap()
            .sources
            .iter()
            .map(|entry| entry.source.name().to_string())
            .collect()
    }

    pub fn js_source_name(&self, index: usize) -> Option<String> {
        self.js_sources
            .read()
            .unwrap()
            .sources
            .get(index)
            .map(|entry| entry.source.name().to_string())
    }

    pub fn js_source_name_for_origin(&self, origin: &str) -> Option<String> {
        self.js_sources
            .read()
            .unwrap()
            .sources
            .iter()
            .find(|entry| entry.origin == origin)
            .map(|entry| entry.source.name().to_string())
    }

    pub fn update_source_preferences(&self, default: SourceId, enabled: &[SourceId]) {
        let enabled: HashSet<_> = enabled.iter().copied().collect();
        let effective_default = if enabled.contains(&default) {
            default
        } else {
            SourceId::all_online()
                .iter()
                .copied()
                .find(|source| enabled.contains(source))
                .unwrap_or(default)
        };
        *self.enabled.write().unwrap_or_else(|e| e.into_inner()) = enabled;
        *self.default.write().unwrap_or_else(|e| e.into_inner()) = effective_default;
    }

    pub fn enabled_sources(&self) -> Vec<SourceId> {
        let enabled = self.enabled.read().unwrap_or_else(|e| e.into_inner());
        SourceId::all_online()
            .iter()
            .copied()
            .filter(|source| enabled.contains(source))
            .collect()
    }

    /// 查询音源声明的能力；未注册的音源按「全部不支持」处理。
    pub fn capabilities(&self, source: SourceId) -> SourceCapabilities {
        self.sources
            .get(&source)
            .map(|source| source.capabilities())
            .unwrap_or_default()
    }

    /// 解析一个分享链接，返回(音源, 解析结果)。
    ///
    /// 域名识别失败或音源尚未实现链接直解时返回可展示的错误信息，
    /// 由界面提示用户改用关键词搜索。
    pub async fn parse_link(&self, link: &str) -> Result<(SourceId, ParsedLink), String> {
        let source_id = SourceId::detect_from_link(link)
            .ok_or_else(|| "无法识别该链接的音源，请改用关键词搜索".to_string())?;
        let source = self
            .sources
            .get(&source_id)
            .ok_or_else(|| format!("{} 音源尚未接入", source_id.display_name()))?;
        if !source.capabilities().link_parse {
            return Err(format!(
                "{} 暂不支持链接直解，请改用关键词搜索",
                source_id.display_name()
            ));
        }
        let parsed = source
            .parse_link(link)
            .await
            .map_err(|error| format!("{} 链接解析失败: {error}", source_id.display_name()))?;
        Ok((source_id, parsed))
    }

    /// 当前启用且满足能力条件的音源，界面据此决定入口显隐。
    pub fn sources_supporting(
        &self,
        supported: impl Fn(&SourceCapabilities) -> bool,
    ) -> Vec<SourceId> {
        self.enabled_sources()
            .into_iter()
            .filter(|source| supported(&self.capabilities(*source)))
            .collect()
    }

    /// 对当前启用的内置及 JS 音源执行轻量搜索检测。
    pub async fn health_check(&self) -> Vec<SourceHealth> {
        let enabled = self.enabled_sources();
        let mut tasks = tokio::task::JoinSet::new();
        for source_id in enabled {
            if let Some(source) = self.sources.get(&source_id).map(Arc::clone) {
                tasks.spawn(check_source(source_id, source));
            }
        }
        for source in self.js_sources() {
            tasks.spawn(check_source(SourceId::Local, source));
        }
        let mut results = Vec::new();
        while let Some(result) = tasks.join_next().await {
            if let Ok(result) = result {
                results.push(result);
            }
        }
        results.sort_by(|a, b| {
            a.id.as_str()
                .cmp(b.id.as_str())
                .then_with(|| a.name.cmp(&b.name))
        });
        results
    }

    pub fn get(&self, id: SourceId) -> Option<Arc<dyn MusicSource>> {
        self.sources.get(&id).map(Arc::clone)
    }

    /// 获取本地音乐源（可直接调用 scan 等特有方法）
    pub fn local_source(&self) -> Arc<LocalSource> {
        Arc::clone(&self.local_source)
    }

    /// 获取哔哩哔哩音源（可直接调用登录等特有方法）
    pub fn bili_source(&self) -> Arc<BiliSource> {
        Arc::clone(&self.bili_source)
    }

    pub fn default_source(&self) -> Arc<dyn MusicSource> {
        let default = *self.default.read().unwrap_or_else(|e| e.into_inner());
        self.sources
            .get(&default)
            .map(Arc::clone)
            .expect("default source must be registered")
    }

    pub async fn search(
        &self,
        keyword: &str,
        page: u32,
        limit: u32,
    ) -> Result<SearchResult, SearchError> {
        let default = *self.default.read().unwrap_or_else(|e| e.into_inner());
        if !self
            .enabled
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&default)
        {
            return Err(SearchError::Other(format!(
                "默认音源 {} 未启用",
                default.as_str()
            )));
        }
        self.default_source().search(keyword, page, limit).await
    }

    pub async fn search_scoped(
        &self,
        keyword: &str,
        page: u32,
        limit: u32,
        source: Option<SourceId>,
    ) -> Result<SearchResult, SearchError> {
        if crate::bili::looks_like_video_reference(keyword) {
            if !self
                .enabled
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&SourceId::Bili)
            {
                return Err(SearchError::Other("哔哩哔哩音源未启用".to_string()));
            }
            return self
                .sources
                .get(&SourceId::Bili)
                .map(Arc::clone)
                .ok_or_else(|| SearchError::Other("哔哩哔哩音源不可用".to_string()))?
                .search(keyword, page, limit)
                .await;
        }
        let Some(source) = source else {
            return self.search_all(keyword, page, limit).await;
        };
        if source != SourceId::Local
            && !self
                .enabled
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&source)
        {
            return Err(SearchError::Other(format!(
                "音源 {} 未启用",
                source.as_str()
            )));
        }
        let source = self
            .sources
            .get(&source)
            .map(Arc::clone)
            .ok_or_else(|| SearchError::Other("音源不可用".to_string()))?;
        source.search(keyword, page, limit).await
    }

    pub async fn search_all(
        &self,
        keyword: &str,
        page: u32,
        limit: u32,
    ) -> Result<SearchResult, SearchError> {
        let per_source_limit = (limit / 2).max(10);
        let mut tasks = tokio::task::JoinSet::new();
        let enabled = self
            .enabled
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for source_id in SourceId::all_online() {
            if !enabled.contains(source_id) {
                continue;
            }
            if let Some(source) = self.sources.get(source_id) {
                let source = Arc::clone(source);
                let keyword = keyword.to_string();
                tasks.spawn(async move {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(10),
                        source.search(&keyword, page, per_source_limit),
                    )
                    .await
                });
            }
        }
        for source in self.js_sources() {
            let keyword = keyword.to_string();
            tasks.spawn(async move {
                tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    source.search(&keyword, page, per_source_limit),
                )
                .await
            });
        }

        let mut items = Vec::new();
        let mut total = 0u32;
        let mut has_more = false;
        let mut success_count = 0usize;
        let mut errors = Vec::new();
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok(Ok(result))) => {
                    success_count += 1;
                    total = total.saturating_add(result.total);
                    has_more |= result.has_more;
                    items.extend(result.items);
                }
                Ok(Ok(Err(error))) => errors.push(error.to_string()),
                Ok(Err(_)) => errors.push("请求超时".to_string()),
                Err(error) => errors.push(error.to_string()),
            }
        }

        if success_count == 0 {
            return Err(SearchError::Other(format!(
                "所有音源搜索失败: {}",
                errors.join("; ")
            )));
        }

        let mut seen = HashSet::new();
        // JS 音源的搜索结果统一标记为 Local，去重键要带上 extra["source"]
        // 平台标记，避免不同平台/不同 JS 音源的同数字 id 互相吞掉。
        items.retain(|song| {
            seen.insert((
                song.source,
                song.extra.get("source").cloned().unwrap_or_default(),
                song.id.clone(),
            ))
        });
        Self::sort_search_results(&mut items, keyword);
        Ok(SearchResult {
            items,
            total,
            has_more,
        })
    }

    fn sort_search_results(items: &mut [SongInfo], keyword: &str) {
        let keyword = Self::normalize_search_text(keyword);
        items.sort_by(|a, b| {
            Self::search_relevance(a, &keyword)
                .cmp(&Self::search_relevance(b, &keyword))
                .then_with(|| {
                    Self::normalize_search_text(&a.name).cmp(&Self::normalize_search_text(&b.name))
                })
                .then_with(|| {
                    Self::normalize_search_text(&a.singer)
                        .cmp(&Self::normalize_search_text(&b.singer))
                })
                .then_with(|| a.source.as_str().cmp(b.source.as_str()))
                .then_with(|| a.id.cmp(&b.id))
        });
    }

    fn search_relevance(song: &SongInfo, keyword: &str) -> u8 {
        if keyword.is_empty() {
            return 5;
        }
        let name = Self::normalize_search_text(&song.name);
        let singer = Self::normalize_search_text(&song.singer);
        let album = Self::normalize_search_text(&song.album_name);
        if name == keyword {
            0
        } else if name.starts_with(keyword) {
            1
        } else if singer == keyword {
            2
        } else if name.contains(keyword) {
            3
        } else if singer.contains(keyword) {
            4
        } else if album.contains(keyword) {
            5
        } else {
            6
        }
    }

    fn normalize_search_text(value: &str) -> String {
        value
            .chars()
            .filter(|character| character.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    }

    pub async fn leaderboard(
        &self,
        source: SourceId,
        board_id: &str,
        page: u32,
        limit: u32,
    ) -> Result<SearchResult, SearchError> {
        self.online_source(source)?
            .get_leaderboard(board_id, page, limit)
            .await
    }

    pub async fn leaderboard_boards(
        &self,
        source: SourceId,
    ) -> Result<Vec<LeaderboardInfo>, SearchError> {
        self.online_source(source)?.get_leaderboard_boards().await
    }

    pub fn leaderboard_sources(&self) -> Vec<SourceId> {
        let enabled = self.enabled.read().unwrap_or_else(|e| e.into_inner());
        SourceId::all_online()
            .iter()
            .copied()
            .filter(|source| enabled.contains(source) && self.sources.contains_key(source))
            .collect()
    }

    /// 指定分类下的歌单；`category` 为空表示「全部/热门」。
    pub async fn playlists(
        &self,
        source: SourceId,
        category: &str,
        page: u32,
    ) -> Result<Vec<Playlist>, FetchError> {
        self.online_source_fetch(source)?
            .get_playlists(category, page)
            .await
    }

    /// 歌单分类目录；不支持分类的音源返回空列表。
    pub async fn playlist_categories(
        &self,
        source: SourceId,
    ) -> Result<Vec<PlaylistCategory>, FetchError> {
        self.online_source_fetch(source)?
            .get_playlist_categories()
            .await
    }

    /// 账号下的个人歌单（需要登录）。
    pub async fn user_playlists(
        &self,
        source: SourceId,
        page: u32,
        limit: u32,
    ) -> Result<Vec<Playlist>, FetchError> {
        self.online_source_fetch(source)?
            .get_user_playlists(page, limit)
            .await
    }

    /// 生成扫码登录会话。
    pub async fn create_qr_login(
        &self,
        source: SourceId,
    ) -> Result<lx_core::model::login::QrLoginSession, FetchError> {
        self.online_source_fetch(source)?.create_qr_login().await
    }

    /// 轮询扫码状态。
    pub async fn check_qr_login(
        &self,
        source: SourceId,
        key: &str,
    ) -> Result<lx_core::model::login::QrLoginResult, FetchError> {
        self.online_source_fetch(source)?.check_qr_login(key).await
    }

    /// 音源是否已登录。
    pub fn is_logged_in(&self, source: SourceId) -> bool {
        self.sources
            .get(&source)
            .is_some_and(|source| source.is_logged_in())
    }

    /// 退出登录并清除本地 cookie。
    pub fn logout(&self, source: SourceId) -> Result<(), String> {
        self.sources
            .get(&source)
            .ok_or_else(|| format!("{} 音源尚未接入", source.display_name()))?
            .logout()
            .map_err(|error| error.to_string())
    }

    pub async fn search_playlists(
        &self,
        source: SourceId,
        keyword: &str,
        page: u32,
    ) -> Result<Vec<Playlist>, SearchError> {
        self.online_source(source)?
            .search_playlists(keyword, page)
            .await
    }

    pub async fn playlist_detail(
        &self,
        source: SourceId,
        playlist_id: &str,
        page: u32,
    ) -> Result<Vec<SongInfo>, FetchError> {
        self.online_source_fetch(source)?
            .get_playlist_detail(playlist_id, page)
            .await
    }

    pub async fn artist_songs(
        &self,
        artist: &Artist,
        page: u32,
        limit: u32,
    ) -> Result<SearchResult, SearchError> {
        self.online_source(artist.source)?
            .get_artist_songs(artist, page, limit)
            .await
    }

    pub async fn artist_albums(
        &self,
        artist: &Artist,
        page: u32,
        limit: u32,
    ) -> Result<Vec<Album>, SearchError> {
        self.online_source(artist.source)?
            .get_artist_albums(artist, page, limit)
            .await
    }

    pub async fn album_songs(
        &self,
        album: &Album,
        page: u32,
        limit: u32,
    ) -> Result<SearchResult, SearchError> {
        self.online_source(album.source)?
            .get_album_songs(album, page, limit)
            .await
    }

    pub fn playlist_sources(&self) -> Vec<SourceId> {
        self.leaderboard_sources()
    }

    fn online_source(&self, source: SourceId) -> Result<Arc<dyn MusicSource>, SearchError> {
        if source == SourceId::Local
            || !self
                .enabled
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&source)
        {
            return Err(SearchError::Other(format!(
                "音源 {} 未启用",
                source.as_str()
            )));
        }
        self.sources
            .get(&source)
            .map(Arc::clone)
            .ok_or_else(|| SearchError::Other("音源不可用".to_string()))
    }

    fn online_source_fetch(&self, source: SourceId) -> Result<Arc<dyn MusicSource>, FetchError> {
        if source == SourceId::Local
            || !self
                .enabled
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&source)
        {
            return Err(FetchError::Other(format!(
                "音源 {} 未启用",
                source.as_str()
            )));
        }
        self.sources
            .get(&source)
            .map(Arc::clone)
            .ok_or_else(|| FetchError::Other("音源不可用".to_string()))
    }

    /// 获取歌曲播放地址。
    /// 在线歌曲优先使用 JS 音源，失败或未导入时回退到对应内置音源。
    pub async fn get_song_url(
        &self,
        song: &SongInfo,
        quality: Quality,
    ) -> Result<SongUrl, FetchError> {
        self.get_song_url_from_js_index(song, quality, 0)
            .await
            .map(|(url, _)| url)
    }

    /// 从指定 JS 音源索引开始解析播放地址。
    ///
    /// 返回成功提供地址的 JS 音源索引；`None` 表示使用了本地、B 站或内置音源。
    /// mpv 实际播放失败后可从下一个索引继续，避免重复使用同一个失效链接。
    pub async fn get_song_url_from_js_index(
        &self,
        song: &SongInfo,
        quality: Quality,
        js_start_index: usize,
    ) -> Result<(SongUrl, Option<usize>), FetchError> {
        // JS 音源引擎本身有较长超时，串行尝试多个音源时最坏耗时会被放大，
        // 这里对整个解析过程加一层外层超时兜底。
        match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.get_song_url_inner(song, quality, js_start_index),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(FetchError::Network("解析播放地址超时".to_string())),
        }
    }

    async fn get_song_url_inner(
        &self,
        song: &SongInfo,
        quality: Quality,
        js_start_index: usize,
    ) -> Result<(SongUrl, Option<usize>), FetchError> {
        // 真本地文件歌曲（非 JS 搜索结果）走本地音源
        if is_local_file_song(song) {
            if let Some(local_src) = self.sources.get(&SourceId::Local) {
                return local_src
                    .get_song_url(song, quality)
                    .await
                    .map(|url| (url, None));
            }
            return Err(FetchError::Other("本地音源不可用".to_string()));
        }
        if song.source == SourceId::Bili {
            return self
                .sources
                .get(&SourceId::Bili)
                .ok_or_else(|| FetchError::Other("哔哩哔哩音源不可用".to_string()))?
                .get_song_url(song, quality)
                .await
                .map(|url| (url, None));
        }
        // 在线歌曲优先使用 JS 音源。
        let mut js_errors = Vec::new();
        for (index, js_source) in self
            .js_sources()
            .into_iter()
            .enumerate()
            .skip(js_start_index)
        {
            match js_source.get_song_url(song, quality).await {
                Ok(result) => return Ok((result, Some(index))),
                Err(error) => js_errors.push(error.to_string()),
            }
        }

        if song.source == SourceId::Local {
            // JS 音源搜到的歌：其 id 就是搜索时记录的平台歌曲 id，
            // JS 音源全部失败后回退到该平台的内置音源。
            let fallback_error = if js_errors.is_empty() {
                FetchError::Other("未加载任何 JS 音源".to_string())
            } else {
                FetchError::Other(format!("JS 音源失败: {}", js_errors.join("; ")))
            };
            let fallback =
                builtin_source_for_js(song).and_then(|id| self.sources.get(&id).map(Arc::clone));
            return match fallback {
                None => Err(fallback_error),
                Some(source) => source
                    .get_song_url(song, quality)
                    .await
                    .map(|url| (url, None))
                    .map_err(|error| {
                        FetchError::Other(format!(
                            "{fallback_error}; 内置{}音源失败: {error}",
                            source.name()
                        ))
                    }),
            };
        }

        let source = self
            .sources
            .get(&song.source)
            .map(Arc::clone)
            .ok_or_else(|| FetchError::Other("歌曲来源不可用".to_string()))?;
        match source.get_song_url(song, quality).await {
            Ok(result) => Ok((result, None)),
            Err(builtin_error) => {
                if !js_errors.is_empty() {
                    Err(FetchError::Other(format!(
                        "JS 音源失败: {}; 内置音源失败: {builtin_error}",
                        js_errors.join("; ")
                    )))
                } else {
                    Err(builtin_error)
                }
            }
        }
    }

    /// 歌曲的内置回退音源：普通歌曲取来源音源；
    /// JS 音源的搜索结果按 `extra["source"]` 平台标记映射到对应内置音源。
    fn builtin_fallback_source(&self, song: &SongInfo) -> Option<Arc<dyn MusicSource>> {
        let id = if song.source == SourceId::Local {
            builtin_source_for_js(song)?
        } else {
            song.source
        };
        self.sources.get(&id).map(Arc::clone)
    }

    /// 优先使用已导入的 lx-music JS 音源获取歌词，空结果时回退到内置搜索源。
    pub async fn get_lyric(&self, song: &SongInfo) -> Result<LyricData, FetchError> {
        match tokio::time::timeout(
            std::time::Duration::from_secs(20),
            self.get_lyric_inner(song),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(FetchError::Network("获取歌词超时".to_string())),
        }
    }

    async fn get_lyric_inner(&self, song: &SongInfo) -> Result<LyricData, FetchError> {
        // 真本地文件歌曲优先读外挂/内嵌歌词，不经过 JS 音源（拿文件路径当
        // 平台歌曲 id 请求必然无效）。无词时返回空数据，由
        // get_lyric_with_fallback 决定是否联网补全。
        if is_local_file_song(song)
            && let Some(local) = self.sources.get(&SourceId::Local)
        {
            return local.get_lyric(song).await;
        }
        for js_source in self.js_sources() {
            if let Ok(data) = js_source.get_lyric(song).await
                && lyric_has_content(&data)
            {
                return Ok(data);
            }
        }

        let source = self
            .builtin_fallback_source(song)
            .ok_or_else(|| FetchError::Other("歌曲来源不可用".to_string()))?;
        source.get_lyric(song).await
    }

    /// 获取歌词，当前音源无内容时自动从同曲候选中补全。
    ///
    /// 只有在所有音源都“确认无词”时才写入负缓存；网络类错误直接向上
    /// 传播，避免一次抖动把歌曲误标为无词。
    pub async fn get_lyric_with_fallback(&self, song: &SongInfo) -> Result<LyricData, FetchError> {
        const NEGATIVE_TTL: std::time::Duration = std::time::Duration::from_secs(600);
        const NEGATIVE_CACHE_LIMIT: usize = 1024;
        // JS 音源的搜索结果都标记为 Local，不同平台的数字 id 可能相同，
        // 缓存键必须带上 extra["source"] 平台标记才能互相区分。
        let cache_key = (
            song.source,
            song.extra.get("source").cloned(),
            song.id.clone(),
        );
        {
            let mut cache = self
                .lyric_negative_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(verified_at) = cache.get(&cache_key)
                && verified_at.elapsed() < NEGATIVE_TTL
            {
                return Err(FetchError::NotFound);
            }
            // 顺手清理过期项并限制容量，防止长跑会话中无限增长
            cache.retain(|_, verified_at| verified_at.elapsed() < NEGATIVE_TTL);
            if cache.len() >= NEGATIVE_CACHE_LIMIT {
                cache.clear();
            }
        }

        let mut last_error: Option<FetchError> = None;
        let mut attempts = |result: Result<LyricData, FetchError>,
                            found: &mut Option<LyricData>| match result {
            Ok(data) if lyric_has_content(&data) => {
                *found = Some(data);
            }
            Ok(_) => {}
            Err(
                e @ (FetchError::Network(_) | FetchError::TooManyRequests | FetchError::Parse(_)),
            ) => {
                last_error = Some(e);
            }
            Err(_) => {}
        };

        let mut found: Option<LyricData> = None;
        attempts(self.get_lyric(song).await, &mut found);
        if found.is_none() {
            for candidate in self.find_music(song).await {
                attempts(self.get_lyric(&candidate).await, &mut found);
                if found.is_some() {
                    tracing::debug!(
                        "lyrics for {} matched from {}",
                        song.name,
                        candidate.source.as_str()
                    );
                    break;
                }
            }
        }

        if let Some(data) = found {
            self.lyric_negative_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&cache_key);
            return Ok(data);
        }

        if let Some(error) = last_error {
            return Err(error);
        }
        self.lyric_negative_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(cache_key, Instant::now());
        Err(FetchError::NotFound)
    }

    /// 优先使用搜索结果中的封面，其次请求 JS 音源，最后回退到内置搜索源。
    pub async fn get_cover_url(&self, song: &SongInfo) -> Result<String, FetchError> {
        if let Some(url) = song.cover_url.as_ref().filter(|url| !url.trim().is_empty()) {
            return Ok(url.clone());
        }
        for js_source in self.js_sources() {
            if let Ok(url) = js_source.get_cover_url(song).await
                && !url.trim().is_empty()
            {
                return Ok(url);
            }
        }

        let source = self
            .builtin_fallback_source(song)
            .ok_or_else(|| FetchError::Other("歌曲来源不可用".to_string()))?;
        source.get_cover_url(song).await
    }

    /// 跨源匹配：在其他音源中搜索同名歌曲
    /// 参考 lx-music findMusic 算法
    pub async fn find_music(&self, song: &SongInfo) -> Vec<SongInfo> {
        let exclude = song.source;
        let keyword = format!("{} {}", song.name, song.singer);

        // 1. 并行搜索所有其他源
        let mut tasks = tokio::task::JoinSet::new();
        let enabled = self
            .enabled
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for id in SourceId::all_online() {
            if *id == exclude || !enabled.contains(id) {
                continue;
            }
            if let Some(source) = self.sources.get(id) {
                let src = Arc::clone(source);
                let kw = keyword.clone();
                tasks.spawn(async move {
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(8),
                        src.search(&kw, 1, 25),
                    )
                    .await
                    {
                        Ok(Ok(result)) => Some(result.items),
                        _ => None,
                    }
                });
            }
        }

        // 2. 收集所有结果
        let mut all: Vec<SongInfo> = Vec::new();
        while let Some(task) = tasks.join_next().await {
            if let Ok(Some(items)) = task {
                all.extend(items);
            }
        }

        // 3. 预处理：计算匹配用字段
        let target_name = crate::filter::filter_str(&song.name).to_lowercase();
        let target_singer =
            crate::filter::filter_str(&crate::filter::sort_singer(&song.singer)).to_lowercase();
        let target_interval = song.duration.as_secs() as i64;

        // 4. 过滤
        all.retain(|s| {
            let f_name = crate::filter::filter_str(&s.name).to_lowercase();
            let f_singer =
                crate::filter::filter_str(&crate::filter::sort_singer(&s.singer)).to_lowercase();
            let f_interval = s.duration.as_secs() as i64;
            let f_album = crate::filter::filter_str(&s.album_name).to_lowercase();

            // 时长匹配 (允许 ±5秒)
            if target_interval > 0 && f_interval > 0 && (target_interval - f_interval).abs() >= 5 {
                return false;
            }

            // 三层匹配
            f_name == target_name && f_singer.contains(&target_singer)
                || f_singer == target_singer && f_name.contains(&target_name)
                || (!f_album.is_empty()
                    && f_album == target_name
                    && f_singer.contains(&target_singer)
                    && f_name.contains(&target_name))
        });

        // 5. 排序（按匹配度）
        all.sort_by(|a, b| {
            let a_score = match_score(a, &target_name, &target_singer, target_interval);
            let b_score = match_score(b, &target_name, &target_singer, target_interval);
            b_score.cmp(&a_score)
        });

        all
    }
}

async fn check_source(id: SourceId, source: Arc<dyn MusicSource>) -> SourceHealth {
    let name = source.name().to_string();
    let started = Instant::now();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        source.search("周杰伦", 1, 1),
    )
    .await;
    let latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    match result {
        Ok(Ok(result)) => SourceHealth {
            id,
            name,
            ok: true,
            latency_ms,
            result_count: result.items.len() as u32,
            detail: "搜索正常".to_string(),
        },
        Ok(Err(error)) => SourceHealth {
            id,
            name,
            ok: false,
            latency_ms,
            result_count: 0,
            detail: error.to_string(),
        },
        Err(_) => SourceHealth {
            id,
            name,
            ok: false,
            latency_ms,
            result_count: 0,
            detail: "请求超时（8 秒）".to_string(),
        },
    }
}

fn lyric_has_content(data: &LyricData) -> bool {
    !data.lyric.trim().is_empty()
        || data
            .tlyric
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        || data
            .rlyric
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        || data
            .lxlyric
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
}

/// 计算匹配分数（越高越匹配）
fn match_score(s: &SongInfo, t_name: &str, t_singer: &str, t_intv: i64) -> i32 {
    let f_name = crate::filter::filter_str(&s.name).to_lowercase();
    let f_singer = crate::filter::filter_str(&crate::filter::sort_singer(&s.singer)).to_lowercase();
    let f_intv = s.duration.as_secs() as i64;

    let mut score = 0;
    if f_singer == *t_singer {
        score += 30;
    }
    if f_name == *t_name {
        score += 30;
    }
    if (f_intv - t_intv).abs() < 2 {
        score += 20;
    }
    if f_name.contains(t_name) || t_name.contains(&f_name) {
        score += 10;
    }
    score
}

#[cfg(test)]
mod tests {
    use super::{SourceManager, lyric_has_content};
    use async_trait::async_trait;
    use lx_core::model::lyric::LyricData;
    use lx_core::model::song::SongInfo;
    use lx_core::model::source::{Quality, SourceId};
    use lx_core::traits::source::{
        FetchError, MusicSource, SearchError, SearchResult, SongUrl, SourceCapabilities,
    };
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[test]
    fn builtin_sources_declare_their_capabilities() {
        let manager = SourceManager::new(SourceId::Kw, SourceId::all_online());

        let kw = manager.capabilities(SourceId::Kw);
        assert!(kw.playlists && kw.leaderboard && kw.album);
        assert!(
            kw.playlist_categories && kw.link_parse,
            "酷我已接入分类与直解"
        );
        assert!(!kw.qr_login, "酷我尚未接入扫码登录");

        let bili = manager.capabilities(SourceId::Bili);
        assert!(bili.login && bili.qr_login && bili.link_parse);

        let kg = manager.capabilities(SourceId::Kg);
        assert!(kg.playlist_search && kg.playlist_categories && kg.link_parse);

        let wy = manager.capabilities(SourceId::Wy);
        assert!(
            wy.playlist_categories && wy.link_parse,
            "网易云是分类与直解的参考实现"
        );

        let tx = manager.capabilities(SourceId::Tx);
        assert!(tx.playlist_search && tx.playlist_categories && tx.link_parse);

        let mg = manager.capabilities(SourceId::Mg);
        assert!(mg.playlist_search && mg.playlist_categories && mg.link_parse);

        // 汽水：歌单与直解可用，但没有分类目录（平台本身不提供）。
        let soda = manager.capabilities(SourceId::Soda);
        assert!(soda.playlist_search && soda.link_parse);
        assert!(!soda.playlist_categories);

        // 尚未实现的新平台按「全部不支持」处理，界面不会展示对应入口。
        assert_eq!(
            manager.capabilities(SourceId::Local),
            SourceCapabilities::default()
        );

        let leaderboards = manager.sources_supporting(|caps| caps.leaderboard);
        assert!(leaderboards.contains(&SourceId::Kw));
        assert!(leaderboards.contains(&SourceId::Bili));
        assert!(!leaderboards.contains(&SourceId::Local));

        // 分类入口只对有实现的音源开放。
        assert_eq!(
            manager.sources_supporting(|caps| caps.playlist_categories),
            vec![
                SourceId::Kw,
                SourceId::Kg,
                SourceId::Tx,
                SourceId::Wy,
                SourceId::Mg,
                SourceId::Qianqian,
                SourceId::Apple
            ]
        );
    }

    struct StubJsSource {
        name: &'static str,
        succeeds: bool,
        calls: Arc<Mutex<Vec<&'static str>>>,
    }

    #[async_trait]
    impl MusicSource for StubJsSource {
        fn id(&self) -> SourceId {
            SourceId::Kw
        }

        fn name(&self) -> &str {
            self.name
        }

        async fn search(
            &self,
            _keyword: &str,
            _page: u32,
            _limit: u32,
        ) -> Result<SearchResult, SearchError> {
            Err(SearchError::Other("unused".to_string()))
        }

        async fn get_song_url(
            &self,
            _song: &SongInfo,
            quality: Quality,
        ) -> Result<SongUrl, FetchError> {
            self.calls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(self.name);
            if !self.succeeds {
                return Err(FetchError::NotFound);
            }
            Ok(SongUrl {
                url: format!("https://example.com/{}.mp3", self.name),
                quality,
                duration: Duration::from_secs(180),
                cover_url: None,
                qualities: vec![quality],
                headers: vec![],
                size: None,
                size_is_advisory: false,
                md5: None,
                candidate_urls: vec![],
                max_chunk_size: 0,
            })
        }

        async fn get_lyric(&self, _song: &SongInfo) -> Result<LyricData, FetchError> {
            Err(FetchError::NotFound)
        }

        async fn get_cover_url(&self, _song: &SongInfo) -> Result<String, FetchError> {
            Err(FetchError::NotFound)
        }

        fn supported_qualities(&self) -> Vec<Quality> {
            vec![Quality::High320]
        }
    }

    #[test]
    fn translated_lyrics_count_as_content() {
        let data = LyricData {
            tlyric: Some("[00:01.00]translation".to_string()),
            ..LyricData::default()
        };

        assert!(lyric_has_content(&data));
        assert!(!lyric_has_content(&LyricData::default()));
    }

    #[test]
    fn stale_js_source_request_cannot_commit() {
        let manager = SourceManager::new(SourceId::Kw, SourceId::all_online());
        let stale_generation = manager.begin_js_source_request(false);
        let current_generation = manager.begin_js_source_request(true);

        assert!(!manager.set_js_source_if_current(
            stale_generation,
            Arc::new(crate::local::LocalSource::new()),
        ));
        assert!(manager.set_js_source_if_current(
            current_generation,
            Arc::new(crate::local::LocalSource::new()),
        ));
        assert!(manager.has_js_source());
    }

    #[tokio::test]
    async fn song_url_uses_js_sources_in_order_until_one_succeeds() {
        let manager = SourceManager::new(SourceId::Kw, SourceId::all_online());
        let generation = manager.begin_js_source_request(true);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let source = |name, succeeds| {
            Arc::new(StubJsSource {
                name,
                succeeds,
                calls: Arc::clone(&calls),
            }) as Arc<dyn MusicSource>
        };
        assert!(manager.set_js_sources_if_current(
            generation,
            vec![
                source("juhe", false),
                source("grass", true),
                source("unused", true),
            ],
        ));
        let song = SongInfo::new(
            "song-id".to_string(),
            SourceId::Kw,
            "song".to_string(),
            "artist".to_string(),
        );

        let result = manager
            .get_song_url(&song, Quality::High320)
            .await
            .expect("the second JS source should resolve the song");

        assert_eq!(result.url, "https://example.com/grass.mp3");
        assert_eq!(
            *calls.lock().unwrap_or_else(|e| e.into_inner()),
            vec!["juhe", "grass"]
        );
    }

    #[tokio::test]
    async fn song_url_can_resume_from_the_next_js_source() {
        let manager = SourceManager::new(SourceId::Kw, SourceId::all_online());
        let generation = manager.begin_js_source_request(true);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let source = |name| {
            Arc::new(StubJsSource {
                name,
                succeeds: true,
                calls: Arc::clone(&calls),
            }) as Arc<dyn MusicSource>
        };
        assert!(
            manager.set_js_sources_if_current(generation, vec![source("juhe"), source("grass")],)
        );
        let song = SongInfo::new(
            "song-id".to_string(),
            SourceId::Kw,
            "song".to_string(),
            "artist".to_string(),
        );

        let (first, first_index) = manager
            .get_song_url_from_js_index(&song, Quality::High320, 0)
            .await
            .unwrap();
        let (second, second_index) = manager
            .get_song_url_from_js_index(&song, Quality::High320, first_index.unwrap() + 1)
            .await
            .unwrap();

        assert_eq!(first.url, "https://example.com/juhe.mp3");
        assert_eq!(second.url, "https://example.com/grass.mp3");
        assert_eq!(second_index, Some(1));
        assert_eq!(
            *calls.lock().unwrap_or_else(|e| e.into_inner()),
            vec!["juhe", "grass"]
        );
    }

    #[test]
    fn named_js_sources_keep_their_origin_and_display_names() {
        let manager = SourceManager::new(SourceId::Kw, SourceId::all_online());
        let generation = manager.begin_js_source_request(true);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let source = |name| {
            Arc::new(StubJsSource {
                name,
                succeeds: true,
                calls: Arc::clone(&calls),
            }) as Arc<dyn MusicSource>
        };

        assert!(manager.set_named_js_sources_if_current(
            generation,
            vec![
                ("https://example.com/juhe.js".to_string(), source("聚合")),
                ("https://example.com/grass.js".to_string(), source("草原")),
            ],
        ));

        assert_eq!(
            manager.js_source_names(),
            vec!["聚合".to_string(), "草原".to_string()]
        );
        assert_eq!(
            manager.js_source_name_for_origin("https://example.com/grass.js"),
            Some("草原".to_string())
        );
    }

    /// JS 音源歌词接口一旦被调用就 panic，用于证明本地歌词路径没有触发请求。
    struct LyricPanicSource;

    #[async_trait]
    impl MusicSource for LyricPanicSource {
        fn id(&self) -> SourceId {
            SourceId::Local
        }

        fn name(&self) -> &str {
            "lyric-panic"
        }

        async fn search(
            &self,
            _keyword: &str,
            _page: u32,
            _limit: u32,
        ) -> Result<SearchResult, SearchError> {
            Err(SearchError::Other("unused".to_string()))
        }

        async fn get_song_url(
            &self,
            _song: &SongInfo,
            _quality: Quality,
        ) -> Result<SongUrl, FetchError> {
            Err(FetchError::NotFound)
        }

        async fn get_lyric(&self, _song: &SongInfo) -> Result<LyricData, FetchError> {
            panic!("本地文件歌曲不应请求 JS 音源歌词");
        }

        async fn get_cover_url(&self, _song: &SongInfo) -> Result<String, FetchError> {
            Err(FetchError::NotFound)
        }

        fn supported_qualities(&self) -> Vec<Quality> {
            vec![Quality::High320]
        }
    }

    #[tokio::test]
    async fn local_file_song_reads_sidecar_lyric_without_network() {
        let dir = std::env::temp_dir().join("voicefox-manager-lyric-test");
        std::fs::create_dir_all(&dir).expect("创建临时目录失败");
        let audio_path = dir.join("song.flac");
        std::fs::write(&audio_path, b"").expect("写入音频占位文件失败");
        std::fs::write(dir.join("song.lrc"), "[00:01.00]本地外挂歌词").expect("写入歌词文件失败");

        let manager = SourceManager::new(SourceId::Kw, SourceId::all_online());
        let generation = manager.begin_js_source_request(true);
        assert!(manager.set_js_source_if_current(generation, Arc::new(LyricPanicSource)));

        let mut song = SongInfo::new(
            audio_path.to_string_lossy().to_string(),
            SourceId::Local,
            "不存在的本地测试歌曲voicefox".to_string(),
            "测试歌手voicefox".to_string(),
        );
        song.file_path = Some(audio_path);

        let data = manager
            .get_lyric_with_fallback(&song)
            .await
            .expect("应读到外挂歌词而不是报错");

        assert_eq!(data.lyric, "[00:01.00]本地外挂歌词");

        std::fs::remove_dir_all(&dir).ok();
    }
}
