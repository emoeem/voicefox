//! 热门歌单与歌单收藏页面

use std::collections::HashMap;
use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lx_core::events::{AppAction, InsertPosition, Notification};
use lx_core::keybinding::{Action, KeybindingResolver};
use lx_core::model::playlist::{Playlist, PlaylistCategory};
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};
use unicode_width::UnicodeWidthChar;

use crate::context::AppContext;
use crate::storage::{CustomPlaylistSummary, local_song_matches_path, same_song_identity};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaylistScope {
    Custom,
    Favorites,
    Source(SourceId),
}

#[derive(Debug, Clone)]
enum PlaylistNameInput {
    Create,
    Rename { playlist_id: String },
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
        /// 歌单分类；空字符串表示「全部/热门」。
        category: String,
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
    /// 歌单分类目录（懒加载，只请求一次）。
    Categories { source: SourceId },
    /// 登录账号下的个人歌单。
    User {
        source: SourceId,
        page: u32,
        append: bool,
    },
}

/// 分类选择浮层的状态。
#[derive(Debug, Clone)]
struct CategoryPicker {
    source: SourceId,
    items: Vec<PlaylistCategory>,
    selected: usize,
    scroll: usize,
}

#[derive(Clone)]
struct PlaylistListCache {
    items: Vec<Playlist>,
    page: u32,
    has_more: bool,
}

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
    /// 各音源的歌单分类（懒加载）与当前选中的分类。
    categories: HashMap<SourceId, Vec<PlaylistCategory>>,
    active_category: HashMap<SourceId, String>,
    /// 是否已排入一次分类目录请求（避免失败后每秒重试）。
    category_loading: bool,
    category_picker: Option<CategoryPicker>,
    /// 音源作用域下是否展示「我的歌单」（需要登录）。
    my_playlists: bool,
}

impl PlaylistsPage {
    pub fn new(sources: Vec<SourceId>) -> Self {
        let scopes = [PlaylistScope::Custom, PlaylistScope::Favorites]
            .into_iter()
            .chain(sources.into_iter().map(PlaylistScope::Source))
            .collect();
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
            categories: HashMap::new(),
            active_category: HashMap::new(),
            category_loading: false,
            category_picker: None,
            my_playlists: false,
        }
    }

    pub fn current_source(&self) -> Option<SourceId> {
        match self.scopes.get(self.scope_index).copied() {
            Some(PlaylistScope::Source(source)) => Some(source),
            _ => None,
        }
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
            _ => return,
        };
        self.list_loaded = true;
        self.list_loading = false;
        self.last_synced_generation = generation;
        self.selected = self.selected.min(self.playlists.len().saturating_sub(1));
    }

    pub fn next_load_request(&self) -> Option<PlaylistLoadRequest> {
        // 分类目录只在用户第一次打开选择器时请求一次。
        if self.category_loading
            && let Some(source) = self.current_source()
        {
            return Some(PlaylistLoadRequest::Categories { source });
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
        // 「我的歌单」优先：开关打开时列表来自账号，而不是热门/分类。
        if self.my_playlists && self.search_keyword.is_none() {
            if !self.list_loading && !self.list_loaded {
                return Some(PlaylistLoadRequest::User {
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
                return Some(PlaylistLoadRequest::User {
                    source,
                    page: self.list_page.saturating_add(1).max(1),
                    append: true,
                });
            }
            return None;
        }
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
            let category = self.current_category();
            return Some(PlaylistLoadRequest::List {
                source,
                category,
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
                let category = self.current_category();
                PlaylistLoadRequest::List {
                    source,
                    category,
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
            PlaylistLoadRequest::Categories { .. } => {
                // 请求已经交给后台：标记清掉，避免主循环每个 tick 重复发起；
                // 请求失败时目录仍为空，用户再按一次 `c` 即可重试。
                self.category_loading = false;
            }
            PlaylistLoadRequest::User { append, .. } => {
                self.list_loading = true;
                if !append {
                    self.list_loaded = false;
                }
            }
        }
    }

    /// 当前音源选中的歌单分类；空字符串表示「全部/热门」。
    pub fn current_category(&self) -> String {
        self.current_source()
            .and_then(|source| self.active_category.get(&source).cloned())
            .unwrap_or_default()
    }

    /// 分类选择器要展示的标题（没有分类入口时返回 `None`）。
    pub fn category_label(&self) -> Option<String> {
        let source = self.current_source()?;
        if !self.categories.contains_key(&source) {
            return None;
        }
        let active = self.active_category.get(&source).map(String::as_str);
        Some(match active {
            Some(name) if !name.is_empty() && name != "全部" => format!("分类 {name}"),
            _ => "分类 全部".to_string(),
        })
    }

    /// 分类目录请求返回：成功则打开选择器，失败只记错误信息。
    pub fn apply_categories(
        &mut self,
        source: SourceId,
        result: Result<Vec<PlaylistCategory>, String>,
    ) {
        self.category_loading = false;
        match result {
            Ok(categories) if !categories.is_empty() => {
                self.categories.insert(source, categories.clone());
                let selected = self
                    .active_category
                    .get(&source)
                    .and_then(|active| categories.iter().position(|item| &item.id == active))
                    .unwrap_or(0);
                self.category_picker = Some(CategoryPicker {
                    source,
                    items: categories,
                    selected,
                    scroll: 0,
                });
            }
            Ok(_) => {
                self.error_message = Some("该音源暂时没有可用的歌单分类".to_string());
            }
            Err(error) => {
                self.error_message = Some(error);
            }
        }
    }

    /// 打开分类选择器；分类目录还没加载时先发起请求。
    fn open_category_picker(&mut self) {
        let Some(source) = self.current_source() else {
            return;
        };
        if let Some(items) = self.categories.get(&source).cloned() {
            let selected = self
                .active_category
                .get(&source)
                .and_then(|active| items.iter().position(|item| &item.id == active))
                .unwrap_or(0);
            self.category_picker = Some(CategoryPicker {
                source,
                items,
                selected,
                scroll: 0,
            });
        } else {
            self.category_loading = true;
            self.error_message = None;
        }
    }

    /// 分类选择器按键；返回 `true` 表示按键已被浮层消费。
    fn handle_category_picker(&mut self, key: &KeyEvent, ctx: &AppContext) -> bool {
        if self.category_picker.is_none() {
            return false;
        }
        let len = self
            .category_picker
            .as_ref()
            .map_or(0, |picker| picker.items.len());
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc) | (KeyModifiers::NONE, KeyCode::Char('q')) => {
                self.category_picker = None;
            }
            (KeyModifiers::NONE, KeyCode::Char('j' | 'J'))
            | (KeyModifiers::NONE, KeyCode::Down) => {
                self.set_category_selection_by(|selected| {
                    (selected + 1).min(len.saturating_sub(1))
                });
            }
            (KeyModifiers::NONE, KeyCode::Char('k' | 'K')) | (KeyModifiers::NONE, KeyCode::Up) => {
                self.set_category_selection_by(|selected| selected.saturating_sub(1));
            }
            (KeyModifiers::NONE, KeyCode::Char('g')) | (KeyModifiers::NONE, KeyCode::Home) => {
                self.set_category_selection_by(|_| 0);
            }
            (KeyModifiers::NONE, KeyCode::Char('G'))
            | (KeyModifiers::SHIFT, KeyCode::Char('G'))
            | (KeyModifiers::NONE, KeyCode::End) => {
                self.set_category_selection_by(|_| len.saturating_sub(1));
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                if let Some(name) = self.apply_category_selection() {
                    ctx.notify(Notification::info(format!("已切换到分类「{name}」")));
                }
            }
            _ => {}
        }
        true
    }

    fn set_category_selection_by(&mut self, update: impl FnOnce(usize) -> usize) {
        if let Some(picker) = self.category_picker.as_mut() {
            picker.selected = update(picker.selected);
        }
    }

    /// 应用选择器里选中的分类：记录分类并重置列表状态，下一轮
    /// `next_load_request` 会带上新分类重新加载。返回分类名供提示使用。
    fn apply_category_selection(&mut self) -> Option<String> {
        let picker = self.category_picker.take()?;
        let item = picker.items.get(picker.selected)?;
        self.active_category.insert(picker.source, item.id.clone());
        // 与 `refresh_current` 的音源分支保持一致：清缓存、回到第一页。
        self.list_cache.remove(&picker.source);
        self.playlists.clear();
        self.list_loaded = false;
        self.list_loading = false;
        self.list_page = 0;
        self.list_has_more = false;
        self.selected = 0;
        self.playlist_scroll_offset = 0;
        self.error_message = None;
        Some(item.name.clone())
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
            // 搜索结果与「我的歌单」不落在热门缓存里，分页时直接用当前列表。
            if self.search_keyword.is_some() || self.my_playlists {
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
        if self.search_keyword.is_none() && !self.my_playlists {
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
            PlaylistLoadRequest::User { source, append, .. }
                if self.current_source() == Some(*source) && self.selected_playlist.is_none() =>
            {
                self.list_loading = false;
                // 拉不到个人歌单（多为未登录或登录失效）：回到热门歌单，
                // 并把错误原因提示给用户。
                self.my_playlists = false;
                self.list_loaded = false;
                self.list_has_more = false;
                self.error_message = Some(format!("获取我的歌单失败：{message}"));
                reset_selection = true;
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
        if self.category_picker.is_some() {
            self.handle_category_picker(key, ctx);
            return AppAction::None;
        }
        if self.name_input.is_some() {
            return self.handle_name_input(key, ctx);
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
                self.selected = self.selected.saturating_sub(10);
            }
            (KeyModifiers::CONTROL, KeyCode::Char('d'))
            | (KeyModifiers::NONE, KeyCode::PageDown) => {
                self.selected = (self.selected + 10).min(self.current_list_len().saturating_sub(1));
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
            // 音源页的 `c`：打开歌单分类选择器（自建歌单页的 `c` 是新建）。
            (KeyModifiers::NONE, KeyCode::Char('c'))
                if self.current_source().is_some()
                    && self.selected_playlist.is_none()
                    && self.search_keyword.is_none() =>
            {
                self.open_category_picker();
            }
            // 音源页的 `m`：在热门歌单与「我的歌单」之间切换。
            (KeyModifiers::NONE, KeyCode::Char('m'))
                if self.current_source().is_some()
                    && self.selected_playlist.is_none()
                    && self.search_keyword.is_none() =>
            {
                let source = self.current_source().unwrap_or(SourceId::Local);
                if !self.my_playlists && !ctx.source_manager.is_logged_in(source) {
                    ctx.notify(Notification::info(format!(
                        "请先在设置页扫码登录{}",
                        source.display_name()
                    )));
                } else {
                    self.my_playlists = !self.my_playlists;
                    self.list_cache.remove(&source);
                    self.playlists.clear();
                    self.list_loaded = false;
                    self.list_loading = false;
                    self.list_page = 0;
                    self.list_has_more = false;
                    self.selected = 0;
                    self.playlist_scroll_offset = 0;
                    self.error_message = None;
                }
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

    pub fn handle_mouse(
        &mut self,
        event: MouseEvent,
        area: Rect,
        activate: bool,
        ctx: &AppContext,
    ) -> AppAction {
        if self.input_active() {
            return AppAction::None;
        }
        let page = page_chunks(area, self.playlists.len());
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
                for (index, tab) in scope_tab_rects(page.scopes, self.scopes.len())
                    .iter()
                    .enumerate()
                {
                    if tab.contains(position) {
                        self.select_scope(index, ctx);
                        return AppAction::None;
                    }
                }

                let playlist_inner = Block::default().borders(Borders::ALL).inner(page.playlists);
                if playlist_inner.contains(position) {
                    let index = self.playlist_scroll_offset
                        + event.row.saturating_sub(playlist_inner.y) as usize;
                    if index < self.playlists.len() {
                        if activate {
                            if self.selected_playlist.is_some() {
                                self.leave_playlist();
                            }
                            self.selected = index;
                            self.enter_selected_playlist(ctx);
                        } else if self.selected_playlist.is_none() {
                            self.selected = index;
                        }
                    }
                    return AppAction::None;
                }

                if self.selected_playlist.is_some() {
                    let song_inner = Block::default().borders(Borders::ALL).inner(page.songs);
                    let list_y = song_inner.y.saturating_add(1);
                    if event.row >= list_y && event.row < song_inner.bottom() {
                        let index =
                            self.song_scroll_offset + event.row.saturating_sub(list_y) as usize;
                        if index < self.songs.len() {
                            self.selected = index;
                            if activate {
                                return AppAction::PlaySong {
                                    songs: self.songs.clone(),
                                    index,
                                };
                            }
                        }
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Right) if self.selected_playlist.is_none() => {
                let playlist_inner = Block::default().borders(Borders::ALL).inner(page.playlists);
                if playlist_inner.contains(position) {
                    let index = self.playlist_scroll_offset
                        + event.row.saturating_sub(playlist_inner.y) as usize;
                    if index < self.playlists.len() {
                        self.selected = index;
                        return self.toggle_favorite(ctx);
                    }
                }
            }
            _ => {}
        }
        AppAction::None
    }

    pub fn context_song_at(
        &mut self,
        event: MouseEvent,
        area: Rect,
    ) -> Option<(Vec<SongInfo>, usize)> {
        self.selected_playlist?;
        let page = page_chunks(area, self.playlists.len());
        let song_inner = Block::default().borders(Borders::ALL).inner(page.songs);
        let position = Position::new(event.column, event.row);
        let list_y = song_inner.y.saturating_add(1);
        if !song_inner.contains(position) || event.row < list_y || event.row >= song_inner.bottom()
        {
            return None;
        }
        let index = self.song_scroll_offset + event.row.saturating_sub(list_y) as usize;
        if index >= self.songs.len() {
            return None;
        }
        self.selected = index;
        Some((self.songs.clone(), index))
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        let page = page_chunks(area, self.playlists.len());
        self.render_scopes(page.scopes, buf, ctx);
        self.render_playlists(page.playlists, buf, ctx);
        self.render_songs(page.songs, buf, ctx);
        self.render_dialog(area, buf, ctx);
        self.render_category_picker(area, buf, ctx);
    }

    /// 歌单分类选择浮层：`j/k` 移动、`Enter` 应用、`Esc` 关闭。
    fn render_category_picker(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        let Some((source, selected)) = self
            .category_picker
            .as_ref()
            .map(|picker| (picker.source, picker.selected))
        else {
            return;
        };
        let items = self.categories.get(&source).cloned().unwrap_or_default();
        if items.is_empty() {
            return;
        }
        let width = area.width.saturating_sub(4).clamp(24, 48);
        let rows = items
            .len()
            .min(area.height.saturating_sub(6) as usize)
            .max(3);
        let height = rows as u16 + 4;
        let popup = Rect::new(
            area.x + area.width.saturating_sub(width) / 2,
            area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        );
        Clear.render(popup, buf);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(crate::theme::accent(ctx)))
            .title(" 歌单分类 · Enter 应用 · Esc 取消 ")
            .style(Style::new().bg(crate::theme::mantle(ctx)));
        let inner = block.inner(popup);
        block.render(popup, buf);

        // 让选中项始终可见。
        let visible = inner.height as usize;
        let mut scroll = self.category_picker.as_ref().map_or(0, |p| p.scroll);
        if selected < scroll {
            scroll = selected;
        } else if visible > 0 && selected >= scroll + visible {
            scroll = selected + 1 - visible;
        }
        let scroll = scroll.min(items.len().saturating_sub(visible.max(1)));
        if let Some(picker) = self.category_picker.as_mut() {
            picker.scroll = scroll;
        }

        let active = self
            .active_category
            .get(&source)
            .map(String::as_str)
            .unwrap_or_default();
        let lines = items
            .iter()
            .enumerate()
            .skip(scroll)
            .take(visible)
            .map(|(index, category)| {
                let marker = if index == selected { "▶ " } else { "  " };
                let checked = if category.id == active {
                    "● "
                } else {
                    "○ "
                };
                let mut line = Line::from(vec![
                    Span::raw(marker),
                    Span::styled(checked, Style::new().fg(crate::theme::accent(ctx))),
                    Span::raw(category.name.clone()),
                ]);
                if let Some(group) = category.group.as_deref()
                    && !group.is_empty()
                    && group != category.name
                {
                    line.push_span(Span::styled(
                        format!("  {group}"),
                        Style::new().fg(crate::theme::muted(ctx)),
                    ));
                }
                line
            })
            .collect::<Vec<_>>();
        Paragraph::new(lines).render(inner, buf);
    }

    fn render_scopes(&self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        for (index, tab) in scope_tab_rects(area, self.scopes.len()).iter().enumerate() {
            let label = scope_label(self.scopes[index], area.width >= 66);
            let style = if index == self.scope_index {
                Style::new()
                    .bg(crate::theme::accent(ctx))
                    .fg(crate::theme::selection_fg(ctx))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(crate::theme::muted(ctx))
            };
            Paragraph::new(label)
                .alignment(Alignment::Center)
                .style(style)
                .render(*tab, buf);
        }
    }

    fn render_playlists(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
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
            if let Some(keyword) = &self.search_keyword {
                format!("歌单搜索：{} ({}){}", keyword, self.playlists.len(), suffix)
            } else if self.my_playlists {
                format!("我的歌单 ({}){} [m 切回热门]", self.playlists.len(), suffix)
            } else {
                // 支持分类的音源在标题里显示当前分类与入口提示。
                let category_hint = self
                    .current_source()
                    .filter(|source| ctx.source_manager.capabilities(*source).playlist_categories)
                    .map(|_| {
                        let label = self
                            .category_label()
                            .unwrap_or_else(|| "分类 全部".to_string());
                        format!(" · {label} [c]")
                    })
                    .unwrap_or_default();
                format!(
                    "热门歌单 ({}，第 {} 页){}{category_hint}",
                    self.playlists.len(),
                    self.list_page.max(1),
                    suffix
                )
            }
        };
        let block = super::components::chrome::card(ctx, title);
        let inner = block.inner(area);
        block.render(area, buf);

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
        let block = super::components::chrome::card(ctx, title);
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

        Paragraph::new(Line::from(Span::styled(
            super::components::song_table::header(inner.width),
            Style::new()
                .fg(crate::theme::muted(ctx))
                .add_modifier(Modifier::BOLD),
        )))
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
            let text =
                super::components::song_table::row(&self.songs[index], index, list_area.width);
            let style = if index == self.selected {
                Style::new()
                    .bg(crate::theme::accent(ctx))
                    .fg(crate::theme::selection_fg(ctx))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(crate::theme::text(ctx))
            };
            Paragraph::new(Line::from(Span::styled(text, style))).render(
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
            Paragraph::new(name_input_with_cursor(
                &self.name_input_value,
                inner.width as usize,
            ))
            .style(
                Style::new()
                    .bg(crate::theme::surface0(ctx))
                    .fg(crate::theme::text(ctx)),
            )
            .render(inner, buf);
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
}

fn page_chunks(area: Rect, playlist_count: usize) -> PageChunks {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);
    let content = if area.width < 82 {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length((playlist_count as u16 + 2).clamp(5, 11)),
                Constraint::Min(0),
            ])
            .split(vertical[1])
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(34), Constraint::Min(0)])
            .split(vertical[1])
    };
    PageChunks {
        scopes: vertical[0],
        playlists: content[0],
        songs: content[1],
    }
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

fn name_input_with_cursor(value: &str, width: usize) -> String {
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
    visible.into_iter().chain(std::iter::once('█')).collect()
}

fn scope_tab_rects(area: Rect, count: usize) -> std::rc::Rc<[Rect]> {
    if count == 0 {
        return std::rc::Rc::from([]);
    }
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![Constraint::Ratio(1, count as u32); count])
        .split(area)
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

fn source_name(source: SourceId) -> &'static str {
    source.display_name()
}

fn scope_label(scope: PlaylistScope, full: bool) -> &'static str {
    match (scope, full) {
        (PlaylistScope::Custom, _) => "自建",
        (PlaylistScope::Favorites, _) => "已收藏",
        (PlaylistScope::Source(source), true) => source.display_label(),
        (PlaylistScope::Source(source), false) => source.as_str(),
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
        extra: Default::default(),
    }
}

fn truncate_chars(value: &str, max: usize) -> String {
    // 按显示宽度截断（CJK 占 2 列），共享实现见 components::text
    super::components::text::truncate_width(value, max).into_owned()
}

#[cfg(test)]
mod tests {
    use super::{
        CustomDeleteTarget, PlaylistLoadRequest, PlaylistNameInput, PlaylistScope, PlaylistsPage,
    };
    use lx_core::events::AppAction;
    use lx_core::model::playlist::Playlist;
    use lx_core::model::playlist::PlaylistCategory;
    use lx_core::model::song::SongInfo;
    use lx_core::model::source::SourceId;

    fn select_source_scope(page: &mut PlaylistsPage, source: SourceId) {
        page.scope_index = page
            .scopes
            .iter()
            .position(|scope| *scope == PlaylistScope::Source(source))
            .unwrap();
    }

    fn category(id: &str, source: SourceId) -> PlaylistCategory {
        PlaylistCategory::new(id, id, source)
    }

    #[test]
    fn categories_are_requested_once_and_open_the_picker() {
        let mut page = PlaylistsPage::new(vec![SourceId::Wy]);
        select_source_scope(&mut page, SourceId::Wy);
        page.list_loaded = true;

        // 分类目录在用户按下分类键之前不会请求。
        assert_eq!(page.next_load_request(), None);
        page.open_category_picker();
        assert_eq!(
            page.next_load_request(),
            Some(PlaylistLoadRequest::Categories {
                source: SourceId::Wy
            })
        );
        // 请求交给后台后标记清掉，主循环不会每个 tick 重复发起。
        page.begin_loading(&PlaylistLoadRequest::Categories {
            source: SourceId::Wy,
        });
        assert_eq!(page.next_load_request(), None);
        assert!(page.category_picker.is_none());
    }

    #[test]
    fn picking_a_category_reloads_the_list_with_it() {
        let mut page = PlaylistsPage::new(vec![SourceId::Wy]);
        select_source_scope(&mut page, SourceId::Wy);
        page.list_loaded = true;
        page.apply_categories(
            SourceId::Wy,
            Ok(vec![
                category("全部", SourceId::Wy),
                category("华语", SourceId::Wy),
            ]),
        );
        assert!(page.category_picker.is_some());

        // 选中「华语」后列表按该分类重新加载。
        if let Some(picker) = page.category_picker.as_mut() {
            picker.selected = 1;
        }
        assert_eq!(
            page.apply_category_selection().as_deref(),
            Some("华语"),
            "选中的分类应当被应用"
        );
        assert!(page.category_picker.is_none());
        assert_eq!(page.current_category(), "华语");
        assert_eq!(page.category_label().as_deref(), Some("分类 华语"));
        assert_eq!(
            page.next_load_request(),
            Some(PlaylistLoadRequest::List {
                source: SourceId::Wy,
                category: "华语".to_string(),
                page: 1,
                append: false,
            })
        );
    }

    #[test]
    fn category_label_is_absent_until_categories_load() {
        let mut page = PlaylistsPage::new(vec![SourceId::Kw]);
        select_source_scope(&mut page, SourceId::Kw);
        assert_eq!(page.category_label(), None);
        assert_eq!(page.current_category(), "");
    }

    #[test]
    fn my_playlists_mode_requests_the_account_list() {
        let mut page = PlaylistsPage::new(vec![SourceId::Wy]);
        select_source_scope(&mut page, SourceId::Wy);
        page.list_loaded = true;
        assert_eq!(page.next_load_request(), None);

        // 打开「我的歌单」后，列表请求走账号接口而不是热门/分类。
        page.my_playlists = true;
        page.list_loaded = false;
        assert_eq!(
            page.next_load_request(),
            Some(PlaylistLoadRequest::User {
                source: SourceId::Wy,
                page: 1,
                append: false,
            })
        );
    }

    #[test]
    fn my_playlists_failure_falls_back_to_hot_playlists() {
        let mut page = PlaylistsPage::new(vec![SourceId::Wy]);
        select_source_scope(&mut page, SourceId::Wy);
        page.my_playlists = true;
        page.list_loaded = false;

        let request = PlaylistLoadRequest::User {
            source: SourceId::Wy,
            page: 1,
            append: false,
        };
        page.update_error(&request, "请先在设置页登录网易云".to_string());

        assert!(!page.my_playlists, "失败后应回到热门歌单");
        assert!(!page.list_loaded);
        assert!(
            page.error_message
                .as_deref()
                .is_some_and(|message| message.contains("我的歌单")),
            "{:?}",
            page.error_message
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
            extra: Default::default(),
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
                category: String::new(),
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
                category: String::new(),
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
                category: String::new(),
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
            extra: Default::default(),
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
            extra: Default::default(),
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
    fn long_playlist_names_keep_the_cursor_visible() {
        assert_eq!(super::name_input_with_cursor("abcdefgh", 7), "cdefgh█");
        assert_eq!(super::name_input_with_cursor("一二三四五", 7), "三四五█");
        assert_eq!(super::name_input_with_cursor("name", 0), "");
    }
}
