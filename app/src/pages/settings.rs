//! 设置页面：支持 JS 音源 URL 或本地路径导入/删除

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lx_core::events::AppAction;
use lx_core::keybinding::{Action, KeybindingConfig, KeybindingResolver};
use lx_core::model::config::StatusBarItem;
use lx_core::model::source::{Quality, SourceId};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::context::AppContext;

/// 删除类操作（音源 / 本地目录）二次确认的窗口时长
const DELETE_CONFIRM_WINDOW: Duration = Duration::from_secs(5);

/// 检查 JS 音源是否已缓存到本地
fn is_source_cached(url: &str) -> bool {
    lx_source::js::loader::is_source_cached(url)
}

fn shorten_source(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    let visible_chars = max_chars.saturating_sub(3);
    format!(
        "{}...",
        value.chars().take(visible_chars).collect::<String>()
    )
}

fn truncate_display(value: &str, max_chars: usize) -> String {
    let width = UnicodeWidthStr::width(value);
    if width <= max_chars {
        return format!("{value}{}", " ".repeat(max_chars - width));
    }
    if max_chars <= 1 {
        return "…".to_string();
    }
    let available = max_chars - 1;
    let mut result = String::new();
    let mut used = 0;
    for character in value.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used + character_width > available {
            break;
        }
        result.push(character);
        used += character_width;
    }
    result.push('…');
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsFocus {
    JsSources,
    LocalPaths,
    StatusBar,
}

/// 下载设置里的文本输入目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DownloadInputTarget {
    /// 下载目录
    Dir,
    /// 文件名模板
    Template,
    /// WebDAV 地址（可带 `user:pass@`，保存时拆成账号密码）
    WebdavUrl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsCategory {
    Interface,
    Playback,
    Sources,
    Integration,
    Download,
    Data,
}

impl SettingsCategory {
    fn next(self) -> Self {
        match self {
            Self::Interface => Self::Playback,
            Self::Playback => Self::Sources,
            Self::Sources => Self::Integration,
            Self::Integration => Self::Download,
            Self::Download => Self::Data,
            Self::Data => Self::Interface,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Interface => Self::Data,
            Self::Playback => Self::Interface,
            Self::Sources => Self::Playback,
            Self::Integration => Self::Sources,
            Self::Download => Self::Integration,
            Self::Data => Self::Download,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Interface => "界面",
            Self::Playback => "播放",
            Self::Sources => "音源与歌词",
            Self::Integration => "通知与集成",
            Self::Download => "下载",
            Self::Data => "数据与本地库",
        }
    }

    fn option_indices(self) -> &'static [usize] {
        match self {
            Self::Interface => &[0, 1, 2, 3, 4, 31, 32, 33, 39],
            Self::Playback => &[
                5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
            ],
            Self::Sources => &[23, 24, 25, 26, 27, 28, 29, 30],
            Self::Integration => &[34, 35, 36, 37, 38, 44],
            Self::Download => &[
                45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60,
            ],
            Self::Data => &[40, 41, 42, 43],
        }
    }
}

impl SettingsFocus {
    fn next(self) -> Self {
        match self {
            Self::JsSources => Self::LocalPaths,
            Self::LocalPaths => Self::StatusBar,
            Self::StatusBar => Self::JsSources,
        }
    }
}

pub struct SettingsPage {
    /// 输入中的 JS 源 URL 或本地路径
    pub input_url: String,
    /// 是否在输入模式
    pub input_mode: bool,
    /// 导入状态消息
    pub status_msg: Option<String>,
    /// JS 源列表的选中索引
    pub selected_source: usize,
    /// 本地音乐路径输入
    pub local_path_input: String,
    /// 本地音乐路径输入模式
    pub local_path_mode: bool,
    /// 本地路径列表选中索引
    pub selected_local_path: usize,
    /// 代理地址输入
    pub proxy_input: String,
    /// 代理地址输入模式
    pub proxy_input_mode: bool,
    /// 音频输出设备输入模式
    pub audio_device_input: String,
    pub audio_device_input_mode: bool,
    /// 外部歌单文件输入模式。
    pub playlist_import_input: String,
    pub playlist_import_mode: bool,
    /// 下载目录 / 文件名模板输入模式。
    download_input: String,
    download_input_target: Option<DownloadInputTarget>,
    /// 扫码登录选择器：支持扫码的音源列表里的选中项。
    login_picker: Option<usize>,
    /// 内置音源开关当前指向的音源
    pub enabled_source_index: usize,
    /// 状态栏字段列表的选中索引
    pub selected_status_item: usize,
    /// 状态栏字段列表的滚动位置
    status_item_scroll: usize,
    /// 状态栏拖拽当前所在的字段行，避免同一行重复触发重排。
    status_drag_target: Option<usize>,
    /// 当前聚焦区域
    focus: SettingsFocus,
    category: SettingsCategory,
    /// 删除 JS 音源的武装时刻：首次按 d 只武装，窗口内再按一次才删除
    delete_source_armed: Option<Instant>,
    /// 删除本地目录的武装时刻，机制同上
    delete_local_path_armed: Option<Instant>,
}

impl SettingsPage {
    /// 检查是否有任何输入模式激活（JS 源输入或本地路径输入）
    pub fn any_input_active(&self) -> bool {
        self.input_mode
            || self.local_path_mode
            || self.proxy_input_mode
            || self.audio_device_input_mode
            || self.playlist_import_mode
            || self.download_input_target.is_some()
    }

    /// 判断按键是否由设置页独占。设置页把整个字母表当作选项开关，
    /// 与用户可自定义的全局快捷键必然重叠，因此这些键不再交给全局分发。
    /// 未被 settings 页面级动作绑定的 Ctrl/Alt 组合键仍归全局。
    pub fn consumes_key(&self, key: &KeyEvent, resolver: &KeybindingResolver) -> bool {
        // Bare number keys are reserved for navigation (1-8 select sidebar
        // tabs). Even a stale or intentionally custom settings binding must
        // not make tab switching stop while the settings page is open.
        if key.modifiers == KeyModifiers::NONE && matches!(key.code, KeyCode::Char('0'..='9')) {
            return false;
        }
        if resolver
            .resolve_page("settings", key)
            .is_some_and(settings_action_is_page_owned)
        {
            return true;
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return false;
        }
        if self.focus == SettingsFocus::StatusBar
            && (matches!(
                (key.modifiers, key.code),
                (KeyModifiers::NONE, KeyCode::Enter | KeyCode::Char(' '))
            ) || matches!(
                (key.modifiers, key.code),
                (KeyModifiers::SHIFT, KeyCode::Left | KeyCode::Right)
            ))
        {
            return true;
        }
        match key.code {
            KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right => true,
            KeyCode::Char(character) => SETTINGS_PAGE_CHAR_KEYS.contains(&character),
            _ => false,
        }
    }

    pub fn new() -> Self {
        Self {
            input_url: String::new(),
            input_mode: false,
            status_msg: None,
            selected_source: 0,
            local_path_input: String::new(),
            local_path_mode: false,
            selected_local_path: 0,
            proxy_input: String::new(),
            proxy_input_mode: false,
            audio_device_input: String::new(),
            audio_device_input_mode: false,
            playlist_import_input: String::new(),
            playlist_import_mode: false,
            download_input: String::new(),
            download_input_target: None,
            login_picker: None,
            enabled_source_index: 0,
            selected_status_item: 0,
            status_item_scroll: 0,
            status_drag_target: None,
            focus: SettingsFocus::JsSources,
            category: SettingsCategory::Interface,
            delete_source_armed: None,
            delete_local_path_armed: None,
        }
    }

    pub fn handle_input(
        &mut self,
        key: KeyEvent,
        ctx: &AppContext,
        resolver: &KeybindingResolver,
    ) -> AppAction {
        if self.login_picker.is_some() {
            return self.handle_login_picker(key, ctx);
        }
        if self.proxy_input_mode {
            return self.handle_proxy_input(key, ctx);
        }
        if self.audio_device_input_mode {
            return self.handle_audio_device_input(key, ctx);
        }
        if self.playlist_import_mode {
            return self.handle_playlist_import_input(key, ctx);
        }
        if self.download_input_target.is_some() {
            return self.handle_download_input(key, ctx);
        }
        if self.local_path_mode {
            return self.handle_local_path_input(key, ctx);
        }
        if self.input_mode {
            match (key.modifiers, key.code) {
                (KeyModifiers::NONE, KeyCode::Esc) => {
                    self.input_mode = false;
                    self.input_url.clear();
                    return AppAction::None;
                }
                (KeyModifiers::NONE, KeyCode::Enter) => {
                    if !self.input_url.trim().is_empty() {
                        let url = self.input_url.trim().to_string();
                        self.input_mode = false;
                        self.input_url.clear();
                        self.status_msg = Some("正在添加音源...".to_string());
                        return AppAction::ImportSource(url);
                    }
                    return AppAction::None;
                }
                (modifiers, KeyCode::Char(c))
                    if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.input_url.push(c);
                }
                (KeyModifiers::NONE, KeyCode::Backspace) => {
                    self.input_url.pop();
                }
                _ => {}
            }
        } else {
            // 同一次按键只解析一次页面级键位，两处共用结果
            let bound_action = resolver.resolve_page("settings", &key);
            if let Some(action) = bound_action
                && let Some(result) = self.handle_bound_action(action, ctx)
            {
                return result;
            }

            if matches!(
                (key.modifiers, key.code),
                (KeyModifiers::NONE, KeyCode::Char('s'))
            ) {
                self.focus = self.focus.next();
                return AppAction::None;
            }

            if key.modifiers == KeyModifiers::NONE && key.code == KeyCode::Left {
                self.category = self.category.previous();
                self.status_msg = None;
                return AppAction::None;
            }
            if key.modifiers == KeyModifiers::NONE && key.code == KeyCode::Right {
                self.category = self.category.next();
                self.status_msg = None;
                return AppAction::None;
            }

            // 当前列表区域的按键优先处理。
            if self.focus == SettingsFocus::LocalPaths
                && let Some(action) = self.handle_local_keys(key, ctx, resolver)
            {
                return action;
            }
            if self.focus == SettingsFocus::StatusBar
                && let Some(action) = self.handle_status_bar_keys(key, ctx, resolver)
            {
                return action;
            }

            let sources = ctx
                .config
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .source
                .js_sources
                .clone();

            if let Some(action) = bound_action {
                match action {
                    Action::ListSelectUp => {
                        if self.selected_source > 0 {
                            self.selected_source -= 1;
                        }
                        return AppAction::None;
                    }
                    Action::ListSelectDown => {
                        if self.selected_source + 1 < sources.len() {
                            self.selected_source += 1;
                        }
                        return AppAction::None;
                    }
                    _ => {}
                }
            }

            match (key.modifiers, key.code) {
                (KeyModifiers::NONE, KeyCode::Char('a')) => {
                    self.input_mode = true;
                    self.status_msg = None;
                }
                (KeyModifiers::NONE, KeyCode::Up) => {
                    if self.selected_source > 0 {
                        self.selected_source -= 1;
                    }
                }
                (KeyModifiers::NONE, KeyCode::Down) => {
                    if self.selected_source + 1 < sources.len() {
                        self.selected_source += 1;
                    }
                }
                (KeyModifiers::NONE, KeyCode::Char('d')) => {
                    if !sources.is_empty() && self.selected_source < sources.len() {
                        // 二次确认：首次按 d 只武装并提示，窗口内再按一次才真正删除
                        let now = Instant::now();
                        let confirmed = matches!(
                            self.delete_source_armed,
                            Some(armed_at)
                                if now.duration_since(armed_at) <= DELETE_CONFIRM_WINDOW
                        );
                        if !confirmed {
                            self.delete_source_armed = Some(now);
                            self.status_msg =
                                Some("再按一次 d 确认删除该音源，Esc 取消".to_string());
                            return AppAction::None;
                        }
                        self.delete_source_armed = None;
                        let url = sources[self.selected_source].clone();
                        self.status_msg = Some("已移除音源".to_string());
                        if self.selected_source >= sources.len().saturating_sub(1) {
                            self.selected_source = self.selected_source.saturating_sub(1);
                        }
                        return AppAction::RemoveSource(url);
                    }
                }
                (KeyModifiers::NONE, KeyCode::Char('h'))
                    if self.focus == SettingsFocus::JsSources =>
                {
                    self.status_msg = Some("正在检测音源…".to_string());
                    return AppAction::CheckSourceHealth;
                }
                (KeyModifiers::NONE, KeyCode::Char('t')) => {
                    self.update_config(ctx, |config| {
                        config.ui.enable_mouse = !config.ui.enable_mouse;
                    });
                }
                (KeyModifiers::NONE, KeyCode::Char('g')) => {
                    self.update_config(ctx, |config| {
                        config.ui.aggregate_search = !config.ui.aggregate_search;
                    });
                }
                (KeyModifiers::NONE, KeyCode::Char('w')) => {
                    self.update_config(ctx, |config| {
                        config.ui.wrap_navigation = !config.ui.wrap_navigation;
                    });
                }
                (KeyModifiers::NONE, KeyCode::Char('c')) => {
                    self.update_config(ctx, |config| {
                        config.ui.show_cover = !config.ui.show_cover;
                    });
                    if !ctx
                        .config
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .ui
                        .show_cover
                    {
                        ctx.cover_service.clear();
                    }
                }
                (KeyModifiers::NONE, KeyCode::Char('e')) => {
                    let enabled = {
                        let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                        config.player.remember_playback_state =
                            !config.player.remember_playback_state;
                        let enabled = config.player.remember_playback_state;
                        let result = crate::config::loader::save(&config, &ctx.config_path);
                        self.status_msg = Some(match result {
                            Ok(()) => "设置已保存".to_string(),
                            Err(error) => format!("保存设置失败: {}", error),
                        });
                        enabled
                    };
                    let result = if enabled {
                        ctx.persist_playback_session()
                    } else {
                        ctx.storage.clear_playback_session()
                    };
                    if let Err(error) = result {
                        self.status_msg =
                            Some(format!("播放状态设置已更新，但会话保存失败: {error}"));
                    }
                }
                (KeyModifiers::SHIFT, KeyCode::Char('Q' | 'q'))
                | (KeyModifiers::NONE, KeyCode::Char('Q')) => {
                    self.update_config(ctx, |config| {
                        config.player.quality = next_quality(config.player.quality);
                    });
                }
                (KeyModifiers::SHIFT, KeyCode::Char('H' | 'h'))
                | (KeyModifiers::NONE, KeyCode::Char('H')) => {
                    let limit = {
                        let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                        config.player.history_limit =
                            next_history_limit(config.player.history_limit);
                        let limit = config.player.history_limit;
                        let result = crate::config::loader::save(&config, &ctx.config_path);
                        self.status_msg = Some(match result {
                            Ok(()) => format!("历史上限: {limit}"),
                            Err(error) => format!("保存设置失败: {error}"),
                        });
                        limit
                    };
                    ctx.storage.trim_history(limit);
                }
                (KeyModifiers::NONE, KeyCode::Char('v')) => {
                    self.cycle_default_source(ctx);
                }
                (KeyModifiers::NONE, KeyCode::Char('u')) => {
                    self.update_config(ctx, |config| {
                        config.source.auto_toggle = !config.source.auto_toggle;
                    });
                }
                (KeyModifiers::NONE, KeyCode::Char('y')) => {
                    self.enabled_source_index =
                        (self.enabled_source_index + 1) % SourceId::all_online().len();
                }
                (KeyModifiers::SHIFT, KeyCode::Char('K' | 'k'))
                | (KeyModifiers::NONE, KeyCode::Char('K')) => {
                    self.toggle_selected_source(ctx);
                }
                (KeyModifiers::SHIFT, KeyCode::Char('T' | 't'))
                | (KeyModifiers::NONE, KeyCode::Char('T')) => {
                    let enabled = {
                        let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                        config.lyric.show_translation = !config.lyric.show_translation;
                        let enabled = config.lyric.show_translation;
                        self.status_msg =
                            save_status(crate::config::loader::save(&config, &ctx.config_path));
                        enabled
                    };
                    ctx.lyric_service.set_translation_enabled(enabled);
                }
                (KeyModifiers::SHIFT, KeyCode::Char('Y' | 'y'))
                | (KeyModifiers::NONE, KeyCode::Char('Y')) => {
                    let enabled = {
                        let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                        config.lyric.show_yrc = !config.lyric.show_yrc;
                        let enabled = config.lyric.show_yrc;
                        self.status_msg =
                            save_status(crate::config::loader::save(&config, &ctx.config_path));
                        enabled
                    };
                    ctx.lyric_service.set_yrc_enabled(enabled);
                }
                (KeyModifiers::NONE, KeyCode::Char('[')) => {
                    self.adjust_lyric_offset(ctx, -100);
                }
                (KeyModifiers::NONE, KeyCode::Char(']')) => {
                    self.adjust_lyric_offset(ctx, 100);
                }
                (KeyModifiers::NONE, KeyCode::Char('n')) => {
                    self.proxy_input = ctx
                        .config
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .network
                        .proxy_url
                        .clone();
                    self.proxy_input_mode = true;
                    self.status_msg = None;
                }
                (KeyModifiers::SHIFT, KeyCode::Char('N' | 'n'))
                | (KeyModifiers::NONE, KeyCode::Char('N')) => {
                    let (proxy, timeout) = {
                        let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                        config.network.timeout = next_network_timeout(config.network.timeout);
                        let values = (config.network.proxy_url.clone(), config.network.timeout);
                        self.status_msg =
                            save_status(crate::config::loader::save(&config, &ctx.config_path));
                        values
                    };
                    lx_source::configure_network(&proxy, timeout);
                }
                (KeyModifiers::SHIFT, KeyCode::Char('P' | 'p'))
                | (KeyModifiers::NONE, KeyCode::Char('P')) => {
                    self.update_config(ctx, |config| {
                        config.ui.cover_protocol =
                            next_cover_protocol(&config.ui.cover_protocol).to_string();
                    });
                    if self.status_msg.as_deref() == Some("设置已保存") {
                        self.status_msg = Some("封面协议已保存，下次启动生效".to_string());
                    }
                }
                (KeyModifiers::NONE, KeyCode::Char('f')) => {
                    self.update_config(ctx, |config| {
                        config.ui.max_fps = next_fps(config.ui.max_fps);
                    });
                    if self.status_msg.as_deref() == Some("设置已保存") {
                        self.status_msg = Some("刷新率已保存，下次启动生效".to_string());
                    }
                }
                (KeyModifiers::NONE, KeyCode::Char('z')) => {
                    self.update_config(ctx, |config| {
                        config.ui.scroll_amount = next_scroll_amount(config.ui.scroll_amount);
                    });
                }
                (KeyModifiers::NONE, KeyCode::Char('i')) => {
                    self.update_config(ctx, |config| {
                        config.integration.mpris = !config.integration.mpris;
                    });
                    if self.status_msg.as_deref() == Some("设置已保存") {
                        self.status_msg = Some("MPRIS 设置已保存，下次启动生效".to_string());
                    }
                }
                (KeyModifiers::NONE, KeyCode::Char('o')) => {
                    self.update_config(ctx, |config| {
                        config.notification.in_app = !config.notification.in_app;
                    });
                }
                (KeyModifiers::SHIFT, KeyCode::Char('O' | 'o'))
                | (KeyModifiers::NONE, KeyCode::Char('O')) => {
                    self.update_config(ctx, |config| {
                        config.notification.in_app_timeout =
                            match config.notification.in_app_timeout {
                                0..=2 => 4,
                                3..=4 => 6,
                                5..=6 => 8,
                                _ => 2,
                            };
                    });
                }
                (KeyModifiers::NONE, KeyCode::Char('x')) => {
                    self.update_config(ctx, |config| {
                        config.notification.enable = !config.notification.enable;
                    });
                }
                (KeyModifiers::SHIFT, KeyCode::Char('X' | 'x'))
                | (KeyModifiers::NONE, KeyCode::Char('X')) => {
                    self.update_config(ctx, |config| {
                        config.notification.album_cover = !config.notification.album_cover;
                    });
                }
                (KeyModifiers::SHIFT, KeyCode::Char('R' | 'r'))
                | (KeyModifiers::NONE, KeyCode::Char('R')) => {
                    self.update_config(ctx, |config| {
                        config.notification.track_change = !config.notification.track_change;
                    });
                }
                // --- 下载设置 ---
                (KeyModifiers::SHIFT, KeyCode::Char('S' | 's'))
                | (KeyModifiers::NONE, KeyCode::Char('S')) => {
                    self.download_input = ctx.downloads.download_dir().display().to_string();
                    self.download_input_target = Some(DownloadInputTarget::Dir);
                    self.status_msg = Some("输入下载目录，Enter 保存，Esc 取消".to_string());
                }
                (KeyModifiers::SHIFT, KeyCode::Char('M' | 'm'))
                | (KeyModifiers::NONE, KeyCode::Char('M')) => {
                    self.download_input = ctx
                        .config
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .download
                        .filename_template
                        .clone();
                    self.download_input_target = Some(DownloadInputTarget::Template);
                    self.status_msg = Some(
                        "输入文件名模板，支持 {name} {singer} {album} {source} {quality}"
                            .to_string(),
                    );
                }
                (KeyModifiers::SHIFT, KeyCode::Char('F' | 'f'))
                | (KeyModifiers::NONE, KeyCode::Char('F')) => {
                    self.update_config(ctx, |config| {
                        config.download.quality = match config.download.quality {
                            None => Some(config.player.quality),
                            Some(Quality::Low128) => Some(Quality::High320),
                            Some(Quality::High320) => Some(Quality::Flac),
                            Some(Quality::Flac) => Some(Quality::Flac24),
                            Some(Quality::Flac24) => None,
                        };
                    });
                }
                (KeyModifiers::SHIFT, KeyCode::Char('B' | 'b'))
                | (KeyModifiers::NONE, KeyCode::Char('B')) => {
                    self.update_config(ctx, |config| {
                        config.download.multipart = !config.download.multipart;
                    });
                }
                (KeyModifiers::SHIFT, KeyCode::Char('V' | 'v'))
                | (KeyModifiers::NONE, KeyCode::Char('V')) => {
                    self.update_config(ctx, |config| {
                        config.download.multipart_min_size_mb = next_step(
                            &[1, 2, 5, 10, 20, 50],
                            config.download.multipart_min_size_mb,
                        );
                    });
                }
                (KeyModifiers::SHIFT, KeyCode::Char('W' | 'w'))
                | (KeyModifiers::NONE, KeyCode::Char('W')) => {
                    self.update_config(ctx, |config| {
                        config.download.concurrency =
                            next_step(&[1, 2, 4, 8, 16], config.download.concurrency as u64)
                                as usize;
                    });
                }
                (KeyModifiers::SHIFT, KeyCode::Char('A' | 'a'))
                | (KeyModifiers::NONE, KeyCode::Char('A')) => {
                    self.update_config(ctx, |config| {
                        config.download.concurrent_songs =
                            next_step(&[1, 2, 3, 4], config.download.concurrent_songs as u64)
                                as usize;
                    });
                }
                // --- WebDAV 同步 ---
                (KeyModifiers::SHIFT, KeyCode::Char('Z' | 'z'))
                | (KeyModifiers::NONE, KeyCode::Char('Z')) => {
                    self.update_config(ctx, |config| {
                        config.download.webdav.enabled = !config.download.webdav.enabled;
                    });
                }
                (KeyModifiers::SHIFT, KeyCode::Char('C' | 'c'))
                | (KeyModifiers::NONE, KeyCode::Char('C')) => {
                    self.download_input = ctx
                        .config
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .download
                        .webdav
                        .display_url();
                    self.download_input_target = Some(DownloadInputTarget::WebdavUrl);
                    self.status_msg = Some(
                        "输入 WebDAV 地址（可用 https://用户:密码@主机/路径），Enter 保存，Esc 取消"
                            .to_string(),
                    );
                }
                (KeyModifiers::SHIFT, KeyCode::Char('E' | 'e'))
                | (KeyModifiers::NONE, KeyCode::Char('E')) => {
                    self.update_config(ctx, |config| {
                        config.download.max_retries =
                            next_step(&[0, 1, 2, 3, 5], config.download.max_retries as u64) as u32;
                    });
                }
                (KeyModifiers::SHIFT, KeyCode::Char('U' | 'u'))
                | (KeyModifiers::NONE, KeyCode::Char('U')) => {
                    self.update_config(ctx, |config| {
                        config.download.verify_size = !config.download.verify_size;
                    });
                }
                (KeyModifiers::SHIFT, KeyCode::Char('L' | 'l'))
                | (KeyModifiers::NONE, KeyCode::Char('L')) => {
                    self.update_config(ctx, |config| {
                        config.download.skip_existing = !config.download.skip_existing;
                    });
                }
                (KeyModifiers::SHIFT, KeyCode::Char('J' | 'j'))
                | (KeyModifiers::NONE, KeyCode::Char('J')) => {
                    self.update_config(ctx, |config| {
                        config.download.write_tags = !config.download.write_tags;
                    });
                }
                (KeyModifiers::SHIFT, KeyCode::Char('G' | 'g'))
                | (KeyModifiers::NONE, KeyCode::Char('G')) => {
                    self.update_config(ctx, |config| {
                        config.download.embed_cover = !config.download.embed_cover;
                    });
                }
                (KeyModifiers::SHIFT, KeyCode::Char('I' | 'i'))
                | (KeyModifiers::NONE, KeyCode::Char('I')) => {
                    self.update_config(ctx, |config| {
                        config.download.save_lyric = !config.download.save_lyric;
                    });
                }
                (KeyModifiers::NONE, KeyCode::Char('m')) => {
                    let mode = ctx.playlist.cycle_mode();
                    let result = {
                        let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                        config.player.play_mode = mode.as_config().to_string();
                        crate::config::loader::save(&config, &ctx.config_path)
                    };
                    self.status_msg = Some(match result {
                        Ok(()) => format!("播放模式: {}", mode.label()),
                        Err(error) => format!("播放模式已切换，但保存失败: {}", error),
                    });
                }
                (KeyModifiers::NONE, KeyCode::Char('p')) => {
                    self.update_config(ctx, |config| {
                        config.theme.accent = match config.theme.accent.as_str() {
                            "#cba6f7" => "#89b4fa",
                            "#89b4fa" => "#94e2d5",
                            "#94e2d5" => "#f5c2e7",
                            "#f5c2e7" => "#fab387",
                            _ => "#cba6f7",
                        }
                        .to_string();
                    });
                }
                (KeyModifiers::SHIFT, KeyCode::Char('D' | 'd'))
                | (KeyModifiers::NONE, KeyCode::Char('D')) => {
                    self.update_config(ctx, |config| {
                        config.local_music.max_depth =
                            next_scan_depth(config.local_music.max_depth);
                    });
                }
                (KeyModifiers::NONE, KeyCode::Char('b')) => {
                    // 支持扫码的音源不止一个，先打开选择器再决定登录/退出。
                    self.login_picker = Some(0);
                }
                (KeyModifiers::NONE, KeyCode::Esc) => {
                    // Esc 取消删除类操作（音源 / 本地目录）的武装状态
                    self.delete_source_armed = None;
                    self.delete_local_path_armed = None;
                }
                _ => {}
            }
        }
        AppAction::None
    }

    /// 执行通过 settings 页面级键位映射解析出的播放与数据动作。
    /// 返回 `None` 表示该动作由其他设置页逻辑处理。
    fn handle_bound_action(&mut self, action: Action, ctx: &AppContext) -> Option<AppAction> {
        match action {
            Action::SettingsCyclePlaybackSpeed => {
                self.status_msg = Some(ctx.cycle_playback_speed());
            }
            Action::SettingsEditAudioDevice => {
                self.audio_device_input = ctx
                    .config
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .player
                    .audio_device
                    .clone();
                self.audio_device_input_mode = true;
                self.status_msg = Some("输入 libmpv 音频设备名，Enter 保存".to_string());
            }
            Action::SettingsCycleReplayGainMode => {
                self.status_msg = Some(ctx.cycle_replaygain_mode());
            }
            Action::SettingsCycleReplayGainPreamp => {
                self.status_msg = Some(ctx.cycle_replaygain_preamp());
            }
            Action::SettingsCycleChannelMode => {
                self.status_msg = Some(ctx.cycle_channel_mode());
            }
            Action::SettingsCycleBalance => {
                self.status_msg = Some(ctx.cycle_balance());
            }
            Action::SettingsToggleReplayGainClip => {
                self.status_msg = Some(ctx.toggle_replaygain_clip());
            }
            Action::SettingsCycleFadeInDuration => {
                let duration = {
                    let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                    config.player.fade_in_ms = next_fade_duration(config.player.fade_in_ms);
                    let value = config.player.fade_in_ms;
                    self.status_msg =
                        save_status(crate::config::loader::save(&config, &ctx.config_path));
                    value
                };
                self.status_msg = Some(format!("淡入: {}", fade_label(duration)));
            }
            Action::SettingsCycleFadeOutDuration => {
                let duration = {
                    let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                    config.player.fade_out_ms = next_fade_duration(config.player.fade_out_ms);
                    let value = config.player.fade_out_ms;
                    self.status_msg =
                        save_status(crate::config::loader::save(&config, &ctx.config_path));
                    value
                };
                self.status_msg = Some(format!("淡出: {}", fade_label(duration)));
            }
            Action::SettingsCycleEqualizerPreset => {
                self.status_msg = Some(ctx.cycle_equalizer_preset());
            }
            Action::SettingsRunFadeIn => {
                self.status_msg = Some(ctx.fade_in_now());
            }
            Action::SettingsRunFadeOut => {
                self.status_msg = Some(ctx.fade_out_now());
            }
            Action::SettingsSetAbLoopStart => {
                self.status_msg = Some(ctx.set_ab_loop_start_now());
            }
            Action::SettingsSetAbLoopEnd => {
                self.status_msg = Some(ctx.set_ab_loop_end_now());
            }
            Action::SettingsClearAbLoop => {
                self.status_msg = Some(ctx.clear_ab_loop());
            }
            Action::SettingsExportData => {
                self.status_msg = Some(match ctx.storage.export_default() {
                    Ok(path) => format!("数据已导出: {}", path.display()),
                    Err(error) => format!("数据导出失败: {error}"),
                });
            }
            Action::SettingsImportData => {
                self.status_msg = Some(match ctx.storage.import_default() {
                    Ok(path) => format!("数据已导入，原数据备份于: {}", path.display()),
                    Err(error) => format!("数据导入失败: {error}"),
                });
            }
            Action::SettingsImportPlaylist => {
                self.playlist_import_input.clear();
                self.playlist_import_mode = true;
                self.status_msg = Some("输入 M3U/LX Music/网易云歌单路径，Enter 导入".to_string());
            }
            _ => return None,
        }
        Some(AppAction::None)
    }

    fn handle_proxy_input(&mut self, key: KeyEvent, ctx: &AppContext) -> AppAction {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc) => {
                self.proxy_input_mode = false;
                self.proxy_input.clear();
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                let proxy = self.proxy_input.trim().to_string();
                let timeout = {
                    let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                    config.network.proxy_url = proxy.clone();
                    let timeout = config.network.timeout;
                    self.status_msg =
                        save_status(crate::config::loader::save(&config, &ctx.config_path));
                    timeout
                };
                lx_source::configure_network(&proxy, timeout);
                self.proxy_input_mode = false;
                self.proxy_input.clear();
            }
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                self.proxy_input.pop();
            }
            (modifiers, KeyCode::Char(c))
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.proxy_input.push(c);
            }
            _ => {}
        }
        AppAction::None
    }

    fn handle_audio_device_input(&mut self, key: KeyEvent, ctx: &AppContext) -> AppAction {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc) => {
                self.audio_device_input_mode = false;
                self.audio_device_input.clear();
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                let device = self.audio_device_input.trim().to_string();
                if !device.is_empty() {
                    self.status_msg = Some(ctx.set_audio_output_device(&device));
                }
                self.audio_device_input_mode = false;
                self.audio_device_input.clear();
            }
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                self.audio_device_input.pop();
            }
            (modifiers, KeyCode::Char(c))
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && c != '\0' =>
            {
                self.audio_device_input.push(c);
            }
            _ => {}
        }
        AppAction::None
    }

    /// 扫码登录选择器：`Enter` 登录，已登录时 `Enter`/`x` 退出，`Esc` 关闭。
    fn handle_login_picker(&mut self, key: KeyEvent, ctx: &AppContext) -> AppAction {
        let sources = ctx
            .source_manager
            .sources_supporting(|capabilities| capabilities.qr_login);
        if sources.is_empty() {
            self.login_picker = None;
            return AppAction::None;
        }
        let selected = self.login_picker.unwrap_or(0).min(sources.len() - 1);
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc) | (KeyModifiers::NONE, KeyCode::Char('q')) => {
                self.login_picker = None;
            }
            (KeyModifiers::NONE, KeyCode::Char('j' | 'J'))
            | (KeyModifiers::NONE, KeyCode::Down) => {
                self.login_picker = Some((selected + 1).min(sources.len() - 1));
            }
            (KeyModifiers::NONE, KeyCode::Char('k' | 'K')) | (KeyModifiers::NONE, KeyCode::Up) => {
                self.login_picker = Some(selected.saturating_sub(1));
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                let source = sources[selected];
                self.login_picker = None;
                return if ctx.source_manager.is_logged_in(source) {
                    AppAction::QrLogout(source)
                } else {
                    AppAction::QrLogin(source)
                };
            }
            (KeyModifiers::NONE, KeyCode::Char('x' | 'X')) => {
                let source = sources[selected];
                self.login_picker = None;
                return AppAction::QrLogout(source);
            }
            _ => {}
        }
        AppAction::None
    }

    /// 下载目录 / 文件名模板的文本输入。
    fn handle_download_input(&mut self, key: KeyEvent, ctx: &AppContext) -> AppAction {
        let Some(target) = self.download_input_target else {
            return AppAction::None;
        };
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc) => {
                self.download_input_target = None;
                self.download_input.clear();
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                let value = self.download_input.trim().to_string();
                self.download_input_target = None;
                self.download_input.clear();
                match target {
                    DownloadInputTarget::Dir => {
                        let dir = crate::download::naming::resolve_download_dir(&value);
                        self.update_config(ctx, |config| {
                            config.download.dir = value.clone();
                        });
                        self.status_msg = Some(format!("下载目录: {}", dir.display()));
                    }
                    DownloadInputTarget::Template => {
                        if value.is_empty() {
                            return AppAction::None;
                        }
                        self.update_config(ctx, |config| {
                            config.download.filename_template = value.clone();
                        });
                        self.status_msg = Some(format!("文件名模板: {value}"));
                    }
                    DownloadInputTarget::WebdavUrl => {
                        let mut display = String::new();
                        self.update_config(ctx, |config| {
                            config.download.webdav.apply_url_input(&value);
                            display = config.download.webdav.display_url();
                        });
                        self.status_msg = Some(if display.is_empty() {
                            "WebDAV 地址已清空".to_string()
                        } else {
                            format!("WebDAV 地址: {display}")
                        });
                    }
                }
            }
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                self.download_input.pop();
            }
            (modifiers, KeyCode::Char(c))
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && c != '\0' =>
            {
                self.download_input.push(c);
            }
            _ => {}
        }
        AppAction::None
    }

    fn handle_playlist_import_input(&mut self, key: KeyEvent, _ctx: &AppContext) -> AppAction {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc) => {
                self.playlist_import_mode = false;
                self.playlist_import_input.clear();
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                let path = self.playlist_import_input.trim().to_string();
                self.playlist_import_mode = false;
                self.playlist_import_input.clear();
                if path.is_empty() {
                    return AppAction::None;
                }
                // 导入在后台任务中完成（解析大歌单 + 一次性写盘），
                // 完成后通过通知汇报结果，避免阻塞 TUI 主循环。
                return AppAction::ImportExternalPlaylist(path);
            }
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                self.playlist_import_input.pop();
            }
            (modifiers, KeyCode::Char(c))
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.playlist_import_input.push(c);
            }
            _ => {}
        }
        AppAction::None
    }

    fn cycle_default_source(&mut self, ctx: &AppContext) {
        let (default, enabled) = {
            let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
            (config.source.default, config.source.enabled.clone())
        };
        if enabled.is_empty() {
            self.status_msg = Some("请先启用至少一个在线音源".to_string());
            return;
        }
        let current = enabled
            .iter()
            .position(|source| *source == default)
            .unwrap_or(0);
        let default = enabled[(current + 1) % enabled.len()];
        let save_result = {
            let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
            config.source.default = default;
            crate::config::loader::save(&config, &ctx.config_path)
        };
        ctx.source_manager
            .update_source_preferences(default, &enabled);
        self.status_msg = Some(match save_result {
            Ok(()) => format!("默认音源: {}", default.as_str()),
            Err(error) => format!("默认音源已切换，但保存失败: {error}"),
        });
    }

    fn toggle_selected_source(&mut self, ctx: &AppContext) {
        // 与渲染处一致对列表长度取模，防止索引越界 panic
        let sources = SourceId::all_online();
        self.enabled_source_index %= sources.len();
        let source = sources[self.enabled_source_index];
        let (default, enabled, save_result) = {
            let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
            if config.source.enabled.contains(&source) {
                if config.source.enabled.len() == 1 {
                    self.status_msg = Some("至少需要保留一个在线音源".to_string());
                    return;
                }
                config.source.enabled.retain(|item| *item != source);
                if config.source.default == source {
                    config.source.default = config.source.enabled[0];
                }
            } else {
                config.source.enabled.push(source);
                config.source.enabled.sort_by_key(|item| {
                    SourceId::all_online()
                        .iter()
                        .position(|candidate| candidate == item)
                        .unwrap_or(usize::MAX)
                });
            }
            let default = config.source.default;
            let enabled = config.source.enabled.clone();
            let save_result = crate::config::loader::save(&config, &ctx.config_path);
            (default, enabled, save_result)
        };
        ctx.source_manager
            .update_source_preferences(default, &enabled);
        self.status_msg = Some(match save_result {
            Ok(()) => format!(
                "{}音源 {}",
                source.as_str(),
                if enabled.contains(&source) {
                    "已启用"
                } else {
                    "已禁用"
                }
            ),
            Err(error) => format!("音源设置已更新，但保存失败: {error}"),
        });
    }

    fn adjust_lyric_offset(&mut self, ctx: &AppContext, delta: i32) {
        let offset = {
            let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
            config.lyric.offset = config
                .lyric
                .offset
                .saturating_add(delta)
                .clamp(-5_000, 5_000);
            let offset = config.lyric.offset;
            self.status_msg = save_status(crate::config::loader::save(&config, &ctx.config_path));
            offset
        };
        ctx.lyric_service.set_offset_ms(offset);
    }

    /// 处理本地音乐路径输入模式
    fn handle_local_path_input(&mut self, key: KeyEvent, ctx: &AppContext) -> AppAction {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc) => {
                self.local_path_mode = false;
                self.local_path_input.clear();
                AppAction::None
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                if !self.local_path_input.trim().is_empty() {
                    let path = self.local_path_input.trim().to_string();
                    let save_result = {
                        let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                        if !config.local_music.paths.contains(&path) {
                            config.local_music.paths.push(path.clone());
                            config.local_music.enabled = true;
                        }
                        crate::config::loader::save(&config, &ctx.config_path)
                    };
                    let (paths, max_depth) = {
                        let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
                        (
                            config.local_music.paths.clone(),
                            config.local_music.max_depth,
                        )
                    };
                    self.local_path_mode = false;
                    self.local_path_input.clear();
                    if let Err(error) = save_result {
                        self.status_msg = Some(format!("目录已添加，但保存失败: {}", error));
                    } else {
                        self.status_msg = Some("正在扫描本地音乐...".to_string());
                    }
                    return AppAction::ScanLocalMusic {
                        paths,
                        max_depth,
                        force: true,
                    };
                }
                AppAction::None
            }
            (modifiers, KeyCode::Char(c))
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.local_path_input.push(c);
                AppAction::None
            }
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                self.local_path_input.pop();
                AppAction::None
            }
            _ => AppAction::None,
        }
    }

    /// 处理本地音乐区域的按键（非输入模式）
    fn handle_local_keys(
        &mut self,
        key: KeyEvent,
        ctx: &AppContext,
        resolver: &KeybindingResolver,
    ) -> Option<AppAction> {
        let paths = ctx
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .local_music
            .paths
            .clone();

        if let Some(action) = resolver.resolve_page("settings", &key) {
            match action {
                Action::ListSelectUp => {
                    if self.selected_local_path > 0 {
                        self.selected_local_path -= 1;
                    }
                    return Some(AppAction::None);
                }
                Action::ListSelectDown => {
                    if self.selected_local_path + 1 < paths.len() {
                        self.selected_local_path += 1;
                    }
                    return Some(AppAction::None);
                }
                _ => {}
            }
        }

        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Char('a')) => {
                self.local_path_mode = true;
                self.local_path_input.clear();
                self.status_msg = None;
                Some(AppAction::None)
            }
            (KeyModifiers::NONE, KeyCode::Up) => {
                if self.selected_local_path > 0 {
                    self.selected_local_path -= 1;
                }
                Some(AppAction::None)
            }
            (KeyModifiers::NONE, KeyCode::Down) => {
                if self.selected_local_path + 1 < paths.len() {
                    self.selected_local_path += 1;
                }
                Some(AppAction::None)
            }
            (KeyModifiers::NONE, KeyCode::Char('d')) => {
                if !paths.is_empty() && self.selected_local_path < paths.len() {
                    // 二次确认：首次按 d 只武装并提示，窗口内再按一次才真正删除
                    let now = Instant::now();
                    let confirmed = matches!(
                        self.delete_local_path_armed,
                        Some(armed_at) if now.duration_since(armed_at) <= DELETE_CONFIRM_WINDOW
                    );
                    if !confirmed {
                        self.delete_local_path_armed = Some(now);
                        self.status_msg =
                            Some("再按一次 d 确认删除该本地目录，Esc 取消".to_string());
                        return Some(AppAction::None);
                    }
                    self.delete_local_path_armed = None;
                    let removed = paths[self.selected_local_path].clone();
                    let save_result = {
                        let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
                        config.local_music.paths.retain(|p| p != &removed);
                        config.local_music.enabled = !config.local_music.paths.is_empty();
                        crate::config::loader::save(&config, &ctx.config_path)
                    };
                    if self.selected_local_path >= paths.len().saturating_sub(1) {
                        self.selected_local_path = self.selected_local_path.saturating_sub(1);
                    }
                    let (remaining, max_depth) = {
                        let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
                        (
                            config.local_music.paths.clone(),
                            config.local_music.max_depth,
                        )
                    };
                    self.status_msg = Some(match save_result {
                        Ok(()) => format!("已移除 {}，正在重新扫描...", removed),
                        Err(error) => format!("已移除，但保存失败: {}", error),
                    });
                    return Some(AppAction::ScanLocalMusic {
                        paths: remaining,
                        max_depth,
                        force: true,
                    });
                }
                Some(AppAction::None)
            }
            (KeyModifiers::NONE, KeyCode::Char('r')) => {
                let max_depth = ctx
                    .config
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .local_music
                    .max_depth;
                self.status_msg = Some("正在扫描本地音乐...".to_string());
                Some(AppAction::ScanLocalMusic {
                    paths,
                    max_depth,
                    force: true,
                })
            }
            _ => None,
        }
    }

    fn handle_status_bar_keys(
        &mut self,
        key: KeyEvent,
        ctx: &AppContext,
        resolver: &KeybindingResolver,
    ) -> Option<AppAction> {
        let item_count = StatusBarItem::ALL.len();
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Enter | KeyCode::Char(' ')) => {
                self.toggle_status_bar_item(ctx);
                return Some(AppAction::None);
            }
            (KeyModifiers::SHIFT, KeyCode::Left | KeyCode::Up) => {
                self.move_status_bar_item(ctx, -1);
                return Some(AppAction::None);
            }
            (KeyModifiers::SHIFT, KeyCode::Right | KeyCode::Down) => {
                self.move_status_bar_item(ctx, 1);
                return Some(AppAction::None);
            }
            (KeyModifiers::NONE, KeyCode::Up) => {
                self.selected_status_item = self.selected_status_item.saturating_sub(1);
                return Some(AppAction::None);
            }
            (KeyModifiers::NONE, KeyCode::Down) => {
                self.selected_status_item =
                    (self.selected_status_item + 1).min(item_count.saturating_sub(1));
                return Some(AppAction::None);
            }
            (KeyModifiers::NONE, KeyCode::Char('a' | 'd' | 'r')) => {
                return Some(AppAction::None);
            }
            _ => {}
        }

        if let Some(action) = resolver.resolve_page("settings", &key) {
            match action {
                Action::ListSelectUp => {
                    self.selected_status_item = self.selected_status_item.saturating_sub(1);
                    return Some(AppAction::None);
                }
                Action::ListSelectDown => {
                    self.selected_status_item =
                        (self.selected_status_item + 1).min(item_count.saturating_sub(1));
                    return Some(AppAction::None);
                }
                _ => {}
            }
        }
        None
    }

    fn toggle_status_bar_item(&mut self, ctx: &AppContext) {
        let item = StatusBarItem::ALL[self.selected_status_item % StatusBarItem::ALL.len()];
        let (enabled, result) = {
            let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
            if config.ui.status_bar_items.contains(&item) {
                config
                    .ui
                    .status_bar_items
                    .retain(|candidate| *candidate != item);
                let result = crate::config::loader::save(&config, &ctx.config_path);
                (false, result)
            } else {
                config.ui.status_bar_items.push(item);
                let result = crate::config::loader::save(&config, &ctx.config_path);
                (true, result)
            }
        };
        self.status_msg = Some(match result {
            Ok(()) => format!(
                "状态栏“{}”已{}",
                status_bar_item_label(item),
                if enabled { "显示" } else { "隐藏" }
            ),
            Err(error) => format!("状态栏已更新，但保存失败: {error}"),
        });
    }

    fn move_status_bar_item(&mut self, ctx: &AppContext, direction: isize) {
        let item = StatusBarItem::ALL[self.selected_status_item % StatusBarItem::ALL.len()];
        let (position, result) = {
            let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
            let Some(index) = config
                .ui
                .status_bar_items
                .iter()
                .position(|candidate| *candidate == item)
            else {
                self.status_msg = Some("请先启用这个状态栏字段".to_string());
                return;
            };
            let new_index = if direction < 0 {
                index.saturating_sub(1)
            } else {
                (index + 1).min(config.ui.status_bar_items.len().saturating_sub(1))
            };
            if new_index == index {
                return;
            }
            config.ui.status_bar_items.swap(index, new_index);
            let result = crate::config::loader::save(&config, &ctx.config_path);
            (new_index + 1, result)
        };
        self.status_msg = Some(match result {
            Ok(()) => format!(
                "状态栏“{}”已移到第 {position} 位",
                status_bar_item_label(item)
            ),
            Err(error) => format!("状态栏顺序已更新，但保存失败: {error}"),
        });
    }

    fn move_status_bar_item_to(&mut self, ctx: &AppContext, target_index: usize) {
        let item = StatusBarItem::ALL[self.selected_status_item % StatusBarItem::ALL.len()];
        let Some(&target) = StatusBarItem::ALL.get(target_index) else {
            return;
        };
        if item == target {
            return;
        }

        let (position, result) = {
            let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
            if !config.ui.status_bar_items.contains(&item) {
                self.status_msg = Some("请先启用这个状态栏字段".to_string());
                return;
            }
            let Some(position) =
                reorder_status_bar_items(&mut config.ui.status_bar_items, item, target)
            else {
                // Disabled fields have no display-order position to drop on.
                return;
            };
            let result = crate::config::loader::save(&config, &ctx.config_path);
            (position + 1, result)
        };
        self.status_msg = Some(match result {
            Ok(()) => format!(
                "状态栏“{}”已移到第 {position} 位",
                status_bar_item_label(item)
            ),
            Err(error) => format!("状态栏顺序已更新，但保存失败: {error}"),
        });
    }

    fn update_config(
        &mut self,
        ctx: &AppContext,
        update: impl FnOnce(&mut lx_core::model::config::Config),
    ) {
        let result = {
            let mut config = ctx.config.write().unwrap_or_else(|e| e.into_inner());
            update(&mut config);
            // 下载目录/分片等参数变化后立即生效，无需重启。
            ctx.downloads.sync_config(&config);
            crate::config::loader::save(&config, &ctx.config_path)
        };
        self.status_msg = Some(match result {
            Ok(()) => "设置已保存".to_string(),
            Err(error) => format!("保存设置失败: {}", error),
        });
    }

    /// 扫码登录选择器浮层。
    fn render_login_picker(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        let sources = ctx
            .source_manager
            .sources_supporting(|capabilities| capabilities.qr_login);
        if sources.is_empty() {
            return;
        }
        let accent = crate::theme::accent(ctx);
        let muted = crate::theme::muted(ctx);
        let text = crate::theme::text(ctx);
        let width = area.width.saturating_sub(6).clamp(24, 44);
        let height = (sources.len() as u16 + 4).min(area.height.saturating_sub(2));
        let popup = Rect::new(
            area.x + area.width.saturating_sub(width) / 2,
            area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        );
        Clear.render(popup, buf);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(accent))
            .title(" 扫码登录 · Enter 登录/退出 · x 退出 · Esc 关闭 ")
            .style(Style::new().bg(crate::theme::mantle(ctx)));
        let inner = block.inner(popup);
        block.render(popup, buf);

        let selected = self.login_picker.unwrap_or(0).min(sources.len() - 1);
        let lines = sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                let logged_in = ctx.source_manager.is_logged_in(*source);
                let marker = if index == selected { "▶ " } else { "  " };
                Line::from(vec![
                    Span::styled(
                        marker,
                        Style::new().fg(if index == selected { accent } else { muted }),
                    ),
                    Span::styled(source.display_name().to_string(), Style::new().fg(text)),
                    Span::styled(
                        if logged_in {
                            "  已登录"
                        } else {
                            "  未登录"
                        },
                        Style::new().fg(if logged_in {
                            crate::theme::green(ctx)
                        } else {
                            muted
                        }),
                    ),
                ])
            })
            .collect::<Vec<_>>();
        Paragraph::new(lines).render(inner, buf);
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &AppContext) {
        let config = ctx.config.read().unwrap_or_else(|e| e.into_inner());
        let sources = &config.source.js_sources;
        let local_paths = &config.local_music.paths;
        let accent = crate::theme::accent(ctx);
        let muted = crate::theme::muted(ctx);
        let chunks = settings_chunks(area, self.focus, self.category);
        let proxy_label = if config.network.proxy_url.is_empty() {
            "未设置".to_string()
        } else {
            shorten_source(&config.network.proxy_url, 18)
        };
        let scan_depth_label = if config.local_music.max_depth == 0 {
            "不限".to_string()
        } else {
            config.local_music.max_depth.to_string()
        };
        let ab_loop = ctx.player.ab_loop();
        let ab_start_label = ab_loop
            .map(|points| format_duration(points.start))
            .unwrap_or_else(|| "未设置".to_string());
        let ab_end_label = ab_loop
            .map(|points| format_duration(points.end))
            .unwrap_or_else(|| "未设置".to_string());

        let options_block = super::components::chrome::focused_card(
            ctx,
            format!(" 设置  ·  {}  ·  ←/→ 分类 ", self.category.label()),
        );
        let options_inner = options_block.inner(chunks[0]);
        options_block.render(chunks[0], buf);
        let options = vec![
            setting_line("鼠标控制", config.ui.enable_mouse, "t", accent, muted),
            setting_line("聚合搜索", config.ui.aggregate_search, "g", accent, muted),
            setting_line("循环导航", config.ui.wrap_navigation, "w", accent, muted),
            setting_line("封面显示", config.ui.show_cover, "c", accent, muted),
            setting_line(
                "保留播放状态",
                config.player.remember_playback_state,
                "e",
                accent,
                muted,
            ),
            setting_value_line(
                "播放音质",
                config.player.quality.label(),
                "Q",
                accent,
                muted,
            ),
            setting_value_line(
                "播放速度",
                &format!("{:.2}x", config.player.playback_speed),
                settings_binding(
                    &config.keybindings,
                    Action::SettingsCyclePlaybackSpeed,
                    "F1",
                ),
                accent,
                muted,
            ),
            setting_value_line(
                "音频设备",
                &config.player.audio_device,
                settings_binding(&config.keybindings, Action::SettingsEditAudioDevice, "F2"),
                accent,
                muted,
            ),
            setting_value_line(
                "ReplayGain",
                &config.player.replaygain_mode,
                settings_binding(
                    &config.keybindings,
                    Action::SettingsCycleReplayGainMode,
                    "F3",
                ),
                accent,
                muted,
            ),
            setting_value_line(
                "RG 预放大",
                &format!("{:+.1} dB", config.player.replaygain_preamp),
                settings_binding(
                    &config.keybindings,
                    Action::SettingsCycleReplayGainPreamp,
                    "F5",
                ),
                accent,
                muted,
            ),
            setting_value_line(
                "声道模式",
                &config.player.channel_mode,
                settings_binding(&config.keybindings, Action::SettingsCycleChannelMode, "F4"),
                accent,
                muted,
            ),
            setting_value_line(
                "左右平衡",
                &format!("{:+.2}", config.player.balance),
                settings_binding(&config.keybindings, Action::SettingsCycleBalance, "F6"),
                accent,
                muted,
            ),
            setting_line(
                "ReplayGain 削波保护",
                config.player.replaygain_clip,
                settings_binding(
                    &config.keybindings,
                    Action::SettingsToggleReplayGainClip,
                    "F7",
                ),
                accent,
                muted,
            ),
            setting_value_line(
                "淡入时长",
                &fade_label(config.player.fade_in_ms),
                settings_binding(
                    &config.keybindings,
                    Action::SettingsCycleFadeInDuration,
                    "F8",
                ),
                accent,
                muted,
            ),
            setting_value_line(
                "淡出时长",
                &fade_label(config.player.fade_out_ms),
                settings_binding(
                    &config.keybindings,
                    Action::SettingsCycleFadeOutDuration,
                    "F9",
                ),
                accent,
                muted,
            ),
            setting_value_line(
                "均衡器",
                crate::context::equalizer_label(&config.player.equalizer_bands),
                settings_binding(
                    &config.keybindings,
                    Action::SettingsCycleEqualizerPreset,
                    "F10",
                ),
                accent,
                muted,
            ),
            setting_value_line(
                "淡入当前歌曲",
                "执行",
                settings_binding(&config.keybindings, Action::SettingsRunFadeIn, "Shift+F1"),
                accent,
                muted,
            ),
            setting_value_line(
                "淡出当前歌曲",
                "执行",
                settings_binding(&config.keybindings, Action::SettingsRunFadeOut, "Shift+F2"),
                accent,
                muted,
            ),
            setting_value_line(
                "A-B 循环起点",
                &ab_start_label,
                settings_binding(
                    &config.keybindings,
                    Action::SettingsSetAbLoopStart,
                    "Shift+F3",
                ),
                accent,
                muted,
            ),
            setting_value_line(
                "A-B 循环终点",
                &ab_end_label,
                settings_binding(
                    &config.keybindings,
                    Action::SettingsSetAbLoopEnd,
                    "Shift+F4",
                ),
                accent,
                muted,
            ),
            setting_value_line(
                "清除 A-B",
                if ab_loop.is_some() {
                    "执行"
                } else {
                    "未设置"
                },
                settings_binding(&config.keybindings, Action::SettingsClearAbLoop, "Shift+F5"),
                accent,
                muted,
            ),
            setting_value_line("播放模式", ctx.playlist.mode().label(), "m", accent, muted),
            setting_value_line(
                "历史上限",
                &config.player.history_limit.to_string(),
                "H",
                accent,
                muted,
            ),
            setting_value_line(
                "默认音源",
                config.source.default.as_str(),
                "v",
                accent,
                muted,
            ),
            setting_line("自动换源", config.source.auto_toggle, "u", accent, muted),
            {
                let source = SourceId::all_online()
                    [self.enabled_source_index % SourceId::all_online().len()];
                setting_value_line(
                    "音源开关",
                    &format!(
                        "{} {}",
                        source.as_str(),
                        enabled(config.source.enabled.contains(&source))
                    ),
                    "y/K",
                    accent,
                    muted,
                )
            },
            setting_line(
                "歌词翻译",
                config.lyric.show_translation,
                "T",
                accent,
                muted,
            ),
            setting_line("逐字歌词", config.lyric.show_yrc, "Y", accent, muted),
            setting_value_line(
                "歌词偏移",
                &format!("{:+} ms", config.lyric.offset),
                "[/]",
                accent,
                muted,
            ),
            setting_value_line("网络代理", &proxy_label, "n", accent, muted),
            setting_value_line(
                "网络超时",
                &format!("{} 秒", config.network.timeout),
                "N",
                accent,
                muted,
            ),
            setting_value_line("封面协议", &config.ui.cover_protocol, "P", accent, muted),
            setting_value_line(
                "最大 FPS",
                &config.ui.max_fps.to_string(),
                "f",
                accent,
                muted,
            ),
            setting_value_line(
                "滚动步长",
                &config.ui.scroll_amount.to_string(),
                "z",
                accent,
                muted,
            ),
            setting_line("MPRIS", config.integration.mpris, "i", accent, muted),
            setting_value_line(
                "TUI 通知",
                &format!(
                    "{} · {} 秒",
                    enabled(config.notification.in_app),
                    config.notification.in_app_timeout.clamp(1, 60)
                ),
                "o/O",
                accent,
                muted,
            ),
            setting_line("桌面通知", config.notification.enable, "x", accent, muted),
            setting_line(
                "通知封面",
                config.notification.album_cover,
                "X",
                accent,
                muted,
            ),
            setting_line(
                "切歌通知",
                config.notification.track_change,
                "R",
                accent,
                muted,
            ),
            setting_value_line("主题强调色", &config.theme.accent, "p", accent, muted),
            setting_value_line("扫描深度", &scan_depth_label, "D", accent, muted),
            setting_value_line(
                "导出数据",
                "voicefox-export.json",
                settings_binding(&config.keybindings, Action::SettingsExportData, "Shift+F6"),
                accent,
                muted,
            ),
            setting_value_line(
                "导入数据",
                "voicefox-export.json",
                settings_binding(&config.keybindings, Action::SettingsImportData, "Shift+F7"),
                accent,
                muted,
            ),
            setting_value_line(
                "导入外部歌单",
                "M3U/JSON",
                settings_binding(
                    &config.keybindings,
                    Action::SettingsImportPlaylist,
                    "Shift+F8",
                ),
                accent,
                muted,
            ),
            {
                // 支持扫码的音源可能不止一个，这里汇总登录状态，
                // 具体登录/退出在 `b` 打开的选择器里完成。
                let sources = ctx
                    .source_manager
                    .sources_supporting(|capabilities| capabilities.qr_login);
                let logged_in = sources
                    .iter()
                    .filter(|source| ctx.source_manager.is_logged_in(**source))
                    .map(|source| source.display_name())
                    .collect::<Vec<_>>();
                let value = if sources.is_empty() {
                    "无可登录音源".to_string()
                } else if logged_in.is_empty() {
                    "未登录".to_string()
                } else {
                    logged_in.join("、")
                };
                setting_row(
                    "扫码登录",
                    Span::styled(
                        value,
                        Style::new().fg(if logged_in.is_empty() { muted } else { accent }),
                    ),
                    "b",
                    muted,
                )
            },
            // --- 下载（索引 45 起，与 SETTING_OPTION_KEYS 保持一致）---
            setting_value_line(
                "下载目录",
                &shorten_source(&ctx.downloads.download_dir().display().to_string(), 22),
                "S",
                accent,
                muted,
            ),
            setting_value_line(
                "下载音质",
                &config
                    .download
                    .quality
                    .map(|quality| quality.label().to_string())
                    .unwrap_or_else(|| format!("跟随播放 ({})", config.player.quality.label())),
                "F",
                accent,
                muted,
            ),
            setting_value_line(
                "文件名模板",
                &shorten_source(&config.download.filename_template, 22),
                "M",
                accent,
                muted,
            ),
            setting_line("多线程分片", config.download.multipart, "B", accent, muted),
            setting_value_line(
                "分片阈值",
                &format!("{} MB", config.download.multipart_min_size_mb),
                "V",
                accent,
                muted,
            ),
            setting_value_line(
                "分片并发",
                &config.download.concurrency.to_string(),
                "W",
                accent,
                muted,
            ),
            setting_value_line(
                "同时下载",
                &format!("{} 首", config.download.concurrent_songs),
                "A",
                accent,
                muted,
            ),
            setting_value_line(
                "失败重试",
                &format!("{} 次", config.download.max_retries),
                "E",
                accent,
                muted,
            ),
            setting_line(
                "校验文件大小",
                config.download.verify_size,
                "U",
                accent,
                muted,
            ),
            setting_line(
                "跳过已下载",
                config.download.skip_existing,
                "L",
                accent,
                muted,
            ),
            setting_line("写入标签", config.download.write_tags, "J", accent, muted),
            setting_line("嵌入封面", config.download.embed_cover, "G", accent, muted),
            setting_line("保存歌词", config.download.save_lyric, "I", accent, muted),
            setting_line(
                "WebDAV 同步",
                config.download.webdav.enabled && !config.download.webdav.url.trim().is_empty(),
                "Z",
                accent,
                muted,
            ),
            setting_value_line(
                "WebDAV 地址",
                &if config.download.webdav.url.trim().is_empty() {
                    "未设置".to_string()
                } else {
                    shorten_source(&config.download.webdav.display_url(), 22)
                },
                "C",
                accent,
                muted,
            ),
            setting_value_line(
                "播放自动缓存",
                &if config.download.auto_cache_on_play {
                    format!("播放 {} 秒后入库", config.download.auto_cache_after_secs)
                } else {
                    "关闭（config.toml）".to_string()
                },
                "",
                accent,
                muted,
            ),
        ];
        let option_indices = self.category.option_indices();
        let options = options
            .into_iter()
            .enumerate()
            .filter_map(|(index, line)| option_indices.contains(&index).then_some(line))
            .collect();
        // 选项个数必须与鼠标点击的键位表长度一致，否则新增设置项后点击会错位。
        debug_assert_eq!(
            SETTING_OPTION_KEYS.len(),
            SETTING_OPTION_ACTIONS.len(),
            "设置项键位表长度必须一致"
        );
        render_setting_options(options, options_inner, buf);

        let source_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(if self.focus == SettingsFocus::JsSources {
                accent
            } else {
                crate::theme::border(ctx)
            }))
            .title(" JS 音源 [s/a/d/h] ");
        let source_inner = source_block.inner(chunks[1]);
        source_block.render(chunks[1], buf);
        if source_inner.height > 0 {
            let loaded_sources = ctx.source_manager.js_source_count();
            let source_state = if loaded_sources > 0 {
                (
                    format!("{loaded_sources} 个音源已就绪，按列表顺序解析"),
                    crate::theme::green(ctx),
                )
            } else if sources.is_empty() {
                ("尚未导入 JS 音源".to_string(), crate::theme::yellow(ctx))
            } else {
                ("加载中或加载失败".to_string(), crate::theme::yellow(ctx))
            };
            // Keep command labels at a stable left-hand position so the
            // mouse hit targets match what is rendered even when the source
            // status text changes length.
            Paragraph::new(Line::from(vec![
                Span::styled(" [a] 添加  [d] 删除  [h] 检测  ", Style::new().fg(muted)),
                Span::styled(source_state.0, Style::new().fg(source_state.1)),
            ]))
            .render(
                Rect::new(source_inner.x, source_inner.y, source_inner.width, 1),
                buf,
            );
            let checking = ctx
                .source_health_checking
                .load(std::sync::atomic::Ordering::Relaxed);
            let health_line = {
                let health = ctx.source_health.read().unwrap_or_else(|e| e.into_inner());
                if checking {
                    " 音源检测中…".to_string()
                } else if health.is_empty() {
                    " 尚未检测音源".to_string()
                } else {
                    let summary = health
                        .iter()
                        .map(|item| {
                            format!(
                                "{} {}{}ms",
                                item.name,
                                if item.ok { "✓" } else { "✗" },
                                item.latency_ms
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("  ");
                    format!(" 检测结果: {summary}")
                }
            };
            Paragraph::new(Line::from(Span::styled(
                truncate_display(&health_line, source_inner.width as usize),
                Style::new().fg(if checking {
                    crate::theme::yellow(ctx)
                } else {
                    muted
                }),
            )))
            .render(
                Rect::new(source_inner.x, source_inner.y + 1, source_inner.width, 1),
                buf,
            );
        }

        // Reserve one row for the command hint and one for the status message.
        let source_rows = source_inner.height.saturating_sub(3) as usize;
        if sources.is_empty() {
            if source_inner.height > 3 {
                Paragraph::new(" (无)")
                    .style(Style::new().fg(muted))
                    .render(
                        Rect::new(source_inner.x, source_inner.y + 2, source_inner.width, 1),
                        buf,
                    );
            }
        } else {
            self.selected_source = self.selected_source.min(sources.len().saturating_sub(1));
            let source_start = list_window_start(self.selected_source, sources.len(), source_rows);
            let max_url_chars = source_inner.width.saturating_sub(28) as usize;
            for (row, (index, url)) in sources
                .iter()
                .enumerate()
                .skip(source_start)
                .take(source_rows)
                .enumerate()
            {
                let cached = is_source_cached(url);
                let status = if cached { "cached" } else { "download" };
                let name = ctx
                    .source_manager
                    .js_source_name_for_origin(url)
                    .unwrap_or_else(|| "未加载".to_string());
                let style = if index == self.selected_source {
                    Style::new()
                        .fg(crate::theme::selection_fg(ctx))
                        .bg(accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(crate::theme::text(ctx))
                };
                let text = format!(
                    " {:<8} {} {}",
                    status,
                    truncate_display(&name, 12),
                    shorten_source(url, max_url_chars.max(8))
                );
                Paragraph::new(Line::from(Span::styled(text, style))).render(
                    Rect::new(
                        source_inner.x,
                        source_inner.y + 2 + row as u16,
                        source_inner.width,
                        1,
                    ),
                    buf,
                );
            }
        }

        // ── 本地音乐目录列表 ──
        let local_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(if self.focus == SettingsFocus::LocalPaths {
                accent
            } else {
                crate::theme::border(ctx)
            }))
            .title(" 本地目录 [s/a/d/r] ");
        let local_inner = local_block.inner(chunks[2]);
        local_block.render(chunks[2], buf);

        if local_inner.height > 1 {
            Paragraph::new(Line::from(Span::styled(
                " [a] 添加目录  [d] 移除  [r] 重新扫描",
                Style::new().fg(muted),
            )))
            .render(
                Rect::new(local_inner.x, local_inner.y, local_inner.width, 1),
                buf,
            );
        }

        // 只为显示“共 N 首”，取计数即可，避免每帧全量克隆本地曲库
        let local_song_count = ctx.source_manager.local_source().song_count();
        // Reserve one row for commands and the final row for the status/count
        // footer so list entries are never overwritten by those lines.
        let local_rows = local_inner.height.saturating_sub(3) as usize;
        if local_paths.is_empty() {
            if local_inner.height > 3 {
                Paragraph::new(Line::from(Span::styled(
                    " (无，按 a 添加音乐目录)",
                    Style::new().fg(crate::theme::yellow(ctx)),
                )))
                .render(
                    Rect::new(local_inner.x, local_inner.y + 2, local_inner.width, 1),
                    buf,
                );
            }
        } else {
            self.selected_local_path = self
                .selected_local_path
                .min(local_paths.len().saturating_sub(1));
            let local_start =
                list_window_start(self.selected_local_path, local_paths.len(), local_rows);
            for (row, (index, path)) in local_paths
                .iter()
                .enumerate()
                .skip(local_start)
                .take(local_rows)
                .enumerate()
            {
                let style = if index == self.selected_local_path {
                    Style::new()
                        .fg(crate::theme::selection_fg(ctx))
                        .bg(accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(crate::theme::text(ctx))
                };
                Paragraph::new(Line::from(Span::styled(format!(" {}", path), style))).render(
                    Rect::new(
                        local_inner.x,
                        local_inner.y + 2 + row as u16,
                        local_inner.width,
                        1,
                    ),
                    buf,
                );
            }
        }
        // 底部显示歌曲数
        if local_inner.height > 1 {
            let footer_y = local_inner.bottom().saturating_sub(1);
            if footer_y > local_inner.y {
                Paragraph::new(Line::from(Span::styled(
                    format!(" 共 {} 首歌曲", local_song_count),
                    Style::new().fg(muted),
                )))
                .render(
                    Rect::new(local_inner.x, footer_y, local_inner.width, 1),
                    buf,
                );
            }
        }

        // ── 状态栏字段 ──
        let status_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(if self.focus == SettingsFocus::StatusBar {
                accent
            } else {
                crate::theme::border(ctx)
            }))
            .title(" 状态栏 [s/Space/Shift+方向键] ");
        let status_inner = status_block.inner(chunks[3]);
        status_block.render(chunks[3], buf);
        let status_rows = status_inner.height.saturating_sub(1) as usize;
        self.selected_status_item = self
            .selected_status_item
            .min(StatusBarItem::ALL.len().saturating_sub(1));
        if self.selected_status_item < self.status_item_scroll {
            self.status_item_scroll = self.selected_status_item;
        } else if status_rows > 0
            && self.selected_status_item >= self.status_item_scroll + status_rows
        {
            self.status_item_scroll = self.selected_status_item + 1 - status_rows;
        }
        self.status_item_scroll = self
            .status_item_scroll
            .min(StatusBarItem::ALL.len().saturating_sub(status_rows.max(1)));

        for (row, (index, item)) in StatusBarItem::ALL
            .iter()
            .enumerate()
            .skip(self.status_item_scroll)
            .take(status_rows)
            .enumerate()
        {
            let order = config
                .ui
                .status_bar_items
                .iter()
                .position(|candidate| candidate == item)
                .map(|position| position + 1);
            let selected = index == self.selected_status_item;
            let style = if selected {
                Style::new()
                    .fg(crate::theme::selection_fg(ctx))
                    .bg(accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(crate::theme::text(ctx))
            };
            let text = format!(
                " [{}] {:>2}  {}",
                if order.is_some() { "x" } else { " " },
                order.map_or_else(|| "-".to_string(), |value| value.to_string()),
                status_bar_item_label(*item)
            );
            Paragraph::new(Line::from(Span::styled(text, style))).render(
                Rect::new(
                    status_inner.x,
                    status_inner.y + row as u16,
                    status_inner.width,
                    1,
                ),
                buf,
            );
        }

        let focused_inner = match self.focus {
            SettingsFocus::JsSources => source_inner,
            SettingsFocus::LocalPaths => local_inner,
            SettingsFocus::StatusBar => status_inner,
        };
        if let Some(ref msg) = self.status_msg
            && focused_inner.height > 1
        {
            Paragraph::new(Line::from(Span::styled(
                format!(" {}", msg),
                Style::new().fg(crate::theme::yellow(ctx)),
            )))
            .render(
                Rect::new(
                    focused_inner.x,
                    focused_inner.bottom().saturating_sub(1),
                    focused_inner.width,
                    1,
                ),
                buf,
            );
        }

        // ── 本地音乐路径输入弹窗 ──
        if self.local_path_mode {
            let width = area.width.saturating_sub(4).min(74);
            let input_area = Rect::new(
                area.x + area.width.saturating_sub(width) / 2,
                area.y + area.height.saturating_sub(3) / 2,
                width,
                3.min(area.height),
            );
            Clear.render(input_area, buf);
            let input_block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(crate::theme::green(ctx)))
                .title("输入本地音乐目录路径");
            let inner = input_block.inner(input_area);
            input_block.render(input_area, buf);
            let cursor = if (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
                / 500)
                .is_multiple_of(2)
            {
                "█"
            } else {
                " "
            };
            Paragraph::new(Line::from(format!("{}{}", self.local_path_input, cursor)))
                .render(inner, buf);
        }

        if self.login_picker.is_some() {
            self.render_login_picker(area, buf, ctx);
        }

        if self.input_mode {
            let width = area.width.saturating_sub(4).min(74);
            let input_area = Rect::new(
                area.x + area.width.saturating_sub(width) / 2,
                area.y + area.height.saturating_sub(3) / 2,
                width,
                3.min(area.height),
            );
            Clear.render(input_area, buf);
            let input_block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(crate::theme::green(ctx)))
                .title("输入 JS 音源 URL 或本地路径");

            let inner = input_block.inner(input_area);
            input_block.render(input_area, buf);
            let cursor = if (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
                / 500)
                .is_multiple_of(2)
            {
                "█"
            } else {
                " "
            };

            Paragraph::new(Line::from(format!("{}{}", self.input_url, cursor))).render(inner, buf);
        }

        if self.proxy_input_mode {
            let width = area.width.saturating_sub(4).min(74);
            let input_area = Rect::new(
                area.x + area.width.saturating_sub(width) / 2,
                area.y + area.height.saturating_sub(3) / 2,
                width,
                3.min(area.height),
            );
            Clear.render(input_area, buf);
            let input_block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(crate::theme::green(ctx)))
                .title("输入代理地址，留空表示关闭");
            let inner = input_block.inner(input_area);
            input_block.render(input_area, buf);
            Paragraph::new(Line::from(self.proxy_input.as_str())).render(inner, buf);
        }

        if self.audio_device_input_mode {
            let width = area.width.saturating_sub(4).min(74);
            let input_area = Rect::new(
                area.x + area.width.saturating_sub(width) / 2,
                area.y + area.height.saturating_sub(3) / 2,
                width,
                3.min(area.height),
            );
            Clear.render(input_area, buf);
            let input_block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(crate::theme::green(ctx)))
                .title("输入 libmpv 音频设备名，Enter 保存");
            let inner = input_block.inner(input_area);
            input_block.render(input_area, buf);
            Paragraph::new(Line::from(self.audio_device_input.as_str())).render(inner, buf);
        }

        if self.playlist_import_mode {
            let width = area.width.saturating_sub(4).min(74);
            let input_area = Rect::new(
                area.x + area.width.saturating_sub(width) / 2,
                area.y + area.height.saturating_sub(3) / 2,
                width,
                3.min(area.height),
            );
            Clear.render(input_area, buf);
            let input_block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(crate::theme::green(ctx)))
                .title("输入 M3U/LX Music/网易云歌单路径，Enter 导入");
            let inner = input_block.inner(input_area);
            input_block.render(input_area, buf);
            Paragraph::new(Line::from(self.playlist_import_input.as_str())).render(inner, buf);
        }
    }

    pub fn handle_mouse(
        &mut self,
        event: MouseEvent,
        area: Rect,
        ctx: &AppContext,
        resolver: &KeybindingResolver,
    ) -> AppAction {
        if self.any_input_active() {
            return AppAction::None;
        }
        let chunks = settings_chunks(area, self.focus, self.category);
        let position = Position::new(event.column, event.row);
        match event.kind {
            MouseEventKind::ScrollUp => {
                if chunks[3].contains(position) {
                    self.selected_status_item = self.selected_status_item.saturating_sub(1);
                    self.focus = SettingsFocus::StatusBar;
                } else if chunks[2].contains(position) {
                    self.selected_local_path = self.selected_local_path.saturating_sub(1);
                    self.focus = SettingsFocus::LocalPaths;
                } else if chunks[1].contains(position) {
                    self.selected_source = self.selected_source.saturating_sub(1);
                    self.focus = SettingsFocus::JsSources;
                }
            }
            MouseEventKind::ScrollDown => {
                if chunks[3].contains(position) {
                    self.selected_status_item = (self.selected_status_item + 1)
                        .min(StatusBarItem::ALL.len().saturating_sub(1));
                    self.focus = SettingsFocus::StatusBar;
                } else if chunks[2].contains(position) {
                    let len = ctx
                        .config
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .local_music
                        .paths
                        .len();
                    self.selected_local_path =
                        (self.selected_local_path + 1).min(len.saturating_sub(1));
                    self.focus = SettingsFocus::LocalPaths;
                } else if chunks[1].contains(position) {
                    let len = ctx
                        .config
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .source
                        .js_sources
                        .len();
                    self.selected_source = (self.selected_source + 1).min(len.saturating_sub(1));
                    self.focus = SettingsFocus::JsSources;
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let status_inner = Block::default().borders(Borders::ALL).inner(chunks[3]);
                if let Some(index) = status_item_at(status_inner, position, self.status_item_scroll)
                {
                    self.focus = SettingsFocus::StatusBar;
                    if self.status_drag_target.is_some() && self.status_drag_target != Some(index) {
                        self.move_status_bar_item_to(ctx, index);
                        self.status_drag_target = Some(index);
                    }
                }
            }
            MouseEventKind::Down(button)
                if matches!(button, MouseButton::Left | MouseButton::Right) =>
            {
                let right_click = button == MouseButton::Right;
                self.status_drag_target = None;
                if !right_click && area.width < ALL_MANAGEMENT_PANELS_MIN_WIDTH {
                    let focused_panel = chunks[match self.focus {
                        SettingsFocus::JsSources => 1,
                        SettingsFocus::LocalPaths => 2,
                        SettingsFocus::StatusBar => 3,
                    }];
                    // Narrow layouts show only one management panel. Its
                    // title already advertises `[s]`; clicking that title
                    // provides the equivalent mouse-only way to cycle panels.
                    if focused_panel.contains(position) && position.y == focused_panel.y {
                        self.focus = self.focus.next();
                        return AppAction::None;
                    }
                }
                if !right_click {
                    let options_inner = Block::default().borders(Borders::ALL).inner(chunks[0]);
                    if options_inner.contains(position) {
                        let visible_index = setting_option_index(options_inner, position);
                        let option_index = self
                            .category
                            .option_indices()
                            .get(visible_index as usize)
                            .copied();
                        if let Some(Some(action)) =
                            option_index.and_then(|index| SETTING_OPTION_ACTIONS.get(index))
                            && let Some(result) = self.handle_bound_action(*action, ctx)
                        {
                            return result;
                        }
                        if let Some(&key) =
                            option_index.and_then(|index| SETTING_OPTION_KEYS.get(index))
                            && key != '\0'
                        {
                            return self.handle_input(
                                KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
                                ctx,
                                resolver,
                            );
                        }
                    }
                }

                let source_inner = Block::default().borders(Borders::ALL).inner(chunks[1]);
                if chunks[1].contains(position) {
                    self.focus = SettingsFocus::JsSources;
                    let command_y = source_inner.y;
                    if event.row == command_y {
                        let key = command_key_at(
                            source_inner,
                            position,
                            &[("[a] 添加", 'a'), ("[d] 删除", 'd'), ("[h] 检测", 'h')],
                        );
                        if let Some(key) = key {
                            return self.handle_input(
                                KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
                                ctx,
                                resolver,
                            );
                        }
                    }
                    let rows = source_inner.height.saturating_sub(3) as usize;
                    if let Some(row) = list_row_at(source_inner, position, rows) {
                        let len = ctx
                            .config
                            .read()
                            .unwrap_or_else(|e| e.into_inner())
                            .source
                            .js_sources
                            .len();
                        let start = list_window_start(self.selected_source, len, rows);
                        let index = start + row;
                        if index < len {
                            self.selected_source = index;
                            if right_click {
                                return self.handle_input(
                                    KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
                                    ctx,
                                    resolver,
                                );
                            }
                        }
                    }
                    return AppAction::None;
                }

                let local_inner = Block::default().borders(Borders::ALL).inner(chunks[2]);
                if chunks[2].contains(position) {
                    self.focus = SettingsFocus::LocalPaths;
                    let command_y = local_inner.y;
                    if event.row == command_y {
                        let key = command_key_at(
                            local_inner,
                            position,
                            &[
                                ("[a] 添加目录", 'a'),
                                ("[d] 移除", 'd'),
                                ("[r] 重新扫描", 'r'),
                            ],
                        );
                        if let Some(key) = key {
                            return self.handle_input(
                                KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
                                ctx,
                                resolver,
                            );
                        }
                    }
                    let rows = local_inner.height.saturating_sub(3) as usize;
                    if let Some(row) = list_row_at(local_inner, position, rows) {
                        let len = ctx
                            .config
                            .read()
                            .unwrap_or_else(|e| e.into_inner())
                            .local_music
                            .paths
                            .len();
                        let start = list_window_start(self.selected_local_path, len, rows);
                        let index = start + row;
                        if index < len {
                            self.selected_local_path = index;
                            if right_click {
                                return self.handle_input(
                                    KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
                                    ctx,
                                    resolver,
                                );
                            }
                        }
                    }
                    return AppAction::None;
                }

                let status_inner = Block::default().borders(Borders::ALL).inner(chunks[3]);
                if let Some(index) = status_item_at(status_inner, position, self.status_item_scroll)
                {
                    self.selected_status_item = index;
                    self.focus = SettingsFocus::StatusBar;
                    let checkbox = event.column < status_inner.x.saturating_add(5);
                    if !right_click && !checkbox {
                        self.status_drag_target = Some(index);
                    }
                    if right_click || checkbox {
                        self.toggle_status_bar_item(ctx);
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.status_drag_target = None;
            }
            _ => {}
        }
        AppAction::None
    }
}

fn list_window_start(selected: usize, len: usize, rows: usize) -> usize {
    if len == 0 || rows == 0 {
        0
    } else {
        selected.min(len - 1).saturating_sub(rows.saturating_sub(1))
    }
}

/// Return the command key under a mouse position on a panel's command row.
///
/// The labels are rendered from the panel's left edge with two spaces between
/// commands. Keeping this calculation in one place prevents the hit regions
/// from drifting when labels are changed or localized.
fn command_key_at(area: Rect, position: Position, commands: &[(&str, char)]) -> Option<char> {
    if position.y != area.y || position.x < area.x || position.x >= area.right() {
        return None;
    }

    let mut x = area.x;
    // The rendered line starts with one leading space before the first label.
    x = x.saturating_add(1);
    for (label, key) in commands {
        let width = UnicodeWidthStr::width(*label) as u16;
        if position.x >= x && position.x < x.saturating_add(width) {
            return Some(*key);
        }
        x = x.saturating_add(width).saturating_add(2);
    }
    None
}

/// Return the zero-based visible row for a source/directory list item.
/// The first two inner rows are reserved for the panel title/commands and the
/// final row is reserved for the status/count footer.
fn list_row_at(area: Rect, position: Position, rows: usize) -> Option<usize> {
    if rows == 0
        || !area.contains(position)
        || position.y < area.y.saturating_add(2)
        || position.y >= area.y.saturating_add(2 + rows as u16)
    {
        return None;
    }
    Some(position.y.saturating_sub(area.y + 2) as usize)
}

fn status_item_at(area: Rect, position: Position, scroll: usize) -> Option<usize> {
    if !area.contains(position) || position.y >= area.bottom().saturating_sub(1) {
        return None;
    }
    let index = scroll + position.y.saturating_sub(area.y) as usize;
    (index < StatusBarItem::ALL.len()).then_some(index)
}

fn enabled(value: bool) -> &'static str {
    if value { "开启" } else { "关闭" }
}

fn settings_binding<'a>(
    config: &'a KeybindingConfig,
    action: Action,
    fallback: &'a str,
) -> &'a str {
    config
        .pages
        .get("settings")
        .and_then(|bindings| bindings.get(&action))
        .map(String::as_str)
        .unwrap_or(fallback)
}

fn settings_action_is_page_owned(action: Action) -> bool {
    matches!(
        action,
        Action::ListSelectUp
            | Action::ListSelectDown
            | Action::SettingsCyclePlaybackSpeed
            | Action::SettingsEditAudioDevice
            | Action::SettingsCycleReplayGainMode
            | Action::SettingsCycleReplayGainPreamp
            | Action::SettingsCycleChannelMode
            | Action::SettingsCycleBalance
            | Action::SettingsToggleReplayGainClip
            | Action::SettingsCycleFadeInDuration
            | Action::SettingsCycleFadeOutDuration
            | Action::SettingsCycleEqualizerPreset
            | Action::SettingsRunFadeIn
            | Action::SettingsRunFadeOut
            | Action::SettingsSetAbLoopStart
            | Action::SettingsSetAbLoopEnd
            | Action::SettingsClearAbLoop
            | Action::SettingsExportData
            | Action::SettingsImportData
            | Action::SettingsImportPlaylist
    )
}

fn status_bar_item_label(item: StatusBarItem) -> &'static str {
    match item {
        StatusBarItem::State => "播放状态",
        StatusBarItem::Source => "当前音源",
        StatusBarItem::Sort => "页面排序",
        StatusBarItem::Song => "歌曲名称",
        StatusBarItem::Time => "播放时间",
        StatusBarItem::Volume => "音量",
        StatusBarItem::PlayMode => "播放模式",
        StatusBarItem::Quality => "音质",
        StatusBarItem::Queue => "队列位置",
        StatusBarItem::JsSourceState => "JS 音源状态",
    }
}

fn reorder_status_bar_items(
    items: &mut Vec<StatusBarItem>,
    item: StatusBarItem,
    target: StatusBarItem,
) -> Option<usize> {
    let item_position = items.iter().position(|candidate| *candidate == item)?;
    let target_position = items.iter().position(|candidate| *candidate == target)?;
    if item_position == target_position {
        return Some(item_position);
    }
    items.remove(item_position);
    let insertion = target_position.min(items.len());
    items.insert(insertion, item);
    Some(insertion)
}

/// 在更新配置项后更新这些常量!
///
/// 最长按键提示的显示宽度 (组合键在界面中使用 C/S/A 缩写)
const KEY_COLUMN_WIDTH: usize = 7;
/// 最长标签的显示宽度 (当前为 保留播放状态)
const LABEL_COLUMN_WIDTH: usize = 12;

/// 在右侧补空格至指定显示宽度，宽字符按两列计算
fn pad_display(value: &str, width: usize) -> String {
    let padding = width.saturating_sub(UnicodeWidthStr::width(value));
    format!("{}{}", value, " ".repeat(padding))
}

/// 组装一行设置项：按键提示、标签与取值分别占固定宽度的列
fn setting_row(label: &str, value: Span<'static>, key: &str, muted: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!(
                " {} ",
                pad_display(&format!("[{}]", compact_key_label(key)), KEY_COLUMN_WIDTH)
            ),
            Style::new().fg(muted),
        ),
        Span::raw(pad_display(label, LABEL_COLUMN_WIDTH)),
        Span::raw(" "),
        value,
    ])
}

fn compact_key_label(key: &str) -> String {
    key.replace("Ctrl+", "C+")
        .replace("Shift+", "S+")
        .replace("Alt+", "A+")
}

fn setting_line(label: &str, value: bool, key: &str, accent: Color, muted: Color) -> Line<'static> {
    setting_row(
        label,
        Span::styled(
            if value { "[x]" } else { "[ ]" },
            Style::new().fg(if value { accent } else { muted }),
        ),
        key,
        muted,
    )
}

fn setting_value_line(
    label: &str,
    value: &str,
    key: &str,
    accent: Color,
    muted: Color,
) -> Line<'static> {
    setting_row(
        label,
        Span::styled(value.to_string(), Style::new().fg(accent)),
        key,
        muted,
    )
}

fn save_status(result: anyhow::Result<()>) -> Option<String> {
    Some(match result {
        Ok(()) => "设置已保存".to_string(),
        Err(error) => format!("保存设置失败: {error}"),
    })
}

fn next_quality(quality: Quality) -> Quality {
    match quality {
        Quality::Low128 => Quality::High320,
        Quality::High320 => Quality::Flac,
        Quality::Flac => Quality::Flac24,
        Quality::Flac24 => Quality::Low128,
    }
}

/// 在一组候选值里循环取值；当前值不在候选里时回到第一个。
fn next_step(values: &[u64], current: u64) -> u64 {
    match values.iter().position(|value| *value == current) {
        Some(index) => values[(index + 1) % values.len()],
        None => values[0],
    }
}

fn next_history_limit(limit: usize) -> usize {
    match limit {
        0..=25 => 50,
        26..=50 => 100,
        51..=100 => 200,
        101..=200 => 500,
        _ => 25,
    }
}

fn next_network_timeout(timeout: u64) -> u64 {
    match timeout {
        0..=5 => 10,
        6..=10 => 15,
        11..=15 => 30,
        16..=30 => 60,
        _ => 5,
    }
}

fn next_cover_protocol(protocol: &str) -> &'static str {
    match protocol {
        "auto" => "kitty",
        "kitty" => "sixel",
        "sixel" => "iterm2",
        "iterm2" => "halfblocks",
        _ => "auto",
    }
}

fn next_fps(fps: u32) -> u32 {
    match fps {
        0..=10 => 20,
        11..=20 => 30,
        21..=30 => 60,
        _ => 10,
    }
}

fn next_scroll_amount(amount: usize) -> usize {
    match amount {
        0..=1 => 3,
        2..=3 => 5,
        4..=5 => 10,
        _ => 1,
    }
}

fn next_scan_depth(depth: u32) -> u32 {
    match depth {
        0 => 1,
        1 => 2,
        2 => 4,
        3..=4 => 8,
        5..=8 => 16,
        _ => 0,
    }
}

fn next_fade_duration(value: u64) -> u64 {
    match value {
        0 => 250,
        1..=250 => 500,
        251..=500 => 1_000,
        501..=1_000 => 2_000,
        _ => 0,
    }
}

fn fade_label(value: u64) -> String {
    if value == 0 {
        "关闭".to_string()
    } else if value.is_multiple_of(1_000) {
        format!("{} 秒", value / 1_000)
    } else {
        format!("{} ms", value)
    }
}

fn format_duration(value: std::time::Duration) -> String {
    let total = value.as_secs();
    format!("{:02}:{:02}", total / 60, total % 60)
}

/// 在更新配置项后更新这些常量!
///
/// 鼠标点击时触发的按键，顺序必须与 render 中的选项列表一致
const SETTING_OPTION_KEYS: [char; 61] = [
    't', 'g', 'w', 'c', 'e', 'Q', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0',
    '\0', '\0', '\0', '\0', 'm', 'H', 'v', 'u', 'K', 'T', 'Y', ']', 'n', 'N', 'P', 'f', 'z', 'i',
    'o', 'x', 'X', 'R', 'p', 'D', '\0', '\0', '\0', 'b', 'S', 'F', 'M', 'B', 'V', 'W', 'A', 'E',
    'U', 'L', 'J', 'G', 'I', 'Z', 'C', '\0',
];
const SETTING_OPTION_ACTIONS: [Option<Action>; 61] = [
    None,
    None,
    None,
    None,
    None,
    None,
    Some(Action::SettingsCyclePlaybackSpeed),
    Some(Action::SettingsEditAudioDevice),
    Some(Action::SettingsCycleReplayGainMode),
    Some(Action::SettingsCycleReplayGainPreamp),
    Some(Action::SettingsCycleChannelMode),
    Some(Action::SettingsCycleBalance),
    Some(Action::SettingsToggleReplayGainClip),
    Some(Action::SettingsCycleFadeInDuration),
    Some(Action::SettingsCycleFadeOutDuration),
    Some(Action::SettingsCycleEqualizerPreset),
    Some(Action::SettingsRunFadeIn),
    Some(Action::SettingsRunFadeOut),
    Some(Action::SettingsSetAbLoopStart),
    Some(Action::SettingsSetAbLoopEnd),
    Some(Action::SettingsClearAbLoop),
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    Some(Action::SettingsExportData),
    Some(Action::SettingsImportData),
    Some(Action::SettingsImportPlaylist),
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
];
const TWO_COLUMN_OPTIONS_MIN_WIDTH: u16 = 36;
const THREE_COLUMN_OPTIONS_MIN_WIDTH: u16 = 72;
const ALL_MANAGEMENT_PANELS_MIN_WIDTH: u16 = 108;

/// 设置页在非输入模式下响应的字符键：选项键之外还有列表操作键
/// （a 添加 / d 删除 / h 检测 / s 切换焦点 / r 扫描 / y 与 [ 见 `handle_input`）。
/// 列表导航键来自页面级绑定，由 `consumes_key` 查表解析，不列在这里。
const SETTINGS_PAGE_CHAR_KEYS: &[char] = &[
    'a', 'd', 'h', 'r', 's', 'y', '[', 'm', 'Q', 'v', 'p', 'b', 'n', 'o', 'c', 'e', 'f', 'g', 'i',
    't', 'u', 'w', 'x', 'z', 'D', 'H', 'K', 'N', 'O', 'P', 'R', 'T', 'X', 'Y', ']', 'S', 'F', 'M',
    'B', 'V', 'W', 'A', 'E', 'U', 'L', 'J', 'G', 'I', 'Z', 'C',
];

fn render_setting_options<'a>(options: Vec<Line<'a>>, area: Rect, buf: &mut Buffer) {
    let column_count = setting_option_column_count(area.width);
    if column_count == 1 {
        Paragraph::new(options).render(area, buf);
        return;
    }

    let columns = setting_option_columns(area);
    let mut lines = (0..column_count).map(|_| Vec::new()).collect::<Vec<_>>();
    for (index, line) in options.into_iter().enumerate() {
        lines[index % column_count].push(line);
    }
    for (column, lines) in columns.iter().zip(lines) {
        Paragraph::new(lines).render(*column, buf);
    }
}

fn setting_option_index(area: Rect, position: Position) -> u16 {
    let row = position.y.saturating_sub(area.y);
    let column_count = setting_option_column_count(area.width);
    if column_count == 1 {
        return row;
    }
    let columns = setting_option_columns(area);
    let column = columns
        .iter()
        .position(|column| column.contains(position))
        .unwrap_or(0) as u16;
    row.saturating_mul(column_count as u16)
        .saturating_add(column)
}

fn setting_option_column_count(width: u16) -> usize {
    if width >= THREE_COLUMN_OPTIONS_MIN_WIDTH {
        3
    } else if width >= TWO_COLUMN_OPTIONS_MIN_WIDTH {
        2
    } else {
        1
    }
}

fn setting_option_columns(area: Rect) -> std::rc::Rc<[Rect]> {
    let count = setting_option_column_count(area.width);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints((0..count).map(|_| Constraint::Ratio(1, count as u32)))
        .split(area)
}

fn setting_options_height(panel_width: u16, option_count: usize) -> u16 {
    let inner_width = panel_width.saturating_sub(2);
    let columns = setting_option_column_count(inner_width) as u16;
    let rows = (option_count as u16).div_ceil(columns);
    rows.saturating_add(2)
}

fn settings_chunks(area: Rect, focus: SettingsFocus, category: SettingsCategory) -> [Rect; 4] {
    let option_height =
        setting_options_height(area.width, category.option_indices().len()).min(area.height);
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(option_height), Constraint::Min(0)])
        .split(area);

    if area.width >= ALL_MANAGEMENT_PANELS_MIN_WIDTH {
        // 宽屏：选项占满上排，三个管理区域共享下排。
        let bottom = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(34),
                Constraint::Percentage(33),
                Constraint::Percentage(33),
            ])
            .split(vertical[1]);
        [vertical[0], bottom[0], bottom[1], bottom[2]]
    } else {
        // 窄屏只显示当前管理区域，避免列表被分割到无法使用。
        let mut chunks = [
            vertical[0],
            Rect::default(),
            Rect::default(),
            Rect::default(),
        ];
        chunks[match focus {
            SettingsFocus::JsSources => 1,
            SettingsFocus::LocalPaths => 2,
            SettingsFocus::StatusBar => 3,
        }] = vertical[1];
        chunks
    }
}

#[cfg(test)]
mod tests {
    use ratatui::layout::{Position, Rect};
    use ratatui::style::Color;
    use ratatui::widgets::{Block, Borders};
    use unicode_width::UnicodeWidthStr;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use lx_core::keybinding::{Action, KeybindingConfig, KeybindingResolver};
    use lx_core::model::config::StatusBarItem;

    use super::{
        KEY_COLUMN_WIDTH, LABEL_COLUMN_WIDTH, SETTING_OPTION_ACTIONS, SETTING_OPTION_KEYS,
        SettingsCategory, SettingsFocus, SettingsPage, command_key_at, reorder_status_bar_items,
        setting_line, setting_option_index, setting_value_line, settings_chunks, shorten_source,
    };

    /// 各设置项取值统一起始的列号
    const VALUE_COLUMN: usize = 1 + KEY_COLUMN_WIDTH + 1 + LABEL_COLUMN_WIDTH + 1;

    #[test]
    fn every_setting_option_belongs_to_exactly_one_category() {
        assert_eq!(SETTING_OPTION_KEYS.len(), SETTING_OPTION_ACTIONS.len());
        let mut seen = std::collections::BTreeSet::new();
        for category in [
            SettingsCategory::Interface,
            SettingsCategory::Playback,
            SettingsCategory::Sources,
            SettingsCategory::Integration,
            SettingsCategory::Download,
            SettingsCategory::Data,
        ] {
            for index in category.option_indices() {
                assert!(
                    *index < SETTING_OPTION_KEYS.len(),
                    "设置项下标 {index} 超出键位表长度"
                );
                assert!(seen.insert(*index), "设置项下标 {index} 归属了多个分类");
            }
        }
        assert_eq!(
            seen.len(),
            SETTING_OPTION_KEYS.len(),
            "每个设置项都应当出现在某个分类里"
        );
    }

    #[test]
    fn download_category_exposes_every_download_option() {
        let indices = SettingsCategory::Download.option_indices();

        assert_eq!(indices.len(), 16);
        let keys: Vec<char> = indices
            .iter()
            .map(|index| SETTING_OPTION_KEYS[*index])
            .collect();
        assert_eq!(
            keys,
            vec![
                'S', 'F', 'M', 'B', 'V', 'W', 'A', 'E', 'U', 'L', 'J', 'G', 'I', 'Z', 'C', '\0'
            ]
        );
    }

    #[test]
    fn shortens_unicode_source_path_on_character_boundaries() {
        let path = "/home/user/音乐音源/这是一个很长的第三方音源脚本文件名/latest.js";
        let shortened = shorten_source(path, 24);

        assert_eq!(shortened.chars().count(), 24);
        assert!(shortened.ends_with("..."));
    }

    #[test]
    fn setting_rows_align_values_on_a_shared_column() {
        let accent = Color::Reset;
        let muted = Color::Reset;
        let rows = [
            setting_line("MPRIS", true, "i", accent, muted),
            setting_line("保留播放状态", false, "e", accent, muted),
            setting_value_line("最大 FPS", "30", "f", accent, muted),
            setting_value_line("歌词偏移", "+0 ms", "[/]", accent, muted),
            setting_value_line("音源开关", "kw 开启", "k/K", accent, muted),
        ];

        for row in rows {
            let prefix: String = row.spans[..row.spans.len() - 1]
                .iter()
                .map(|span| span.content.as_ref())
                .collect();

            assert_eq!(UnicodeWidthStr::width(prefix.as_str()), VALUE_COLUMN);
        }
    }

    #[test]
    fn settings_page_owns_every_option_key() {
        let resolver = KeybindingResolver::from_config(&KeybindingConfig::default());
        let page = SettingsPage::new();

        for (index, key) in SETTING_OPTION_KEYS.into_iter().enumerate() {
            // 新播放/数据动作由页面级 Action 处理，不能再把它们的旧数字
            // 占位键视为设置页快捷键，否则会遮挡侧边栏的 1-8 切换。
            if SETTING_OPTION_ACTIONS[index].is_some() {
                continue;
            }
            // `\0` 表示这一行只展示状态、没有对应按键（例如「播放自动缓存」）。
            if key == '\0' {
                continue;
            }
            let event = KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE);

            assert!(
                page.consumes_key(&event, &resolver),
                "选项键 {key} 不应被全局快捷键抢先处理"
            );
        }
    }

    #[test]
    fn tab_number_keys_remain_available_on_the_settings_page() {
        let resolver = KeybindingResolver::from_config(&KeybindingConfig::default());
        let page = SettingsPage::new();

        for key in '0'..='9' {
            assert!(!page.consumes_key(
                &KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
                &resolver
            ));
        }
    }

    #[test]
    fn tab_number_keys_remain_reserved_with_custom_settings_bindings() {
        let mut config = KeybindingConfig::default();
        config
            .pages
            .get_mut("settings")
            .unwrap()
            .insert(Action::SettingsCyclePlaybackSpeed, "1".to_string());
        let resolver = KeybindingResolver::from_config(&config);
        let page = SettingsPage::new();

        for key in '1'..='8' {
            assert!(!page.consumes_key(
                &KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
                &resolver
            ));
        }
    }

    #[test]
    fn settings_page_owns_the_configured_list_navigation_keys() {
        let mut config = KeybindingConfig::default();
        config
            .pages
            .get_mut("settings")
            .unwrap()
            .insert(Action::ListSelectUp, "h".to_string());
        let resolver = KeybindingResolver::from_config(&config);
        let page = SettingsPage::new();

        let rebound = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE);
        let released = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE);

        assert!(page.consumes_key(&rebound, &resolver));
        // 'k' 不再是导航键，也不是选项键，应交还给全局
        assert!(!page.consumes_key(&released, &resolver));
    }

    #[test]
    fn settings_page_owns_rebound_playback_action_even_with_ctrl() {
        let mut config = KeybindingConfig::default();
        config
            .pages
            .get_mut("settings")
            .unwrap()
            .insert(Action::SettingsCyclePlaybackSpeed, "Ctrl+1".to_string());
        let resolver = KeybindingResolver::from_config(&config);
        let page = SettingsPage::new();

        let rebound = KeyEvent::new(KeyCode::Char('1'), KeyModifiers::CONTROL);
        let released = KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE);

        assert!(page.consumes_key(&rebound, &resolver));
        assert!(!page.consumes_key(&released, &resolver));
    }

    #[test]
    fn settings_page_leaves_playback_and_navigation_keys_global() {
        let resolver = KeybindingResolver::from_config(&KeybindingConfig::default());
        let page = SettingsPage::new();
        let global = [
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char(','), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('.'), KeyModifiers::NONE),
        ];

        for event in global {
            assert!(
                !page.consumes_key(&event, &resolver),
                "{:?} 不应被设置页独占",
                event.code
            );
        }
    }

    #[test]
    fn status_bar_focus_owns_toggle_and_reorder_keys() {
        let resolver = KeybindingResolver::from_config(&KeybindingConfig::default());
        let mut page = SettingsPage::new();
        let space = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
        let shift_left = KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT);

        assert!(!page.consumes_key(&space, &resolver));
        page.focus = SettingsFocus::StatusBar;
        assert!(page.consumes_key(&space, &resolver));
        assert!(page.consumes_key(&shift_left, &resolver));
    }

    #[test]
    fn narrow_settings_show_only_the_focused_management_panel() {
        let chunks = settings_chunks(
            Rect::new(0, 0, 80, 24),
            SettingsFocus::StatusBar,
            SettingsCategory::Interface,
        );

        assert_eq!(chunks[0].height, 5);
        assert_eq!(chunks[1], Rect::default());
        assert_eq!(chunks[2], Rect::default());
        assert!(chunks[3].height > 0);
        assert_eq!(chunks[3].bottom(), 24);
    }

    #[test]
    fn wide_settings_keep_all_management_panels_visible() {
        let chunks = settings_chunks(
            Rect::new(0, 0, 120, 30),
            SettingsFocus::JsSources,
            SettingsCategory::Interface,
        );

        assert_eq!(chunks[0].height, 5);
        assert_eq!(chunks[1].y, chunks[0].bottom());
        assert_eq!(chunks[2].y, chunks[0].bottom());
        assert_eq!(chunks[3].y, chunks[0].bottom());
        assert_eq!(chunks[1].width + chunks[2].width + chunks[3].width, 120);
    }

    #[test]
    fn setting_mouse_rows_follow_the_two_column_layout() {
        let panel = settings_chunks(
            Rect::new(0, 0, 60, 24),
            SettingsFocus::JsSources,
            SettingsCategory::Interface,
        )[0];
        let inner = Block::default().borders(Borders::ALL).inner(panel);
        let right_column_x = inner.x + inner.width / 2 + 1;

        assert_eq!(
            setting_option_index(inner, Position::new(inner.x, inner.y)),
            0
        );
        assert_eq!(
            setting_option_index(inner, Position::new(right_column_x, inner.y)),
            1
        );
        assert_eq!(
            setting_option_index(inner, Position::new(right_column_x, inner.y + 1)),
            3
        );
    }

    #[test]
    fn setting_mouse_rows_follow_the_three_column_layout() {
        let panel = settings_chunks(
            Rect::new(0, 0, 80, 24),
            SettingsFocus::JsSources,
            SettingsCategory::Interface,
        )[0];
        let inner = Block::default().borders(Borders::ALL).inner(panel);
        let third_column_x = inner.x + inner.width * 5 / 6;

        assert_eq!(
            setting_option_index(inner, Position::new(third_column_x, inner.y)),
            2
        );
        assert_eq!(
            setting_option_index(inner, Position::new(third_column_x, inner.y + 1)),
            5
        );
    }

    #[test]
    fn bottom_panel_command_hit_targets_match_rendered_labels() {
        let area = Rect::new(10, 20, 50, 8);
        let source_commands = [("[a] 添加", 'a'), ("[d] 删除", 'd')];
        // One leading space is part of the rendered command row.
        assert_eq!(
            command_key_at(area, Position::new(area.x + 2, area.y), &source_commands),
            Some('a')
        );
        assert_eq!(
            command_key_at(area, Position::new(area.x + 12, area.y), &source_commands),
            Some('d')
        );
        assert_eq!(
            command_key_at(area, Position::new(area.x, area.y), &source_commands),
            None
        );
        assert_eq!(
            command_key_at(
                area,
                Position::new(area.x + 1, area.y + 1),
                &source_commands
            ),
            None
        );
    }

    #[test]
    fn status_bar_drag_reorders_once_at_the_target_position() {
        let mut items = vec![
            StatusBarItem::State,
            StatusBarItem::Source,
            StatusBarItem::Sort,
            StatusBarItem::Song,
        ];

        assert_eq!(
            reorder_status_bar_items(&mut items, StatusBarItem::State, StatusBarItem::Sort),
            Some(2)
        );
        assert_eq!(
            items,
            vec![
                StatusBarItem::Source,
                StatusBarItem::Sort,
                StatusBarItem::State,
                StatusBarItem::Song,
            ]
        );
        assert_eq!(
            reorder_status_bar_items(&mut items, StatusBarItem::Song, StatusBarItem::Source),
            Some(0)
        );
        assert_eq!(items[0], StatusBarItem::Song);
    }
}
