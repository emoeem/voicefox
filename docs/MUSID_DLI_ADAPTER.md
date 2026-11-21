# musid-dli 适配层设计（V0 - V5）

> 状态：**V0 已落地**（2026-11-20；映射函数 + fixture 测试）→ **V1 双栈接入
> 已落地**（2026-11-21；opt-in 四面路由 + 回退计数 + parity 对账，§11）→
> **V2 逐源迁移已完成全表**（2026-11-21；§12；切片 1-11 = jamendo /
> fivesing / apple / bilibili / migu / kuwo / kugou（v2-batch-1）+ qqmusic /
> netease / qianqian / joox（v2-batch-2，§12.7），登记项 soda /
> youtube-music 不迁）→ **V3 本地库/歌词缓存/封面接入**（2026-11-22；
> §13.1/13.2/13.3 三切片，全部 feature 门控 + 对账测试）→ **V4 legacy 删除
> 进行中**（2026-11-22；§14：joox（八面全删）与 qianqian（十面全删）的
> legacy 实现已注销、适配面升 lx-source default 的唯一实现，其余 10 源
> 缺口账本 §14.0）→ **V5 CI 强化已落地**（2026-11-22；§14.2/§15：rust-cache
> 五 job + adapter 特性 clippy 步 + MINGW64 抑制）。最终验收 run
> 44933775046 六 job 全绿。
>
> V0 落地物：
> - 映射代码 `source/src/adapter_musid.rs`，由 `source/src/lib.rs` 在
>   feature `musid-dli-adapter` 后注册；
> - fixture `source/tests/fixtures/musid/`（8 个 JSON，来源与形状说明见 §7）；
> - 依赖方案 `source/Cargo.toml`：crates.io `musid-dli = "=0.2.0"`
>   optional + `default-features = false`，feature `musid-dli-adapter` 与
>   歌词变体 `musid-dli-adapter-lyrics`（取舍与验收口径见 §8）。
>
> 映射规模：盘点映射对 45 对（40 对双向映射 + 5 对单侧缺口登记，缺口
> 逐条见 §3）；namespace 通道 11 条（§1）。
>
> V0 门禁（2026-11-20 复跑，命令与口径见 §8）：
> - `cargo fmt --all -- --check`：干净；
> - 确定性硬门 `cargo test -p lx-source --features musid-dli-adapter --
>   adapter_musid`：29 通过 / 0 失败；
> - feature 全量 `cargo test -p lx-source --features musid-dli-adapter`：
>   lib 249 例，247 通过 / 0 失败 / 2 ignored（本次复跑未触发 §0 登记 flaky）；
> - 默认基线 `cargo test -p lx-source`：lib 220 例，218 通过 / 0 失败 /
>   2 ignored；
> - `cargo clippy -p lx-source --all-targets` 开关 feature 均 exit 0，
>   `adapter_musid` 零告警引用（既有告警全部在 lx-core lib）；
> - `cargo tree -p lx-source`（默认）无 musid 边；开 feature 可见
>   `musid-dli v0.2.0`。
>
> V1 门禁（2026-11-21 复跑；两轮评审修订后复跑，命令与结果见 §11.9；
> 本段例数一律按"总例数 = 通过 + 失败 + ignored"计）：默认基线 lib 仍为
> 220 例（218 通过 / 0 失败 / 2 ignored，未触发 §0 登记 flaky——其根因已
> 定位修复，见 §11.10 第二轮 F2）——旧实现零删改、默认路径行为零变化的
> 双证；adapter feature 全量 lib 260 例（258 通过 / 0 失败 / 2 ignored；
> V0 249 例 + 新增 11 例单测），歌词变体 lib 265 例（263/0/2）；新增集成
> 测试 `musid_dual_stack`（adapter feature 下 19 例 / 歌词变体下 23 例 /
> 默认构建 `#![cfg]` 编译为空 0 例）与 `musid_parity`（6 例）全绿；CI
> 新增 `musid-stack` job（评审 P6）。
>
> 依据：2026-11-20 双仓工作树现场盘点（`pairs` 逐条读过源码）+ 本阶段复核。
> 本文是 voicefox 侧 `lx-source` 与 musid-dli 0.2.0（crates.io）之间的
> 模型映射契约；V1 双栈的落地语义见 §11，V2 逐源迁移框架见 §12，
> V2-V4 升级顺序见 §5，取消/超时/换源语义对齐见 §6。

## 0. V0 范围与非范围

**做**：
- `source/src/adapter_musid.rs`（feature `musid-dli-adapter`）内的纯映射函数：
  namespace、音质档位、Track↔SongInfo、StreamInfo→SongUrl（含伴随字段）、
  AudioSpec↔AudioProperties、集合模型（Playlist/分类/专辑/歌手/榜单）、
  搜索页、错误分类、能力位、会话；歌词面（文档↔LyricData 往返在
  `musid-dli-adapter-lyrics` 变体下、`LyricSearchResult` 跨源补全候选为
  专属映射且不挂 feature 门，§2.10）。
- fixture 测试：musid-dli 模型 wire 形状 JSON → musid-dli 模型 → voicefox
  类型断言（fixture 来源与形状说明见 §7，**含一处与盘点口径的偏差**）。

**不做**（V0 硬边界）：
- 不接 `SourceManager`，不改任何既有 provider/播放器/下载调度/JS 音源/本地库。
- 不做 `MusicSource` 双栈注册（V1）。
- 不写任何绝对路径、path 依赖或 fork 绕过（transport 注入缺口见 §4）。

基线现状（本阶段在零改动树上实测）：`lx-source` lib 套件共 **220 例**。
首跑 217 通过 / 1 失败 / 2 ignored：失败项
`cover_cache::tests::sweep_evicts_down_to_the_limit_by_oldest_access`
（`source/src/cover_cache.rs:353`）为既有 flaky（mtime 秒级精度，并发
全量跑下间歇触发；本阶段用硬门命令连跑实测复现：多次中一次
245 通过 / 1 失败，其余全绿 246 通过；隔离重跑稳定通过），非本阶段引入，
V0 不触碰（业务路径）。硬门定义与该 flaky 的口径处理见 §8。

## 1. 命名空间映射（唯一合法通道）

musid-dli 侧 `musid_dli::models::namespace`（`models/namespace.rs:20-54`，
恒编译、无 feature gate）的双向表是两套命名互转的唯一合法通道；voicefox 侧
落点是 `lx_core::model::source::SourceId::as_str()`（`core/src/model/source.rs:46-62`）。

| voicefox `SourceId` | musid-dli provider id | 备注 |
|---|---|---|
| `wy` | `netease` | 改名 |
| `tx` | `qqmusic` | 改名 |
| `kg` | `kugou` | 改名 |
| `kw` | `kuwo` | 改名 |
| `mg` | `migu` | 改名 |
| `bili` | `bilibili` | 改名 |
| `jamendo` | `jamendo` | 同名 |
| `apple` | `applemusic` | 改名 |
| `fivesing` | `fivesing` | 同名 |
| `qianqian` | `qianqian` | 同名（2026-11-19 迁移） |
| `joox` | `joox` | 同名（2026-11-19 迁移） |

**显式拒绝项**（适配层返回错误/None，绝不兜底改名）：
- voicefox-only：`soda`、`local`（local 走 LOCAL_LIBRARY_PARITY.md）。
- musid-dli-only：`youtube-music`、`lx-js`。
- 跨语言查表必须失败：`"netease"` 不是 voicefox 名，`"wy"` 不是 musid-dli 名
  （musid-dli 侧测试 `namespace.rs:95-111` 已锁定）。

适配层封装为 `source_id_to_provider` / `provider_to_source_id` /
`require_provider`（后者给 TrackId→SongInfo 这类"查不到即报错"的路径用）。

## 2. 模型映射表

### 2.1 TrackId → SongInfo.id + source（§12.2 硬要求 1）

`musid_dli::models::TrackId { provider, id }`（`models/track.rs:6-9`）拆成
`SongInfo.id: String` + `SongInfo.source: SourceId`：

- `id` 原样透传（含本地歌曲用文件路径当 id 的形态），永不重编；
- `provider` 经 §1 命名表转 `SourceId`；查表失败（`youtube-music`/`lx-js`）
  时适配层返回 `AdapterError::UnmappedProvider`，绝不静默改名；
- 反向 `SongInfo→Track` 时 provider 必须来自歌曲的 `source` 字段而非路径
  推断（MODEL_MAPPING.md §2 第 1 行约定）；
- 专辑 id 不模仿 voicefox 的"缺 ID 回落专辑名"默认
  （`core/src/traits/source.rs:207-217`）：musid-dli 的 `TrackId.id` 是
  平台原 ID，必须保留。

### 2.2 Track ↔ SongInfo

musid-dli `Track`（`models/track.rs:64-91`，恒编译）↔ voicefox `SongInfo`
（`core/src/model/song.rs:12-36`）：

| musid-dli | voicefox | 规则 |
|---|---|---|
| `id: TrackId` | `id` + `source` | §2.1 |
| `title` | `name` | 直通 |
| `artists: Vec<String>` | `singer` | 正向 `'、'` join；反向**只按 `'、'` 切**（voicefox `singer` 的约定格式就是"歌手A、歌手B"，适配层自身 join 也只用 `'、'`，因此 `'、'` 单字符切分对适配层产物无损往返；voicefox 归一分隔符集合 `、, & / \|`（`traits/source.rs:303-311`）只用于匹配归一，从不用于重构歌手列表，拿全集切分会把 "Earth, Wind & Fire" 撕成 3 个、"AC/DC" 撕成 2 个，破坏 `Track.artists` 身份数据）、trim、去空 |
| `album: Option<String>` | `album_name: String` | `None`↔空串（voicefox 以空串为"未知"，`traits/source.rs:196-204`） |
| `album_id: Option<String>` | `album_id: String` | 同上 |
| `duration: Option<Duration>` | `duration: Duration` | `None`→`ZERO`；反向 `ZERO`→`None` |
| `artwork_url` | `cover_url` | 直通 |
| `available_qualities: Vec<StreamQuality>` | `qualities: BTreeSet<Quality>` | 先 `Track::sorted_qualities()`（去重、tier 升序、Unknown 最后，`track.rs:96-101`）再逐档翻译（§2.4）；`Unknown` 掉出集合 |
| `audio: Option<AudioSpec>` | `audio: Option<AudioProperties>` | 键对键（§2.3） |
| `extra: BTreeMap` | `extra: HashMap` | 键值原样 |
| `isrc: Option<String>` | 无槽位 → `extra["isrc"]` | 反向从 `extra["isrc"]` 还原进 `isrc` 槽位（往返保真） |
| 无槽位 | `toggle_source` / `file_path` / `file_ext` | voicefox-only，musid-dli 边界上不存在（G6 死槽降级已 pin，M3 §9）；适配层不产不读 |

### 2.3 AudioSpec ↔ AudioProperties（键对键零换名）

`bitrate/sample_rate/bit_depth/lossless` 同名同义（M3 §3.3 特意对齐
voicefox wire key `audio`），序列化对象可直接跨边界搬。`label()` 两侧
（`track.rs:42-51` vs `source.rs:272-281`）是逐字移植，UI 标签任取一侧。
producer 现状：仅本地扫描填 `audio`，provider 解析点全部 `audio: None`。

### 2.4 StreamQuality ↔ Quality

musid-dli tier 序 `Low<Standard<High<Lossless<HiRes<Unknown>`
（`models/stream.rs:4-12`）；voicefox `Quality` default `Low128`
（`core/src/model/source.rs:220-233`）。

| musid-dli | voicefox | 说明 |
|---|---|---|
| `Low` | `Low128` | |
| `Standard` | `Low128` | **有损降档**：voicefox 无 Standard 槽位，映射到最近的低位；适配层在 `ResolvedUrl.notes` / 文档层记录该降档，不静默 |
| `High` | `High320` | |
| `Lossless` | `Flac` | |
| `HiRes` | `Flac24` | |
| `Unknown` | 无 | 正向从 qualities 集合掉出；反向不存在（voicefox 无 Unknown） |

反向（voicefox→musid-dli）：`Low128→Low`、`High320→High`、`Flac→Lossless`、
`Flac24→HiRes`，无歧义。

**档位语义注意**：voicefox 默认 `Low128` vs musid-dli `ResolveOptions::default()=High`
（`provider/traits.rs:120-126`），适配层必须**显式传档位**而不是用 Default。
resolve 未命中档位时 musid-dli 回落到 provider ladder 内部、voicefox 是全局
`QUALITY_ORDER`：**resolve 结果必须以 `StreamInfo.quality`（实际档位）回填
`SongUrl.quality`，不能假设请求档位=返回档位**。lx bridge 的
Lossless/HiRes→`'flac'` 折叠（`lx_js.rs:46-52`）只作用于 JS 音源路径，不进
本映射。

### 2.5 StreamInfo → SongUrl + 伴随字段（ResolvedUrl）

musid-dli `StreamInfo`（`models/stream.rs:88-128`）→ voicefox `SongUrl`
（`core/src/traits/source.rs:79-103`）。主映射：

| musid-dli | voicefox | 规则 |
|---|---|---|
| `url` | `url` | 直通 |
| `quality` | `quality` | §2.4（`Unknown`→`Low128` + note，不静默） |
| `duration: Option` | `duration: Duration` | `None`→`ZERO` |
| `content_length` | `size` | 两侧 `None` 同义"未提供/不可靠" |
| `size_is_advisory` | `size_is_advisory` | 布尔直通 |
| `headers: Vec<(String,String)>` | `headers` | 原样保序 |
| `max_chunk_size: Option<u64>` | `max_chunk_size: u64` | `None`→`0`（0=off）；**`Some(n>0)` 而 `content_length=None` 时降级为 `0` + note**（voicefox 侧"启用必须配 size"、musid-dli 引擎 fail `"strict chunked download needs the source-declared size"`，`src/download.rs:473-480`，消息在 :478）：适配层选择禁用 strict chunk 向上暴露，而不是让引擎在下载时才炸 |
| `md5_checksum()` | `md5` | 只在 algorithm 缺省（legacy=MD5）或显式 `Md5` 时产出；`Sha1/Sha256` 掉 checksum 并记 note（绝不可把 SHA1 值喂 md5 门）。trim 语义是 pin 过的 adapter 行为（`stream.rs` 测试 `md5_checksum_only_for_md5_algorithm`）：空白填充的摘要可过校验、纯空白跳过 |
| `ordered_fallback_urls()` | `candidate_urls` | §2.6 |
| `expires_at_unix` | 无槽位 | → `ResolvedUrl.expires_at_unix` 伴随字段（§3 G4） |
| `format: AudioFormat` | 无槽位 | → `ResolvedUrl.format`（§3） |
| `index_range`/`init_range` | 无槽位 | → `ResolvedUrl` 同名伴随字段（G3，两侧均 descriptive-only） |
| `ordered_fallback_endpoints()` | 无槽位 | → `ResolvedUrl.fallback_endpoints`（§2.6） |
| 无来源字段 | `cover_url` | musid-dli `StreamInfo` 上没有封面来源字段：映射产物恒 `None`（缺口 §3 第 13 条）。voicefox 内置源两处都从歌曲模型补齐（`source/src/wy/url.rs:241,248,273`、`source/src/kg/url.rs:305` 附近），消费面真实存在：`app/src/download/manager.rs:925-931` 封面嵌入优先 `url.cover_url` 回落 `song.cover_url`、`application/src/services/playback.rs:139-141` 播放侧同样回落。V1 双栈接入时在 `get_song_url` 调用点用适配层 `enrich_song_url_from_song(&mut SongUrl, &SongInfo)` 补齐，不静默依赖下游 fallback |
| 无来源字段 | `qualities` | 同上恒空 `Vec`（缺口 §3 第 14 条；voicefox 内置源从 `song.qualities` 收集）。全仓未发现读取方（只有生产方），V1 用同一 `enrich_song_url_from_song` 补齐 |

伴随结构（适配层所有"voicefox 无槽位"信息都挂这里，不静默丢）：

```rust
pub struct ResolvedUrl {
    pub url: SongUrl,                       // 主映射产物
    pub expires_at_unix: Option<u64>,       // G4：过期 URL 语义（§3）
    pub format: AudioFormat,                // musid-dli-only：下载/NCM 场景用
    pub index_range: Option<ByteRange>,     // G3：DASH 描述元数据
    pub init_range: Option<ByteRange>,
    pub fallback_endpoints: Vec<FallbackEndpoint>, // per-candidate 元数据旁路
    pub notes: Vec<&'static str>,           // 显式降档/丢弃登记（Unknown 档位、
                                            // Standard→Low128、SHA 算法、strict chunk 关闭）
}
```

反向 `SongUrl→StreamInfo`（下载组装方向，`musid_dli::download::download_request_from_stream`
的入参前一步）：`url` 直通、`quality` 反向、`duration ZERO→None`、
`size→content_length`、`md5→checksum`（algorithm 用缺省=None=MD5）、
`headers` 直通、`candidate_urls→fallback_urls`、`max_chunk_size 0→None`、
`size_is_advisory` 直通。V0 只提供映射本身，不接下载引擎。

### 2.6 备用 URL 顺序契约（§12.2 硬要求）

`candidate_urls` = 主 URL 之外按 try 顺序排列：

- musid-dli 侧 `ordered_fallback_urls`（`stream.rs:160-210`，M6 §6.3 R9）：
  legacy `fallback_urls` 先、结构化 `fallback_endpoints` 后，trim、去空、
  keep-first 去重；primary 不含（`SongUrl.url` 已是 primary）。
- `FallbackEndpoint` 的逐 URL `headers/expires_at_unix/content_length/quality`
  在 voicefox 形状里没有槽位：V0 折叠为 URL 字符串进 `candidate_urls`，
  元数据挂 `ResolvedUrl.fallback_endpoints` 旁路留给 V1 下载引擎切片
  （引擎目前对每个 candidate 复用 primary 的 headers/size，两边引擎同病）。
- 缺省字段（`#[serde(default)]`）语义是"与 primary 相同"，折叠时不得把
  `None` 误当 0/空串覆盖 primary 值。

### 2.7 集合模型

| musid-dli | voicefox | 规则 |
|---|---|---|
| `Playlist`（`models/playlist.rs:3-29`） | `lx_core::model::playlist::Playlist` | `id: TrackId`→`id`+`source`（§1）；`title→name`；`description→description`；`artwork_url→cover_url`；`track_count: Option<u32>→song_count: u32`（`None→0`，voicefox 无"未知计数"表达，UI 层注意 0 与未知同形）；`play_count/creator/link` 直通；`extra` 原样。**`tracks: Vec<Track>` 不进 voicefox Playlist**：voicefox 由 `get_playlist_detail` 按页拉取，适配层走 `playlist_tracks()`（`provider/traits.rs:142-145`）转 `SongInfo` |
| `PlaylistCategory`（`models/playlist.rs:31-48`） | `PlaylistCategory` | `id/name/group/count/hot` 同名；`provider: Option<String>→source: SourceId`（`None` 时 voicefox 无"未声明"表达：适配层要求 provider 必填，`None` 报 `AdapterError::MissingProvider`）；`extra` 原样 |
| `Album`（`models/album.rs:4-13`） | `Album` | `id: TrackId`→`id`+`source`；`title→name`；`artwork_url→cover_url`；`artists: Vec`→`artist: String`（join `'、'`，多歌手信息合并有损，反向只能整串）；`track_count` 无槽位丢弃 |
| `Artist`（`models/artist.rs:4-9`） | `Artist` | 1:1：`id`→`id`+`source`、`name`、`artwork_url→cover_url` |
| `Chart`（`models/chart.rs:4-21`） | `LeaderboardInfo` | `id`→`id`+`source`、`title→name`、`updated_description→update`（语义即"每日更新"，M3 专为此新增独立字段）；`description` 无槽位丢弃 |
| `ChartEntry { rank, track }` | `SongInfo` | track 走 §2.2；`rank` 挂 `extra["rank"]`（字符串化） |
| `SearchPage`（`provider/traits.rs:110-115`） | `SearchResult` | items 逐条 §2.2；`total: Option<u64>→u32`：`None→items.len()`（saturating 收敛，避免"有结果却显示 0 条"），`Some→clamp u32`；**G12 的 0 与未知同形缺口照实保留**（`SearchResult` 无 extra 槽位可登记）；`has_more` 直通 |

入参对：`SearchQuery { keyword, page, page_size }` ↔
`MusicSource::search(keyword, page, limit)`：直接拆/装。
`ResolveOptions { quality }` 由反向 `Track` + 反向 `Quality` 构成，
**必须显式传档位**（§2.4）。

### 2.8 能力位（ProviderCapabilities → SourceCapabilities）

bit 表不是 1:1（M3 §5.4 pin 的粒度表）：

| musid-dli | voicefox | 说明 |
|---|---|---|
| `track_search` / `stream_resolve` / `lyrics` | 无位 | 所有内置音源都提供，恒 true 或由 adapter 声明 |
| `playlists` | `playlists` | |
| `playlists` + `search_playlists` 存在 | `playlist_search` | voicefox 独有位，musid-dli 有接口即 true |
| `categories()` 未返回 Unsupported | `playlist_categories` | |
| `albums` | `album` | |
| `artists` | `artist` | |
| `charts` | `leaderboard` | |
| `link_resolve` | `link_parse` | |
| `authentication` | `login` | **机制细分**：voicefox login 位语义是"支持配置登录 cookie"（`core/src/traits/source.rs:57-58`），只对 cookie 类机制置 true：`capabilities.authentication`（传统 authentication 位，`auth_capability()` 缺省把它折叠成 `AuthCapability::Cookie`）或显式 `AuthCapability::Cookie`；`Anonymous`（kuwo web token / migu sid 式匿名设备会话，musid-dli `provider/traits.rs:51-53`）、`OAuth`、`Password`、`Token` 不置 login（这些机制在 voicefox 没有 cookie 配置入口语义，误置 true 会让设置页出现对不上的登录入口；V1 统一登录时再评估是否上浮）。**矛盾声明的处理约定**：`authentication=true` 与覆写 `auth_capability()` 为非 Cookie 机制同时出现时按 authentication 位胜出登记（musid-dli 侧该 bool 的语义即"Cookie session 是真实机制"，缺省推导也把它折叠成 Cookie）；当前 12 个 provider 无此矛盾组合（kuwo 匿名 token 场景是 authentication=false + Anonymous、fivesing 是 false + None，musid-dli `source_kuwo.rs:1918-1941`、`source_fivesing.rs:494-511` 有测试 pin），适配层测试 pin 了该约定；V1 调用点如遇矛盾组合须先在 musid-dli 侧修 descriptor 再接线 |
| `AuthCapability::Qr` 注册 | `qr_login` | |
| 无数据源 | `wechat_login` | **双栈期恒 false**（微信扫码仍走 voicefox 内置 tx 源） |
| 无数据源 | `vip_account` | 恒 false |
| `download` | 忽略 | registry 不外露该位（`traits.rs:16-22`） |
| `artwork` | 忽略 | voicefox `SourceCapabilities` 无 artwork 位（`traits.rs:44-52` 的位面），适配层不映射 |
| 无数据源 | `wechat_login` | **双栈期恒 false**（微信扫码仍走 voicefox 内置 tx 源） |
| 无数据源 | `vip_account` | 恒 false |
| `download` | 忽略 | registry 不外露该位（`traits.rs:16-22`） |

`version/priority/supported_qualities` 供设置页展示，capability 聚合是 OR
语义。未注册音源 `capabilities()=default`（`manager.rs:309-314`）行为与
voicefox 现状一致。

### 2.9 登录/会话

- `QrLoginStatus`（九态）/`QrLoginKind`：**零转换，serde 直通**（G8/G9 关闭
  后两侧 wire 与 `is_transient`/`is_terminal` 逐字一致）。
- `QrLoginSession`：唯一差异是 voicefox 多 `source: SourceId`：适配层由
  发起 provider 的 id 经 §1 表回填；`youtube-music/lx-js` 无映射时该对不可用。
- `QrLoginPoll` → `QrLoginResult`：`status/message` 直通；`user_name` 用
  `QrLoginPoll::user_name()` 折叠（display_name 优先回退 user_id，两者皆
  None 保持 None，绝不折叠成空串）。cookie 永不经结果对象。
- `AuthState` 七态 → `is_logged_in() -> bool`：
  `Authenticated/Refreshing→true`，其余（`LoggedOut`/`PendingQrScan`/`Expired`/
  `Invalidated`/`ReauthorizationRequired`）→false。（`LoggedIn` 是
  `LoginStatus` 三态视图的变体，不是 `AuthState` 七态之一，
  `traits.rs:197-204`。）
- `ProviderSession` ↔ `lx_source::session::SourceSession`（§12.2 硬要求 2）：
  `cookies` 直通；`tokens→extra`（键加 `token:` 前缀防撞）；`display_name→user_name`、
  `user_id` 直通（musid-dli 两字段分存，voicefox `user_name` 只存 display_name
  值，身份不丢）；`expires_at/updated_at/region→extra`（`expires_at_unix` 等
  自定义键）。每源一文件的布局与 `SessionExport` 信封等价，不做合并存储。

### 2.10 歌词（复用 musid-dli 内置转换器，不重写）

musid-dli `lyrics_convert`（feature `lyrics-conversions`）已内置 voicefox
形状转换器，适配层直接调用：

- `document_to_lyric_data`：`original→lyric`（仅当原文含 mm:ss.xx 括号才信
  原始文本，否则渲染解析行：引擎信任规则 `download.rs:1270-1324`
  （`standard_lyric` 于 :1278 起、`looks_like_timed_lrc` 的 mm:ss.xx 判定
  于 :1300-1318；注意 :1327-1340 是 `cover_is_usable` 封面魔数检查，与此
  无关））、
  `translation→tlyric`、`romanization→rlyric`、**`lxlyric 恒 None`**
  （musid-dli 无 YRC/QRC 序列化器，逐字时间只在 `lines[].words`，伪造即假
  数据）、`raw_lrc`←非空 original。
- `lyric_lines_from_document`：行时长规则 end_ms→end-start；无→下一行
  start-本行；末行→0（voicefox 原生规则 `lyric.rs:26-27`）。
- `LyricFormat` 与逐行 `translations/romanization`：有损丢弃（M3 §4 pin）。
  Krc 标签是语法提示不是"有卡拉OK"，判断用 `LyricDocument::has_word_timing`。
- `LyricSearchResult` → 跨源补全候选：**V0 已实现**为专属函数
  `lyric_search_candidate_from_md`（返回 `LyricSearchCandidate { song,
  match_score }`）。注意 `LyricSearchResult` 是独立模型（provider/id/
  title/artists/album/duration_ms/match_score，musid-dli
  `models/lyrics.rs:57-67`），不是 `Track`，不能机械套用 Track→SongInfo
  规则，因此是专属映射：provider+id 经 §1 表回 `SourceId`（查不到报
  `UnmappedProvider`）、`title→name`、`artists` 按 `'、'` join 组
  `singer`、`album: None→空串`、`duration_ms: None→ZERO`（与 Track 的
  None→ZERO 同口径）、`match_score` 原样透传供排序（替代 voicefox
  `find_music` 的分数语义）；voicefox 负缓存键
  `(source, extra[source], id)`（`manager.rs:1015-1019`）须换算为
  `(provider, raw_id)` 对接 musid-dli NegativeCache。该模型在 musid-dli
  侧恒编译（models 层无 feature gate），适配层函数同样不挂 feature 门。
- `get_lyric` 返回 `None` → voicefox 侧按现有语义处理
  （`LyricData::default()` 或 `FetchError::NotFound`，`manager.rs:1040-1050`）。

## 3. 有损缺口登记（V0 评审清单）

1. **SongUrl 缺 `expires_at_unix`**（G4）：→ `ResolvedUrl` 伴随字段，不丢弃、
   不缓存过期值；过期 URL 绝不透传给播放器（musid-dli `StreamCache`
   `cache.rs:26-51` 按 expires 拒发，voicefox 无此缓存）。
2. **SongUrl 缺 `format`**（musid-dli-only）：→ `ResolvedUrl.format`；不得把
   format 反推回 Quality/extension 近似值。
3. **SongUrl 缺 `index_range`/`init_range`**（G3）：DASH 下载两侧都不支持，
   仅描述性，`ResolvedUrl` 原样携带。
4. **`StreamQuality::Standard` 无 voicefox 档位**：降档 `Low128` + note。
5. **`StreamQuality::Unknown`**：不产 `Quality`。
6. **歌手串的切分边界**：适配层 `singer_to_artists` **只按 `'、'` 切**
   （voicefox `singer` 约定格式 + 适配层自身 join 字符，对适配层产物无损
   往返）。voicefox 归一分隔符集合 `、, & / |`（`traits/source.rs:303-311`）
   只用于匹配归一、从不用于重构歌手列表：拿全集切分会把含分隔符的单一
   歌手名撕碎（"Earth, Wind & Fire"→3 个、"AC/DC"→2 个），是
   `Track.artists` 身份数据损坏向量（musid-dli matcher 评分与 tag_write
   消费它），已收窄切分集规避。剩余固有歧义：歌手名本身含 `'、'` 时在
   voicefox 单串模型里无法区分（模型表达力限制，与切分集无关）。
   Album 反向 `artists: Vec→artist: String` 只能整串 join。
7. **`Playlist.track_count: None→song_count: 0`**：0 与未知同形。
8. **`SearchPage.total: None→items.len()`**：0 与未知仍同形（无 extra 槽位）。
9. **歌词逐行 translations/romanization、LyricFormat、逐字→lxlyric 反向**：
   有损丢弃（musid-dli lyrics_convert 已 pin，适配层复用不重写）。
10. **`wechat_login`/`vip_account`**：musid-dli 无数据源，双栈期恒 false。
11. **musid-dli `Track.isrc`**：落 `SongInfo.extra["isrc"]`（往返还原）。
12. **voicefox `toggle_source`/`file_path`/`file_ext`**：musid-dli 无槽位，
    簿记留 voicefox 层（G6 死槽降级已 pin）。
13. **`SongUrl.cover_url` 无 musid-dli 来源字段**：`StreamInfo` 上没有封面
    字段，`stream_to_resolved_url` 产物恒 `None`（§2.5 表）。voicefox 内置
    源从歌曲模型补齐（`source/src/wy/url.rs:241,248,273`、
    `source/src/kg/url.rs:305` 附近）；消费面真实存在：
    `app/src/download/manager.rs:925-931` 封面嵌入优先 `url.cover_url` 回落
    `song.cover_url`、`application/src/services/playback.rs:139-141` 播放侧
    同样回落。V0 靠下游 fallback 兜住不致错；V1 双栈接入时在
    `get_song_url` 调用点用适配层 `enrich_song_url_from_song` 补齐，
    不静默依赖 fallback。
14. **`SongUrl.qualities` 无 musid-dli 来源字段**：`StreamInfo` 不携带
    "全部可用档位"列表（那在 `Track.available_qualities` 上），映射产物恒
    空 `Vec`（§2.5 表）。全仓未发现读取方（只有生产方，wy/kg 内置源从
    `song.qualities` 收集），暂无害；V1 用同一
    `enrich_song_url_from_song` 补齐。
15. **`extra["isrc"]` 是保留键，键碰撞往返不保真**：`track_to_song` 中
    `Track.isrc` 有值时写入 `SongInfo.extra["isrc"]`（覆盖同名键），
    `song_to_track` 反向总是把 `extra["isrc"]` 提升进 `isrc` 槽位并从
    extra 删除。因此 `Track { isrc: None, extra: {"isrc": X} }` 经
    Track→SongInfo→Track 往返会变成 `Track { isrc: Some(X), extra: {} }`
    ≠ 原始输入（适配层测试 `extra_isrc_reserved_key_round_trip_is_lossy`
    pin 了该行为）。musid-dli 现有 provider 不产 `extra["isrc"]`（isrc 有
    专属槽位），属边角缺口而非现行缺陷；V1 如需严格保真可改为保留键
    检测报错（`AdapterError` 新变体）。

## 4. Transport 注入缺口（硬发现，如实上报）

§12.2 硬要求"voicefox 的代理/超时/网络配置注入 musid-dli Transport"
在已发布 **0.2.0 上不可实现**：

- `src/lib.rs:40-54`：`pub(crate) mod transport`：`Transport` trait/
  `ClientProfile`/`RetryPolicy` 形状齐全（`transport.rs:245-263`）但
  **crate 外不可见**；provider 的 `with_transport` 也是 `pub(crate)`
  （`source_netease.rs:73`）。
- 公开 API 只有各 `provider::new()`（内置 `ClientProfile::provider_default`：
  15s 总超时、无代理）。

处置：适配层保留显式占位 `TransportInjection`（构造即记录 voicefox 预算，
getter 供设置页展示；文档注明"上游公开 transport 后接通"）。proxy/超时
继续只驱动 voicefox 内置音源（`lx_source::http::configure`，
`source/src/http.rs:28-32`）。**升级前置（两步，缺一不可）**：
1. musid-dli 侧把 transport 至少按一个 feature（建议 `transport-inject`）
   公开；这是外层升级项，适配层不做 path 依赖或 fork 绕过。
2. **voicefox 侧开启 musid-dli 的 source-\* feature**：transport 模块只在
   十二个 reqwest-bearing feature 下编译（cfg 块与 `pub(crate) mod
   transport;` 在 musid-dli `src/lib.rs:40-54`），且全部 provider 实现
   （`source_youtube_music` :70 起、`source_joox` :103 止的十二个
   `source-*` 模块）都在 `source-*` feature 后
   （musid-dli `src/lib.rs:70-104`）；V0 的 `default-features = false`
   **不编译任何 transport/provider 面**（V0 依赖形态下 musid-dli 只出
   models + provider 核心类型）。即使上游照办第 1 步，不开 `source-*`
   就没有可注入、可构造的对象；开启会连带把 reqwest/aes/md-5/sha1/
   flate2/tracing/urlencoding/regex 拉进 feature 树（见 §8 V1 预告）。

## 5. V1-V4 迁移顺序草案

- **V0（本阶段）**：映射函数 + fixture 测试，零业务路径接触。
  依赖：crates.io `musid-dli = "=0.2.0"` optional + feature gate（§8）。
- **V1（双栈接入，已落地，语义与验收见 §11）**：
  1. 前置 (a) 上游公开 Transport 注入面 **且** voicefox 开启 musid-dli
     `source-*` feature（两步前置，见 §4：transport 与全部 provider 实现
     都在 `source-*` 后面）；(b) `AdapterMusicSource`
     实现 `MusicSource` fat-trait（`search→TrackSearch::search`、
     `get_song_url→StreamResolver::resolve`（Track 由 SongInfo 反向构造，
     TrackId 保真；调用点用 `enrich_song_url_from_song` 补
     cover_url/qualities，§3 第 13、14 条）、`get_lyric→LyricsProvider::lyrics`、
     `get_playlist_*→PlaylistProvider`、`get_album_songs→AlbumProvider::album_tracks`、
     `get_artist_songs→ArtistProvider`（保留 voicefox fat-trait 默认回退）、
     `get_leaderboard*→ChartProvider`、`parse_link→LinkResolver`（V0 骨架
     已预留该包装的语义位））；(c) voicefox CI 加
     `--features musid-dli-adapter` 的 test/clippy 与 no-default 两条 job；
     (d) **feature 增量**：`musid-dli-adapter` 追加按源子 feature（如
     `musid-dli-adapter-netease = ["musid-dli-adapter",
     "musid-dli/source-netease"]`，其余源同形），provider 实现与 transport
     面随子 feature 进入 feature 树（§8 V1 预告）。
     **V1 落地差异（如实登记）**：V1 实装收敛到三个运行时面（搜索/流地址/
     歌词，§11.4）；歌单/专辑/歌手/榜单/链接直解各面 V1 不接（影子源不进
     capabilities 聚合，这些方法在 manager 路径上不可达，接了即死代码），
     留给 V2 逐源迁移；(b) 的歌词转换变体依赖在 V1 如实成立
     （`musid-dli-adapter-lyrics`）；(d) 的子 feature 已按 §11.1 落地
     （bilibili 不设——`bili` 恒旧路径）。
  2. `SourceManager` 注册双栈源；session 统一（§2.9）。
  3. 依赖升档：`"=0.2.0"`→`"0.2"`（wire 形状已 pin，V0 禁漂移）。
- **V2（逐源迁移，已开篇，框架与账面见 §12；引擎收敛候选另列）**：
  下载引擎切片消费 `ResolvedUrl.fallback_endpoints` per-candidate 元数据；
  musid-dli `lx-js-bridge` 收敛评估（质量折叠 Lossless/HiRes→'flac' 需先
  对齐 voicefox JS 质量语义）；本地扫描合并候选（musid-dli `local`，避免
  双扫描器语义漂移）。**与 §12 的关系如实声明**：本条目原本只写引擎收敛
  候选；2026-11-21 盘点给出的逐源迁移序列（jamendo → fivesing → apple →
  bilibili → migu → kuwo → kugou → qqmusic → netease → qianqian → joox，
  soda/local 登记不迁）以盘点为准，落在 §12——两者不是同一清单，逐源
  迁移只覆盖四面（搜索/详情/流地址/歌词），集合面逐源保持旧路径并在各片
  登记 musid-dli 侧可用性。
- **V3（取消/缓存统一）**：musid-dli `resolve::resolve_candidates*` +
  `StreamCache` TTL 语义接入（适配层最小缓存，不改 lx-core 模型）；
  `SearchCancelToken` 硬挂接（§6.1）。
- **V4（收尾）**：`AuthState` 状态机上浮决策；本地库 G11 键集合并；
  移除 voicefox 侧重复实现（每步都按 §7 fixture 契约回归）。

## 6. 取消/超时/换源语义对齐方案

### 6.1 取消（§12.2）

voicefox 用 `JoinSet` drop + `tokio::time::timeout` 实现"宿主放弃即取消"
（`manager.rs:399-549`：每源 10s/8s）；musid-dli 提供协作式
`SearchCancelToken`（`AtomicBool`，`search.rs:19-113`）。适配层约定：
每次聚合并发给 `aggregate_search` 传 token，voicefox 层取消（future drop）
时同步 `token.cancel()`；`AggregationOptions` 对齐每源预算（voicefox 10s vs
musid-dli options 内字段）。聚合去重键以 voicefox 为准
`(source, extra[source], id)`（`manager.rs:536-542`）重建
SearchPage→SearchResult。

### 6.2 超时

voicefox 外层 30s/20s/10s/8s 各面超时保留；musid-dli per-request timeout 与
RetryPolicy 在 0.2.0 内部固化（`provider_default` 15s，注入面受 §4 限制）。
适配层语义上以 voicefox 预算为准；超时差异如实记录（本节），不掩盖。

### 6.3 过期 URL

musid-dli `StreamCache` 按 `expires_at_unix` 拒发过期 URL；voicefox 无槽位。
适配层：`ResolvedUrl.expires_at_unix` 伴随字段携带，播放器侧可用它提前重解析；
适配层不缓存过期值。V1 若双栈接入，在 adapter 层做最小 TTL 缓存对齐
StreamCache 语义而不是改 lx-core 模型。

### 6.4 换源

musid-dli 的 alternatives 是瞬态（`resolve_with_alternatives*` +
`matcher::rank_alternatives` 现场排序现场消费，G6 confirmed 不迁移
`toggle_source`）；voicefox 把匹配目标持久化在 `SongInfo.toggle_source`。
适配约定：跨源候选匹配可用 `matcher::match_breakdown`（version_tokens 分词 +
时长容差）或保留 voicefox 自己的 `find_music` 规则（时长 ±5s 严格过滤，
`manager.rs:1206`，**不放宽**）；匹配成功的 `SongInfo` 仍由 voicefox 侧写入
`toggle_source`；musid-dli 边界上该字段不存在，簿记不丢。

## 7. fixture 测试契约（含与盘点口径的偏差声明）

**偏差（如实声明）**：盘点要求"从 musid-dli 的 tests/fixtures 里取真实形状
JSON，反序列化成 musid-dli 模型"。现场核实（本阶段）：`musid-dli
tests/fixtures/` 里**与适配层消费面相关的目录**（netease/jamendo/apple/
fivesing/qqmusic/kuwo 等）的 JSON **是平台原始响应形状**，由 musid-dli
provider 的**私有**解析函数消费（`parse_track` 等均非 pub，
`source_netease.rs:351`）；原始 JSON 不能直接反序列化成
`musid_dli::models::Track/StreamInfo`，且 crate 外无法调用这些解析器。
非响应形状的例外如实登记：`matcher/version_matrix.json` 是 matcher 的
模型形状测试输入（target/candidate 带 provider/id/title/artists/album/
duration_secs，喂 `matcher::match_score` 而非 serde 模型）；`cue/`、
`local/`、`ncm/` 目录是音频/容器样本而非 JSON 模型文件。核心结论不变：
**没有任何 fixture 文件能直接反序列化成适配层消费的
`Track/StreamInfo/Playlist` serde 模型**。因此 V0 fixture 采用：

- **形状**：musid-dli 模型 wire 形状（serde wire 被 musid-dli 模型单测 pin：
  legacy JSON 兼容测试遍布 `models/`，`track.rs`/`stream.rs`/`playlist.rs`
  的 T1 legacy 测试即真实 wire 形状）；
- **样本值**：取自 musid-dli `tests/fixtures` 真实文件（netease
  `song_detail_legacy.json` 的 id 789/老接口/周杰伦·费玉清/老专辑/258000ms、
  `song_enhance_player_url_v1.json` 的 m801 URL/8523134/md5/320k 等），
  逐字段来源在 fixture 文件头 `derivation_note` 注明；
- fixture 落位 `source/tests/fixtures/musid/`（voicefox 仓内相对路径，
  `include_str!` 引用），不跨仓引用、不写绝对路径。

测试断言形状：fixture JSON → `serde_json::from_str::<musid_dli::models::X>`
→ 适配层映射函数 → `lx_core` 类型断言。另含：gap 源显式拒绝、md5 算法门、
fallback 顺序契约、strict chunk 降级、错误分类映射、能力位映射。

## 8. 依赖策略与 feature gating

```toml
[dependencies]
# V0：仅映射与测试，不接业务路径；默认构建完全不引入（feature 关闭无边）。
# V1：musid-dli 成为影子源（业务路径）的 regular optional dependency，
#     版本 pin 维持（"=0.2.0"→"0.2" 的放宽随外层 CI 矩阵落地，§11.1）。
musid-dli = { version = "=0.2.0", optional = true, default-features = false }

[features]
musid-dli-adapter = ["dep:musid-dli"]
# 歌词往返映射变体：V1 起歌词面双栈运行时需要它（缺它该面不生效 + warn）
musid-dli-adapter-lyrics = ["musid-dli-adapter", "musid-dli/lyrics-conversions"]
# V1 按源子 feature（bilibili 不设，§11.1；其余源同形）：
musid-dli-adapter-netease = ["musid-dli-adapter", "musid-dli/source-netease"]
# ... qqmusic / kugou / kuwo / migu / jamendo / apple / fivesing / qianqian / joox
```

- **`=0.2.0` 精确 pin**：模型 serde wire 在 0.2.0 已 pin，但 V0 映射测试的
  断言对 wire 形状敏感，禁 semver 漂移；V1 提升时再放宽到 `"0.2"`。
- **`default-features = false`**：musid-dli default = `["local","lyrics",
  "session-file"]` 会拉 lofty/walkdir/cue-rw/lyrics-helper/tempfile：模型层
  一个都不需要，V0 fixture 只需 serde。
- **与盘点口径的偏差**：盘点建议 V0 挂 `[dev-dependencies]`；但 ask 指定
  模块落位 `source/src/adapter_musid.rs`（lib 代码），lib 代码引用
  musid-dli 时必须是 regular optional dependency（dev-dep 下
  `cargo build --features musid-dli-adapter` 不成立）。按 ask 执行：
  optional regular dep，feature 关闭时 `cargo tree` 无 musid 边，默认构建
  不背 musid-dli。
- **验收命令（硬门定义）**：
  - `cargo test -p lx-source --features musid-dli-adapter -- adapter_musid`
    全绿：适配器面的**确定性硬门**（`--` 作用域过滤，不含既有 flaky 所在
    模块）；
  - 全量 `cargo test -p lx-source --features musid-dli-adapter` 以
    "**除 §0 登记的既有 flaky 外零失败**"口径解释：该 flaky
    （`cover_cache::tests::sweep_evicts_down_to_the_limit_by_oldest_access`，
    `source/src/cover_cache.rs:353`）是 mtime 秒级精度问题，并发全量跑
    间歇触发（本阶段实测：硬门命令连跑五次，一次 245 通过 / 1 失败、
    其余 246 通过 / 0 失败；隔离重跑稳定通过）。V0 不触碰业务路径，不在
    本阶段修复；
  - **V1 CI 前置（§5 (c) 的可执行条件）**：把该 flaky 修掉（voicefox
    业务侧独立修复，不得降低断言严格度）或 CI job 按作用域/名单拆分
    （排除名单里只允许出现该登记项），否则全量 CI 门不可靠；
  - `cargo test -p lx-source`（默认）基线不变（同口径解释）；
  - `cargo tree -p lx-source`（默认）grep 不到 musid-dli；
  - `cargo tree -p lx-source --features musid-dli-adapter` 能看到 musid-dli 0.2.0；
  - `cargo fmt --all` 干净。
- **V1 的 feature 增量预告（依赖账提前算清）**：V0 的
  `default-features = false` 下，musid-dli 编译面只有 models + provider/
  registry/resolve/search 等核心类型（SearchPage/QrLogin*/AuthState/
  ProviderSession），**没有任何 provider 实现**：`source_netease` 等模块
  全部在 `#[cfg(feature = "source-*")]` 后（musid-dli `src/lib.rs:70-104`），
  `transport` 也只在十二个 reqwest-bearing feature 下编译
  （musid-dli `src/lib.rs:40-54`）。因此 V1 双栈接入必须新增按源子 feature
  （如 `musid-dli-adapter-netease = ["musid-dli-adapter",
  "musid-dli/source-netease"]`），随之 reqwest/aes/md-5/sha1/flate2/
  tracing/urlencoding/regex 进入 **feature 树**（不是默认树）。本节的
  验收口径在 V1 保持同形、不用重议：默认构建（不开任何 adapter feature）
  依旧零 musid 边；"默认构建不背 musid-dli"的承诺不破；变化的只是
  `--features musid-dli-adapter-<source>` 树里多了 musid-dli 的 source 传递
  依赖，这本来就是双栈运行的必要成本，且仍按源粒度 optional。

## 9. 错误映射速查

musid-dli `Error`/`ErrorCategory`（`error.rs:241-267`，M2 taxonomy）→
voicefox `SearchError{Network,Parse,Api,Other}` /
`FetchError{Network,NotFound,TooManyRequests,Parse,Other}`
（`core/src/traits/source.rs:314-339`）：

| musid-dli | voicefox |
|---|---|
| `Network` / `Timeout` / `Unavailable` / `Io` | `Network` |
| `RateLimited` | `TooManyRequests` |
| `NotFound` / `ExpiredResource` | `NotFound`（Fetch） |
| `InvalidResponse` | `Api`（Search）/`Parse`（Fetch） |
| `Serialization` | `Parse` |
| `AuthenticationRequired`/`AuthenticationFailed`/`SessionExpired`/`PermissionDenied`/`VipRequired`/`RegionRestricted`/`CopyrightRestricted` | `Other`（**musid-dli `Display` 是英文原文**：`"login required"`、`"login required or session expired"`、`"authentication failed"`、`"permission denied"`、`"vip membership required"`、`"region restricted"`、`"copyright restricted"`，`musid-dli src/error.rs:110-240` 的 `#[error]` 属性（本会话实读
120-180 段核实后六个变体，`"login required"` 经适配层测试
`errors_map_to_fetch_and_search_semantics` 断言验证）；适配层 `error.to_string()` 原样透传，上屏文本含的是 `login`/`vip` 等英文关键词而非中文"登录"。UI 分流必须按错误语义变体/分类做，**不能按中文"登录"子串匹配**；中文展示由 voicefox UI 层按变体翻译） |
| `Unsupported`/`UnsupportedOperation`/`UnsupportedQuality`/`InvalidInput`/`Protocol`/`ProviderInternal`/`Provider`/`Cancelled` | `Other`（"该音源不支持…"） |

文本来源：`Error` 的 `Display`（thiserror）：`ErrorDetail` 消毒契约
（不泄 cookie/token/签名参数/完整 URL）必须保持，**不得把 voicefox 侧原始
reqwest 错误文本塞进 musid-dli `ErrorDetail`**。Display 文本是英文分类句
（见上表登录族行），适配层透传；中文展示由 voicefox UI 层按语义变体翻译。
反向（voicefox→musid-dli）
仅适配层内部需要，构造 `Error::Provider` 即可。

## 10. 注册语义（V1 接线用，V0 只记录）

`SourceManager::register` 以 `SourceId` 为键；musid-dli
`ProviderRegistry` 以 provider id 字符串 + capability slot 注册并做冲突检查
（`RegistryError::DuplicateRegistration`）。适配层为每个启用的 provider 建
一个 adapter 包装（§1 表 id→SourceId），同 id 多 capability trait 的冲突在
musid-dli 内部解决；`ProviderDescriptor` 的 version/priority/supported_qualities
供设置页展示。链接域名路由仍走 voicefox `SourceId::detect_from_link`
（host-side 保留），musid-dli `LinkResolver` 是 provider 内部解析、不做域
路由；`ResolvedLink::Artist` 变体 voicefox 无 → 适配层报不支持或降级为歌手
搜索；voicefox 的 Playlist/Album 变体自带 songs：适配层 resolve 后立即调
`playlist_tracks/album_tracks` 补齐。

## 11. V1 双栈接入（已落地，2026-11-21）

V1 把 musid-dli 提升为 `lx-source` 的可选正式依赖并接通三个运行时面
（搜索 / 流地址 / 歌词），全部 opt-in：**编译期 feature × 运行期 config
× 用户源 enabled 三重门**。硬约束的落点：

- 默认路径行为零变化：所有开关默认 false，影子 map 恒空，路由判定恒不
  命中；默认构建 lib 套件例数不变（220，§11.9）。
- musid-dli 不进默认 feature：`cargo tree -p lx-source`（默认）无 musid 边
  （§11.9）。
- 旧实现一行不删：路由是"槽位插入 + 失败回落"，不是替换。唯一例外是
  三处机械等价的重排（§11.11 如实登记）。
- SourceManager 应用级职责全部保留（多源调度/超时隔离/错误隔离/用户源
  优先级/内置与 JS fallback/歌词负缓存/换源/UI 状态）。

### 11.1 依赖与 feature 面

- musid-dli 维持 `optional = true` + `default-features = false` +
  `"=0.2.0"` 精确 pin（§8）；"提升为正式依赖"指的是它从"仅映射与测试"
  变为业务路径（影子源）的 regular optional dependency——V0 已是该形态，
  V1 增加的是**按源子 feature**（feature 名对照 musid-dli Cargo.toml 的
  `source-*`，注意 apple 是 `source-apple` 不是 `source-applemusic`）：
  `musid-dli-adapter-netease/qqmusic/kugou/kuwo/migu/jamendo/apple/fivesing/qianqian/joox`，
  均为 `["musid-dli-adapter", "musid-dli/source-<x>"]`。
- **bilibili 不设子 feature**：`bili` 恒旧路径（独立存储 + 扫码登录链路，
  §11.4）。
- provider 实现与 transport 面随子 feature 进入 feature 树（reqwest/aes/
  md-5/... 只进 feature 树，不进默认树）。
- `"=0.2.0"` → `"0.2"` 的放宽（§5 V1 步骤 3）**暂缓**：等外层 CI 的
  feature 矩阵跑通后再执行，V1 期间 wire 形状仍由 fixture 测试 pin。
- musid-dli 的 `lyrics-cache` / `session-file` / `download` / `lx-js-bridge`
  feature V1 均不启用（voicefox 负缓存/会话存储/下载引擎权威）。

### 11.2 配置面（core/src/model/config.rs SourceConfig）

新增 5 个字段，全部 `bool`、`#[serde(default)]`、默认 false；配置形状不随
feature 变化（字段恒解析）：

| 字段 | 语义 |
|---|---|
| `source.musid_stack` | 总闸；false 时四面全走旧路径 |
| `source.musid_search` | 搜索面（单源 + 聚合同规则） |
| `source.musid_song_url` | 流地址面（内置槽位前插 musid 判定） |
| `source.musid_lyric` | 歌词面（另需编译期 `musid-dli-adapter-lyrics` 变体） |
| `source.musid_parity_probe` | parity 对账影子模式（恒纯观测：恒展示旧路径结果，musid 调用仅用于计数且不阻塞用户路径） |

双维度 opt-in 的组合语义：config 开而 feature 关 → 该面静默走旧路径 +
启动时一次 warn（`lx_source::MUSID_ADAPTER_ENABLED` /
`MUSID_LYRIC_CONVERSIONS_ENABLED` 两个运行时常量把 cfg! 结果透给宿主，
因为 `cfg!` 只对本 crate 的 feature 生效）。接线点唯一：
`app/src/context.rs` 在 `set_js_url_first` 之后调用
`lx_source::bootstrap_musid_stack(&source_manager, &config.source, &config.network)`——
总闸关/feature 缺时该函数为 no-op 或仅 warn，不会注册影子源。

### 11.3 影子源注册（独立 map，不覆盖内置）

`SourceManager` 新增（均 `cfg(feature = "musid-dli-adapter")`）：

- `musid_sources: RwLock<HashMap<SourceId, Arc<dyn MusicSource>>>`——影子
  map 与 `sources` 完全独立。`register` 以 `SourceId` 为键、二次注册会覆盖
  内置实现，所以影子源走专用入口
  `register_musid_source(&self, Arc<dyn MusicSource>)`（`&self` + RwLock，
  app 侧 Arc 共享下可注册）。
- `set_musid_routes(MusidRouteConfig)`：下发 5 个开关（AtomicBool）。
- `musid_parity_snapshot()`：计数只读快照（设置页/诊断）。
- **health_check / capabilities 聚合 / `sources_supporting` /
  `leaderboard_sources` / `playlist_sources` / 登录链路（qr login/logout/
  is_logged_in）一律不读 musid map**：影子源不进健康检测、不改设置页入口
  显隐。测试 `musid_registration_never_touches_builtin_state` 用带
  `playlists=true` 能力位的影子源验证聚合位不翻真。

`AdapterMusicSource`（adapter_musid.rs，V1 新写）实现 voicefox
`MusicSource` fat-trait：`search→TrackSearch::search`（SearchQuery 拆装 +
search_page_to_result）、`get_song_url→StreamResolver::resolve`（song_to_track
保真 TrackId；JS 歌按 `builtin_source_for_js` 还原平台；**ResolveOptions
显式传档位**，禁 Default=High；stream_to_resolved_url +
enrich_song_url_from_song 补 cover_url/qualities；`expires_at_unix` 已过期
→ Err，绝不透传播放器（G4）；notes 走 debug 日志）、
`get_lyric→LyricsProvider::lyrics`（lyric_document_to_lx_data，lxlyric 恒
None，需歌词转换变体）、`get_cover_url` 只回填歌曲模型封面（ArtworkProvider
不接）、`supported_qualities` 经 §2.4 翻译。登录方法全部保持 trait 默认
（恒未登录/默认错误），`capabilities()` 仅回填 provider descriptor 位作
诊断对照。装配点 `source/src/musid_bootstrap.rs`：每个子 feature 一个
装配分支，同一 provider 实例按 Arc 克隆拆成 TrackSearch/StreamResolver/
LyricsProvider 三面（共享同一分配），constructor 走 musid-dli 无参
`new()`（provider_default 传输：15s、无代理）。

### 11.4 路由决策（四面）

共用判定 `musid_route(face_gate, source)`：总闸 && 面开关 && 源可映射
（`soda`/`local` 不在 §1 命名表、`bili` 显式排除——恒旧路径）&& 影子源
已注册 && 用户 enabled 集含该源。歌曲的路由平台：JS 音源搜到的歌
（`Local` + `extra[source]`）按 `builtin_source_for_js` 还原，普通歌曲取
`source`。**probe（`musid_parity_probe`）优先于替换且恒纯观测（评审 P1
修订）**：probe 开启时该面 musid 只作影子——旧路径权威、恒展示旧路径
结果，musid 的产物一律不进用户路径（含流地址面：旧链失败侧对账只计数，
旧错误原样向上且时延与不开 probe 一致）；对账全部在后台任务落账
（评审 P4 与第二轮 F1：聚合 spawn_search_shadow_reconcile、单源搜索与
歌词经 oneshot、流地址失败侧 tokio::spawn），用户路径零额外延迟，计数
最终一致。**取消语义（第二轮 F5 登记）**：影子与对账任务是
fire-and-forget——用户请求被取消或外层超时后，影子照常跑满自己的预算
（搜索 10s、歌词 20s、流地址对账 10s）并消耗 musid 配额、照常落账
（oneshot 发送端被弃的影子只浪费查询、不计）；计数因此可能包含用户从未
见到结果的对账，属诊断面的已知形态。

1. **搜索面**（`search_scoped` 单源 + `search_all` 聚合）：单源路径在
   enabled 检查后插入 musid 判定，musid 先行；聚合路径按源逐个路由进
   **同一 JoinSet**，同享每源 10s 超时与同一去重键 `(source,
   extra[source], id)`（manager 原键，adapter 产物无 `extra[source]`，
   天然同形）。**musid 尝试预算取每源预算前半（`MUSID_ATTEMPT_BUDGET`
   5s，评审 P2）**：musid Err **或预算耗尽**后立即按旧路径重试该源，回退
   继承剩余预算——musid 慢失败/挂死不再把回退挤到归零，替换模式总时延
   ≤ 基线每源 10s（单源路径同切分，整体受 app 外层 12s 约束）。probe
   影子任务限 10s、在后台对账（评审 P4），聚合结果不被最慢影子拖住。
   musid-dli `aggregate_search` 不引入——多源调度/超时隔离是
   SourceManager 应用级职责（硬边界）；`SearchCancelToken` 在 V1 **无接缝**
   （0.2.0 的 `TrackSearch::search` 无 token 参数，token 只被未引入的
   `aggregate_search*` 消费），取消完全依赖 voicefox 的 future-drop
   （JoinSet/timeout），硬挂接留 V3（§6.1）。
2. **流地址面**（`get_song_url_inner`）：本地/bili 早退后、内置槽位前插
   musid 判定；`js_first` 语义不变——false=musid 先于内置先行槽，
   true=JS 先于 musid（musid 槽位在 JS 循环之后、内置兜底之前）；
   `js_start_index` 续播机制零改动（续播轮次 musid 槽位仍参与，V1 未记
   跳过状态，配额按轮重复消耗——评审 P8 确认已登记）。musid resolve
   尝试限 [`MUSID_RESOLVE_BUDGET`] 10s（评审 P3：musid 内部固 15s
   `provider_default` 不可注入，预算在宿主侧收口）——Err/超时 → 继续既有
   内置→JS→内置兜底链（错误文本逐字保持原实现），回退链预算 ≥20s；
   **残余退化如实登记**：对比基线满 30s 外层预算，musid 尝试至多占用
   10s，慢失败场景下回退链可用预算被压缩（≤20s）。
3. **歌词面**（`get_lyric_inner`）：musid 槽位与 JS 槽位同位——JS 之后、
   内置 fallback 之前；产物过同一内容门 `lyric_has_content`；None/空内容/
   Err → 继续链、不写负缓存；musid 确认无词时与旧路径同口径写同一负缓存
   键（负缓存仍由 `get_lyric_with_fallback` 统一裁决）。该面整体需要
   `musid-dli-adapter-lyrics` 变体（缺变体时 musid 歌词槽位与 probe 双跑
   均不生效 + 启动 warn）。musid-dli 自带 `LyricsCache` 不启用，voicefox
   负缓存权威（`(provider, raw_id)` 键换算留 V3）。
4. **歌曲详情面**：无独立运行时开关（如实登记，非漏项）——voicefox
   fat-trait 无 by-id 详情方法、musid-dli 0.2.0 无公开 detail trait
   （netease `track_details` 是 crate 内部方法）；详情由承载面携带：搜索
   产物经 search_page_to_result 逐条 track_to_song、resolve 入参经
   song_to_track 保真（专辑 id 不模仿缺省回落，§2.1）。若 V2 需要
   by-id 详情钩子（如 wy 歌单同步 `songs_from_detail` 路径），前置是上游
   公开 per-provider detail trait。

### 11.5 回退语义与计数（source/src/parity.rs）

`ParityCounters`（`Mutex<HashMap<(ParityFace, SourceId, ParityOutcome), u64>>`，
内存计数不落盘，`Arc` 共享供 JoinSet 任务记录）：

- `MusidOk`：musid 路径成功给出产物（替换模式命中；probe 模式流地址面
  失败侧对账命中——产物不进用户路径，评审 P1）。
- `MusidErrLegacyOk`：musid 失败（Err/空产出/过期地址/尝试预算耗尽），
  旧路径同请求成功。
- `MusidErrLegacyErr`：两栈同请求都失败。
- `ParityMatch` / `ParityMismatch`：probe 真双跑的对照结论。

各面口径（不是所有 outcome 在所有面都产生，如实登记）：

| 面 | 替换模式 | probe 模式 |
|---|---|---|
| 搜索 | MusidOk / MusidErrLegacyOk / MusidErrLegacyErr（单源在 `musid_search_single` 计；聚合在各源任务内计；musid 尝试预算耗尽也回退并计——评审 P2） | 双 Ok 才对照（ParityMatch/Mismatch；聚合在后台任务 `spawn_search_shadow_reconcile` 计、单源在影子任务经 oneshot 拿到旧路径产物后计——均不阻塞用户路径，评审 P4，计数最终一致）；单侧瞬时 Err 不计（无双方产物可归一）；双 Err 记 MusidErrLegacyErr |
| 流地址 | 同搜索三态（在 `get_song_url_inner` 的成功/错误出口计；err 侧为纯簿记，零时延） | **只做失败侧对账且恒纯观测（评审 P1；第二轮 F1 后台化）**：旧链成功不双跑 resolve（避免双倍平台配额，零调用零计数）；旧链失败才在后台任务跑 musid（限 10s）——命中记 MusidOk、失败记 MusidErrLegacyErr，**musid 地址绝不返回**，旧错误原样向上且时延与不开 probe 一致；计数最终一致 |
| 歌词 | 槽位命中记 MusidOk；err 侧 legacy 归因仅在 `get_lyric_with_fallback` **主请求链**记（评审 P7：本地外挂→JS→musid→内置 fallback 的同请求 rescue 才算 ErrLegacyOk，find_music 跨源候选的 rescue 不归因到路由源平台——该情形不计；Err 与空产出同归因；跨源补全候选的 musid 槽位只计 MusidOk） | 双 Ok 按内容对照（双方都确认无词 = ParityMatch；单侧有词 = ParityMismatch）；单侧 Err 不计；双 Err 记 MusidErrLegacyErr；影子在后台任务对账（评审 P4） |

错误分类经 fetch_error_from_error/search_error_from_error（§9 表，禁止按
中文子串匹配）。**整体外层超时不计**（无双方结局）：聚合替换模式的两栈
（musid 尝试 + 回退）都在每源 10s 预算内挂死时，任务整体超时，不计（测试
`search_all_both_stacks_timing_out_counts_nothing` pin）；musid 尝试预算
（5s）耗尽但回退完成时正常计数（测试
`search_all_musid_hang_falls_back_within_the_budget_split` pin）。V1 只计数
不自动熔断（连续失败自动禁用 musid 是 V2 候选）。**probe 计数为后台落账、
最终一致**（评审 P4 与第二轮 F1）：读取快照用于诊断时存在短暂滞后，属
预期；用户取消/外层超时后影子照常跑满预算并落账（见 §11.4 取消语义），
计数可能包含用户未见结果的对账。

### 11.6 parity 归一对照规则

- 歌曲对（`parity::songs_equivalent`）：id 精确相等；name/singer/album 经
  `filter_str`（+ singer 先 `sort_singer`）后相等——与 find_music 同一套
  归一（manager.rs:1192-1204）；album 任一侧空串视为缺失不比；时长两侧
  都非 ZERO 才比、差 ≤1s；qualities 集合相等（adapter 产物在
  track_to_song 已按 §2.4 折档，Standard→Low128、Unknown 掉出，直接比
  折档后集合）。
- 搜索产物集（`compare_search_items`）：条数一致且 musid 侧每条都能按 id
  精确配对到一条等价 legacy 歌曲；`total`/`has_more` 不对照（§2.7
  `None→items.len()` 语义差如实豁免）。
- 歌词（`compare_lyrics`）：`lyric` 正文逐行去空白去空行后的（行数，首行
  `[...]` 时间戳标签）指纹相等；翻译/逐字不比（musid-dli 无 YRC/QRC
  序列化器，lxlyric 恒 None，逐字对照无法成立）。

### 11.7 Transport 注入现状（P9，硬发现维持）

musid-dli 0.2.0 的 `transport` 模块与 `with_transport` 均 `pub(crate)`
（本次复核 `src/lib.rs:40-54` 属实），**voicefox 的代理/超时在 0.2.0 上
无法注入**。V1 落地形态：影子源构造时
`TransportInjection::new(config.network.proxy_url, config.network.timeout)`
记录预算（`AdapterMusicSource::transport_injection()` getter 供诊断），
proxy/超时继续只驱动 `lx_source::http::configure`。升级前置两步缺一不可
（§4）：上游公开 transport feature + voicefox 开 source-\* 子 feature；
不做 path 依赖或 fork 绕过。注入接通后 musid_bootstrap 的装配点是唯一
改动位置。

### 11.8 V1 显式边界（不接清单）

- **换源/匹配**：`find_music`/`find_music_on`/auto_toggle/policy 候选
  收窄完全走 voicefox 旧路径（±5s 严格过滤不放宽）；musid-dli
  `matcher::match_breakdown` 不接线、无配置项（不给可误开的开关）。
  musid 搜到的歌（source 非 Local）天然参与既有换源链；匹配成功的
  SongInfo 仍由 voicefox 写 `toggle_source`（musid 边界上该字段不存在）。
- **登录/会话**：扫码（bili/tx/wy）与 cookie 会话全走旧路径；
  musid-dli `ProviderSession`/`FileSessionStore` 不接线；
  `capabilities_from_provider` 仅用于 AdapterMusicSource 自身
  capabilities()（诊断对照），不改 `sources_supporting` 显隐。
  `wechat_login`/`vip_account` 双栈期恒 false。**双栈零凭据流动**
  （评审 P5 登记）：musid provider 全部以无参 `new()` 构造（匿名），voicefox
  侧无任何 cookie/session 接线、musid-dli `session-file` feature 未启用
  （§11.1）——不存在"双栈并发串账号"风险；反面后果两条：
  (a) 替换模式下对内置栈已 cookie 登录的用户，musid 面恒匿名，VIP/登录态
  曲目的结果集与档位可能变窄甚至退化（musid 命中不等于登录态等价）；
  (b) probe 对账在登录源上是"内置带登录态 vs musid 匿名"的不对称双跑，
  会产生系统性 ParityMismatch 噪声——解读 §11.5 计数时必须把登录源
  （wy/tx/kg/bili 等 cookie 类）的 mismatch 与真实行为差异区分开。
- **歌单/专辑/歌手/榜单/链接直解**：影子源不实现这些 fat-trait 方法
  （保持 trait 默认错误）——影子源不进 capabilities 聚合，这些路径经
  manager 恒走内置，接了即不可达死代码；V2 逐源迁移时再按 §5 步骤 2
  评估。
- **musid-dli aggregate_search / SearchCancelToken / StreamCache /
  LyricsCache / 下载引擎**：V1 均不引入（§11.4/§11.5）。

### 11.9 测试与门禁（V1 复跑结果）

- `cargo fmt --all`：已执行，`--check` 干净。
- 确定性硬门 `cargo test -p lx-source --features musid-dli-adapter --
  adapter_musid`：35 通过 / 0 失败（V0 的 29 + 新增 6 例 AdapterMusicSource
  单测）；歌词变体下 40 通过 / 0 失败。
- 新增集成测试：`source/tests/musid_dual_stack.rs`（路由判定 / 回退计数 /
  尝试预算切分 / js_first 语义 / probe 影子与不阻塞用户路径（搜索面 +
  流地址错误路径）/ 整体超时不计 / 会话隔离 / 负缓存语义；歌词面用例挂
  `musid-dli-adapter-lyrics` 门）——adapter feature 下 19 例（`--list`
  口径）/ 歌词变体下 23 例全绿；`source/tests/musid_parity.rs`（聚合 probe
  真双跑对照 / 归一差异 / 单侧失败豁免 / 模式计数分离 / 累计）6 例全绿。
  probe 计数断言按"后台落账最终一致"轮询（`settled_count`）。
- 默认基线 `cargo test -p lx-source`：lib 220 例（218 通过 / 0 失败 /
  2 ignored）——默认 lib 例数与 V0 完全一致，musid 相关 lib 单测全部挂
  feature 门；新集成测试文件 `#![cfg(feature)]` 在默认构建下编译为空。
  **§0 登记 flaky 已根因修复（评审第二轮 F2）**：`sweep_in` 按 **atime**
  排序淘汰，而测试助手旧实现只钉 mtime——relatime/秒级精度下六个文件的
  atime 全是创建时刻的近同值，排序不稳定。现用 `std::fs::File::set_times`
  （1.75+）把 atime/mtime 一起显式钉死，断言一字未动；复跑验证：默认
  lib 全量连跑 6 次 + 定向用例连跑 8 次 + cover_cache 模块连跑 6 次，
  全部 0 失败（修订前实测约 1/6 全量跑触发 1 次）。§8 的"修掉 flaky 或
  CI 按名单拆分"前置以第一种方式兑现，musid-stack job 维持无作用域全量。
- `cargo tree -p lx-source`（默认）无 musid-dli 边；
  `--features musid-dli-adapter` 可见 `musid-dli v0.2.0`；
  `--features musid-dli-adapter-netease` 连带 reqwest 等进入 feature 树。
- `cargo check -p lx-source`（默认 / adapter / netease 子 feature /
  lyrics 变体）与 `cargo check -p voicefox-app`（默认 +
  `--features lx-source/musid-dli-adapter-lyrics,lx-source/musid-dli-adapter-netease`）
  均零错误零警告。
- **CI 覆盖已闭环（评审 P6）**：`.github/workflows/ci.yml` 新增
  `musid-stack` job（fmt-check / clippy `-p lx-source --all-targets` 开
  `musid-dli-adapter` 与 `musid-dli-adapter-lyrics` 两形态 `-D warnings` /
  默认 `cargo tree` 零 musid 边断言 / adapter 与歌词变体两形态
  `cargo test -p lx-source` / 默认基线）——§11 全部 cfg 代码与两个集成
  测试文件在 CI 上可复现，不再可静默烂掉。V0 §8 的 flaky 修复口径继续
  有效（该 job 与既有 workspace 测试 job 同口径承受同一登记 flaky）。
- 未跑（外层门禁职责）：全仓 clippy/test、feature 全矩阵、MSRV、三平台
  构建（CI job 覆盖的是 lx-source 单包两形态）。

### 11.10 评审修订（2026-11-21，逐条对应评审意见；第二轮 F1-F5 追加于后）

- **P1（probe 语义泄漏）**：流地址面 probe 从"旧链失败跑 musid 并返回其
  地址兜底"改为**恒纯观测**——只计数（MusidOk/MusidErrLegacyErr），musid
  产物不进用户路径，旧错误原样向上。五处"恒展示旧路径结果"声明
  （§11.2 表、§11.4、config.rs、config.example.toml、MusidRouteConfig
  注释）与实现恢复一致；测试 `probe_song_url_runs_musid_only_after_the_legacy_chain_fails`
  改为 pin 旧错误向上 + MusidOk 计数 + musid 确实被调用。
- **P2（聚合回退饥饿）**：替换模式的 musid 尝试独立给预算
  `MUSID_ATTEMPT_BUDGET` 5s（每源 10s 前半；单源路径同切分），Err/预算
  耗尽后回退继承剩余预算——musid 慢失败/挂死不再把回退挤到归零；总时延
  ≤ 基线。新测试 `search_all_musid_hang_falls_back_within_the_budget_split`
  / `search_single_musid_hang_falls_back_after_the_attempt_budget` pin；
  "整体超时不计"收窄为"两栈都挂死"（`search_all_both_stacks_timing_out_counts_nothing`）。
- **P3（resolve 挤占 30s）**：musid resolve 槽位宿主侧收口
  `MUSID_RESOLVE_BUDGET` 10s（`musid_url_try` 与 probe 失败侧对账同限）；
  回退链预算 ≥20s，残余退化在 §11.4 登记。
- **P4（probe 拖慢用户路径）**：三类 probe 对账全部改后台任务——聚合
  `spawn_search_shadow_reconcile`（fire-and-forget）、单源搜索与歌词经
  oneshot 交接旧路径产物/结论给影子任务；计数最终一致（§11.5 登记）。
  新测试 `probe_shadow_does_not_block_the_user_path` pin（musid 影子挂死时
  单源搜索 <1s 返回）；probe 计数断言改轮询（`settled_count`）。
- **P5（会话不对称）**：§11.8 登记"双栈零凭据流动 + (a) musid 面恒匿名
  退化 (b) probe 在登录源上的系统性 mismatch 噪声"。
- **P6（CI 零覆盖）**：ci.yml 新增 `musid-stack` job（见 §11.9）。
- **P7（歌词 err 侧跨源归因）**：`MusidErrLegacyOk` 归因收窄到主请求链
  （本地外挂→JS→musid→内置 fallback）；find_music 跨源候选 rescue 不归因
  （不计），新测试 `lyric_cross_source_rescue_is_not_attributed_to_the_routed_source`
  pin。
- **P8（续播轮次重试 musid）**：维持已登记口径（V1 未记跳过状态，§11.4
  加交叉引用）。

**第二轮评审修订（2026-11-21）**：

- **F1（probe 对账时延声明与实现不符）**：流地址面失败侧对账原为内联
  await（错误返回前同步等 musid resolve 至多 10s），"用户路径零额外延迟"
  对该面为假。改实现为后台化：`musid_url_err_exit` 的 probe 分支
  tokio::spawn 对账任务（限 `MUSID_RESOLVE_BUDGET`），旧错误立即向上，
  时延与不开 probe 一致——§11.2/§11.4/config 注释的"不阻塞用户路径"
  声明对四面成立，无需收窄。新测试
  `probe_song_url_reconcile_does_not_block_the_error_path`（musid resolve
  挂死 → 错误 <1s 返回 + 预算耗尽后 MusidErrLegacyErr 后台落账）；
  原 URL probe 用例的计数断言改 `settled_count`。
- **F2（CI 承受登记 flaky）**：根因修复而非 CI 拆名单——见 §11.9 默认
  基线条（`sweep_in` 按 atime 排序、测试助手旧实现只钉 mtime；修复用
  `File::set_times` 钉死两个时间戳，断言未动，连跑 6+8+6 次全绿）。
  musid-stack job 维持无作用域全量测试，§8 前置以"修掉 flaky"路径兑现。
- **F3（头部门禁数据错误）**：头部 musid_dual_stack 例数改为实测口径
  （adapter 19 / 歌词变体 23 / 默认构建 0），全量 lib 例数统一为
  "总例数 = 通过 + 失败 + ignored"口径（adapter 260、歌词 265）。
- **F4（测试注释与 §11.5 口径矛盾）**：musid_parity.rs 注释改写——单侧
  失败场景不计；probe 双 Err 记 MusidErrLegacyErr（§11.5 搜索面 probe 列），
  MusidErrLegacyOk 属替换模式归因。
- **F5（取消语义未登记）**：§11.4 probe 总则与 §11.5 登记——影子/对账
  fire-and-forget，用户取消/外层超时后照常跑满预算（搜索 10s / 歌词 20s /
  流地址对账 10s）并消耗 musid 配额、照常落账；oneshot 发送端被弃的影子
  只浪费查询、不计。

### 11.11 旧代码的重排登记（"一行不删"的例外）

V1 为承载路由对三处旧实现做了机械等价重排，语义逐字保持（默认构建
lib 例数不变 + 全量回归双证）：

1. `search_all` 的单源任务返回值从裸超时结果改为 `(SourceId, 超时结果)`
   元组——probe 影子对照需要按源配对；join 分支同步加一层元组解构。
2. `get_lyric_inner` 返回值从裸 `Result` 改为 `(Result, Option<SourceId>)`
   ——err 侧 legacy 归因需要"musid 槽位已尝试且无产出"轨迹；尾部的
   `ok_or_else(...)?` 等价改写为 let-else。
3. `get_song_url_inner` 尾部三个错误出口收敛为单一 `final_error` 出口
   （错误文本逐字保持），probe 兜底/err 侧计数在统一出口评估。

## 12. V2 逐源迁移框架（2026-11-21 落地；切片 1 = jamendo 模板）

V2 把 V1 的"四面通用路由"细化为**逐源逐面**的迁移账面：每源一行、每面
一格的注册表（§12.1）决定该源该面是否经 musid-dli；musid 侧能力缺失/
语义不等价的面回落旧路径并登记（§12.2）；每个切片按统一的 fixture 方案
做 parity 对账（§12.3）。盘点结论（2026-11-21，实读双仓）确认 V1 代码面
上 10 源全部"已接"，差异只在子 feature 与运行期 config，因此 V2 逐源迁移
= 每源对账 + 改账 + 登记，不需要新适配代码（唯一例外是 bilibili 切片的
三件结构前置，已随切片 4 落地，§12.4/§12.6）。

与 §5 引擎收敛候选的关系见 §5 的如实声明；`soda`（voicefox-only，
musid-dli 无此源）与 `youtube-music`（musid-dli-only，voicefox 侧无对应
源）**登记不迁**（§12.4 末两行）。

### 12.1 逐源路由注册表（`adapter_musid.rs`）

三态账面 `FaceRoute`：

| 状态 | 路由判定 | 语义 |
|---|---|---|
| `Migrated` | 放行 | V2 切片已落地：对账 fixture 与回落规则已登记（§12.4） |
| `V1Routed` | 放行 | V1 双栈已路由、V2 对账切片未落地：运行行为与 V1 完全一致（基线不破坏的关键——V1 的 musid_dual_stack/musid_parity 用例以 Wy 为载体继续 pin 该账面） |
| `LegacyOnly(reason)` | 拒绝 | 恒旧路径并登记原因；musid 的"该音源不支持"类错误永远到不了用户 |

注册表行 `SourceRouteEntry { source, slice, search, song_url, lyric }`，
消费点唯一：`manager::musid_route` 在 V1 判定链（总闸 && 面开关 && 命名表
可映射 && 非 Local/Soda/Bili && enabled && 影子源已注册）**之内**追加
`adapter_musid::face_route(face, source).routes_to_musid()`。`soda`/`local`
无行 → `LegacyOnly`（命名表拒绝的镜像）；`bili` 行为 LegacyOnly 三面，与
manager 既有 `matches!` 排除双保险、语义相同。面级改账独立：切片落地时逐
格改 `Migrated`/`LegacyOnly`，其余源不受影响（**每源独立可回退**——改回
一格即回退）。集合面（歌单/专辑/歌手/榜单/链接直解）、登录/会话面、歌曲
详情面不设行项：影子源未实现 fat-trait 集合方法/登录方法（V1 边界
§11.8）、详情无独立运行时开关（§11.4 第 4 条），manager 路径不可达。

V1 判定链的次序保持不变（注册表检查插在命名表检查之后、enabled 检查之
前）；probe 影子与替换模式同经 `musid_route`，所以**未迁移面的 probe 也
被注册表拒绝**（不烧 musid 配额、不产无意义计数）。

**Migrated 行的生效前提与脱节暴露（评审 P4）**：行的放行还要求对应
`musid-dli-adapter-<source>` 子 feature 在本次构建中启用（影子源被
`musid_bootstrap` 装配）。子 feature 未启用时影子源不在 map 里、路由判定
返回 None → 该源**静默**走旧路径且 parity 计数恒零（V1 既有形态：bootstrap
只在"一个子 feature 都没开"时 warn，单源缺失原本无任何信号）。V2 起该
脱节显式暴露：`adapter_musid::unregistered_migrated_sources(registered)`
在 `bootstrap_musid_stack` 启动时对"账面 Migrated 但未装配"的源逐源 warn
（`migrated_route_sources`/`unregistered_migrated_sources` 为公开助手，
单测 pin）。账面"已落地"因此始终可从启动日志与运行期计数两侧核验。

### 12.2 能力缺失回落规则（三层）

硬约束：**能力缺失回落旧路径，不是把 Unsupported 直接抛给用户**。三层：

1. **静态层（注册表）**：musid 侧显式 stub/语义不等价的面在注册表标
   `LegacyOnly`，路由判定直接拒绝。判定依据是 musid-dli provider 的
   capabilities/实现形态（编译期可查），不做运行时 capability 探测。
   首例：jamendo 歌词面——musid `LyricsProvider` 是显式 unsupported stub
   （musid-dli `source_jamendo.rs:538-542`，"the CC catalog exposes no
   lyrics"），voicefox 旧路径恒 `FetchError::NotFound`
   （`jamendo/mod.rs:421-424`）；若路由，无词语义会从 `NotFound` 污染成
   `Other("该音源不支持…")`（§9 错误映射表的分类语义差异），故不路由。
2. **动态层（运行期，V1 语义不变）**：路由放行的面里 musid 失败（Err/
   空产出/过期地址/尝试预算耗尽）→ 立即旧路径同请求重试（§11.4/§11.5
   的预算切分与计数口径原样生效）。首例：jamendo 流地址面，musid
   `resolve` 从 `track.extra["stream_url"]` 离线取址（musid-dli
   `source_jamendo.rs:402-444`，搜索时嵌入）——legacy 搜到的歌 extra 无
   该键，musid 必然失败（Provider 类错误 → `FetchError::Other`）→ 旧链
   `fetch_tracks + pick_stream` 兜底，计 `MusidErrLegacyOk`。musid 搜到的
   歌经 `track_to_song`/`song_to_track` 的 extra 原样透传，嵌入地址回到
   musid，离线 resolve 成立（adapter 单测 pin 该数据链）。
3. **观测层（probe）**：恒纯观测（评审 P1 口径不变），musid 产物不进
   用户路径；probe 影子同经注册表门（§12.1）。

**jamendo 流地址面：快照取址的新鲜度风险（第二轮评审登记，2026-11-21）**。
musid 路径的取址语义是**搜索时内嵌的快照 URL**（musid-dli
`jam_parse_track` 在搜索时把 best-tier URL 嵌入 `extra["stream_url"]`，
`resolve` 恒离线返回该快照，`source_jamendo.rs:402-444`）；legacy 路径是
播放时 `fetch_tracks` 现取。搜索面已 Migrated → musid 搜到的歌走快照取址
是 jamendo 的**常态路径而非边角**。风险与观测口径逐条登记：

- **上游自证的 token 缺口**：musid-dli
  `tests/fixtures/jamendo/README.md:40-41` 明文 "token validity is
  unknown (an audit gap) and nothing pins the token bytes"——URL 形状被
  pin，token 字节与有效期无任何背书。
- **stream 组 URL 携带 `from=` token**：download 组为空的曲目内嵌的是
  stream 组带 `from=` token 的 URL（上游 fixture track 741555 "Placeholder
  Stream-Only" 即此形状，`search_response.json:35-43` + `source_jamendo.rs`
  测试注释 "Track 2 has an empty download dict, so the stream group's mp31
  URL is embedded instead"）。
- **G4 过期门不设防**：musid `resolve` 产物 `expires_at_unix: None`
  （`source_jamendo.rs:432`），voicefox 的 G4 过期门
  （`adapter_musid.rs:1008-1015`）只拦"带过期时间且已过期"的地址，对这类
  无信号过期无法拦截。
- **动态层回退的盲区**：动态层只在 resolve **失败**时触发；陈旧 URL 在
  resolve **成功**（确定性返回同一快照）之后才在播放器侧 403/播放失败
  ——`MusidErrLegacyOk` 不计数、legacy 现取不会被尝试，播放器侧失败也不
  进任何 parity 计数（§11.5 的计数口径只覆盖 resolve 结局）。
- **观测口径**：§12.3 的"盯 MusidErrLegacyOk"只能发现 resolve 失败型
  回退，**不能**作为本风险的健康信号；有效观测靠诊断面之外的播放失败
  信号（播放器首字失败/403 日志，区分源维度）。若实测出现系统性失效，
  处置选项按 §12.1 执行：把 jamendo 流地址面单独改账 `LegacyOnly`
  （每源独立可回退，一行改动），或等上游补 expires 信号/现取端点后再改回。

**fivesing 三面（切片 2）的对账登记（2026-11-21）**。fivesing 与 jamendo 的
关键差异：musid `FivesingProvider::resolve` 是**现取**语义（恒走 getSongUrl
网络端点，musid-dli `source_fivesing.rs:546-596`），没有 jamendo 式"搜索时
内嵌快照"的动态层盲区——musid 失败 → 旧链重试的动态层（本节第 2 层）在该
面正常闭合（manager 级真实双侧链路：musid Provider 错误 → legacy
`FetchError::NotFound`、`MusidErrLegacyErr` 计数，`musid_fivesing_slice.rs`
pin）。逐条登记的差异：

- **Referer 头**：legacy `SongUrl` 恒挂
  `Referer: http://5sing.kugou.com/`（`fivesing/mod.rs:110`，播放侧消费点
  `application/src/services/playback.rs:123` 应用 `url.headers`）；musid
  `StreamInfo.headers` 恒空（`source_fivesing.rs:377`）。上游 fixture
  README 明文登记 live probe 结论（2026-11-18，4KiB Range 请求**无**
  Referer 得 `206 audio/mpeg`）：**CDN 已不再强制 Referer**，musid 对齐
  live 行为，legacy 侧头是历史残留、无害——不做适配层改写，适配层单测
  pin 空 headers 为登记载体。
- **URL 时效**：getSongUrl 直链内嵌时间戳+hash 路径段（上游 README 明文
  "so they expire"）且无 expires 信号 → G4 过期门
  （`adapter_musid.rs:1008-1015`）不设防；两侧同是播放时现取，同病，
  不存在 jamendo 式"快照成功型失效"回退盲区（musid 现取失败时动态层
  照常回落旧链现取）。
- **getSongUrl 的 md5/size 两侧同弃**（上游 README 登记项）：live data 块
  带 `squrlmd5`/`sqsize` 等键，legacy 留空 `SongUrl.md5/size`、musid 留空
  `content_length/checksum`——对照面无分歧。
- **搜索面 qualities 簿记分歧（probe 信号限定）**：legacy `search_songs`
  不填 `SongInfo.qualities`（`fivesing/api.rs`）、musid
  `fs_parse_search_song` 静态声明 `[Lossless, High, Low]`
  （`source_fivesing.rs:422-426`）→ `songs_equivalent` 的 qualities 集合
  比对（`parity.rs:180`）对 fivesing **系统性 false**：probe 搜索面的
  ParityMatch/Mismatch 计数对该源无鉴别力（adapter 单测 pin"唯一分歧
  字段是 qualities"，身份字段 id/name/singer/时长全对齐）。解读口径与
  §11.8 (b) 登录源 mismatch 噪声同类：簿记差异，不是歌曲身份差异。musid
  侧携带的静态三档是"5sing 曲库档位声明"，UI `quality_label` 因此可能对
  musid 搜索产物显示档位而 legacy 显示 "-"——信息更全，不是退化。

**切片 3-7（apple / bilibili / migu / kuwo / kugou）的对账登记
（2026-11-21，v2-batch-1）**。五个源的可路由面 musid 侧 capabilities 均
true 且无 jamendo 式"搜索时快照"动态层盲区（全部网络现取），逐源登记的
差异如下：

- **apple**：
  (a) preview-only 契约两侧同形——musid `resolve` 忽略请求档位恒回 30 秒
  M4a 试听（quality Low、duration 30s、descriptor `supported_qualities`
  = [Low]，`source_apple.rs:589-617` 注释 pin）；legacy `apple/mod.rs:75-99`
  同样恒 Low128 试听。quality parity 可严格断言双侧均 Low/Low128。
  (b) `SongUrl.duration` 分歧：legacy 带整曲时长（`apple/mod.rs:89`
  `detail.song.duration`）、musid 带 preview 物理时长 30s。
  (c) 搜索面 qualities 簿记分歧（同 fivesing 口径）：legacy `parse_song`
  不填档位、musid 静态 `[Low]→{Low128}` → probe 搜索面对 apple 恒
  ParityMismatch（簿记差异；adapter 单测 pin 唯一分歧字段）。
  (d) 歌词面 PlainText 无时间戳（catalog 无 timing）→
  `document_to_lyric_data` 的 `lyric` 为空、原文落 `raw_lrc`——musid
  槽位产物过不了 `lyric_has_content` 内容门，动态层恒回落旧路径
  （MusidErrLegacyOk），**无错误语义污染**（musid 返回 Ok 非错误）；
  V1Routed 时该面已路由，本切片不改运行行为。
  保持旧路径：歌单分类/歌单（musid PlaylistProvider categories/playlists
  恒 Unsupported）、专辑面（musid AlbumProvider 存在但集合面不路由）、
  链接直解；歌手/榜单双侧皆无。
- **bilibili**：三件结构前置已落地——子 feature `musid-dli-adapter-
  bilibili`（source-* 名 `source-bilibili`）、bootstrap 匿名装配分支
  （`BilibiliProvider` 恒无参 `new()`）、manager 路由放开（`matches!`
  排除移除 + 流地址面 bili 早退臂前插 musid 槽位，旧早退臂逐字保持为
  musid 之后的旧路径；簿记三件套与主链同款）。
  (a) 歌词面 LegacyOnly：双侧皆无——musid LyricsProvider 硬 stub
  （"no lyric API exists on Bilibili"，capabilities.lyrics=false）+
  voicefox `bili/mod.rs:497-499` 恒 NotFound；不路由，旧路径 NotFound
  语义不被 musid UnsupportedOperation 污染。
  (b) 登录/会话/用户数据恒旧路径：musid authentication=true（Cookie/
  SESSDATA，descriptor 诊断位 login=true）但影子源恒匿名（§11.8 零凭据
  流动）；manager 登录链路（create_qr_login/check_qr_login/logout/
  is_logged_in）不读影子 map；收藏夹（fav_folders）不经路由判定。
  (c) 搜索面的前置特例：`search_scoped` 对 BV/av/b23.tv 视频引用关键词
  有旧直解臂（`manager.rs:645-658`）**先行于 musid 槽位**——bili 搜索面
  Migrated 仅覆盖普通关键词路径；视频引用直解与链接直解同语义，恒旧
  路径（musid TrackSearch 本身支持 BV 引用，voicefox 侧不接）。
  (d) 流地址面 DASH 形状两侧同形：headers（Referer + UA）恒挂
  （legacy `bili/url.rs:61-64`、musid StreamInfo.headers）；backupUrl
  镜像进 fallback_endpoints → candidate_urls 折叠；index/init range 走
  G3 伴随字段（musid 不产 → None，descriptive-only）；format=Aac 不反推
  档位。
  (e) **匿名 resolve 可用性未实测（如实登记）**：musid playurl 带
  `try_look=1` 试看参数（DASH 四分支 fixture 形状 pin），匿名可用性的
  真网验证属运行期 probe 对账口径；匿名退化（§11.8 (a)）的计数信号是
  MusidErrLegacyOk。
- **migu**：登录面双侧匿名（musid authentication=false + 无
  auth_capability 覆写 → None；voicefox mg 无 login/qr_login 位）——不设
  登录行项。搜索面 qualities 完全对齐（两侧 formatType PQ/HQ/SQ/ZQ* 同
  表折档）——无簿记分歧，parity 可全等命中。歌词面 MRC 逐字降级：
  legacy `mg/lyric.rs` 的 MRC 产 lxlyric + trc 产 tlyric；musid 无
  YRC/QRC 序列化器（lxlyric 恒 None），路由后逐字降级为纯 LRC 正文 +
  tlyric 保留（trc → translation），`compare_lyrics` 正文指纹不比逐字
  （同 kugou/kuwo 口径）。保持旧路径：榜单/专辑/歌单/链接直解（musid
  ChartProvider/AlbumProvider/PlaylistProvider/LinkResolver 全存在，
  集合面不路由，musid 侧可用性登记供后续片）；歌手面双侧皆无（musid
  无 ArtistProvider 实现、artists 位缺省 false；voicefox mg 的 artist
  位是搜索归一类能力，恒旧路径——后续片接 ArtistProvider 需先在
  musid-dli 侧补实现）。
- **kuwo**：登录面 Anonymous web token（musid authentication=false +
  `auth_capability()` 显式覆写 `AuthCapability::Anonymous`，上游测试
  pin；§2.8 机制细分不置 voicefox login 位）——恒旧路径。搜索面 N_MINFO
  档位两侧同表（bitrate 128/320/2000/4000）——parity 可全等命中；22000/
  20900 等 bitrate 不在两侧表中（同弃）。流地址面双通道（mobile/web
  并发；无档位请求降级 320k 再 128k；lossless 请求 web 为主、移动端档位
  URL 进候选）与 legacy `kw/url.rs:228-256` 同形；移动端通道 headers 两侧
  同挂 okhttp UA。歌词面 lrcx 逐字降级同 migu/kugou 口径（legacy
  `kw/lyric.rs:93-107` 先 lrcx=1 逐字；musid LyricDocument 无 lxlyric
  序列化器）。保持旧路径：登录、歌手面（musid 无 ArtistProvider，
  voicefox kw 的 artist 位恒旧路径并登记缺口）、榜单/专辑/歌单/链接
  （musid 全存在，集合面不路由登记）。
- **kugou**：登录/扫码面恒旧路径（voicefox kg/login.rs + session.rs 扫码
  链路；musid authentication=true 折叠 Cookie 诊断位但影子源恒匿名）。
  **流地址面的匿名硬退化（本片最重要的登记）**：musid `resolve` 在
  `login_parts()` 处无会话即失败（"Kugou playback requires login; please
  scan in first"，`source_kugou.rs:239-258`——酷狗不给未登录用户返回任何
  播放地址，与 legacy `kg/url.rs` 同病）——影子源恒匿名 → 流地址面
  musid 槽位在双栈下**必然失败**、动态层回落旧链现取（MusidErrLegacyOk
  计数恒增）；该面 V1Routed 时已如此路由，本切片不改运行行为，只补对账
  与观测口径。缺 hash 的歌在 `select_hash` 处先于 login_parts 离线
  NotFound（`source_kugou.rs:519-536、:717`）。**逐字歌词降级的片级决策**：
  本切片按批 ask 落地三面全路由——musid 无 YRC/QRC 序列化器（lxlyric 恒
  None，§2.10），legacy `kg/lyric.rs` parse_krc 产逐字；路由后逐字能力
  降级为纯 LRC 正文（KRC 词级时间在 musid LyricDocument.lines[].words，
  不伪造 lxlyric），`compare_lyrics` 正文指纹对照不比逐字。降级的用户
  可见影响：逐字卡拉OK显示变纯 LRC 高亮——接受该降级并在此登记；若片级
  评审推翻，回退动作是注册表 kugou 行歌词面改 `LegacyOnly`（一行改动）。
  搜索面 qualities 两侧同表（FileSize/HQ/SQ/Res > 0 → 四档）——parity
  可全等命中。保持旧路径：登录/扫码、歌手（musid artists=true 但
  ArtistProvider 是 search-based fallback，voicefox artist 位恒旧路径）、
  榜单/专辑/歌单/链接（musid 全存在，集合面不路由登记）。

**切片 8-11（v2-batch-2：tx / wy / qianqian / joox）的对账登记
（2026-11-21）**。四个源的可路由面 musid 侧 capabilities 均 true、三面
V1 起即放行（本批不改运行行为，只补对账与登记）；逐源差异如下：

- **tx（qqmusic，切片 8）**：登录/扫码面恒旧路径（QQ + 微信双扫码，
  voicefox tx/mod.rs:70-73 login/qr_login/wechat_login 位；微信扫码
  wechat_login 双栈期恒 false，§2.8；musid authentication=true 折叠
  Cookie 诊断位但影子源恒匿名 new()）。**P5 口径在 tx 上权重最大**
  （用户基数最大）：musid 匿名面对 cookie 登录用户的 VIP 曲目/档位
  系统性收窄，probe 对账产生系统性 ParityMismatch 噪声——解读计数先
  剥离登录态不对称（§11.8 (b)）。流地址面 vkey 请求链两侧同构
  （musid-dli source_qqmusic.rs:809-863，stream_vkey_ok/
  stream_vkey_empty_sip 形状 pin；strMediaMid/songId/size_* 经 extra
  透传）；簿记分歧一处：size_128mp3 缺席时 musid 恒产 `Low` 而 legacy
  不产 `Low128`（musid 恒声明 Low 档）——真实搜索形状（四档 size 全正）
  下折档集合相等，adapter 单测 pin 全等命中。歌词面 **QRC 逐字降级**：
  musid GetPlayLyricInfo 用 crypt:0/qrc:0 取 plain LRC（musid 无
  YRC/QRC 序列化器，lxlyric 恒 None），legacy tx/lyric.rs crypt:1/qrc:1
  产 QRC 逐字——路由后逐字降级为纯 LRC 正文 + 翻译/罗马音保留
  （compare_lyrics 不比逐字，同 kugou/kuwo/migu 口径；降级的回退动作 =
  注册表 tx 行歌词面改 `LegacyOnly` 一行）。歌词离线失败形态：extra 无
  数值 songId → InvalidInput（source_qqmusic.rs:873-877）。保持旧路径：
  登录/扫码、歌手面（musid 无 ArtistProvider、artists=false，voicefox
  tx/mod.rs:67 artist=true 恒旧路径**登记缺口**）、榜单/专辑/歌单/链接
  （musid ChartProvider :1467/AlbumProvider :1408/PlaylistProvider
  :1313/LinkResolver :913 全存在且 capabilities true）集合面不路由登记。
- **wy（netease，切片 9，放最后）**：主源且登录态依赖最重——musid 恒
  匿名 vs voicefox cookie 登录（wy/mod.rs:182-184 login/qr_login/
  user_playlists），结果集/档位系统性收窄是 P5 已登记口径，probe 对账
  的 mismatch 噪声在 wy 上预期最高，解读计数须先剥离。流地址面 eapi
  song/enhance/player/url/v1 + outer-url 死兜底 HEAD 探测（musid-dli
  source_netease.rs:494-601，song_enhance_player_url_v1 形状）——
  **expires_at_unix 从响应/URL 提取，G4 过期门对 wy 设防**（与 jamendo/
  fivesing 的 `None` 形态不同，无快照取址风险）。歌词面 eapi
  song/lyric/v1：musid 无 YRC/QRC 序列化器（lxlyric 恒 None），legacy
  wy/lyric.rs 的 yrc 逐字路由后降级为纯 LRC 正文（同批口径）。搜索面
  **无簿记分歧**（legacy 恒产 Low128 = musid 恒产 Low，h/sq/hr 同表），
  ±1s 时长容差 + 折档后 qualities 集合相等的归一对照可全等命中
  （adapter 单测 pin）。保持旧路径：登录/扫码/用户歌单；集合面（musid
  ArtistProvider :766/ChartProvider :809/AlbumProvider :735/
  PlaylistProvider :652/LinkResolver :840 全存在、capabilities 全 true
  全家桶）不路由登记——**wy 的歌单同步若依赖 by-id 详情
  （songs_from_detail 类路径），前置是 musid-dli 0.2.0 无公开
  per-provider detail trait（track_details 是 crate 内部方法，§11.4
  第 4 条），需上游先公开**。
- **qianqian（切片 10，用户启用源）**：登录面双侧皆无认证位
  （authentication=false + AuthCapability::None）不设行项。
  **空 tsid 的歌在两栈都在任何网络调用之前失败**（musid resolve/lyrics
  source_qianqian.rs:791-793/:840-842 与 legacy url.rs:39-41、
  lyric.rs:19-21 同键同 NotFound）——manager 级真实双侧链路的确定性
  触发点（musid_qianqian_slice.rs pin MusidErrLegacyErr + legacy
  "not found" 文本向上）。流地址面 tracklink 双分支两侧同构
  （free_128 完整文件 + vip_nopath 阶梯退 trail_audio_info 试听；
  VIP 曲目匿名恒 path-less → NotFound，上游 live-pinned，同病同回落
  **无回退盲区**；expireTime 进 expires_at_unix，G4 对该源设防）。
  搜索面 rate 表两侧同形（3000/320/128|64 → Lossless/High/Low 折档后
  相等）parity 全对齐。歌词面 song_infoandlrc 类形状纯 LRC（legacy
  千千本就无逐字，无降级）。保持旧路径：歌手面（musid 无
  ArtistProvider、artists=false，voicefox qianqian/mod.rs:54
  artist=true 恒旧路径**登记缺口**）；榜单面双侧皆无（voicefox 无
  leaderboard 位、musid 无 ChartProvider）不设；专辑/歌单/链接（musid
  AlbumProvider :999/PlaylistProvider :874/LinkResolver :1015 且
  capabilities true）集合面不路由登记——**musid 侧
  search_playlist_refused.json（歌单搜索被拒形状）后续接集合面时须
  处理该语义**（envelope 恒不按 state/errno 把关、`data: []` 解析为
  空页，source_qianqian.rs:762-764）。用户启用源：翻源后首周盯
  MusidErrLegacyOk 计数（§12.3）。
- **joox（切片 11，用户启用源）**：登录面双侧皆无认证位不设行项。
  搜索面两侧同构（tracks 嵌套数组、id/album_id 加密串、normalize_id
  两侧逐字同实现——percent-decode only，`+`/`=` 保真）；**歌手 join
  簿记分歧**：legacy parse_song_info 按 " / " join（上游 parity）、
  musid Track.artists 逐元素进向量经适配层 '、' join——单项歌手同串、
  多项歌手经 parity 归一（filter_str 分隔符集含 `/`）无损，adapter
  单测 pin（§12.3 joox 块）。流地址面 songinfo_stream 两侧同构
  （select_candidate 与 legacy get_song_url 同序：sort desc → 第一个
  ≤ 请求档 → 首选兜底；size/md5/expiry 两侧同弃、size_is_advisory=
  true——下载引擎单连接流式；kbps 320/192/128 → High/Low/Low 同表）。
  **空 songid 的歌 musid 侧离线 NotFound（resolve/lyrics，
  source_joox.rs:754-756/:802-804），legacy 无离线守卫走网络——
  manager 级双失败链不可确定性复现**，失败侧对账由 adapter 级离线
  用例 + 运行期 probe 承载（与 tx/wy 同类登记）。歌词面 lyric_live
  形状纯 LRC（web_lyric base64 字段的 live 偏差上游已登记），legacy
  本就无逐字，无降级。**封面面不接 musid ArtworkProvider**：legacy
  joox/mod.rs:167 get_cover_url 走歌曲模型回填，适配层 get_cover_url
  同口径，不设行项。保持旧路径：榜单/歌手双侧皆无（voicefox
  joox/mod.rs:103-111 仅 playlists/playlist_search/album/link_parse；
  musid 无 ChartProvider/ArtistProvider）不设；专辑/歌单/链接（musid
  AlbumProvider :873/PlaylistProvider :839/LinkResolver :908 且
  capabilities true）集合面不路由登记——**musid 侧
  playlist_dead_404.html 死链形状后续接集合面时须处理**。用户启用
  源：翻源后同 qianqian 盯 MusidErrLegacyOk 计数。

### 12.3 parity 对账 fixture 方案

musid-dli `tests/fixtures/<source>/` 是**平台原始响应形状**、由 musid-dli
私有 parse 消费（§7 偏差声明，不能直接 serde 成 Track/StreamInfo）——在
V2 的用法是：上游 provider 回归参考 + voicefox 侧 fixture 的样本值来源。
voicefox 侧对账三件套（每切片同形执行）：

1. **voicefox 侧 wire 形状 fixture**：`source/tests/fixtures/musid/`
   新增 `track_<source>.json`（musid Track wire 形状 + `derivation_note`
   注明逐字段来源）。切片 1 落地 `track_jamendo.json`（样本值取自
   musid-dli `tests/fixtures/jamendo/track_parse.json` 的 item + 
   `pick_stream.json` 第 4 例：trackid 1538710 / "Love Recycle" /
   LoveRecycle / 176100 / 245s / download 组 mp32 URL /
   `available_qualities ["Low"]` / extra 带 `track_id`+`stream_url`+
   `stream_format`+`stream_quality`）。**provenance 限定（评审 P3）**：
   上游 fixture 文件自述为按 2026-11-18 live 捕获**重建**的形状记录，原始
   捕获未随上游仓提交（`tests/fixtures/jamendo/README.md` 明文）；上游
   fixture 是本样本值在两仓内唯一可独立复核的落盘出处，"live 捕获"本身
   不构成仓内证据。切片 2 落地 `track_fivesing.json`（样本值取自
   musid-dli `tests/fixtures/fivesing/search_songs.json` 第 1 项——该目录
   自述为 2026-11-18 **逐字 live 捕获**且随上游仓提交，provenance 强于
   jamendo 的重建形状：songId 15622111 / typeEname fc / "晴天"（live 高亮
   形状 `<em class="keyword">`）/ singer Vk / songSize 10462607 字节按时长
   两侧共用的 320kbps 公式估 261s / live 搜索项无 `user` 键故无封面 /
   `available_qualities` 为解析器静态三档声明）。切片 3-7 落地
   `track_apple.json` / `track_bilibili.json` / `track_migu.json` /
   `track_kuwo.json` / `track_kugou.json`（样本值来源逐文件
   `derivation_note` 注明：apple 取自 musid-dli
   `tests/fixtures/apple/song_parse.json`（该目录 README 明文**全部为重建
   形状**，金值取自双仓共用的 inline parse 测试）；bilibili 取自
   `tests/fixtures/bilibili/search_title_em.json`（重建形状，`<em>` 高亮
   strip 两侧同源）；migu 取自 `tests/fixtures/migu/search_response.json`
   的 ok_numeric_total 第 1 项（重建形状）；kuwo 取自
   `tests/fixtures/kuwo/search_page.json` abslist[0]（重建形状）；
   kugou 上游无 search 形状 fixture 文件，取自 musid-dli
   `source_kugou.rs` inline parse 测试的 pinned 值（`derivation_note`
   如实登记为重建形状）——五源均无 jamendo 式"live 捕获"宣称）。
   切片 8-11 落地 `track_qqmusic.json` / `track_wy.json` /
   `track_qianqian.json` / `track_joox.json`（provenance 分两档，逐文件
   `derivation_note` 注明：**qianqian/joox 为 live 捕获档**——上游
   `tests/fixtures/qianqian/README.md` 明文 search_songs.json 是
   2026-11-19 匿名 GET 的 gzip 逐字节捕获（前 2/5 项 verbatim），
   `tests/fixtures/joox/README.md` 明文 search_songs.json 同日捕获
   （envelope 键 verbatim、前 3/30 项保留），provenance 等于 fivesing、
   强于重建形状；**qqmusic/netease 为重建形状档**——上游两目录 README
   明文 "Every fixture here is therefore reconstructed, not captured"
   （qqmusic 的 README 进一步说明全部取值为占位样本，无 live 捕获
   宣称）。wy 的样本值沿用 V0 口径取自 `song_detail_legacy.json`
   songs[0]（id 789/老接口/周杰伦·费玉清/258000ms）；**track_wy.json
   与 V0 的 track_netease.json 的唯一刻意差异**：available_qualities
   补上 `Low`——V0 fixture 是手写简化集（无 Low），V2 的 parity 对照
   要求 fixture 反映 `source_netease.rs parse_track` 的真实产物
   （Low 恒声明 :388），legacy 侧 parse_song 恒产 Low128（:152），两侧
   恒声明档同口径后 qualities 集合才可全等命中；V0 fixture 原样保留
   （V0 映射测试继续 pin）。joox 的 parity 平台响应含 `tracks` 嵌套
   数组单项（与上游 search_songs.json tracks[0] 同形），legacy 侧消费
   真实的 `joox::api::songs_from_search`（可见性扩宽登记见 §12.7）。
2. **adapter 单测**（`adapter_musid.rs::tests::v2_registry`）：fixture →
   `track_to_song` 断言（含 extra 的 `stream_url` 系键透传与
   `song_to_track` 保真往返）、`parity::songs_equivalent` /
   `compare_search_items` 归一对照（legacy 侧 SongInfo 由**真实的**
   `crate::jamendo::parse_track` 从平台原始响应形状组出——评审 P2：任一
   侧 parse 改字段必红；`parse_track` 为此做 `pub(crate)` 可见性扩宽，
   语义零变化，登记见 §12.5）、流地址面行为（离线 resolve + 实际档位
   回填 + enrich 补齐；无嵌入地址 → 动态层回落触发）。
3. **运行期路由计数**（`musid_dual_stack.rs`）：桩源双栈用例 pin 注册表
   放行/拒绝的 manager 行为（jamendo 搜索/流地址 MusidOk、歌词面 musid
   零调用 + 旧路径 NotFound 向上）。真实网络行为对账靠 probe 模式的
   `ParityCounters`（恒纯观测、后台落账、最终一致，§11.5）——切片翻源后
   首周盯 `MusidErrLegacyOk` 计数（用户启用源 qianqian/joox 尤其）。

歌词面 parity 口径维持 §11.6：`compare_lyrics` 只比 `lyric` 正文指纹，
翻译/逐字豁免（musid-dli 无 YRC/QRC 序列化器，lxlyric 恒 None）。

### 12.4 切片账面（盘点切片 → 注册表行）

逐源顺序以盘点为准（与 §11.8 "集合面 V2 逐源迁移时再评估" 相容）。各源
**保持旧路径的面**在切片落地时逐条登记进注册表行的 `LegacyOnly` 或留在
"集合面不设行项"口径：

| # | 源（SourceId） | 切片 | 搜索 | 流地址 | 歌词 | 保持旧路径并登记 |
|---|---|---|---|---|---|---|
| 1 | jamendo | v2-jamendo（**已落地**） | Migrated | Migrated | LegacyOnly | 歌词（musid stub，NotFound 语义）；集合面（musid capabilities 恒 false，匿名挂起）；登录面双侧匿名。**流地址面快照新鲜度风险已登记**（搜索时内嵌 URL + stream 组 `from=` token + `expires_at_unix=None` 使 G4 不设防 + 播放侧 403 不进 parity 计数，观测口径与回退选项见 §12.2 末段） |
| 2 | fivesing | v2-fivesing（**已落地**） | Migrated | Migrated | Migrated | 歌单面（musid 有 PlaylistProvider 且 capabilities.playlists=true，但 V1 影子源不实现 fat-trait 集合方法、manager 路径不可达，search_playlists/fetch_playlist 恒旧路径，登记可后续片）；链接直解（musid LinkResolver 存在但不接，parse_link 恒旧路径）；专辑/歌手/榜单双侧同缺不设行项。**对账登记**：流地址面 Referer 头差异（CDN 已不强制，musid 对齐 live 行为）与 getSongUrl 直链无 expires 信号（G4 不设防，两侧同现取同病）；搜索面 qualities 簿记分歧使 probe 搜索面 ParityMatch/Mismatch 无鉴别力（§12.2 fivesing 块）；歌词面 lxlyric 恒 None 不比（§11.6 口径） |
| 3 | apple | v2-apple（**已落地**） | Migrated | Migrated | Migrated | 歌单分类/歌单/专辑面/链接直解（musid PlaylistProvider categories 恒 Unsupported、AlbumProvider 存在，集合面不路由）；歌手/榜单双侧皆无。**对账登记**：preview-only 契约两侧同形（quality 严格对齐 Low/Low128）；SongUrl.duration 分歧（legacy 整曲 vs musid 30s）；搜索面 qualities 簿记分歧（probe 搜索面无鉴别力）；歌词面 PlainText → musid 产空 lyric + raw_lrc、恒动态层回落（§12.2 apple 块） |
| 4 | bili | v2-bilibili（**已落地**） | Migrated | Migrated | LegacyOnly | **三件结构前置已落地**（子 feature `musid-dli-adapter-bilibili` / bootstrap 匿名装配分支 / manager `matches!` 放开 + 流地址面 bili 早退臂前插 musid 槽位）；歌词面双侧皆无（musid 硬 stub + voicefox 恒 NotFound）；登录/会话/用户数据（SESSDATA 扫码链路）恒旧路径；搜索面视频引用关键词（BV/av/b23）走旧直解臂先行于 musid（链接直解同语义）；匿名 resolve 可用性（try_look=1）未实测，真网验证属 probe 对账口径（§12.2 bilibili 块） |
| 5 | migu | v2-migu（**已落地**） | Migrated | Migrated | Migrated | 登录 Anonymous 机制双侧匿名不设行项；集合面（musid 侧 Chart/Album/Playlist/Link 全存在且 capabilities true，登记供后续片）；歌手面双侧皆无（musid 无 ArtistProvider；voicefox artist 位是搜索归一类能力，恒旧）。**对账登记**：搜索面 qualities 全对齐（formatType 同表）；MRC 逐字歌词降级为纯 LRC + tlyric 保留（§12.2 migu 块） |
| 6 | kuwo | v2-kuwo（**已落地**） | Migrated | Migrated | Migrated | 登录 Anonymous web token 恒旧路径（musid auth_capability()=Anonymous 上游测试 pin，不置 voicefox login 位）；歌手面（musid 无 ArtistProvider，voicefox artist 位恒旧路径并登记缺口）；榜单/专辑/歌单/链接（musid 全存在）集合面不路由登记。**对账登记**：N_MINFO 档位两侧同表（parity 全对齐）；流地址面 mobile/web 双通道同形 + okhttp UA 两侧同挂；lrcx 逐字降级（§12.2 kuwo 块） |
| 7 | kugou | v2-kugou（**已落地**） | Migrated | Migrated | Migrated | 登录/扫码恒旧路径（musid Cookie 诊断位 + 影子源恒匿名）；**流地址面匿名硬退化**（musid login_parts 无会话硬错误 → musid 槽位必然失败进动态层，MusidErrLegacyOk 恒增——V1Routed 时已如此，本片不改运行行为）；**逐字歌词降级已按片级决策接受并登记**（musid 无 YRC/QRC 序列化器，路由后 KRC 逐字降为纯 LRC 正文；回退动作 = 注册表歌词面改 LegacyOnly 一行）；歌手（musid artists=true 是 search-based fallback，voicefox 位恒旧）；集合面（musid 全存在）不路由登记（§12.2 kugou 块） |
| 8 | tx | v2-qqmusic（**已落地**） | Migrated | Migrated | Migrated | 登录/扫码恒旧路径（QQ+微信双扫码，wechat_login 双栈期恒 false §2.8；musid 恒匿名，VIP 收窄是 P5 已登记口径且 tx 用户基数最大——probe mismatch 噪声先剥离登录态不对称）；歌手面（musid 无 ArtistProvider）恒旧路径**登记缺口**；榜单/专辑/歌单/链接（musid 全存在）集合面不路由登记。**对账登记**：QRC 逐字降级为纯 LRC（回退动作=歌词面改 LegacyOnly 一行）；size_128mp3 缺席时 musid 恒产 Low 的簿记分歧（真实形状下折档集合相等）（§12.2 tx 块） |
| 9 | wy | v2-netease（**已落地**） | Migrated | Migrated | Migrated | 登录/扫码/用户歌单恒旧路径（登录态依赖最重，放最后；musid 恒匿名 vs cookie 登录——probe mismatch 噪声预期最高，P5 口径）；集合面（musid 全家桶 capabilities 全 true）不路由登记，**歌单同步 by-id 详情前置=上游公开 per-provider detail trait**（track_details 是 crate 内部方法，§11.4 第 4 条）。**对账登记**：yrc 逐字降级为纯 LRC（同批口径）；流地址面 expires_at_unix 从 URL 提取（G4 设防，无快照风险）；搜索面无簿记分歧、±1s 容差 + 折档集合相等（§12.2 wy 块） |
| 10 | qianqian | v2-qianqian（**已落地**） | Migrated | Migrated | Migrated | 用户启用源（翻源后首周盯 MusidErrLegacyOk）；登录面双侧皆无认证位不设行项；歌手面（musid 无 ArtistProvider）恒旧路径**登记缺口**；榜单面双侧皆无不设；专辑/歌单/链接集合面不路由登记（**search_playlist_refused 被拒形状**待后续片处理）。**对账登记**：空 tsid 两栈离线双失败（manager 级真实链路 pin MusidErrLegacyErr）；tracklink free/vip 双分支同构、VIP 匿名 NotFound 同病同回落无盲区、G4 设防；rate 表同形 parity 全对齐（§12.2 qianqian 块） |
| 11 | joox | v2-joox（**已落地**） | Migrated | Migrated | Migrated | 用户启用源（同 qianqian 盯 MusidErrLegacyOk）；登录面双侧皆无不设；榜单/歌手双侧皆无不设；专辑/歌单/链接集合面不路由登记（**playlist_dead_404 死链形状**待后续片）；**封面面不接 musid ArtworkProvider**（歌曲模型回填同口径）。**对账登记**：歌手 join 分歧（" / " vs '、'）经归一无损；空 songid musid 离线 NotFound、legacy 无离线守卫——manager 级双失败链不可确定性复现，失败侧对账由 adapter 级用例 + probe 承载（§12.2 joox 块） |
| — | soda | reg-soda（**登记不迁**） | 无行 | 无行 | 无行 | voicefox-only 源：musid-dli 无此 provider，feature 无从定义；自实现完整保留，四面恒旧路径 |
| — | youtube-music | reg-ytm（**登记不迁**） | — | — | — | musid-dli-only 源：voicefox 无对应 SourceId；musid 侧走 rustypipe 自带栈，不在迁移序列 |

注册表账面与上表一致（切片 1-11 已落地：`jamendo` 行 Migrated×2 +
LegacyOnly×1，`fivesing`/`apple`/`migu`/`kuwo`/`kugou`/`tx`/`wy`/
`qianqian`/`joox` 行 Migrated×3，`bilibili` 行 Migrated×2 +
LegacyOnly×1（歌词）；**无 V1Routed 余行**——§1 命名表的 11 个映射源
全部完成逐源对账，运行行为与 V1 完全一致：三面 V1 起即放行，V2 只补
对账与登记）；`registry_covers_
every_namespace_mappable_source` 单测 pin"行覆盖 = §1 命名表映射源数 +
行数相等 + 无同源重复行 + 每行 namespace 一致"（评审 P5：`source_route_
entry` 用 find() 取首行，重复行也须防），防后续切片漏行/重行。
`migrated_route_sources` 随账面增长（现 = jamendo、fivesing、apple、
bilibili、migu、kuwo、kugou、tx、wy、qianqian、joox 十一源，行序），
单测 pin
行序；`unregistered_migrated_sources` 的全等空断言只在**全部 Migrated 子
feature 同开**的组合构建下成立（`musid_bootstrap.rs` 的
`migrated_slices_assemble_without_ledger_gap`，CI 的十一源+lyrics 组合
步跑到），单子 feature 构建按"不含本切片源"口径断言（部分装配的 warn
形态是 §12.1 生效前提的预期行为）。`soda`/`youtube-music` 登记行
（reg-soda/reg-ytm）无注册表行可改账：登记断言随各自
`musid-dli-adapter-reg-*` feature 进门禁（§12.7）。

### 12.5 V2 门禁（切片 1 实跑，2026-11-21；评审 P1-P5 修订后复跑）

- **真实 provider 编译/行为覆盖（评审 P1 的修复）**：V1 遗留的盲区是按源
  子 feature 的装配分支（含 `musid_dli::source_<x>` 路径引用）不在任何
  门禁里——写错路径不会红。切片 1 起两层覆盖：
  (a) `musid_bootstrap.rs::jamendo_slice_tests`（挂 `musid-dli-adapter-
  jamendo` 门）：编译真实 `JamendoProvider` 路径，pin 装配产物（id/显示名/
  descriptor 位全 default/supported_qualities 缺省空）与 `register_
  available` 装配（Migrated 账面行全部有影子源，P4）；
  (b) 新集成测试 `source/tests/musid_jamendo_slice.rs`（同门）：**真实**
  provider 全链路 5 例——离线 resolve（adapter 组 Track → 真实 resolve 取
  内嵌地址 → 实际档位回填 + enrich）、无内嵌地址失败进动态层、真实歌词
  stub 的 `Other("该音源不支持…")` 污染证据（§12.2 静态层依据）、真实
  descriptor 逐位 pin、manager 级路由（真实影子源 + 注册表放行 →
  MusidOk 计数，零网络）。复刻桩（`adapter_musid.rs::JamendoStreamStub`）
  只测适配层自身，与真实实现的一致性由本文件钉住；
  (c) CI `musid-stack` job 新增两步：10 个子 feature 逐个 `cargo check`
  （全部装配分支的编译覆盖，不限于 jamendo）+ jamendo 子 feature 全量
  `cargo test`（真实 provider 测试随门进 CI）。
- 确定性硬门 `cargo test -p lx-source --features musid-dli-adapter --
  adapter_musid`：**42 通过 / 0 失败**（V1 的 35 + V2 新增 7 例：注册表
  账面/行数/去重/namespace 一致 pin ×1、Migrated 账面助手 ×1、jamendo
  fixture 映射 ×1、真实 parse_track 的 parity 归一对照 ×1、流地址面模板
  ×2，后两者为 `#[tokio::test]`）；
- `cargo test -p lx-source --features musid-dli-adapter-jamendo`：全量
  （lib + 全部集成测试）零失败——含上述真实 provider 5 例与 bootstrap
  装配 2 例；`cargo test -p lx-source --features musid-dli-adapter --test
  musid_dual_stack`：**20 通过 / 0 失败**（V1 的 19 例在注册表下原样全绿
  = `V1Routed` 行为保持的双证；新增 jamendo 注册表路由用例 1 例）；
- 歌词变体 `cargo test -p lx-source --features musid-dli-adapter-lyrics`
  两套件同口径复跑（结果见交付记录）；默认基线 `cargo test -p lx-source`
  lib 例数不变（V2 代码全部挂 `musid-dli-adapter` 门；新集成测试文件在
  默认构建 `#![cfg]` 编译为空）；
- `cargo fmt --all` 干净；`cargo tree -p lx-source -e normal`（默认）
  musid 边 = 0（V2 未引入新依赖）。

**旧代码改动登记（"一行不删"边界的本轮例外）**：`jamendo/mod.rs` 的
`parse_track` 由私有改为 `pub(crate)`（评审 P2：parity 测试消费真实
parse 产物；仅可见性扩宽，函数体零改动）。其余改动全部是新增代码或
musid 框架内机械重排。

### 12.6 切片 3-7（v2-batch-1：apple / bilibili / migu / kuwo / kugou）门禁与登记（2026-11-21）

与切片 1/2 同形的三件套逐源落地：注册表行改账（`adapter_musid.rs` 行注释
+ `registry_ledger_pins_the_slice_states` 逐源 pin）、bootstrap 真实
provider 装配测试（`apple/bilibili/migu/kuwo/kugou_slice_tests`，各挂对应
子 feature 门）、`source/tests/musid_<source>_slice.rs` 真实 provider 对账
（descriptor 逐位 pin + 离线守卫/失败路径 + migu/kugou 的 manager 级真实
双侧链路 + apple/kuwo/kugou 歌词门下的指纹 parity）。五源 wire fixture
落 `source/tests/fixtures/musid/`（provenance 见 §12.3）。legacy 侧 parity
消费点可见性扩宽（仅 `pub(crate)`，函数体零改动，`crate::<source>::…`
路径）：`apple/api.rs parse_song` + `apple/mod.rs` 的 `api` 模块、
`bili/search.rs parse_song` + `bili/mod.rs` 的 `search` 模块、
`kg/search.rs parse_song`（模块本就 pub）、`mg/mod.rs` 的 `song` 模块
（`mg/song.rs parse_song` 本就 pub(crate)）；`kw/search.rs` 的 search
内联逐项解析**逐字搬出**为 `pub(crate) fn parse_search_song`（fivesing
切片先例同款机械重排，行为逐字一致）。

**bilibili 切片的结构前置（三件）**：
1. 子 feature `musid-dli-adapter-bilibili = ["musid-dli-adapter",
   "musid-dli/source-bilibili"]`（source/Cargo.toml，`source-*` 名对照
   musid-dli Cargo.toml）；
2. bootstrap 装配分支 `bilibili(transport)`（恒匿名 `new()`，§11.8 零凭据
   流动——SESSDATA/收藏夹/扫码登录完全不经 musid）；
3. manager 放开：`musid_route` 的 `matches!` 排除移除 `SourceId::Bili`
   （路由判定完全由注册表 bili 行承载）；流地址面的旧 bili 早退臂
   （`get_song_url_inner`，独立存储语义）**逐字保持**为 musid 槽位之后的
   旧路径——musid 槽位插入早退臂之前，簿记三件套（try / legacy-ok /
   err 出口）与主链同款；搜索面的视频引用直解臂（BV/av/b23）保持先行于
   musid（链接直解同语义，§12.2 bilibili 块 (c)）。默认构建
   （feature 关）该臂为编译期原样代码，行为零变化。

**本批新增的行为差异登记（摘要，全量见 §12.2 切片 3-7 块）**：apple
preview-only 双侧同形（quality 严格对齐）+ SongUrl.duration 分歧 + 搜索面
qualities 簿记分歧（probe 搜索面无鉴别力，同 fivesing 口径）+ PlainText
歌词 → musid 产空 lyric 恒动态层回落；kugou 流地址面匿名硬退化
（login_parts 无会话即失败，musid 槽位必然失败进动态层）；kugou/kuwo/migu
歌词面逐字降级为纯 LRC 正文（lxlyric 恒 None；kugou 降级的回退动作 =
注册表歌词面改 LegacyOnly 一行）。

V2 各切片的门禁口径沿用 §11.9（外层脚本跑全仓矩阵；逐片只跑窄命令 +
fmt），新 fixture 与子 feature 覆盖步随所在套件进 CI 的 `musid-stack`
job。

**本批实跑记录（2026-11-21，全部在本会话执行）**：
- 外层门禁命令 `cargo test -p lx-source --features musid-dli-adapter-
  fivesing,musid-dli-adapter-apple,musid-dli-adapter-bilibili,musid-dli-
  adapter-migu,musid-dli-adapter-kuwo,musid-dli-adapter-kugou --lib`：
  **292 通过 / 0 失败 / 2 ignored**（默认基线 220 + 切片 1-7 的 lib 单测
  增量）；同参数 `cargo clippy -p lx-source --all-targets … -- -D
  warnings -A clippy::double_must_use`：exit 0（修复一例新测试文件的
  clippy `doc_lazy_continuation`——文档注释行首 `+` 被当列表项）；
- 同六 feature `--tests` 全目标：lib 292 + js_user_api 4 + dual_stack
  **26**（切片 1-2 的既有注册表用例 + 本批新增 5 例注册表路由用例：
  apple / migu / kuwo / kugou 各 1 + bilibili 1 例含歌词 LegacyOnly
  断言）+ parity 6 + 五源 slice 文件各 3（fivesing 3、jamendo 0——该
  构建无 jamendo feature）；`musid-dli-adapter-lyrics` 单变体：lib
  **285** / dual_stack **32**（歌词门用例进）；**七源 + 歌词变体组合**：
  lib **300** / dual_stack 32 / 七个 slice 文件 4+3+4+4+4+4+5 全绿（含
  `migrated_slices_assemble_without_ledger_gap` 的全等空分支与
  apple/kuwo/kugou 歌词指纹 parity）；
- 默认基线 `cargo test -p lx-source --lib`：**218 通过 / 0 失败 / 2
  ignored**（220 例，与 V0/V1 完全一致——manager bili 臂改动全挂
  `musid-dli-adapter` 门，默认构建编译期原样）；`cargo tree -p lx-source
  -e normal`（默认）musid 边 = 0；`cargo fmt --all` 干净。


### 12.7 切片 8-11（v2-batch-2：tx / wy / qianqian / joox）+ 登记切片（reg-soda / reg-ytm）门禁与登记（2026-11-21）

与切片 1-7 同形的三件套逐源落地：

1. **注册表行改账**（`adapter_musid.rs`）：tx/wy/qianqian/joox 四行
   V1Routed×3 → Migrated×3（行注释逐条登记 §12.2 各块）；账面至此无
   V1Routed 余行——三面 V1 起即放行，**本批不改运行行为**（V1 的
   musid_dual_stack/musid_parity 用例以 Wy 为载体继续全绿 = 行为保持的
   双证；manager.rs 本批零改动，路由判定由 §12.1 注册表承载）。
   `registry_ledger_pins_the_slice_states` 逐源 pin；`migrated_route_
   sources_are_visible_to_the_build_wiring` 更新为十一源行序。
2. **bootstrap 装配测试**（`musid_bootstrap.rs`）：`qqmusic/netease/
   qianqian/joox_slice_tests` 各挂对应子 feature 门——真实 provider
   装配产物 pin（id/显示名/descriptor 位/supported_qualities）+
   `register_available` 装配覆盖；`migrated_slices_assemble_without_
   ledger_gap` 的全量账面扩到 11 源（CI 的十一源组合步跑到全等空分支）。
3. **真实 provider 对账**（`source/tests/musid_{qqmusic,netease,
   qianqian,joox}_slice.rs`，各挂对应子 feature 门）：descriptor 逐位
   pin（§12.4 行 8-11 的账面依据）；离线失败触发点 pin——tx 歌词面无数值
   songId → InvalidInput、wy 搜索面空关键词 → InvalidInput、qianqian
   空 tsid 在 resolve/lyrics 双面 NotFound（**qianqian 另有 manager 级
   真实双侧链路：两栈同请求离线双失败 → MusidErrLegacyErr + legacy
   "not found" 文本向上**，fivesing/migu/kugou 同款）、joox 空 songid
   在 resolve/lyrics 双面 NotFound；歌词门用例（qianqian/joox）挂
   `musid-dli-adapter-lyrics`。**tx/wy/joox 无 manager 级双失败链**
   （legacy 侧同请求无离线守卫、必须走网络——如实登记，失败侧对账由
   adapter 级用例 + 运行期 probe 承载，§12.2 各块）。
4. **wire fixture**：`track_qqmusic.json`/`track_wy.json`/
   `track_qianqian.json`/`track_joox.json`（provenance 分两档：
   qianqian/joox = 上游 live 捕获、qqmusic/netease = 上游重建形状；
   track_wy 与 V0 track_netease 的刻意差异见 §12.3）；adapter 单测
   8 例（fixture 映射 + 真实 legacy parse 的 parity 归一对照，tx/wy 的
   parse_song 本就 pub(crate) 零改动）。
5. **登记切片（reg-soda / reg-ytm）**：`source/Cargo.toml` 新增
   `musid-dli-adapter-reg-soda` / `musid-dli-adapter-reg-ytm`——均为
   `["musid-dli-adapter"]`（**无 musid-dli/source-* 可挂**：soda 是
   voicefox-only、musid-dli 0.2.0 无 source_soda；ytm 是 musid-dli-only、
   `source-youtube-music` 依赖 rustypipe，voicefox 不启用——rustypipe
   不进 voicefox feature 树）。两 feature 只作门禁/账面载体：
   `adapter_musid.rs` 各 1 例登记断言（soda：命名表双向拒绝 + 无注册表
   行 + face_route LegacyOnly；ytm：provider_to_source_id("youtube-music")
   == None + track 携带该 provider 时 `UnmappedProvider`），随所在
   feature 进外层门禁（--lib）。voicefox 自实现（soda 四面）完整保留；
   运行行为零变化。

**legacy 侧 parity 消费点可见性扩宽（仅 `pub(crate)`，函数体零改动，
§12.5 先例同款登记）**：`qianqian/song.rs parse_song`（pub(super) →
pub(crate)）、`joox/api.rs songs_from_search`（pub(super) → pub(crate)）。

**CI（musid-stack job）**：4 个子 feature 全量 `cargo test` 步（qqmusic/
netease/qianqian/joox，真实 provider 测试随门进 CI）+ 十一源 + lyrics
组合步（`migrated_slices_assemble_without_ledger_gap` 全等空分支与
qianqian/joox 歌词 stub 路由用例在该组合下编译跑通）。

**观测口径（翻源后）**：qianqian/joox 是用户启用源——翻源后首周盯
`MusidErrLegacyOk` 计数（§12.3）；wy/tx 的 probe 对账计数须先剥离 P5
登录态不对称（musid 恒匿名 vs cookie 登录，§11.8 (b)）再解读
ParityMatch/Mismatch 噪声。

**本批实跑记录（2026-11-21，全部在本会话执行）**：
- 外层门禁命令 `cargo test -p lx-source --features
  musid-dli-adapter-qqmusic,musid-dli-adapter-netease,
  musid-dli-adapter-qianqian,musid-dli-adapter-joox,
  musid-dli-adapter-reg-soda,musid-dli-adapter-reg-ytm --lib`：
  **298 通过 / 0 失败 / 2 ignored**（默认基线 220 + 批内 lib 单测增量；
  含 reg-soda/reg-ytm 登记断言 2 例与四源 bootstrap 装配测试 8 例）；
  同参数 `cargo clippy -p lx-source … --lib -- -D warnings`：exit 0；
- 确定性硬门 `cargo test -p lx-source --features musid-dli-adapter --
  adapter_musid`：**65 通过 / 0 失败**（含本批 adapter 单测 8 例 +
  登记 2 例 + 批 1 既有各例）；
- 四源组合 `cargo test -p lx-source --features
  musid-dli-adapter-qqmusic,musid-dli-adapter-netease,
  musid-dli-adapter-qianqian,musid-dli-adapter-joox --tests` 与
  歌词变体、默认基线、fmt/tree 门：结果见交付记录（外层门禁复跑口径
  与 §12.5/§12.6 相同）。

---

## 13. V3 本地库 / 歌词 / 封面接入（2026-11-21 起；逐切片）

原则（master plan §13.5 + §13.3）：voicefox 逐步**使用** musid-dli 的通用
元数据、CUE、Artwork、Lyrics 能力；数据库、索引 schema、Watcher 调度、
UI、进度与用户操作仍由 voicefox 管理；SourceManager 策略不动。每个切片
以 parity 对账测试为证据（迁移前后对比扫描结果、曲目数量、CUE 轨、标签、
歌词），musid-dli 0.2.0 从 crates.io 引入，子 feature 门控，默认树零变化。

### 13.1 切片 s1：本地扫描对账 + 本地歌词读委托（musid-dli-adapter-local）

**feature**：`musid-dli-adapter-local = ["musid-dli-adapter", "musid-dli/local"]`
（source/Cargo.toml）。musid `local` 面带来 local_scan 扫描核心、
`local::read_lyrics` 歌词读（L17/L18 全量）、tag_write 标签写（L16 全量）。

**委托（唯一的行为接线）**：`LocalSource::read_local_lyric`
（`source/src/local/mod.rs`）在 feature 下整体委托
`musid_dli::local::read_lyrics`——该实现是 voicefox 原函数的逐字移植
（外挂 `.lrc`（小写扩展名）优先、UTF-8 失败降级 GB18030、非空白外挂胜出、
否则内嵌 `ItemKey::Lyrics` 兜底；LOCAL_LIBRARY_PARITY.md L17/L18 行）。
feature 关闭时保留本仓实现（`read_text_file` 同步门控，`encoding_rs`
在 scanner.rs CUE 解码与 kw/lyric.rs 另有使用，依赖面不受影响）。
`get_lyric` 的 spawn_blocking 包装与空歌词语义（`LyricData::default`）不变。

**对账（比较测试，不改本仓实现）**：`source/tests/musid_local_parity.rs`
挂 `musid-dli-adapter-local` 门，运行期合成语料（44100 Hz mono 16-bit PCM
WAV stub + CUE sheet + 外挂歌词，无真实用户目录），11 用例：

1. `plain_scan_metadata_specs_and_fingerprints_match`：条目配对逐字段
   （标题/艺术家/专辑回填 L15、时长、实测规格 AudioProperties ↔ AudioSpec
   四字段、指纹 size）、无标签文件的「未知艺术家/未知专辑 + 文件名干」
   回填、统计（parsed/reused/discovered/failed）。
2. `cue_sheet_tracks_match_field_by_field`：1 整轨 + 2 分轨（引用音频
   两侧同口径保留整轨条目）；分轨 id ↔ container_track 形状
   `{cue}#NN`、cue_file/cue_track/cue_start_ms extras、INDEX 01 起点、
   轨时长（下一 INDEX 01 - 本轨）、TITLE/PERFORMER/sheet TITLE 落位；
   键法对应：voicefox `LocalSong.file_path`（CUE 路径，复用键）↔ musid
   `source_path`，voicefox `SongInfo.file_path`（音频）↔ musid `play_path`。
3. `incremental_reuse_accounting_matches`：普通音频上双侧复用记账一致
   （voicefox 2/2、musid stats.reused 2/2、条目级 reused 位全真）。
   已知键法差异（LOCAL_LIBRARY_PARITY.md §2，不断言相等）：voicefox 复用
   键 = 来源文件（单 FILE sheet 的分轨共享音频路径键组成 Vec），musid =
   source_path（CUE 路径）。
4. `duplicate_group_keys_match`：同元数据两文件的归一化键逐字节一致
   （空白折叠 + 小写 + `\u{1f}` 连接 + 精确时长，L12）。
5. `corrupt_file_lands_in_both_ledgers_without_blocking`：损坏文件进双侧
   台账且不挡好文件（L8）。
6. 歌词四用例：外挂 UTF-8 优先 / 外挂 GB18030 降级（encoding_rs 编码
   构造）/ 内嵌兜底（musid tag_write 写入后双侧同文）/ 双缺两侧空。
7. `voicefox_local_get_lyric_equals_musid_read_lyrics` +
   `voicefox_local_get_lyric_empty_for_bare_file`：委托 pin（trait 入口
   `LocalSource::get_lyric` 与 musid 直读同源同文，双缺空串）。

**门禁实跑（2026-11-21，本会话）**：
- `cargo test -p lx-source --features musid-dli-adapter-local --test
  musid_local_parity`：**11 通过 / 0 失败**；
- `cargo clippy -p lx-source --features musid-dli-adapter-local
  --all-targets -- -D warnings`：exit 0；
- `cargo fmt --all -- --check`：clean；
- 默认树回归 `cargo test -p lx-source --lib --release`：**218 通过 /
  0 失败 / 2 忽略**（feature 关闭分支编译，零行为变化）。

**CI**：musid-stack job 新增「Tests (V3 local library parity slice)」步
（`.github/workflows/ci.yml`）。

**范围留待后续切片**：歌词缓存（§13.2，LyricsCache 文档域接入 musid 歌词
槽）、本地封面（§13.3，`local_cover::read_cover` 外挂封面回落）。

### 13.2 切片 s2：歌词文档缓存接入（musid-dli-adapter-lyrics-cache）

**feature**：`musid-dli-adapter-lyrics-cache =
["musid-dli-adapter-lyrics", "musid-dli/lyrics-cache"]`（source/Cargo.toml）。
musid `LyricsCache`（RESOURCE_DESIGN.md §8）文档域接入 adapter 歌词面；
搜索域与负域**不接**——搜索域按 SearchMode 键控搜索列表，voicefox 的
歌词链不走 musid 搜索；负域（`NegativeCache` + M2 错误类别）由 manager
链级负缓存承担策略（`LyricNegativeCache`：600 s TTL、1024 容量、网络
错误不投毒、全链确认无词才记——§11 的既有语义，原样保留）。

**接线（`adapter_musid.rs`）**：`AdapterMusicSource` 新增 cfg 门控字段
`lyrics_cache: LyricsCache`（`LyricsCacheConfig::default()`：文档 24 h、
容量 512）。`get_lyric` 流程：

1. `document_get(provider, id)`（键 = musid `TrackId` 的 provider + id，
   稳定 id，绝无标题参与）：`Some(Some(doc))` 命中直接
   `lyric_document_to_lx_data` 返回；`Some(None)` 是缓存的确认无词，归
   `FetchError::NotFound`——与新鲜 `Ok(None)` 完全同形；
2. 未缓存才打 provider：`Ok(Some)` / `Ok(None)` 写入文档域；
   `Err` **不写缓存**（网络抖动不投毒，错误直接向上）。

**probe 对账透明性**：probe 影子与主路径共用本面，缓存前后对账结论不变
——单侧瞬时 Err 本就不计结论；双缺两侧都 NotFound；内容对照以真实产物
为准（缓存内容即真实产物）。manager 计数（MusidOk 等）读的是用户可见
行为，缓存命中仍是真实有词，计数语义不变。

**测试（adapter_musid.rs，`--lib` 门，计数桩钉行为）**：

- `lyrics_cache_hit_skips_the_second_provider_call`：两次 get_lyric 同文，
  provider 调用数 = 1；
- `lyrics_cached_absence_answers_not_found_without_recalls`：确认无词
  两次 NotFound，provider 调用数 = 1；
- `lyrics_errors_are_never_cached`：provider 恒 Err，三次全 Err 且错误
  变体不变（若误缓存会变 NotFound），错误永不落缓存。

**门禁实跑（2026-11-21，本会话）**：
- `cargo test -p lx-source --features musid-dli-adapter-lyrics-cache
  --lib`：**296 通过 / 0 失败 / 2 忽略**（含本切片 3 例）；
- `cargo clippy -p lx-source --features musid-dli-adapter-lyrics-cache
  --all-targets -- -D warnings`：0 错误；
- `cargo fmt --all -- --check`：clean；
- 歌词变体回归（缓存代码 cfg 关闭）`--features musid-dli-adapter-lyrics
  --lib`：**293 通过 / 0 失败 / 2 忽略**；默认树 release：**218 通过 /
  0 失败 / 2 忽略**（零变化）。

**CI**：musid-stack job 新增「Tests (V3 lyrics document cache slice)」步。

### 13.3 切片 s3：本地封面解析回落（musid-dli-adapter-artwork-local）

**feature**：`musid-dli-adapter-artwork-local =
["musid-dli-adapter", "musid-dli/artwork-local"]`（source/Cargo.toml）。
musid `artwork-local` 是 union feature（artwork + local-cover，
`local-cover` 自带 `local`）；带来 `local_cover::read_cover`（L19 全量：
内嵌首标签封面进解码校验缓存，键 sha256(音频路径字节 + 图片字节) 命名
`{hash}.jpg`，与 voicefox `cover_cache::embedded_cover_path` 同配方）与
同目录外挂封面检出（cover.jpg/cover.png/folder.png 等，大小写不敏感，
检出不复制不校验）。

**接线（`source/src/local/mod.rs`）**：`LocalSource::get_cover_url` 的
模型封面（`song.cover_url`，扫描期产物）优先原样返回不变；缺席时经
musid `read_cover(EmbeddedFirst)` 解析回落——内嵌（缓存化）优先、其次
外挂检出；再缺才 NotFound。阻塞面（标签探测 + 可能的解码校验）与
`get_lyric` 同款走 spawn_blocking。feature 关闭时维持原 NotFound 行为，
默认树零变化。缓存目录复用 `cover_cache::cache_dir()`（voicefox 既有
内容寻址封面缓存约定，两栈键配方同源）。

**测试（`source/tests/musid_local_cover_parity.rs`，挂
`musid-dli-adapter-artwork-local` 门，5 用例）**：

1. `embedded_cache_key_recipe_matches_across_stacks`：MP3 fixture 写入
   内嵌封面后，voicefox 扫描管线的 cover_url 缓存路径与 musid
   `read_cover` 在同一缓存目录的产物**逐字符相同**（键配方同源的证据）；
2. `external_cover_is_resolved_when_no_embedded_art`：无内嵌标签 +
   同目录 cover.png → get_cover_url 落外挂文件原路径（musid 直读一致）
   ——**新能力**：本仓原实现此处恒 NotFound；
3. `model_cover_url_still_wins_unchanged`：模型封面在则原样返回，
   优先级不变；
4. `absent_everywhere_still_not_found`：双缺恒 NotFound（行为兼容）；
5. `embedded_first_priority_is_pinned_in_read_cover`：内嵌与外挂俱在时
   内嵌落缓存目录、外挂按原路径返回（防回落路径误配 ExternalFirst）。

测试语料：WAV stub（外挂/缺席用例；WAV 无封面槽，musid local_cover.rs
测试注释同口径）+ musid 同款 ffmpeg 构建 MP3 fixture
（`source/tests/fixtures/local/spec_mp3_cbr128.mp3`，musid-dli 仓
`tests/fixtures/local/` 逐字拷贝，出处见该目录 README.md）+ 68 字节
1x1 RGBA PNG（zlib/struct 生成，chunk CRC 与 image crate 解码验证）。

**门禁实跑（2026-11-21，本会话）**：
- `cargo test -p lx-source --features musid-dli-adapter-artwork-local
  --test musid_local_cover_parity`：**5 通过 / 0 失败**；
- `cargo clippy -p lx-source --features musid-dli-adapter-artwork-local
  --all-targets -- -D warnings`：0 错误；
- `cargo fmt --all -- --check`：clean；
- 默认树回归 `--lib --release` 与 artwork-local `--lib`：见提交说明
  （实跑数字随提交记录）。

**CI**：musid-stack job 新增「Tests (V3 local cover parity slice)」步。

---

## 14. V4 冗余实现删除（2026-11-22 起；逐切片）

规则（master plan §13.1 + 交接文档删除规则）：每源**只在** parity 绿且
用户可见面全覆盖后删除旧实现；每次删除一个 commit、parity 证据进提交
说明；musid 缺覆盖的面保留 legacy 路径，不为「统一」砍功能。

### 14.0 逐源删除账本（V4 起点，2026-11-22 盘点）

全仓盘点（`source/src/<source>/` 的 `impl MusicSource` 面 × musid-dli
0.2.0 provider trait 实现面）：

| 源 | legacy 用户可见面 | musid 覆盖 | 缺口 | 可删 |
|---|---|---|---|---|
| joox | search/url/lyric/cover + search_playlists/playlist_detail/album_songs/parse_link | **8/8 全覆盖**（M4 逐字移植：CollectionProvider/PlaylistProvider/AlbumProvider/LinkResolver，fixture 钉住） | 无 | **已删（14.1）** |
| qianqian | 上 8 面 + get_playlist_categories/get_playlists（浏览面） | **10/10 全覆盖**（M4 逐字移植：CollectionProvider/PlaylistProvider（categories+playlists+search_playlists）/AlbumProvider/LinkResolver，§14.0 初版误记 parse_link 缺失——musid 状态日志与 source_qianqian.rs:1015 的 LinkResolver impl 修正） | 无 | **已删（14.3）** |
| wy/tx/kg/kw/mg/bili | 集合面 + 登录（kg login.rs / bili SESSDATA / tx 个人歌单 / mg SMS / kw VIP） | 数据面 3/4 已迁（V2），集合面/登录缺 | 同左 | 待 musid 补集合面与登录位 |
| jamendo/fivesing/apple | 集合面（search_playlists/playlist_detail/album_songs/parse_link） | 数据面 3/4 已迁，集合面缺（musid 容器端点匿名挂起） | 集合面 | 待 musid 补 |
| soda | 全部四面 | musid 无此源 | 全部 | 不迁（voicefox-only，reg-soda 登记） |
| local | 本地库全集 | musid local 已对账（V3 §13.1），主注册表留 voicefox（schema/状态/UI 属 voicefox，§13.3 边界） | — | 不迁 |

结论：**joox 与 qianqian 满足「全用户可见面覆盖」**（均已删，见
§14.1/§14.3）；其余源的删除卡在 musid-dli 0.2.0 的集合面/登录缺口上
（musid-dli 侧补齐属于上游变更，不在本仓 V4 范围内）。kw/mg 的 musid
集合 trait impl 齐全但**逐面真实性未核验**（可能有 Unsupported 桩——
fivesing 的 playlist_tracks 已验证是桩），删除前必须逐面核验；wy/tx/
kg/bili 另有登录/个人歌单 legacy 独有面。

### 14.1 切片 1：joox legacy 实现删除（musid-dli-adapter-joox 升 default）

**删除**：`source/src/joox/`（mod.rs + api.rs，legacy 8 面实现）整体
删除；`manager.rs` 的 `JooxSource` 注册与 import、`lib.rs` 的
`pub mod joox` 随之注销。

**主注册表直装**：`SourceManager::new` 的注册块以
`crate::musid_bootstrap::joox(&TransportInjection::new(None, 15))`
（feature 门控）装配 musid 适配面进 `self.sources`——joox 的唯一实现
路径。TransportInjection 仅诊断登记（§4 口径：0.2.0 不注入 transport，
voicefox 代理/超时对 musid provider 不生效——该缺口原为影子面登记，自
本切片起对 joox 全面生效，如实入账）。

**adapter 集合面接线（§12 注册表的原「无行」位）**：
`AdapterMusicSource` 新增四个集合槽位（collection/playlists/albums/
links，`musid-dli-adapter` 门控）+ `with_collections` 装配器；joox 装配
分支四槽同实例（musid `JooxProvider` 按 trait 拆面）。四个 trait face
实装（`musid-dli-adapter-joox` 门控）：

- `search_playlists`：musid `PlaylistProvider::search_playlists`（legacy
  逐字移植：search type=0 读 section_list，空结果恒 Ok 不报错）→
  `playlist_to_lx` 逐条转换；
- `get_playlist_detail`：musid `CollectionProvider::playlist_tracks` →
  `track_to_song` 逐条；空曲目归 `NotFound`（legacy 空守卫同口径）；
- `get_album_songs`：musid `AlbumProvider::album_tracks` →
  `SearchResult { total, has_more: false, items }`（OpenJOOX 专辑是特殊
  歌单，与 legacy 同一接口）；
- `parse_link`：musid `LinkResolver::resolve_link` →
  `ParsedLink::Song/Playlist/Album` 三分支（playlist 分支曲目取
  `playlist_tracks`、album 分支取 `album_tracks`，与 legacy「详情 +
  曲目一次取齐」同形）；`Artist` 变体归 Other（joox 不产）。

**模型转换**：`playlist_to_lx` 复用既有 V0 转换（namespace 表推源、
extra 透传）；新增 `album_to_lx_playlist`（专辑以歌单形状进详情页，
artists 首位落 creator 槽，legacy 专辑分支同构）。

**能力位**：`capabilities()` 的面级 flags 按源登记——joox
`has_search_playlists = true`（musid `PlaylistProvider` 有真实实现）；
playlists/album/link_parse 位来自 provider descriptor（本就 true）。与
legacy joox/mod.rs:103-110 的声明逐位一致。

**feature flip**：`lx-source` 新增 `default =
["musid-dli-adapter-joox"]`——legacy 删除后 joox 子 feature 是源存续的
前提，进 default 保默认构建不缺源；`--no-default-features` 仍可构建零
musid 树（此时 joox 源整体缺席）。V1 的「默认树零 musid」口径自此由
V4 的目标架构（master plan §1.3：musid-dli 是 provider 引擎）取代，仅
joox 一个源先行。

**路由语义**：joox 的 §12 注册表行保持 Migrated 不变；影子注册不再有
joox（`register_available` 不装配，`unregistered_migrated_sources` 显式
跳过）——四面路由判定影子查空后回落主注册表 = 适配面自身，行为一致；
parity 计数对 joox 静默（迁移监控语义已随 legacy 死亡终结，无对账对
象）。`bootstrap_musid_stack` 的启动 warn 不再出现 joox。

**测试账**：
- `joox_search_parity_normalized_equivalent`（crate::joox::api 真实
  legacy parse 双侧对账）随源注销——musid 侧形状由 track_joox.json
  fixture 映射用例与 musid 仓 tests/fixtures/joox 的 provider 测试继续
  钉住；§12.2 joox 块的歌手 join 分歧（legacy " / " vs musid '、'）随
  legacy 死亡收敛为 musid 口径（voicefox SongInfo 约定即 '、'）；
- bootstrap 切片测试 `joox_slice_is_registered_...` 改为
  `joox_slice_is_primary_and_never_shadow_registered`（影子注册无 joox
  行 + warn 输入跳过）；
- 账本测试两处 warn 输入列表 Joox 摘除（十一源 → 十源口径）；
- `musid_joox_slice.rs`（musid-only import）不受影响继续通过；
- 封面面口径差：legacy get_cover_url 缺封面返回 `Ok("")`，适配面返回
  `NotFound`（V0 消费面口径）——行为收紧如实登记。

**门禁实跑（2026-11-22，本会话）**：
- `cargo test -p lx-source --lib`（default，含 joox 适配面测试）：
  **284 通过 / 0 失败 / 2 忽略**；
- `cargo test -p lx-source --no-default-features --lib`（零 musid 边
  界）：**213 通过 / 0 失败 / 2 忽略**（218 − 5 个随 legacy 死亡的
  joox 库测，边界语义不变）；
- `cargo test -p lx-source --features musid-dli-adapter-joox --test
  musid_joox_slice`：**2 通过 / 0 失败**；
- `cargo test -p lx-source --test musid_dual_stack`：**30 通过 /
  0 失败**；
- `cargo clippy -p lx-source --all-targets -- -D warnings`（default）与
  `--features musid-dli-adapter --all-targets`：均 0 错误；
- `cargo fmt --all -- --check`：clean。

### 14.2 V5 前置：CI 覆盖面（2026-11-22，随 V4-s1 后提交）

- lint job 新增「Clippy (adapter feature, zero warnings)」步：
  `cargo clippy -p lx-source --features musid-dli-adapter --all-targets
  -- -D warnings`——default 只带 joox 子 feature（V4-s1 flip），其余九个
  子 feature 的装配分支与 musid cfg 代码由本步以零警告口径覆盖（与
  musid-stack job 的测试步互补）。`double_must_use` 例外沿用（core/
  src/lib.rs 的登记：async-trait 宏展开，源码层面消除不掉）。
- 全部五个 job（lint/musid-stack/三平台构建）加 Swatinem/rust-cache@v2。

## 15. V5 CI 完整回归（2026-11-22）

- **rust-cache**：五个 job 全部加 Swatinem/rust-cache@v2（checkout 后
  一步），依赖缓存跨 run 复用。
- **lint 覆盖 adapter 特性**：见 §14.2。
- **MINGW64 弃用注解**：Windows job 的 msys2/setup-msys2 注解
  `[msystem-mingw64] MINGW64 is deprecated. Migrate to UCRT64 or
  CLANG64`——真实迁移涉及 libmpv DLL 的 ABI/导入表校验与全部 mingw64
  路径绑定（sha256 固定的音频构建包按 MINGW64 ABI 出包），**非平凡
  变更**，按注解自带的确认口径 `suppress-deprecation-warnings:
  msystem-mingw64` 显式抑制并延后；真实迁移需重验 import table 检查与
  三平台打包（诚实登记：这是抑制而非修复，延后理由如上）。
- 验证：提交后的 CI run 全绿（run id 见提交说明与 F0 报告）。

### 14.3 切片 2：qianqian legacy 实现删除（musid-dli-adapter-qianqian 升 default）

**删除**：`source/src/qianqian/`（mod.rs + song.rs + url.rs + lyric.rs +
playlist.rs + album.rs + parse.rs + crypto.rs，legacy 10 面实现）整体删
除；`manager.rs` 的 `QianqianSource` 注册与 import、`lib.rs` 的
`pub mod qianqian` 随之注销。

**覆盖核验（删除前提，§14.0 行 2）**：musid-dli 0.2.0
`QianqianProvider` 十面全覆盖——TrackSearch/StreamResolver/LyricsProvider
（V2 切片 10 账）+ CollectionProvider/PlaylistProvider（categories :877、
playlists :923、search_playlists、user_playlists Unsupported 与 legacy
无个人歌单一致）/AlbumProvider/LinkResolver（:1015，song/songlist|
tracklist|playlist/album/albumAssetCode=/albumid=/bare-id 路由为 legacy
parse.rs 逐字移植）。§14.0 初版误记 parse_link 缺失（grep 漏
LinkResolver impl），以 musid 状态日志 + 源码修正。

**主注册表直装**：`SourceManager::new` 以
`crate::musid_bootstrap::qianqian(&TransportInjection::new(None, 15))`
（feature 门控）装配进 `self.sources`——qianqian 的唯一实现路径。

**adapter 集合面接线**：四槽路由实现合并为
`cfg(any(musid-dli-adapter-joox, musid-dli-adapter-qianqian))` 的通用块
（search_playlists/get_playlist_detail/get_album_songs/parse_link，错误
文案去源名）；qianqian 专属浏览面 `get_playlist_categories`/
`get_playlists`（cfg qianqian：musid categories 带合成「全部」lead、
playlists 走 subCateId 过滤、pageSize 30 与 legacy 同值硬编码）。
`playlist_category_to_lx` 为既有 V0 转换（provider 必填语义不变）。

**能力位**：qianqian flags = has_search_playlists + has_categories 为
true（musid PlaylistProvider 两面真实实现）；playlists/album/link_parse
位来自 descriptor（本就 true）。与 legacy 声明逐位一致。

**feature flip**：default 扩为
`["musid-dli-adapter-joox", "musid-dli-adapter-qianqian"]`。

**路由语义**：与 joox 同——影子注册无 qianqian 行，四面回落主注册表 =
适配面自身；parity 计数静默（无对账对象）；账本 warn 显式跳过。

**测试账**：
- `qianqian_search_parity_normalized_equivalent`（crate::qianqian::song::
  parse_song 双侧对账）随源注销——musid 侧形状由 track_qianqian.json
  fixture 映射用例与 musid 仓 tests/fixtures/qianqian 的 provider 测试
  继续钉住；
- slice 测试的 manager 级「musid 失败 → legacy 同键失败」双侧链路用例
  随源注销（qianqian 失败语义由 adapter 级离线用例承载）；
- manager 的 `builtin_sources_declare_their_capabilities` 分类入口清单
  改 feature 感知（零 musid 树下 qianqian 缺席）；
- bootstrap 切片测试改 primary 口径 + flags pin 翻真（playlist_search/
  playlist_categories）；
- 账本测试两处 warn 输入摘 Qianqian（十源 → 九源口径）。

**门禁实跑（2026-11-22，本会话）**：
- `cargo test -p lx-source --lib`（default，双适配源）：
  **271 通过 / 0 失败 / 2 忽略**（284 − legacy qianqian 自带单测 +
  qianqian 适配面测试净变化）；
- `cargo test -p lx-source --no-default-features --lib`：
  **199 通过 / 0 失败 / 2 忽略**（零 musid 边界，213 − legacy qianqian
  库测）；
- `cargo test -p lx-source --features musid-dli-adapter-qianqian --test
  musid_qianqian_slice`：**2 通过 / 0 失败**；
- `cargo test -p lx-source --test musid_dual_stack`：**30 通过 / 0 失败**；
- `cargo clippy -p lx-source --all-targets -- -D warnings`（default）与
  `--features musid-dli-adapter --all-targets`：均 0 错误；
- `cargo fmt --all -- --check`：clean。
