//! lx-player: 播放引擎
//!
//! 当前实现：mpv 子进程 + JSON IPC
//! 对标 go-musicfox internal/player/ + lx-music renderer/plugins/player/

pub mod engine;
pub mod mpv_ipc;
