//! 歌手与专辑详情页。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lx_core::events::{AppAction, InsertPosition, Notification};
use lx_core::keybinding::KeybindingResolver;
use lx_core::model::playlist::{Album, Artist};
use lx_core::model::song::SongInfo;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::context::AppContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailsFocus {
    Albums,
    Songs,
}

#[derive(Debug, Clone)]
pub enum DetailsTarget {
    Artist(Artist),
    Album(Album),
}

pub struct DetailsPage {
    target: DetailsTarget,
    albums: Vec<Album>,
    songs: Vec<SongInfo>,
    focus: DetailsFocus,
    selected_album: usize,
    selected_song: usize,
    album_scroll: usize,
    song_scroll: usize,
    loading: bool,
    error: Option<String>,
    /// 歌手歌曲当前已加载的页码（从 0 开始）
    songs_page: u32,
    /// 歌手歌曲是否还有下一页
    has_more: bool,
    /// 下一页是否正在加载，防止重复触发
    loading_more: bool,
}

impl DetailsPage {
    pub fn artist(artist: Artist) -> Self {
        Self {
            target: DetailsTarget::Artist(artist),
            albums: Vec::new(),
            songs: Vec::new(),
            focus: DetailsFocus::Albums,
            selected_album: 0,
            selected_song: 0,
            album_scroll: 0,
            song_scroll: 0,
            loading: true,
            error: None,
            songs_page: 0,
            has_more: false,
            loading_more: false,
        }
    }

    pub fn album(album: Album) -> Self {
        Self {
            target: DetailsTarget::Album(album),
            albums: Vec::new(),
            songs: Vec::new(),
            focus: DetailsFocus::Songs,
            selected_album: 0,
            selected_song: 0,
            album_scroll: 0,
            song_scroll: 0,
            loading: true,
            error: None,
            songs_page: 0,
            has_more: false,
            loading_more: false,
        }
    }

    /// 歌手目标（仅歌手详情页返回 Some），供分页加载任务使用。
    pub fn artist_target(&self) -> Option<&Artist> {
        match &self.target {
            DetailsTarget::Artist(artist) => Some(artist),
            DetailsTarget::Album(_) => None,
        }
    }

    /// 当前歌手歌曲页码。
    pub fn songs_page(&self) -> u32 {
        self.songs_page
    }

    /// 滚动位置接近末尾且还有下一页时返回 true，由主循环触发追加加载。
    pub fn wants_more_songs(&self) -> bool {
        !self.loading
            && !self.loading_more
            && self.has_more
            && self.selected_song + 20 >= self.songs.len()
    }

    /// 首屏数据写入（第 0 页）。
    pub fn set_artist_page(&mut self, albums: Vec<Album>, songs: Vec<SongInfo>, has_more: bool) {
        self.albums = albums;
        self.songs = songs;
        self.has_more = has_more;
        self.loading = false;
        self.error = if self.songs.is_empty() && self.albums.is_empty() {
            Some("未找到该歌手的歌曲".to_string())
        } else {
            None
        };
        self.clamp_selection();
    }

    /// 下一页加载失败时停止自动分页，避免主循环高频重试。
    pub fn set_no_more_songs(&mut self) {
        self.loading_more = false;
        self.has_more = false;
    }

    /// 标记下一页正在加载，防止主循环重复触发。
    pub fn mark_loading_more(&mut self) {
        self.loading_more = true;
    }

    /// 追加下一页歌曲（按 音源+ID 去重）与专辑（按 ID 去重）。
    pub fn append_page(&mut self, albums: Vec<Album>, songs: Vec<SongInfo>, has_more: bool) {
        self.loading_more = false;
        self.songs_page += 1;
        self.has_more = has_more;
        let mut seen: std::collections::HashSet<(String, _)> = self
            .songs
            .iter()
            .map(|song| (song.id.clone(), song.source))
            .collect();
        for song in songs {
            if seen.insert((song.id.clone(), song.source)) {
                self.songs.push(song);
            }
        }
        let mut album_keys: std::collections::HashSet<String> =
            self.albums.iter().map(|album| album.id.clone()).collect();
        for album in albums {
            if album_keys.insert(album.id.clone()) {
                self.albums.push(album);
            }
        }
        self.clamp_selection();
    }

    pub fn update_album(&mut self, songs: Result<Vec<SongInfo>, String>) {
        self.songs = songs.unwrap_or_default();
        self.loading = false;
        self.error = if self.songs.is_empty() {
            Some("未找到该专辑的曲目".to_string())
        } else {
            None
        };
        self.clamp_selection();
    }

    pub fn update_error(&mut self, error: String) {
        self.loading = false;
        self.error = Some(error);
        self.albums.clear();
        self.songs.clear();
        self.clamp_selection();
    }

    pub fn handle_input(
        &mut self,
        key: &KeyEvent,
        ctx: &AppContext,
        resolver: &KeybindingResolver,
    ) -> AppAction {
        if matches!(
            (key.modifiers, key.code),
            (KeyModifiers::NONE, KeyCode::Esc)
        ) {
            return AppAction::GoBack;
        }
        if matches!(
            (key.modifiers, key.code),
            (KeyModifiers::NONE, KeyCode::Tab)
        ) && matches!(self.target, DetailsTarget::Artist(_))
        {
            self.toggle_focus();
            return AppAction::None;
        }

        let action = resolver.resolve_page("details", key);
        match action {
            Some(lx_core::keybinding::Action::ListSelectUp) => self.move_up(ctx),
            Some(lx_core::keybinding::Action::ListSelectDown) => self.move_down(ctx),
            Some(lx_core::keybinding::Action::ListSelectFirst) => self.select_first(),
            Some(lx_core::keybinding::Action::ListSelectLast) => self.select_last(),
            Some(lx_core::keybinding::Action::ListPageUp) => self.page_up(),
            Some(lx_core::keybinding::Action::ListPageDown) => self.page_down(),
            Some(lx_core::keybinding::Action::ListActivate) => return self.activate(),
            Some(lx_core::keybinding::Action::ListAddToQueue) => {
                return self.add_selected(InsertPosition::End);
            }
            Some(lx_core::keybinding::Action::ListAddToQueueNext) => {
                return self.add_selected(InsertPosition::Next);
            }
            Some(lx_core::keybinding::Action::ListDownload) => {
                return self.download_selected();
            }
            _ => {}
        }

        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Char('p')) => self.play_all(),
            (KeyModifiers::NONE, KeyCode::Char('a')) => self.add_selected(InsertPosition::End),
            (KeyModifiers::NONE, KeyCode::Char('A'))
            | (KeyModifiers::SHIFT, KeyCode::Char('A')) => self.add_selected(InsertPosition::Next),
            (KeyModifiers::NONE, KeyCode::Char('f')) => self.toggle_favorite(ctx),
            (KeyModifiers::NONE, KeyCode::Char('h')) | (KeyModifiers::NONE, KeyCode::Left)
                if matches!(self.target, DetailsTarget::Artist(_))
                    && self.focus == DetailsFocus::Songs =>
            {
                self.focus = DetailsFocus::Albums;
                AppAction::None
            }
            (KeyModifiers::NONE, KeyCode::Char('l')) | (KeyModifiers::NONE, KeyCode::Right)
                if matches!(self.target, DetailsTarget::Artist(_))
                    && self.focus == DetailsFocus::Albums =>
            {
                self.focus = DetailsFocus::Songs;
                AppAction::None
            }
            (KeyModifiers::NONE, KeyCode::Up) => {
                self.move_up(ctx);
                AppAction::None
            }
            (KeyModifiers::NONE, KeyCode::Down) => {
                self.move_down(ctx);
                AppAction::None
            }
            (KeyModifiers::NONE, KeyCode::Home) | (KeyModifiers::NONE, KeyCode::Char('g')) => {
                self.select_first();
                AppAction::None
            }
            (KeyModifiers::NONE, KeyCode::End)
            | (KeyModifiers::NONE, KeyCode::Char('G'))
            | (KeyModifiers::SHIFT, KeyCode::Char('G')) => {
                self.select_last();
                AppAction::None
            }
            (KeyModifiers::CONTROL, KeyCode::Char('u')) | (KeyModifiers::NONE, KeyCode::PageUp) => {
                self.page_up();
                AppAction::None
            }
            (KeyModifiers::CONTROL, KeyCode::Char('d'))
            | (KeyModifiers::NONE, KeyCode::PageDown) => {
                self.page_down();
                AppAction::None
            }
            _ => AppAction::None,
        }
    }

    pub fn handle_mouse(
        &mut self,
        event: MouseEvent,
        area: Rect,
        _ctx: &AppContext,
        activate: bool,
    ) -> AppAction {
        let chunks = self.content_chunks(area);
        match event.kind {
            MouseEventKind::ScrollUp => {
                if chunks
                    .songs
                    .contains(Position::new(event.column, event.row))
                {
                    self.selected_song = self.selected_song.saturating_sub(1);
                    self.ensure_song_visible(chunks.songs);
                } else if chunks
                    .albums
                    .contains(Position::new(event.column, event.row))
                {
                    self.selected_album = self.selected_album.saturating_sub(1);
                    self.ensure_album_visible(chunks.albums);
                }
            }
            MouseEventKind::ScrollDown => {
                if chunks
                    .songs
                    .contains(Position::new(event.column, event.row))
                {
                    self.selected_song =
                        (self.selected_song + 1).min(self.songs.len().saturating_sub(1));
                    self.ensure_song_visible(chunks.songs);
                } else if chunks
                    .albums
                    .contains(Position::new(event.column, event.row))
                {
                    self.selected_album =
                        (self.selected_album + 1).min(self.albums.len().saturating_sub(1));
                    self.ensure_album_visible(chunks.albums);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let position = Position::new(event.column, event.row);
                if let Some(index) =
                    self.row_at(chunks.songs, position, self.song_scroll, self.songs.len())
                {
                    self.focus = DetailsFocus::Songs;
                    self.selected_song = index;
                    if activate {
                        return self.activate();
                    }
                } else if let Some(index) = self.row_at(
                    chunks.albums,
                    position,
                    self.album_scroll,
                    self.albums.len(),
                ) {
                    self.focus = DetailsFocus::Albums;
                    self.selected_album = index;
                    if activate {
                        return self.activate();
                    }
                }
            }
            _ => {}
        }
        AppAction::None
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        let title = match &self.target {
            DetailsTarget::Artist(artist) => format!(
                "歌手 · {} · p 播放全部 · a/A 入队 · f 收藏 · Tab 切换区域",
                artist.name
            ),
            DetailsTarget::Album(album) => format!(
                "专辑 · {} · {} · p 播放全部 · a/A 入队 · f 收藏",
                album.name, album.artist
            ),
        };
        let outer = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(crate::theme::border(ctx)))
            .title(title);
        let inner = outer.inner(area);
        outer.render(area, buf);

        if self.loading {
            Paragraph::new("正在加载详情...")
                .style(Style::new().fg(crate::theme::muted(ctx)))
                .render(inner, buf);
            return;
        }
        if let Some(error) = &self.error
            && self.songs.is_empty()
        {
            Paragraph::new(error.as_str())
                .style(Style::new().fg(crate::theme::red(ctx)))
                .render(inner, buf);
            // 错误信息占据整个内容区后直接返回，避免被下方的
            // 歌手/专辑布局面板覆盖，用户看不到真实失败原因。
            return;
        }

        match self.target {
            DetailsTarget::Artist(_) => {
                let chunks = self.content_chunks(inner);
                self.render_albums(chunks.albums, buf, ctx);
                self.render_songs(chunks.songs, buf, ctx);
            }
            DetailsTarget::Album(_) => self.render_songs(inner, buf, ctx),
        }
    }

    fn activate(&self) -> AppAction {
        match self.focus {
            DetailsFocus::Songs => {
                if self.songs.get(self.selected_song).is_some() {
                    AppAction::PlaySong {
                        songs: self.songs.clone(),
                        index: self.selected_song,
                    }
                } else {
                    AppAction::None
                }
            }
            DetailsFocus::Albums => self
                .albums
                .get(self.selected_album)
                .map(|album| AppAction::ShowAlbumDetails(Box::new(album.clone())))
                .unwrap_or(AppAction::None),
        }
    }

    fn play_all(&self) -> AppAction {
        if self.songs.is_empty() {
            return AppAction::ShowNotification(Notification::info("暂无可播放歌曲"));
        }
        AppAction::PlaySong {
            songs: self.songs.clone(),
            index: 0,
        }
    }

    fn add_selected(&self, position: InsertPosition) -> AppAction {
        self.songs
            .get(self.selected_song)
            .cloned()
            .map(|song| AppAction::AddToQueue {
                song: Box::new(song),
                position,
            })
            .unwrap_or_else(|| {
                AppAction::ShowNotification(Notification::info("暂无可加入队列的歌曲"))
            })
    }

    fn download_selected(&self) -> AppAction {
        self.songs
            .get(self.selected_song)
            .cloned()
            .map(|song| AppAction::DownloadSong(Box::new(song)))
            .unwrap_or_else(|| AppAction::ShowNotification(Notification::info("暂无可下载的歌曲")))
    }

    fn toggle_favorite(&self, ctx: &AppContext) -> AppAction {
        let Some(song) = self.songs.get(self.selected_song) else {
            return AppAction::ShowNotification(Notification::info("暂无可收藏歌曲"));
        };
        if ctx.storage.is_favorite(song) {
            ctx.storage.remove_favorite(song);
            AppAction::ShowNotification(Notification::success("已取消收藏歌曲"))
        } else {
            ctx.storage.add_favorite(song);
            AppAction::ShowNotification(Notification::success("已收藏歌曲"))
        }
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            DetailsFocus::Albums => DetailsFocus::Songs,
            DetailsFocus::Songs => DetailsFocus::Albums,
        };
    }

    fn move_up(&mut self, ctx: &AppContext) {
        match self.focus {
            DetailsFocus::Albums => {
                self.selected_album = self.selected_album.saturating_sub(1);
            }
            DetailsFocus::Songs => {
                self.selected_song = self.selected_song.saturating_sub(1);
            }
        }
        self.wrap_selection(ctx, false);
    }

    fn move_down(&mut self, ctx: &AppContext) {
        match self.focus {
            DetailsFocus::Albums => {
                self.selected_album =
                    (self.selected_album + 1).min(self.albums.len().saturating_sub(1));
            }
            DetailsFocus::Songs => {
                self.selected_song =
                    (self.selected_song + 1).min(self.songs.len().saturating_sub(1));
            }
        }
        self.wrap_selection(ctx, true);
    }

    fn wrap_selection(&mut self, ctx: &AppContext, down: bool) {
        if !ctx
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .ui
            .wrap_navigation
        {
            return;
        }
        match self.focus {
            DetailsFocus::Albums if self.albums.is_empty() => {}
            DetailsFocus::Albums if down && self.selected_album + 1 >= self.albums.len() => {
                self.selected_album = 0;
            }
            DetailsFocus::Albums if !down && self.selected_album == 0 => {
                self.selected_album = self.albums.len() - 1;
            }
            DetailsFocus::Songs if self.songs.is_empty() => {}
            DetailsFocus::Songs if down && self.selected_song + 1 >= self.songs.len() => {
                self.selected_song = 0;
            }
            DetailsFocus::Songs if !down && self.selected_song == 0 => {
                self.selected_song = self.songs.len() - 1;
            }
            _ => {}
        }
    }

    fn select_first(&mut self) {
        match self.focus {
            DetailsFocus::Albums => self.selected_album = 0,
            DetailsFocus::Songs => self.selected_song = 0,
        }
    }

    fn select_last(&mut self) {
        match self.focus {
            DetailsFocus::Albums => self.selected_album = self.albums.len().saturating_sub(1),
            DetailsFocus::Songs => self.selected_song = self.songs.len().saturating_sub(1),
        }
    }

    fn page_up(&mut self) {
        match self.focus {
            DetailsFocus::Albums => self.selected_album = self.selected_album.saturating_sub(10),
            DetailsFocus::Songs => self.selected_song = self.selected_song.saturating_sub(10),
        }
    }

    fn page_down(&mut self) {
        match self.focus {
            DetailsFocus::Albums => {
                self.selected_album =
                    (self.selected_album + 10).min(self.albums.len().saturating_sub(1));
            }
            DetailsFocus::Songs => {
                self.selected_song =
                    (self.selected_song + 10).min(self.songs.len().saturating_sub(1));
            }
        }
    }

    fn clamp_selection(&mut self) {
        self.selected_album = self.selected_album.min(self.albums.len().saturating_sub(1));
        self.selected_song = self.selected_song.min(self.songs.len().saturating_sub(1));
    }

    fn content_chunks(&self, area: Rect) -> DetailsChunks {
        if matches!(self.target, DetailsTarget::Album(_)) {
            return DetailsChunks {
                albums: Rect::default(),
                songs: area,
            };
        }
        let [albums, songs] = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(34), Constraint::Percentage(66)])
            .areas(area);
        DetailsChunks { albums, songs }
    }

    fn render_albums(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(crate::theme::border(ctx)))
            .title(format!("专辑 ({})", self.albums.len()));
        let inner = block.inner(area);
        block.render(area, buf);
        if self.albums.is_empty() {
            Paragraph::new("暂无专辑")
                .style(Style::new().fg(crate::theme::muted(ctx)))
                .render(inner, buf);
            return;
        }
        self.ensure_album_visible(inner);
        for index in
            self.album_scroll..(self.album_scroll + inner.height as usize).min(self.albums.len())
        {
            let album = &self.albums[index];
            let text = truncate(&format!("{} · {}", album.name, album.artist), inner.width);
            let style = if self.focus == DetailsFocus::Albums && index == self.selected_album {
                Style::new()
                    .bg(crate::theme::accent(ctx))
                    .fg(crate::theme::selection_fg(ctx))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(crate::theme::text(ctx))
            };
            Paragraph::new(Line::from(Span::styled(text, style))).render(
                Rect::new(
                    inner.x,
                    inner.y + (index - self.album_scroll) as u16,
                    inner.width,
                    1,
                ),
                buf,
            );
        }
    }

    fn render_songs(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(crate::theme::border(ctx)))
            .title(format!("歌曲 ({})", self.songs.len()));
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height < 2 {
            return;
        }
        if self.songs.is_empty() {
            Paragraph::new("暂无歌曲")
                .style(Style::new().fg(crate::theme::muted(ctx)))
                .render(inner, buf);
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
            inner.y + 1,
            inner.width,
            inner.height.saturating_sub(1),
        );
        self.ensure_song_visible(list_area);
        for index in
            self.song_scroll..(self.song_scroll + list_area.height as usize).min(self.songs.len())
        {
            let text =
                super::components::song_table::row(&self.songs[index], index, list_area.width);
            let style = if self.focus == DetailsFocus::Songs && index == self.selected_song {
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
                    list_area.y + (index - self.song_scroll) as u16,
                    list_area.width,
                    1,
                ),
                buf,
            );
        }
    }

    fn ensure_album_visible(&mut self, area: Rect) {
        let visible = area.height.max(1) as usize;
        if self.selected_album >= self.album_scroll + visible {
            self.album_scroll = self.selected_album + 1 - visible;
        } else if self.selected_album < self.album_scroll {
            self.album_scroll = self.selected_album;
        }
        self.album_scroll = self
            .album_scroll
            .min(self.albums.len().saturating_sub(visible));
    }

    fn ensure_song_visible(&mut self, area: Rect) {
        let visible = area.height.max(1) as usize;
        if self.selected_song >= self.song_scroll + visible {
            self.song_scroll = self.selected_song + 1 - visible;
        } else if self.selected_song < self.song_scroll {
            self.song_scroll = self.selected_song;
        }
        self.song_scroll = self
            .song_scroll
            .min(self.songs.len().saturating_sub(visible));
    }

    /// `len` 由调用方传入：歌曲区传 `songs.len()`，专辑区传 `albums.len()`，
    /// 两个列表长度不同时才不会互相错判边界。
    fn row_at(&self, area: Rect, position: Position, scroll: usize, len: usize) -> Option<usize> {
        if !area.contains(position) {
            return None;
        }
        let index = scroll + position.y.saturating_sub(area.y) as usize;
        (index < len).then_some(index)
    }
}

struct DetailsChunks {
    albums: Rect,
    songs: Rect,
}

fn truncate(value: &str, width: u16) -> String {
    // 按显示宽度截断（CJK 占 2 列），共享实现见 components::text
    super::components::text::truncate_width(value, width as usize).into_owned()
}

#[cfg(test)]
mod tests {
    use super::{DetailsPage, DetailsTarget};
    use lx_core::model::playlist::{Album, Artist};

    #[test]
    fn artist_starts_with_album_focus_and_album_starts_with_song_focus() {
        let artist = DetailsPage::artist(Artist {
            id: "1".to_string(),
            name: "歌手".to_string(),
            source: lx_core::model::source::SourceId::Kw,
            cover_url: None,
        });
        assert!(matches!(artist.target, DetailsTarget::Artist(_)));

        let album = DetailsPage::album(Album {
            id: "1".to_string(),
            name: "专辑".to_string(),
            source: lx_core::model::source::SourceId::Kw,
            cover_url: None,
            artist: "歌手".to_string(),
        });
        assert!(matches!(album.target, DetailsTarget::Album(_)));
    }
}
