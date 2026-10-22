//! 单实例锁。
//!
//! 收藏、历史与播放会话均由进程内存整体回写到数据目录，两个实例同时运行时
//! 后退出的一方会覆盖另一方的改动。启动 TUI 或执行导入前须先取得数据目录下
//! 锁文件的排他锁。锁随文件句柄一同释放，进程异常退出时由操作系统回收，
//! 不会残留失效的锁。

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

use anyhow::Context;

const LOCK_FILE: &str = "voicefox.lock";

/// 持有期间独占数据目录，析构时释放。
pub struct InstanceLock {
    _file: File,
}

pub fn acquire(data_dir: &Path) -> anyhow::Result<InstanceLock> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("创建数据目录失败: {}", data_dir.display()))?;
    let path = data_dir.join(LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("打开锁文件失败: {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            anyhow::bail!(
                "voicefox 已在运行，数据目录 {} 正被另一个实例占用",
                data_dir.display()
            );
        }
        // 部分网络文件系统不支持文件锁，此时放弃互斥而非拒绝启动。
        Err(TryLockError::Error(error)) => {
            tracing::warn!("无法锁定 {}，跳过单实例检查: {error}", path.display());
        }
    }
    Ok(InstanceLock { _file: file })
}
