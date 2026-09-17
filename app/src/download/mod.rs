//! 音乐下载：多线程分片下载 + 标签/封面/歌词写入。
//!
//! 设计参考 MusicBot-Go 的 `bot/download` 与 `bot/id3`：
//! - `engine`：与音源无关的下载引擎（Range 探测、分片、重试、大小校验）；
//! - `manager`：下载队列，负责解析播放地址、换源、落盘与通知；
//! - `naming`：文件名模板、非法字符清理、扩展名推断；
//! - `tags`：写标签、嵌封面、导出歌词。

pub mod engine;
pub mod manager;
pub mod naming;
pub mod records;
pub mod tags;
#[cfg(test)]
pub mod test_support;
pub mod webdav;

pub use manager::{DownloadManager, DownloadState, DownloadTaskView, DownloadTrigger};
