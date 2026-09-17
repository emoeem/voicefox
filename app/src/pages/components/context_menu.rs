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

#[derive(Debug, Clone)]
struct MenuItem {
    label: String,
    action: SongMenuAction,
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
        let mut root_items = vec![MenuItem {
            label: "播放".to_string(),
            action: SongMenuAction::Play,
        }];
        // 本地文件已经在磁盘上，不提供下载入口。
        if kind != SongMenuKind::Local {
            root_items.push(MenuItem {
                label: "下载歌曲".to_string(),
                action: SongMenuAction::Download,
            });
        }
        if kind != SongMenuKind::Queue {
            root_items.extend([
                MenuItem {
                    label: "设为下一首".to_string(),
                    action: SongMenuAction::PlayNext,
                },
                MenuItem {
                    label: "加入队尾".to_string(),
                    action: SongMenuAction::AddToQueue,
                },
            ]);
        }
        root_items.push(MenuItem {
            label: "加入自建歌单...".to_string(),
            action: SongMenuAction::OpenCustomPlaylists,
        });
        if let Some(playlist_id) = current_custom_playlist {
            root_items.push(MenuItem {
                label: "从当前歌单移除".to_string(),
                action: SongMenuAction::RemoveFromCustomPlaylist(playlist_id),
            });
        }
        root_items.push(MenuItem {
            label: if is_favorite {
                "取消收藏".to_string()
            } else {
                "收藏歌曲".to_string()
            },
            action: SongMenuAction::ToggleFavorite,
        });
        let song = &songs[index];
        if !song.singer.trim().is_empty() {
            root_items.push(MenuItem {
                label: "查看歌手".to_string(),
                action: SongMenuAction::ViewArtist,
            });
        }
        if !song.album_name.trim().is_empty() {
            root_items.push(MenuItem {
                label: "查看专辑".to_string(),
                action: SongMenuAction::ViewAlbum,
            });
        }
        if playback.is_some() {
            root_items.push(MenuItem {
                label: "播放控制...".to_string(),
                action: SongMenuAction::OpenPlaybackControls,
            });
        }
        if let Some((target, mode)) = sort {
            root_items.push(MenuItem {
                label: format!("排序：{}（切换）", mode.label(target)),
                action: SongMenuAction::CycleSort(target),
            });
        }
        match kind {
            SongMenuKind::Queue => root_items.push(MenuItem {
                label: "从队列移除".to_string(),
                action: SongMenuAction::RemoveFromQueue,
            }),
            SongMenuKind::History => root_items.extend([
                MenuItem {
                    label: "删除这条历史".to_string(),
                    action: SongMenuAction::RemoveFromHistory,
                },
                MenuItem {
                    label: "清空播放历史".to_string(),
                    action: SongMenuAction::ClearHistory,
                },
            ]),
            SongMenuKind::Local => root_items.push(MenuItem {
                label: "删除本地文件".to_string(),
                action: SongMenuAction::DeleteLocal,
            }),
            SongMenuKind::Standard => {}
        }
        let custom_playlist_items = if custom_playlists.is_empty() {
            vec![MenuItem {
                label: "暂无自建歌单，请先创建".to_string(),
                action: SongMenuAction::NoCustomPlaylists,
            }]
        } else {
            custom_playlists
                .into_iter()
                .map(|(id, name)| MenuItem {
                    label: name,
                    action: SongMenuAction::AddToCustomPlaylist(id),
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
                // 在子菜单内右键返回上一级，菜单外右键才关闭整个菜单。
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
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(crate::theme::accent(ctx)))
            .style(
                Style::new()
                    .bg(crate::theme::surface0(ctx))
                    .fg(crate::theme::text(ctx)),
            )
            .title(match self.level {
                MenuLevel::Root => " 歌曲操作 ",
                MenuLevel::CustomPlaylists => " 选择自建歌单 ",
                MenuLevel::PlaybackControls => " 播放控制 ",
            });
        let inner = block.inner(area);
        block.render(area, buf);

        for (row, (index, item)) in self
            .items()
            .iter()
            .enumerate()
            .skip(self.scroll_offset)
            .take(inner.height as usize)
            .enumerate()
        {
            let style = if index == self.selected {
                Style::new()
                    .bg(crate::theme::accent(ctx))
                    .fg(crate::theme::selection_fg(ctx))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new()
                    .bg(crate::theme::surface0(ctx))
                    .fg(crate::theme::text(ctx))
            };
            Paragraph::new(Line::from(Span::styled(format!(" {}", item.label), style)))
                .style(style)
                .render(
                    Rect::new(inner.x, inner.y + row as u16, inner.width, 1),
                    buf,
                );
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
    // 根据当前层级最长标签自适应宽度，避免 ReplayGain / 中文歌单名被固定宽度截断。
    // 上限保持紧凑，窄终端仍由 bounds 做最终裁剪。
    let max_label_width = items
        .iter()
        .map(|item| UnicodeWidthStr::width(item.label.as_str()))
        .max()
        .unwrap_or(8);
    let width = (max_label_width as u16 + 4).clamp(24, 48).min(bounds.width);
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
    use PlaybackMenuAction as Action;

    vec![
        playback_item(
            format!("播放速度: {:.2}x（切换）", state.speed),
            Action::CycleSpeed,
        ),
        playback_item(
            format!("音频设备: {}（恢复默认）", state.audio_device),
            Action::UseDefaultAudioDevice,
        ),
        playback_item(
            format!("ReplayGain: {}（切换）", state.replaygain_mode),
            Action::CycleReplayGainMode,
        ),
        playback_item(
            format!("ReplayGain 预放大: {:+.1} dB", state.replaygain_preamp),
            Action::CycleReplayGainPreamp,
        ),
        playback_item(
            format!(
                "ReplayGain 削波保护: {}",
                if state.replaygain_clip { "开" } else { "关" }
            ),
            Action::ToggleReplayGainClip,
        ),
        playback_item(
            format!("均衡器: {}（切换）", state.equalizer),
            Action::CycleEqualizer,
        ),
        playback_item(
            format!("声道模式: {}（切换）", state.channel_mode),
            Action::CycleChannelMode,
        ),
        playback_item(
            format!("左右平衡: {:+.2}（切换）", state.balance),
            Action::CycleBalance,
        ),
        playback_item("立即淡入".to_string(), Action::FadeIn),
        playback_item("立即淡出".to_string(), Action::FadeOut),
        playback_item("以当前进度设置 A 点".to_string(), Action::SetAbLoopStart),
        playback_item("以当前进度设置 B 点".to_string(), Action::SetAbLoopEnd),
        playback_item(
            format!("清除 A-B 循环: {}", state.ab_loop),
            Action::ClearAbLoop,
        ),
    ]
}

fn playback_item(label: String, action: PlaybackMenuAction) -> MenuItem {
    MenuItem {
        label,
        action: SongMenuAction::Playback(action),
    }
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
        let items = (0..5)
            .map(|i| super::MenuItem {
                label: format!("item {i}"),
                action: SongMenuAction::Play,
            })
            .collect::<Vec<_>>();
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
}
