use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use lx_core::events::AppAction;
use lx_core::model::song::SongInfo;
use ratatui::layout::{Position, Rect};
use ratatui::widgets::{Block, Borders};

use crate::context::AppContext;
use crate::pages::sort::{SortState, SortTarget, SortedListCache};

/// 排序 + 过滤后的本地歌曲视图：键盘、鼠标和渲染共用同一份下标映射，
/// 保证"光标所在行 = 双击播放的歌"在任何过滤状态下都一致。
///
/// 与收藏页的 `filtered_song_indices`、历史页的 `HistoryIndices` 同一模式：
/// query 为空时零分配恒等映射，否则只保存命中下标，不深拷贝歌曲。
pub enum LocalSongView<'a> {
    Identity(&'a [SongInfo]),
    Filtered(&'a [SongInfo], Vec<usize>),
}

impl<'a> LocalSongView<'a> {
    pub fn build(all: &'a [SongInfo], query: &str) -> Self {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Self::Identity(all);
        }
        let indices = all
            .iter()
            .enumerate()
            .filter(|(_, song)| {
                song.name.to_lowercase().contains(&query)
                    || song.singer.to_lowercase().contains(&query)
            })
            .map(|(index, _)| index)
            .collect();
        Self::Filtered(all, indices)
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Identity(songs) => songs.len(),
            Self::Filtered(_, indices) => indices.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 视图下标 → 歌曲引用（自动穿透过滤映射）。
    pub fn get(&self, index: usize) -> Option<&SongInfo> {
        match self {
            Self::Identity(songs) => songs.get(index),
            Self::Filtered(songs, indices) => songs.get(*indices.get(index)?),
        }
    }

    /// 具体化播放队列：恒等视图整表克隆，过滤视图只克隆命中歌曲。
    pub fn to_queue(&self) -> Vec<SongInfo> {
        match self {
            Self::Identity(songs) => songs.to_vec(),
            Self::Filtered(songs, indices) => indices.iter().map(|&i| songs[i].clone()).collect(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn handle_mouse(
    event: MouseEvent,
    area: Rect,
    ctx: &AppContext,
    state: &mut SortState,
    cache: &mut SortedListCache,
    filter_visible: bool,
    filter_query: &str,
    activate: bool,
) -> AppAction {
    let all_songs = sorted_local_songs(ctx, state, cache);
    let view = LocalSongView::build(all_songs, filter_query);
    let scroll_amount = ctx
        .config
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .ui
        .scroll_amount
        .max(1);
    match event.kind {
        MouseEventKind::ScrollUp => {
            state.selected = state.selected.saturating_sub(scroll_amount);
        }
        MouseEventKind::ScrollDown => {
            state.selected = (state.selected + scroll_amount).min(view.len().saturating_sub(1));
        }
        MouseEventKind::Down(MouseButton::Left) => {
            let position = Position::new(event.column, event.row);
            if let Some(index) =
                song_index_at(area, position, state.scroll, view.len(), filter_visible)
            {
                state.selected = index;
                if activate {
                    return AppAction::PlaySong {
                        songs: view.to_queue(),
                        index,
                    };
                }
            }
        }
        _ => {}
    }
    AppAction::None
}

pub fn context_song_at(
    event: MouseEvent,
    area: Rect,
    ctx: &AppContext,
    state: &mut SortState,
    cache: &mut SortedListCache,
    filter_visible: bool,
    filter_query: &str,
) -> Option<(Vec<SongInfo>, usize)> {
    let all_songs = sorted_local_songs(ctx, state, cache);
    let view = LocalSongView::build(all_songs, filter_query);
    let position = Position::new(event.column, event.row);
    let index = song_index_at(area, position, state.scroll, view.len(), filter_visible)?;
    state.selected = index;
    Some((view.to_queue(), index))
}

/// 获取排序后的本地歌曲列表，结果按曲库代次 + 排序方式缓存，
/// 渲染路径不再每帧全量 clone + 排序。
pub fn sorted_local_songs<'a>(
    ctx: &AppContext,
    state: &SortState,
    cache: &'a mut SortedListCache,
) -> &'a [SongInfo] {
    let source = ctx.source_manager.local_source();
    cache.get_or_build(
        source.library_generation(),
        state.mode,
        SortTarget::Local,
        || source.all_songs(),
    )
}

/// 屏幕行 → 视图下标。`filter_visible` 时列表上方多占一行过滤输入框，
/// 命中区域与渲染路径的行坐标保持一致。
fn song_index_at(
    area: Rect,
    position: Position,
    scroll: usize,
    len: usize,
    filter_visible: bool,
) -> Option<usize> {
    let inner = Block::default().borders(Borders::ALL).inner(area);
    let rows_top = 1 + u16::from(filter_visible);
    let visible_height = inner.height.saturating_sub(2);
    let list_area = Rect::new(
        inner.x,
        inner.y.saturating_add(rows_top),
        inner.width,
        visible_height,
    );
    if !list_area.contains(position) {
        return None;
    }

    let index = scroll + position.y.saturating_sub(list_area.y) as usize;
    (index < len).then_some(index)
}

#[cfg(test)]
mod tests {
    use super::song_index_at;
    use ratatui::layout::{Position, Rect};

    #[test]
    fn maps_visible_rows_to_scrolled_song_indices() {
        let area = Rect::new(10, 5, 80, 12);

        assert_eq!(
            song_index_at(area, Position::new(12, 7), 4, 20, false),
            Some(4)
        );
        assert_eq!(
            song_index_at(area, Position::new(12, 10), 4, 20, false),
            Some(7)
        );
    }

    #[test]
    fn ignores_header_border_and_unused_bottom_row() {
        let area = Rect::new(10, 5, 80, 12);

        assert_eq!(
            song_index_at(area, Position::new(12, 6), 0, 20, false),
            None
        );
        assert_eq!(
            song_index_at(area, Position::new(12, 15), 0, 20, false),
            None
        );
        assert_eq!(song_index_at(area, Position::new(9, 7), 0, 20, false), None);
    }

    #[test]
    fn filter_row_shifts_hit_area_down_by_one() {
        let area = Rect::new(10, 5, 80, 12);

        // 过滤条可见时数据行整体下移一行：原第 0 行位置现在点不中表头之上
        assert_eq!(song_index_at(area, Position::new(12, 7), 0, 20, true), None);
        assert_eq!(
            song_index_at(area, Position::new(12, 8), 0, 20, true),
            Some(0)
        );
        assert_eq!(
            song_index_at(area, Position::new(12, 7), 0, 20, false),
            Some(0)
        );
    }
}
