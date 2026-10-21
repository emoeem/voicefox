# voicefox

> Rust / ratatui / libmpv 终端音乐播放器

[![CI](https://github.com/emoeem/voicefox/actions/workflows/ci.yml/badge.svg)](https://github.com/emoeem/voicefox/actions/workflows/ci.yml)

voicefox 将多音源音乐搜索、在线播放、本地音乐库、歌词、下载、收藏、历史和歌单管理整合进键盘优先的终端界面。当前 0.3.15 发布线使用稳定旧版 TUI，同时集成新的音源、下载、登录和跨平台能力。

## 主要功能

- 多音源聚合搜索与失败换源
- libmpv 播放、ReplayGain、EQ、A-B 循环、淡入淡出
- 队列键盘/鼠标排序、筛选、全屏歌词
- LRC/KRC/QRC/YRC 歌词、翻译与罗马音
- 并发分片下载、完整性校验、标签/封面/LRC 写入
- 本地音乐扫描、标签、封面、歌词、CUE 分轨与文件诊断
- 收藏、历史、自建歌单、排行榜、热门歌单
- Kitty / Sixel / iTerm2 封面协议与 Unicode fallback
- Linux D-Bus/MPRIS、Windows SMTC、macOS Now Playing 媒体键与桌面提示路径
- lx-music 兼容 JS 自定义音源
- 可配置快捷键与运行时帮助
- 跨音源歌单/收藏同步：Diff 预览、双向增量、持久化歌单映射与未匹配提示

## 当前音源

| 音源 | 主要能力 |
|---|---|
| 酷我 `kw` | 搜索、播放、歌词、封面、歌手/专辑、歌单/分类、排行榜、链接直解 |
| 酷狗 `kg` | 上述能力 + 扫码登录、个人歌单 |
| QQ `tx` | 上述能力 + 扫码登录、个人歌单 |
| 网易云 `wy` | 搜索、播放、歌词、封面、歌手/专辑、歌单/分类、排行榜、链接直解、扫码登录、个人歌单 |
| 咪咕 `mg` | 搜索、播放、歌词、封面、歌手/专辑、歌单/分类、排行榜、链接直解 |
| 哔哩哔哩 `bili` | 搜索、视频音频播放、歌词/封面、歌手/UP 主、歌单/排行榜、链接直解、扫码登录 |
| 千千 `qianqian` | 搜索、播放、歌词、封面、歌手/专辑、歌单/分类、链接直解 |
| JOOX `joox` | 搜索、播放、歌词、封面、专辑、歌单、链接直解 |
| 5sing `fivesing` | 搜索、播放、歌词、封面、歌单、链接直解 |
| Jamendo `jamendo` | 搜索、播放、歌词、封面、专辑、歌单、链接直解 |
| Apple Music `apple` | 搜索、试听播放、歌词、封面、专辑/歌单、分类、链接直解 |
| 汽水 `soda` | 搜索、播放、歌词、封面、歌单、链接直解 |
| 本地 `local` | 本地文件扫描、播放、标签、封面、歌词、CUE、诊断 |

完整能力矩阵见 [`docs/USER_GUIDE.md`](docs/USER_GUIDE.md)。同步设计、使用原则与已知限制见 [`docs/SYNC.md`](docs/SYNC.md)。

## 快捷键速览

| 按键 | 功能 |
|---|---|
| `1`~`8` | 页面导航 |
| `Space` | 播放/暂停 |
| `n` / `b` | 下一首/上一首 |
| `m` | 播放模式 |
| `.` / `,` | 音量 |
| `Ctrl+L` | 收藏当前歌曲 |
| `Ctrl+S` | 下载 |
| `Ctrl+O` | 下载面板 |
| `Ctrl+R` | 重绘/重传封面 |
| `V` | 队列歌词全屏 |
| `/` | 支持筛选的页面进入筛选 |
| `?` / `F1` | 快捷键帮助 |
| `q` | 退出 |

完整按页面快捷键见 [`KEYBINDINGS.md`](KEYBINDINGS.md)。

## 安装

### Windows

Release 提供包含 `voicefox.exe` 与 `libmpv-2.dll` 的压缩包。

### Linux

Release 提供仅包含 `voicefox` 可执行文件的压缩包。

需要 `libmpv`：Arch 可 `sudo pacman -S mpv`，Debian/Ubuntu 可 `sudo apt install libmpv-dev`。

### macOS

Release 提供仅包含 `voicefox` 可执行文件的压缩包。

如通过浏览器下载，确保在校验文件完整性后去除 `com.apple.quarantine` 属性：

```bash
xattr -d com.apple.quarantine /path/to/voicefox
```

需要 `libmpv`，可通过 brew 安装：

```bash
brew install mpv
```

## 从源码构建

```bash
git clone https://github.com/emoeem/voicefox.git
cd voicefox
cargo build --release
cargo run --release
```

## 文档

- [`KEYBINDINGS.md`](KEYBINDINGS.md) — 完整快捷键
- [`docs/USER_GUIDE.md`](docs/USER_GUIDE.md) — 功能、音源与平台说明
- [`docs/CONFIGURATION.md`](docs/CONFIGURATION.md) — 配置
- [`docs/SYNC.md`](docs/SYNC.md) — 跨音源同步、Diff、匹配与登录限制
- [`flutter/README.md`](flutter/README.md) — Flutter/Rust 新架构开发线

## 验证

```bash
cargo fmt --all
cargo test -p voicefox-app
cargo test -p lx-core
cargo test -p lx-lyric
cargo check -p voicefox-app --target x86_64-pc-windows-gnu
```

## License

MIT
