use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use lx_core::keybinding::migrate_legacy_settings_bindings;
use lx_core::model::config::{
    CURRENT_CONFIG_VERSION, Config, ThemeConfig, sanitize_status_bar_items,
};
use lx_core::model::source::SourceId;

const VERSION_1_DEFAULT_SOURCES: &[SourceId] = &[
    SourceId::Kw,
    SourceId::Kg,
    SourceId::Tx,
    SourceId::Wy,
    SourceId::Mg,
];

/// 接入千千音乐之前的默认音源组合（版本 2 迁移后追加了 B 站）。
const VERSION_10_DEFAULT_SOURCES: &[SourceId] = &[
    SourceId::Kw,
    SourceId::Kg,
    SourceId::Tx,
    SourceId::Wy,
    SourceId::Mg,
    SourceId::Bili,
];

/// 接入汽水音乐之前的默认音源组合（版本 15 未新增默认音源）。
const VERSION_15_DEFAULT_SOURCES: &[SourceId] = &[
    SourceId::Kw,
    SourceId::Kg,
    SourceId::Tx,
    SourceId::Wy,
    SourceId::Mg,
    SourceId::Bili,
    SourceId::Qianqian,
    SourceId::Joox,
    SourceId::Fivesing,
    SourceId::Jamendo,
];

/// 接入 Jamendo 之前的默认音源组合（版本 13 迁移后追加了 5sing）。
const VERSION_13_DEFAULT_SOURCES: &[SourceId] = &[
    SourceId::Kw,
    SourceId::Kg,
    SourceId::Tx,
    SourceId::Wy,
    SourceId::Mg,
    SourceId::Bili,
    SourceId::Qianqian,
    SourceId::Joox,
    SourceId::Fivesing,
];

/// 接入 5sing 之前的默认音源组合（版本 12 迁移后追加了 JOOX）。
const VERSION_12_DEFAULT_SOURCES: &[SourceId] = &[
    SourceId::Kw,
    SourceId::Kg,
    SourceId::Tx,
    SourceId::Wy,
    SourceId::Mg,
    SourceId::Bili,
    SourceId::Qianqian,
    SourceId::Joox,
];

/// 接入 JOOX 之前的默认音源组合（版本 11 迁移后追加了千千音乐）。
const VERSION_11_DEFAULT_SOURCES: &[SourceId] = &[
    SourceId::Kw,
    SourceId::Kg,
    SourceId::Tx,
    SourceId::Wy,
    SourceId::Mg,
    SourceId::Bili,
    SourceId::Qianqian,
];

/// 加载配置：优先读用户配置文件，否则用默认值
pub fn load(custom_path: &str) -> anyhow::Result<(Config, PathBuf)> {
    let config_path = resolve_config_path(custom_path);

    match fs::read_to_string(&config_path) {
        Ok(content) => {
            let mut config: Config = toml::from_str(&content)?;
            if migrate_legacy_config(&mut config) {
                save(&config, &config_path)?;
            }
            Ok((config, config_path))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // 配置文件不存在，写入默认配置
            let config = Config::default();
            let toml_str = toml::to_string_pretty(&config)?;
            if let Some(parent) = config_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&config_path, toml_str)?;
            Ok((config, config_path))
        }
        Err(error) => Err(error.into()),
    }
}

fn migrate_legacy_config(config: &mut Config) -> bool {
    // 主题迁移只对未记录过版本的配置执行：用户若刻意把四色配成旧默认值，
    // 反复执行会把整个主题静默替换成 Mocha。
    let mut changed = if config.version < 1 {
        migrate_legacy_theme(config)
    } else {
        false
    };
    changed |= sanitize_status_bar_items(&mut config.ui.status_bar_items);
    if config.version < 1 {
        if config.source.enabled == [SourceId::Kw] {
            config.source.enabled = VERSION_1_DEFAULT_SOURCES.to_vec();
        }
        config.version = 1;
        changed = true;
    }
    if config.version < 2 {
        if same_sources(&config.source.enabled, VERSION_1_DEFAULT_SOURCES) {
            config.source.enabled.push(SourceId::Bili);
        }
        config.version = 2;
        changed = true;
    }
    if config.version < 3 {
        // 旧版本曾把 local_music.enabled 的默认值保存为 false，导致已经
        // 配置过目录的用户升级后不会自动扫描。只在版本迁移时恢复一次；
        // 版本 3 之后用户主动关闭仍会被保留。
        if !config.local_music.paths.is_empty() && !config.local_music.enabled {
            config.local_music.enabled = true;
        }
        config.version = 3;
        changed = true;
    }
    if config.version < 4 {
        // 通知原先位于 [ui]，迁移到独立的 [notification]，保留用户已有
        // 的开关和停留时间；缺失字段继续使用新的默认值。
        if let Some(enabled) = config.ui.show_notifications {
            config.notification.in_app = enabled;
        }
        if let Some(timeout) = config.ui.notification_timeout {
            config.notification.in_app_timeout = timeout;
        }
        config.ui.show_notifications = None;
        config.ui.notification_timeout = None;
        config.version = 4;
        changed = true;
    }
    if config.version < 5 {
        config.player.history_limit = config.player.history_limit.max(1);
        config.version = 5;
        changed = true;
    }
    if config.version < 6 {
        config.version = 6;
        changed = true;
    }
    if config.version < 7 {
        // 新增播放器控制项均带有 serde(default)，旧配置只需提升版本即可。
        config.version = 7;
        changed = true;
    }
    if config.version < 8 {
        // Fade/EQ/ReplayGain clip fields also have serde defaults; bump the
        // version after deserialization so the next save records the schema.
        config.version = 8;
        changed = true;
    }
    if config.version < 9 {
        migrate_legacy_settings_bindings(&mut config.keybindings);
        config.version = 9;
        changed = true;
    }
    if config.version < 10 {
        // 新增 [download] 下载配置，字段均有 serde 默认值，旧配置提升版本即可。
        config.version = 10;
        changed = true;
    }
    if config.version < 11 {
        // 千千音乐接入后成为内置音源：只有仍在使用「上一版默认音源组合」
        // 的用户才自动补上，用户自己挑选过的列表保持不动。
        if same_sources(&config.source.enabled, VERSION_10_DEFAULT_SOURCES) {
            config.source.enabled.push(SourceId::Qianqian);
        }
        config.version = 11;
        changed = true;
    }
    if config.version < 12 {
        // JOOX 同理：只补自己没动过音源列表的用户。
        if same_sources(&config.source.enabled, VERSION_11_DEFAULT_SOURCES) {
            config.source.enabled.push(SourceId::Joox);
        }
        config.version = 12;
        changed = true;
    }
    if config.version < 13 {
        // 5sing 同理。
        if same_sources(&config.source.enabled, VERSION_12_DEFAULT_SOURCES) {
            config.source.enabled.push(SourceId::Fivesing);
        }
        config.version = 13;
        changed = true;
    }
    if config.version < 14 {
        // Jamendo 同理。
        if same_sources(&config.source.enabled, VERSION_13_DEFAULT_SOURCES) {
            config.source.enabled.push(SourceId::Jamendo);
        }
        config.version = 14;
        changed = true;
    }
    if config.version < 15 {
        // 新增 Apple Music 音源，但它只能播放 30 秒试听，默认不启用：
        // 用户需要在设置页显式打开，避免搜索结果里出现放不完整的歌。
        config.version = 15;
        changed = true;
    }
    if config.version < 16 {
        // 汽水音乐：同样只补没动过音源列表的用户。
        if same_sources(&config.source.enabled, VERSION_15_DEFAULT_SOURCES) {
            config.source.enabled.push(SourceId::Soda);
        }
        config.version = 16;
        changed = true;
    }
    if config.version > CURRENT_CONFIG_VERSION {
        // 用户可能带着更高版本的配置降级运行：只警告不 panic（debug_assert
        // 会在开发构建直接崩溃），字段由 serde default 兜底。
        tracing::warn!(
            "配置文件版本 {} 高于当前支持的 {}，部分新字段将被忽略",
            config.version,
            CURRENT_CONFIG_VERSION
        );
    }
    changed
}

fn same_sources(left: &[SourceId], right: &[SourceId]) -> bool {
    left.len() == right.len() && right.iter().all(|source| left.contains(source))
}

fn migrate_legacy_theme(config: &mut Config) -> bool {
    let theme = &config.theme;
    let uses_original_defaults = theme.accent.eq_ignore_ascii_case("cyan")
        && theme.text.eq_ignore_ascii_case("white")
        && theme.muted.eq_ignore_ascii_case("dark_gray")
        && theme.border.eq_ignore_ascii_case("cyan");
    if uses_original_defaults {
        config.theme = ThemeConfig::default();
    }
    uses_original_defaults
}

/// 获取配置文件路径: ~/.config/voicefox/config.toml
pub fn config_path() -> PathBuf {
    let dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("voicefox");
    dir.join("config.toml")
}

fn resolve_config_path(custom_path: &str) -> PathBuf {
    let custom_path = custom_path.trim();
    if custom_path.is_empty() {
        return config_path();
    }
    if let Some(relative) = custom_path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(relative);
    }
    PathBuf::from(custom_path)
}

/// 保存配置到文件
pub fn save(config: &Config, path: &std::path::Path) -> anyhow::Result<()> {
    let toml_str = toml::to_string_pretty(config)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    save_atomic(path, toml_str.as_bytes())?;
    Ok(())
}

/// 原子写入配置文件：先写同目录临时文件再 rename。
///
/// 音量、播放模式等操作会高频触发配置保存，直接覆盖可能在进程崩溃或断电时
/// 留下半个配置文件；临时文件 + rename 保证目标文件要么完整要么保持旧内容。
fn save_atomic(path: &std::path::Path, content: &[u8]) -> std::io::Result<()> {
    let temp_path = path.with_file_name(format!(
        "{}.tmp-{}-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        CONFIG_TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> std::io::Result<()> {
        fs::write(&temp_path, content)?;
        #[cfg(windows)]
        {
            // Windows 不支持覆盖式 rename：先把旧文件挪走（而非删除），
            // 换名失败时挪回来，保证任何时刻配置文件都存在。
            let old_path = path.with_extension(format!(
                "{}.old-{}",
                path.extension().unwrap_or_default().to_string_lossy(),
                std::process::id()
            ));
            if path.exists() {
                fs::rename(path, &old_path)?;
            }
            match fs::rename(&temp_path, path) {
                Ok(()) => {
                    let _ = fs::remove_file(&old_path);
                    Ok(())
                }
                Err(error) => {
                    let _ = fs::rename(&old_path, path);
                    Err(error)
                }
            }
        }
        #[cfg(not(windows))]
        fs::rename(&temp_path, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

static CONFIG_TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
mod tests {
    use super::{load, migrate_legacy_config};
    use lx_core::keybinding::Action;
    use lx_core::model::config::{CURRENT_CONFIG_VERSION, Config, StatusBarItem};
    use lx_core::model::source::SourceId;

    #[test]
    fn migrates_the_original_default_theme_to_mocha() {
        let mut config = Config {
            version: 0,
            ..Config::default()
        };
        config.theme.accent = "cyan".into();
        config.theme.text = "white".into();
        config.theme.muted = "dark_gray".into();
        config.theme.border = "cyan".into();

        migrate_legacy_config(&mut config);

        assert_eq!(config.theme.base, "#1e1e2e");
        assert_eq!(config.theme.accent, "#cba6f7");
    }

    #[test]
    fn expands_the_original_kw_only_source_default_once() {
        let mut config = Config {
            version: 0,
            ..Config::default()
        };
        config.source.enabled = vec![SourceId::Kw];

        assert!(migrate_legacy_config(&mut config));
        assert_eq!(config.version, CURRENT_CONFIG_VERSION);
        assert_eq!(config.source.enabled, SourceId::default_enabled());

        config.source.enabled = vec![SourceId::Kw];
        assert!(!migrate_legacy_config(&mut config));
        assert_eq!(config.source.enabled, vec![SourceId::Kw]);
    }

    #[test]
    fn preserves_a_custom_source_selection_during_version_two_migration() {
        let mut config = Config {
            version: 1,
            ..Config::default()
        };
        config.source.enabled = vec![SourceId::Kg, SourceId::Wy];

        assert!(migrate_legacy_config(&mut config));
        assert_eq!(config.version, CURRENT_CONFIG_VERSION);
        assert_eq!(config.source.enabled, vec![SourceId::Kg, SourceId::Wy]);
    }

    #[test]
    fn reenables_existing_local_music_paths_during_version_three_migration() {
        let mut config = Config {
            version: 2,
            ..Config::default()
        };
        config.local_music.paths = vec!["/music".to_string()];
        config.local_music.enabled = false;

        assert!(migrate_legacy_config(&mut config));
        assert_eq!(config.version, CURRENT_CONFIG_VERSION);
        assert!(config.local_music.enabled);
    }

    #[test]
    fn preserves_local_music_disable_after_version_three_migration() {
        let mut config = Config::default();
        config.local_music.paths = vec!["/music".to_string()];
        config.local_music.enabled = false;

        assert!(!migrate_legacy_config(&mut config));
        assert!(!config.local_music.enabled);
    }

    #[test]
    fn migrates_legacy_ui_notification_options() {
        let mut config = Config {
            version: 3,
            ..Config::default()
        };
        config.ui.show_notifications = Some(false);
        config.ui.notification_timeout = Some(9);

        assert!(migrate_legacy_config(&mut config));
        assert_eq!(config.version, CURRENT_CONFIG_VERSION);
        assert!(!config.notification.in_app);
        assert_eq!(config.notification.in_app_timeout, 9);
        assert_eq!(config.ui.show_notifications, None);
        assert_eq!(config.ui.notification_timeout, None);
    }

    #[test]
    fn migrates_history_limit_to_a_positive_value() {
        let mut config = Config {
            version: 4,
            ..Config::default()
        };
        config.player.history_limit = 0;

        assert!(migrate_legacy_config(&mut config));
        assert_eq!(config.version, CURRENT_CONFIG_VERSION);
        assert_eq!(config.player.history_limit, 1);
    }

    #[test]
    fn migrates_version_eight_settings_shortcuts_away_from_tab_numbers() {
        let mut config = Config {
            version: 8,
            ..Config::default()
        };
        let settings = config.keybindings.pages.get_mut("settings").unwrap();
        settings.insert(Action::SettingsCyclePlaybackSpeed, "1".to_string());
        settings.insert(Action::SettingsCycleBalance, "6".to_string());

        assert!(migrate_legacy_config(&mut config));
        assert_eq!(config.version, CURRENT_CONFIG_VERSION);
        let settings = config.keybindings.pages.get("settings").unwrap();
        assert_eq!(
            settings
                .get(&Action::SettingsCyclePlaybackSpeed)
                .map(String::as_str),
            Some("F1")
        );
        assert_eq!(
            settings
                .get(&Action::SettingsCycleBalance)
                .map(String::as_str),
            Some("F6")
        );
    }

    #[test]
    fn migration_preserves_status_bar_order_and_removes_duplicates() {
        let mut config = Config::default();
        config.ui.status_bar_items = vec![
            StatusBarItem::Source,
            StatusBarItem::Song,
            StatusBarItem::Source,
        ];

        assert!(migrate_legacy_config(&mut config));
        assert_eq!(
            config.ui.status_bar_items,
            vec![StatusBarItem::Source, StatusBarItem::Song]
        );
    }

    #[test]
    fn loads_partial_config_with_defaults() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "voicefox-partial-config-{}-{}.toml",
            std::process::id(),
            unique
        ));
        std::fs::write(&path, "[player]\nvolume = 42\n").unwrap();

        let (config, _) = load(path.to_str().unwrap()).unwrap();

        assert_eq!(config.player.volume, 42);
        assert_eq!(config.player.engine, "mpv");
        assert!(config.player.remember_playback_state);
        assert_eq!(config.player.history_limit, 100);
        assert_eq!(config.network.timeout, 15);
        assert_eq!(config.version, CURRENT_CONFIG_VERSION);
        assert_eq!(config.source.enabled, SourceId::default_enabled());
        let _ = std::fs::remove_file(path);
    }
}
