# voicefox

> 终端里的音乐播放器 — Rust TUI 版 lx-music-desktop

[![CI](https://github.com/emoeem/voicefox/actions/workflows/ci.yml/badge.svg)](https://github.com/emoeem/voicefox/actions/workflows/ci.yml)

voicefox 是一个运行在终端中的音乐播放器，使用 Rust 编写，基于 ratatui 构建界面，通过 libmpv 播放音频。支持多音源搜索、在线播放、歌词显示、收藏管理等功能。

无需离开终端，也能享受完整的音乐体验。

## 项目简介

voicefox 对标 lx-music-desktop 与 go-musicfox，把完整的桌面音乐播放体验搬进终端：

- **多音源聚合**：酷我、酷狗、QQ、网易云、咪咕、千千音乐、JOOX、5sing、Jamendo、Apple Music、汽水音乐、哔哩哔哩十二种内置音源，外加任意数量的 lx-music 兼容 JS 脚本，统一搜索、统一播放、失败自动换源。
- **播放器内核**：进程内嵌 libmpv，支持高品质音质、ReplayGain、均衡器、声道与平衡、A-B 循环、无缝切换和淡入淡出。
- **本地音乐库**：目录监听与增量扫描，自动读取标签、封面、内嵌歌词与 CUE 分轨，并提供重复/损坏/缺失诊断和确认删除。
- **终端原生体验**：键盘优先 + 完整鼠标支持，Kitty/Sixel/iTerm2 封面渲染，tmux passthrough，Waybar/MPRIS 桌面集成。
- **数据自持**：收藏、历史、自建歌单与播放状态保存在本地 JSON，版本化导出/导入并自动备份。

整个项目按工作区拆分为五个 crate：`app`（TUI 与业务）、`core`（模型与接口）、`source`（音源实现）、`player`（libmpv 引擎）、`lyric`（歌词解析），详见下方[项目结构](#项目结构)。

## 文档导航

| 主题 | 文档 |
|------|------|
| 安装与编译 | [安装](#安装) |
| 快速开始与快捷键 | [快速开始](#快速开始)、[KEYBINDINGS.md](KEYBINDINGS.md) |
| 自定义配置与推荐配置 | [docs/CONFIGURATION.md](docs/CONFIGURATION.md) |
| 音源说明 | [音源说明](#音源说明) |
| 项目结构 | [项目结构](#项目结构) |

## 截图

```
┌──────────────────────────────────────────────────────────┐
│ 1 队列 │ 2 搜索 │ 3 排行榜 │ 4 歌单 │ 5 收藏 │ 6 历史 │ 7 本地 │ 8 设置 │
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
- **多音源搜索**：网易云音乐、酷狗音乐、酷我音乐、QQ 音乐、咪咕音乐、哔哩哔哩
- **在线播放**：通过进程内 libmpv 播放高品质音乐
- **本地音乐**：目录监听与增量扫描，支持 MP3/FLAC/M4A/OGG/OPUS/WAV/WMA/AAC/APE/AIFF 及 CUE 分轨，自动读取封面、同名 LRC 和音频内嵌歌词，提供重复/损坏/缺失诊断，并可确认后删除本地文件
- **封面显示**：按终端能力自动选择 Kitty / Sixel / iTerm2 图片协议，都不支持时用 Unicode 半格块渲染
- **tmux 封面**：在 tmux 中通过 passthrough 传递图形协议序列，封面照常显示；detach 后再 attach 自动重传，关掉终端、SSH 断线、换机器 attach 都能恢复，前提是这些终端支持同一种图形协议
- **歌词支持**：支持 LRC、KRC、QRC、YRC 多种歌词格式，支持翻译歌词
- **音乐下载**：把任意在线歌曲下载到本地，多线程分片、完整性校验、失败重试与换源；自动写入标签、封面和歌词；下载记录兼作去重索引，换模板或重启后也不会重复下载；支持边听边自动缓存和 WebDAV 同步；`Ctrl+O` 打开下载面板查看进度、体积与码率
- **收藏管理**：添加/取消收藏歌曲和热门歌单
- **播放历史**：自动记录播放记录，支持删除单条、清空和配置保留上限
- **内容排序**：收藏、历史和本地音乐支持按最近时间、名称、歌手、专辑和时长排序；收藏与历史可按来源排序，本地音乐可按路径排序，当前模式显示在底部状态栏和右键菜单
- **单曲入队**：可将任意列表中的选中歌曲追加到队尾或设为下一首
- **队列管理**：支持键盘调序、鼠标拖拽调序、移除单曲和清空队列
- **播放模式**：支持列表循环、单曲循环、随机、顺序播放和播完停止
- **排行榜**：按音源切换榜单目录，查看各音源实时热门歌曲
- **热门歌单**：按音源切换并分页浏览酷我、酷狗、QQ、网易云、咪咕的实时歌单
- **歌单分类与链接直解**：网易云、咪咕、千千音乐、酷狗、酷我、QQ 音乐的歌单可按语种 / 风格等分类浏览；搜索框直接粘贴网易云、咪咕、千千音乐、酷狗、酷我、QQ 音乐、哔哩哔哩的歌曲 / 歌单 / 专辑链接即可解析出曲目
- **换源与失败跳过**：获取地址或实际播放失败时依次尝试其他解析器和音源，全部失败后自动播放下一首
- **JS 自定义音源**：同时加载多个社区音源脚本，按配置顺序解析播放地址、歌词和封面；libmpv 拒绝失效链接时可从下一个脚本继续
- **扫码登录**：设置页可对支持扫码的音源（哔哩哔哩、网易云音乐、酷狗音乐、QQ 音乐）打开二维码登录，登录后请求自动带上 cookie，用于 VIP / 无损曲目
- **我的歌单**：已登录的音源（网易云音乐、酷狗音乐）在歌单页按 `m` 可直接浏览账号下的个人歌单 / 收藏歌单
- **主题配置**：可自定义颜色主题
- **鼠标支持**：支持点击、滚轮、队列拖拽和歌曲右键操作菜单
- **TUI 通知**：支持信息、成功、警告、错误四级浮动通知，可配置开关和停留时间
- **桌面通知**：Linux 上通过 D-Bus 发送系统通知，切歌时支持专辑封面
- **Waybar / MPRIS**：显示歌曲、歌手、专辑、进度和播放状态，并支持播放控制
- **键盘快捷键**：完整的键盘操作

## 开发中 / 未来计划
- [x] **哔哩哔哩音频**：支持搜索、BV/av 号、视频长链接、`b23.tv` 短链、分 P 直达、热门推荐、收藏夹、视频音频流和扫码登录；推荐接口异常时自动降级到全网热门
- [ ] **听书模式**：支持有声书、播客内容
- [x] **自动补全歌词**：播放时自动从多个源匹配歌词
- [x] **歌单管理**：创建、重命名和编辑自定义歌单
- [x] **非原生图片终端兼容**：Kitty、Sixel、iTerm2 三种图片协议自动探测，都不支持时退回 Unicode 半格块渲染
- [ ] **跨平台包管理**：支持更多 Linux 发行版、macOS
- [ ] **更多音源插件**：兼容更多 lx-music 社区音源
- [ ] **TUI 超窄布局**：主要页面已支持宽窄布局，继续完善极小终端下的导航和提示

## 安装

### 前置依赖

- **libmpv**（必需）：音频播放引擎，无需安装或调用 `mpv` 命令行程序
  - Linux：`sudo pacman -S mpv`（Arch） / `sudo apt install libmpv-dev`（Debian/Ubuntu）
  - macOS：`brew install mpv`
  - Windows：GitHub Actions 制品已包含 `libmpv-2.dll`

### tmux 中显示封面

在 `~/.tmux.conf` 中启用终端控制序列透传：

```tmux
set -g allow-passthrough on
```

同时确保 `$TERM` 以 `tmux` 为前缀：

```tmux
set -g default-terminal "tmux-256color"
```

重新加载配置并重启 voicefox：

```bash
tmux source-file ~/.tmux.conf
```

detach 再 attach 之后封面会自动恢复。极少数情况下可能无法自动恢复，这种情况下可以按 `Ctrl+R` 手动重传。

图形协议只在启动时探测一次，之后不再重探。因此同一个 voicefox 进程的生命周期内，所有 attach 上来的终端模拟器必须支持同一种图形协议：

- 不要从图形协议不兼容的终端同时 attach 到一个 session（例如仅支持 kitty 协议的 Ghostty 和仅支持 sixel 协议的 Foot）
- 也不要 detach 之后换用不兼容的终端 attach 回来

协议不匹配时封面会显示异常或完全不显示，重启 voicefox 即可按新终端重新探测。都使用同一协议的情况不受影响。混用无法避免时，可以在配置里把 `cover_protocol` 固定成各终端都支持的协议（例如 `halfblocks`）。

### 封面渲染协议

默认自动探测，按终端能力选择 Kitty / Sixel / iTerm2，都不支持时用 Unicode 半格块。探测不准时可以在配置文件里强制指定：

```toml
[ui]
# auto（默认）| kitty | sixel | iterm2 | halfblocks
cover_protocol = "auto"
```

### Waybar 控制模块

voicefox 在 Linux 上默认注册标准 MPRIS 服务。Waybar 使用内置 `mpris` 模块即可显示和控制，无需额外轮询脚本：

```jsonc
"mpris": {
  "format": "{player_icon}  {title}",
  "format-paused": "{status_icon}  {title}",
  "format-stopped": "",
  "player-icons": {
    "voicefox": "",
    "default": ""
  },
  "status-icons": {
    "playing": "",
    "paused": ""
  },
  "tooltip-format": "{player} - {status}\n{title}\n{artist} - {album}",
  "on-click": "play-pause",
  "on-click-middle": "previous",
  "on-click-right": "next"
}
```

将 `"mpris"` 放入 Waybar 的模块列表并重启 Waybar。也可以使用 `playerctl -l` 检查是否出现 `voicefox`。

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

# 安装桌面入口和通知图标（当前用户）
install -Dm644 icons/512.png \
  ~/.local/share/icons/hicolor/512x512/apps/voicefox.png
install -Dm644 assets/voicefox.desktop \
  ~/.local/share/applications/voicefox.desktop

# Arch Linux 安装 libmpv
sudo pacman -S mpv

# Debian/Ubuntu 安装 libmpv 开发包
sudo apt install libmpv-dev

# Fedora 安装 libmpv 开发包
sudo dnf install mpv-libs-devel
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

#### 方法一：Scoop（推荐，自动跟随 Release 更新）

```powershell
scoop bucket add voicefox https://github.com/emoeem/scoop-bucket
scoop install voicefox/voicefox
```

安装包内已捆绑 `libmpv-2.dll`，无需其他依赖。

#### 方法二：GitHub Release / Actions 下载（无需安装 Rust）

1. 前往 [Releases](https://github.com/emoeem/voicefox/releases) 下载最新的 `voicefox-windows-x86_64.zip`（或从 [Actions](https://github.com/emoeem/voicefox/actions) 页面获取最新 CI 制品）
2. 解压 `voicefox-windows-x86_64.zip`
3. 保持 `voicefox.exe` 和 `libmpv-2.dll` 在同一目录并运行

#### 方法三：从 Linux 交叉编译

```bash
# 在 Linux 上交叉编译 Windows 版本
sudo apt install gcc-mingw-w64-x86-64 p7zip-full
rustup target add x86_64-pc-windows-gnu

# 取 audio-only 的 libmpv
# 版本列表：https://github.com/Uyanide/mpv-winbuild-audio/releases
tag=v0.41.0-1
curl -LO "https://github.com/Uyanide/mpv-winbuild-audio/releases/download/$tag/libmpv-audio-x86_64-$tag.7z"

mkdir -p .deps/mpv/64
7z e "libmpv-audio-x86_64-$tag.7z" -o.deps/mpv/64 -r libmpv-2.dll libmpv.dll.a

MPV_SOURCE="$PWD/.deps/mpv" cargo build --release \
  --target x86_64-pc-windows-gnu \
  --features lx-player/build_libmpv

# 输出文件
# ./target/x86_64-pc-windows-gnu/release/voicefox.exe
# 将 .deps/mpv/64/libmpv-2.dll 复制到 exe 同一目录
```

#### 方法四：在 Windows 上本地编译

```powershell
# 安装 Rust 和 MinGW-w64
# 从 https://github.com/Uyanide/mpv-winbuild-audio/releases 下载
# libmpv-audio-x86_64-<tag>.7z，把其中的 libmpv-2.dll 与 libmpv.dll.a
# 解压到 .deps\mpv\64

git clone https://github.com/emoeem/voicefox.git
cd voicefox
rustup target add x86_64-pc-windows-gnu
$env:MPV_SOURCE = "$PWD\.deps\mpv"
cargo build --release --target x86_64-pc-windows-gnu --features lx-player/build_libmpv
# 输出在 target/x86_64-pc-windows-gnu/release/voicefox.exe
# 将 .deps/mpv/64/libmpv-2.dll 复制到 exe 同一目录
```

## 快速开始

### 启动

```bash
voicefox
```

首次启动会自动创建默认配置文件 `~/.config/voicefox/config.toml`。

完整的快捷键说明见 [KEYBINDINGS.md](KEYBINDINGS.md)。

### 键盘快捷键

常用全局快捷键：

| 按键 | 功能 |
|------|------|
| `1`-`8` | 切换队列 / 搜索 / 排行榜 / 歌单 / 收藏 / 历史 / 本地 / 设置页面 |
| `Space` | 播放 / 暂停 |
| `n` / `b` | 下一首 / 上一首 |
| `m` | 切换播放模式（列表循环 / 单曲循环 / 随机 / 顺序 / 停止） |
| `.` / `,` | 音量加 / 减 |
| `q` | 退出 |
| `f` | 收藏 / 取消收藏当前选中的歌曲或歌单 |
| `Ctrl+L` | 收藏 / 取消收藏当前播放中的歌曲（所有页面一致） |
| `Ctrl+S` | 下载当前播放中的歌曲（队列页下载选中的队列项） |
| `D` | 下载选中歌曲（搜索、排行榜、歌单、收藏、历史、歌手/专辑详情） |
| `Ctrl+O` | 打开 / 关闭下载面板（查看进度、取消任务、清理记录） |

各页面（队列、搜索、排行榜、歌单、收藏、历史、本地音乐、设置）的完整键位表见 [KEYBINDINGS.md](KEYBINDINGS.md)；自定义键位见 [docs/CONFIGURATION.md](docs/CONFIGURATION.md)。

## 配置

配置文件位于 `~/.config/voicefox/config.toml`，首次启动时自动生成，旧版本配置自动迁移，缺失字段使用默认值。所有配置项都可以在设置页（`8`）中直接修改并写回文件。

- **完整配置说明**（字段表、推荐配置、数据文件、键位自定义）：[docs/CONFIGURATION.md](docs/CONFIGURATION.md)
- **配置示例**：[config.example.toml](config.example.toml)
- **默认快捷键参考**：[KEYBINDINGS.md](KEYBINDINGS.md)

常用配置速览：

```toml
[player]
quality = "flac"        # 请求音质：128k / 320k / flac / flac24bit
play_mode = "list-loop" # list-loop / single-loop / random / list / none
history_limit = 200

[source]
enabled = ["kw", "kg", "tx", "wy", "mg", "bili"]
default = "kw"
auto_toggle = true      # 播放失败时自动尝试其它音源
js_sources = []         # JS 音源脚本 URL 或本地路径，数组顺序即优先级

[local_music]
enabled = true
paths = ["/home/user/Music"]
max_depth = 0           # 扫描深度，0 为不限制

[download]
dir = "/home/user/Music/voicefox"  # 留空表示 ~/Music/voicefox
filename_template = "{singer} - {name}"
multipart = true        # 音源支持 Range 且文件超过阈值时多线程分片
concurrent_songs = 2    # 同时下载的歌曲数
verify_size = true      # 校验落盘字节数，防止 CDN 掉包
write_tags = true       # 写入标题/歌手/专辑标签
embed_cover = true      # 嵌入封面
save_lyric = true       # 保存 .lrc 并内嵌歌词
```

`quality` 是请求音质，不一定等于最终拿到的编码和码率。播放后状态栏会优先显示 libmpv 检测到的实际音频参数；在设置页的 JS 音源面板按 `h` 可运行音源健康检测。

搜索页选中结果后可按 `@` 追搜歌手或哔哩哔哩 UP 主，按 `#` 追搜专辑；再次按相同按键可删除对应的歌手/专辑条件。歌单页按 `/` 或 `s` 搜索歌单，网易云等支持该能力的音源返回匹配结果，不支持的音源自动保留热门歌单。

需要一份开箱即用的完整推荐配置？直接复制 [docs/CONFIGURATION.md](docs/CONFIGURATION.md) 中的「推荐配置」小节。

## 音源说明

voicefox 内置以下音源模块：

| 音源 | ID | 说明 |
|------|----|------|
| 酷我音乐 | kw | **默认音源**，稳定性较好 |
| 酷狗音乐 | kg | 曲库丰富 |
| QQ 音乐 | tx | 腾讯旗下，热门歌曲全 |
| 网易云音乐 | wy | 社区活跃，评论多 |
| 咪咕音乐 | mg | 移动旗下，版权较多 |
| 千千音乐 | qianqian | 百度系曲库，支持歌单分类与链接直解 |
| JOOX | joox | 腾讯系海外平台，支持单曲/歌单/专辑链接直解 |
| 5sing | fivesing | 酷狗旗下原创音乐平台，支持单曲/歌单链接直解 |
| Jamendo | jamendo | CC 授权曲库，支持单曲/歌单/专辑链接直解 |
| Apple Music | apple | 仅提供 30 秒试听（完整曲目为 DRM 加密），支持链接直解与策展人歌单分类 |
| 汽水音乐 | soda | 加密音频（CENC）在本地解密后缓存播放，支持歌单与链接直解 |
| 哔哩哔哩 | bili | 支持搜索、BV/av 号、视频链接、短链和多 P 音频解析 |

**JS 自定义音源**：支持同时加载多个社区维护的 lx-music 兼容音源脚本。脚本按 `js_sources` 数组顺序参与播放地址、歌词和封面解析，并共同参与聚合搜索；前一个脚本返回错误时会继续尝试后续脚本。若某个脚本成功返回 URL，但 libmpv 实际无法播放，开启自动换源后会记住该脚本并从下一个脚本继续，而不会重复请求同一个失效链接。

通过设置页添加脚本时，新添加的 URL 会放到列表最前面，因此最后添加的脚本优先级最高。需要固定顺序时可直接编辑 `config.toml` 中的 `js_sources`。

## 项目结构

```
voicefox/
├── app/          # 主程序（TUI 界面 + 业务逻辑）
│   └── src/
│       ├── pages/       # 各页面（搜索/队列/歌单/收藏/历史/设置/排行）
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
│       ├── wy/ kw/ kg/ tx/ mg/ qianqian/ joox/ fivesing/ jamendo/ apple/ soda/ bili/ local/
│       └── js/                  # JS 自定义音源引擎
├── player/       # 播放器引擎（libmpv2）
└── lyric/        # 歌词解析库（LRC/KRC/QRC/YRC）
```

## 技术栈

- **语言**：Rust (edition 2024)
- **TUI 框架**：[ratatui](https://github.com/ratatui/ratatui) 0.30
- **终端事件**：[crossterm](https://github.com/crossterm-rs/crossterm) 0.29
- **异步运行时**：[tokio](https://github.com/tokio-rs/tokio)
- **音频播放**：[libmpv2](https://docs.rs/libmpv2/)（进程内控制 libmpv）
- **HTTP 客户端**：[reqwest](https://github.com/seanmonstar/reqwest)
- **歌词解析**：LRC/KRC/QRC/YRC 自实现解析器

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
