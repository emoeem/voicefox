//! 共享 UI 组件
//!
//! - header: SourceBar 顶栏 1 行（tab + 歌名）
//! - player_controls: PlayerBar 底部 3 行（歌名 / 进度 / 控制）
//! - song_table: 歌曲列表
//! - notification: 错误/提示 toast

pub mod chrome;
pub mod context_menu;
pub mod header;
pub mod list_filter;
pub mod lyric;
pub mod notification;
pub mod player_controls;
pub mod scroll;
pub mod song_table;
pub mod text;
