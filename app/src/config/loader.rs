use std::fs;
use std::path::PathBuf;

use lx_core::model::config::Config;

/// 加载配置：优先读用户配置文件，否则用默认值
pub fn load(custom_path: &str) -> anyhow::Result<(Config, PathBuf)> {
    let config_path = resolve_config_path(custom_path);

    match fs::read_to_string(&config_path) {
        Ok(content) => {
            let config: Config = toml::from_str(&content)?;
            Ok((config, config_path))
        }
        Err(_) => {
            // 配置文件不存在，写入默认配置
            let config = Config::default();
            let toml_str = toml::to_string_pretty(&config)?;
            if let Some(parent) = config_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&config_path, toml_str)?;
            Ok((config, config_path))
        }
    }
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
