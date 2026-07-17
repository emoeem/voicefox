use std::fs;
use std::path::PathBuf;

use lx_core::model::config::{CURRENT_CONFIG_VERSION, Config, ThemeConfig};
use lx_core::model::source::SourceId;

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
    let mut changed = migrate_legacy_theme(config);
    if config.version < CURRENT_CONFIG_VERSION {
        if config.source.enabled == [SourceId::Kw] {
            config.source.enabled = SourceId::all_online().to_vec();
        }
        config.version = CURRENT_CONFIG_VERSION;
        changed = true;
    }
    changed
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
    std::fs::write(path, toml_str)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{load, migrate_legacy_config};
    use lx_core::model::config::{CURRENT_CONFIG_VERSION, Config};
    use lx_core::model::source::SourceId;

    #[test]
    fn migrates_the_original_default_theme_to_mocha() {
        let mut config = Config::default();
        config.version = 0;
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
        let mut config = Config::default();
        config.version = 0;
        config.source.enabled = vec![SourceId::Kw];

        assert!(migrate_legacy_config(&mut config));
        assert_eq!(config.version, CURRENT_CONFIG_VERSION);
        assert_eq!(config.source.enabled, SourceId::all_online());

        config.source.enabled = vec![SourceId::Kw];
        assert!(!migrate_legacy_config(&mut config));
        assert_eq!(config.source.enabled, vec![SourceId::Kw]);
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
        assert_eq!(config.network.timeout, 15);
        assert_eq!(config.version, CURRENT_CONFIG_VERSION);
        assert_eq!(config.source.enabled, SourceId::all_online());
        let _ = std::fs::remove_file(path);
    }
}
