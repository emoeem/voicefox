//! 歌曲右键上下文菜单。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lx_core::keybinding::{Action, KeybindingResolver};
use lx_core::model::song::SongInfo;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};
use unicode_width::UnicodeWidthStr;

use crate::context::AppContext;
use crate::pages::sort::{SortMode, SortTarget};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SongMenuKind {
    Queue,
    Standard,
    History,
    Local,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SongMenuAction {
    Play,
    Download,
    PlayNext,
    AddToQueue,
    OpenCustomPlaylists,
    OpenPlaybackControls,
    Playback(PlaybackMenuAction),
    AddToCustomPlaylist(String),
    NoCustomPlaylists,
    ToggleFavorite,
    CycleSort(SortTarget),
    RemoveFromQueue,
    RemoveFromHistory,
    ClearHistory,
    DeleteLocal,
    RemoveFromCustomPlaylist(String),
    ViewArtist,
    ViewAlbum,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackMenuAction {
    CycleSpeed,
    UseDefaultAudioDevice,
    CycleReplayGainMode,
    CycleReplayGainPreamp,
    ToggleReplayGainClip,
    CycleEqualizer,
    CycleChannelMode,
    CycleBalance,
    FadeIn,
    FadeOut,
    SetAbLoopStart,
    SetAbLoopEnd,
    ClearAbLoop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuOutcome {
    None,
    Close,
    Action(SongMenuAction),
}

#[derive(Debug, Clone, Default)]
pub struct SongContextMenuOptions {
    pub sort: Option<(SortTarget, SortMode)>,
    pub custom_playlists: Vec<(String, String)>,
    pub current_custom_playlist: Option<String>,
    pub playback: Option<PlaybackMenuState>,
}

#[derive(Debug, Clone, Default)]
pub struct PlaybackMenuState {
    pub speed: f64,
    pub audio_device: String,
    pub replaygain_mode: String,
    pub replaygain_preamp: f64,
    pub replaygain_clip: bool,
    pub equalizer: String,
    pub channel_mode: String,
    pub balance: f64,
    pub ab_loop: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuGroup {
    Playback,
    Operations,
    Playlist,
    Info,
    Manage,
    Other,
}

#[derive(Debug, Clone)]
pub struct MenuItem {
    pub label: String,
    pub action: SongMenuAction,
    pub(crate) icon: Option<&'static str>,
    pub(crate) shortcut: Option<&'static str>,
    pub(crate) disabled: bool,
    pub(crate) group: MenuGroup,
}

impl MenuItem {
    pub fn new(label: impl Into<String>, action: SongMenuAction) -> Self {
        Self {
            label: label.into(),
            action,
            icon: None,
            shortcut: None,
            disabled: false,
            group: MenuGroup::Other,
        }
    }
    pub fn with_icon(mut self, icon: &'static str) -> Self {
        self.icon = Some(icon);
        self
    }
    pub fn with_shortcut(mut self, sc: &'static str) -> Self {
        self.shortcut = Some(sc);
        self
    }
    pub fn disabled(mut self) -> Self {
        self.disabled = true;
        self
    }
    pub fn in_group(mut self, g: MenuGroup) -> Self {
        self.group = g;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuLevel {
    Root,
    CustomPlaylists,
    PlaybackControls,
}

#[derive(Debug, Clone)]
pub struct SongContextMenu {
    origin: Position,
    songs: Vec<SongInfo>,
    index: usize,
    selected: usize,
    scroll_offset: usize,
    level: MenuLevel,
    root_items: Vec<MenuItem>,
    custom_playlist_items: Vec<MenuItem>,
    playback_control_items: Vec<MenuItem>,
}

impl SongContextMenu {
    pub fn new(
        origin: Position,
        songs: Vec<SongInfo>,
        index: usize,
        kind: SongMenuKind,
        is_favorite: bool,
        options: SongContextMenuOptions,
    ) -> Option<Self> {
        songs.get(index)?;
        let SongContextMenuOptions {
            sort,
            custom_playlists,
            current_custom_playlist,
            playback,
        } = options;

        use MenuGroup as G;
        use SongMenuAction as A;

        let mut root_items: Vec<MenuItem> = Vec::new();

        // —— 播放组 ——
        root_items.push(
            MenuItem::new("播放", A::Play)
                .with_icon("▶")
                .with_shortcut("Enter")
                .in_group(G::Playback),
        );
        if kind != SongMenuKind::Local {
            root_items.push(
                MenuItem::new("下载歌曲", A::Download)
                    .with_icon("⇩")
                    .with_shortcut("d")
                    .in_group(G::Playback),
            );
        }
        if kind != SongMenuKind::Queue {
            root_items.push(
                MenuItem::new("设为下一首", A::PlayNext)
                    .with_icon("⟶")
                    .with_shortcut("N")
                    .in_group(G::Playback),
            );
            root_items.push(
                MenuItem::new("加入队尾", A::AddToQueue)
                    .with_icon("＋")
                    .with_shortcut("+")
                    .in_group(G::Playback),
            );
        }

        // —— 歌单组 ——
        root_items.push(
            MenuItem::new("加入自建歌单…", A::OpenCustomPlaylists)
                .with_icon("♪")
                .in_group(G::Playlist),
        );
        if let Some(playlist_id) = current_custom_playlist {
            root_items.push(
                MenuItem::new("从当前歌单移除", A::RemoveFromCustomPlaylist(playlist_id))
                    .with_icon("✕")
                    .with_shortcut("r")
                    .in_group(G::Playlist),
            );
        }
        root_items.push(
            MenuItem::new(
                if is_favorite {
                    "取消收藏"
                } else {
                    "收藏歌曲"
                },
                A::ToggleFavorite,
            )
            .with_icon(if is_favorite { "☆" } else { "♥" })
            .with_shortcut("f")
            .in_group(G::Playlist),
        );

        // —— 信息组 ——
        let song = &songs[index];
        if !song.singer.trim().is_empty() {
            root_items.push(
                MenuItem::new("查看歌手", A::ViewArtist)
                    .with_icon("👤")
                    .in_group(G::Info),
            );
        }
        if !song.album_name.trim().is_empty() {
            root_items.push(
                MenuItem::new("查看专辑", A::ViewAlbum)
                    .with_icon("◉")
                    .in_group(G::Info),
            );
        }

        // —— 操作组 ——
        if playback.is_some() {
            root_items.push(
                MenuItem::new("播放控制…", A::OpenPlaybackControls)
                    .with_icon("⚙")
                    .in_group(G::Operations),
            );
        }
        if let Some((target, mode)) = sort {
            root_items.push(
                MenuItem::new(
                    format!("排序：{}（切换）", mode.label(target)),
                    A::CycleSort(target),
                )
                .with_icon("↕")
                .in_group(G::Operations),
            );
        }

        // —— 管理组（危险操作）——
        match kind {
            SongMenuKind::Queue => root_items.push(
                MenuItem::new("从队列移除", A::RemoveFromQueue)
                    .with_icon("✕")
                    .with_shortcut("Del")
                    .in_group(G::Manage),
            ),
            SongMenuKind::History => {
                root_items.push(
                    MenuItem::new("删除这条历史", A::RemoveFromHistory)
                        .with_icon("✕")
                        .with_shortcut("Del")
                        .in_group(G::Manage),
                );
                root_items.push(
                    MenuItem::new("清空播放历史", A::ClearHistory)
                        .with_icon("🗑")
                        .with_shortcut("⇧D")
                        .in_group(G::Manage),
                );
            }
            SongMenuKind::Local => root_items.push(
                MenuItem::new("删除本地文件", A::DeleteLocal)
                    .with_icon("🗑")
                    .with_shortcut("D")
                    .in_group(G::Manage),
            ),
            SongMenuKind::Standard => {}
        }

        let custom_playlist_items = if custom_playlists.is_empty() {
            vec![
                MenuItem::new("暂无自建歌单，请先创建", A::NoCustomPlaylists)
                    .disabled()
                    .with_icon("—"),
            ]
        } else {
            custom_playlists
                .into_iter()
                .map(|(id, name)| {
                    MenuItem::new(name, A::AddToCustomPlaylist(id))
                        .with_icon("♪")
                        .in_group(G::Playlist)
                })
                .collect()
        };
        let playback_control_items = playback
            .map(build_playback_control_items)
            .unwrap_or_default();

        Some(Self {
            origin,
            songs,
            index,
            selected: 0,
            scroll_offset: 0,
            level: MenuLevel::Root,
            root_items,
            custom_playlist_items,
            playback_control_items,
        })
    }

    pub fn songs(&self) -> &[SongInfo] {
        &self.songs
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn song(&self) -> &SongInfo {
        &self.songs[self.index]
    }

    pub fn handle_key(
        &mut self,
        key: &KeyEvent,
        resolver: &KeybindingResolver,
        page_scope: &str,
        bounds: Rect,
    ) -> MenuOutcome {
        let visible_items = self.visible_item_count(bounds);
        if matches!(
            (key.modifiers, key.code),
            (KeyModifiers::NONE, KeyCode::Esc | KeyCode::Char('q'))
        ) {
            return MenuOutcome::Close;
        }
        if matches!(
            (key.modifiers, key.code),
            (KeyModifiers::NONE, KeyCode::Enter)
        ) {
            return self.activate();
        }

        match resolver.resolve_page(page_scope, key) {
            Some(Action::ListSelectUp) => self.select_previous(visible_items),
            Some(Action::ListSelectDown) => self.select_next(visible_items),
            Some(Action::ListActivate) => return self.activate(),
            Some(Action::ListGoBack) => return MenuOutcome::Close,
            _ => match (key.modifiers, key.code) {
                (KeyModifiers::NONE, KeyCode::Up) => self.select_previous(visible_items),
                (KeyModifiers::NONE, KeyCode::Down) => self.select_next(visible_items),
                _ => {}
            },
        }
        MenuOutcome::None
    }

    pub fn handle_mouse(&mut self, event: MouseEvent, bounds: Rect) -> MenuOutcome {
        let area = self.area(bounds);
        let visible_items = Block::default().borders(Borders::ALL).inner(area).height as usize;
        match event.kind {
            MouseEventKind::ScrollUp => self.select_previous(visible_items),
            MouseEventKind::ScrollDown => self.select_next(visible_items),
            MouseEventKind::Moved => {
                if let Some(index) = item_at(
                    area,
                    Position::new(event.column, event.row),
                    self.items().len(),
                    self.scroll_offset,
                ) {
                    self.selected = index;
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let position = Position::new(event.column, event.row);
                let Some(index) = item_at(area, position, self.items().len(), self.scroll_offset)
                else {
                    return MenuOutcome::Close;
                };
                self.selected = index;
                return self.activate();
            }
            MouseEventKind::Down(MouseButton::Right) => {
                if self.level != MenuLevel::Root
                    && area.contains(Position::new(event.column, event.row))
                {
                    self.level = MenuLevel::Root;
                    self.selected = 0;
                    self.scroll_offset = 0;
                } else {
                    return MenuOutcome::Close;
                }
            }
            _ => {}
        }
        MenuOutcome::None
    }

    pub fn render(&self, bounds: Rect, buf: &mut Buffer, ctx: &AppContext) {
        let area = self.area(bounds);
        if area.width == 0 || area.height == 0 {
            return;
        }
        Clear.render(area, buf);

        let title = match self.level {
            MenuLevel::Root => " 歌曲操作 ".to_string(),
            MenuLevel::CustomPlaylists => " 歌曲操作 › 选择自建歌单 ".to_string(),
            MenuLevel::PlaybackControls => " 歌曲操作 › 播放控制 ".to_string(),
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(crate::theme::accent(ctx)))
            .style(
                Style::new()
                    .bg(crate::theme::surface0(ctx))
                    .fg(crate::theme::text(ctx)),
            )
            .title(title.as_str());
        let inner = block.inner(area);
        block.render(area, buf);

        let items = self.items();
        let visible = items
            .iter()
            .enumerate()
            .skip(self.scroll_offset)
            .take(inner.height as usize)
            .collect::<Vec<_>>();

        for (row, (index, item)) in visible.iter().enumerate() {
            let y = inner.y + row as u16;

            // 分组分隔线 —— 当前 item 不是第一，且 group 和上一个不同
            if *index > 0 && *index > self.scroll_offset {
                let prev = &items[index - 1];
                if prev.group != item.group && !prev.disabled {
                    let sep = "─".repeat(inner.width.saturating_sub(2) as usize);
                    Paragraph::new(Line::from(Span::styled(
                        format!(" {} ", sep),
                        Style::new()
                            .fg(crate::theme::surface2(ctx))
                            .bg(crate::theme::surface0(ctx)),
                    )))
                    .render(Rect::new(inner.x, y, inner.width, 1), buf);
                    continue;
                }
            }

            let (label_fg, label_bg, is_selected) = if item.disabled {
                (
                    crate::theme::surface2(ctx),
                    crate::theme::surface0(ctx),
                    false,
                )
            } else if *index == self.selected {
                (
                    crate::theme::selection_fg(ctx),
                    crate::theme::accent(ctx),
                    true,
                )
            } else {
                (crate::theme::text(ctx), crate::theme::surface0(ctx), false)
            };

            let style = Style::new().fg(label_fg).bg(label_bg);

            let mut line_spans: Vec<Span<'static>> = Vec::new();

            // 图标
            if let Some(icon) = item.icon {
                let icon_style = if is_selected {
                    Style::new()
                        .fg(crate::theme::selection_fg(ctx))
                        .bg(label_bg)
                } else if item.disabled {
                    Style::new().fg(crate::theme::surface2(ctx)).bg(label_bg)
                } else {
                    Style::new().fg(crate::theme::accent(ctx)).bg(label_bg)
                };
                line_spans.push(Span::styled(format!(" {icon} "), icon_style));
            } else {
                line_spans.push(Span::styled("   ", Style::new().bg(label_bg)));
            }

            // 标签文本
            let label_text: String = if is_selected {
                item.label.to_string()
            } else {
                item.label.clone()
            };
            line_spans.push(Span::styled(
                label_text,
                style.add_modifier(if is_selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
            ));

            // 右侧快捷键 — 用剩余宽度右对齐
            let consumed_width = line_spans
                .iter()
                .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                .sum::<usize>() as u16;

            if let Some(shortcut) = item.shortcut {
                let right_label = format!(" {shortcut} ");
                let right_width = UnicodeWidthStr::width(right_label.as_str()) as u16;
                let gap = inner.width.saturating_sub(consumed_width + right_width + 2);
                if gap > 0 {
                    line_spans.push(Span::styled(
                        " ".repeat(gap as usize),
                        Style::new().bg(label_bg),
                    ));
                }
                let sc_style = if is_selected {
                    Style::new()
                        .fg(crate::theme::selection_fg(ctx))
                        .bg(label_bg)
                } else {
                    Style::new().fg(crate::theme::surface2(ctx)).bg(label_bg)
                };
                line_spans.push(Span::styled(right_label, sc_style));
            }

            Paragraph::new(Line::from(line_spans))
                .style(Style::new().bg(label_bg))
                .render(Rect::new(inner.x, y, inner.width, 1), buf);
        }
    }

    fn area(&self, bounds: Rect) -> Rect {
        menu_area(bounds, self.origin, self.items())
    }

    fn visible_item_count(&self, bounds: Rect) -> usize {
        Block::default()
            .borders(Borders::ALL)
            .inner(self.area(bounds))
            .height as usize
    }

    fn select_previous(&mut self, visible_items: usize) {
        let len = self.items().len();
        if len == 0 {
            return;
        }
        self.selected = if self.selected == 0 {
            len - 1
        } else {
            self.selected - 1
        };
        self.ensure_selected_visible(visible_items);
    }

    fn select_next(&mut self, visible_items: usize) {
        let len = self.items().len();
        if len > 0 {
            self.selected = (self.selected + 1) % len;
            self.ensure_selected_visible(visible_items);
        }
    }

    fn activate(&mut self) -> MenuOutcome {
        let action = self
            .items()
            .get(self.selected)
            .filter(|item| !item.disabled)
            .map(|item| item.action.clone());
        match action {
            Some(SongMenuAction::OpenCustomPlaylists) => {
                self.level = MenuLevel::CustomPlaylists;
                self.selected = 0;
                self.scroll_offset = 0;
                MenuOutcome::None
            }
            Some(SongMenuAction::OpenPlaybackControls) => {
                self.level = MenuLevel::PlaybackControls;
                self.selected = 0;
                self.scroll_offset = 0;
                MenuOutcome::None
            }
            Some(action) => MenuOutcome::Action(action),
            None => MenuOutcome::Close,
        }
    }

    fn items(&self) -> &[MenuItem] {
        match self.level {
            MenuLevel::Root => &self.root_items,
            MenuLevel::CustomPlaylists => &self.custom_playlist_items,
            MenuLevel::PlaybackControls => &self.playback_control_items,
        }
    }

    fn ensure_selected_visible(&mut self, visible_items: usize) {
        let visible_items = visible_items.max(1);
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + visible_items {
            self.scroll_offset = self.selected + 1 - visible_items;
        }
    }
}

fn menu_area(bounds: Rect, origin: Position, items: &[MenuItem]) -> Rect {
    if bounds.width == 0 || bounds.height == 0 {
        return Rect::default();
    }
    let max_label_width = items
        .iter()
        .map(|item| {
            let base = UnicodeWidthStr::width(item.label.as_str());
            let icon = item
                .icon
                .map(|i| UnicodeWidthStr::width(i) + 2)
                .unwrap_or(3);
            let sc = item
                .shortcut
                .map(|s| UnicodeWidthStr::width(s) + 2)
                .unwrap_or(0);
            base + icon + sc
        })
        .max()
        .unwrap_or(16);
    let width = (max_label_width as u16 + 4).clamp(32, 56).min(bounds.width);
    let height = (items.len() as u16 + 2).min(bounds.height);
    let max_x = bounds.right().saturating_sub(width);
    let max_y = bounds.bottom().saturating_sub(height);
    Rect::new(
        origin.x.clamp(bounds.x, max_x),
        origin.y.clamp(bounds.y, max_y),
        width,
        height,
    )
}

fn build_playback_control_items(state: PlaybackMenuState) -> Vec<MenuItem> {
    use MenuGroup as G;
    use PlaybackMenuAction as A;
    use SongMenuAction as MA;

    vec![
        MenuItem::new(
            format!("播放速度: {:.2}x（切换）", state.speed),
            MA::Playback(A::CycleSpeed),
        )
        .with_icon("⏩")
        .in_group(G::Operations),
        MenuItem::new(
            format!("音频设备: {}（恢复默认）", state.audio_device),
            MA::Playback(A::UseDefaultAudioDevice),
        )
        .with_icon("🔊")
        .in_group(G::Operations),
        MenuItem::new(
            format!("ReplayGain: {}（切换）", state.replaygain_mode),
            MA::Playback(A::CycleReplayGainMode),
        )
        .with_icon("↯")
        .in_group(G::Operations),
        MenuItem::new(
            format!("ReplayGain 预放大: {:+.1} dB", state.replaygain_preamp),
            MA::Playback(A::CycleReplayGainPreamp),
        )
        .with_icon("↯")
        .in_group(G::Operations),
        MenuItem::new(
            format!(
                "ReplayGain 削波保护: {}",
                if state.replaygain_clip { "开" } else { "关" }
            ),
            MA::Playback(A::ToggleReplayGainClip),
        )
        .with_icon("✓")
        .in_group(G::Operations),
        MenuItem::new(
            format!("均衡器: {}（切换）", state.equalizer),
            MA::Playback(A::CycleEqualizer),
        )
        .with_icon("≡")
        .in_group(G::Operations),
        MenuItem::new(
            format!("声道模式: {}（切换）", state.channel_mode),
            MA::Playback(A::CycleChannelMode),
        )
        .with_icon("◐")
        .in_group(G::Operations),
        MenuItem::new(
            format!("左右平衡: {:+.2}（切换）", state.balance),
            MA::Playback(A::CycleBalance),
        )
        .with_icon("⇆")
        .in_group(G::Operations),
        MenuItem::new("立即淡入", MA::Playback(A::FadeIn))
            .with_icon("▲")
            .with_shortcut("F")
            .in_group(G::Playback),
        MenuItem::new("立即淡出", MA::Playback(A::FadeOut))
            .with_icon("▼")
            .with_shortcut("⇧F")
            .in_group(G::Playback),
        MenuItem::new("以当前进度设置 A 点", MA::Playback(A::SetAbLoopStart))
            .with_icon("A")
            .in_group(G::Playback),
        MenuItem::new("以当前进度设置 B 点", MA::Playback(A::SetAbLoopEnd))
            .with_icon("B")
            .in_group(G::Playback),
        MenuItem::new(
            format!("清除 A-B 循环: {}", state.ab_loop),
            MA::Playback(A::ClearAbLoop),
        )
        .with_icon("✕")
        .in_group(G::Playback),
    ]
}

fn item_at(
    area: Rect,
    position: Position,
    item_count: usize,
    scroll_offset: usize,
) -> Option<usize> {
    let inner = Block::default().borders(Borders::ALL).inner(area);
    if !inner.contains(position) {
        return None;
    }
    let index = scroll_offset + position.y.saturating_sub(inner.y) as usize;
    (index < item_count).then_some(index)
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use lx_core::keybinding::{KeybindingConfig, KeybindingResolver};
    use lx_core::model::song::SongInfo;
    use lx_core::model::source::SourceId;

    use super::{
        MenuLevel, MenuOutcome, PlaybackMenuAction, PlaybackMenuState, SongContextMenu,
        SongContextMenuOptions, SongMenuAction, SongMenuKind, item_at, menu_area,
    };
    use crate::pages::sort::{SortMode, SortTarget};
    use ratatui::layout::{Position, Rect};

    #[test]
    fn context_menu_is_clamped_inside_content_area() {
        let bounds = Rect::new(10, 5, 40, 12);
        let items = vec![
            super::MenuItem::new("item 1", SongMenuAction::Play),
            super::MenuItem::new("item 2", SongMenuAction::Play),
            super::MenuItem::new("item 3", SongMenuAction::Play),
            super::MenuItem::new("item 4", SongMenuAction::Play),
            super::MenuItem::new("item 5", SongMenuAction::Play),
        ];
        let area = menu_area(bounds, Position::new(48, 15), &items);

        assert!(bounds.contains(Position::new(area.x, area.y)));
        assert_eq!(area.right(), bounds.right());
        assert_eq!(area.bottom(), bounds.bottom());
    }

    #[test]
    fn menu_item_hit_test_ignores_border() {
        let area = Rect::new(10, 5, 24, 7);

        assert_eq!(item_at(area, Position::new(12, 6), 5, 0), Some(0));
        assert_eq!(item_at(area, Position::new(12, 10), 5, 0), Some(4));
        assert_eq!(item_at(area, Position::new(12, 6), 8, 3), Some(3));
        assert_eq!(item_at(area, Position::new(10, 6), 5, 0), None);
    }

    #[test]
    fn sortable_pages_append_a_sort_action() {
        let song = SongInfo::new(
            "1".to_string(),
            SourceId::Kw,
            "Song".to_string(),
            "Artist".to_string(),
        );
        let menu = SongContextMenu::new(
            Position::new(1, 1),
            vec![song],
            0,
            SongMenuKind::Standard,
            false,
            SongContextMenuOptions {
                sort: Some((SortTarget::Favorites, SortMode::Newest)),
                ..SongContextMenuOptions::default()
            },
        )
        .unwrap();

        assert_eq!(
            menu.items().last().map(|item| item.action.clone()),
            Some(SongMenuAction::CycleSort(SortTarget::Favorites))
        );
    }

    #[test]
    fn escape_closes_the_custom_playlist_submenu_in_one_step() {
        let song = SongInfo::new(
            "1".to_string(),
            SourceId::Kw,
            "Song".to_string(),
            "Artist".to_string(),
        );
        let mut menu = SongContextMenu::new(
            Position::new(1, 1),
            vec![song],
            0,
            SongMenuKind::Standard,
            false,
            SongContextMenuOptions {
                custom_playlists: vec![("custom-1".to_string(), "通勤".to_string())],
                ..SongContextMenuOptions::default()
            },
        )
        .unwrap();
        menu.selected = menu
            .root_items
            .iter()
            .position(|item| item.action == SongMenuAction::OpenCustomPlaylists)
            .unwrap();
        assert_eq!(menu.activate(), MenuOutcome::None);
        assert_eq!(menu.level, MenuLevel::CustomPlaylists);

        let resolver = KeybindingResolver::from_config(&KeybindingConfig::default());
        assert_eq!(
            menu.handle_key(
                &KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                &resolver,
                "playlists",
                Rect::new(0, 0, 40, 20),
            ),
            MenuOutcome::Close
        );
    }

    #[test]
    fn custom_playlist_menu_scrolls_with_the_actual_viewport_height() {
        let song = SongInfo::new(
            "1".to_string(),
            SourceId::Kw,
            "Song".to_string(),
            "Artist".to_string(),
        );
        let custom_playlists = (0..8)
            .map(|index| (format!("custom-{index}"), format!("歌单 {index}")))
            .collect();
        let mut menu = SongContextMenu::new(
            Position::new(0, 0),
            vec![song],
            0,
            SongMenuKind::Standard,
            false,
            SongContextMenuOptions {
                custom_playlists,
                ..SongContextMenuOptions::default()
            },
        )
        .unwrap();
        menu.level = MenuLevel::CustomPlaylists;
        let resolver = KeybindingResolver::from_config(&KeybindingConfig::default());
        let bounds = Rect::new(0, 0, 40, 5);

        for _ in 0..4 {
            assert_eq!(
                menu.handle_key(
                    &KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
                    &resolver,
                    "playlists",
                    bounds,
                ),
                MenuOutcome::None
            );
        }

        assert_eq!(menu.selected, 4);
        assert_eq!(menu.scroll_offset, 2);
    }

    #[test]
    fn playback_controls_open_as_a_submenu_and_dispatch_actions() {
        let song = SongInfo::new(
            "1".to_string(),
            SourceId::Kw,
            "Song".to_string(),
            "Artist".to_string(),
        );
        let mut menu = SongContextMenu::new(
            Position::new(0, 0),
            vec![song],
            0,
            SongMenuKind::Standard,
            false,
            SongContextMenuOptions {
                playback: Some(PlaybackMenuState {
                    speed: 1.25,
                    audio_device: "auto".to_string(),
                    replaygain_mode: "track".to_string(),
                    equalizer: "关闭".to_string(),
                    channel_mode: "stereo".to_string(),
                    ab_loop: "未设置".to_string(),
                    ..PlaybackMenuState::default()
                }),
                ..SongContextMenuOptions::default()
            },
        )
        .unwrap();
        menu.selected = menu
            .root_items
            .iter()
            .position(|item| item.action == SongMenuAction::OpenPlaybackControls)
            .unwrap();

        assert_eq!(menu.activate(), MenuOutcome::None);
        assert_eq!(menu.level, MenuLevel::PlaybackControls);
        assert_eq!(
            menu.activate(),
            MenuOutcome::Action(SongMenuAction::Playback(PlaybackMenuAction::CycleSpeed))
        );
    }

    #[test]
    fn disabled_items_cannot_be_activated() {
        let song = SongInfo::new(
            "1".to_string(),
            SourceId::Kw,
            "Song".to_string(),
            "Artist".to_string(),
        );
        let mut menu = SongContextMenu::new(
            Position::new(0, 0),
            vec![song],
            0,
            SongMenuKind::Standard,
            false,
            SongContextMenuOptions::default(),
        )
        .unwrap();
        menu.level = MenuLevel::CustomPlaylists;
        menu.selected = 0;

        assert!(menu.items()[0].disabled);
        assert_eq!(menu.activate(), MenuOutcome::Close);
    }

    #[test]
    fn menu_items_have_icons_and_shortcuts() {
        let song = SongInfo::new(
            "1".to_string(),
            SourceId::Kw,
            "Song".to_string(),
            "Artist".to_string(),
        );
        let menu = SongContextMenu::new(
            Position::new(0, 0),
            vec![song],
            0,
            SongMenuKind::Standard,
            false,
            SongContextMenuOptions::default(),
        )
        .unwrap();

        let play = menu
            .root_items
            .iter()
            .find(|i| matches!(i.action, SongMenuAction::Play))
            .unwrap();
        assert_eq!(play.icon, Some("▶"));
        assert_eq!(play.shortcut, Some("Enter"));
        assert!(!play.disabled);
    }
}
