# voicefox

> 终端里的音乐播放器 — Rust TUI 版 lx-music-desktop

[![CI](https://github.com/emoeem/voicefox/actions/workflows/ci.yml/badge.svg)](https://github.com/emoeem/voicefox/actions/workflows/ci.yml)

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
- **本地音乐**：扫描本地音乐目录，支持 MP3/FLAC/M4A/OGG/WAV，自动读取封面和歌词，支持浏览和搜索本地歌曲
- **封面显示**：支持 Kitty/WezTerm/Ghostty 终端原生图片协议，真实显示专辑封面
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
git clone https://github.com/emoeem/voicefox.git
cd voicefox

# 编译运行
cargo run --release

# 编译后的二进制位于
# ./target/release/voicefox
```

### Linux

```bash
# 编译
git clone https://github.com/emoeem/voicefox.git
cd voicefox
cargo build --release

# 安装到系统
sudo cp target/release/voicefox /usr/local/bin/

# Arch Linux 安装依赖
sudo pacman -S mpv

# Debian/Ubuntu 安装依赖
sudo apt install mpv

# Fedora 安装依赖
sudo dnf install mpv
```

### macOS

```bash
# 安装依赖
brew install mpv

# 编译
git clone https://github.com/emoeem/voicefox.git
cd voicefox
cargo build --release

# 安装
cp target/release/voicefox /usr/local/bin/
```

### Windows

#### 方法一：GitHub Actions 下载（推荐，无需安装 Rust）

1. 前往 [Actions](https://github.com/emoeem/voicefox/actions) 页面
2. 选择最新的 CI 构建
3. 下载 `voicefox-windows-x86_64.exe` 制品
4. 安装 [mpv](https://mpv.io/installation/) 并加入 PATH
5. 运行 `voicefox.exe`

#### 方法二：从 Linux 交叉编译

```bash
# 在 Linux 上交叉编译 Windows 版本
sudo apt install gcc-mingw-w64-x86-64
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu

# 输出文件
# ./target/x86_64-pc-windows-gnu/release/voicefox.exe
```

#### 方法三：在 Windows 上本地编译

```powershell
# 安装 Rust（从 https://rustup.rs 下载）
# 安装 mpv（从 https://mpv.io/installation/ 下载并加入 PATH）

git clone https://github.com/emoeem/voicefox.git
cd voicefox
cargo build --release
# 输出在 target/release/voicefox.exe
```

## 快速开始

### 启动

```bash
voicefox
```

首次启动会自动创建默认配置文件 `~/.config/voicefox/config.toml`。

完整的快捷键说明见 [KEYBINDINGS.md](KEYBINDINGS.md)。

### 键盘快捷键

#### 全局快捷键（任意页面）

| 按键 | 功能 |
|------|------|
| `1` | 队列页面 |
| `2` | 搜索页面 |
| `3` | 排行榜页面 |
| `4` | 收藏页面 |
| `5` | 历史页面 |
| `6` | 本地音乐页面 |
| `7` | 设置页面 |
| `/` | 切换到搜索页面 |
| `Tab` / `Shift+Tab` | 下一个 / 上一个标签页 |
| `q` | 退出 |
| `Space` | 播放 / 暂停 |
| `n` / `>` | 下一首 |
| `b` / `<` | 上一首 |
| `m` | 切换播放模式（列表循环 / 单曲循环 / 随机） |
| `]` | 快进 5 秒 |
| `[` | 后退 5 秒 |
| `.` | 音量增加 |
| `,` | 音量减少 |
| `Ctrl+L` | 收藏 / 取消收藏当前歌曲 |
| `Esc` | 返回上一级 / 取消 |

#### 队列页面

| 按键 | 功能 |
|------|------|
| `←` `→` | 后退 / 快进 5 秒 |
| `↑` `↓` | 音量加 / 音量减 |

#### 搜索页面

| 按键 | 功能 |
|------|------|
| 输入文字 | 自动搜索（300ms 防抖） |
| `i` / `/` | 进入搜索输入模式 |
| `Enter` | 播放选中歌曲 |
| `↑` `k` | 选择上一首 |
| `↓` `j` | 选择下一首 |
| `PgUp` / `Ctrl+U` | 向上翻页 |
| `PgDn` / `Ctrl+D` | 向下翻页 |
| `Home` / `g` | 跳到列表顶部 |
| `End` / `G` / `Shift+G` | 跳到列表底部 |
| `v` | 切换聚合搜索 / 单音源搜索 |
| `←` `→` | 切换当前音源（非聚合模式） |
| `Esc` | 退出输入模式 / 返回队列页面 |

#### 排行榜页面

| 按键 | 功能 |
|------|------|
| `↑` `k` | 选择上一项 |
| `↓` `j` | 选择下一项 |
| `Enter` | 播放选中歌曲 |
| `PgUp` / `Ctrl+U` | 向上翻页 |
| `PgDn` / `Ctrl+D` | 向下翻页 |
| `Home` / `g` | 跳到列表顶部 |
| `End` / `G` / `Shift+G` | 跳到列表底部 |
| `←` | 返回榜单列表 |
| `→` | 进入选中榜单 |
| `Esc` | 返回上一级 |

#### 收藏页面

| 按键 | 功能 |
|------|------|
| `↑` `k` | 选择上一首 |
| `↓` `j` | 选择下一首 |
| `Enter` | 播放选中歌曲 |
| `/` | 筛选收藏歌曲 |
| `d` | 取消收藏选中歌曲 |
| `Esc` | 退出筛选模式 |

#### 历史页面

| 按键 | 功能 |
|------|------|
| `↑` `k` | 选择上一首 |
| `↓` `j` | 选择下一首 |
| `Enter` | 播放选中歌曲 |
| `PgUp` / `Ctrl+U` | 向上翻页 |
| `PgDn` / `Ctrl+D` | 向下翻页 |
| `Home` / `g` | 跳到列表顶部 |
| `End` / `G` / `Shift+G` | 跳到列表底部 |

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
accent = "#cba6f7"
text = "#cdd6f4"
muted = "#a6adc8"
border = "#585b70"
rosewater = "#f5e0dc"
flamingo = "#f2cdcd"
pink = "#f5c2e7"
mauve = "#cba6f7"
red = "#f38ba8"
maroon = "#eba0ac"
peach = "#fab387"
yellow = "#f9e2af"
green = "#a6e3a1"
teal = "#94e2d5"
sky = "#89dceb"
sapphire = "#74c7ec"
blue = "#89b4fa"
lavender = "#b4befe"
subtext_1 = "#bac2de"
subtext_0 = "#a6adc8"
overlay_2 = "#9399b2"
overlay_1 = "#7f849c"
overlay_0 = "#6c7086"
surface_2 = "#585b70"
surface_1 = "#45475a"
surface_0 = "#313244"
base = "#1e1e2e"
mantle = "#181825"
crust = "#11111b"

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

## 更新日志

### v0.1.0 (2026-07-13)

- ✨ 首次发布，原项目名 lx-tui 更名为 voicefox
- 🎵 多音源在线音乐搜索与播放（网易云、酷狗、酷我、QQ、咪咕）
- 📜 歌词显示（LRC/KRC/QRC/YRC 格式，支持翻译）
- 🔍 聚合搜索与单音源切换
- 🏆 各音源排行榜浏览
- ❤️ 收藏管理与播放历史
- 🔄 音源播放失败时自动跨源匹配
- 📦 JS 自定义音源加载（兼容 lx-music 社区脚本）
- 🎨 可配置颜色主题
- 🖱️ 鼠标支持（点击切换标签页、拖拽进度条）
- ⚙️ 完整的设置页面

## ☕ 赞助

如果 voicefox 对你的工作和生活有帮助，欢迎请我喝杯咖啡 ❤️

| 支付宝 | 微信 |
|--------|------|
| <img src=".github/alipay.jpg" width="200" alt="支付宝收款码"> | <img src=".github/wechat.png" width="200" alt="微信收款码"> |

## 许可证

MIT

## 致谢

- [lx-music-desktop](https://github.com/lyswhut/lx-music-desktop) — 项目灵感来源
- [lx-music-source](https://github.com/pdone/lx-music-source) — 社区音源脚本
- [rmpc](https://github.com/mierak/rmpc) — TUI 架构参考
- [go-musicfox](https://github.com/go-musicfox/go-musicfox) — 播放器设计参考
- [azusa-player-mobile](https://github.com/lovegaoshi/azusa-player-mobile) — 哔哩哔哩音源模块参考
