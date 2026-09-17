//! 音源会话存储：各平台登录态的统一读写。
//!
//! 原先只有哔哩哔哩实现了私有的会话结构，扫码登录与需要登录态的接口
//! 无法复用到其它平台。这里把「cookie 集合 + 账号显示信息」抽成通用
//! 存储：每个音源绑定一个 `~/.config/voicefox/session/<source>.json`，
//! 由音源自己决定哪些 cookie 参与请求。

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use lx_core::model::source::SourceId;
use serde::{Deserialize, Serialize};

/// 单个音源的会话数据。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSession {
    /// 请求时拼进 `Cookie` 头的键值对。
    #[serde(default)]
    pub cookies: BTreeMap<String, String>,
    /// 非 cookie 的会话数据（refresh_token、头像地址等）。
    #[serde(default)]
    pub extra: BTreeMap<String, String>,
    /// 登录账号显示名。
    #[serde(default)]
    pub user_name: Option<String>,
    /// 登录账号 ID。
    #[serde(default)]
    pub user_id: Option<String>,
}

impl SourceSession {
    pub fn cookie(&self, name: &str) -> Option<&str> {
        self.cookies
            .get(name)
            .map(String::as_str)
            .filter(|value| !value.is_empty())
    }

    pub fn has_cookie(&self, name: &str) -> bool {
        self.cookie(name).is_some()
    }

    /// 写入 cookie；值为空时删除该键，避免留下 `name=` 这种无效值。
    pub fn set_cookie(&mut self, name: &str, value: &str) {
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() {
            return;
        }
        if value.is_empty() {
            self.cookies.remove(name);
        } else {
            self.cookies.insert(name.to_string(), value.to_string());
        }
    }

    pub fn set_extra(&mut self, name: &str, value: &str) {
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() {
            return;
        }
        if value.is_empty() {
            self.extra.remove(name);
        } else {
            self.extra.insert(name.to_string(), value.to_string());
        }
    }

    pub fn extra(&self, name: &str) -> Option<&str> {
        self.extra
            .get(name)
            .map(String::as_str)
            .filter(|value| !value.is_empty())
    }

    /// 按给定顺序拼接指定 cookie，未配置的键会被跳过。
    ///
    /// 顺序固定可以让请求头保持稳定，也便于测试断言。
    pub fn cookie_header_of(&self, names: &[&str]) -> Option<String> {
        let pairs = names
            .iter()
            .filter_map(|name| self.cookie(name).map(|value| format!("{name}={value}")))
            .collect::<Vec<_>>();
        (!pairs.is_empty()).then(|| pairs.join("; "))
    }

    /// 解析 `a=1; b=2` 形式的 cookie 串。
    pub fn apply_cookie_header(&mut self, raw: &str) {
        for pair in raw.split(';') {
            let Some((name, value)) = pair.split_once('=') else {
                continue;
            };
            self.set_cookie(name.trim(), value.trim());
        }
    }

    /// 解析一条 `Set-Cookie` 头，忽略 `Path` / `Expires` 等属性。
    pub fn apply_set_cookie(&mut self, raw: &str) {
        let Some(first) = raw.split(';').next() else {
            return;
        };
        let Some((name, value)) = first.split_once('=') else {
            return;
        };
        self.set_cookie(name.trim(), value.trim());
    }

    pub fn is_empty(&self) -> bool {
        self.cookies.is_empty()
            && self.extra.is_empty()
            && self.user_name.is_none()
            && self.user_id.is_none()
    }
}

/// 按音源绑定的会话存储句柄。
///
/// 读走内存快照、写回文件，读写都加锁；文件写入复用原子替换，避免进程
/// 中途退出留下半个 JSON。
pub struct SessionStore {
    source: SourceId,
    path: PathBuf,
    session: RwLock<SourceSession>,
}

impl SessionStore {
    /// 加载指定音源的会话，必要时把旧的哔哩哔哩会话文件迁移过来。
    pub fn load(source: SourceId) -> Self {
        let store = Self::at_path(source, session_path(source));
        store.migrate_legacy_bili();
        store
    }

    /// 指定存储路径的会话句柄，供测试与自定义数据目录使用。
    ///
    /// 只读给定文件，不触发任何迁移：迁移依赖用户配置目录，只有
    /// [`SessionStore::load`] 会执行。
    pub fn at_path(source: SourceId, path: PathBuf) -> Self {
        Self {
            source,
            session: RwLock::new(load_file(&path)),
            path,
        }
    }

    pub fn source(&self) -> SourceId {
        self.source
    }

    pub fn snapshot(&self) -> SourceSession {
        self.session
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// 修改会话并落盘。
    pub fn update(&self, edit: impl FnOnce(&mut SourceSession)) -> Result<(), String> {
        let snapshot = {
            let mut session = self
                .session
                .write()
                .unwrap_or_else(|error| error.into_inner());
            edit(&mut session);
            session.clone()
        };
        save_file(&self.path, &snapshot)
    }

    /// 整体替换会话并落盘，用于扫码登录成功后的回写。
    pub fn replace(&self, session: SourceSession) -> Result<(), String> {
        {
            let mut guard = self
                .session
                .write()
                .unwrap_or_else(|error| error.into_inner());
            *guard = session.clone();
        }
        save_file(&self.path, &session)
    }

    /// 清空会话并删除文件；文件不存在视为成功。
    pub fn clear(&self) -> Result<(), String> {
        {
            let mut guard = self
                .session
                .write()
                .unwrap_or_else(|error| error.into_inner());
            *guard = SourceSession::default();
        }
        remove_file(&self.path)
    }

    /// 把旧的哔哩哔哩会话文件迁移到通用存储，只在首次加载时执行。
    fn migrate_legacy_bili(&self) {
        if self.source != SourceId::Bili || !self.snapshot().is_empty() {
            return;
        }
        let Some(session) = migrate_legacy_file(&legacy_bili_path(), &self.path) else {
            return;
        };
        let mut guard = self
            .session
            .write()
            .unwrap_or_else(|error| error.into_inner());
        *guard = session;
    }
}

/// 会话文件路径：`<config>/voicefox/session/<source>.json`。
pub fn session_path(source: SourceId) -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("voicefox")
        .join("session")
        .join(format!("{}.json", source.as_str()))
}

fn legacy_bili_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("voicefox")
        .join("bilibili.json")
}

/// 旧的哔哩哔哩会话结构：字段是具名的，不是 cookie 键值对。
#[derive(Default, Deserialize)]
struct LegacyBiliSession {
    sessdata: Option<String>,
    bili_jct: Option<String>,
    buvid3: Option<String>,
    buvid4: Option<String>,
    dede_user_id: Option<String>,
    access_key: Option<String>,
    refresh_token: Option<String>,
    user_name: Option<String>,
    user_id: Option<String>,
    avatar: Option<String>,
}

/// 把旧会话转换成通用结构；登录 cookie 之外的数据进 `extra`。
fn legacy_bili_session(legacy: LegacyBiliSession) -> SourceSession {
    let mut session = SourceSession::default();
    for (name, value) in [
        ("SESSDATA", legacy.sessdata.as_deref()),
        ("bili_jct", legacy.bili_jct.as_deref()),
        ("buvid3", legacy.buvid3.as_deref()),
        ("buvid4", legacy.buvid4.as_deref()),
        ("DedeUserID", legacy.dede_user_id.as_deref()),
    ] {
        if let Some(value) = value {
            session.set_cookie(name, value);
        }
    }
    for (name, value) in [
        ("access_key", legacy.access_key.as_deref()),
        ("refresh_token", legacy.refresh_token.as_deref()),
        ("avatar", legacy.avatar.as_deref()),
    ] {
        if let Some(value) = value {
            session.set_extra(name, value);
        }
    }
    session.user_name = legacy.user_name.filter(|value| !value.is_empty());
    session.user_id = legacy.user_id.filter(|value| !value.is_empty());
    session
}

/// 迁移旧的哔哩哔哩会话到通用存储。
///
/// 迁移成功后把旧文件改名为 `.migrated`：否则用户退出登录（删除新文件）
/// 后，下次启动会再次从旧文件恢复出一份已登录会话。改名而不是删除，
/// 保留用户手动回滚的余地。
fn migrate_legacy_file(legacy_path: &Path, new_path: &Path) -> Option<SourceSession> {
    let raw = std::fs::read_to_string(legacy_path).ok()?;
    let legacy: LegacyBiliSession = serde_json::from_str(&raw).ok()?;
    let session = legacy_bili_session(legacy);
    if session.is_empty() {
        return None;
    }
    // 落盘失败（例如配置目录只读）时仍然返回会话：本次运行保持登录状态，
    // 下次启动再重试迁移。
    if save_file(new_path, &session).is_ok() {
        let mut migrated_path = legacy_path.as_os_str().to_os_string();
        migrated_path.push(".migrated");
        let _ = std::fs::rename(legacy_path, migrated_path);
    }
    Some(session)
}

fn load_file(path: &Path) -> SourceSession {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or_default()
}

fn save_file(path: &Path, session: &SourceSession) -> Result<(), String> {
    let content = serde_json::to_string_pretty(session).map_err(|error| error.to_string())?;
    save_file_bytes(path, content.as_bytes())
}

/// 原子写入：先写同目录临时文件并 fsync，再 rename 覆盖目标。
///
/// 会话文件包含登录 cookie，权限固定 0600。
fn save_file_bytes(path: &Path, content: &[u8]) -> Result<(), String> {
    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }

    let suffix = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_path = {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".tmp-{}-{suffix}", std::process::id()));
        path.with_file_name(name)
    };
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let result = (|| {
        let mut file = options
            .open(&temp_path)
            .map_err(|error| error.to_string())?;
        file.write_all(content).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        drop(file);

        #[cfg(unix)]
        {
            std::fs::rename(&temp_path, path).map_err(|error| error.to_string())?;
        }
        #[cfg(windows)]
        {
            // Windows 不支持覆盖式 rename：先把旧文件挪走再改名，失败时恢复。
            let old_path = temp_path.with_extension("old");
            if path.exists() {
                std::fs::rename(path, &old_path).map_err(|error| error.to_string())?;
            }
            match std::fs::rename(&temp_path, path) {
                Ok(()) => {
                    let _ = std::fs::remove_file(&old_path);
                }
                Err(error) => {
                    let _ = std::fs::rename(&old_path, path);
                    return Err(error.to_string());
                }
            }
        }
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

fn remove_file(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("删除会话文件失败: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        let counter = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or_default();
        std::env::temp_dir().join(format!(
            "voicefox-session-{tag}-{}-{counter}.json",
            std::process::id()
        ))
    }

    #[test]
    fn cookie_header_keeps_requested_order_and_skips_missing() {
        let mut session = SourceSession::default();
        session.set_cookie("bili_jct", "csrf");
        session.set_cookie("SESSDATA", "session");
        assert_eq!(
            session
                .cookie_header_of(&["SESSDATA", "bili_jct", "DedeUserID"])
                .as_deref(),
            Some("SESSDATA=session; bili_jct=csrf")
        );
    }

    #[test]
    fn applying_set_cookie_ignores_attributes() {
        let mut session = SourceSession::default();
        session.apply_set_cookie("SESSDATA=abc%3D; Path=/; Domain=.bilibili.com; HttpOnly");
        assert_eq!(session.cookie("SESSDATA"), Some("abc%3D"));
        assert_eq!(session.cookies.len(), 1);
    }

    #[test]
    fn empty_cookie_value_removes_the_key() {
        let mut session = SourceSession::default();
        session.set_cookie("SESSDATA", "value");
        session.set_cookie("SESSDATA", "");
        assert!(!session.has_cookie("SESSDATA"));
    }

    #[test]
    fn store_round_trips_through_the_file() {
        let path = temp_path("round-trip");
        let store = SessionStore::at_path(SourceId::Wy, path.clone());
        store
            .update(|session| {
                session.set_cookie("MUSIC_U", "token");
                session.user_name = Some("听歌的人".to_string());
            })
            .unwrap();

        let reloaded = SessionStore::at_path(SourceId::Wy, path.clone());
        let snapshot = reloaded.snapshot();
        assert_eq!(snapshot.cookie("MUSIC_U"), Some("token"));
        assert_eq!(snapshot.user_name.as_deref(), Some("听歌的人"));

        reloaded.clear().unwrap();
        assert!(
            SessionStore::at_path(SourceId::Wy, path.clone())
                .snapshot()
                .is_empty()
        );
        let _ = std::fs::remove_file(path);
    }

    #[cfg(unix)]
    #[test]
    fn session_file_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let path = temp_path("private");
        let mut session = SourceSession::default();
        session.set_cookie("SESSDATA", "secret");
        save_file(&path, &session).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn legacy_bili_schema_maps_to_cookies() {
        let raw = r#"{
            "sessdata": "session",
            "bili_jct": "csrf",
            "buvid3": "buvid",
            "refresh_token": "refresh",
            "user_name": "哔哩哔哩用户",
            "user_id": "12345",
            "avatar": "https://example.com/a.png"
        }"#;
        let legacy: LegacyBiliSession = serde_json::from_str(raw).unwrap();
        let session = legacy_bili_session(legacy);

        assert_eq!(session.cookie("SESSDATA"), Some("session"));
        assert_eq!(session.cookie("buvid3"), Some("buvid"));
        assert_eq!(session.extra("refresh_token"), Some("refresh"));
        assert_eq!(session.user_name.as_deref(), Some("哔哩哔哩用户"));
    }

    #[test]
    fn legacy_file_is_migrated_once_and_renamed() {
        let legacy_path = temp_path("legacy").with_extension("json");
        let new_path = temp_path("migrated").with_extension("json");
        std::fs::write(&legacy_path, r#"{"sessdata":"session","user_name":"用户"}"#).unwrap();

        let session = migrate_legacy_file(&legacy_path, &new_path).unwrap();
        assert_eq!(session.cookie("SESSDATA"), Some("session"));
        // 旧文件已改名，不会在用户退出登录后把会话「复活」。
        let mut renamed = legacy_path.as_os_str().to_os_string();
        renamed.push(".migrated");
        assert!(!legacy_path.exists());
        assert!(PathBuf::from(renamed).exists());

        // 迁移结果已经落盘，退出登录删除新文件后不会再从旧文件恢复。
        let store = SessionStore::at_path(SourceId::Bili, new_path.clone());
        assert_eq!(store.snapshot().cookie("SESSDATA"), Some("session"));
        store.clear().unwrap();
        assert!(migrate_legacy_file(&legacy_path, &new_path).is_none());

        let _ = std::fs::remove_file(new_path);
    }
}
