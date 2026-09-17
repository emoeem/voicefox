//! 收藏页面

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lx_core::events::{AppAction, InsertPosition};
use lx_core::keybinding::{Action, KeybindingResolver};
use lx_core::model::song::SongInfo;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::context::AppContext;
use crate::pages::sort::{SortMode, SortTarget, SortedListCache};

pub struct FavoritesPage {
    selected: usize,
    scroll: usize,
    filter: super::components::list_filter::ListFilter,
    viewport_height: usize,
    sort_mode: SortMode,
}

impl FavoritesPage {
    pub fn new() -> Self {
        Self {
            selected: 0,
            scroll: 0,
            filter: super::components::list_filter::ListFilter::new(),
            viewport_height: 1,
            sort_mode: SortMode::Newest,
        }
    }

    pub fn input_mode(&self) -> bool {
        self.filter.is_active()
    }

    pub fn sort_mode(&self) -> SortMode {
        self.sort_mode
    }

    pub fn sort_label(&self) -> &'static str {
        self.sort_mode.label(SortTarget::Favorites)
    }

    pub fn cycle_sort(&mut self) -> SortMode {
        self.sort_mode = self.sort_mode.next();
        self.selected = 0;
        self.scroll = 0;
        self.sort_mode
    }

    pub fn handle_input(
        &mut self,
        key: &KeyEvent,
        ctx: &AppContext,
        resolver: &KeybindingResolver,
        cache: &mut SortedListCache,
    ) -> AppAction {
        let query_before = self.filter.query().to_string();
        if self.filter.handle_input(key) {
            if self.filter.query() != query_before {
                self.selected = 0;
                self.scroll = 0;
            }
            return AppAction::None;
        }

        let favorites = self.sorted_favorites(ctx, cache);
        let filtered = self.filtered_song_indices(favorites);
        self.clamp_selection(filtered.len());
        let half_page = (self.viewport_height / 2).max(1);

        if let Some(action) = resolver.resolve_page("favorites", key) {
            match action {
                Action::FavoritesFilter => {
                    self.filter.activate();
                    return AppAction::None;
                }
                Action::ListCycleSort => {
                    let mode = self.cycle_sort();
                    return AppAction::ShowNotification(lx_core::events::Notification::info(
                        format!("收藏排序: {}", mode.label(SortTarget::Favorites)),
                    ));
                }
                Action::ListSelectUp => {
                    if !filtered.is_empty() {
                        if self.selected > 0 {
                            self.selected -= 1;
                        } else if ctx
                            .config
                            .read()
                            .unwrap_or_else(|e| e.into_inner())
                            .ui
                            .wrap_navigation
                        {
                            self.selected = filtered.len().saturating_sub(1);
                        }
                    }
                    return AppAction::None;
                }
                Action::ListSelectDown => {
                    if !filtered.is_empty() {
                        if self.selected + 1 < filtered.len() {
                            self.selected += 1;
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
                    return AppAction::None;
                }
                Action::ListSelectFirst => {
                    self.selected = 0;
                    return AppAction::None;
                }
                Action::ListSelectLast => {
                    self.selected = filtered.len().saturating_sub(1);
                    return AppAction::None;
                }
                Action::ListPageUp => {
                    self.selected = self.selected.saturating_sub(half_page);
                    return AppAction::None;
                }
                Action::ListPageDown => {
                    self.selected =
                        (self.selected + half_page).min(filtered.len().saturating_sub(1));
                    return AppAction::None;
                }
                Action::ListActivate => {
                    if self.selected < filtered.len() {
                        let songs = filtered
                            .iter()
                            .filter_map(|index| favorites.get(*index).cloned())
                            .collect::<Vec<_>>();
                        self.log_keyboard_playback(&songs);
                        return AppAction::PlaySong {
                            songs,
                            index: self.selected,
                        };
                    }
                    return AppAction::None;
                }
                Action::ListDownload => {
                    if let Some(song) = filtered
                        .get(self.selected)
                        .and_then(|index| favorites.get(*index))
                        .cloned()
                    {
                        return AppAction::DownloadSong(Box::new(song));
                    }
                    return AppAction::None;
                }
                Action::ListAddToQueue => {
                    if let Some(song) = filtered
                        .get(self.selected)
                        .and_then(|index| favorites.get(*index))
                        .cloned()
                    {
                        return AppAction::AddToQueue {
                            song: Box::new(song),
                            position: InsertPosition::End,
                        };
                    }
                    return AppAction::None;
                }
                Action::ListAddToQueueNext => {
                    if let Some(song) = filtered
                        .get(self.selected)
                        .and_then(|index| favorites.get(*index))
                        .cloned()
                    {
                        return AppAction::AddToQueue {
                            song: Box::new(song),
                            position: InsertPosition::Next,
                        };
                    }
                    return AppAction::None;
                }
                Action::FavoritesRemove => {
                    if let Some(original_index) = filtered.get(self.selected).copied()
                        && let Some(song) = favorites.get(original_index)
                        && ctx.storage.remove_favorite(song)
                    {
                        let remaining = ctx.storage.load_favorites();
                        let remaining_len = self.filtered_song_indices(&remaining).len();
                        self.clamp_selection(remaining_len);
                        return AppAction::ShowNotification(lx_core::events::Notification::info(
                            "已取消收藏",
                        ));
                    }
                    return AppAction::None;
                }
                Action::ListToggleFavorite => {
                    if let Some(original_index) = filtered.get(self.selected).copied()
                        && let Some(song) = favorites.get(original_index).cloned()
                    {
                        return AppAction::ToggleFavoriteSong(Box::new(song));
                    }
                    return AppAction::None;
                }
                Action::ListGoBack => {
                    if !self.filter.query().is_empty() {
                        self.filter.reset();
                        self.selected = 0;
                        self.scroll = 0;
                    }
                    return AppAction::None;
                }
                _ => {}
            }
        }

        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Char('/')) => {
                self.filter.activate();
            }
            (KeyModifiers::NONE, KeyCode::Char('s')) => {
                let mode = self.cycle_sort();
                return AppAction::ShowNotification(lx_core::events::Notification::info(format!(
                    "收藏排序: {}",
                    mode.label(SortTarget::Favorites)
                )));
            }
            (KeyModifiers::NONE, KeyCode::Esc) => {
                if !self.filter.query().is_empty() {
                    self.filter.reset();
                    self.selected = 0;
                    self.scroll = 0;
                }
            }
            (KeyModifiers::NONE, KeyCode::Up) => {
                if !filtered.is_empty() {
                    if self.selected > 0 {
                        self.selected -= 1;
                    } else if ctx
                        .config
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .ui
                        .wrap_navigation
                    {
                        self.selected = filtered.len().saturating_sub(1);
                    }
                }
            }
            (KeyModifiers::NONE, KeyCode::Down) => {
                if !filtered.is_empty() {
                    if self.selected + 1 < filtered.len() {
                        self.selected += 1;
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
            }
            (KeyModifiers::NONE, KeyCode::Home) | (KeyModifiers::NONE, KeyCode::Char('g')) => {
                self.selected = 0;
            }
            (KeyModifiers::NONE, KeyCode::End)
            | (KeyModifiers::NONE, KeyCode::Char('G'))
            | (KeyModifiers::SHIFT, KeyCode::Char('G')) => {
                self.selected = filtered.len().saturating_sub(1);
            }
            (KeyModifiers::CONTROL, KeyCode::Char('u')) | (KeyModifiers::NONE, KeyCode::PageUp) => {
                self.selected = self.selected.saturating_sub(half_page);
            }
            (KeyModifiers::CONTROL, KeyCode::Char('d'))
            | (KeyModifiers::NONE, KeyCode::PageDown) => {
                self.selected = (self.selected + half_page).min(filtered.len().saturating_sub(1));
            }
            _ if super::is_song_activation_key(key) => {
                if self.selected < filtered.len() {
                    let songs = filtered
                        .iter()
                        .filter_map(|index| favorites.get(*index).cloned())
                        .collect::<Vec<_>>();
                    self.log_keyboard_playback(&songs);
                    return AppAction::PlaySong {
                        songs,
                        index: self.selected,
                    };
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('a')) => {
                if let Some(song) = filtered
                    .get(self.selected)
                    .and_then(|index| favorites.get(*index))
                    .cloned()
                {
                    return AppAction::AddToQueue {
                        song: Box::new(song),
                        position: InsertPosition::End,
                    };
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('A'))
            | (KeyModifiers::SHIFT, KeyCode::Char('A')) => {
                if let Some(song) = filtered
                    .get(self.selected)
                    .and_then(|index| favorites.get(*index))
                    .cloned()
                {
                    return AppAction::AddToQueue {
                        song: Box::new(song),
                        position: InsertPosition::Next,
                    };
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('d')) | (KeyModifiers::NONE, KeyCode::Delete) => {
                if let Some(original_index) = filtered.get(self.selected).copied()
                    && let Some(song) = favorites.get(original_index)
                    && ctx.storage.remove_favorite(song)
                {
                    let remaining = ctx.storage.load_favorites();
                    let remaining_len = self.filtered_song_indices(&remaining).len();
                    self.clamp_selection(remaining_len);
                    return AppAction::ShowNotification(lx_core::events::Notification::info(
                        "已取消收藏",
                    ));
                }
            }
            (KeyModifiers::NONE, KeyCode::Char('f')) => {
                if let Some(original_index) = filtered.get(self.selected).copied()
                    && let Some(song) = favorites.get(original_index).cloned()
                {
                    return AppAction::ToggleFavoriteSong(Box::new(song));
                }
            }
            _ => {}
        }
        AppAction::None
    }

    pub fn render(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        ctx: &AppContext,
        cache: &mut SortedListCache,
    ) {
        let favorites = self.sorted_favorites(ctx, cache);
        let filtered = self.filtered_song_indices(favorites);
        self.clamp_selection(filtered.len());

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(crate::theme::border(ctx)))
            .title(format!(
                " 收藏 {}/{} · 排序 {} · s 切换 · / 筛选 ",
                filtered.len(),
                favorites.len(),
                self.sort_label()
            ));
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height == 0 {
            return;
        }

        let show_search = self.filter.is_active() || !self.filter.query().is_empty();
        let mut cursor_y = inner.y;
        if show_search {
            self.filter
                .render(Rect::new(inner.x, cursor_y, inner.width, 1), buf, ctx);
            cursor_y = cursor_y.saturating_add(1);
        }

        if cursor_y >= inner.bottom() {
            return;
        }
        Paragraph::new(Line::from(Span::styled(
            super::components::song_table::header(inner.width),
            Style::new()
                .fg(crate::theme::subtext0(ctx))
                .add_modifier(Modifier::BOLD),
        )))
        .render(Rect::new(inner.x, cursor_y, inner.width, 1), buf);
        cursor_y = cursor_y.saturating_add(1);

        let list = Rect::new(
            inner.x,
            cursor_y,
            inner.width,
            inner.bottom().saturating_sub(cursor_y),
        );
        self.viewport_height = list.height.max(1) as usize;

        if favorites.is_empty() {
            Paragraph::new("暂无收藏，播放时按 Ctrl+L 或列表中按 f 添加")
                .style(Style::new().fg(crate::theme::muted(ctx)))
                .render(list, buf);
            return;
        }
        if filtered.is_empty() {
            Paragraph::new(format!("没有匹配“{}”的歌曲", self.filter.query()))
                .style(Style::new().fg(crate::theme::overlay1(ctx)))
                .render(list, buf);
            return;
        }
        if list.height == 0 {
            return;
        }

        if self.selected >= self.scroll + self.viewport_height {
            self.scroll = self.selected.saturating_sub(self.viewport_height - 1);
        } else if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        self.scroll = self
            .scroll
            .min(filtered.len().saturating_sub(self.viewport_height));

        for (row, filtered_index) in
            (self.scroll..filtered.len().min(self.scroll + self.viewport_height)).enumerate()
        {
            let Some(song) = favorites.get(filtered[filtered_index]) else {
                continue;
            };
            let style = if filtered_index == self.selected {
                Style::new()
                    .bg(crate::theme::accent(ctx))
                    .fg(crate::theme::selection_fg(ctx))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(crate::theme::text(ctx))
            };
            Paragraph::new(Line::from(Span::styled(
                super::components::song_table::row(song, filtered_index, list.width),
                style,
            )))
            .render(Rect::new(list.x, list.y + row as u16, list.width, 1), buf);
        }
    }

    pub fn handle_mouse(
        &mut self,
        event: MouseEvent,
        area: Rect,
        ctx: &AppContext,
        cache: &mut SortedListCache,
        activate: bool,
    ) -> AppAction {
        let favorites = self.sorted_favorites(ctx, cache);
        let filtered = self.filtered_song_indices(favorites);
        let scroll_amount = ctx
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .ui
            .scroll_amount
            .max(1);
        match event.kind {
            MouseEventKind::ScrollUp => {
                self.selected = self.selected.saturating_sub(scroll_amount);
            }
            MouseEventKind::ScrollDown => {
                self.selected =
                    (self.selected + scroll_amount).min(filtered.len().saturating_sub(1));
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let inner = Block::default().borders(Borders::ALL).inner(area);
                let search_height =
                    u16::from(self.filter.is_active() || !self.filter.query().is_empty());
                if search_height == 1 && event.row == inner.y {
                    self.filter.activate();
                    return AppAction::None;
                }
                let list_y = inner.y.saturating_add(search_height).saturating_add(1);
                if event.row >= list_y && event.row < inner.bottom() {
                    let selected = self.scroll + event.row.saturating_sub(list_y) as usize;
                    if selected < filtered.len() {
                        self.selected = selected;
                        if activate {
                            let songs = filtered
                                .iter()
                                .filter_map(|index| favorites.get(*index).cloned())
                                .collect();
                            return AppAction::PlaySong {
                                songs,
                                index: selected,
                            };
                        }
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
        ctx: &AppContext,
        cache: &mut SortedListCache,
    ) -> Option<(Vec<SongInfo>, usize)> {
        let favorites = self.sorted_favorites(ctx, cache);
        let filtered = self.filtered_song_indices(favorites);
        let inner = Block::default().borders(Borders::ALL).inner(area);
        let search_height = u16::from(self.filter.is_active() || !self.filter.query().is_empty());
        let list_y = inner.y.saturating_add(search_height).saturating_add(1);
        if event.row < list_y || event.row >= inner.bottom() {
            return None;
        }
        let index = self.scroll + event.row.saturating_sub(list_y) as usize;
        if index >= filtered.len() {
            return None;
        }
        let original_index = filtered.get(index).copied()?;
        // 菜单仍按过滤视图返回 (songs, index)；但右键会退出过滤，列表回到
        // 完整视图，selected 必须映射回原始下标，否则会指向另一首歌。
        let songs = filtered
            .iter()
            .filter_map(|original| favorites.get(*original).cloned())
            .collect::<Vec<_>>();
        // deactivate() 只退出输入模式、保留 query，这里要真正关闭过滤
        self.filter.reset();
        self.selected = original_index;
        // 同步调整 scroll，让该行在完整视图中可见（与 render 的滚动逻辑一致）
        if self.selected >= self.scroll + self.viewport_height {
            self.scroll = self.selected.saturating_sub(self.viewport_height - 1);
        } else if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        self.scroll = self
            .scroll
            .min(favorites.len().saturating_sub(self.viewport_height));
        Some((songs, index))
    }

    fn filtered_song_indices(&self, favorites: &[SongInfo]) -> Vec<usize> {
        let query = self.filter.query().trim().to_lowercase();
        // favorites 已经是按当前排序方式排好序的缓存结果，这里只做过滤，
        // 不再重复排序。
        (0..favorites.len())
            .filter(|index| {
                let song = &favorites[*index];
                query.is_empty()
                    || song.name.to_lowercase().contains(&query)
                    || song.singer.to_lowercase().contains(&query)
                    || song.album_name.to_lowercase().contains(&query)
                    || song.source.as_str().contains(&query)
            })
            .collect()
    }

    /// 返回按当前排序方式排列的收藏列表（来自页面缓存，命中时零拷贝）。
    fn sorted_favorites<'a>(
        &self,
        ctx: &AppContext,
        cache: &'a mut SortedListCache,
    ) -> &'a [SongInfo] {
        let version = ctx.storage.generation();
        let mode = self.sort_mode;
        cache.get_or_build(version, mode, SortTarget::Favorites, || {
            ctx.storage.load_favorites()
        })
    }

    fn clamp_selection(&mut self, len: usize) {
        if len == 0 {
            self.selected = 0;
            self.scroll = 0;
        } else {
            self.selected = self.selected.min(len - 1);
            self.scroll = self.scroll.min(self.selected);
        }
    }

    fn log_keyboard_playback(&self, songs: &[SongInfo]) {
        let selected = songs.get(self.selected);
        let order = songs
            .iter()
            .take(5)
            .map(|song| format!("{}:{}", song.id, song.name))
            .collect::<Vec<_>>();
        tracing::debug!(
            sort = ?self.sort_mode,
            selected_index = self.selected,
            selected_id = selected.map(|song| song.id.as_str()),
            selected_name = selected.map(|song| song.name.as_str()),
            ?order,
            "favorites keyboard playback"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::FavoritesPage;
    use crate::pages::sort::{SortMode, SortTarget, sorted_songs};
    use lx_core::model::song::SongInfo;
    use lx_core::model::source::SourceId;

    #[test]
    fn filters_title_artist_album_and_source() {
        let mut page = FavoritesPage::new();
        let mut song = SongInfo::new("1".into(), SourceId::Kw, "晴天".into(), "周杰伦".into());
        song.album_name = "叶惠美".into();
        let songs = vec![song];

        for query in ["晴天", "周杰伦", "叶惠美", "kw"] {
            page.filter.set_query(query);
            assert_eq!(page.filtered_song_indices(&songs), vec![0]);
        }
    }

    #[test]
    fn defaults_to_most_recent_favorite_and_cycles_sorting() {
        let mut page = FavoritesPage::new();
        let songs = vec![
            SongInfo::new("1".into(), SourceId::Kw, "A".into(), "X".into()),
            SongInfo::new("2".into(), SourceId::Kw, "B".into(), "Y".into()),
        ];

        // filtered_song_indices 只负责过滤；排序由 SortedListCache 提供，
        // 输入按当前排序方式排好序时顺序保持不变。
        assert_eq!(page.filtered_song_indices(&songs), vec![0, 1]);
        assert_eq!(page.cycle_sort(), SortMode::Oldest);
        assert_eq!(page.filtered_song_indices(&songs), vec![0, 1]);
    }

    #[test]
    fn keyboard_selection_indices_follow_the_sorted_favorites() {
        let page = FavoritesPage::new();
        let stored = vec![
            SongInfo::new("1".into(), SourceId::Kw, "C".into(), "X".into()),
            SongInfo::new("2".into(), SourceId::Kw, "B".into(), "Y".into()),
            SongInfo::new("3".into(), SourceId::Kw, "A".into(), "Z".into()),
        ];
        let sorted = sorted_songs(stored, SortMode::TitleAsc, SortTarget::Favorites);
        let filtered = page.filtered_song_indices(&sorted);
        let songs = filtered
            .iter()
            .filter_map(|index| sorted.get(*index).cloned())
            .collect::<Vec<_>>();

        assert_eq!(songs[0].name, "A");
        assert_eq!(songs[2].name, "C");
    }
}
