//! 音源管理器：注册、调度、换源匹配
//!
//! 对标 lx-music src/renderer/utils/musicSdk/index.js

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use lx_core::model::leaderboard::LeaderboardInfo;
use lx_core::model::lyric::LyricData;
use lx_core::model::playlist::Playlist;
use lx_core::model::playlist::PlaylistCategory;
use lx_core::model::playlist::{Album, Artist};
use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceHealth, SourceId};
use lx_core::traits::source::{
    FetchError, MusicSource, ParsedLink, SearchError, SearchResult, SongUrl, SourceCapabilities,
};

use crate::apple::AppleSource;
use crate::bili::BiliSource;
use crate::fivesing::FivesingSource;
use crate::jamendo::JamendoSource;
use crate::kg::KgSource;
use crate::kw::KwSource;
use crate::local::LocalSource;
use crate::mg::MgSource;
use crate::soda::SodaSource;
use crate::tx::TxSource;
use crate::wy::WySource;

struct JsSourceState {
    generation: u64,
    sources: Vec<JsSourceEntry>,
}

struct JsSourceEntry {
    origin: String,
    source: Arc<dyn MusicSource>,
}

/// 音源管理器
type LyricNegativeCache = HashMap<(SourceId, Option<String>, String), Instant>;

/// V1 双栈路由开关（feature `musid-dli-adapter`；对应 config.source.musid_*，
/// 全部默认 false——不开 opt-in 时旧实现唯一生效）。
#[cfg(feature = "musid-dli-adapter")]
struct MusidRoutes {
    stack: std::sync::atomic::AtomicBool,
    search: std::sync::atomic::AtomicBool,
    song_url: std::sync::atomic::AtomicBool,
    lyric: std::sync::atomic::AtomicBool,
    parity_probe: std::sync::atomic::AtomicBool,
}

/// 双栈路由开关的公共设置入口（`lx_source::bootstrap_musid_stack` 构造）。
#[cfg(feature = "musid-dli-adapter")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MusidRouteConfig {
    /// 总闸：false 时四面全走旧路径。
    pub stack: bool,
    /// 搜索面。
    pub search: bool,
    /// 流地址面。
    pub song_url: bool,
    /// 歌词面（运行时另需 `musid-dli-adapter-lyrics` 歌词转换变体）。
    pub lyric: bool,
    /// parity 对账影子模式（纯观测：旧路径权威恒展示旧路径结果，musid
    /// 调用仅用于计数对照且不阻塞用户路径）。
    pub parity_probe: bool,
}

/// 聚合搜索任务输出：(源, 超时包裹的单源结果)。旧路径任务与 musid 替换
/// 任务共用该形状（同 JoinSet 同预算），probe 影子任务复用同一形状走
/// 第二个 JoinSet。
#[cfg(feature = "musid-dli-adapter")]
type AggregateTaskOutput = (
    SourceId,
    Result<Result<SearchResult, SearchError>, tokio::time::error::Elapsed>,
);

/// musid 搜索尝试预算（替换模式）：每源/单源预算的前半段——musid 慢失败
/// 或挂死不再把回退挤到归零（评审 P2）；回退继承剩余预算，整体仍 ≤ 基线
/// （聚合每源 10s / 单源 app 外层 12s）。
#[cfg(feature = "musid-dli-adapter")]
const MUSID_ATTEMPT_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// musid resolve 槽位预算（流地址面）：外层 30s 的前段——musid 慢失败后
/// 回退链仍有 ≥20s（评审 P3；对比基线满 30s 的残余退化在 §11.4 登记；
/// musid-dli 0.2.0 内部固 15s 不可注入，预算只能在宿主侧收口）。
#[cfg(feature = "musid-dli-adapter")]
const MUSID_RESOLVE_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// V1 双栈流地址槽位状态（feature `musid-dli-adapter`）。
#[cfg(feature = "musid-dli-adapter")]
struct MusidUrlSlot {
    musid: Option<Arc<dyn MusicSource>>,
    /// 计数键：路由命中的平台（JS 歌按 extra[source] 还原）。
    source: SourceId,
    /// probe 影子模式：不抢跑，旧链失败后仅对账计数（纯观测，评审 P1）。
    probe: bool,
    /// musid resolve 是否已尝试过（err 侧 legacy 归因的输入）。
    attempted: bool,
}

/// V1 双栈歌词槽位状态（feature `musid-dli-adapter`）：content 为 musid
/// 产物（已过同一内容门）；empty_source 为"musid 已尝试且无产出"的归因
/// 平台（err 侧 legacy 归因的输入）。
#[cfg(feature = "musid-dli-adapter")]
struct MusidLyricSlot {
    content: Option<LyricData>,
    empty_source: Option<SourceId>,
}

pub struct SourceManager {
    sources: HashMap<SourceId, Arc<dyn MusicSource>>,
    /// JS 自定义音源，按配置顺序依次用于解析，并共同参与聚合搜索。
    js_sources: std::sync::RwLock<JsSourceState>,
    /// 本地音乐源（单独存储以便调用扫描等特有方法）
    local_source: Arc<LocalSource>,
    /// 哔哩哔哩音源（单独存储以便调用登录等特有方法）
    bili_source: Arc<BiliSource>,
    default: std::sync::RwLock<SourceId>,
    enabled: std::sync::RwLock<HashSet<SourceId>>,
    /// 「JS 音源优先」解析顺序开关；false（默认）= 内置音源先行。
    js_url_first: std::sync::atomic::AtomicBool,
    /// 歌词"确认无词"负缓存（上次确认时间），避免纯音乐/无词歌曲
    /// 每次播放都触发 JS 音源 + 全源聚合补全的长耗时请求。
    /// 键为 (来源，JS 平台标记，歌曲 id)。
    lyric_negative_cache: std::sync::Mutex<LyricNegativeCache>,
    /// V1 双栈影子源（feature `musid-dli-adapter`，opt-in）：独立于
    /// `sources`，绝不覆盖内置注册（`register` 以 `SourceId` 为键，二次
    /// 注册会覆盖内置实现）。影子源不进健康检测、capabilities 聚合、
    /// 设置页入口显隐与登录链路（docs/MUSID_DLI_ADAPTER.md §11）。
    #[cfg(feature = "musid-dli-adapter")]
    musid_sources: std::sync::RwLock<HashMap<SourceId, Arc<dyn MusicSource>>>,
    /// 双栈路由开关（config.source.musid_*，全部默认 false）。
    #[cfg(feature = "musid-dli-adapter")]
    musid_routes: MusidRoutes,
    /// parity 对账计数（内存，不落盘；JoinSet 任务经 Arc 记录）。
    #[cfg(feature = "musid-dli-adapter")]
    parity: Arc<crate::parity::ParityCounters>,
}

/// 判断是否为真本地文件歌曲：JS 音源的搜索结果同样标记为 `SourceId::Local`，
/// 但额外带有 `extra["source"]` 平台标记，两者据此区分。
///
/// 不能用 `file_path` 判定：本地扫描是在 `metadata::read_metadata` 之后才补上
/// `file_path` 的，存在只有 `source` / `id` 的中间态。
///
/// 播放地址解析（`get_song_url_inner`）与「播放失败后是否换源重试」（app 侧）
/// 必须共用这一份判定，否则同一首歌会出现"解析按本地文件、重试按在线歌曲"的分裂。
pub fn is_local_file_song(song: &SongInfo) -> bool {
    song.source == SourceId::Local && !song.extra.contains_key("source")
}

/// JS 音源搜索结果对应的平台内置音源（其 `id` 即该平台的歌曲 id），
/// JS 音源解析失败时可用它回退。
///
/// 覆盖全部已接入内置实现的平台；脚本侧的平台标记以各家 user API 的
/// 惯用写法为准，`SourceId::as_str` 之外列的是常见别名。
pub fn builtin_source_for_js(song: &SongInfo) -> Option<SourceId> {
    if song.source != SourceId::Local {
        return None;
    }
    // 平台标记来自各家脚本，大小写不统一，这里统一按小写匹配。
    match song
        .extra
        .get("source")
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("kw") => Some(SourceId::Kw),
        Some("kg") => Some(SourceId::Kg),
        Some("tx") => Some(SourceId::Tx),
        Some("wy") => Some(SourceId::Wy),
        Some("mg") => Some(SourceId::Mg),
        // 千千（原百度音乐）
        Some("qianqian") | Some("bd") | Some("baidu") => Some(SourceId::Qianqian),
        Some("joox") => Some(SourceId::Joox),
        Some("jamendo") => Some(SourceId::Jamendo),
        // 5sing 脚本里也写作 fiveSing / 5sing
        Some("fivesing") | Some("5sing") | Some("fivesing_kugou") => Some(SourceId::Fivesing),
        Some("apple") | Some("applemusic") => Some(SourceId::Apple),
        Some("bili") | Some("bilibili") => Some(SourceId::Bili),
        // 汽水音乐（抖音）
        Some("soda") | Some("qishui") | Some("douyin") => Some(SourceId::Soda),
        _ => None,
    }
}

impl SourceManager {
    pub fn new(default: SourceId, enabled: &[SourceId]) -> Self {
        let local_source = Arc::new(LocalSource::new());
        let bili_source = Arc::new(BiliSource::new());
        let mut manager = Self {
            sources: HashMap::new(),
            js_sources: std::sync::RwLock::new(JsSourceState {
                generation: 0,
                sources: Vec::new(),
            }),
            local_source: Arc::clone(&local_source),
            bili_source: Arc::clone(&bili_source),
            default: std::sync::RwLock::new(default),
            enabled: std::sync::RwLock::new(enabled.iter().copied().collect()),
            js_url_first: std::sync::atomic::AtomicBool::new(false),
            lyric_negative_cache: std::sync::Mutex::new(HashMap::new()),
            #[cfg(feature = "musid-dli-adapter")]
            musid_sources: std::sync::RwLock::new(HashMap::new()),
            #[cfg(feature = "musid-dli-adapter")]
            musid_routes: MusidRoutes {
                stack: std::sync::atomic::AtomicBool::new(false),
                search: std::sync::atomic::AtomicBool::new(false),
                song_url: std::sync::atomic::AtomicBool::new(false),
                lyric: std::sync::atomic::AtomicBool::new(false),
                parity_probe: std::sync::atomic::AtomicBool::new(false),
            },
            #[cfg(feature = "musid-dli-adapter")]
            parity: Arc::new(crate::parity::ParityCounters::default()),
        };
        // 注册内置音源
        manager.register(Arc::new(KwSource::new()));
        manager.register(Arc::new(KgSource::new()));
        manager.register(Arc::new(MgSource::new()));
        manager.register(Arc::new(TxSource::new()));
        manager.register(Arc::new(WySource::new()));
        // V4-s2（docs/MUSID_DLI_ADAPTER.md §14.3）起 qianqian 的 legacy 实
        // 现已删，唯一实现是 musid 适配面：主注册表直装（不走影子 map）。
        // TransportInjection 预算为占位（manager::new 拿不到网络配置）；
        // musid-dli 0.2.1 起注入生效，真实预算随 bootstrap 路径（影子源）。
        #[cfg(feature = "musid-dli-adapter-qianqian")]
        if let Some(qianqian) = crate::musid_bootstrap::qianqian(
            &crate::adapter_musid::TransportInjection::new(None, 15),
        ) {
            manager.register(qianqian);
        }
        // V4-s1（docs/MUSID_DLI_ADAPTER.md §14.1）起 joox 的 legacy 实现已
        // 删，唯一实现是 musid 适配面：主注册表直装（不走影子 map）。
        // TransportInjection 预算为占位（manager::new 拿不到网络配置）；
        // musid-dli 0.2.1 起注入生效，真实预算随 bootstrap 路径（影子源）。
        #[cfg(feature = "musid-dli-adapter-joox")]
        if let Some(joox) =
            crate::musid_bootstrap::joox(&crate::adapter_musid::TransportInjection::new(None, 15))
        {
            manager.register(joox);
        }
        manager.register(Arc::new(FivesingSource::new()));
        manager.register(Arc::new(JamendoSource::new()));
        manager.register(Arc::new(AppleSource::new()));
        manager.register(Arc::new(SodaSource::new()));
        manager.register(bili_source);
        // 注册本地音源
        manager.register(local_source);
        manager
    }

    pub fn register(&mut self, source: Arc<dyn MusicSource>) {
        self.sources.insert(source.id(), source);
    }

    // ------------------------------------------------------------------
    // V1 双栈（musid-dli-adapter feature，opt-in；docs/MUSID_DLI_ADAPTER.md §11）
    // ------------------------------------------------------------------

    /// 注册 musid-dli 影子源。
    ///
    /// 影子源以 [`SourceId`] 为键存入**独立**的 `musid_sources`，绝不写入
    /// `self.sources`（后者以 `SourceId` 为键，二次注册会覆盖内置实现）。
    /// 影子源不参与健康检测、capabilities 聚合、入口显隐与登录链路；只在
    /// 四面路由判定（[`Self::musid_route`]）命中的时候按开关被调用。
    #[cfg(feature = "musid-dli-adapter")]
    pub fn register_musid_source(&self, source: Arc<dyn MusicSource>) {
        self.musid_sources
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(source.id(), source);
    }

    /// 下发双栈路由开关（全部 false = 旧路径唯一生效；config 开而 feature
    /// 关的场景在 `bootstrap_musid_stack` 里就不会到达本方法）。
    #[cfg(feature = "musid-dli-adapter")]
    pub fn set_musid_routes(&self, routes: MusidRouteConfig) {
        let store = |flag: &std::sync::atomic::AtomicBool, value: bool| {
            flag.store(value, std::sync::atomic::Ordering::Relaxed);
        };
        store(&self.musid_routes.stack, routes.stack);
        store(&self.musid_routes.search, routes.search);
        store(&self.musid_routes.song_url, routes.song_url);
        store(&self.musid_routes.lyric, routes.lyric);
        store(&self.musid_routes.parity_probe, routes.parity_probe);
    }

    /// parity 对账计数快照（设置页/诊断只读展示）。
    #[cfg(feature = "musid-dli-adapter")]
    pub fn musid_parity_snapshot(&self) -> Vec<crate::parity::ParityRecord> {
        self.parity.snapshot()
    }

    /// 四面共用的路由判定：总闸 && 面开关 && 源可映射（`soda`/`local`
    /// 恒旧路径；bili 的面级账面由注册表行承载，V2 切片 4 起）&& V2 逐源
    /// 路由注册表放行该面（docs §12）&& 影子源已注册 && 用户 enabled 集
    /// 含该源。
    #[cfg(feature = "musid-dli-adapter")]
    fn musid_route(
        &self,
        face_gate: bool,
        face: crate::parity::ParityFace,
        source: SourceId,
    ) -> Option<Arc<dyn MusicSource>> {
        if !self
            .musid_routes
            .stack
            .load(std::sync::atomic::Ordering::Relaxed)
            || !face_gate
        {
            return None;
        }
        // 命名表可映射的才可路由。V2 切片 4（docs §12.4）起 bili 的排除由
        // 注册表行承载（搜索/流地址 Migrated、歌词 LegacyOnly）——这里只
        // 拒绝 §1 命名表外的 soda/local；bili 的登录/会话/用户数据本就不
        // 经本判定（manager 登录链路不读影子 map，V1 边界 §11.8），扫码
        // 链路与 SESSDATA 独立存储恒走 voicefox 旧路径。
        if matches!(source, SourceId::Local | SourceId::Soda)
            || crate::adapter_musid::source_id_to_provider(source).is_none()
        {
            return None;
        }
        // V2 逐源路由注册表（adapter_musid::face_route，docs §12.1）：面级
        // 账面不放行 = 恒旧路径。LegacyOnly 是能力缺失回落规则的静态层——
        // musid 侧 stub/语义不等价的面（如 jamendo 歌词）在此被拒，用户
        // 永远拿不到 musid 的"该音源不支持"类错误。probe 影子同门（未迁移
        // 面的 probe 不烧 musid 配额）。
        if !crate::adapter_musid::face_route(face, source).routes_to_musid() {
            return None;
        }
        if !self
            .enabled
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&source)
        {
            return None;
        }
        self.musid_sources
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&source)
            .cloned()
    }

    /// 歌曲的双栈路由平台：JS 音源搜到的歌（`Local` + `extra[source]`）按
    /// 平台标记还原，普通歌曲取 `source`。
    #[cfg(feature = "musid-dli-adapter")]
    fn musid_effective_source(song: &SongInfo) -> Option<SourceId> {
        if song.source == SourceId::Local {
            return builtin_source_for_js(song);
        }
        Some(song.source)
    }

    /// parity 计数入口（内存计数，只增不减）。
    #[cfg(feature = "musid-dli-adapter")]
    fn musid_record(
        &self,
        face: crate::parity::ParityFace,
        source: SourceId,
        outcome: crate::parity::ParityOutcome,
    ) {
        self.parity.record(face, source, outcome);
    }

    /// 对账影子模式是否在生效（总闸 && probe）。
    #[cfg(feature = "musid-dli-adapter")]
    fn musid_probe_active(&self) -> bool {
        self.musid_routes
            .stack
            .load(std::sync::atomic::Ordering::Relaxed)
            && self
                .musid_routes
                .parity_probe
                .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 开始一次 JS 音源请求。代次和当前音源受同一把锁保护，
    /// 避免旧任务在检查代次后跨过删除或新导入操作写回。
    pub fn begin_js_source_request(&self, clear_current: bool) -> u64 {
        let mut state = self.js_sources.write().unwrap_or_else(|e| e.into_inner());
        state.generation = state.generation.wrapping_add(1);
        if clear_current {
            state.sources.clear();
        }
        state.generation
    }

    pub fn is_js_source_request_current(&self, generation: u64) -> bool {
        self.js_sources
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .generation
            == generation
    }

    pub fn set_js_source_if_current(&self, generation: u64, source: Arc<dyn MusicSource>) -> bool {
        self.set_js_sources_if_current(generation, vec![source])
    }

    pub fn set_js_sources_if_current(
        &self,
        generation: u64,
        sources: Vec<Arc<dyn MusicSource>>,
    ) -> bool {
        self.set_named_js_sources_if_current(
            generation,
            sources
                .into_iter()
                .map(|source| (String::new(), source))
                .collect(),
        )
    }

    pub fn set_named_js_sources_if_current(
        &self,
        generation: u64,
        sources: Vec<(String, Arc<dyn MusicSource>)>,
    ) -> bool {
        let mut state = self.js_sources.write().unwrap_or_else(|e| e.into_inner());
        if state.generation != generation {
            return false;
        }
        state.sources = sources
            .into_iter()
            .map(|(origin, source)| JsSourceEntry { origin, source })
            .collect();
        true
    }

    pub fn insert_js_source_if_current(
        &self,
        generation: u64,
        source: Arc<dyn MusicSource>,
    ) -> bool {
        let mut state = self.js_sources.write().unwrap_or_else(|e| e.into_inner());
        if state.generation != generation {
            return false;
        }
        state.sources.insert(
            0,
            JsSourceEntry {
                origin: String::new(),
                source,
            },
        );
        true
    }

    pub fn clear_js_source_if_current(&self, generation: u64) -> bool {
        let mut state = self.js_sources.write().unwrap_or_else(|e| e.into_inner());
        if state.generation != generation {
            return false;
        }
        state.sources.clear();
        true
    }

    /// 检查是否有 JS 音源
    pub fn has_js_source(&self) -> bool {
        self.js_source_count() > 0
    }

    pub fn js_source_count(&self) -> usize {
        self.js_sources
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .sources
            .len()
    }

    fn js_sources(&self) -> Vec<Arc<dyn MusicSource>> {
        self.js_sources
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .sources
            .iter()
            .map(|entry| Arc::clone(&entry.source))
            .collect()
    }

    pub fn js_source_names(&self) -> Vec<String> {
        self.js_sources
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .sources
            .iter()
            .map(|entry| entry.source.name().to_string())
            .collect()
    }

    pub fn js_source_name(&self, index: usize) -> Option<String> {
        self.js_sources
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .sources
            .get(index)
            .map(|entry| entry.source.name().to_string())
    }

    pub fn js_source_name_for_origin(&self, origin: &str) -> Option<String> {
        self.js_sources
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .sources
            .iter()
            .find(|entry| entry.origin == origin)
            .map(|entry| entry.source.name().to_string())
    }

    pub fn update_source_preferences(&self, default: SourceId, enabled: &[SourceId]) {
        let enabled: HashSet<_> = enabled.iter().copied().collect();
        // 校验对象是**本构建已注册**的源：配置里点名了一个本次构建没有的
        // 源（例如 --no-default-features 下的 joox/qianqian——它们的实现
        // 随 default 特性树走）时，绝不能把不可达的源装进 default——否则
        // `default_source()` 的注册断言会在首次 search 时 panic（优化轮
        // P1-2，审查账本 §16）。最终兜底 Local（manager::new 恒注册）。
        let effective_default = if enabled.contains(&default) && self.sources.contains_key(&default)
        {
            default
        } else {
            SourceId::all_online()
                .iter()
                .copied()
                .find(|source| enabled.contains(source) && self.sources.contains_key(source))
                .unwrap_or(SourceId::Local)
        };
        *self.enabled.write().unwrap_or_else(|e| e.into_inner()) = enabled;
        *self.default.write().unwrap_or_else(|e| e.into_inner()) = effective_default;
    }

    pub fn enabled_sources(&self) -> Vec<SourceId> {
        let enabled = self.enabled.read().unwrap_or_else(|e| e.into_inner());
        SourceId::all_online()
            .iter()
            .copied()
            .filter(|source| enabled.contains(source))
            .collect()
    }

    /// 查询音源声明的能力；未注册的音源按「全部不支持」处理。
    pub fn capabilities(&self, source: SourceId) -> SourceCapabilities {
        self.sources
            .get(&source)
            .map(|source| source.capabilities())
            .unwrap_or_default()
    }

    /// 解析一个分享链接，返回(音源, 解析结果)。
    ///
    /// 域名识别失败或音源尚未实现链接直解时返回可展示的错误信息，
    /// 由界面提示用户改用关键词搜索。
    pub async fn parse_link(&self, link: &str) -> Result<(SourceId, ParsedLink), String> {
        let source_id = SourceId::detect_from_link(link)
            .ok_or_else(|| "无法识别该链接的音源，请改用关键词搜索".to_string())?;
        let source = self
            .sources
            .get(&source_id)
            .ok_or_else(|| format!("{} 音源尚未接入", source_id.display_name()))?;
        if !source.capabilities().link_parse {
            return Err(format!(
                "{} 暂不支持链接直解，请改用关键词搜索",
                source_id.display_name()
            ));
        }
        let parsed = source
            .parse_link(link)
            .await
            .map_err(|error| format!("{} 链接解析失败: {error}", source_id.display_name()))?;
        Ok((source_id, parsed))
    }

    /// 当前启用且满足能力条件的音源，界面据此决定入口显隐。
    pub fn sources_supporting(
        &self,
        supported: impl Fn(&SourceCapabilities) -> bool,
    ) -> Vec<SourceId> {
        self.enabled_sources()
            .into_iter()
            .filter(|source| supported(&self.capabilities(*source)))
            .collect()
    }

    /// 对当前启用的内置及 JS 音源执行轻量搜索检测。
    pub async fn health_check(&self) -> Vec<SourceHealth> {
        let enabled = self.enabled_sources();
        let mut tasks = tokio::task::JoinSet::new();
        for source_id in enabled {
            if let Some(source) = self.sources.get(&source_id).map(Arc::clone) {
                tasks.spawn(check_source(source_id, source));
            }
        }
        for source in self.js_sources() {
            tasks.spawn(check_source(SourceId::Local, source));
        }
        let mut results = Vec::new();
        while let Some(result) = tasks.join_next().await {
            if let Ok(result) = result {
                results.push(result);
            }
        }
        results.sort_by(|a, b| {
            a.id.as_str()
                .cmp(b.id.as_str())
                .then_with(|| a.name.cmp(&b.name))
        });
        results
    }

    pub fn get(&self, id: SourceId) -> Option<Arc<dyn MusicSource>> {
        self.sources.get(&id).map(Arc::clone)
    }

    /// 获取本地音乐源（可直接调用 scan 等特有方法）
    pub fn local_source(&self) -> Arc<LocalSource> {
        Arc::clone(&self.local_source)
    }

    /// 获取哔哩哔哩音源（可直接调用登录等特有方法）
    pub fn bili_source(&self) -> Arc<BiliSource> {
        Arc::clone(&self.bili_source)
    }

    pub fn default_source(&self) -> Arc<dyn MusicSource> {
        let default = *self.default.read().unwrap_or_else(|e| e.into_inner());
        self.sources
            .get(&default)
            .map(Arc::clone)
            .expect("default source must be registered")
    }

    pub async fn search(
        &self,
        keyword: &str,
        page: u32,
        limit: u32,
    ) -> Result<SearchResult, SearchError> {
        let default = *self.default.read().unwrap_or_else(|e| e.into_inner());
        if !self
            .enabled
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&default)
        {
            return Err(SearchError::Other(format!(
                "默认音源 {} 未启用",
                default.as_str()
            )));
        }
        self.default_source().search(keyword, page, limit).await
    }

    pub async fn search_scoped(
        &self,
        keyword: &str,
        page: u32,
        limit: u32,
        source: Option<SourceId>,
    ) -> Result<SearchResult, SearchError> {
        if crate::bili::looks_like_video_reference(keyword) {
            if !self
                .enabled
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&SourceId::Bili)
            {
                return Err(SearchError::Other("哔哩哔哩音源未启用".to_string()));
            }
            return self
                .sources
                .get(&SourceId::Bili)
                .map(Arc::clone)
                .ok_or_else(|| SearchError::Other("哔哩哔哩音源不可用".to_string()))?
                .search(keyword, page, limit)
                .await;
        }
        let Some(source) = source else {
            return self.search_all(keyword, page, limit).await;
        };
        if source != SourceId::Local
            && !self
                .enabled
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&source)
        {
            return Err(SearchError::Other(format!(
                "音源 {} 未启用",
                source.as_str()
            )));
        }
        // V1 双栈搜索面（musid-dli-adapter，opt-in）：musid 为替换路径，
        // 失败立即按旧路径重试并计 fallback；probe 模式真双跑、恒展示旧
        // 路径结果（docs/MUSID_DLI_ADAPTER.md §11）。
        #[cfg(feature = "musid-dli-adapter")]
        if let Some(outcome) = self.musid_search_single(keyword, page, limit, source).await {
            return outcome;
        }
        let source = self
            .sources
            .get(&source)
            .map(Arc::clone)
            .ok_or_else(|| SearchError::Other("音源不可用".to_string()))?;
        source.search(keyword, page, limit).await
    }

    /// 单源搜索的双栈路径（feature `musid-dli-adapter`）。
    ///
    /// 返回 `None` = 未路由（调用方继续旧路径）；`Some(result)` = musid
    /// 路径的最终结果（替换模式含失败回落旧路径后的结果；probe 模式恒为
    /// 旧路径结果）。替换模式：musid 尝试预算 [`MUSID_ATTEMPT_BUDGET`]，
    /// Err/预算耗尽后立即按旧路径重试该源（回退不被 musid 慢失败挤占，
    /// 评审 P2；整体仍受 app 外层 12s 约束）。probe 模式（纯观测）：旧路径
    /// 权威，musid 影子在后台任务对账（oneshot 交接旧路径产物，评审 P4
    /// ——用户路径零额外延迟），计数最终一致。
    #[cfg(feature = "musid-dli-adapter")]
    async fn musid_search_single(
        &self,
        keyword: &str,
        page: u32,
        limit: u32,
        source: SourceId,
    ) -> Option<Result<SearchResult, SearchError>> {
        let search_flag = self
            .musid_routes
            .search
            .load(std::sync::atomic::Ordering::Relaxed);
        let probe = self
            .musid_routes
            .parity_probe
            .load(std::sync::atomic::Ordering::Relaxed);
        if !search_flag && !probe {
            return None;
        }
        let musid = self.musid_route(
            search_flag || probe,
            crate::parity::ParityFace::Search,
            source,
        )?;
        if probe {
            // 对账影子模式（纯观测）：旧路径权威，musid 影子任务后台真双跑
            // （限 10s，与聚合每源预算同口径）；旧路径产物经 oneshot 交接给
            // 影子任务对照计数。计数：双 Ok 才对照（ParityMatch/Mismatch），
            // 单侧瞬时 Err 不计，双 Err 记 MusidErrLegacyErr。
            let legacy = self.sources.get(&source).map(Arc::clone);
            let (verdict_tx, verdict_rx) = tokio::sync::oneshot::channel();
            let parity = Arc::clone(&self.parity);
            let shadow_keyword = keyword.to_string();
            tokio::spawn(async move {
                let shadow = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    musid.search(&shadow_keyword, page, limit),
                )
                .await
                .map_err(|_| SearchError::Other("musid 对账搜索超时".to_string()))
                .and_then(|inner| inner);
                let legacy_items: Result<Vec<SongInfo>, ()> = match verdict_rx.await {
                    Ok(verdict) => verdict,
                    // 主路径异常结束（发送端被弃）：不作对账结论。
                    Err(_) => return,
                };
                let outcome = match (&legacy_items, &shadow) {
                    (Err(()), Err(_)) => Some(crate::parity::ParityOutcome::MusidErrLegacyErr),
                    (Err(()), _) | (_, Err(_)) => None,
                    (Ok(legacy), Ok(shadow)) => {
                        let matched = crate::parity::compare_search_items(legacy, &shadow.items);
                        Some(if matched {
                            crate::parity::ParityOutcome::ParityMatch
                        } else {
                            crate::parity::ParityOutcome::ParityMismatch
                        })
                    }
                };
                if let Some(outcome) = outcome {
                    parity.record(crate::parity::ParityFace::Search, source, outcome);
                }
            });
            let legacy_result = match legacy {
                Some(legacy) => legacy.search(keyword, page, limit).await,
                None => Err(SearchError::Other("音源不可用".to_string())),
            };
            let _ = verdict_tx.send(
                legacy_result
                    .as_ref()
                    .map(|result| result.items.clone())
                    .map_err(|_| ()),
            );
            return Some(legacy_result);
        }
        // 替换模式：musid 先行（尝试预算 MUSID_ATTEMPT_BUDGET）；Err/预算
        // 耗尽立即按旧路径重试该源并计 fallback——回退不被 musid 慢失败
        // 挤占（评审 P2）。
        let attempt =
            tokio::time::timeout(MUSID_ATTEMPT_BUDGET, musid.search(keyword, page, limit)).await;
        match attempt {
            Ok(Ok(result)) => {
                self.musid_record(
                    crate::parity::ParityFace::Search,
                    source,
                    crate::parity::ParityOutcome::MusidOk,
                );
                Some(Ok(result))
            }
            Ok(Err(_)) | Err(_) => {
                let legacy = self.sources.get(&source).map(Arc::clone);
                let fallback = match legacy {
                    Some(legacy) => legacy.search(keyword, page, limit).await,
                    None => Err(SearchError::Other("音源不可用".to_string())),
                };
                let outcome = if fallback.is_ok() {
                    crate::parity::ParityOutcome::MusidErrLegacyOk
                } else {
                    crate::parity::ParityOutcome::MusidErrLegacyErr
                };
                self.musid_record(crate::parity::ParityFace::Search, source, outcome);
                Some(fallback)
            }
        }
    }

    pub async fn search_all(
        &self,
        keyword: &str,
        page: u32,
        limit: u32,
    ) -> Result<SearchResult, SearchError> {
        let per_source_limit = (limit / 2).max(10);
        let mut tasks = tokio::task::JoinSet::new();
        // V1 双栈聚合（musid-dli-adapter，opt-in）：probe 影子任务走第二个
        // JoinSet，与旧路径任务并行（真双跑），产物不进聚合结果。
        #[cfg(feature = "musid-dli-adapter")]
        let mut musid_shadows = tokio::task::JoinSet::new();
        let enabled = self
            .enabled
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for source_id in SourceId::all_online() {
            if !enabled.contains(source_id) {
                continue;
            }
            // musid 路由进同一 JoinSet（同享每源 10s 超时与同一去重键）；
            // probe 模式只 spawn 影子任务、旧路径任务照常 spawn。
            #[cfg(feature = "musid-dli-adapter")]
            if self.musid_search_all_spawn(
                &mut tasks,
                &mut musid_shadows,
                keyword,
                page,
                per_source_limit,
                *source_id,
            ) {
                continue;
            }
            if let Some(source) = self.sources.get(source_id) {
                let source = Arc::clone(source);
                let keyword = keyword.to_string();
                tasks.spawn(async move {
                    (
                        *source_id,
                        tokio::time::timeout(
                            std::time::Duration::from_secs(10),
                            source.search(&keyword, page, per_source_limit),
                        )
                        .await,
                    )
                });
            }
        }
        for source in self.js_sources() {
            let keyword = keyword.to_string();
            tasks.spawn(async move {
                (
                    SourceId::Local,
                    tokio::time::timeout(
                        std::time::Duration::from_secs(10),
                        source.search(&keyword, page, per_source_limit),
                    )
                    .await,
                )
            });
        }

        // probe 影子对照所需的旧路径逐源产物（仅 feature 构建读取）。
        #[cfg(feature = "musid-dli-adapter")]
        let mut legacy_probe_items: HashMap<SourceId, Vec<SongInfo>> = HashMap::new();
        let mut items = Vec::new();
        let mut total = 0u32;
        let mut has_more = false;
        let mut success_count = 0usize;
        let mut errors = Vec::new();
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok((_source_id, Ok(Ok(result)))) => {
                    #[cfg(feature = "musid-dli-adapter")]
                    let source_id = _source_id;
                    success_count += 1;
                    total = total.saturating_add(result.total);
                    has_more |= result.has_more;
                    #[cfg(feature = "musid-dli-adapter")]
                    if self.musid_probe_active() {
                        legacy_probe_items.insert(source_id, result.items.clone());
                    }
                    items.extend(result.items);
                }
                Ok((_, Ok(Err(error)))) => errors.push(error.to_string()),
                Ok((_, Err(_))) => errors.push("请求超时".to_string()),
                Err(error) => errors.push(error.to_string()),
            }
        }

        // probe 影子对照：musid 有产物且旧路径该源也有产物时按 §11 归一
        // 规则对照计数（旧路径失败的源不对照——无双方产物）。对账在后台
        // 任务落账（评审 P4）：聚合不被最慢的影子拖住（最多 10s），计数
        // 最终一致。
        #[cfg(feature = "musid-dli-adapter")]
        if self.musid_probe_active() {
            Self::spawn_search_shadow_reconcile(
                musid_shadows,
                legacy_probe_items,
                Arc::clone(&self.parity),
            );
        }

        if success_count == 0 {
            return Err(SearchError::Other(format!(
                "所有音源搜索失败: {}",
                errors.join("; ")
            )));
        }

        let mut seen = HashSet::new();
        // JS 音源的搜索结果统一标记为 Local，去重键要带上 extra["source"]
        // 平台标记，避免不同平台/不同 JS 音源的同数字 id 互相吞掉。
        items.retain(|song| {
            seen.insert((
                song.source,
                song.extra.get("source").cloned().unwrap_or_default(),
                song.id.clone(),
            ))
        });
        Self::sort_search_results(&mut items, keyword);
        Ok(SearchResult {
            items,
            total,
            has_more,
        })
    }

    /// 聚合搜索的按源双栈路由（feature `musid-dli-adapter`）。
    ///
    /// 返回 `true` = 本源已由 musid 替换任务处理（旧路径 spawn 跳过）；
    /// `false` = 旧路径任务照常 spawn（未路由，或 probe 影子模式——影子
    /// 任务已另行 spawn 到 `shadows`，旧路径仍是结果来源）。
    #[cfg(feature = "musid-dli-adapter")]
    fn musid_search_all_spawn(
        &self,
        tasks: &mut tokio::task::JoinSet<AggregateTaskOutput>,
        shadows: &mut tokio::task::JoinSet<AggregateTaskOutput>,
        keyword: &str,
        page: u32,
        per_source_limit: u32,
        source_id: SourceId,
    ) -> bool {
        let search_flag = self
            .musid_routes
            .search
            .load(std::sync::atomic::Ordering::Relaxed);
        let probe = self
            .musid_routes
            .parity_probe
            .load(std::sync::atomic::Ordering::Relaxed);
        if !search_flag && !probe {
            return false;
        }
        let Some(musid) = self.musid_route(
            search_flag || probe,
            crate::parity::ParityFace::Search,
            source_id,
        ) else {
            return false;
        };
        let parity = Arc::clone(&self.parity);
        if probe {
            // 对账影子模式：musid 作为额外并行 provider 参与同一次聚合
            // （真双跑），产物不进结果，仅供对照计数（恒展示旧路径结果）。
            let keyword = keyword.to_string();
            shadows.spawn(async move {
                (
                    source_id,
                    tokio::time::timeout(
                        std::time::Duration::from_secs(10),
                        musid.search(&keyword, page, per_source_limit),
                    )
                    .await,
                )
            });
            return false;
        }
        // 替换模式：musid 先行（尝试预算 MUSID_ATTEMPT_BUDGET = 每源预算
        // 前半，评审 P2），Err/预算耗尽立即按旧路径重试该源并计 fallback
        // ——回退继承剩余预算，不被 musid 慢失败/挂死挤占；仅整体外层超时
        // （回退也超时）不计（无双方结局）。
        let legacy = self.sources.get(&source_id).map(Arc::clone);
        let keyword = keyword.to_string();
        tasks.spawn(async move {
            (
                source_id,
                tokio::time::timeout(std::time::Duration::from_secs(10), async move {
                    let attempt = tokio::time::timeout(
                        MUSID_ATTEMPT_BUDGET,
                        musid.search(&keyword, page, per_source_limit),
                    )
                    .await;
                    match attempt {
                        Ok(Ok(result)) => {
                            parity.record(
                                crate::parity::ParityFace::Search,
                                source_id,
                                crate::parity::ParityOutcome::MusidOk,
                            );
                            Ok(result)
                        }
                        Ok(Err(_)) | Err(_) => {
                            let fallback = match legacy {
                                Some(legacy) => {
                                    legacy.search(&keyword, page, per_source_limit).await
                                }
                                None => Err(SearchError::Other("音源不可用".to_string())),
                            };
                            let outcome = if fallback.is_ok() {
                                crate::parity::ParityOutcome::MusidErrLegacyOk
                            } else {
                                crate::parity::ParityOutcome::MusidErrLegacyErr
                            };
                            parity.record(crate::parity::ParityFace::Search, source_id, outcome);
                            fallback
                        }
                    }
                })
                .await,
            )
        });
        true
    }

    /// 聚合搜索 probe 影子的后台对账（评审 P4：fire-and-forget，不阻塞聚合
    /// 返回，计数最终一致）：musid 有产物且旧路径同源也有产物时按 §11 归一
    /// 规则对照（ParityMatch/ParityMismatch）；旧路径失败的源不对照（无双方
    /// 产物可归一）；影子 Err/超时同样不计。
    #[cfg(feature = "musid-dli-adapter")]
    fn spawn_search_shadow_reconcile(
        mut shadows: tokio::task::JoinSet<AggregateTaskOutput>,
        legacy_probe_items: HashMap<SourceId, Vec<SongInfo>>,
        parity: Arc<crate::parity::ParityCounters>,
    ) {
        tokio::spawn(async move {
            while let Some(joined) = shadows.join_next().await {
                let Ok((source, Ok(Ok(page)))) = joined else {
                    continue;
                };
                let Some(legacy) = legacy_probe_items.get(&source) else {
                    continue;
                };
                let matched = crate::parity::compare_search_items(legacy, &page.items);
                let outcome = if matched {
                    crate::parity::ParityOutcome::ParityMatch
                } else {
                    crate::parity::ParityOutcome::ParityMismatch
                };
                parity.record(crate::parity::ParityFace::Search, source, outcome);
            }
        });
    }

    fn sort_search_results(items: &mut [SongInfo], keyword: &str) {
        let keyword = Self::normalize_search_text(keyword);
        // 优化轮 P2-3：装饰-排序-去装饰——每项的归一化键（name/singer/album）
        // 与相关性只在排序前算一次（原实现每个比较重算 ~10 次全字符串扫描）。
        let mut decorated: Vec<_> = items
            .iter()
            .enumerate()
            .map(|(index, song)| {
                (
                    index,
                    Self::search_relevance(song, &keyword),
                    Self::normalize_search_text(&song.name),
                    Self::normalize_search_text(&song.singer),
                )
            })
            .collect();
        decorated.sort_by(|a, b| {
            a.1.cmp(&b.1)
                .then_with(|| a.2.cmp(&b.2))
                .then_with(|| a.3.cmp(&b.3))
                .then_with(|| items[a.0].source.as_str().cmp(items[b.0].source.as_str()))
                .then_with(|| items[a.0].id.cmp(&items[b.0].id))
        });
        let original: Vec<_> = items.to_vec();
        for (position, (index, _, _, _)) in decorated.iter().enumerate() {
            items[position] = original[*index].clone();
        }
    }

    fn search_relevance(song: &SongInfo, keyword: &str) -> u8 {
        if keyword.is_empty() {
            return 5;
        }
        let name = Self::normalize_search_text(&song.name);
        let singer = Self::normalize_search_text(&song.singer);
        let album = Self::normalize_search_text(&song.album_name);
        if name == keyword {
            0
        } else if name.starts_with(keyword) {
            1
        } else if singer == keyword {
            2
        } else if name.contains(keyword) {
            3
        } else if singer.contains(keyword) {
            4
        } else if album.contains(keyword) {
            5
        } else {
            6
        }
    }

    fn normalize_search_text(value: &str) -> String {
        value
            .chars()
            .filter(|character| character.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    }

    pub async fn leaderboard(
        &self,
        source: SourceId,
        board_id: &str,
        page: u32,
        limit: u32,
    ) -> Result<SearchResult, SearchError> {
        self.online_source(source)?
            .get_leaderboard(board_id, page, limit)
            .await
    }

    pub async fn leaderboard_boards(
        &self,
        source: SourceId,
    ) -> Result<Vec<LeaderboardInfo>, SearchError> {
        self.online_source(source)?.get_leaderboard_boards().await
    }

    pub fn leaderboard_sources(&self) -> Vec<SourceId> {
        let enabled = self.enabled.read().unwrap_or_else(|e| e.into_inner());
        SourceId::all_online()
            .iter()
            .copied()
            .filter(|source| {
                enabled.contains(source)
                    && self.sources.contains_key(source)
                    && self.capabilities(*source).leaderboard
            })
            .collect()
    }

    /// 指定分类下的歌单；`category` 为空表示「全部/热门」。
    pub async fn playlists(
        &self,
        source: SourceId,
        category: &str,
        page: u32,
    ) -> Result<Vec<Playlist>, FetchError> {
        self.online_source_fetch(source)?
            .get_playlists(category, page)
            .await
    }

    /// 歌单分类目录；不支持分类的音源返回空列表。
    pub async fn playlist_categories(
        &self,
        source: SourceId,
    ) -> Result<Vec<PlaylistCategory>, FetchError> {
        self.online_source_fetch(source)?
            .get_playlist_categories()
            .await
    }

    /// 账号下的个人歌单（需要登录）。
    pub async fn user_playlists(
        &self,
        source: SourceId,
        page: u32,
        limit: u32,
    ) -> Result<Vec<Playlist>, FetchError> {
        self.online_source_fetch(source)?
            .get_user_playlists(page, limit)
            .await
    }

    /// 生成扫码登录会话。
    pub async fn create_qr_login(
        &self,
        source: SourceId,
    ) -> Result<lx_core::model::login::QrLoginSession, FetchError> {
        self.online_source_fetch(source)?.create_qr_login().await
    }

    /// 按渠道生成扫码登录会话（QQ 音乐的微信入口用）。
    pub async fn create_qr_login_kind(
        &self,
        source: SourceId,
        kind: lx_core::model::login::QrLoginKind,
    ) -> Result<lx_core::model::login::QrLoginSession, FetchError> {
        self.online_source_fetch(source)?
            .create_qr_login_kind(kind)
            .await
    }

    /// 轮询扫码状态。
    pub async fn check_qr_login(
        &self,
        source: SourceId,
        key: &str,
    ) -> Result<lx_core::model::login::QrLoginResult, FetchError> {
        self.online_source_fetch(source)?.check_qr_login(key).await
    }

    /// 音源是否已登录。
    pub fn is_logged_in(&self, source: SourceId) -> bool {
        self.sources
            .get(&source)
            .is_some_and(|source| source.is_logged_in())
    }

    /// 退出登录并清除本地 cookie。
    pub fn logout(&self, source: SourceId) -> Result<(), String> {
        self.sources
            .get(&source)
            .ok_or_else(|| format!("{} 音源尚未接入", source.display_name()))?
            .logout()
            .map_err(|error| error.to_string())
    }

    pub async fn search_playlists(
        &self,
        source: SourceId,
        keyword: &str,
        page: u32,
    ) -> Result<Vec<Playlist>, SearchError> {
        self.online_source(source)?
            .search_playlists(keyword, page)
            .await
    }

    pub async fn playlist_detail(
        &self,
        source: SourceId,
        playlist_id: &str,
        page: u32,
    ) -> Result<Vec<SongInfo>, FetchError> {
        self.online_source_fetch(source)?
            .get_playlist_detail(playlist_id, page)
            .await
    }

    pub async fn artist_songs(
        &self,
        artist: &Artist,
        page: u32,
        limit: u32,
    ) -> Result<SearchResult, SearchError> {
        self.online_source(artist.source)?
            .get_artist_songs(artist, page, limit)
            .await
    }

    pub async fn artist_albums(
        &self,
        artist: &Artist,
        page: u32,
        limit: u32,
    ) -> Result<Vec<Album>, SearchError> {
        self.online_source(artist.source)?
            .get_artist_albums(artist, page, limit)
            .await
    }

    pub async fn album_songs(
        &self,
        album: &Album,
        page: u32,
        limit: u32,
    ) -> Result<SearchResult, SearchError> {
        self.online_source(album.source)?
            .get_album_songs(album, page, limit)
            .await
    }

    pub fn playlist_sources(&self) -> Vec<SourceId> {
        let enabled = self.enabled.read().unwrap_or_else(|e| e.into_inner());
        SourceId::all_online()
            .iter()
            .copied()
            .filter(|source| {
                enabled.contains(source)
                    && self.sources.contains_key(source)
                    && self.capabilities(*source).playlists
            })
            .collect()
    }

    fn online_source(&self, source: SourceId) -> Result<Arc<dyn MusicSource>, SearchError> {
        if source == SourceId::Local
            || !self
                .enabled
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&source)
        {
            return Err(SearchError::Other(format!(
                "音源 {} 未启用",
                source.as_str()
            )));
        }
        self.sources
            .get(&source)
            .map(Arc::clone)
            .ok_or_else(|| SearchError::Other("音源不可用".to_string()))
    }

    fn online_source_fetch(&self, source: SourceId) -> Result<Arc<dyn MusicSource>, FetchError> {
        if source == SourceId::Local
            || !self
                .enabled
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&source)
        {
            return Err(FetchError::Other(format!(
                "音源 {} 未启用",
                source.as_str()
            )));
        }
        self.sources
            .get(&source)
            .map(Arc::clone)
            .ok_or_else(|| FetchError::Other("音源不可用".to_string()))
    }

    /// 获取歌曲播放地址。
    /// 默认先用歌曲所属平台的内置音源，失败后依次尝试 JS 音源；
    /// `js_url_first` 开启时恢复旧的「JS 音源优先」顺序。
    pub async fn get_song_url(
        &self,
        song: &SongInfo,
        quality: Quality,
    ) -> Result<SongUrl, FetchError> {
        self.get_song_url_from_js_index(song, quality, 0)
            .await
            .map(|(url, _)| url)
    }

    /// 「JS 音源优先」开关（默认关闭：内置音源先行，JS 作为回退换源）。
    ///
    /// 单个挂死的 JS 音源曾让酷我、网易云这些本可直接解析的歌一起变慢；
    /// 依赖 JS 音源解 VIP 曲目的用户可以用这个开关恢复旧行为。
    pub fn set_js_url_first(&self, js_first: bool) {
        self.js_url_first
            .store(js_first, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn js_url_first(&self) -> bool {
        self.js_url_first.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 从指定 JS 音源索引开始解析播放地址。
    ///
    /// 返回成功提供地址的 JS 音源索引；`None` 表示使用了本地、B 站或内置音源。
    /// mpv 实际播放失败后可从下一个索引继续，避免重复使用同一个失效链接。
    pub async fn get_song_url_from_js_index(
        &self,
        song: &SongInfo,
        quality: Quality,
        js_start_index: usize,
    ) -> Result<(SongUrl, Option<usize>), FetchError> {
        // JS 音源引擎本身有较长超时，串行尝试多个音源时最坏耗时会被放大，
        // 这里对整个解析过程加一层外层超时兜底。
        match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.get_song_url_inner(song, quality, js_start_index),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(FetchError::Network("解析播放地址超时".to_string())),
        }
    }

    async fn get_song_url_inner(
        &self,
        song: &SongInfo,
        quality: Quality,
        js_start_index: usize,
    ) -> Result<(SongUrl, Option<usize>), FetchError> {
        // 真本地文件歌曲（非 JS 搜索结果）走本地音源
        if is_local_file_song(song) {
            if let Some(local_src) = self.sources.get(&SourceId::Local) {
                return local_src
                    .get_song_url(song, quality)
                    .await
                    .map(|url| (url, None));
            }
            return Err(FetchError::Other("本地音源不可用".to_string()));
        }
        if song.source == SourceId::Bili {
            // V2 切片 4（docs/MUSID_DLI_ADAPTER.md §12.4 行 4）：bili 的
            // musid 流地址槽位在旧早退臂**之前**（注册表 bili 行 SongUrl=
            // Migrated 放行时；js_first 对本臂无意义——bili 歌从不参与 JS
            // 链）。musid Err/未路由/预算耗尽 → 旧早退臂照旧（独立存储 +
            // 扫码登录链路的旧实现逐字保持）；簿记三件套与主链同款
            // （MusidOk 在 try 内、legacy-ok 在旧链成功、err 侧在本臂错误
            // 出口）。
            #[cfg(feature = "musid-dli-adapter")]
            {
                let mut musid_url = self.musid_url_slot_route(song);
                if let Some(url) = self.musid_url_try(&mut musid_url, song, quality).await {
                    return Ok((url, None));
                }
                let legacy = self
                    .sources
                    .get(&SourceId::Bili)
                    .ok_or_else(|| FetchError::Other("哔哩哔哩音源不可用".to_string()))?
                    .get_song_url(song, quality)
                    .await;
                return match legacy {
                    Ok(url) => {
                        self.musid_url_record_legacy_ok(&musid_url);
                        Ok((url, None))
                    }
                    Err(error) => {
                        self.musid_url_err_exit(&mut musid_url, song, quality).await;
                        Err(error)
                    }
                };
            }
            #[cfg(not(feature = "musid-dli-adapter"))]
            {
                return self
                    .sources
                    .get(&SourceId::Bili)
                    .ok_or_else(|| FetchError::Other("哔哩哔哩音源不可用".to_string()))?
                    .get_song_url(song, quality)
                    .await
                    .map(|url| (url, None));
            }
        }

        // js_start_index > 0 说明上一轮已经解析过一次（播放失败后从下一个
        // JS 音源继续）：这一轮直接从 JS 扫起，不再重复请求内置音源。
        let js_first = self.js_url_first() || js_start_index > 0;
        let mut builtin_error: Option<String> = None;
        let mut builtin_attempted = false;
        let mut js_errors = Vec::new();

        // V1 双栈流地址槽位（musid-dli-adapter，opt-in）：本地/bili 早退后、
        // 内置槽位前插 musid 判定；js_first 语义不变——true=JS 先于 musid
        // （musid 槽位落到 JS 循环之后、内置兜底之前）。musid Err → 继续
        // 既有内置→JS→内置兜底链；过期地址在适配源内已视为失败（G4）。
        #[cfg(feature = "musid-dli-adapter")]
        let mut musid_url = self.musid_url_slot_route(song);
        #[cfg(feature = "musid-dli-adapter")]
        if !js_first && let Some(url) = self.musid_url_try(&mut musid_url, song, quality).await {
            return Ok((url, None));
        }

        // 默认内置音源先行：单个挂死的 JS 音源不再拖慢本可直接播放的歌。
        if !js_first && let Some(builtin) = self.builtin_fallback_source(song) {
            builtin_attempted = true;
            match builtin.get_song_url(song, quality).await {
                Ok(result) => {
                    #[cfg(feature = "musid-dli-adapter")]
                    self.musid_url_record_legacy_ok(&musid_url);
                    return Ok((result, None));
                }
                Err(error) => builtin_error = Some(error.to_string()),
            }
        }

        for (index, js_source) in self
            .js_sources()
            .into_iter()
            .enumerate()
            .skip(js_start_index)
        {
            match js_source.get_song_url(song, quality).await {
                Ok(result) => {
                    #[cfg(feature = "musid-dli-adapter")]
                    self.musid_url_record_legacy_ok(&musid_url);
                    return Ok((result, Some(index)));
                }
                Err(error) => js_errors.push(error.to_string()),
            }
        }

        // V1 双栈：js_first（或续播轮次）时 musid 槽位在 JS 之后、内置兜底
        // 之前（js_start_index 续播机制零改动）。
        #[cfg(feature = "musid-dli-adapter")]
        if js_first && let Some(url) = self.musid_url_try(&mut musid_url, song, quality).await {
            return Ok((url, None));
        }

        // 错误汇总：JS 音源实际尝试过才提 JS；JS 搜到的歌一个源都没有时
        // 保留原来的「未加载任何 JS 音源」提示。
        let js_note = (!js_errors.is_empty())
            .then(|| format!("JS 音源失败: {}", js_errors.join("; ")))
            .or_else(|| (song.source == SourceId::Local).then(|| "未加载任何 JS 音源".to_string()));

        // V1 双栈：错误路径收敛到统一出口（probe 影子模式在旧链失败后才跑
        // musid 兜底并计数；替换模式在此计 both_err）。旧路径各臂的错误文本
        // 与原实现逐字一致。
        let final_error: FetchError;
        // 全部失败后内置音源兜底（内置音源先行模式下上面已经试过，不再重复）。
        if let Some(source) = self
            .builtin_fallback_source(song)
            .filter(|_| !builtin_attempted)
        {
            match source.get_song_url(song, quality).await {
                Ok(result) => {
                    #[cfg(feature = "musid-dli-adapter")]
                    self.musid_url_record_legacy_ok(&musid_url);
                    return Ok((result, None));
                }
                Err(error) => {
                    final_error = match js_note {
                        Some(note) => FetchError::Other(format!(
                            "{note}; 内置{}音源失败: {error}",
                            source.name()
                        )),
                        None => error,
                    };
                }
            }
        } else if !builtin_attempted {
            final_error =
                FetchError::Other(js_note.unwrap_or_else(|| "歌曲来源不可用".to_string()));
        } else {
            final_error = match (builtin_error, js_note) {
                (Some(builtin_error), Some(js_note)) => {
                    FetchError::Other(format!("{js_note}; 内置音源失败: {builtin_error}"))
                }
                (Some(builtin_error), None) => FetchError::Other(builtin_error),
                // builtin_attempted 为 true 时必然已经失败返回，这里只是兜底。
                (None, js_note) => {
                    FetchError::Other(js_note.unwrap_or_else(|| "歌曲来源不可用".to_string()))
                }
            };
        }

        // V1 双栈错误出口（评审 P1：probe 纯观测——旧链失败才跑 musid 仅
        // 计数，musid 产物不进用户路径；替换模式 musid 已失败过 → 计
        // both_err）。未路由/未命中时原样返回旧错误。
        #[cfg(feature = "musid-dli-adapter")]
        self.musid_url_err_exit(&mut musid_url, song, quality).await;
        Err(final_error)
    }

    /// V1 双栈流地址槽位状态（feature `musid-dli-adapter`）。
    #[cfg(feature = "musid-dli-adapter")]
    fn musid_url_slot_route(&self, song: &SongInfo) -> MusidUrlSlot {
        let none = MusidUrlSlot {
            musid: None,
            source: song.source,
            probe: false,
            attempted: false,
        };
        let Some(source) = Self::musid_effective_source(song) else {
            return none;
        };
        let probe = self
            .musid_routes
            .parity_probe
            .load(std::sync::atomic::Ordering::Relaxed);
        let face = self
            .musid_routes
            .song_url
            .load(std::sync::atomic::Ordering::Relaxed);
        if !face && !probe {
            return none;
        }
        let Some(musid) =
            self.musid_route(face || probe, crate::parity::ParityFace::SongUrl, source)
        else {
            return none;
        };
        MusidUrlSlot {
            musid: Some(musid),
            source,
            probe,
            attempted: false,
        }
    }

    /// 替换模式的 musid resolve 尝试：成功（未过期地址）记 MusidOk 并返回；
    /// Err/预算耗尽（[`MUSID_RESOLVE_BUDGET`]，评审 P3——musid 内部固 15s
    /// 不可注入，预算在宿主侧收口，不挤占外层 30s 内的回退链）返回 None
    /// 让调用方继续旧链（attempted 置位供 err 侧归因）。probe 模式不抢跑
    /// （返回 None）。
    #[cfg(feature = "musid-dli-adapter")]
    async fn musid_url_try(
        &self,
        slot: &mut MusidUrlSlot,
        song: &SongInfo,
        quality: Quality,
    ) -> Option<SongUrl> {
        if slot.probe {
            return None;
        }
        let musid = slot.musid.as_ref()?;
        slot.attempted = true;
        let attempt =
            tokio::time::timeout(MUSID_RESOLVE_BUDGET, musid.get_song_url(song, quality)).await;
        match attempt {
            Ok(Ok(url)) => {
                self.musid_record(
                    crate::parity::ParityFace::SongUrl,
                    slot.source,
                    crate::parity::ParityOutcome::MusidOk,
                );
                Some(url)
            }
            Ok(Err(error)) => {
                tracing::debug!(
                    target: "musid_adapter",
                    source = slot.source.as_str(),
                    error = %error,
                    "musid resolve 失败，回落旧链"
                );
                None
            }
            Err(_) => {
                tracing::debug!(
                    target: "musid_adapter",
                    source = slot.source.as_str(),
                    budget_secs = MUSID_RESOLVE_BUDGET.as_secs(),
                    "musid resolve 预算耗尽，回落旧链"
                );
                None
            }
        }
    }

    /// musid 已尝试且未成功、旧路径同请求成功 → MusidErrLegacyOk。
    #[cfg(feature = "musid-dli-adapter")]
    fn musid_url_record_legacy_ok(&self, slot: &MusidUrlSlot) {
        if slot.attempted && !slot.probe {
            self.musid_record(
                crate::parity::ParityFace::SongUrl,
                slot.source,
                crate::parity::ParityOutcome::MusidErrLegacyOk,
            );
        }
    }

    /// 统一错误出口（评审 P1 修订：probe 恒纯观测；评审第二轮 F1：对账
    /// 后台化）。probe 模式旧链失败才跑 musid（失败侧对账，legacy 成功不
    /// 双跑 resolve 避免双倍平台配额，尝试限 [`MUSID_RESOLVE_BUDGET`]）
    /// ——**只计数**（成功记 MusidOk、失败记 MusidErrLegacyErr），musid
    /// 产物不进用户路径，旧错误原样向上（与不开 probe 时逐字节**与时延**
    /// 一致：对账 resolve 在后台任务进行，错误路径不等它）。替换模式
    /// musid 已尝试过 → MusidErrLegacyErr（纯簿记，零时延）。未路由时无
    /// 动作。
    #[cfg(feature = "musid-dli-adapter")]
    async fn musid_url_err_exit(&self, slot: &mut MusidUrlSlot, song: &SongInfo, quality: Quality) {
        if slot.probe {
            let Some(musid) = slot.musid.as_ref() else {
                return;
            };
            // 失败侧对账在后台任务落账（评审第二轮 F1：probe 影子不阻塞
            // 错误路径——旧错误立即向上；fire-and-forget 的取消语义见
            // §11.5：用户取消/外层超时后影子仍消耗 musid 配额并落账）。
            let parity = Arc::clone(&self.parity);
            let source = slot.source;
            let probe_song = song.clone();
            let musid = Arc::clone(musid);
            tokio::spawn(async move {
                let attempt = tokio::time::timeout(
                    MUSID_RESOLVE_BUDGET,
                    musid.get_song_url(&probe_song, quality),
                )
                .await;
                let outcome = match attempt {
                    Ok(Ok(_)) => crate::parity::ParityOutcome::MusidOk,
                    Ok(Err(_)) | Err(_) => crate::parity::ParityOutcome::MusidErrLegacyErr,
                };
                parity.record(crate::parity::ParityFace::SongUrl, source, outcome);
            });
            return;
        }
        if slot.attempted {
            self.musid_record(
                crate::parity::ParityFace::SongUrl,
                slot.source,
                crate::parity::ParityOutcome::MusidErrLegacyErr,
            );
        }
    }

    /// 歌曲的内置回退音源：普通歌曲取来源音源；
    /// JS 音源的搜索结果按 `extra["source"]` 平台标记映射到对应内置音源。
    fn builtin_fallback_source(&self, song: &SongInfo) -> Option<Arc<dyn MusicSource>> {
        let id = if song.source == SourceId::Local {
            builtin_source_for_js(song)?
        } else {
            song.source
        };
        self.sources.get(&id).map(Arc::clone)
    }

    /// 优先使用已导入的 lx-music JS 音源获取歌词，空结果时回退到内置搜索源。
    pub async fn get_lyric(&self, song: &SongInfo) -> Result<LyricData, FetchError> {
        match tokio::time::timeout(
            std::time::Duration::from_secs(20),
            self.get_lyric_inner(song),
        )
        .await
        {
            // 第二个元组位是 musid 槽位的 err 侧归因轨迹（feature 构建）；
            // 裸 get_lyric 调用（跨源补全候选等）不用它。
            Ok((result, _)) => result,
            Err(_) => Err(FetchError::Network("获取歌词超时".to_string())),
        }
    }

    /// 歌词主链（本地外挂 → JS → [musid 槽位] → 内置 fallback）。
    ///
    /// 第二个元组位：musid 槽位"已尝试且无产出"的归因平台（替换模式的
    /// err 侧 legacy 计数输入；feature 关闭时恒 `None`）。
    async fn get_lyric_inner(
        &self,
        song: &SongInfo,
    ) -> (Result<LyricData, FetchError>, Option<SourceId>) {
        // 真本地文件歌曲优先读外挂/内嵌歌词，不经过 JS 音源（拿文件路径当
        // 平台歌曲 id 请求必然无效）。无词时返回空数据，由
        // get_lyric_with_fallback 决定是否联网补全。
        if is_local_file_song(song)
            && let Some(local) = self.sources.get(&SourceId::Local)
        {
            return (local.get_lyric(song).await, None);
        }
        for js_source in self.js_sources() {
            if let Ok(data) = js_source.get_lyric(song).await
                && lyric_has_content(&data)
            {
                return (Ok(data), None);
            }
        }

        // V1 双栈歌词槽位（musid-dli-adapter + 歌词转换变体，opt-in）：与
        // JS 槽位同位——JS 之后、内置 fallback 之前；产物过同一内容门
        // （lyric_has_content）；None/空内容/Err → 继续链、不写负缓存
        // （负缓存仍由 get_lyric_with_fallback 统一裁决，与旧路径同键）。
        #[cfg(feature = "musid-dli-adapter")]
        let musid_slot = self.musid_lyric_slot(song).await;
        #[cfg(feature = "musid-dli-adapter")]
        if let Some(data) = musid_slot.content {
            return (Ok(data), None);
        }

        let Some(source) = self.builtin_fallback_source(song) else {
            return (Err(FetchError::Other("歌曲来源不可用".to_string())), None);
        };
        let result = source.get_lyric(song).await;
        #[cfg(feature = "musid-dli-adapter")]
        let empty_source = musid_slot.empty_source;
        #[cfg(not(feature = "musid-dli-adapter"))]
        let empty_source = None;
        (result, empty_source)
    }

    /// V1 双栈歌词槽位（feature `musid-dli-adapter`）：替换模式尝试 musid
    /// 取词；probe 模式不在此抢跑（真双跑对照在 `get_lyric_with_fallback`
    /// 的主请求）；缺歌词转换变体（`musid-dli-adapter-lyrics`）时该面恒
    /// 不生效（启动 warn 由 `bootstrap_musid_stack` 负责）。
    #[cfg(feature = "musid-dli-adapter")]
    async fn musid_lyric_slot(&self, song: &SongInfo) -> MusidLyricSlot {
        let none = MusidLyricSlot {
            content: None,
            empty_source: None,
        };
        if !cfg!(feature = "musid-dli-adapter-lyrics") {
            return none;
        }
        let Some(source) = Self::musid_effective_source(song) else {
            return none;
        };
        let lyric_flag = self
            .musid_routes
            .lyric
            .load(std::sync::atomic::Ordering::Relaxed);
        let probe = self
            .musid_routes
            .parity_probe
            .load(std::sync::atomic::Ordering::Relaxed);
        if !lyric_flag || probe {
            return none;
        }
        let Some(musid) = self.musid_route(lyric_flag, crate::parity::ParityFace::Lyric, source)
        else {
            return none;
        };
        match musid.get_lyric(song).await {
            Ok(data) if lyric_has_content(&data) => {
                self.musid_record(
                    crate::parity::ParityFace::Lyric,
                    source,
                    crate::parity::ParityOutcome::MusidOk,
                );
                MusidLyricSlot {
                    content: Some(data),
                    empty_source: None,
                }
            }
            _ => MusidLyricSlot {
                content: None,
                empty_source: Some(source),
            },
        }
    }

    /// 歌词主请求的双栈入口（feature `musid-dli-adapter`）：返回 (旧路径
    /// 结果, musid 空产出归因平台)。probe 模式真双跑——musid 影子在后台
    /// 任务对账（评审 P4，用户路径零额外延迟），恒展示旧路径结果。
    #[cfg(feature = "musid-dli-adapter")]
    async fn musid_lyric_main(
        &self,
        song: &SongInfo,
    ) -> (Result<LyricData, FetchError>, Option<SourceId>) {
        // 歌词面的 musid 参与（替换与 probe 双跑）都依赖歌词转换变体才有
        // 产物可归一；缺变体时该面整体不生效。
        if !cfg!(feature = "musid-dli-adapter-lyrics") {
            return (self.get_lyric(song).await, None);
        }
        let probe = self
            .musid_routes
            .parity_probe
            .load(std::sync::atomic::Ordering::Relaxed);
        if probe {
            let lyric_flag = self
                .musid_routes
                .lyric
                .load(std::sync::atomic::Ordering::Relaxed);
            let Some(source) = Self::musid_effective_source(song) else {
                return (self.get_lyric(song).await, None);
            };
            let Some(musid) = self.musid_route(
                lyric_flag || probe,
                crate::parity::ParityFace::Lyric,
                source,
            ) else {
                return (self.get_lyric(song).await, None);
            };
            // 真双跑（纯观测，评审 P4）：musid 影子在后台任务对账（oneshot
            // 接收旧路径结论），用户路径零额外延迟，计数最终一致。旧路径带
            // 20s 超时，影子同预算。计数——单侧瞬时 Err 不作对账结论，双 Err
            // 记 MusidErrLegacyErr，双 Ok 按内容对照（双方都确认无词 =
            // ParityMatch）。
            let (verdict_tx, verdict_rx) = tokio::sync::oneshot::channel();
            let parity = Arc::clone(&self.parity);
            let probe_song = song.clone();
            tokio::spawn(async move {
                let shadow = tokio::time::timeout(
                    std::time::Duration::from_secs(20),
                    musid.get_lyric(&probe_song),
                )
                .await;
                let verdict = match verdict_rx.await {
                    Ok(verdict) => verdict,
                    // 主路径异常结束（发送端被弃）：不作对账结论。
                    Err(_) => return,
                };
                let outcome = match (&verdict, &shadow) {
                    (crate::parity::LyricLegacyVerdict::Failed, Err(_)) => {
                        Some(crate::parity::ParityOutcome::MusidErrLegacyErr)
                    }
                    (crate::parity::LyricLegacyVerdict::Failed, _) => None,
                    (_, Err(_)) | (_, Ok(Err(_))) => None,
                    (crate::parity::LyricLegacyVerdict::NoContent, Ok(Ok(shadow))) => {
                        let matched = !lyric_has_content(shadow);
                        Some(if matched {
                            crate::parity::ParityOutcome::ParityMatch
                        } else {
                            crate::parity::ParityOutcome::ParityMismatch
                        })
                    }
                    (crate::parity::LyricLegacyVerdict::Content(fingerprint), Ok(Ok(shadow))) => {
                        let matched = lyric_has_content(shadow)
                            && crate::parity::lyric_fingerprints_equivalent(fingerprint, shadow);
                        Some(if matched {
                            crate::parity::ParityOutcome::ParityMatch
                        } else {
                            crate::parity::ParityOutcome::ParityMismatch
                        })
                    }
                };
                if let Some(outcome) = outcome {
                    parity.record(crate::parity::ParityFace::Lyric, source, outcome);
                }
            });
            let legacy_result = self.get_lyric(song).await;
            let verdict = match &legacy_result {
                Err(_) => crate::parity::LyricLegacyVerdict::Failed,
                Ok(data) if lyric_has_content(data) => crate::parity::LyricLegacyVerdict::Content(
                    crate::parity::lyric_fingerprint(data),
                ),
                Ok(_) => crate::parity::LyricLegacyVerdict::NoContent,
            };
            let _ = verdict_tx.send(verdict);
            return (legacy_result, None);
        }
        // 替换模式：主请求走含 musid 槽位的链（get_lyric_inner），带回
        // err 侧归因轨迹。
        match tokio::time::timeout(
            std::time::Duration::from_secs(20),
            self.get_lyric_inner(song),
        )
        .await
        {
            Ok((result, empty_source)) => (result, empty_source),
            Err(_) => (Err(FetchError::Network("获取歌词超时".to_string())), None),
        }
    }

    /// 获取歌词，当前音源无内容时自动从同曲候选中补全。
    ///
    /// 只有在所有音源都“确认无词”时才写入负缓存；网络类错误直接向上
    /// 传播，避免一次抖动把歌曲误标为无词。
    pub async fn get_lyric_with_fallback(&self, song: &SongInfo) -> Result<LyricData, FetchError> {
        const NEGATIVE_TTL: std::time::Duration = std::time::Duration::from_secs(600);
        const NEGATIVE_CACHE_LIMIT: usize = 1024;
        // JS 音源的搜索结果都标记为 Local，不同平台的数字 id 可能相同，
        // 缓存键必须带上 extra["source"] 平台标记才能互相区分。
        let cache_key = (
            song.source,
            song.extra.get("source").cloned(),
            song.id.clone(),
        );
        {
            let mut cache = self
                .lyric_negative_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(verified_at) = cache.get(&cache_key)
                && verified_at.elapsed() < NEGATIVE_TTL
            {
                return Err(FetchError::NotFound);
            }
            // 顺手清理过期项并限制容量，防止长跑会话中无限增长
            cache.retain(|_, verified_at| verified_at.elapsed() < NEGATIVE_TTL);
            if cache.len() >= NEGATIVE_CACHE_LIMIT {
                cache.clear();
            }
        }

        let mut last_error: Option<FetchError> = None;
        let mut attempts = |result: Result<LyricData, FetchError>,
                            found: &mut Option<LyricData>| match result {
            Ok(data) if lyric_has_content(&data) => {
                *found = Some(data);
            }
            Ok(_) => {}
            Err(
                e @ (FetchError::Network(_) | FetchError::TooManyRequests | FetchError::Parse(_)),
            ) => {
                last_error = Some(e);
            }
            Err(_) => {}
        };

        let mut found: Option<LyricData> = None;
        // V1 双栈歌词主请求（musid-dli-adapter，opt-in）：probe 影子真双跑
        // 对照计数（恒展示旧路径结果）；替换模式走含 musid 槽位的链并带回
        // err 侧 legacy 归因轨迹。
        #[cfg(feature = "musid-dli-adapter")]
        let (musid_empty_source, found_in_main) = {
            let (result, empty_source) = self.musid_lyric_main(song).await;
            attempts(result, &mut found);
            // err 侧归因只看主请求链（评审 P7）：跨源补全候选来自其它平台，
            // 不归因到路由源平台。
            (empty_source, found.is_some())
        };
        #[cfg(not(feature = "musid-dli-adapter"))]
        attempts(self.get_lyric(song).await, &mut found);
        if found.is_none() {
            // find_music 最多返回上百个候选，而单个 get_lyric 自带 20s 超时：串行
            // 逐个 await 会让「确实无词」的歌曲长时间阻塞播放路径。这里限制候选
            // 数量，并给整段回退（搜索 + 取词）设一个总预算；预算内没找到就按
            // 未命中处理，继续走下方的负缓存逻辑。
            const FALLBACK_CANDIDATE_LIMIT: usize = 8;
            const FALLBACK_BUDGET: std::time::Duration = std::time::Duration::from_secs(15);
            let fallback = async {
                for candidate in self
                    .find_music(song)
                    .await
                    .into_iter()
                    .take(FALLBACK_CANDIDATE_LIMIT)
                {
                    attempts(self.get_lyric(&candidate).await, &mut found);
                    if found.is_some() {
                        tracing::debug!(
                            "lyrics for {} matched from {}",
                            song.name,
                            candidate.source.as_str()
                        );
                        break;
                    }
                }
            };
            if tokio::time::timeout(FALLBACK_BUDGET, fallback)
                .await
                .is_err()
            {
                tracing::debug!(
                    "lyrics for {} fallback exceeded {:?}",
                    song.name,
                    FALLBACK_BUDGET
                );
            }
        }

        if let Some(data) = found {
            #[cfg(feature = "musid-dli-adapter")]
            if found_in_main && let Some(source) = musid_empty_source {
                // musid 槽位无产出（空/Err），旧路径主请求链（本地外挂→JS→
                // 内置 fallback）给出了词 → MusidErrLegacyOk。跨源补全候选
                // 的 rescue 不归因（评审 P7：另一平台的产物不算"同请求
                // 成功"），该情形不计。
                self.musid_record(
                    crate::parity::ParityFace::Lyric,
                    source,
                    crate::parity::ParityOutcome::MusidErrLegacyOk,
                );
            }
            self.lyric_negative_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&cache_key);
            return Ok(data);
        }

        #[cfg(feature = "musid-dli-adapter")]
        if let Some(source) = musid_empty_source {
            // musid 槽位无产出且整条链（含跨源补全）确认无词 → 双方皆无
            // （无词结论是全链的，归因到路由源平台）。
            self.musid_record(
                crate::parity::ParityFace::Lyric,
                source,
                crate::parity::ParityOutcome::MusidErrLegacyErr,
            );
        }
        if let Some(error) = last_error {
            return Err(error);
        }
        self.lyric_negative_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(cache_key, Instant::now());
        Err(FetchError::NotFound)
    }

    /// 优先使用搜索结果中的封面，其次请求 JS 音源，最后回退到内置搜索源。
    pub async fn get_cover_url(&self, song: &SongInfo) -> Result<String, FetchError> {
        if let Some(url) = song.cover_url.as_ref().filter(|url| !url.trim().is_empty()) {
            return Ok(url.clone());
        }
        for js_source in self.js_sources() {
            if let Ok(url) = js_source.get_cover_url(song).await
                && !url.trim().is_empty()
            {
                return Ok(url);
            }
        }

        let source = self
            .builtin_fallback_source(song)
            .ok_or_else(|| FetchError::Other("歌曲来源不可用".to_string()))?;
        source.get_cover_url(song).await
    }

    /// 跨源匹配：在其他音源中搜索同名歌曲
    /// 参考 lx-music findMusic 算法
    pub async fn find_music(&self, song: &SongInfo) -> Vec<SongInfo> {
        self.find_music_within(song, None).await
    }

    /// 只在指定平台里找同曲（解析策略 `only` 用）。
    ///
    /// 匹配规则与 [`Self::find_music`] 完全一致，只是把候选来源收窄 —— 这样
    /// "只用某个平台"不会顺带改变严格匹配的行为。
    pub async fn find_music_on(&self, song: &SongInfo, platforms: &[SourceId]) -> Vec<SongInfo> {
        self.find_music_within(song, Some(platforms)).await
    }

    async fn find_music_within(
        &self,
        song: &SongInfo,
        restrict: Option<&[SourceId]>,
    ) -> Vec<SongInfo> {
        let exclude = song.source;
        let keyword = format!("{} {}", song.name, song.singer);

        // 1. 并行搜索所有其他源
        let mut tasks = tokio::task::JoinSet::new();
        let enabled = self
            .enabled
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for id in SourceId::all_online() {
            if *id == exclude || !enabled.contains(id) {
                continue;
            }
            if let Some(platforms) = restrict
                && !platforms.contains(id)
            {
                continue;
            }
            if let Some(source) = self.sources.get(id) {
                let src = Arc::clone(source);
                let kw = keyword.clone();
                tasks.spawn(async move {
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(8),
                        src.search(&kw, 1, 25),
                    )
                    .await
                    {
                        Ok(Ok(result)) => Some(result.items),
                        _ => None,
                    }
                });
            }
        }

        // 2. 收集所有结果
        let mut all: Vec<SongInfo> = Vec::new();
        while let Some(task) = tasks.join_next().await {
            if let Ok(Some(items)) = task {
                all.extend(items);
            }
        }

        // 3. 预处理：计算匹配用字段
        let target_name = crate::filter::filter_str(&song.name).to_lowercase();
        let target_singer =
            crate::filter::filter_str(&crate::filter::sort_singer(&song.singer)).to_lowercase();
        let target_interval = song.duration.as_secs() as i64;

        // 4. 过滤
        all.retain(|s| {
            let f_name = crate::filter::filter_str(&s.name).to_lowercase();
            let f_singer =
                crate::filter::filter_str(&crate::filter::sort_singer(&s.singer)).to_lowercase();
            let f_interval = s.duration.as_secs() as i64;
            let f_album = crate::filter::filter_str(&s.album_name).to_lowercase();

            // 时长匹配 (允许 ±5秒)
            if target_interval > 0 && f_interval > 0 && (target_interval - f_interval).abs() >= 5 {
                return false;
            }

            // 三层匹配
            f_name == target_name && f_singer.contains(&target_singer)
                || f_singer == target_singer && f_name.contains(&target_name)
                || (!f_album.is_empty()
                    && f_album == target_name
                    && f_singer.contains(&target_singer)
                    && f_name.contains(&target_name))
        });

        // 5. 排序（按匹配度）。优化轮 P2-4：retain 阶段已算过的归一化字段
        // 直接携带进排序（原实现 match_score 在每个比较里对两侧重跑
        // filter_str + sort_singer + to_lowercase）。
        let decorated: Vec<(SongInfo, String, String, i64)> = all
            .into_iter()
            .map(|s| {
                let f_name = crate::filter::filter_str(&s.name).to_lowercase();
                let f_singer = crate::filter::filter_str(&crate::filter::sort_singer(&s.singer))
                    .to_lowercase();
                let f_intv = s.duration.as_secs() as i64;
                (s, f_name, f_singer, f_intv)
            })
            .collect();
        let mut decorated: Vec<_> = decorated;
        decorated.sort_by(|a, b| {
            let a_score = scored_match(
                &a.1,
                &a.2,
                a.3,
                &target_name,
                &target_singer,
                target_interval,
            );
            let b_score = scored_match(
                &b.1,
                &b.2,
                b.3,
                &target_name,
                &target_singer,
                target_interval,
            );
            b_score.cmp(&a_score)
        });

        decorated.into_iter().map(|(song, _, _, _)| song).collect()
    }
}

async fn check_source(id: SourceId, source: Arc<dyn MusicSource>) -> SourceHealth {
    let name = source.name().to_string();
    let started = Instant::now();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        source.search("周杰伦", 1, 1),
    )
    .await;
    let latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    match result {
        Ok(Ok(result)) => SourceHealth {
            id,
            name,
            ok: true,
            latency_ms,
            result_count: result.items.len() as u32,
            detail: "搜索正常".to_string(),
        },
        Ok(Err(error)) => SourceHealth {
            id,
            name,
            ok: false,
            latency_ms,
            result_count: 0,
            detail: error.to_string(),
        },
        Err(_) => SourceHealth {
            id,
            name,
            ok: false,
            latency_ms,
            result_count: 0,
            detail: "请求超时（8 秒）".to_string(),
        },
    }
}

fn lyric_has_content(data: &LyricData) -> bool {
    !data.lyric.trim().is_empty()
        || data
            .tlyric
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        || data
            .rlyric
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        || data
            .lxlyric
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
}

/// 计算匹配分数（越高越匹配）
/// `match_score` 的纯分数半：吃已归一化的字段（调用方缓存重用，优化轮
/// P2-4）。
fn scored_match(
    f_name: &str,
    f_singer: &str,
    f_intv: i64,
    t_name: &str,
    t_singer: &str,
    t_intv: i64,
) -> i32 {
    let mut score = 0;
    if f_singer == t_singer {
        score += 30;
    }
    if f_name == t_name {
        score += 30;
    }
    if (f_intv - t_intv).abs() < 2 {
        score += 20;
    }
    if f_name.contains(t_name) || t_name.contains(f_name) {
        score += 10;
    }
    score
}

#[cfg(test)]
mod tests {
    use super::{SourceManager, is_local_file_song, lyric_has_content};
    use async_trait::async_trait;
    use lx_core::model::lyric::LyricData;
    use lx_core::model::song::SongInfo;
    use lx_core::model::source::{Quality, SourceId};
    use lx_core::traits::source::{
        FetchError, MusicSource, SearchError, SearchResult, SongUrl, SourceCapabilities,
    };
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[test]
    fn builtin_sources_declare_their_capabilities() {
        let manager = SourceManager::new(SourceId::Kw, SourceId::all_online());

        let kw = manager.capabilities(SourceId::Kw);
        assert!(kw.playlists && kw.leaderboard && kw.album);
        assert!(
            kw.playlist_categories && kw.link_parse,
            "酷我已接入分类与直解"
        );
        assert!(!kw.qr_login, "酷我尚未接入扫码登录");

        let bili = manager.capabilities(SourceId::Bili);
        assert!(bili.login && bili.qr_login && bili.link_parse);

        let kg = manager.capabilities(SourceId::Kg);
        assert!(kg.playlist_search && kg.playlist_categories && kg.link_parse);

        let wy = manager.capabilities(SourceId::Wy);
        assert!(
            wy.playlist_categories && wy.link_parse,
            "网易云是分类与直解的参考实现"
        );

        let tx = manager.capabilities(SourceId::Tx);
        assert!(tx.playlist_search && tx.playlist_categories && tx.link_parse);

        let mg = manager.capabilities(SourceId::Mg);
        assert!(mg.playlist_search && mg.playlist_categories && mg.link_parse);

        // 汽水：歌单与直解可用，但没有分类目录（平台本身不提供）。
        let soda = manager.capabilities(SourceId::Soda);
        assert!(soda.playlist_search && soda.link_parse);
        assert!(!soda.playlist_categories);

        // 尚未实现的新平台按「全部不支持」处理，界面不会展示对应入口。
        assert_eq!(
            manager.capabilities(SourceId::Local),
            SourceCapabilities::default()
        );

        let leaderboards = manager.sources_supporting(|caps| caps.leaderboard);
        assert!(leaderboards.contains(&SourceId::Kw));
        assert!(leaderboards.contains(&SourceId::Bili));
        assert!(!leaderboards.contains(&SourceId::Local));

        // 分类入口只对有实现的音源开放。V4-s2（§14.3）：qianqian 的 legacy
        // 已删——分类面由 musid 适配面承担（default 特性在）；零 musid 树
        // （--no-default-features）下 qianqian 源整体缺席，不进本列。
        #[cfg(feature = "musid-dli-adapter-qianqian")]
        let with_qianqian = vec![SourceId::Qianqian];
        #[cfg(not(feature = "musid-dli-adapter-qianqian"))]
        let with_qianqian: Vec<SourceId> = vec![];
        let mut expected_categories = vec![
            SourceId::Kw,
            SourceId::Kg,
            SourceId::Tx,
            SourceId::Wy,
            SourceId::Mg,
        ];
        expected_categories.extend(with_qianqian);
        expected_categories.push(SourceId::Apple);
        assert_eq!(
            manager.sources_supporting(|caps| caps.playlist_categories),
            expected_categories
        );
    }

    struct StubJsSource {
        name: &'static str,
        succeeds: bool,
        calls: Arc<Mutex<Vec<&'static str>>>,
    }

    #[async_trait]
    impl MusicSource for StubJsSource {
        fn id(&self) -> SourceId {
            SourceId::Kw
        }

        fn name(&self) -> &str {
            self.name
        }

        async fn search(
            &self,
            _keyword: &str,
            _page: u32,
            _limit: u32,
        ) -> Result<SearchResult, SearchError> {
            Err(SearchError::Other("unused".to_string()))
        }

        async fn get_song_url(
            &self,
            _song: &SongInfo,
            quality: Quality,
        ) -> Result<SongUrl, FetchError> {
            self.calls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(self.name);
            if !self.succeeds {
                return Err(FetchError::NotFound);
            }
            Ok(SongUrl {
                url: format!("https://example.com/{}.mp3", self.name),
                quality,
                duration: Duration::from_secs(180),
                cover_url: None,
                qualities: vec![quality],
                headers: vec![],
                size: None,
                size_is_advisory: false,
                md5: None,
                candidate_urls: vec![],
                max_chunk_size: 0,
            })
        }

        async fn get_lyric(&self, _song: &SongInfo) -> Result<LyricData, FetchError> {
            Err(FetchError::NotFound)
        }

        async fn get_cover_url(&self, _song: &SongInfo) -> Result<String, FetchError> {
            Err(FetchError::NotFound)
        }

        fn supported_qualities(&self) -> Vec<Quality> {
            vec![Quality::High320]
        }
    }

    #[test]
    fn translated_lyrics_count_as_content() {
        let data = LyricData {
            tlyric: Some("[00:01.00]translation".to_string()),
            ..LyricData::default()
        };

        assert!(lyric_has_content(&data));
        assert!(!lyric_has_content(&LyricData::default()));
    }

    #[test]
    fn stale_js_source_request_cannot_commit() {
        let manager = SourceManager::new(SourceId::Kw, SourceId::all_online());
        let stale_generation = manager.begin_js_source_request(false);
        let current_generation = manager.begin_js_source_request(true);

        assert!(!manager.set_js_source_if_current(
            stale_generation,
            Arc::new(crate::local::LocalSource::new()),
        ));
        assert!(manager.set_js_source_if_current(
            current_generation,
            Arc::new(crate::local::LocalSource::new()),
        ));
        assert!(manager.has_js_source());
    }

    #[tokio::test]
    async fn song_url_uses_js_sources_in_order_until_one_succeeds() {
        let manager = SourceManager::new(SourceId::Kw, SourceId::all_online());
        // 本用例锁定的是「JS 优先」模式下的顺序语义；默认顺序（内置先行）
        // 在下面的 builtin_source_resolves_before_js_sources 里锁定。
        manager.set_js_url_first(true);
        let generation = manager.begin_js_source_request(true);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let source = |name, succeeds| {
            Arc::new(StubJsSource {
                name,
                succeeds,
                calls: Arc::clone(&calls),
            }) as Arc<dyn MusicSource>
        };
        assert!(manager.set_js_sources_if_current(
            generation,
            vec![
                source("juhe", false),
                source("grass", true),
                source("unused", true),
            ],
        ));
        let song = SongInfo::new(
            "song-id".to_string(),
            SourceId::Kw,
            "song".to_string(),
            "artist".to_string(),
        );

        let result = manager
            .get_song_url(&song, Quality::High320)
            .await
            .expect("the second JS source should resolve the song");

        assert_eq!(result.url, "https://example.com/grass.mp3");
        assert_eq!(
            *calls.lock().unwrap_or_else(|e| e.into_inner()),
            vec!["juhe", "grass"]
        );
    }

    #[tokio::test]
    async fn song_url_can_resume_from_the_next_js_source() {
        let manager = SourceManager::new(SourceId::Kw, SourceId::all_online());
        manager.set_js_url_first(true);
        let generation = manager.begin_js_source_request(true);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let source = |name| {
            Arc::new(StubJsSource {
                name,
                succeeds: true,
                calls: Arc::clone(&calls),
            }) as Arc<dyn MusicSource>
        };
        assert!(
            manager.set_js_sources_if_current(generation, vec![source("juhe"), source("grass")],)
        );
        let song = SongInfo::new(
            "song-id".to_string(),
            SourceId::Kw,
            "song".to_string(),
            "artist".to_string(),
        );

        let (first, first_index) = manager
            .get_song_url_from_js_index(&song, Quality::High320, 0)
            .await
            .unwrap();
        let (second, second_index) = manager
            .get_song_url_from_js_index(&song, Quality::High320, first_index.unwrap() + 1)
            .await
            .unwrap();

        assert_eq!(first.url, "https://example.com/juhe.mp3");
        assert_eq!(second.url, "https://example.com/grass.mp3");
        assert_eq!(second_index, Some(1));
        assert_eq!(
            *calls.lock().unwrap_or_else(|e| e.into_inner()),
            vec!["juhe", "grass"]
        );
    }

    /// 默认顺序（内置音源先行）：单个坏掉的 JS 音源不再拦在本可直接解析的
    /// 歌前面；JS 音源仍在其后参与回退。
    #[tokio::test]
    async fn builtin_source_resolves_before_js_sources() {
        let mut manager = SourceManager::new(SourceId::Kw, SourceId::all_online());
        let calls = Arc::new(Mutex::new(Vec::new()));
        let source = |name, succeeds| {
            Arc::new(StubJsSource {
                name,
                succeeds,
                calls: Arc::clone(&calls),
            }) as Arc<dyn MusicSource>
        };
        // 覆盖内置酷我音源：立即失败的可记录桩。
        manager.register(source("kw-builtin", false));
        let generation = manager.begin_js_source_request(true);
        assert!(manager.set_js_sources_if_current(generation, vec![source("juhe", true)]));
        let song = SongInfo::new(
            "song-id".to_string(),
            SourceId::Kw,
            "song".to_string(),
            "artist".to_string(),
        );

        assert!(!manager.js_url_first(), "默认必须是内置音源先行");
        let result = manager
            .get_song_url(&song, Quality::High320)
            .await
            .expect("内置失败后应由 JS 音源兜底");

        assert_eq!(result.url, "https://example.com/juhe.mp3");
        assert_eq!(
            *calls.lock().unwrap_or_else(|e| e.into_inner()),
            vec!["kw-builtin", "juhe"],
            "必须先试内置音源，失败后才轮到 JS 音源"
        );
    }

    /// 可指定平台 id 的可记录桩：验证 JS 搜到的歌按 extra[source] 回到平台
    /// 内置音源时用（`StubJsSource` 的 id 固定为 Kw）。
    struct PlatformStub {
        id: SourceId,
        name: &'static str,
        succeeds: bool,
        calls: Arc<Mutex<Vec<&'static str>>>,
    }

    #[async_trait]
    impl MusicSource for PlatformStub {
        fn id(&self) -> SourceId {
            self.id
        }

        fn name(&self) -> &str {
            self.name
        }

        async fn search(
            &self,
            _keyword: &str,
            _page: u32,
            _limit: u32,
        ) -> Result<SearchResult, SearchError> {
            Err(SearchError::Other("unused".to_string()))
        }

        async fn get_song_url(
            &self,
            _song: &SongInfo,
            quality: Quality,
        ) -> Result<SongUrl, FetchError> {
            self.calls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(self.name);
            if !self.succeeds {
                return Err(FetchError::NotFound);
            }
            Ok(SongUrl {
                url: format!("https://example.com/{}.mp3", self.name),
                quality,
                duration: Duration::from_secs(180),
                cover_url: None,
                qualities: vec![quality],
                headers: vec![],
                size: None,
                size_is_advisory: false,
                md5: None,
                candidate_urls: vec![],
                max_chunk_size: 0,
            })
        }

        async fn get_lyric(&self, _song: &SongInfo) -> Result<LyricData, FetchError> {
            Err(FetchError::NotFound)
        }

        async fn get_cover_url(&self, _song: &SongInfo) -> Result<String, FetchError> {
            Err(FetchError::NotFound)
        }

        fn supported_qualities(&self) -> Vec<Quality> {
            vec![Quality::High320]
        }
    }

    /// JS 音源搜到的歌（Local + extra[source]）同样内置先行。
    #[tokio::test]
    async fn js_sourced_songs_prefer_their_platform_builtin_source() {
        let mut manager = SourceManager::new(SourceId::Wy, SourceId::all_online());
        let calls = Arc::new(Mutex::new(Vec::new()));
        let builtin = Arc::new(PlatformStub {
            id: SourceId::Wy,
            name: "wy-builtin",
            succeeds: true,
            calls: Arc::clone(&calls),
        });
        manager.register(builtin);
        let generation = manager.begin_js_source_request(true);
        let juhe = Arc::new(PlatformStub {
            id: SourceId::Kw,
            name: "juhe",
            succeeds: true,
            calls: Arc::clone(&calls),
        });
        assert!(manager.set_js_sources_if_current(generation, vec![juhe as Arc<dyn MusicSource>],));
        let mut song = SongInfo::new(
            "wy-id".to_string(),
            SourceId::Local,
            "song".to_string(),
            "artist".to_string(),
        );
        song.extra.insert("source".to_string(), "wy".to_string());

        let result = manager
            .get_song_url(&song, Quality::High320)
            .await
            .expect("平台内置音源应直接解析");

        assert_eq!(result.url, "https://example.com/wy-builtin.mp3");
        assert_eq!(
            *calls.lock().unwrap_or_else(|e| e.into_inner()),
            vec!["wy-builtin"],
            "内置音源命中后不应再请求 JS 音源"
        );
    }

    #[test]
    fn js_platform_tags_map_to_builtin_sources() {
        let tag = |platform: &str| {
            let mut song = song_with(SourceId::Local);
            song.extra
                .insert("source".to_string(), platform.to_string());
            super::builtin_source_for_js(&song)
        };

        assert_eq!(tag("kw"), Some(SourceId::Kw));
        assert_eq!(tag("kg"), Some(SourceId::Kg));
        assert_eq!(tag("tx"), Some(SourceId::Tx));
        assert_eq!(tag("wy"), Some(SourceId::Wy));
        assert_eq!(tag("mg"), Some(SourceId::Mg));
        // 千千（脚本里也写作 bd / baidu）
        assert_eq!(tag("qianqian"), Some(SourceId::Qianqian));
        assert_eq!(tag("bd"), Some(SourceId::Qianqian));
        assert_eq!(tag("joox"), Some(SourceId::Joox));
        assert_eq!(tag("jamendo"), Some(SourceId::Jamendo));
        assert_eq!(tag("5sing"), Some(SourceId::Fivesing));
        assert_eq!(tag("Fivesing"), Some(SourceId::Fivesing), "大小写不敏感");
        assert_eq!(tag("apple"), Some(SourceId::Apple));
        assert_eq!(tag("bilibili"), Some(SourceId::Bili));
        assert_eq!(tag("qishui"), Some(SourceId::Soda));
        assert_eq!(tag("unknown-platform"), None);
        // 普通歌曲不参与映射
        assert_eq!(super::builtin_source_for_js(&song_with(SourceId::Kw)), None);
    }

    #[test]
    fn named_js_sources_keep_their_origin_and_display_names() {
        let manager = SourceManager::new(SourceId::Kw, SourceId::all_online());
        let generation = manager.begin_js_source_request(true);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let source = |name| {
            Arc::new(StubJsSource {
                name,
                succeeds: true,
                calls: Arc::clone(&calls),
            }) as Arc<dyn MusicSource>
        };

        assert!(manager.set_named_js_sources_if_current(
            generation,
            vec![
                ("https://example.com/juhe.js".to_string(), source("聚合")),
                ("https://example.com/grass.js".to_string(), source("草原")),
            ],
        ));

        assert_eq!(
            manager.js_source_names(),
            vec!["聚合".to_string(), "草原".to_string()]
        );
        assert_eq!(
            manager.js_source_name_for_origin("https://example.com/grass.js"),
            Some("草原".to_string())
        );
    }

    /// JS 音源歌词接口一旦被调用就 panic，用于证明本地歌词路径没有触发请求。
    struct LyricPanicSource;

    #[async_trait]
    impl MusicSource for LyricPanicSource {
        fn id(&self) -> SourceId {
            SourceId::Local
        }

        fn name(&self) -> &str {
            "lyric-panic"
        }

        async fn search(
            &self,
            _keyword: &str,
            _page: u32,
            _limit: u32,
        ) -> Result<SearchResult, SearchError> {
            Err(SearchError::Other("unused".to_string()))
        }

        async fn get_song_url(
            &self,
            _song: &SongInfo,
            _quality: Quality,
        ) -> Result<SongUrl, FetchError> {
            Err(FetchError::NotFound)
        }

        async fn get_lyric(&self, _song: &SongInfo) -> Result<LyricData, FetchError> {
            panic!("本地文件歌曲不应请求 JS 音源歌词");
        }

        async fn get_cover_url(&self, _song: &SongInfo) -> Result<String, FetchError> {
            Err(FetchError::NotFound)
        }

        fn supported_qualities(&self) -> Vec<Quality> {
            vec![Quality::High320]
        }
    }

    #[tokio::test]
    async fn local_file_song_reads_sidecar_lyric_without_network() {
        let dir = std::env::temp_dir().join("voicefox-manager-lyric-test");
        std::fs::create_dir_all(&dir).expect("创建临时目录失败");
        let audio_path = dir.join("song.flac");
        std::fs::write(&audio_path, b"").expect("写入音频占位文件失败");
        std::fs::write(dir.join("song.lrc"), "[00:01.00]本地外挂歌词").expect("写入歌词文件失败");

        let manager = SourceManager::new(SourceId::Kw, SourceId::all_online());
        let generation = manager.begin_js_source_request(true);
        assert!(manager.set_js_source_if_current(generation, Arc::new(LyricPanicSource)));

        let mut song = SongInfo::new(
            audio_path.to_string_lossy().to_string(),
            SourceId::Local,
            "不存在的本地测试歌曲voicefox".to_string(),
            "测试歌手voicefox".to_string(),
        );
        song.file_path = Some(audio_path);

        let data = manager
            .get_lyric_with_fallback(&song)
            .await
            .expect("应读到外挂歌词而不是报错");

        assert_eq!(data.lyric, "[00:01.00]本地外挂歌词");

        std::fs::remove_dir_all(&dir).ok();
    }

    // ── 本地文件判定 ──
    //
    // 这组用例锁定的是"app 侧播放失败重试"与"解析路径 get_song_url_inner"
    // 必须共用同一份判定：JS 音源搜到的歌也是 `SourceId::Local`，
    // 绝不能因为它 source 是 Local 就被当成真本地文件而跳过换源重试。

    fn song_with(source: SourceId) -> SongInfo {
        SongInfo::new(
            "id".to_string(),
            source,
            "歌名".to_string(),
            "歌手".to_string(),
        )
    }

    #[test]
    fn local_song_with_file_path_is_a_local_file() {
        let mut song = song_with(SourceId::Local);
        song.file_path = Some(std::path::PathBuf::from("/music/a.flac"));
        assert!(is_local_file_song(&song));
    }

    #[test]
    fn local_song_without_file_path_is_still_a_local_file() {
        // 本地扫描器是在 metadata::read_metadata 之后才补 file_path 的，
        // 因此"source=Local 且没有 extra[source]"这个中间态也算本地文件。
        let song = song_with(SourceId::Local);
        assert!(song.file_path.is_none());
        assert!(is_local_file_song(&song));
    }

    #[test]
    fn js_sourced_song_is_not_a_local_file_even_with_file_path() {
        // JS 音源搜索结果：source=Local + extra["source"]=平台。
        let mut song = song_with(SourceId::Local);
        song.extra.insert("source".to_string(), "wy".to_string());
        assert!(!is_local_file_song(&song), "JS 搜到的歌必须可换源重试");

        // 即使它后来被下载落到本地路径，也仍然是"可换源的歌"而不是本地文件。
        song.file_path = Some(std::path::PathBuf::from("/music/b.flac"));
        assert!(!is_local_file_song(&song));
    }

    #[test]
    fn remote_songs_are_never_local_files() {
        for source in [
            SourceId::Wy,
            SourceId::Kw,
            SourceId::Kg,
            SourceId::Tx,
            SourceId::Mg,
            SourceId::Bili,
        ] {
            assert!(
                !is_local_file_song(&song_with(source)),
                "{source:?} 不是本地文件"
            );
        }
    }

    // ── 跨平台回退 ──

    /// 跨平台搜索的行为可配置桩音源。
    enum SearchBehavior {
        /// 立即失败（模拟 tx 那种"快速失败"的坏源）
        Fail,
        /// 立即返回给定结果
        Items(Vec<SongInfo>),
        /// 永不返回（模拟挂死的源）
        Hang,
    }

    struct StubSearchSource {
        id: SourceId,
        behavior: SearchBehavior,
    }

    #[async_trait]
    impl MusicSource for StubSearchSource {
        fn id(&self) -> SourceId {
            self.id
        }

        fn name(&self) -> &str {
            "stub"
        }

        async fn search(
            &self,
            _keyword: &str,
            _page: u32,
            _limit: u32,
        ) -> Result<SearchResult, SearchError> {
            match &self.behavior {
                SearchBehavior::Fail => Err(SearchError::Other("stub failure".to_string())),
                SearchBehavior::Items(items) => Ok(SearchResult {
                    items: items.clone(),
                    total: items.len() as u32,
                    has_more: false,
                }),
                SearchBehavior::Hang => {
                    std::future::pending::<()>().await;
                    unreachable!("Hang 分支永不返回")
                }
            }
        }

        async fn get_song_url(
            &self,
            _song: &SongInfo,
            _quality: Quality,
        ) -> Result<SongUrl, FetchError> {
            Err(FetchError::NotFound)
        }

        async fn get_lyric(&self, _song: &SongInfo) -> Result<LyricData, FetchError> {
            Err(FetchError::NotFound)
        }

        async fn get_cover_url(&self, _song: &SongInfo) -> Result<String, FetchError> {
            Err(FetchError::NotFound)
        }

        fn supported_qualities(&self) -> Vec<Quality> {
            vec![Quality::High320]
        }
    }

    fn fallback_target() -> SongInfo {
        let mut song = song_with(SourceId::Wy);
        song.name = "晴天(深情版)".to_string();
        song.singer = "Lucky小爱".to_string();
        song.duration = Duration::from_secs(278);
        song
    }

    fn candidate(name: &str, singer: &str, seconds: u64, source: SourceId) -> SongInfo {
        let mut song = song_with(source);
        song.name = name.to_string();
        song.singer = singer.to_string();
        song.duration = Duration::from_secs(seconds);
        song
    }

    /// 一个立即失败的源不能拖慢整体搜索，也不能贡献候选。
    #[tokio::test]
    async fn find_music_ignores_a_failing_source_without_delaying() {
        let mut manager = SourceManager::new(SourceId::Kw, &[SourceId::Kw, SourceId::Kg]);
        manager.register(Arc::new(StubSearchSource {
            id: SourceId::Kw,
            behavior: SearchBehavior::Fail,
        }));
        manager.register(Arc::new(StubSearchSource {
            id: SourceId::Kg,
            behavior: SearchBehavior::Items(vec![candidate(
                "晴天(深情版)",
                "Lucky小爱",
                278,
                SourceId::Kg,
            )]),
        }));

        let started = std::time::Instant::now();
        let candidates = manager.find_music(&fallback_target()).await;
        let elapsed = started.elapsed();

        assert_eq!(candidates.len(), 1, "失败的源不应产生候选");
        assert_eq!(candidates[0].source, SourceId::Kg);
        assert!(
            elapsed < Duration::from_secs(2),
            "立即失败的源不应拖慢整体搜索，实际耗时 {elapsed:?}"
        );
    }

    /// 挂死的源必须被每源超时切断，而不是让整体搜索无限等待。
    ///
    /// 用 `start_paused` 让 tokio 虚拟时间自动推进，8 秒上限不会真的等 8 秒。
    #[tokio::test(start_paused = true)]
    async fn find_music_cuts_off_a_hanging_source() {
        let mut manager = SourceManager::new(SourceId::Kw, &[SourceId::Kw, SourceId::Kg]);
        manager.register(Arc::new(StubSearchSource {
            id: SourceId::Kw,
            behavior: SearchBehavior::Hang,
        }));
        manager.register(Arc::new(StubSearchSource {
            id: SourceId::Kg,
            behavior: SearchBehavior::Items(vec![candidate(
                "晴天(深情版)",
                "Lucky小爱",
                278,
                SourceId::Kg,
            )]),
        }));

        let started = std::time::Instant::now();
        let candidates = manager.find_music(&fallback_target()).await;
        let elapsed = started.elapsed();

        assert_eq!(candidates.len(), 1, "挂死的源被切断后仍应返回其它源的候选");
        assert!(
            elapsed < Duration::from_secs(1),
            "每源超时应当立即生效（虚拟时间），实际耗时 {elapsed:?}"
        );
    }

    /// 锁定跨平台匹配的严格性（本次不做任何放宽）：
    /// 同名不同歌手、同名同歌手但时长差 ≥5 秒都不能顶替。
    #[tokio::test]
    async fn find_music_keeps_strict_matching() {
        let mut manager = SourceManager::new(SourceId::Kw, &[SourceId::Kw, SourceId::Kg]);
        manager.register(Arc::new(StubSearchSource {
            id: SourceId::Kw,
            behavior: SearchBehavior::Fail,
        }));
        manager.register(Arc::new(StubSearchSource {
            id: SourceId::Kg,
            behavior: SearchBehavior::Items(vec![
                // 原曲同名但歌手不同（"晴天" 也不等于 "晴天(深情版)"）
                candidate("晴天", "周杰伦", 269, SourceId::Kg),
                // 同名同歌手但时长差 ≥5 秒
                candidate("晴天(深情版)", "Lucky小爱", 400, SourceId::Kg),
                // 严格同曲
                candidate("晴天(深情版)", "Lucky小爱", 278, SourceId::Kg),
            ]),
        }));

        let candidates = manager.find_music(&fallback_target()).await;
        assert_eq!(
            candidates.len(),
            1,
            "只有严格同曲应当通过过滤，实际: {:?}",
            candidates
                .iter()
                .map(|c| format!("{} - {} {}s", c.name, c.singer, c.duration.as_secs()))
                .collect::<Vec<_>>()
        );
        assert_eq!(candidates[0].singer, "Lucky小爱");
        assert_eq!(candidates[0].duration, Duration::from_secs(278));
    }

    /// `find_music_on` 只搜指定平台，且不改变严格匹配规则。
    #[tokio::test]
    async fn find_music_on_restricts_the_platform_set() {
        let mut manager = SourceManager::new(SourceId::Tx, &[SourceId::Kw, SourceId::Kg]);
        manager.register(Arc::new(StubSearchSource {
            id: SourceId::Kw,
            behavior: SearchBehavior::Items(vec![candidate(
                "晴天(深情版)",
                "Lucky小爱",
                278,
                SourceId::Kw,
            )]),
        }));
        manager.register(Arc::new(StubSearchSource {
            id: SourceId::Kg,
            behavior: SearchBehavior::Items(vec![candidate(
                "晴天(深情版)",
                "Lucky小爱",
                278,
                SourceId::Kg,
            )]),
        }));

        // 不限定：两个平台都会命中
        let all = manager.find_music(&fallback_target()).await;
        assert_eq!(all.len(), 2, "两个平台各一条同曲");

        // 限定只用酷狗：酷我的候选必须消失
        let only_kg = manager
            .find_music_on(&fallback_target(), &[SourceId::Kg])
            .await;
        assert_eq!(only_kg.len(), 1);
        assert_eq!(only_kg[0].source, SourceId::Kg);

        // 限定到没有候选的平台：直接为空，不会回退到别的平台
        let only_tx = manager
            .find_music_on(&fallback_target(), &[SourceId::Tx])
            .await;
        assert!(only_tx.is_empty());
    }
}
