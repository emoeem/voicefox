# musid-dli 适配层设计（V0）

> 状态：**V0 已落地**（2026-11-20；映射函数 + fixture 测试，未接业务路径）。
>
> 落地物：
> - 映射代码 `source/src/adapter_musid.rs`，由 `source/src/lib.rs:31-32` 在
>   feature `musid-dli-adapter` 后注册；
> - fixture `source/tests/fixtures/musid/`（8 个 JSON，来源与形状说明见 §7）；
> - 依赖方案 `source/Cargo.toml:43-49`：crates.io `musid-dli = "=0.2.0"`
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
> 依据：2026-11-20 双仓工作树现场盘点（`pairs` 逐条读过源码）+ 本阶段复核。
> 本文是 voicefox 侧 `lx-source` 与 musid-dli 0.2.0（crates.io）之间的
> 模型映射契约；V1-V4 的升级顺序草案见 §5，取消/超时/换源语义对齐见 §6。

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
- **V1（双栈接入）**：
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
  2. `SourceManager` 注册双栈源；session 统一（§2.9）。
  3. 依赖升档：`"=0.2.0"`→`"0.2"`（wire 形状已 pin，V0 禁漂移）。
- **V2（引擎收敛候选）**：下载引擎切片消费 `ResolvedUrl.fallback_endpoints`
  per-candidate 元数据；musid-dli `lx-js-bridge` 收敛评估（质量折叠
  Lossless/HiRes→'flac' 需先对齐 voicefox JS 质量语义）；本地扫描合并候选
  （musid-dli `local`，避免双扫描器语义漂移）。
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
musid-dli = { version = "=0.2.0", optional = true, default-features = false }

[features]
musid-dli-adapter = ["dep:musid-dli"]
# 歌词往返映射变体（仅测试用）：多拉 musid-dli/lyrics-conversions（含 lyrics-helper 0.5.1 default-features=false）
musid-dli-adapter-lyrics = ["musid-dli-adapter", "musid-dli/lyrics-conversions"]
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
