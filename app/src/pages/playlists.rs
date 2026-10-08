//! 热门歌单与歌单收藏页面

use std::collections::HashMap;
use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lx_core::events::{AppAction, InsertPosition, Notification};
use lx_core::keybinding::{Action, KeybindingResolver};
use lx_core::model::playlist::Playlist;
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};
use unicode_width::UnicodeWidthChar;

use crate::context::AppContext;
use crate::pages::components::context_menu::MenuHitSource;
use crate::pages::components::hit_test::{PANEL_BORDERS, panel_inner};
use crate::pages::components::source_selector::{SourceSelector, SourceSelectorKey};
use crate::pages::components::splitter::{
    DividerHit, GUTTER, SplitAxis, Splitter, clamp_extent, clamp_ratio, divider_line, ratio_within,
    split_with_gutter,
};
use crate::storage::{CustomPlaylistSummary, local_song_matches_path, same_song_identity};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaylistScope {
    Custom,
    Favorites,
    /// 登录账号下的个人歌单（只读镜像自远程缓存，当前仅网易云）。
    Account(SourceId),
    Source(SourceId),
}

/// 网易云远程歌单在页面内的**虚拟音源标识**。
///
/// 账号歌单不是任何网络音源的公开歌单，但下游（歌曲加载、提示文案）只需要
/// 一个能配对的 id，因此用这个前缀把「来自远程缓存的账号歌单」和
/// 「当前音源接口返回的歌单」区分开，避免再去改一遍 `SourceId`。
const NETEASE_ACCOUNT_ID_PREFIX: &str = "netease-account:";

/// 远程缓存里的网易云账号歌单是否可用（登录并刷新过才有内容）。
///
/// 这一条只做"有没有"的判断：以前这里把整个缓存深拷贝一遍只为 `is_empty()`，
/// 缓存里有几千首歌时是白拷贝（借用判断即可）。
fn has_netease_account_collections() -> bool {
    crate::remote_cache::with_netease(|collections| {
        collections
            .iter()
            .any(|collection| collection.kind == lx_core::sync::SyncCollectionKind::Playlist)
    })
}

/// 「我的歌单」列表：取数与设置页的远程歌单窗口**同源**
/// （`remote_collections::account_collections`），一边显示 23 个、一边 22 个
/// 这类不一致不会再出现。
fn account_playlist_list() -> Vec<Playlist> {
    crate::pages::components::remote_collections::account_collections()
        .iter()
        .map(account_playlist)
        .collect()
}

fn account_playlist(collection: &lx_core::sync::SyncCollection) -> Playlist {
    Playlist {
        id: format!("{NETEASE_ACCOUNT_ID_PREFIX}{}", collection.id),
        name: collection.name.clone(),
        source: SourceId::Wy,
        cover_url: collection
            .songs
            .first()
            .and_then(|song| song.cover_url.clone()),
        song_count: collection.songs.len() as u32,
        description: None,
        play_count: None,
        creator: None,
        link: None,
        extra: Default::default(),
    }
}

#[derive(Debug, Clone)]
enum PlaylistNameInput {
    Create,
    Rename {
        playlist_id: String,
    },
    /// 导出歌单：值是目标 M3U8 文件路径（预填默认位置，可直接编辑）。
    Export {
        playlist_id: String,
    },
}

#[derive(Debug, Clone)]
enum CustomDeleteTarget {
    Playlist {
        playlist_id: String,
        name: String,
    },
    Song {
        playlist_id: String,
        song: Box<SongInfo>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaylistLoadRequest {
    List {
        source: SourceId,
        page: u32,
        append: bool,
    },
    Search {
        source: SourceId,
        keyword: String,
        page: u32,
        append: bool,
    },
    Songs {
        source: SourceId,
        playlist_id: String,
    },
}

#[derive(Clone)]
struct PlaylistListCache {
    items: Vec<Playlist>,
    page: u32,
    has_more: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PPResizeTarget {
    WidePlaylists,
    NarrowPlaylists,
}

const PP_DEFAULT_PLAYLISTS_RATIO_WIDE: f32 = 0.34;
const PP_DEFAULT_PLAYLISTS_RATIO_NARROW: f32 = 0.18;
/// 宽/窄布局分界（与 leaderboard 一致）。
const PP_WIDE_MIN_WIDTH: u16 = 82;
/// 页面在 `ui.pane_ratios` 里的 key。
const PP_PAGE_KEY: &str = "playlists";

pub struct PlaylistsPage {
    scopes: Vec<PlaylistScope>,
    scope_index: usize,
    pub playlists: Vec<Playlist>,
    pub songs: Vec<SongInfo>,
    pub selected: usize,
    pub selected_playlist: Option<usize>,
    list_loaded: bool,
    list_loading: bool,
    /// 上次同步时的 storage generation：收藏/自建歌单列表只在数据
    /// 变化后重建，避免每轮主循环全量克隆。
    last_synced_generation: u64,
    list_page: u32,
    list_has_more: bool,
    search_input: Option<String>,
    search_keyword: Option<String>,
    songs_loaded: bool,
    songs_loading: bool,
    playlist_scroll_offset: usize,
    song_scroll_offset: usize,
    error_message: Option<String>,
    list_cache: HashMap<SourceId, PlaylistListCache>,
    song_cache: HashMap<(SourceId, String), Vec<SongInfo>>,
    name_input: Option<PlaylistNameInput>,
    name_input_value: String,
    pending_delete: Option<CustomDeleteTarget>,
    scope_selector: Option<SourceSelector>,
    /// 上次同步「我的歌单」入口时的远程缓存 generation。
    ///
    /// 远程歌单是异步刷新进缓存的：刷新完成前页面里不该出现空的「我的歌单」，
    /// 刷新完成后又必须立刻出现，因此入口列表要跟着 generation 重建。
    account_scope_generation: u64,
    /// 构造时用的音源列表，重建 scope 入口时复用。
    browse_sources: Vec<SourceId>,
    playlists_ratio_wide: f32,
    playlists_ratio_narrow: f32,
    splitter: Splitter<PPResizeTarget>,
    column_resize: Option<super::components::song_table::ColumnResizeState>,
    song_columns: Vec<lx_core::model::config::TableColumnConfig>,
}

impl PlaylistsPage {
    pub fn new(sources: Vec<SourceId>) -> Self {
        let browse_sources: Vec<SourceId> = sources
            .iter()
            .copied()
            .filter(|source| supports_playlist_browse(*source))
            .collect();
        let (scopes, selector_items) = Self::build_scopes(&browse_sources);
        Self {
            scopes,
            scope_index: 0,
            playlists: Vec::new(),
            songs: Vec::new(),
            selected: 0,
            selected_playlist: None,
            list_loaded: true,
            list_loading: false,
            last_synced_generation: u64::MAX,
            list_page: 0,
            list_has_more: false,
            search_input: None,
            search_keyword: None,
            songs_loaded: false,
            songs_loading: false,
            playlist_scroll_offset: 0,
            song_scroll_offset: 0,
            error_message: None,
            list_cache: HashMap::new(),
            song_cache: HashMap::new(),
            name_input: None,
            name_input_value: String::new(),
            pending_delete: None,
            scope_selector: Some(SourceSelector::new(selector_items, 0)),
            account_scope_generation: crate::remote_cache::generation(),
            browse_sources,
            playlists_ratio_wide: PP_DEFAULT_PLAYLISTS_RATIO_WIDE,
            playlists_ratio_narrow: PP_DEFAULT_PLAYLISTS_RATIO_NARROW,
            splitter: Splitter::default(),
            column_resize: None,
            song_columns: Vec::new(),
        }
    }

    /// scope 入口列表 + 选择器条目：**只在这一处构造**，因此两者永远一一对应
    /// （点击/循环切换用下标定位 scope 时不会错位）。
    fn build_scopes(
        browse_sources: &[SourceId],
    ) -> (Vec<PlaylistScope>, Vec<(SourceSelectorKey, String)>) {
        let scopes: Vec<PlaylistScope> = [PlaylistScope::Custom, PlaylistScope::Favorites]
            .into_iter()
            .chain(browse_sources.iter().copied().map(PlaylistScope::Source))
            // 「我的歌单」只在远程缓存真有内容时出现，避免登录了但没刷新时
            // 多出一个永远空着的入口。
            .chain(
                has_netease_account_collections().then_some(PlaylistScope::Account(SourceId::Wy)),
            )
            .collect();
        let items = scopes
            .iter()
            .map(|scope| {
                let key = match scope {
                    PlaylistScope::Custom => SourceSelectorKey::Custom,
                    PlaylistScope::Favorites => SourceSelectorKey::Favorites,
                    PlaylistScope::Account(source) => SourceSelectorKey::Account(*source),
                    PlaylistScope::Source(source) => SourceSelectorKey::Source(*source),
                };
                (key, scope_label(*scope, true).to_string())
            })
            .collect();
        (scopes, items)
    }

    /// 远程歌单刷新完成后补齐/移除「我的歌单」入口。
    ///
    /// 远程刷新是异步的：只在启动时构造一次 scope，会让「刷新远程歌单」之后
    /// 必须重启才看得到自己的歌单。
    pub fn sync_scopes(&mut self) {
        let generation = crate::remote_cache::generation();
        if generation == self.account_scope_generation {
            return;
        }
        self.account_scope_generation = generation;

        let current = self.current_scope();
        let (scopes, items) = Self::build_scopes(&self.browse_sources);
        if scopes == self.scopes {
            return;
        }
        // 保持用户当前停留的 scope：入口数量变化后下标会漂移
        let index = current
            .and_then(|scope| scopes.iter().position(|item| *item == scope))
            .unwrap_or_else(|| self.scope_index.min(scopes.len().saturating_sub(1)));
        self.scopes = scopes;
        self.scope_index = index;
        self.scope_selector = Some(SourceSelector::new(items, index));
    }

    pub fn current_source(&self) -> Option<SourceId> {
        match self.scopes.get(self.scope_index).copied() {
            Some(PlaylistScope::Source(source)) => Some(source),
            _ => None,
        }
    }

    /// 跳到「我的歌单」入口，并可选地定位到某个远端歌单。
    ///
    /// 供设置页的远程歌单窗口回车使用：窗口只做浏览，"管理"在歌单页里做。
    pub fn focus_account_playlist(&mut self, remote_id: &str, ctx: &AppContext) {
        self.sync_scopes();
        let Some(index) = self
            .scopes
            .iter()
            .position(|scope| matches!(scope, PlaylistScope::Account(_)))
        else {
            return;
        };
        if index != self.scope_index {
            self.select_scope(index, ctx);
        }
        self.selected_playlist = None;
        self.sync_saved_playlists(ctx);
        if let Some(position) = self
            .playlists
            .iter()
            .position(|playlist| playlist.id == format!("{NETEASE_ACCOUNT_ID_PREFIX}{remote_id}"))
        {
            self.selected = position;
        }
    }

    /// 当前是否停在「我的歌单」（账号只读镜像）上。
    fn is_account_scope(&self) -> bool {
        matches!(self.current_scope(), Some(PlaylistScope::Account(_)))
    }

    pub fn search_keyword(&self) -> Option<&str> {
        self.search_keyword.as_deref()
    }

    fn current_scope(&self) -> Option<PlaylistScope> {
        self.scopes.get(self.scope_index).copied()
    }

    fn is_custom_scope(&self) -> bool {
        self.current_scope() == Some(PlaylistScope::Custom)
    }

    fn is_favorites_scope(&self) -> bool {
        self.current_scope() == Some(PlaylistScope::Favorites)
    }

    pub fn current_playlist(&self) -> Option<&Playlist> {
        self.selected_playlist
            .and_then(|index| self.playlists.get(index))
    }

    fn current_playlist_mut(&mut self) -> Option<&mut Playlist> {
        self.selected_playlist
            .and_then(|index| self.playlists.get_mut(index))
    }

    pub fn current_custom_playlist_id(&self) -> Option<String> {
        self.is_custom_scope()
            .then(|| self.current_playlist().map(|playlist| playlist.id.clone()))
            .flatten()
    }

    pub fn input_active(&self) -> bool {
        self.name_input.is_some() || self.pending_delete.is_some()
    }

    pub fn apply_custom_song_addition(&mut self, playlist_id: &str, song: &SongInfo) {
        if !self.is_custom_scope() {
            return;
        }
        if let Some(playlist) = self
            .playlists
            .iter_mut()
            .find(|playlist| playlist.id == playlist_id)
        {
            playlist.song_count = playlist.song_count.saturating_add(1);
            if playlist.cover_url.is_none() {
                playlist.cover_url = song.cover_url.clone();
            }
        }
        if self.current_custom_playlist_id().as_deref() == Some(playlist_id)
            && !self.songs.iter().any(|item| same_song_identity(item, song))
        {
            self.songs.push(song.clone());
            self.songs_loaded = true;
        }
    }

    pub fn apply_custom_song_removal(&mut self, playlist_id: &str, song: &SongInfo) {
        if self.current_custom_playlist_id().as_deref() != Some(playlist_id) {
            return;
        }
        self.songs.retain(|item| !same_song_identity(item, song));
        let song_count = self.songs.len() as u32;
        let cover_url = self.songs.iter().find_map(|item| item.cover_url.clone());
        if let Some(playlist) = self.current_playlist_mut() {
            playlist.song_count = song_count;
            playlist.cover_url = cover_url;
        }
        self.selected = self.selected.min(self.songs.len().saturating_sub(1));
        self.song_scroll_offset = self
            .song_scroll_offset
            .min(self.songs.len().saturating_sub(1));
    }

    pub fn apply_local_file_removal(&mut self, path: &Path, summaries: &[CustomPlaylistSummary]) {
        if !self.is_custom_scope() {
            return;
        }
        for playlist in &mut self.playlists {
            if let Some(summary) = summaries.iter().find(|summary| summary.id == playlist.id) {
                playlist.name = summary.name.clone();
                playlist.cover_url = summary.cover_url.clone();
                playlist.song_count = summary.song_count;
            }
        }
        if self.selected_playlist.is_some() {
            self.songs
                .retain(|song| !local_song_matches_path(song, path));
            self.selected = self.selected.min(self.songs.len().saturating_sub(1));
            self.song_scroll_offset = self
                .song_scroll_offset
                .min(self.songs.len().saturating_sub(1));
        }
    }

    pub fn sync_saved_playlists(&mut self, ctx: &AppContext) {
        if self.selected_playlist.is_some() {
            return;
        }
        let generation = ctx.storage.generation();
        // scope 切换会把 list_loaded 置回 false，强制重建一次
        if self.list_loaded && generation == self.last_synced_generation {
            return;
        }
        self.playlists = match self.current_scope() {
            Some(PlaylistScope::Custom) => ctx
                .storage
                .custom_playlist_summaries()
                .iter()
                .map(custom_playlist_metadata)
                .collect(),
            Some(PlaylistScope::Favorites) => ctx.storage.load_favorite_playlists(),
            // 账号歌单只在刷新过之后才有内容，进入时重新读一次缓存，
            // 刷新完成后的 generation 变化会触发这条路径重建列表。
            Some(PlaylistScope::Account(_)) => account_playlist_list(),
            _ => return,
        };
        self.list_loaded = true;
        self.list_loading = false;
        self.last_synced_generation = generation;
        self.selected = self.selected.min(self.playlists.len().saturating_sub(1));
    }

    /// 「我的歌单」是只读镜像：列表已经全在内存里，不需要发任何网络请求。
    fn account_next_load_request(&self) -> Option<PlaylistLoadRequest> {
        if let Some(playlist) = self.current_playlist() {
            if self.songs_loaded || self.songs_loading {
                return None;
            }
            return Some(PlaylistLoadRequest::Songs {
                source: playlist.source,
                playlist_id: playlist.id.clone(),
            });
        }
        if self.list_loaded || self.list_loading {
            return None;
        }
        Some(PlaylistLoadRequest::List {
            source: SourceId::Wy,
            page: 1,
            append: false,
        })
    }

    pub fn next_load_request(&self) -> Option<PlaylistLoadRequest> {
        if self.is_account_scope() {
            return self.account_next_load_request();
        }
        if let Some(playlist) = self.current_playlist() {
            if self.is_custom_scope() {
                return None;
            }
            if !self.songs_loading && !self.songs_loaded {
                return Some(PlaylistLoadRequest::Songs {
                    source: playlist.source,
                    playlist_id: playlist.id.clone(),
                });
            }
            return None;
        }
        let source = self.current_source()?;
        if let Some(keyword) = self.search_keyword.as_deref()
            && !self.list_loading
            && !self.list_loaded
        {
            return Some(PlaylistLoadRequest::Search {
                source,
                keyword: keyword.to_string(),
                page: 1,
                append: false,
            });
        }
        if !self.list_loading && !self.list_loaded {
            return Some(PlaylistLoadRequest::List {
                source,
                page: 1,
                append: false,
            });
        }
        if !self.list_loading
            && self.list_loaded
            && self.list_has_more
            && !self.playlists.is_empty()
            && self.selected + 1 >= self.playlists.len()
        {
            let page = self.list_page.saturating_add(1).max(1);
            return Some(if let Some(keyword) = &self.search_keyword {
                PlaylistLoadRequest::Search {
                    source,
                    keyword: keyword.clone(),
                    page,
                    append: true,
                }
            } else {
                PlaylistLoadRequest::List {
                    source,
                    page,
                    append: true,
                }
            });
        }
        None
    }

    pub fn begin_loading(&mut self, request: &PlaylistLoadRequest) {
        self.error_message = None;
        match request {
            PlaylistLoadRequest::List { append, .. } => {
                self.list_loading = true;
                if !append {
                    self.list_loaded = false;
                }
            }
            PlaylistLoadRequest::Search { append, .. } => {
                self.list_loading = true;
                if !append {
                    self.list_loaded = false;
                }
            }
            PlaylistLoadRequest::Songs { .. } => {
                self.songs_loading = true;
                self.songs_loaded = false;
            }
        }
    }

    pub fn update_playlists(
        &mut self,
        source: SourceId,
        page: u32,
        append: bool,
        playlists: Vec<Playlist>,
    ) {
        let received_items = !playlists.is_empty();
        let mut items = if append {
            if self.search_keyword.is_some() {
                self.playlists.clone()
            } else {
                self.list_cache
                    .get(&source)
                    .map(|cache| cache.items.clone())
                    .unwrap_or_default()
            }
        } else {
            Vec::new()
        };
        let previous_len = items.len();
        for playlist in playlists {
            if !items
                .iter()
                .any(|item| item.id == playlist.id && item.source == playlist.source)
            {
                items.push(playlist);
            }
        }
        let added_items = items.len() > previous_len;
        let has_more = received_items && (!append || added_items);
        if self.search_keyword.is_none() {
            self.list_cache.insert(
                source,
                PlaylistListCache {
                    items: items.clone(),
                    page,
                    has_more,
                },
            );
        }
        if self.current_source() != Some(source) || self.selected_playlist.is_some() {
            return;
        }
        self.playlists = items;
        self.list_loading = false;
        self.list_loaded = true;
        self.list_page = page;
        self.list_has_more = has_more;
        self.error_message = None;
        self.selected = self.selected.min(self.playlists.len().saturating_sub(1));
        if !append {
            self.playlist_scroll_offset = 0;
        }
    }

    pub fn begin_search_input(&mut self) {
        if self.selected_playlist.is_none() && self.current_source().is_some() {
            self.search_input = Some(self.search_keyword.clone().unwrap_or_default());
        }
    }

    pub fn clear_playlist_search(&mut self) {
        self.search_input = None;
        self.search_keyword = None;
        self.list_loaded = false;
        self.list_page = 0;
        self.list_has_more = false;
    }

    fn submit_search(&mut self) {
        if let Some(value) = self.search_input.take() {
            let value = value.trim().to_string();
            self.search_keyword = (!value.is_empty()).then_some(value);
            self.list_loaded = false;
            self.list_page = 0;
            self.list_has_more = false;
            self.playlists.clear();
            self.selected = 0;
        }
    }

    pub fn update_songs(&mut self, source: SourceId, playlist_id: &str, songs: Vec<SongInfo>) {
        self.song_cache
            .insert((source, playlist_id.to_string()), songs.clone());
        if self
            .current_playlist()
            .map(|playlist| (playlist.source, playlist.id.as_str()))
            != Some((source, playlist_id))
        {
            return;
        }
        self.songs = songs;
        self.songs_loading = false;
        self.songs_loaded = true;
        self.error_message = None;
        self.selected = 0;
        self.song_scroll_offset = 0;
    }

    pub fn update_error(&mut self, request: &PlaylistLoadRequest, message: String) {
        let mut reset_selection = false;
        match request {
            PlaylistLoadRequest::List { source, append, .. }
                if self.current_source() == Some(*source) && self.selected_playlist.is_none() =>
            {
                self.list_loading = false;
                self.list_loaded = true;
                if *append {
                    self.list_has_more = false;
                } else {
                    self.playlists.clear();
                    self.list_page = 0;
                    self.list_has_more = false;
                    reset_selection = true;
                }
                self.error_message = Some(message);
            }
            PlaylistLoadRequest::Search { source, append, .. }
                if self.current_source() == Some(*source) && self.selected_playlist.is_none() =>
            {
                self.list_loading = false;
                // 搜索不受支持时保留/恢复热门列表，避免把页面留在空白错误态。
                self.search_keyword = None;
                self.list_loaded = false;
                self.list_has_more = false;
                self.error_message = Some(format!(
                    "该音源不支持歌单搜索，已切换为热门歌单（{message}）"
                ));
            }
            PlaylistLoadRequest::Songs {
                source,
                playlist_id,
            } if self
                .current_playlist()
                .map(|playlist| (playlist.source, playlist.id.as_str()))
                == Some((*source, playlist_id.as_str())) =>
            {
                self.songs.clear();
                self.songs_loading = false;
                self.songs_loaded = true;
                self.error_message = Some(message);
                reset_selection = true;
            }
            _ => {}
        }
        if reset_selection {
            self.selected = 0;
        }
    }

    pub fn handle_input(
        &mut self,
        key: &KeyEvent,
        ctx: &AppContext,
        resolver: &KeybindingResolver,
    ) -> AppAction {
        if self.splitter.is_dragging() && key.code == KeyCode::Esc {
            // Esc 取消本次分栏拖拽预览，不污染已提交布局
            // （与 main_page / leaderboard 保持一致）。
            self.cancel_resize();
            return AppAction::None;
        }
        if self.name_input.is_some() {
            return self.handle_name_input(key, ctx);
        }
        if self
            .scope_selector
            .as_ref()
            .is_some_and(|selector| selector.is_open())
        {
            self.handle_scope_selector(key, ctx);
            return AppAction::None;
        }
        if self.pending_delete.is_some() {
            return self.handle_delete_confirmation(key, ctx);
        }
        if let Some(input) = self.search_input.as_mut() {
            match (key.modifiers, key.code) {
                (KeyModifiers::NONE, KeyCode::Esc) => self.search_input = None,
                (KeyModifiers::NONE, KeyCode::Enter) => self.submit_search(),
                (KeyModifiers::NONE, KeyCode::Backspace) => {
                    input.pop();
                }
                (modifiers, KeyCode::Char(c))
                    if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    input.push(c)
                }
                _ => {}
            }
            return AppAction::None;
        }
        if let Some(action) = resolver.resolve_page("playlists", key) {
            match action {
                Action::ListSelectUp => {
                    self.move_selection_up(ctx);
                    return AppAction::None;
                }
                Action::ListSelectDown => {
                    self.move_selection_down(ctx);
                    return AppAction::None;
                }
                Action::ListSelectFirst => {
                    self.selected = 0;
                    return AppAction::None;
                }
                Action::ListSelectLast => {
                    self.selected = self.current_list_len().saturating_sub(1);
                    return AppAction::None;
                }
                Action::ListPageUp => {
                    self.selected = self.selected.saturating_sub(10);
                    return AppAction::None;
                }
                Action::ListPageDown => {
                    self.selected =
                        (self.selected + 10).min(self.current_list_len().saturating_sub(1));
                    return AppAction::None;
                }
                Action::ListActivate => {
                    if self.selected_playlist.is_some() && !self.songs.is_empty() {
                        return AppAction::PlaySong {
                            songs: self.songs.clone(),
                            index: self.selected,
                        };
                    }
                    self.enter_selected_playlist(ctx);
                    return AppAction::None;
                }
                Action::ListDownload => {
                    if self.selected_playlist.is_some()
                        && let Some(song) = self.songs.get(self.selected).cloned()
                    {
                        return AppAction::DownloadSong(Box::new(song));
                    }
                    return AppAction::None;
                }
                Action::ListAddToQueue => {
                    if self.selected_playlist.is_some()
                        && let Some(song) = self.songs.get(self.selected).cloned()
                    {
                        return AppAction::AddToQueue {
                            song: Box::new(song),
                            position: InsertPosition::End,
                        };
                    }
                    return AppAction::None;
                }
                Action::ListAddToQueueNext => {
                    if self.selected_playlist.is_some()
                        && let Some(song) = self.songs.get(self.selected).cloned()
                    {
                        return AppAction::AddToQueue {
                            song: Box::new(song),
                            position: InsertPosition::Next,
                        };
                    }
                    return AppAction::None;
                }
                Action::ListToggleFavorite => {
                    return self.toggle_selected_favorite(ctx);
                }
                Action::ListGoBack => {
                    return self.go_back();
                }
                Action::SearchCycleSourcePrev => {
                    self.select_previous_scope(ctx);
                    return AppAction::None;
                }
                Action::SearchCycleSourceNext => {
                    self.select_next_scope(ctx);
                    return AppAction::None;
                }
                _ => {}
            }
        }

        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Char('p' | 'P')) => {
                self.open_scope_selector();
            }
            (KeyModifiers::CONTROL, KeyCode::Left)
            | (KeyModifiers::CONTROL, KeyCode::Char('h'))
            | (KeyModifiers::NONE, KeyCode::Char('[')) => self.select_previous_scope(ctx),
            (KeyModifiers::CONTROL, KeyCode::Right) | (KeyModifiers::NONE, KeyCode::Char(']')) => {
                self.select_next_scope(ctx)
            }
            (KeyModifiers::NONE, KeyCode::Left) if self.selected_playlist.is_none() => {
                self.select_previous_scope(ctx);
            }
            (KeyModifiers::NONE, KeyCode::Right) if self.selected_playlist.is_none() => {
                self.select_next_scope(ctx);
            }
            (KeyModifiers::NONE, KeyCode::Up) => {
                self.move_selection_up(ctx);
            }
            (KeyModifiers::NONE, KeyCode::Down) => {
                self.move_selection_down(ctx);
            }
            (KeyModifiers::NONE, KeyCode::Home) | (KeyModifiers::NONE, KeyCode::Char('g')) => {
                self.selected = 0;
            }
            (KeyModifiers::NONE, KeyCode::End)
            | (KeyModifiers::NONE, KeyCode::Char('G'))
            | (KeyModifiers::SHIFT, KeyCode::Char('G')) => {
                self.selected = self.current_list_len().saturating_sub(1);
            }
            (KeyModifiers::CONTROL, KeyCode::Char('u')) | (KeyModifiers::NONE, KeyCode::PageUp) => {
                self.selected = self.selected.saturating_sub(ctx.page_step());
            }
            (KeyModifiers::CONTROL, KeyCode::Char('d'))
            | (KeyModifiers::NONE, KeyCode::PageDown) => {
                self.selected = (self.selected + ctx.page_step())
                    .min(self.current_list_len().saturating_sub(1));
            }
            _ if super::is_song_activation_key(key) => {
                if self.selected_playlist.is_some() && !self.songs.is_empty() {
                    return AppAction::PlaySong {
                        songs: self.songs.clone(),
                        index: self.selected,
                    };
                }
                self.enter_selected_playlist(ctx);
            }
            (KeyModifiers::NONE, KeyCode::Char('a')) if self.selected_playlist.is_some() => {
                if let Some(song) = self.songs.get(self.selected).cloned() {
                    return AppAction::AddToQueue {
                        song: Box::new(song),
                        position: InsertPosition::End,
                    };
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('A'))
            | (KeyModifiers::SHIFT, KeyCode::Char('A'))
                if self.selected_playlist.is_some() =>
            {
                if let Some(song) = self.songs.get(self.selected).cloned() {
                    return AppAction::AddToQueue {
                        song: Box::new(song),
                        position: InsertPosition::Next,
                    };
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('h')) | (KeyModifiers::NONE, KeyCode::Left)
                if self.selected_playlist.is_some() =>
            {
                self.leave_playlist();
            }
            (KeyModifiers::NONE, KeyCode::Esc) => return self.go_back(),
            (KeyModifiers::NONE, KeyCode::Char('f')) => {
                return self.toggle_selected_favorite(ctx);
            }
            (KeyModifiers::NONE, KeyCode::Char('c'))
                if self.is_custom_scope() && self.selected_playlist.is_none() =>
            {
                self.name_input = Some(PlaylistNameInput::Create);
                self.name_input_value.clear();
            }
            (KeyModifiers::NONE, KeyCode::Char('e')) if self.is_custom_scope() => {
                if let Some((playlist_id, playlist_name)) = self
                    .current_playlist()
                    .or_else(|| self.playlists.get(self.selected))
                    .map(|playlist| (playlist.id.clone(), playlist.name.clone()))
                {
                    self.name_input = Some(PlaylistNameInput::Rename { playlist_id });
                    self.name_input_value = playlist_name;
                }
            }
            // 导出自建歌单为 M3U8（可被其他播放器 / 车机识别）。
            (KeyModifiers::SHIFT, KeyCode::Char('E')) if self.is_custom_scope() => {
                if let Some((playlist_id, playlist_name)) = self
                    .current_playlist()
                    .or_else(|| self.playlists.get(self.selected))
                    .map(|playlist| (playlist.id.clone(), playlist.name.clone()))
                {
                    self.name_input = Some(PlaylistNameInput::Export { playlist_id });
                    self.name_input_value = default_m3u_path(&playlist_name);
                }
            }
            // 推送自建歌单到网易云（写回，追加不删除）。
            (KeyModifiers::SHIFT, KeyCode::Char('P')) if self.is_custom_scope() => {
                if let Some(playlist_id) = self
                    .current_playlist()
                    .or_else(|| self.playlists.get(self.selected))
                    .map(|playlist| playlist.id.clone())
                {
                    return AppAction::PushLocalPlaylist { playlist_id };
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('d') | KeyCode::Delete)
                if self.is_custom_scope() =>
            {
                self.begin_custom_delete();
            }
            (KeyModifiers::NONE, KeyCode::Char('r')) => self.refresh_current(ctx),
            (KeyModifiers::NONE, KeyCode::Char('/')) | (KeyModifiers::NONE, KeyCode::Char('s')) => {
                self.begin_search_input();
            }
            (KeyModifiers::NONE, KeyCode::Char('c'))
                if self.search_keyword.is_some() && self.selected_playlist.is_none() =>
            {
                self.clear_playlist_search();
            }
            _ => {}
        }
        AppAction::None
    }

    fn handle_name_input(&mut self, key: &KeyEvent, ctx: &AppContext) -> AppAction {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc) => {
                self.name_input = None;
                self.name_input_value.clear();
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                let mode = self.name_input.clone().expect("input mode checked above");
                let name = self.name_input_value.trim().to_string();
                let result = match mode {
                    PlaylistNameInput::Create => {
                        ctx.storage.create_custom_playlist(&name).map(|playlist| {
                            self.sync_saved_playlists(ctx);
                            self.selected = self
                                .playlists
                                .iter()
                                .position(|item| item.id == playlist.id)
                                .unwrap_or_default();
                            format!("已创建歌单: {}", playlist.name)
                        })
                    }
                    PlaylistNameInput::Rename { playlist_id } => ctx
                        .storage
                        .rename_custom_playlist(&playlist_id, &name)
                        .map(|()| {
                            for playlist in &mut self.playlists {
                                if playlist.id == playlist_id {
                                    playlist.name = name.clone();
                                }
                            }
                            format!("已重命名为: {name}")
                        }),
                    PlaylistNameInput::Export { playlist_id } => {
                        let path = std::path::PathBuf::from(&name);
                        ctx.storage
                            .export_custom_playlist(&playlist_id, &path)
                            .map(|count| format!("已导出 {count} 首到 {}", path.display()))
                    }
                };
                return match result {
                    Ok(message) => {
                        self.name_input = None;
                        self.name_input_value.clear();
                        AppAction::ShowNotification(Notification::success(message))
                    }
                    Err(error) => AppAction::ShowNotification(Notification::error(error)),
                };
            }
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                self.name_input_value.pop();
            }
            (modifiers, KeyCode::Char(character))
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.name_input_value.push(character);
            }
            _ => {}
        }
        AppAction::None
    }

    fn begin_custom_delete(&mut self) {
        if let Some(playlist) = self.current_playlist() {
            if let Some(song) = self.songs.get(self.selected).cloned() {
                self.pending_delete = Some(CustomDeleteTarget::Song {
                    playlist_id: playlist.id.clone(),
                    song: Box::new(song),
                });
            }
            return;
        }
        if let Some(playlist) = self.playlists.get(self.selected) {
            self.pending_delete = Some(CustomDeleteTarget::Playlist {
                playlist_id: playlist.id.clone(),
                name: playlist.name.clone(),
            });
        }
    }

    fn handle_delete_confirmation(&mut self, key: &KeyEvent, ctx: &AppContext) -> AppAction {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char('n' | 'N'))
            | (KeyModifiers::NONE, KeyCode::Esc) => {
                self.pending_delete = None;
            }
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char('y' | 'Y')) => {
                let target = self
                    .pending_delete
                    .take()
                    .expect("delete target checked above");
                let result = match target {
                    CustomDeleteTarget::Playlist { playlist_id, name } => ctx
                        .storage
                        .delete_custom_playlist(&playlist_id)
                        .map(|deleted| {
                            if deleted {
                                self.sync_saved_playlists(ctx);
                                self.selected =
                                    self.selected.min(self.playlists.len().saturating_sub(1));
                                Notification::success(format!("已删除歌单: {name}"))
                            } else {
                                Notification::info("歌单已不存在")
                            }
                        }),
                    CustomDeleteTarget::Song { playlist_id, song } => ctx
                        .storage
                        .remove_song_from_custom_playlist(&playlist_id, &song)
                        .map(|removed| {
                            if removed {
                                self.apply_custom_song_removal(&playlist_id, &song);
                                Notification::success(format!("已从歌单移除: {}", song.name))
                            } else {
                                Notification::info("歌曲已不在歌单中")
                            }
                        }),
                };
                return match result {
                    Ok(notification) => AppAction::ShowNotification(notification),
                    Err(error) => AppAction::ShowNotification(Notification::error(error)),
                };
            }
            _ => {}
        }
        AppAction::None
    }

    /// 歌曲表头所在的一行（供 main.rs 判定"表头右键 → 列菜单"）。
    pub fn table_header_rect(&self, area: Rect, _ctx: &AppContext) -> Option<Rect> {
        self.selected_playlist?;
        let page = self.compute_layout(area, self.playlists.len());
        let inner = panel_inner(page.songs);
        (inner.height > 0).then(|| Rect::new(inner.x, inner.y, inner.width, 1))
    }

    /// 自动列宽的测量样本，**无副作用**。
    pub fn autofit_samples(&self, _ctx: &AppContext) -> Vec<SongInfo> {
        self.songs.clone()
    }

    /// 兜底取消所有进行中的拖拽会话（分割条 + 列宽）。
    pub fn abort_drag_sessions(&mut self) {
        self.splitter.cancel();
        self.column_resize = None;
    }

    pub fn handle_mouse(
        &mut self,
        event: MouseEvent,
        area: Rect,
        activate: bool,
        ctx: &AppContext,
    ) -> AppAction {
        if self
            .scope_selector
            .as_ref()
            .is_some_and(|selector| selector.is_open())
        {
            let result = self
                .scope_selector
                .as_mut()
                .and_then(|selector| selector.handle_mouse(event, area));
            if let Some(key) = result
                && let Some(index) = self.scopes.iter().position(|scope| match (scope, key) {
                    (PlaylistScope::Custom, SourceSelectorKey::Custom)
                    | (PlaylistScope::Favorites, SourceSelectorKey::Favorites) => true,
                    (PlaylistScope::Account(source), SourceSelectorKey::Account(selected)) => {
                        *source == selected
                    }
                    (PlaylistScope::Source(source), SourceSelectorKey::Source(selected)) => {
                        *source == selected
                    }
                    _ => false,
                })
            {
                if let Some(selector) = self.scope_selector.as_mut() {
                    selector.close()
                }
                self.select_scope(index, ctx);
            }
            return AppAction::None;
        }
        // 只有"按下"才算用户主动放弃输入：以前对所有鼠标事件都执行，
        // 于是新歌单一动鼠标就把刚输入的名字清掉（删除确认也会被鼠标移动取消）。
        if self.input_active() && matches!(event.kind, MouseEventKind::Down(_)) {
            self.name_input = None;
            self.name_input_value.clear();
            self.pending_delete = None;
        }
        let page = self.compute_layout(area, self.playlists.len());
        let content_area = Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(1),
        );
        if let Some(target) = self.splitter.dragging().copied() {
            match event.kind {
                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved => {
                    self.update_resize_preview(target, event, content_area);
                    return AppAction::None;
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    return match self.commit_resize() {
                        Some((ratio_key, ratio)) => AppAction::CommitPaneRatio {
                            page_key: PP_PAGE_KEY.to_string(),
                            ratio_key: ratio_key.to_string(),
                            ratio,
                        },
                        None => AppAction::None,
                    };
                }
                _ => return AppAction::None,
            }
        } else if matches!(event.kind, MouseEventKind::Down(MouseButton::Left))
            && let Some(target) = self.resize_target_at(event, &page)
        {
            let committed = self.committed_ratio(target);
            self.splitter.begin(target, committed);
            return AppAction::None;
        }

        if self.selected_playlist.is_some() {
            let songs_inner = panel_inner(page.songs);
            match super::components::song_table::handle_column_resize(
                &mut self.column_resize,
                &mut self.song_columns,
                event,
                Some(Rect::new(
                    songs_inner.x,
                    songs_inner.y,
                    songs_inner.width,
                    1,
                )),
                songs_inner,
            ) {
                super::components::song_table::ColumnResizeOutcome::Updated => {
                    return AppAction::None;
                }
                super::components::song_table::ColumnResizeOutcome::Finished => {
                    return AppAction::CommitColumnResize {
                        page_key: "playlists".to_string(),
                        columns: self.song_columns.clone(),
                    };
                }
                super::components::song_table::ColumnResizeOutcome::NotHandled => {}
            }
        }

        let position = Position::new(event.column, event.row);
        let scroll_amount = ctx
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .ui
            .scroll_amount
            .max(1);
        match event.kind {
            MouseEventKind::ScrollUp => {
                let scroll_area = if self.selected_playlist.is_some() {
                    page.songs
                } else {
                    page.playlists
                };
                if scroll_area.contains(position) {
                    self.selected = self.selected.saturating_sub(scroll_amount);
                }
            }
            MouseEventKind::ScrollDown => {
                let scroll_area = if self.selected_playlist.is_some() {
                    page.songs
                } else {
                    page.playlists
                };
                if scroll_area.contains(position) {
                    self.selected = (self.selected + scroll_amount)
                        .min(self.current_list_len().saturating_sub(1));
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(key) = self
                    .scope_selector
                    .as_ref()
                    .and_then(|selector| selector.tab_at(page.scopes, position))
                {
                    if let Some(index) = self.scopes.iter().position(|scope| match (scope, key) {
                        (PlaylistScope::Custom, SourceSelectorKey::Custom)
                        | (PlaylistScope::Favorites, SourceSelectorKey::Favorites) => true,
                        (PlaylistScope::Account(source), SourceSelectorKey::Account(selected)) => {
                            *source == selected
                        }
                        (PlaylistScope::Source(source), SourceSelectorKey::Source(selected)) => {
                            *source == selected
                        }
                        _ => false,
                    }) {
                        self.select_scope(index, ctx);
                    }
                    return AppAction::None;
                }

                if let Some(index) = crate::pages::components::hit_test::row_at(
                    page.playlists,
                    position,
                    self.playlist_scroll_offset,
                    self.playlists.len(),
                    0,
                ) {
                    if activate {
                        if self.selected_playlist.is_some() {
                            self.leave_playlist();
                        }
                        self.selected = index;
                        self.enter_selected_playlist(ctx);
                    } else if self.selected_playlist.is_none() {
                        self.selected = index;
                    }
                    return AppAction::None;
                }

                if self.selected_playlist.is_some()
                    && let Some(index) = crate::pages::components::hit_test::row_at(
                        page.songs,
                        position,
                        self.song_scroll_offset,
                        self.songs.len(),
                        1,
                    )
                {
                    self.selected = index;
                    if activate {
                        return AppAction::PlaySong {
                            songs: self.songs.clone(),
                            index,
                        };
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Right) if self.selected_playlist.is_none() => {
                if let Some(index) = crate::pages::components::hit_test::row_at(
                    page.playlists,
                    position,
                    self.playlist_scroll_offset,
                    self.playlists.len(),
                    0,
                ) {
                    self.selected = index;
                    return self.toggle_favorite(ctx);
                }
            }
            _ => {}
        }
        AppAction::None
    }

    pub fn context_song_at(
        &mut self,
        source: MenuHitSource,
        area: Rect,
    ) -> Option<(Vec<SongInfo>, usize)> {
        self.selected_playlist?;
        let page = self.compute_layout(area, self.playlists.len());
        let index = source.resolve_index(
            |event| {
                crate::pages::components::hit_test::row_at(
                    page.songs,
                    Position::new(event.column, event.row),
                    self.song_scroll_offset,
                    self.songs.len(),
                    1,
                )
            },
            self.selected,
            self.songs.len(),
        )?;
        self.selected = index;
        Some((self.songs.clone(), index))
    }

    fn compute_layout(&self, area: Rect, playlist_count: usize) -> PageChunks {
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(area);
        let min_playlists_rows = (playlist_count as u16 + 2).clamp(5, 11);
        let min_playlists_cols: u16 = 14;
        let min_songs_cols: u16 = 20;
        let min_songs_rows: u16 = 5;

        let wide = vertical[1].width >= PP_WIDE_MIN_WIDTH;
        let content = if !wide {
            // 上下分栏：两块面板之间留 1 行 gutter 给横分割线。
            // 比例按整个内容区高度算，与拖拽预览的口径一致。
            let content_area = vertical[1];
            let usable = content_area.height.saturating_sub(GUTTER);
            let ratio = self.splitter.effective(
                &PPResizeTarget::NarrowPlaylists,
                self.playlists_ratio_narrow,
            );
            let desired = ((content_area.height as f32) * ratio).round() as u16;
            let max_playlists = usable.saturating_sub(min_songs_rows);
            // 窗口太矮时先降低下限，别把歌曲面板挤成 0（那样分割线也没了）。
            let min_playlists = min_playlists_rows.min(max_playlists);
            let playlists_height = clamp_extent(desired, min_playlists, max_playlists);
            let (playlists, songs) =
                split_with_gutter(content_area, SplitAxis::Horizontal, playlists_height);
            [playlists, songs]
        } else {
            // 左右分栏：中间留 1 列 gutter，谁都不占用对方的边框列。
            let content_area = vertical[1];
            let usable = content_area.width.saturating_sub(GUTTER);
            let ratio = self
                .splitter
                .effective(&PPResizeTarget::WidePlaylists, self.playlists_ratio_wide);
            let desired = ((content_area.width as f32) * ratio).round() as u16;
            let max_playlists = usable.saturating_sub(min_songs_cols);
            let min_playlists = min_playlists_cols.min(max_playlists);
            let playlists_width = clamp_extent(desired, min_playlists, max_playlists);
            let (playlists, songs) =
                split_with_gutter(content_area, SplitAxis::Vertical, playlists_width);
            [playlists, songs]
        };
        PageChunks {
            scopes: vertical[0],
            playlists: content[0],
            songs: content[1],
            wide,
        }
    }

    /// 当前布局下**唯一**可拖拽的那条分割线。
    ///
    /// 坐标来自 [`divider_line`]（`split_with_gutter` 留下的那 1 格 gutter），
    /// 渲染与命中都读它，所以"看得见的分割线"就是"拖得动的分割线"。
    fn divider(&self, layout: &PageChunks) -> Option<(PPResizeTarget, DividerHit)> {
        if layout.wide {
            if layout.playlists.width == 0 || layout.songs.width == 0 {
                return None;
            }
            let x = divider_line(layout.playlists, SplitAxis::Vertical);
            Some((
                PPResizeTarget::WidePlaylists,
                DividerHit::new(
                    SplitAxis::Vertical,
                    x,
                    (layout.playlists.y, layout.playlists.bottom()),
                ),
            ))
        } else {
            if layout.playlists.height == 0 || layout.songs.height == 0 {
                return None;
            }
            let y = divider_line(layout.playlists, SplitAxis::Horizontal);
            Some((
                PPResizeTarget::NarrowPlaylists,
                DividerHit::new(
                    SplitAxis::Horizontal,
                    y,
                    (layout.playlists.x, layout.playlists.right()),
                ),
            ))
        }
    }

    fn resize_target_at(&self, event: MouseEvent, layout: &PageChunks) -> Option<PPResizeTarget> {
        let (target, hit) = self.divider(layout)?;
        hit.matches(event.column, event.row).then_some(target)
    }

    fn clamp_resize_ratio(target: PPResizeTarget, ratio: f32) -> f32 {
        match target {
            PPResizeTarget::WidePlaylists => clamp_ratio(ratio, 0.15, 0.55),
            PPResizeTarget::NarrowPlaylists => clamp_ratio(ratio, 0.10, 0.40),
        }
    }

    /// 该分割线已提交的比例（开始拖拽时取初值）。
    fn committed_ratio(&self, target: PPResizeTarget) -> f32 {
        match target {
            PPResizeTarget::WidePlaylists => self.playlists_ratio_wide,
            PPResizeTarget::NarrowPlaylists => self.playlists_ratio_narrow,
        }
    }

    fn update_resize_preview(
        &mut self,
        target: PPResizeTarget,
        event: MouseEvent,
        content_area: Rect,
    ) {
        let raw_ratio = match target {
            PPResizeTarget::WidePlaylists if content_area.width > 0 => {
                ratio_within(content_area.x, content_area.width, event.column)
            }
            PPResizeTarget::NarrowPlaylists if content_area.height > 0 => {
                ratio_within(content_area.y, content_area.height, event.row)
            }
            _ => return,
        };
        self.splitter
            .drag(Self::clamp_resize_ratio(target, raw_ratio));
    }

    /// 鼠标抬起：提交比例并返回需要持久化的 `(ratio_key, ratio)`。
    fn commit_resize(&mut self) -> Option<(&'static str, f32)> {
        let (target, ratio) = self.splitter.commit()?;
        let key = match target {
            PPResizeTarget::WidePlaylists => {
                self.playlists_ratio_wide = ratio;
                "playlists_wide"
            }
            PPResizeTarget::NarrowPlaylists => {
                self.playlists_ratio_narrow = ratio;
                "playlists_narrow"
            }
        };
        Some((key, ratio))
    }

    fn cancel_resize(&mut self) {
        self.splitter.cancel();
    }

    /// 从 Config 恢复用户拖拽过的比例（页面构造后调用一次）。
    pub fn apply_pane_ratios(&mut self, ratios: &HashMap<String, f32>) {
        if let Some(value) = ratios.get("playlists_wide").copied() {
            self.playlists_ratio_wide =
                Self::clamp_resize_ratio(PPResizeTarget::WidePlaylists, value);
        }
        if let Some(value) = ratios.get("playlists_narrow").copied() {
            self.playlists_ratio_narrow =
                Self::clamp_resize_ratio(PPResizeTarget::NarrowPlaylists, value);
        }
    }

    pub fn pane_page_key(&self) -> &'static str {
        PP_PAGE_KEY
    }

    /// 把两处分栏比例恢复成内置默认值（与 `apply_pane_ratios` 对称）。
    pub fn reset_pane_ratios(&mut self) {
        self.playlists_ratio_wide = PP_DEFAULT_PLAYLISTS_RATIO_WIDE;
        self.playlists_ratio_narrow = PP_DEFAULT_PLAYLISTS_RATIO_NARROW;
        self.splitter.cancel();
    }

    fn render_resize_dividers(&self, layout: &PageChunks, buf: &mut Buffer, ctx: &AppContext) {
        use ratatui::style::Style;
        let base = Style::new().fg(crate::theme::accent(ctx));
        let style = if self.splitter.is_dragging() {
            base.bold()
        } else {
            base
        };
        // 只画当前方向的那条线（以前两个方向都画，只是恰好压在面板边框上）。
        let Some((_, hit)) = self.divider(layout) else {
            return;
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

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        let page = self.compute_layout(area, self.playlists.len());
        self.render_scopes(page.scopes, buf, ctx);
        self.render_playlists(page.playlists, buf, ctx);
        self.render_songs(page.songs, buf, ctx);
        self.render_resize_dividers(&page, buf, ctx);
        self.render_dialog(area, buf, ctx);
        self.render_scope_selector(area, buf, ctx);
    }

    fn render_scopes(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        if let Some(selector) = self.scope_selector.as_ref() {
            selector.render_tabs(area, buf, ctx);
        }
    }

    fn render_playlists(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        // 正在输入的关键字也要显示出来，否则用户看不到自己打了什么，
        // 输入法候选框也没有可依附的位置。
        let search_prefix = "歌单搜索：";
        let title = if self.is_custom_scope() {
            format!("自建歌单 ({}) [c/e/d]", self.playlists.len())
        } else if self.is_favorites_scope() {
            format!("收藏歌单 ({})", self.playlists.len())
        } else if self.current_source() == Some(SourceId::Bili) {
            format!("我的收藏夹 ({})", self.playlists.len())
        } else {
            let suffix = if self.list_loading {
                " · 加载下一页"
            } else if self.list_has_more {
                " · 向下加载更多"
            } else {
                ""
            };
            if let Some(input) = &self.search_input {
                format!("{search_prefix}{input} ({}){suffix}", self.playlists.len())
            } else if let Some(keyword) = &self.search_keyword {
                format!(
                    "{search_prefix}{keyword} ({}){suffix}",
                    self.playlists.len()
                )
            } else {
                format!(
                    "热门歌单 ({}，第 {} 页){}",
                    self.playlists.len(),
                    self.list_page.max(1),
                    suffix
                )
            }
        };
        let block = Block::default()
            .borders(PANEL_BORDERS)
            .border_style(Style::new().fg(crate::theme::border(ctx)))
            .title(title);
        let inner = block.inner(area);
        block.render(area, buf);
        if let Some(input) = &self.search_input {
            // 插入点 = 标题内 + "歌单搜索：" + 已输入文本（去掉左右边框各一列）。
            crate::ui_cursor::request_after(
                Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), 1),
                search_prefix,
                input,
            );
        }

        if self.selected_playlist.is_none() {
            if let Some(error) = &self.error_message
                && self.playlists.is_empty()
            {
                Paragraph::new(format!("加载失败: {error}"))
                    .style(Style::new().fg(crate::theme::red(ctx)))
                    .render(inner, buf);
                return;
            }
            if self.list_loading && self.playlists.is_empty() {
                let message = if self.current_source() == Some(SourceId::Bili) {
                    "加载哔哩哔哩收藏夹..."
                } else {
                    "加载热门歌单..."
                };
                render_muted(message, inner, buf, ctx);
                return;
            }
            if self.list_loaded && self.playlists.is_empty() {
                let message = if self.is_custom_scope() {
                    "暂无自建歌单，按 c 创建"
                } else if self.is_favorites_scope() {
                    "暂无收藏歌单"
                } else if self.current_source() == Some(SourceId::Bili) {
                    "暂无收藏夹，或尚未登录哔哩哔哩"
                } else {
                    "该音源暂无热门歌单"
                };
                render_muted(message, inner, buf, ctx);
                return;
            }
        }
        if inner.height == 0 || self.playlists.is_empty() {
            return;
        }

        let visible_height = inner.height as usize;
        if self.selected_playlist.is_none() {
            ensure_visible(
                self.selected,
                visible_height,
                self.playlists.len(),
                &mut self.playlist_scroll_offset,
            );
        }
        for index in self.playlist_scroll_offset
            ..(self.playlist_scroll_offset + visible_height).min(self.playlists.len())
        {
            let playlist = &self.playlists[index];
            let favorite = !self.is_custom_scope() && ctx.storage.is_favorite_playlist(playlist);
            let prefix = if favorite { "* " } else { "  " };
            let details = if playlist.song_count > 0 {
                format!(" · {} 首", playlist.song_count)
            } else {
                String::new()
            };
            let text = truncate_chars(
                &format!("{prefix}{}{}", playlist.name, details),
                inner.width as usize,
            );
            let style = if self.selected_playlist.is_none() && index == self.selected {
                Style::new()
                    .bg(crate::theme::accent(ctx))
                    .fg(crate::theme::selection_fg(ctx))
                    .add_modifier(Modifier::BOLD)
            } else if self.selected_playlist == Some(index) {
                Style::new()
                    .fg(crate::theme::accent(ctx))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(crate::theme::text(ctx))
            };
            Paragraph::new(Line::from(Span::styled(text, style))).render(
                Rect::new(
                    inner.x,
                    inner.y + (index - self.playlist_scroll_offset) as u16,
                    inner.width,
                    1,
                ),
                buf,
            );
        }
    }

    fn render_songs(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        let title = self
            .current_playlist()
            .map(|playlist| {
                if self.is_custom_scope() {
                    format!("自建 · {} · d 移除", playlist.name)
                } else {
                    format!("{} · {}", source_name(playlist.source), playlist.name)
                }
            })
            .unwrap_or_else(|| "歌曲列表".to_string());
        let block = Block::default()
            .borders(PANEL_BORDERS)
            .border_style(Style::new().fg(crate::theme::border(ctx)))
            .title(title);
        let inner = block.inner(area);
        block.render(area, buf);

        if self.selected_playlist.is_none() {
            render_muted("选择一个歌单", inner, buf, ctx);
            return;
        }
        if let Some(error) = &self.error_message {
            Paragraph::new(format!("加载失败: {error}"))
                .style(Style::new().fg(crate::theme::red(ctx)))
                .render(inner, buf);
            return;
        }
        if self.songs_loading {
            render_muted("加载歌单歌曲...", inner, buf, ctx);
            return;
        }
        if self.songs_loaded && self.songs.is_empty() {
            render_muted(
                if self.is_custom_scope() {
                    "该自建歌单暂无歌曲，可在任意歌曲右键菜单中加入"
                } else {
                    "该歌单暂无歌曲"
                },
                inner,
                buf,
                ctx,
            );
            return;
        }
        if self.songs.is_empty() || inner.height == 0 {
            return;
        }

        // 拖拽期间以页面状态为准，否则拖拽结果会被每帧重载覆盖。
        if self.column_resize.is_none() {
            let cfg = ctx.config.read().unwrap_or_else(|e| e.into_inner());
            self.song_columns = super::components::song_table::load_columns_for_page(
                &cfg.ui.table_columns,
                "playlists",
                inner.width,
            );
        }

        super::components::song_table::header_paragraph(
            inner.width,
            &self.song_columns,
            super::components::song_table::TablePalette::from_theme(ctx),
        )
        .render(Rect::new(inner.x, inner.y, inner.width, 1), buf);
        let list_area = Rect::new(
            inner.x,
            inner.y.saturating_add(1),
            inner.width,
            inner.height.saturating_sub(1),
        );
        let visible_height = list_area.height as usize;
        if visible_height == 0 {
            return;
        }
        ensure_visible(
            self.selected,
            visible_height,
            self.songs.len(),
            &mut self.song_scroll_offset,
        );
        for index in self.song_scroll_offset
            ..(self.song_scroll_offset + visible_height).min(self.songs.len())
        {
            let row_paragraph = super::components::song_table::row_paragraph(
                &self.songs[index],
                index,
                list_area.width,
                &self.song_columns,
                super::components::song_table::TablePalette::from_theme(ctx),
            );
            let style = if index == self.selected {
                Style::new()
                    .bg(crate::theme::accent(ctx))
                    .fg(crate::theme::selection_fg(ctx))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(crate::theme::text(ctx))
            };
            row_paragraph.style(style).render(
                Rect::new(
                    list_area.x,
                    list_area.y + (index - self.song_scroll_offset) as u16,
                    list_area.width,
                    1,
                ),
                buf,
            );
        }
    }

    fn render_scope_selector(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        if let Some(selector) = self.scope_selector.as_mut() {
            selector.render_popup(area, buf, ctx, "选择范围");
        }
    }

    fn render_dialog(&self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        if let Some(mode) = &self.name_input {
            let dialog = centered_dialog(area, 56, 3);
            if dialog.width == 0 || dialog.height == 0 {
                return;
            }
            Clear.render(dialog, buf);
            let title = match mode {
                PlaylistNameInput::Create => " 创建自建歌单 ",
                PlaylistNameInput::Rename { .. } => " 重命名自建歌单 ",
                PlaylistNameInput::Export { .. } => " 导出歌单 M3U8（输入完整文件路径） ",
            };
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(crate::theme::accent(ctx)))
                .style(
                    Style::new()
                        .bg(crate::theme::surface0(ctx))
                        .fg(crate::theme::text(ctx)),
                )
                .title(title);
            let inner = block.inner(dialog);
            block.render(dialog, buf);
            let shown = name_input_display(&self.name_input_value, inner.width as usize);
            Paragraph::new(shown.clone())
                .style(
                    Style::new()
                        .bg(crate::theme::surface0(ctx))
                        .fg(crate::theme::text(ctx)),
                )
                .render(inner, buf);
            // 宽度不足时显示的是文本尾部，插入点就在显示出来的尾部之后。
            crate::ui_cursor::request_after(inner, "", &shown);
            return;
        }

        let Some(target) = &self.pending_delete else {
            return;
        };
        let message = match target {
            CustomDeleteTarget::Playlist { name, .. } => {
                format!("删除歌单“{name}”及其歌曲列表？ [y/n]")
            }
            CustomDeleteTarget::Song { song, .. } => {
                format!("从当前歌单移除“{}”？ [y/n]", song.name)
            }
        };
        let dialog = centered_dialog(area, 64, 3);
        if dialog.width == 0 || dialog.height == 0 {
            return;
        }
        Clear.render(dialog, buf);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(crate::theme::red(ctx)))
            .style(
                Style::new()
                    .bg(crate::theme::surface0(ctx))
                    .fg(crate::theme::text(ctx)),
            )
            .title(" 确认删除 ");
        let inner = block.inner(dialog);
        block.render(dialog, buf);
        Paragraph::new(truncate_chars(&message, inner.width as usize))
            .style(
                Style::new()
                    .bg(crate::theme::surface0(ctx))
                    .fg(crate::theme::text(ctx)),
            )
            .render(inner, buf);
    }

    fn toggle_favorite(&mut self, ctx: &AppContext) -> AppAction {
        if self.is_custom_scope() {
            return AppAction::ShowNotification(Notification::info("自建歌单无需收藏"));
        }
        let Some(playlist) = self
            .current_playlist()
            .or_else(|| self.playlists.get(self.selected))
            .cloned()
        else {
            return AppAction::None;
        };
        if ctx.storage.is_favorite_playlist(&playlist) {
            ctx.storage.remove_favorite_playlist(&playlist);
            if self.is_favorites_scope() {
                if self.selected_playlist.is_some() {
                    self.leave_playlist();
                }
                self.sync_saved_playlists(ctx);
            }
            AppAction::ShowNotification(Notification::success("已取消收藏歌单"))
        } else {
            ctx.storage.add_favorite_playlist(&playlist);
            AppAction::ShowNotification(Notification::success("已收藏歌单"))
        }
    }

    fn toggle_selected_favorite(&mut self, ctx: &AppContext) -> AppAction {
        if self.selected_playlist.is_some() {
            if let Some(song) = self.songs.get(self.selected).cloned() {
                return AppAction::ToggleFavoriteSong(Box::new(song));
            }
            return AppAction::None;
        }
        self.toggle_favorite(ctx)
    }

    fn enter_selected_playlist(&mut self, ctx: &AppContext) {
        if self.selected_playlist.is_some() || self.selected >= self.playlists.len() {
            return;
        }
        let playlist_index = self.selected;
        let playlist = &self.playlists[playlist_index];
        let cache_key = (playlist.source, playlist.id.clone());
        self.selected_playlist = Some(playlist_index);
        self.selected = 0;
        self.song_scroll_offset = 0;
        self.error_message = None;
        self.songs_loading = false;
        if self.is_custom_scope() {
            self.songs = ctx
                .storage
                .custom_playlist(&playlist.id)
                .map(|custom| custom.songs)
                .unwrap_or_default();
            self.songs_loaded = true;
            return;
        }
        if let Some(songs) = self.song_cache.get(&cache_key) {
            self.songs = songs.clone();
            self.songs_loaded = true;
        } else {
            self.songs.clear();
            self.songs_loaded = false;
        }
    }

    fn leave_playlist(&mut self) {
        let playlist_index = self.selected_playlist.take().unwrap_or_default();
        self.songs.clear();
        self.songs_loaded = false;
        self.songs_loading = false;
        self.error_message = None;
        self.selected = playlist_index.min(self.playlists.len().saturating_sub(1));
        self.song_scroll_offset = 0;
    }

    fn go_back(&mut self) -> AppAction {
        if self.selected_playlist.is_some() {
            self.leave_playlist();
        }
        // Esc is page-local.  Leaving the top-level tab is done with the
        // sidebar or Tab navigation rather than a second Esc press.
        AppAction::None
    }

    fn refresh_current(&mut self, ctx: &AppContext) {
        self.error_message = None;
        if let Some(playlist) = self.current_playlist() {
            if self.is_custom_scope() {
                self.songs = ctx
                    .storage
                    .custom_playlist(&playlist.id)
                    .map(|custom| custom.songs)
                    .unwrap_or_default();
                self.songs_loaded = true;
                self.songs_loading = false;
                self.selected = self.selected.min(self.songs.len().saturating_sub(1));
                self.song_scroll_offset = 0;
                return;
            }
            self.song_cache
                .remove(&(playlist.source, playlist.id.clone()));
            self.songs.clear();
            self.songs_loaded = false;
            self.songs_loading = false;
            self.selected = 0;
            self.song_scroll_offset = 0;
        } else if let Some(source) = self.current_source() {
            self.list_cache.remove(&source);
            self.playlists.clear();
            self.list_loaded = false;
            self.list_loading = false;
            self.list_page = 0;
            self.list_has_more = false;
            self.selected = 0;
            self.playlist_scroll_offset = 0;
        } else {
            self.sync_saved_playlists(ctx);
        }
    }

    fn select_previous_scope(&mut self, ctx: &AppContext) {
        if self.scopes.is_empty() {
            return;
        }
        let index = if self.scope_index == 0 {
            self.scopes.len() - 1
        } else {
            self.scope_index - 1
        };
        self.select_scope(index, ctx);
    }

    fn select_next_scope(&mut self, ctx: &AppContext) {
        if self.scopes.is_empty() {
            return;
        }
        self.select_scope((self.scope_index + 1) % self.scopes.len(), ctx);
    }

    fn select_scope(&mut self, index: usize, ctx: &AppContext) {
        if index >= self.scopes.len() || index == self.scope_index {
            return;
        }
        self.scope_index = index;
        if let Some(selector) = self.scope_selector.as_mut() {
            selector.select(index);
        }
        self.selected_playlist = None;
        self.songs.clear();
        self.songs_loaded = false;
        self.songs_loading = false;
        self.error_message = None;
        self.selected = 0;
        self.playlist_scroll_offset = 0;
        self.song_scroll_offset = 0;
        self.search_input = None;
        self.search_keyword = None;
        if let Some(source) = self.current_source() {
            if let Some(cache) = self.list_cache.get(&source) {
                self.playlists = cache.items.clone();
                self.list_loaded = true;
                self.list_loading = false;
                self.list_page = cache.page;
                self.list_has_more = cache.has_more;
            } else {
                self.playlists.clear();
                self.list_loaded = false;
                self.list_loading = false;
                self.list_page = 0;
                self.list_has_more = false;
            }
        } else {
            self.playlists = match self.current_scope() {
                Some(PlaylistScope::Custom) => ctx
                    .storage
                    .custom_playlist_summaries()
                    .iter()
                    .map(custom_playlist_metadata)
                    .collect(),
                Some(PlaylistScope::Favorites) => ctx.storage.load_favorite_playlists(),
                Some(PlaylistScope::Account(_)) => account_playlist_list(),
                Some(PlaylistScope::Source(_)) | None => Vec::new(),
            };
            self.list_loaded = true;
            self.list_loading = false;
            self.list_page = 0;
            self.list_has_more = false;
        }
    }

    fn move_selection_up(&mut self, ctx: &AppContext) {
        let len = self.current_list_len();
        if len == 0 {
            return;
        }
        if self.selected > 0 {
            self.selected -= 1;
        } else if ctx
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .ui
            .wrap_navigation
        {
            self.selected = len - 1;
        }
    }

    fn move_selection_down(&mut self, ctx: &AppContext) {
        let len = self.current_list_len();
        if len == 0 {
            return;
        }
        if self.selected + 1 < len {
            self.selected += 1;
        } else if self.selected_playlist.is_none() && self.list_has_more {
            // 保持末项选中，主循环会在下一轮请求下一页。
        } else if ctx
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .ui
            .wrap_navigation
        {
            self.selected = 0;
        }
    }

    fn open_scope_selector(&mut self) {
        if let Some(selector) = self.scope_selector.as_mut() {
            selector.select(self.scope_index);
            selector.open();
        }
    }

    fn handle_scope_selector(&mut self, key: &KeyEvent, ctx: &AppContext) {
        let Some(selector) = self.scope_selector.as_mut() else {
            return;
        };
        if selector.handle_key(*key).is_some() {
            let index = selector.selected_index();
            if let Some(selector) = self.scope_selector.as_mut() {
                selector.close()
            }
            self.select_scope(index, ctx);
        } else if !selector.is_open()
            && let Some(selector) = self.scope_selector.as_mut()
        {
            selector.close()
        }
    }

    fn current_list_len(&self) -> usize {
        if self.selected_playlist.is_some() {
            self.songs.len()
        } else {
            self.playlists.len()
        }
    }
}

struct PageChunks {
    scopes: Rect,
    playlists: Rect,
    songs: Rect,
    /// 是否宽布局（左右并排）。分割条靠它决定方向，不再靠几何猜。
    wide: bool,
}

fn centered_dialog(area: Rect, max_width: u16, height: u16) -> Rect {
    let width = area.width.saturating_sub(2).min(max_width);
    let height = area.height.min(height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

/// 输入框宽度不足时，从尾部开始显示能放下的文本。
///
/// 只返回文本本身：光标由终端绘制（见 `ui_cursor`），这样输入法候选框才会跟着
/// 插入点走；以前这里额外拼一个 `█` 当软件光标，会和终端光标重影。
/// 仍然保留最后一列不用，避免插入点落在最右侧列上。
///
/// 注意这里**不能**直接用 [`crate::pages::components::text::truncate_width`]：
/// 它保留的是字符串**开头**并加省略号，而输入框要显示的是**末尾**（插入点附近），
/// 两者语义不同，因此保留这份反向累计的实现。
fn name_input_display(value: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let available = width.saturating_sub(1);
    let mut used = 0;
    let mut visible = Vec::new();
    for character in value.chars().rev() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used + character_width > available {
            break;
        }
        used += character_width;
        visible.push(character);
    }
    visible.reverse();
    visible.into_iter().collect()
}

fn ensure_visible(selected: usize, visible: usize, total: usize, offset: &mut usize) {
    // 共享实现在 components::scroll，leaderboard / playlists 保持一致行为。
    crate::pages::components::scroll::ensure_visible(selected, visible, total, offset)
}

fn render_muted(text: &str, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
    Paragraph::new(text)
        .style(Style::new().fg(crate::theme::muted(ctx)))
        .render(area, buf);
}

fn supports_playlist_browse(source: SourceId) -> bool {
    matches!(
        source,
        SourceId::Kw
            | SourceId::Kg
            | SourceId::Tx
            | SourceId::Wy
            | SourceId::Mg
            | SourceId::Bili
            | SourceId::Qianqian
            | SourceId::Apple
    )
}

fn source_name(source: SourceId) -> &'static str {
    source.display_name()
}

fn scope_label(scope: PlaylistScope, full: bool) -> &'static str {
    match (scope, full) {
        (PlaylistScope::Custom, _) => "自建",
        (PlaylistScope::Favorites, _) => "已收藏",
        // 账号歌单与音源推荐必须分开呈现：登录后两者是两套完全不同的数据
        (PlaylistScope::Account(_), _) => "我的歌单",
        (PlaylistScope::Source(source), true) => source.display_label(),
        (PlaylistScope::Source(source), false) => source.display_name(),
    }
}

fn custom_playlist_metadata(playlist: &CustomPlaylistSummary) -> Playlist {
    Playlist {
        id: playlist.id.clone(),
        name: playlist.name.clone(),
        source: SourceId::Local,
        cover_url: playlist.cover_url.clone(),
        song_count: playlist.song_count,
        description: Some("voicefox-custom-playlist".to_string()),
        play_count: None,
        creator: None,
        link: None,
        extra: HashMap::new(),
    }
}

fn truncate_chars(value: &str, max: usize) -> String {
    // 按显示宽度截断（CJK 占 2 列），共享实现见 components::text
    super::components::text::truncate_width(value, max).into_owned()
}

/// 自建歌单导出路径的预填值：音乐目录（或主目录）下的「歌单名.m3u」。
fn default_m3u_path(playlist_name: &str) -> String {
    let file_name = format!(
        "{}.m3u",
        playlist_name
            .trim()
            .replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_")
    );
    let base = dirs::audio_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join(file_name).to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        CustomDeleteTarget, PlaylistLoadRequest, PlaylistNameInput, PlaylistScope, PlaylistsPage,
    };
    use lx_core::events::AppAction;
    use lx_core::model::playlist::Playlist;
    use lx_core::model::song::SongInfo;
    use lx_core::model::source::SourceId;
    use std::collections::HashMap;

    fn select_source_scope(page: &mut PlaylistsPage, source: SourceId) {
        page.scope_index = page
            .scopes
            .iter()
            .position(|scope| *scope == PlaylistScope::Source(source))
            .unwrap();
    }

    /// 账号歌单（「我的歌单」）与同音源的推荐歌单必须能同时存在：
    /// 它们的 selector key 不同，因此不会互相顶掉。
    #[test]
    fn account_scope_and_source_scope_coexist_for_the_same_source() {
        use crate::pages::components::source_selector::SourceSelectorKey;

        let mut page = PlaylistsPage::new(vec![SourceId::Wy]);
        page.scopes.push(PlaylistScope::Account(SourceId::Wy));
        let account_index = page.scopes.len() - 1;

        let source_key = SourceSelectorKey::Source(SourceId::Wy);
        let account_key = SourceSelectorKey::Account(SourceId::Wy);
        assert_ne!(source_key, account_key);

        page.scope_index = account_index;
        assert!(page.is_account_scope());
        assert_eq!(page.current_source(), None, "账号入口不是音源入口");

        // 「我的歌单」不发网络列表请求，直接读缓存；没有选集时只请求列表
        page.list_loaded = false;
        assert_eq!(
            page.next_load_request(),
            Some(PlaylistLoadRequest::List {
                source: SourceId::Wy,
                page: 1,
                append: false,
            })
        );
    }

    /// 账号歌单的条目用虚拟 id 前缀标记，下游据此还原成远端歌单 id。
    #[test]
    fn account_playlists_carry_the_virtual_id_prefix() {
        let collection = lx_core::sync::SyncCollection {
            kind: lx_core::sync::SyncCollectionKind::Playlist,
            id: "5270339257".to_string(),
            name: "7".to_string(),
            source: SourceId::Wy,
            songs: vec![SongInfo::new(
                "27731362".to_string(),
                SourceId::Wy,
                "背对背拥抱".to_string(),
                "林俊杰".to_string(),
            )],
        };
        let playlist = super::account_playlist(&collection);

        assert_eq!(playlist.id, "netease-account:5270339257");
        assert_eq!(playlist.song_count, 1);
        assert_eq!(
            playlist
                .id
                .strip_prefix(super::NETEASE_ACCOUNT_ID_PREFIX)
                .unwrap(),
            "5270339257"
        );
    }

    fn playlist(id: &str, source: SourceId) -> Playlist {
        Playlist {
            id: id.to_string(),
            name: id.to_string(),
            source,
            cover_url: None,
            song_count: 0,
            description: None,
            play_count: None,
            creator: None,
            link: None,
            extra: HashMap::new(),
        }
    }

    #[test]
    fn source_lists_are_cached_independently() {
        let mut page = PlaylistsPage::new(vec![SourceId::Kw, SourceId::Kg]);
        select_source_scope(&mut page, SourceId::Kw);
        page.list_loaded = false;
        assert_eq!(
            page.next_load_request(),
            Some(PlaylistLoadRequest::List {
                source: SourceId::Kw,
                page: 1,
                append: false,
            })
        );
        page.update_playlists(SourceId::Kw, 1, false, vec![playlist("kw-1", SourceId::Kw)]);
        select_source_scope(&mut page, SourceId::Kg);
        page.list_loaded = false;
        assert_eq!(
            page.next_load_request(),
            Some(PlaylistLoadRequest::List {
                source: SourceId::Kg,
                page: 1,
                append: false,
            })
        );
    }

    #[test]
    fn reaching_the_end_requests_and_appends_the_next_page() {
        let mut page = PlaylistsPage::new(vec![SourceId::Kw]);
        select_source_scope(&mut page, SourceId::Kw);
        page.list_loaded = false;
        page.update_playlists(SourceId::Kw, 1, false, vec![playlist("kw-1", SourceId::Kw)]);

        assert_eq!(
            page.next_load_request(),
            Some(PlaylistLoadRequest::List {
                source: SourceId::Kw,
                page: 2,
                append: true,
            })
        );

        page.update_playlists(
            SourceId::Kw,
            2,
            true,
            vec![
                playlist("kw-1", SourceId::Kw),
                playlist("kw-2", SourceId::Kw),
            ],
        );
        assert_eq!(page.playlists.len(), 2);
        assert_eq!(page.list_page, 2);
    }

    #[test]
    fn duplicate_or_empty_pages_stop_pagination() {
        let mut page = PlaylistsPage::new(vec![SourceId::Kw]);
        select_source_scope(&mut page, SourceId::Kw);
        page.update_playlists(SourceId::Kw, 1, false, vec![playlist("kw-1", SourceId::Kw)]);
        page.update_playlists(SourceId::Kw, 2, true, vec![playlist("kw-1", SourceId::Kw)]);

        assert!(!page.list_has_more);
        assert_eq!(page.next_load_request(), None);
    }

    #[test]
    fn custom_playlist_overlays_capture_global_keys() {
        let mut page = PlaylistsPage::new(Vec::new());
        page.name_input = Some(PlaylistNameInput::Create);
        assert!(page.input_active());

        page.name_input = None;
        page.pending_delete = Some(CustomDeleteTarget::Playlist {
            playlist_id: "custom-1".to_string(),
            name: "通勤".to_string(),
        });
        assert!(page.input_active());
    }

    #[test]
    fn external_custom_song_removal_updates_the_open_playlist() {
        let mut page = PlaylistsPage::new(Vec::new());
        page.playlists = vec![Playlist {
            id: "custom-1".to_string(),
            name: "通勤".to_string(),
            source: SourceId::Local,
            cover_url: Some("first-cover".to_string()),
            song_count: 2,
            description: None,
            play_count: None,
            creator: None,
            link: None,
            extra: HashMap::new(),
        }];
        page.selected_playlist = Some(0);
        let mut first = SongInfo::new(
            "first".to_string(),
            SourceId::Kw,
            "First".to_string(),
            "Artist".to_string(),
        );
        first.cover_url = Some("first-cover".to_string());
        let mut second = SongInfo::new(
            "second".to_string(),
            SourceId::Wy,
            "Second".to_string(),
            "Artist".to_string(),
        );
        second.cover_url = Some("second-cover".to_string());
        page.songs = vec![first.clone(), second.clone()];
        page.selected = 1;

        page.apply_custom_song_removal("custom-1", &first);

        assert_eq!(page.songs.len(), 1);
        assert_eq!(page.songs[0].id, second.id);
        assert_eq!(page.playlists[0].song_count, 1);
        assert_eq!(page.playlists[0].cover_url.as_deref(), Some("second-cover"));
        assert_eq!(page.selected, 0);
    }

    #[test]
    fn external_custom_song_addition_updates_metadata_and_the_open_playlist() {
        let mut page = PlaylistsPage::new(Vec::new());
        page.playlists = vec![Playlist {
            id: "custom-1".to_string(),
            name: "通勤".to_string(),
            source: SourceId::Local,
            cover_url: None,
            song_count: 0,
            description: None,
            play_count: None,
            creator: None,
            link: None,
            extra: HashMap::new(),
        }];
        page.selected_playlist = Some(0);
        let mut song = SongInfo::new(
            "song".to_string(),
            SourceId::Kw,
            "Song".to_string(),
            "Artist".to_string(),
        );
        song.cover_url = Some("cover".to_string());

        page.apply_custom_song_addition("custom-1", &song);

        assert_eq!(page.songs.len(), 1);
        assert_eq!(page.songs[0].id, song.id);
        assert_eq!(page.playlists[0].song_count, 1);
        assert_eq!(page.playlists[0].cover_url.as_deref(), Some("cover"));
    }

    #[test]
    fn playlist_go_back_leaves_the_open_playlist_but_stays_on_the_tab() {
        let mut page = PlaylistsPage::new(Vec::new());
        page.playlists = vec![playlist("custom-1", SourceId::Local)];
        page.selected_playlist = Some(0);

        assert!(matches!(page.go_back(), AppAction::None));
        assert!(page.selected_playlist.is_none());
        assert!(matches!(page.go_back(), AppAction::None));
    }

    #[test]
    fn local_file_removal_updates_custom_playlist_metadata_and_open_songs() {
        let mut page = PlaylistsPage::new(Vec::new());
        page.playlists = vec![
            playlist("custom-1", SourceId::Local),
            playlist("custom-2", SourceId::Local),
        ];
        page.playlists[0].song_count = 2;
        page.playlists[1].song_count = 1;
        page.selected_playlist = Some(0);
        let path = std::path::Path::new("/music/local.flac");
        let mut local = SongInfo::new(
            path.to_string_lossy().into_owned(),
            SourceId::Local,
            "Local".to_string(),
            "Artist".to_string(),
        );
        local.file_path = Some(path.to_path_buf());
        let network = SongInfo::new(
            "network".to_string(),
            SourceId::Wy,
            "Network".to_string(),
            "Artist".to_string(),
        );
        page.songs = vec![local, network.clone()];
        let summaries = vec![
            crate::storage::CustomPlaylistSummary {
                id: "custom-1".to_string(),
                name: "通勤".to_string(),
                cover_url: None,
                song_count: 1,
            },
            crate::storage::CustomPlaylistSummary {
                id: "custom-2".to_string(),
                name: "夜晚".to_string(),
                cover_url: None,
                song_count: 0,
            },
        ];

        page.apply_local_file_removal(path, &summaries);

        assert_eq!(page.songs.len(), 1);
        assert_eq!(page.songs[0].id, network.id);
        assert_eq!(page.playlists[0].name, "通勤");
        assert_eq!(page.playlists[0].song_count, 1);
        assert_eq!(page.playlists[1].song_count, 0);
    }

    #[test]
    fn long_playlist_names_show_their_tail() {
        // 宽度不足时保留尾部，并留出最后一列给终端光标（光标由 ui_cursor 定位）。
        assert_eq!(super::name_input_display("abcdefgh", 7), "cdefgh");
        assert_eq!(super::name_input_display("一二三四五", 7), "三四五");
        assert_eq!(super::name_input_display("name", 0), "");
        assert_eq!(super::name_input_display("abc", 10), "abc");
    }

    /// 回归（窄布局）：横分割线占**自己**那一行 gutter，歌单与歌曲面板的
    /// 最后一行内容既不被覆盖，也仍然能被鼠标点中（滚到底时最后一行点不到）。
    #[test]
    fn narrow_divider_row_is_a_gutter_and_the_last_song_row_stays_clickable() {
        use crate::pages::components::hit_test::{panel_inner, row_at};
        use crate::pages::components::splitter::{GUTTER, SplitAxis, divider_line};
        use ratatui::layout::{Position, Rect};

        let page = PlaylistsPage::new(Vec::new());
        let area = Rect::new(0, 0, 60, 24);
        let chunks = page.compute_layout(area, 0);
        let (_, hit) = page.divider(&chunks).expect("窄布局必须有横分割线");

        // 两块面板 + 1 行 gutter 恰好铺满去掉音源标签行之后的内容区
        assert_eq!(
            hit.divider,
            divider_line(chunks.playlists, SplitAxis::Horizontal)
        );
        assert_eq!(chunks.songs.y, hit.divider + GUTTER);
        assert_eq!(chunks.playlists.bottom(), hit.divider);
        assert_eq!(chunks.playlists.y, chunks.scopes.bottom());

        let list_inner = panel_inner(chunks.playlists);
        let songs_inner = panel_inner(chunks.songs);
        // 分隔线所在的那一行不属于任何面板的内容区
        assert!(
            list_inner.bottom() < hit.divider,
            "歌单内容区必须在 gutter 之上"
        );
        assert!(songs_inner.y > hit.divider, "歌曲内容区必须在 gutter 之下");

        // 滚到底：歌单面板最后一行内容能命中最后一个歌单
        let visible_playlists = list_inner.height as usize;
        assert_eq!(
            row_at(
                chunks.playlists,
                Position::new(list_inner.x + 1, list_inner.bottom() - 1),
                0,
                visible_playlists,
                0,
            ),
            Some(visible_playlists - 1)
        );
        // 歌曲面板最后一行内容能命中最后一首歌（表格占 1 行表头）
        let visible_songs = songs_inner.height as usize - 1;
        assert_eq!(
            row_at(
                chunks.songs,
                Position::new(songs_inner.x + 1, songs_inner.bottom() - 1),
                0,
                visible_songs,
                1,
            ),
            Some(visible_songs - 1)
        );
        // gutter 那一行不是内容区
        assert_eq!(
            row_at(
                chunks.playlists,
                Position::new(list_inner.x + 1, hit.divider),
                0,
                99,
                0
            ),
            None
        );
        assert_eq!(
            row_at(
                chunks.songs,
                Position::new(songs_inner.x + 1, hit.divider),
                0,
                99,
                1
            ),
            None
        );
        // 拖拽命中落在 gutter 上，容差不会吃掉最后一行内容
        assert!(hit.matches(list_inner.x, hit.divider));
        assert!(!hit.matches(list_inner.x, list_inner.bottom() - 1));
        assert!(!hit.matches(songs_inner.x, songs_inner.y));
    }

    /// 回归（宽布局）：竖分割线占歌单面板右边的那 1 列 gutter，
    /// 歌曲面板的最后一列内容照样点得中。
    #[test]
    fn wide_divider_column_is_a_gutter_and_the_last_song_column_stays_clickable() {
        use crate::pages::components::hit_test::{panel_inner, row_at};
        use crate::pages::components::splitter::{GUTTER, SplitAxis, divider_line};
        use ratatui::layout::{Position, Rect};

        let page = PlaylistsPage::new(Vec::new());
        let area = Rect::new(0, 0, 120, 30);
        let chunks = page.compute_layout(area, 0);
        let (_, hit) = page.divider(&chunks).expect("宽布局必须有竖分割线");

        assert_eq!(
            hit.divider,
            divider_line(chunks.playlists, SplitAxis::Vertical)
        );
        assert_eq!(chunks.songs.x, hit.divider + GUTTER);
        assert_eq!(chunks.playlists.right(), hit.divider);
        assert_eq!(chunks.playlists.y, chunks.scopes.bottom());

        let list_inner = panel_inner(chunks.playlists);
        let songs_inner = panel_inner(chunks.songs);
        assert!(
            list_inner.right() <= hit.divider,
            "歌单内容区必须在 gutter 之左"
        );
        assert!(songs_inner.x > hit.divider, "歌曲内容区必须在 gutter 之右");

        // 歌曲面板最后一列内容（第一首歌那一行）能命中
        let visible_songs = songs_inner.height as usize - 1;
        assert_eq!(
            row_at(
                chunks.songs,
                Position::new(songs_inner.right() - 1, songs_inner.y + 1),
                0,
                visible_songs,
                1,
            ),
            Some(0)
        );
        // gutter 那一列不是内容区
        assert_eq!(
            row_at(
                chunks.songs,
                Position::new(hit.divider, songs_inner.y + 1),
                0,
                visible_songs,
                1,
            ),
            None
        );
        assert!(hit.matches(hit.divider, songs_inner.y));
        assert!(!hit.matches(list_inner.right() - 1, songs_inner.y));
        assert!(!hit.matches(songs_inner.x, songs_inner.y));
    }
}
