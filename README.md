# voicefox

> 终端里的音乐播放器 — Rust TUI 版 lx-music-desktop

voicefox 是一个运行在终端中的音乐播放器，使用 Rust 编写，基于 ratatui 构建界面，通过 mpv 播放音频。支持多音源搜索、在线播放、歌词显示、收藏管理等功能。

无需离开终端，也能享受完整的音乐体验。

## 截图

```
┌──────────────────────────────────────────────────────────┐
│ 1 队列 │ 2 搜索 │ 3 排行榜 │ 4 收藏 │ 5 历史 │ 6 设置     │
├──────────────────────────────────────────────────────────┤
│ ┌────────────────────────────────────────────────────┐  │
│ │ ▶ 歌曲名称                                        │  │
│ │ 歌手 - 专辑                                       │  │
│ │ 音源: kw | 音质: 320k                             │  │
│ └────────────────────────────────────────────────────┘  │
│ ┌──────────────┐  ┌──────────────────────────────────┐  │
│ │ 最近播放      │  │ 歌词                             │  │
│ │ 1. 歌曲 A     │  │ [00:12.34] 歌词第一行            │  │
│ │ 2. 歌曲 B     │  │ [00:16.78] 歌词第二行            │  │
│ │ 3. 歌曲 C     │  │ ...                              │  │
│ └──────────────┘  └──────────────────────────────────┘  │
├──────────────────────────────────────────────────────────┤
│ ████████████████░░░░░░░░░░░░░░░░ 03:12 / 04:30          │
│ ▶ 歌曲名称 · 音量: 80 · 循环列表                       │
└──────────────────────────────────────────────────────────┘
```

## 特性

### ✅ 已实现
- **多音源搜索**：网易云音乐、酷狗音乐、酷我音乐、QQ 音乐、咪咕音乐
- **在线播放**：通过 mpv 播放高品质音乐
- **歌词支持**：支持 LRC、KRC、QRC、YRC 多种歌词格式，支持翻译歌词
- **收藏管理**：添加/取消收藏歌曲
- **播放历史**：自动记录播放记录
- **排行榜**：查看各音源热门歌曲
- **换源匹配**：音源播放失败时自动跨源搜索替代
- **JS 自定义音源**：加载社区维护的音源脚本（兼容 lx-music user API 协议）
- **主题配置**：可自定义颜色主题
- **鼠标支持**：支持点击和滚轮操作
- **键盘快捷键**：完整的键盘操作

### 🚧 开发中 / 未来计划
- [ ] **哔哩哔哩音频**：支持搜索和播放 B 站音频内容（视频音频流）
- [ ] **听书模式**：支持有声书、播客内容
- [ ] **自动补全封面**：在终端中显示专辑封面（使用 halfblock/sixel 字符）
- [ ] **自动补全歌词**：播放时自动从多个源匹配歌词
- [ ] **本地音乐播放**：扫描本地音乐文件并构建本地曲库
- [ ] **歌单管理**：创建和编辑自定义歌单
- [ ] **歌词卡拉 OK 模式**：逐字高亮歌词
- [ ] **跨平台包管理**：支持更多 Linux 发行版、macOS
- [ ] **更多音源插件**：兼容更多 lx-music 社区音源
- [ ] **TUI 响应式布局**：自适应终端窗口大小变化

## 安装

### 前置依赖

- **mpv**（必需）：音频播放引擎
  - Linux：`sudo pacman -S mpv`（Arch） / `sudo apt install mpv`（Debian/Ubuntu）
  - macOS：`brew install mpv`
  - Windows：从 https://mpv.io/ 下载安装

### 从源码编译

```bash
# 克隆仓库
git clone https://github.com/lx-tui/lx-tui.git
cd lx-tui

# 编译运行
cargo run --release

# 编译后的二进制位于
# ./target/release/voicefox
```

### Linux

```bash
# Arch Linux (AUR)
# 待提交到 AUR

# 手动安装
git clone https://github.com/lx-tui/lx-tui.git
cd lx-tui
cargo build --release
sudo cp target/release/voicefox /usr/local/bin/
```

### macOS

```bash
git clone https://github.com/lx-tui/lx-tui.git
cd lx-tui
cargo build --release
cp target/release/voicefox /usr/local/bin/
```

### Windows

```powershell
git clone https://github.com/lx-tui/lx-tui.git
cd lx-tui
cargo build --release
# 将 target/release/voicefox.exe 加入 PATH
```

## 快速开始

### 启动

```bash
voicefox
```

首次启动会自动创建默认配置文件 `~/.config/voicefox/config.toml`。

### 键盘快捷键

| 按键 | 功能 |
|------|------|
| `1`-`6` 或 `/` | 切换标签页（队列/搜索/排行/收藏/历史/设置） |
| `Tab` | 循环切换标签页 |
| `q` | 退出 |
| `Space` | 播放/暂停 |
| `←` `→` | 后退/快进 5 秒 |
| `↑` `↓` | 音量加减 |
| `Ctrl+L` | 收藏/取消收藏当前歌曲 |
| `n` | 下一首 |
| `b` | 上一首 |
| `m` | 切换播放模式 |
| `/` | 搜索模式 |
| `Esc` | 返回/取消 |

搜索页面：
| 按键 | 功能 |
|------|------|
| 输入文字 | 自动搜索（300ms 防抖） |
| `Enter` | 播放选中歌曲 |
| `↑` `↓` | 选择歌曲 |
| `PgUp` `PgDn` | 翻页 |
| `Esc` | 返回主页 |

### 配置

配置文件位于 `~/.config/voicefox/config.toml`：

```toml
[player]
engine = "mpv"
quality = "320k"      # 音质: 128k / 320k / flac / flac24bit
volume = 80
play_mode = "list-loop"  # list-loop / single-loop / shuffle

[source]
enabled = ["kw", "kg", "tx", "wy", "mg"]
default = "kw"
auto_toggle = true

# JS 自定义音源
# js_sources = ["https://example.com/latest.js"]

[lyric]
show_translation = true
show_yrc = true
offset = 0

[network]
proxy_url = ""
timeout = 15

[theme]
use_dark = true
accent = "cyan"
text = "white"
muted = "dark_gray"
border = "cyan"

[ui]
enable_mouse = true
wrap_navigation = true
scroll_amount = 3
aggregate_search = true
show_cover = true
max_fps = 20
```

## 音源说明

voicefox 内置以下音源模块：

| 音源 | ID | 说明 |
|------|----|------|
| 酷我音乐 | kw | **默认音源**，稳定性较好 |
| 酷狗音乐 | kg | 曲库丰富 |
| QQ 音乐 | tx | 腾讯旗下，热门歌曲全 |
| 网易云音乐 | wy | 社区活跃，评论多 |
| 咪咕音乐 | mg | 移动旗下，版权较多 |

**JS 自定义音源**：支持加载社区维护的 lx-music 兼容音源脚本，可解决内置音源接口过时的问题。

## 项目结构

```
voicefox/
├── app/          # 主程序（TUI 界面 + 业务逻辑）
│   └── src/
│       ├── pages/       # 各页面（搜索/队列/收藏/历史/设置/排行）
│       │   └── components/  # 可复用组件（歌词/进度条/状态栏/表格）
│       ├── config/      # 配置加载
│       ├── playlist/    # 播放队列管理
│       └── theme.rs     # 主题系统
├── core/         # 核心类型和接口定义
│   └── src/
│       ├── model/       # 数据模型（歌曲/歌词/配置/音源）
│       └── traits/      # 抽象接口（音源/播放器/歌词）
├── source/       # 音源实现（各平台 API 对接）
│   └── src/
│       ├── wy/ kw/ kg/ tx/ mg/  # 各音源实现
│       └── js/                  # JS 自定义音源引擎
├── player/       # 播放器引擎（mpv IPC）
└── lyric/        # 歌词解析库（LRC/KRC/QRC/YRC）
```

## 技术栈

- **语言**：Rust (edition 2024)
- **TUI 框架**：[ratatui](https://github.com/ratatui/ratatui) 0.29
- **终端事件**：[crossterm](https://github.com/crossterm-rs/crossterm) 0.28
- **异步运行时**：[tokio](https://github.com/tokio-rs/tokio)
- **音频播放**：[mpv](https://mpv.io/)（通过 IPC 控制）
- **HTTP 客户端**：[reqwest](https://github.com/seanmonstar/reqwest)
- **歌词解析**：LRC/KRC/QRC/YRC 自实现解析器

## 许可证

MIT

## 致谢

- [lx-music-desktop](https://github.com/lyswhut/lx-music-desktop) — 项目灵感来源
- [lx-music-source](https://github.com/pdone/lx-music-source) — 社区音源脚本
