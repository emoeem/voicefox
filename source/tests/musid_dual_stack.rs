//! V1 双栈路由 / 回退计数 / 会话隔离测试（feature `musid-dli-adapter`）。
//!
//! 契约见 docs/MUSID_DLI_ADAPTER.md §11：
//! - 影子 map：`register_musid_source` 绝不覆盖内置注册，capabilities 聚合 /
//!   登录链路 / 健康检测不读 musid map；
//! - 替换模式：musid 先行，失败立即按旧路径重试并计 fallback；
//! - probe 影子模式：旧路径权威，恒展示旧路径结果，仅计数对照；
//! - `soda`/`local` 恒旧路径；bili 的面级账面由 V2 注册表行承载（切片 4
//!   起搜索/流地址路由、歌词 LegacyOnly）；未启用源不路由。
//!
//! 歌词面路由（替换 + probe 双跑）依赖歌词转换变体：相关用例内联在本文件
//! 尾部，逐个挂 `#[cfg(feature = "musid-dli-adapter-lyrics")]` 门。

#![cfg(feature = "musid-dli-adapter")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use lx_core::model::lyric::LyricData;
use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{
    FetchError, MusicSource, SearchError, SearchResult, SongUrl, SourceCapabilities,
};
use lx_source::manager::{MusidRouteConfig, SourceManager};
use lx_source::parity::{ParityFace, ParityOutcome};

// ---------------------------------------------------------------------------
// 可脚本化桩源（旧路径与 musid 影子源共用同一实现，按调用日志区分）
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum Script<T> {
    Value(T),
    Fail,
    Hang,
}

struct ScriptedSource {
    id: SourceId,
    name: &'static str,
    search: Script<Vec<SongInfo>>,
    url: Script<String>,
    lyric: Script<LyricData>,
    /// 参与的对账面（capabilities 诊断位用）。
    playlists: bool,
    calls: Arc<Mutex<Vec<String>>>,
}

impl ScriptedSource {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[async_trait]
impl MusicSource for ScriptedSource {
    fn id(&self) -> SourceId {
        self.id
    }

    fn name(&self) -> &str {
        self.name
    }

    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities {
            playlists: self.playlists,
            ..SourceCapabilities::default()
        }
    }

    async fn search(
        &self,
        _keyword: &str,
        _page: u32,
        _limit: u32,
    ) -> Result<SearchResult, SearchError> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!("{}/search", self.name));
        match &self.search {
            Script::Value(items) => Ok(SearchResult {
                items: items.clone(),
                total: items.len() as u32,
                has_more: false,
            }),
            Script::Fail => Err(SearchError::Other("stub search failure".to_string())),
            Script::Hang => {
                std::future::pending::<()>().await;
                unreachable!("Hang 分支永不返回")
            }
        }
    }

    async fn get_song_url(
        &self,
        _song: &SongInfo,
        quality: Quality,
    ) -> Result<SongUrl, FetchError> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!("{}/url", self.name));
        match &self.url {
            Script::Value(host) => Ok(SongUrl {
                url: format!("https://{host}/audio.mp3"),
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
            }),
            Script::Fail => Err(FetchError::NotFound),
            Script::Hang => {
                std::future::pending::<()>().await;
                unreachable!("Hang 分支永不返回")
            }
        }
    }

    async fn get_lyric(&self, _song: &SongInfo) -> Result<LyricData, FetchError> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!("{}/lyric", self.name));
        match &self.lyric {
            Script::Value(data) => Ok(data.clone()),
            Script::Fail => Err(FetchError::NotFound),
            Script::Hang => {
                std::future::pending::<()>().await;
                unreachable!("Hang 分支永不返回")
            }
        }
    }

    async fn get_cover_url(&self, _song: &SongInfo) -> Result<String, FetchError> {
        Err(FetchError::NotFound)
    }

    fn supported_qualities(&self) -> Vec<Quality> {
        vec![Quality::High320]
    }
}

// ---------------------------------------------------------------------------
// 装配助手
// ---------------------------------------------------------------------------

fn song_item(id: &str, source: SourceId, name: &str, singer: &str) -> SongInfo {
    let mut song = SongInfo::new(id.to_string(), source, name.to_string(), singer.to_string());
    song.duration = Duration::from_secs(180);
    song
}

fn lyric(text: &str) -> LyricData {
    LyricData {
        lyric: text.to_string(),
        ..LyricData::default()
    }
}

fn routes(stack: bool, search: bool, song_url: bool, lyric: bool, probe: bool) -> MusidRouteConfig {
    MusidRouteConfig {
        stack,
        search,
        song_url,
        lyric,
        parity_probe: probe,
    }
}

fn count_of(
    manager: &SourceManager,
    face: ParityFace,
    source: SourceId,
    outcome: ParityOutcome,
) -> u64 {
    manager
        .musid_parity_snapshot()
        .into_iter()
        .find(|record| record.face == face && record.source == source && record.outcome == outcome)
        .map(|record| record.count)
        .unwrap_or(0)
}

/// probe 影子对账在后台任务落账（评审 P4，计数最终一致）：轮询等待计数
/// 到达 `min`（桩源查询微秒级完成，真实等待上限 1s 只兜调度抖动）。
async fn settled_count(
    manager: &SourceManager,
    face: ParityFace,
    source: SourceId,
    outcome: ParityOutcome,
    min: u64,
) -> u64 {
    for _ in 0..200 {
        let count = count_of(manager, face, source, outcome);
        if count >= min {
            return count;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    count_of(manager, face, source, outcome)
}

/// 等待一个后台落账窗口后返回计数（用于断言"不计"）。
async fn settled_absence(
    manager: &SourceManager,
    face: ParityFace,
    source: SourceId,
    outcome: ParityOutcome,
) -> u64 {
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    count_of(manager, face, source, outcome)
}

/// 双栈接线：legacy 桩覆盖内置（`register` 覆盖式），musid 桩进影子 map。
fn wired_manager(
    enabled: &[SourceId],
    legacy: Arc<ScriptedSource>,
    musid: Arc<ScriptedSource>,
) -> SourceManager {
    let mut manager = SourceManager::new(SourceId::Kw, enabled);
    manager.register(legacy);
    manager.register_musid_source(musid);
    manager.set_musid_routes(routes(true, true, true, true, false));
    manager
}

fn musid_only_manager(enabled: &[SourceId], musid: Arc<ScriptedSource>) -> SourceManager {
    let manager = SourceManager::new(SourceId::Kw, enabled);
    manager.register_musid_source(musid);
    manager
}

const WY_ENABLED: &[SourceId] = &[SourceId::Wy];

// ---------------------------------------------------------------------------
// 总闸 / 路由判定
// ---------------------------------------------------------------------------

/// 总闸与各面开关全关（默认形态）：旧实现唯一生效，musid 桩零调用。
#[tokio::test]
async fn routes_default_off_keep_legacy_only() {
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Value(vec![song_item("L1", SourceId::Wy, "旧路", "歌手")]),
        url: Script::Value("legacy.example".to_string()),
        lyric: Script::Value(lyric("[00:01.00]legacy")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Value(vec![song_item("M1", SourceId::Wy, "双栈", "歌手")]),
        url: Script::Value("musid.example".to_string()),
        lyric: Script::Value(lyric("[00:01.00]musid")),
        playlists: true,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let mut manager = SourceManager::new(SourceId::Kw, WY_ENABLED);
    manager.register(legacy.clone());
    manager.register_musid_source(musid.clone());
    // 不调用 set_musid_routes：与默认构建同形（全 false）。
    manager.set_musid_routes(routes(false, false, false, false, false));

    let search = manager
        .search_scoped("kw", 1, 20, Some(SourceId::Wy))
        .await
        .expect("旧路径搜索");
    assert_eq!(search.items[0].name, "旧路");
    let url = manager
        .get_song_url(
            &song_item("L1", SourceId::Wy, "旧路", "歌手"),
            Quality::High320,
        )
        .await
        .expect("旧路径解析");
    assert_eq!(url.url, "https://legacy.example/audio.mp3");
    let lyric = manager
        .get_lyric(&song_item("L1", SourceId::Wy, "旧路", "歌手"))
        .await
        .expect("旧路径歌词");
    assert_eq!(lyric.lyric, "[00:01.00]legacy");

    assert!(musid.calls().is_empty(), "总闸关闭时 musid 零调用");
    assert!(
        manager.musid_parity_snapshot().is_empty(),
        "旧路径不产生任何 parity 计数"
    );
}

/// `soda`（不在 §1 命名表）恒旧路径；未启用的源不路由。bili 的面级账面在
/// V2 切片 4 起由注册表行承载（搜索/流地址路由、歌词 LegacyOnly）——
/// bili 的路由行为由本文件尾部 `v2_registry_bilibili_routes_search_and_song_url_but_never_lyric`
/// 承载，不再在本用例断言"恒旧路径"。
#[tokio::test]
async fn soda_and_disabled_sources_never_route_to_musid() {
    // soda 不在 §1 命名表：即使注册了影子源也走旧路径。
    let legacy_soda = Arc::new(ScriptedSource {
        id: SourceId::Soda,
        name: "legacy-soda",
        search: Script::Value(vec![song_item("S1", SourceId::Soda, "汽水", "歌手")]),
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid_soda = Arc::new(ScriptedSource {
        id: SourceId::Soda,
        name: "musid-soda",
        search: Script::Value(vec![song_item("S1", SourceId::Soda, "汽水", "歌手")]),
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = musid_only_manager(&[SourceId::Soda], musid_soda.clone());
    let mut manager = manager;
    manager.register(legacy_soda.clone());
    manager.set_musid_routes(routes(true, true, true, true, false));
    let result = manager
        .search_scoped("kw", 1, 20, Some(SourceId::Soda))
        .await
        .expect("soda 走旧路径");
    assert_eq!(result.items[0].name, "汽水");
    assert_eq!(
        musid_soda.calls(),
        Vec::<String>::new(),
        "soda 影子源零调用"
    );

    // 未启用的源：整体报「未启用」，影子源不参与。
    let musid_wy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Value(vec![]),
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = musid_only_manager(&[SourceId::Kw], musid_wy.clone());
    manager.set_musid_routes(routes(true, true, true, true, false));
    let error = manager
        .search_scoped("kw", 1, 20, Some(SourceId::Wy))
        .await
        .expect_err("wy 未启用");
    assert!(error.to_string().contains("未启用"));
    assert_eq!(musid_wy.calls(), Vec::<String>::new());
}

// ---------------------------------------------------------------------------
// 搜索面：替换模式 + 回退计数
// ---------------------------------------------------------------------------

/// 单源搜索路由到 musid；命中计 MusidOk，legacy 零调用。
#[tokio::test]
async fn search_single_routes_to_musid_and_counts_hit() {
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Value(vec![song_item("L1", SourceId::Wy, "旧路", "歌手")]),
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Value(vec![song_item("M1", SourceId::Wy, "双栈", "歌手")]),
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = wired_manager(WY_ENABLED, legacy.clone(), musid.clone());

    let result = manager
        .search_scoped("kw", 1, 20, Some(SourceId::Wy))
        .await
        .expect("musid 路径搜索");
    assert_eq!(result.items[0].name, "双栈");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Search,
            SourceId::Wy,
            ParityOutcome::MusidOk
        ),
        1
    );
    assert_eq!(musid.calls(), vec!["musid-wy/search".to_string()]);
    assert_eq!(legacy.calls(), Vec::<String>::new());
}

/// musid 失败 → 同请求立即按旧路径重试并计 MusidErrLegacyOk。
#[tokio::test]
async fn search_single_falls_back_to_legacy_and_counts() {
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Value(vec![song_item("L1", SourceId::Wy, "旧路", "歌手")]),
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = wired_manager(WY_ENABLED, legacy.clone(), musid.clone());

    let result = manager
        .search_scoped("kw", 1, 20, Some(SourceId::Wy))
        .await
        .expect("musid 失败后旧路径重试");
    assert_eq!(result.items[0].name, "旧路");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Search,
            SourceId::Wy,
            ParityOutcome::MusidErrLegacyOk
        ),
        1
    );
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Search,
            SourceId::Wy,
            ParityOutcome::MusidOk
        ),
        0
    );
    assert_eq!(
        musid.calls(),
        vec!["musid-wy/search".to_string()],
        "musid 先行一次"
    );
    assert_eq!(legacy.calls(), vec!["legacy-wy/search".to_string()]);
}

/// 两栈同请求都失败 → MusidErrLegacyErr，错误向上（与旧路径失败形态同形）。
#[tokio::test]
async fn search_double_failure_counts_both_err() {
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = wired_manager(WY_ENABLED, legacy, musid);

    let error = manager
        .search_scoped("kw", 1, 20, Some(SourceId::Wy))
        .await
        .expect_err("两栈皆败");
    assert!(error.to_string().contains("stub search failure"));
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Search,
            SourceId::Wy,
            ParityOutcome::MusidErrLegacyErr
        ),
        1
    );
}

/// 聚合搜索按源路由进同一 JoinSet：Wy 走 musid、Kw 走旧路径，产物合并。
#[tokio::test]
async fn search_all_routes_per_source_into_the_same_joinset() {
    let legacy_wy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Value(vec![song_item("L1", SourceId::Wy, "旧路", "歌手")]),
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let legacy_kw = Arc::new(ScriptedSource {
        id: SourceId::Kw,
        name: "legacy-kw",
        search: Script::Value(vec![song_item("L2", SourceId::Kw, "酷我", "歌手")]),
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid_wy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Value(vec![song_item("M1", SourceId::Wy, "双栈", "歌手")]),
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let mut manager = SourceManager::new(SourceId::Kw, &[SourceId::Wy, SourceId::Kw]);
    manager.register(legacy_wy.clone());
    manager.register(legacy_kw.clone());
    manager.register_musid_source(musid_wy.clone());
    manager.set_musid_routes(routes(true, true, true, true, false));

    let result = manager.search_all("kw", 1, 20).await.expect("聚合搜索");
    let names: Vec<&str> = result.items.iter().map(|song| song.name.as_str()).collect();
    assert!(names.contains(&"双栈"), "musid 产物进聚合结果: {names:?}");
    assert!(names.contains(&"酷我"), "旧路径产物进聚合结果: {names:?}");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Search,
            SourceId::Wy,
            ParityOutcome::MusidOk
        ),
        1
    );
    assert_eq!(musid_wy.calls(), vec!["musid-wy/search".to_string()]);
    assert_eq!(legacy_wy.calls(), Vec::<String>::new());
    assert_eq!(legacy_kw.calls(), vec!["legacy-kw/search".to_string()]);
}

/// 聚合内 musid 失败 → 立即按旧路径重试该源并计 MusidErrLegacyOk。
#[tokio::test]
async fn search_all_musid_failure_falls_back_within_the_aggregation() {
    let legacy_wy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Value(vec![song_item("L1", SourceId::Wy, "旧路", "歌手")]),
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid_wy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let mut manager = SourceManager::new(SourceId::Kw, WY_ENABLED);
    manager.register(legacy_wy.clone());
    manager.register_musid_source(musid_wy.clone());
    manager.set_musid_routes(routes(true, true, true, true, false));

    let result = manager.search_all("kw", 1, 20).await.expect("回退后有产物");
    assert_eq!(result.items[0].name, "旧路");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Search,
            SourceId::Wy,
            ParityOutcome::MusidErrLegacyOk
        ),
        1
    );
    assert_eq!(musid_wy.calls(), vec!["musid-wy/search".to_string()]);
    assert_eq!(legacy_wy.calls(), vec!["legacy-wy/search".to_string()]);
}

/// musid 尝试预算（5s）耗尽也回退并计 MusidErrLegacyOk——musid 慢失败/
/// 挂死不再把回退挤到归零（评审 P2）。
#[tokio::test(start_paused = true)]
async fn search_all_musid_hang_falls_back_within_the_budget_split() {
    let legacy_wy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Value(vec![song_item("L1", SourceId::Wy, "旧路", "歌手")]),
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid_wy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Hang,
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let mut manager = SourceManager::new(SourceId::Kw, WY_ENABLED);
    manager.register(legacy_wy.clone());
    manager.register_musid_source(musid_wy);
    manager.set_musid_routes(routes(true, true, true, true, false));

    let result = manager
        .search_all("kw", 1, 20)
        .await
        .expect("回退继承剩余预算");
    assert_eq!(result.items[0].name, "旧路");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Search,
            SourceId::Wy,
            ParityOutcome::MusidErrLegacyOk
        ),
        1,
        "musid 尝试预算耗尽后回退，双方结局齐全 → 计 ErrLegacyOk"
    );
    assert_eq!(
        legacy_wy.calls(),
        vec!["legacy-wy/search".to_string()],
        "回退确实执行了"
    );
}

/// 两栈（musid 尝试 + 回退）都在每源预算内挂死 → 整体超时不计（无双方结局）。
#[tokio::test(start_paused = true)]
async fn search_all_both_stacks_timing_out_counts_nothing() {
    let legacy_wy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Hang,
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid_wy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Hang,
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let mut manager = SourceManager::new(SourceId::Kw, WY_ENABLED);
    manager.register(legacy_wy);
    manager.register_musid_source(musid_wy);
    manager.set_musid_routes(routes(true, true, true, true, false));

    let error = manager.search_all("kw", 1, 20).await.expect_err("整体超时");
    assert!(error.to_string().contains("请求超时"));
    assert!(manager.musid_parity_snapshot().is_empty(), "整体超时不计");
}

/// 单源替换路径的预算切分（评审 P2）：musid 挂死耗尽尝试预算后仍回退。
#[tokio::test(start_paused = true)]
async fn search_single_musid_hang_falls_back_after_the_attempt_budget() {
    let legacy_wy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Value(vec![song_item("L1", SourceId::Wy, "旧路", "歌手")]),
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid_wy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Hang,
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let mut manager = SourceManager::new(SourceId::Kw, WY_ENABLED);
    manager.register(legacy_wy.clone());
    manager.register_musid_source(musid_wy);
    manager.set_musid_routes(routes(true, true, true, true, false));

    let result = manager
        .search_scoped("kw", 1, 20, Some(SourceId::Wy))
        .await
        .expect("musid 预算耗尽后旧路径重试");
    assert_eq!(result.items[0].name, "旧路");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Search,
            SourceId::Wy,
            ParityOutcome::MusidErrLegacyOk
        ),
        1
    );
    assert_eq!(legacy_wy.calls(), vec!["legacy-wy/search".to_string()]);
}

// ---------------------------------------------------------------------------
// 流地址面：替换模式 + js_first 语义 + 回退计数
// ---------------------------------------------------------------------------

fn wy_song() -> SongInfo {
    song_item("M1", SourceId::Wy, "双栈", "歌手")
}

#[tokio::test]
async fn song_url_slot_precedes_builtin_and_falls_back() {
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Fail,
        url: Script::Value("legacy.example".to_string()),
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    // a. musid 命中：内置零调用，计 MusidOk。
    let musid_hit = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Fail,
        url: Script::Value("musid.example".to_string()),
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = wired_manager(WY_ENABLED, legacy.clone(), musid_hit.clone());
    let url = manager
        .get_song_url(&wy_song(), Quality::High320)
        .await
        .expect("musid 解析");
    assert_eq!(url.url, "https://musid.example/audio.mp3");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::SongUrl,
            SourceId::Wy,
            ParityOutcome::MusidOk
        ),
        1
    );
    assert_eq!(musid_hit.calls(), vec!["musid-wy/url".to_string()]);
    assert_eq!(legacy.calls(), Vec::<String>::new());

    // b. musid 失败 → 既有内置先行链兜底，计 MusidErrLegacyOk。
    let musid_fail = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = wired_manager(WY_ENABLED, legacy.clone(), musid_fail.clone());
    let url = manager
        .get_song_url(&wy_song(), Quality::High320)
        .await
        .expect("内置兜底");
    assert_eq!(url.url, "https://legacy.example/audio.mp3");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::SongUrl,
            SourceId::Wy,
            ParityOutcome::MusidErrLegacyOk
        ),
        1
    );
    assert_eq!(musid_fail.calls(), vec!["musid-wy/url".to_string()]);
    assert_eq!(legacy.calls(), vec!["legacy-wy/url".to_string()]);

    // c. 两栈皆败 → MusidErrLegacyErr，错误向上。
    let legacy_fail = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = wired_manager(WY_ENABLED, legacy_fail, musid_fail.clone());
    let error = manager
        .get_song_url(&wy_song(), Quality::High320)
        .await
        .expect_err("两栈皆败");
    assert!(error.to_string().contains("not found"));
    assert_eq!(
        count_of(
            &manager,
            ParityFace::SongUrl,
            SourceId::Wy,
            ParityOutcome::MusidErrLegacyErr
        ),
        1
    );
}

/// js_first 语义不变：true=JS 先于 musid；JS 成功时 musid 零调用。
#[tokio::test]
async fn song_url_js_first_keeps_js_precedence_over_musid() {
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Fail,
        url: Script::Value("legacy.example".to_string()),
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Fail,
        url: Script::Value("musid.example".to_string()),
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = wired_manager(WY_ENABLED, legacy.clone(), musid.clone());
    manager.set_musid_routes(routes(true, true, true, true, false));
    manager.set_js_url_first(true);
    let generation = manager.begin_js_source_request(true);
    let js = Arc::new(ScriptedSource {
        id: SourceId::Kw,
        name: "js-juhe",
        search: Script::Fail,
        url: Script::Value("js.example".to_string()),
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    assert!(manager.set_js_source_if_current(generation, js.clone()));

    let url = manager
        .get_song_url(&wy_song(), Quality::High320)
        .await
        .expect("JS 先行");
    assert_eq!(url.url, "https://js.example/audio.mp3");
    assert_eq!(
        musid.calls(),
        Vec::<String>::new(),
        "JS 命中后 musid 零调用"
    );

    // JS 失败 → musid 在内置兜底之前接手。
    let js_fail = Arc::new(ScriptedSource {
        id: SourceId::Kw,
        name: "js-juhe",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    assert!(manager.set_js_source_if_current(generation, js_fail));
    let url = manager
        .get_song_url(&wy_song(), Quality::High320)
        .await
        .expect("JS 失败后 musid 接手");
    assert_eq!(url.url, "https://musid.example/audio.mp3");
    assert_eq!(musid.calls(), vec!["musid-wy/url".to_string()]);
    assert_eq!(
        legacy.calls(),
        Vec::<String>::new(),
        "musid 命中后内置零调用"
    );
}

// ---------------------------------------------------------------------------
// probe 影子模式（搜索 + 流地址；歌词双跑在 lyrics 变体文件）
// ---------------------------------------------------------------------------

/// probe 搜索：恒展示旧路径结果；双 Ok 归一相等 → ParityMatch。
#[tokio::test]
async fn parity_probe_search_is_shadow_only_and_counts_match() {
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Value(vec![song_item("M1", SourceId::Wy, "晴天", "周杰伦")]),
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Value(vec![song_item("M1", SourceId::Wy, "晴天", "周杰伦")]),
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let mut manager = SourceManager::new(SourceId::Kw, WY_ENABLED);
    manager.register(legacy.clone());
    manager.register_musid_source(musid.clone());
    // probe 开、面开关关：影子参与但旧路径权威。
    manager.set_musid_routes(routes(true, false, false, false, true));

    let result = manager
        .search_scoped("kw", 1, 20, Some(SourceId::Wy))
        .await
        .expect("旧路径结果");
    assert_eq!(result.items.len(), 1);
    assert_eq!(
        settled_count(
            &manager,
            ParityFace::Search,
            SourceId::Wy,
            ParityOutcome::ParityMatch,
            1
        )
        .await,
        1
    );
    assert_eq!(musid.calls(), vec!["musid-wy/search".to_string()]);
    assert_eq!(legacy.calls(), vec!["legacy-wy/search".to_string()]);

    // 归一不等（同名不同歌手）→ ParityMismatch，结果仍是旧路径产物。
    let musid_mismatch = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Value(vec![song_item("M1", SourceId::Wy, "晴天", "费玉清")]),
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let mut manager = SourceManager::new(SourceId::Kw, WY_ENABLED);
    manager.register(legacy.clone());
    manager.register_musid_source(musid_mismatch);
    manager.set_musid_routes(routes(true, false, false, false, true));
    let result = manager
        .search_scoped("kw", 1, 20, Some(SourceId::Wy))
        .await
        .expect("旧路径结果");
    assert_eq!(result.items[0].name, "晴天");
    assert_eq!(
        settled_count(
            &manager,
            ParityFace::Search,
            SourceId::Wy,
            ParityOutcome::ParityMismatch,
            1
        )
        .await,
        1
    );
}

/// probe 搜索：单侧瞬时 Err 不作对账结论；双 Err 记 MusidErrLegacyErr。
#[tokio::test]
async fn parity_probe_search_single_sided_errors_are_not_counted() {
    // legacy Ok + musid Err：无对照产物，不计。
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Value(vec![song_item("L1", SourceId::Wy, "旧路", "歌手")]),
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let mut manager = SourceManager::new(SourceId::Kw, WY_ENABLED);
    manager.register(legacy.clone());
    manager.register_musid_source(musid.clone());
    manager.set_musid_routes(routes(true, false, false, false, true));
    let result = manager
        .search_scoped("kw", 1, 20, Some(SourceId::Wy))
        .await
        .expect("旧路径结果");
    assert_eq!(result.items[0].name, "旧路");
    assert_eq!(
        settled_absence(
            &manager,
            ParityFace::Search,
            SourceId::Wy,
            ParityOutcome::ParityMatch
        )
        .await,
        0,
        "单侧 Err 不计"
    );
    assert_eq!(
        settled_absence(
            &manager,
            ParityFace::Search,
            SourceId::Wy,
            ParityOutcome::ParityMismatch
        )
        .await,
        0
    );

    // 双 Err：记 MusidErrLegacyErr，错误仍向上（旧路径形态）。
    let legacy_fail = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let mut manager = SourceManager::new(SourceId::Kw, WY_ENABLED);
    manager.register(legacy_fail);
    manager.register_musid_source(musid.clone());
    manager.set_musid_routes(routes(true, false, false, false, true));
    assert!(
        manager
            .search_scoped("kw", 1, 20, Some(SourceId::Wy))
            .await
            .is_err()
    );
    assert_eq!(
        settled_count(
            &manager,
            ParityFace::Search,
            SourceId::Wy,
            ParityOutcome::MusidErrLegacyErr,
            1
        )
        .await,
        1
    );
}

/// probe 流地址面（评审 P1：恒纯观测）：旧链成功不双跑 resolve（零影子
/// 调用）；旧链失败才跑 musid——只计数（命中记 MusidOk），**不返回 musid
/// 地址**，旧错误原样向上（与不开 probe 时一致）。
#[tokio::test]
async fn probe_song_url_runs_musid_only_after_the_legacy_chain_fails() {
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Fail,
        url: Script::Value("legacy.example".to_string()),
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Fail,
        url: Script::Value("musid.example".to_string()),
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let mut manager = SourceManager::new(SourceId::Kw, WY_ENABLED);
    manager.register(legacy.clone());
    manager.register_musid_source(musid.clone());
    manager.set_musid_routes(routes(true, false, false, false, true));

    // 旧链成功：musid 零调用（避免双倍平台配额），零计数。
    let url = manager
        .get_song_url(&wy_song(), Quality::High320)
        .await
        .expect("旧链解析");
    assert_eq!(url.url, "https://legacy.example/audio.mp3");
    assert_eq!(musid.calls(), Vec::<String>::new());
    assert!(manager.musid_parity_snapshot().is_empty());

    // 旧链失败：musid 只计数（命中记 MusidOk），musid 地址不进用户路径，
    // 旧错误原样向上（评审 P1）。
    let legacy_fail = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let mut manager = SourceManager::new(SourceId::Kw, WY_ENABLED);
    manager.register(legacy_fail);
    manager.register_musid_source(musid.clone());
    manager.set_musid_routes(routes(true, false, false, false, true));
    let error = manager
        .get_song_url(&wy_song(), Quality::High320)
        .await
        .expect_err("纯观测：旧错误向上，musid 地址不兜底");
    assert!(error.to_string().contains("not found"));
    assert_eq!(
        settled_count(
            &manager,
            ParityFace::SongUrl,
            SourceId::Wy,
            ParityOutcome::MusidOk,
            1
        )
        .await,
        1,
        "musid 确实跑了失败侧对账（后台落账，评审 F1），但产物未透传"
    );
    assert_eq!(
        musid.calls(),
        vec!["musid-wy/url".to_string()],
        "失败侧对账确实调用了 musid"
    );

    // 两栈皆败：MusidErrLegacyErr，旧错误向上。
    let musid_fail = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let legacy_fail = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let mut manager = SourceManager::new(SourceId::Kw, WY_ENABLED);
    manager.register(legacy_fail);
    manager.register_musid_source(musid_fail);
    manager.set_musid_routes(routes(true, false, false, false, true));
    assert!(
        manager
            .get_song_url(&wy_song(), Quality::High320)
            .await
            .is_err()
    );
    assert_eq!(
        settled_count(
            &manager,
            ParityFace::SongUrl,
            SourceId::Wy,
            ParityOutcome::MusidErrLegacyErr,
            1
        )
        .await,
        1
    );
}

/// 流地址面失败侧对账不阻塞错误路径（评审 F1）：musid resolve 挂死时，
/// 旧错误立即向上（时延与不开 probe 一致），对账在后台按 10s 预算落账。
#[tokio::test(start_paused = true)]
async fn probe_song_url_reconcile_does_not_block_the_error_path() {
    let legacy_fail = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid_hang = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Fail,
        url: Script::Hang,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let mut manager = SourceManager::new(SourceId::Kw, WY_ENABLED);
    manager.register(legacy_fail);
    manager.register_musid_source(musid_hang.clone());
    manager.set_musid_routes(routes(true, false, false, false, true));

    let started = std::time::Instant::now();
    let error = manager
        .get_song_url(&wy_song(), Quality::High320)
        .await
        .expect_err("对账不阻塞错误路径");
    let elapsed = started.elapsed();
    assert!(error.to_string().contains("not found"));
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "挂死的对账 resolve 不得推迟错误返回（内联实现会等满 10s 预算），实际 {elapsed:?}"
    );
    // 虚拟时间越过对账预算：后台落账 MusidErrLegacyErr（取消语义评审 F5：
    // 用户已拿到的错误不影响影子照常消耗配额并落账）。
    tokio::time::sleep(std::time::Duration::from_secs(11)).await;
    assert_eq!(
        count_of(
            &manager,
            ParityFace::SongUrl,
            SourceId::Wy,
            ParityOutcome::MusidErrLegacyErr
        ),
        1
    );
    assert_eq!(
        musid_hang.calls(),
        vec!["musid-wy/url".to_string()],
        "失败侧对账确实调用了 musid"
    );
}

// ---------------------------------------------------------------------------
// 会话隔离（P10：V1 登录/会话四面全部走旧路径）
// ---------------------------------------------------------------------------

/// 影子注册绝不触碰内置状态：capabilities 聚合、`get`、登录位、入口显隐
/// 全部仍是内置实现；musid 适配源自身的登录面恒"未登录 / 默认错误"。
#[tokio::test]
async fn musid_registration_never_touches_builtin_state() {
    let manager = SourceManager::new(SourceId::Kw, WY_ENABLED);
    let builtin_caps = manager.capabilities(SourceId::Wy);
    let builtin = manager.get(SourceId::Wy).expect("内置 wy 已注册");
    let builtin_logged_in = manager.is_logged_in(SourceId::Wy);
    let leaderboard_before = manager.leaderboard_sources();
    let playlist_before = manager.playlist_sources();

    // 带独立 capabilities 位（playlists=true）的影子源：若 capabilities 聚合
    // 误读影子 map，这里的 playlists 会翻真。
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: true,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    manager.register_musid_source(musid.clone());
    manager.set_musid_routes(routes(true, true, true, true, false));

    assert_eq!(
        manager.capabilities(SourceId::Wy),
        builtin_caps,
        "capabilities 聚合不读影子 map（影子源带 playlists=true，若误读必然翻真）"
    );
    assert!(
        Arc::ptr_eq(&manager.get(SourceId::Wy).expect("内置 wy"), &builtin),
        "get 返回的仍是内置实现"
    );
    assert_eq!(
        manager.is_logged_in(SourceId::Wy),
        builtin_logged_in,
        "登录位不读影子 map"
    );
    assert_eq!(manager.leaderboard_sources(), leaderboard_before);
    assert_eq!(manager.playlist_sources(), playlist_before);

    // 影子源自身的登录/会话面：恒未登录、默认错误（QR/直解不覆写）。
    assert!(!musid.is_logged_in(), "musid 影子源恒未登录");
    assert!(musid.logout().is_ok(), "logout 无会话可清");
    assert!(musid.create_qr_login().await.is_err(), "扫码走旧路径");
    assert!(
        musid.check_qr_login("key").await.is_err(),
        "扫码轮询走旧路径"
    );
    assert!(
        musid
            .parse_link("https://music.163.com/song?id=1")
            .await
            .is_err(),
        "链接直解走旧路径"
    );
}

/// 影子源注册不动歌词负缓存键与 JS 源状态（同一首歌的负缓存语义与
/// 关闭双栈时逐字节一致）。
#[tokio::test]
async fn musid_registration_keeps_negative_cache_semantics() {
    // 无词歌曲（两栈皆空）→ NotFound + 负缓存写入；影子源注册与否不改变
    // 返回形态。
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Value(LyricData::default()),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Value(LyricData::default()),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    // 歌词面路由依赖歌词转换变体（本文件未启用）：影子源注册后歌词面
    // 仍整体走旧路径——这正是「缺变体不生效」的默认构建形态。
    let manager = wired_manager(WY_ENABLED, legacy.clone(), musid.clone());
    let song = song_item("M1", SourceId::Wy, "双栈", "歌手");
    let error = manager
        .get_lyric_with_fallback(&song)
        .await
        .expect_err("两栈皆无词 → NotFound");
    assert!(matches!(error, FetchError::NotFound));
    // 负缓存命中：第二次直接 NotFound（musid 不再被调用）。
    let calls_before = musid.calls();
    let error = manager
        .get_lyric_with_fallback(&song)
        .await
        .expect_err("负缓存命中");
    assert!(matches!(error, FetchError::NotFound));
    assert_eq!(musid.calls(), calls_before, "负缓存命中后影子源零调用");
}

// ---------------------------------------------------------------------------
// 歌词面：替换模式 + probe 双跑（需要 musid-dli-adapter-lyrics 歌词转换变体）
// ---------------------------------------------------------------------------

/// 歌词槽位与 JS 槽位同位：JS 之后、内置 fallback 之前；musid 命中计
/// MusidOk（走 get_lyric_with_fallback 的主请求归因口径）。
#[cfg(feature = "musid-dli-adapter-lyrics")]
#[tokio::test]
async fn lyric_slot_runs_after_js_and_before_builtin() {
    let js = Arc::new(ScriptedSource {
        id: SourceId::Kw,
        name: "js-juhe",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Value(lyric("[00:01.00]legacy")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid_hit = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Value(lyric("[00:01.00]musid")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = wired_manager(WY_ENABLED, legacy.clone(), musid_hit.clone());
    let generation = manager.begin_js_source_request(true);
    assert!(manager.set_js_source_if_current(generation, js.clone()));
    let song = song_item("M1", SourceId::Wy, "双栈", "歌手");

    let data = manager
        .get_lyric_with_fallback(&song)
        .await
        .expect("musid 歌词");
    assert_eq!(data.lyric, "[00:01.00]musid");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Lyric,
            SourceId::Wy,
            ParityOutcome::MusidOk
        ),
        1
    );
    assert_eq!(
        js.calls(),
        vec!["js-juhe/lyric".to_string()],
        "JS 槽位先于 musid"
    );
    assert_eq!(
        legacy.calls(),
        Vec::<String>::new(),
        "musid 命中后内置零调用"
    );

    // musid 空产出 → 内置 fallback 兜底，计 MusidErrLegacyOk。
    let musid_empty = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Value(LyricData::default()),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = wired_manager(WY_ENABLED, legacy.clone(), musid_empty.clone());
    let generation = manager.begin_js_source_request(true);
    assert!(manager.set_js_source_if_current(generation, js.clone()));
    let data = manager
        .get_lyric_with_fallback(&song)
        .await
        .expect("内置兜底");
    assert_eq!(data.lyric, "[00:01.00]legacy");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Lyric,
            SourceId::Wy,
            ParityOutcome::MusidErrLegacyOk
        ),
        1
    );

    // 两栈皆无词 → MusidErrLegacyErr + 负缓存（同键）。
    let legacy_empty = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Value(LyricData::default()),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = wired_manager(WY_ENABLED, legacy_empty, musid_empty.clone());
    let generation = manager.begin_js_source_request(true);
    assert!(manager.set_js_source_if_current(generation, js.clone()));
    let error = manager
        .get_lyric_with_fallback(&song)
        .await
        .expect_err("两栈皆无词");
    assert!(matches!(error, FetchError::NotFound));
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Lyric,
            SourceId::Wy,
            ParityOutcome::MusidErrLegacyErr
        ),
        1
    );
}

/// 歌词 probe 双跑：同请求两栈各取一次词，恒展示旧路径结果；双 Ok 内容
/// 归一相等 → ParityMatch；单侧有词 → ParityMismatch；单侧 Err 不计。
#[cfg(feature = "musid-dli-adapter-lyrics")]
#[tokio::test]
async fn lyric_probe_dual_runs_and_counts_verdicts() {
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Value(lyric("[00:01.00]shared\n[00:05.00]text")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Value(lyric("[00:01.00] shared \n[00:05.00]text")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let mut manager = SourceManager::new(SourceId::Kw, WY_ENABLED);
    manager.register(legacy.clone());
    manager.register_musid_source(musid.clone());
    manager.set_musid_routes(routes(true, false, false, false, true));
    let song = song_item("M1", SourceId::Wy, "双栈", "歌手");

    let data = manager
        .get_lyric_with_fallback(&song)
        .await
        .expect("旧路径权威结果");
    assert_eq!(data.lyric, "[00:01.00]shared\n[00:05.00]text");
    assert_eq!(
        settled_count(
            &manager,
            ParityFace::Lyric,
            SourceId::Wy,
            ParityOutcome::ParityMatch,
            1
        )
        .await,
        1,
        "影子后台对账落账（评审 P4，最终一致）"
    );
    assert_eq!(
        legacy.calls(),
        vec!["legacy-wy/lyric".to_string()],
        "旧路径取词一次"
    );
    assert_eq!(musid.calls(), vec!["musid-wy/lyric".to_string()]);

    // 单侧有词（musid 无词而旧路径有词）→ ParityMismatch，结果仍是旧路径。
    let musid_empty = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Value(LyricData::default()),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let mut manager = SourceManager::new(SourceId::Kw, WY_ENABLED);
    manager.register(legacy.clone());
    manager.register_musid_source(musid_empty);
    manager.set_musid_routes(routes(true, false, false, false, true));
    let data = manager
        .get_lyric_with_fallback(&song)
        .await
        .expect("旧路径权威结果");
    assert_eq!(data.lyric, "[00:01.00]shared\n[00:05.00]text");
    assert_eq!(
        settled_count(
            &manager,
            ParityFace::Lyric,
            SourceId::Wy,
            ParityOutcome::ParityMismatch,
            1
        )
        .await,
        1
    );

    // 旧路径 Err + musid 有词：单侧瞬时 Err 不作结论，不计；结果仍是错误。
    let legacy_fail = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid_ok = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Value(lyric("[00:01.00]musid")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let mut manager = SourceManager::new(SourceId::Kw, WY_ENABLED);
    manager.register(legacy_fail);
    manager.register_musid_source(musid_ok.clone());
    manager.set_musid_routes(routes(true, false, false, false, true));
    assert!(manager.get_lyric_with_fallback(&song).await.is_err());
    assert_eq!(
        settled_absence(
            &manager,
            ParityFace::Lyric,
            SourceId::Wy,
            ParityOutcome::ParityMismatch
        )
        .await,
        0,
        "单侧 Err 不计"
    );
    assert_eq!(
        settled_absence(
            &manager,
            ParityFace::Lyric,
            SourceId::Wy,
            ParityOutcome::ParityMatch
        )
        .await,
        0
    );
}

/// probe 影子不阻塞用户路径（评审 P4）：musid 影子挂死时，单源搜索仍以
/// 旧路径时延返回（对账在后台任务，计数最终一致或缺失）。
#[tokio::test]
async fn probe_shadow_does_not_block_the_user_path() {
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Value(vec![song_item("L1", SourceId::Wy, "旧路", "歌手")]),
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Hang,
        url: Script::Fail,
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let mut manager = SourceManager::new(SourceId::Kw, WY_ENABLED);
    manager.register(legacy);
    manager.register_musid_source(musid);
    manager.set_musid_routes(routes(true, false, false, false, true));

    let started = std::time::Instant::now();
    let result = manager
        .search_scoped("kw", 1, 20, Some(SourceId::Wy))
        .await
        .expect("旧路径结果");
    let elapsed = started.elapsed();
    assert_eq!(result.items[0].name, "旧路");
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "挂死的影子不得拖慢用户路径（join! 旧实现会等满影子 10s），实际 {elapsed:?}"
    );
}

/// 歌词 probe 双方都确认无词 → ParityMatch（无词一致性也是对账结论）。
#[cfg(feature = "musid-dli-adapter-lyrics")]
#[tokio::test]
async fn lyric_probe_both_empty_counts_as_match() {
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Value(LyricData::default()),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Value(LyricData::default()),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let mut manager = SourceManager::new(SourceId::Kw, WY_ENABLED);
    manager.register(legacy);
    manager.register_musid_source(musid);
    manager.set_musid_routes(routes(true, false, false, false, true));
    let song = song_item("M1", SourceId::Wy, "双栈", "歌手");

    let error = manager
        .get_lyric_with_fallback(&song)
        .await
        .expect_err("双方皆无词");
    assert!(matches!(error, FetchError::NotFound));
    assert_eq!(
        settled_count(
            &manager,
            ParityFace::Lyric,
            SourceId::Wy,
            ParityOutcome::ParityMatch,
            1
        )
        .await,
        1
    );
}

/// 歌词 err 侧归因只看主请求链（评审 P7）：musid 槽位无产出、主请求链
/// 无词、词由 find_music 跨源候选（另一平台）补到 → 不计 MusidErrLegacyOk
/// /MusidErrLegacyErr（跨平台产物不算"旧路径同请求成功"）。
#[cfg(feature = "musid-dli-adapter-lyrics")]
#[tokio::test]
async fn lyric_cross_source_rescue_is_not_attributed_to_the_routed_source() {
    let legacy_wy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Value(LyricData::default()),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let legacy_kg = Arc::new(ScriptedSource {
        id: SourceId::Kg,
        name: "legacy-kg",
        search: Script::Value(vec![song_item("K1", SourceId::Kg, "双栈", "歌手")]),
        url: Script::Fail,
        lyric: Script::Value(lyric("[00:01.00]kg-candidate")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid_wy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Value(LyricData::default()),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let mut manager = SourceManager::new(SourceId::Kw, &[SourceId::Wy, SourceId::Kg]);
    manager.register(legacy_wy.clone());
    manager.register(legacy_kg.clone());
    manager.register_musid_source(musid_wy.clone());
    manager.set_musid_routes(routes(true, true, true, true, false));

    let song = song_item("W1", SourceId::Wy, "双栈", "歌手");
    let data = manager
        .get_lyric_with_fallback(&song)
        .await
        .expect("跨源候选补到词");
    assert_eq!(data.lyric, "[00:01.00]kg-candidate");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Lyric,
            SourceId::Wy,
            ParityOutcome::MusidErrLegacyOk
        ),
        0,
        "跨源 rescue 不归因到路由源平台（评审 P7）"
    );
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Lyric,
            SourceId::Wy,
            ParityOutcome::MusidErrLegacyErr
        ),
        0
    );
    // 主请求链确实走过：musid 槽位 + 内置 wy 都被调用且都无词。
    assert!(musid_wy.calls().contains(&"musid-wy/lyric".to_string()));
    assert!(legacy_wy.calls().contains(&"legacy-wy/lyric".to_string()));
}

// ---------------------------------------------------------------------------
// V2 逐源路由注册表（docs/MUSID_DLI_ADAPTER.md §12；jamendo = 切片 1 模板）
// ---------------------------------------------------------------------------
// 上方 V1 用例以 Wy 为载体继续全绿 = 注册表放行行为保持的回归 pin（切片
// 9 起 Wy 三面账面为 Migrated，放行结论与 V1Routed 完全一致——运行行为
// 零变化，V2 只补对账与登记）。

/// jamendo（切片 1）：搜索面 + 流地址面（Migrated）路由到 musid；歌词面
/// （LegacyOnly，musid 歌词 stub 的 Other 不许污染旧路径 NotFound）恒旧
/// 路径——即便 musid 影子源"有词"也不参与。
#[tokio::test]
async fn v2_registry_jamendo_routes_search_and_song_url_but_never_lyric() {
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Jamendo,
        name: "legacy-jamendo",
        search: Script::Value(vec![song_item("J1", SourceId::Jamendo, "旧路", "歌手")]),
        url: Script::Value("legacy.example".to_string()),
        // 与真实旧路径同形：jamendo/mod.rs:421-424 恒 FetchError::NotFound。
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Jamendo,
        name: "musid-jamendo",
        search: Script::Value(vec![song_item("J1", SourceId::Jamendo, "双栈", "歌手")]),
        url: Script::Value("musid.example".to_string()),
        // 若注册表歌词门失效，这个"有词"的 stub 会被歌词槽位命中。
        lyric: Script::Value(lyric("[00:01.00]musid-stub")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = wired_manager(&[SourceId::Jamendo], legacy.clone(), musid.clone());

    // 搜索面（Migrated）：musid 先行命中，计 MusidOk。
    let result = manager
        .search_scoped("kw", 1, 20, Some(SourceId::Jamendo))
        .await
        .expect("musid 路径搜索");
    assert_eq!(result.items[0].name, "双栈");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Search,
            SourceId::Jamendo,
            ParityOutcome::MusidOk
        ),
        1
    );

    // 流地址面（Migrated）：musid 命中。
    let url = manager
        .get_song_url(
            &song_item("J1", SourceId::Jamendo, "双栈", "歌手"),
            Quality::Low128,
        )
        .await
        .expect("musid 解析");
    assert_eq!(url.url, "https://musid.example/audio.mp3");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::SongUrl,
            SourceId::Jamendo,
            ParityOutcome::MusidOk
        ),
        1
    );

    // 歌词面（LegacyOnly）：用户拿到旧路径 NotFound，musid stub 的
    // "musid-stub" 歌词与"该音源不支持"错误都到不了用户；影子源歌词面
    // 零调用。
    let error = manager
        .get_lyric_with_fallback(&song_item("J1", SourceId::Jamendo, "双栈", "歌手"))
        .await
        .expect_err("歌词面恒旧路径");
    assert!(matches!(error, FetchError::NotFound));
    assert!(
        !musid.calls().contains(&"musid-jamendo/lyric".to_string()),
        "注册表拒绝后 musid 歌词面零调用（probe 影子同门），实际: {:?}",
        musid.calls()
    );
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Lyric,
            SourceId::Jamendo,
            ParityOutcome::MusidOk
        ),
        0
    );
    assert_eq!(
        musid.calls(),
        vec![
            "musid-jamendo/search".to_string(),
            "musid-jamendo/url".to_string()
        ],
        "musid 只参与搜索/流地址两面，歌词面（注册表拒绝）零调用"
    );
    assert!(
        legacy.calls().contains(&"legacy-jamendo/lyric".to_string()),
        "歌词走旧路径（find_music 跨源 rescue 会再触发同源 search，故按 contains 断言）"
    );
}

/// fivesing（切片 2）：搜索面 + 流地址面 Migrated——musid 先行命中、旧链
/// 零调用（与 jamendo 同形；歌词面在下方歌词门用例）。
#[tokio::test]
async fn v2_registry_fivesing_routes_search_and_song_url() {
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Fivesing,
        name: "legacy-fivesing",
        search: Script::Value(vec![song_item("F1", SourceId::Fivesing, "旧路", "歌手")]),
        url: Script::Value("legacy.example".to_string()),
        lyric: Script::Value(lyric("[00:01.00]legacy")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Fivesing,
        name: "musid-fivesing",
        search: Script::Value(vec![song_item("F1", SourceId::Fivesing, "双栈", "歌手")]),
        url: Script::Value("musid.example".to_string()),
        lyric: Script::Value(lyric("[00:01.00]musid-stub")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = wired_manager(&[SourceId::Fivesing], legacy.clone(), musid.clone());

    // 搜索面（Migrated）：musid 先行命中，计 MusidOk。
    let result = manager
        .search_scoped("kw", 1, 20, Some(SourceId::Fivesing))
        .await
        .expect("musid 路径搜索");
    assert_eq!(result.items[0].name, "双栈");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Search,
            SourceId::Fivesing,
            ParityOutcome::MusidOk
        ),
        1
    );

    // 流地址面（Migrated）：musid 命中，旧链零调用。
    let url = manager
        .get_song_url(
            &song_item("F1", SourceId::Fivesing, "双栈", "歌手"),
            Quality::Low128,
        )
        .await
        .expect("musid 解析");
    assert_eq!(url.url, "https://musid.example/audio.mp3");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::SongUrl,
            SourceId::Fivesing,
            ParityOutcome::MusidOk
        ),
        1
    );
    assert!(
        !legacy.calls().contains(&"legacy-fivesing/url".to_string()),
        "musid 命中后旧链不重试，实际: {:?}",
        legacy.calls()
    );
    assert_eq!(
        musid.calls(),
        vec![
            "musid-fivesing/search".to_string(),
            "musid-fivesing/url".to_string()
        ],
        "musid 参与搜索/流地址两面（歌词面在本文件歌词门用例）"
    );
}

/// fivesing 歌词面（Migrated，切片 2）：musid 歌词槽位先于内置 fallback
/// 命中——与 jamendo 的 LegacyOnly 拒绝互为镜像：账面从 V1Routed 改
/// Migrated 后运行行为不变（V1 起即放行），本用例把"三面全路由"的第三面
/// 钉进 manager 级门禁。
#[cfg(feature = "musid-dli-adapter-lyrics")]
#[tokio::test]
async fn v2_registry_fivesing_lyric_face_hits_the_musid_slot() {
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Fivesing,
        name: "legacy-fivesing",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Value(lyric("[00:01.00]legacy")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Fivesing,
        name: "musid-fivesing",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Value(lyric("[00:01.00]musid-stub")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = wired_manager(&[SourceId::Fivesing], legacy.clone(), musid.clone());

    let data = manager
        .get_lyric_with_fallback(&song_item("F1", SourceId::Fivesing, "双栈", "歌手"))
        .await
        .expect("歌词面 Migrated：musid 槽位命中");
    assert_eq!(data.lyric, "[00:01.00]musid-stub");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Lyric,
            SourceId::Fivesing,
            ParityOutcome::MusidOk
        ),
        1
    );
    assert!(
        !legacy
            .calls()
            .contains(&"legacy-fivesing/lyric".to_string()),
        "musid 有词时内置 fallback 零调用，实际: {:?}",
        legacy.calls()
    );
}

// ---------------------------------------------------------------------------
// V2 切片 3-7（apple/bilibili/migu/kuwo/kugou）：注册表放行/拒绝的 manager
// 行为（桩源双栈用例，与 jamendo/fivesing 同形；真网对账靠 probe 计数）。
// ---------------------------------------------------------------------------

/// apple（切片 3）：搜索面 + 流地址面 Migrated——musid 先行命中、旧链
/// 零调用（与 fivesing 同形；歌词面见下方 apple LegacyOnly 形态说明）。
#[tokio::test]
async fn v2_registry_apple_routes_search_and_song_url() {
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Apple,
        name: "legacy-apple",
        search: Script::Value(vec![song_item("A1", SourceId::Apple, "旧路", "歌手")]),
        url: Script::Value("legacy.example".to_string()),
        lyric: Script::Value(lyric("[00:01.00]legacy")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Apple,
        name: "musid-apple",
        search: Script::Value(vec![song_item("A1", SourceId::Apple, "双栈", "歌手")]),
        url: Script::Value("musid.example".to_string()),
        lyric: Script::Value(lyric("[00:01.00]musid-stub")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = wired_manager(&[SourceId::Apple], legacy.clone(), musid.clone());

    // 搜索面（Migrated）：musid 先行命中，计 MusidOk。
    let result = manager
        .search_scoped("kw", 1, 20, Some(SourceId::Apple))
        .await
        .expect("musid 路径搜索");
    assert_eq!(result.items[0].name, "双栈");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Search,
            SourceId::Apple,
            ParityOutcome::MusidOk
        ),
        1
    );

    // 流地址面（Migrated）：musid 命中，旧链零调用。
    let url = manager
        .get_song_url(
            &song_item("A1", SourceId::Apple, "双栈", "歌手"),
            Quality::Low128,
        )
        .await
        .expect("musid 解析");
    assert_eq!(url.url, "https://musid.example/audio.mp3");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::SongUrl,
            SourceId::Apple,
            ParityOutcome::MusidOk
        ),
        1
    );
    assert!(
        !legacy.calls().contains(&"legacy-apple/url".to_string()),
        "musid 命中后旧链不重试，实际: {:?}",
        legacy.calls()
    );
}

/// bilibili（切片 4）：搜索 + 流地址 Migrated、歌词 LegacyOnly——V1 显式
/// matches! 排除放开后由注册表行承载路由判定：搜索/流地址 musid 先行
/// 命中；歌词面用户拿到旧路径 NotFound，musid stub 的歌词到不了用户、
/// 影子源歌词面零调用（与 jamendo 歌词面同形镜像）。登录/会话/用户数据
/// 不经本判定（manager 登录链路不读影子 map），恒旧路径。
#[tokio::test]
async fn v2_registry_bilibili_routes_search_and_song_url_but_never_lyric() {
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Bili,
        name: "legacy-bili",
        search: Script::Value(vec![song_item("B1", SourceId::Bili, "旧路", "歌手")]),
        url: Script::Value("legacy.example".to_string()),
        // 与真实旧路径同形：bili/mod.rs:497-499 恒 FetchError::NotFound。
        lyric: Script::Fail,
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Bili,
        name: "musid-bili",
        search: Script::Value(vec![song_item("B1", SourceId::Bili, "双栈", "歌手")]),
        url: Script::Value("musid.example".to_string()),
        // 若注册表歌词门失效，这个"有词"的 stub 会被歌词槽位命中。
        lyric: Script::Value(lyric("[00:01.00]musid-stub")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = wired_manager(&[SourceId::Bili], legacy.clone(), musid.clone());

    // 搜索面（Migrated）：musid 先行命中，计 MusidOk。
    let result = manager
        .search_scoped("普通关键词", 1, 20, Some(SourceId::Bili))
        .await
        .expect("musid 路径搜索");
    assert_eq!(result.items[0].name, "双栈");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Search,
            SourceId::Bili,
            ParityOutcome::MusidOk
        ),
        1
    );

    // 流地址面（Migrated）：musid 命中。
    let url = manager
        .get_song_url(
            &song_item("B1", SourceId::Bili, "双栈", "歌手"),
            Quality::Low128,
        )
        .await
        .expect("musid 解析");
    assert_eq!(url.url, "https://musid.example/audio.mp3");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::SongUrl,
            SourceId::Bili,
            ParityOutcome::MusidOk
        ),
        1
    );

    // 歌词面（LegacyOnly）：用户拿到旧路径 NotFound，musid stub 的
    // "musid-stub" 歌词到不了用户；影子源歌词面零调用。
    let error = manager
        .get_lyric_with_fallback(&song_item("B1", SourceId::Bili, "双栈", "歌手"))
        .await
        .expect_err("歌词面恒旧路径");
    assert!(matches!(error, FetchError::NotFound));
    assert!(
        !musid.calls().contains(&"musid-bili/lyric".to_string()),
        "注册表拒绝后 musid 歌词面零调用（probe 影子同门），实际: {:?}",
        musid.calls()
    );
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Lyric,
            SourceId::Bili,
            ParityOutcome::MusidOk
        ),
        0
    );
    assert_eq!(
        musid.calls(),
        vec![
            "musid-bili/search".to_string(),
            "musid-bili/url".to_string()
        ],
        "musid 只参与搜索/流地址两面，歌词面（注册表拒绝）零调用"
    );
}

/// migu（切片 5）：搜索面 + 流地址面 Migrated（与 apple 同形；登录面双侧
/// 匿名不设行项，本用例的桩无登录面可断言——登录链路本就不读影子 map）。
#[tokio::test]
async fn v2_registry_migu_routes_search_and_song_url() {
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Mg,
        name: "legacy-migu",
        search: Script::Value(vec![song_item("M1", SourceId::Mg, "旧路", "歌手")]),
        url: Script::Value("legacy.example".to_string()),
        lyric: Script::Value(lyric("[00:01.00]legacy")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Mg,
        name: "musid-migu",
        search: Script::Value(vec![song_item("M1", SourceId::Mg, "双栈", "歌手")]),
        url: Script::Value("musid.example".to_string()),
        lyric: Script::Value(lyric("[00:01.00]musid-stub")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = wired_manager(&[SourceId::Mg], legacy.clone(), musid.clone());

    let result = manager
        .search_scoped("kw", 1, 20, Some(SourceId::Mg))
        .await
        .expect("musid 路径搜索");
    assert_eq!(result.items[0].name, "双栈");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Search,
            SourceId::Mg,
            ParityOutcome::MusidOk
        ),
        1
    );

    let url = manager
        .get_song_url(
            &song_item("M1", SourceId::Mg, "双栈", "歌手"),
            Quality::Low128,
        )
        .await
        .expect("musid 解析");
    assert_eq!(url.url, "https://musid.example/audio.mp3");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::SongUrl,
            SourceId::Mg,
            ParityOutcome::MusidOk
        ),
        1
    );
    assert!(
        !legacy.calls().contains(&"legacy-migu/url".to_string()),
        "musid 命中后旧链不重试，实际: {:?}",
        legacy.calls()
    );
}

/// kuwo（切片 6）：搜索面 + 流地址面 Migrated（与 migu 同形；Anonymous
/// web token 登录恒旧路径——登录链路本就不读影子 map）。
#[tokio::test]
async fn v2_registry_kuwo_routes_search_and_song_url() {
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Kw,
        name: "legacy-kuwo",
        search: Script::Value(vec![song_item("K1", SourceId::Kw, "旧路", "歌手")]),
        url: Script::Value("legacy.example".to_string()),
        lyric: Script::Value(lyric("[00:01.00]legacy")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Kw,
        name: "musid-kuwo",
        search: Script::Value(vec![song_item("K1", SourceId::Kw, "双栈", "歌手")]),
        url: Script::Value("musid.example".to_string()),
        lyric: Script::Value(lyric("[00:01.00]musid-stub")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = wired_manager(&[SourceId::Kw], legacy.clone(), musid.clone());

    let result = manager
        .search_scoped("kw", 1, 20, Some(SourceId::Kw))
        .await
        .expect("musid 路径搜索");
    assert_eq!(result.items[0].name, "双栈");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Search,
            SourceId::Kw,
            ParityOutcome::MusidOk
        ),
        1
    );

    let url = manager
        .get_song_url(
            &song_item("K1", SourceId::Kw, "双栈", "歌手"),
            Quality::Low128,
        )
        .await
        .expect("musid 解析");
    assert_eq!(url.url, "https://musid.example/audio.mp3");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::SongUrl,
            SourceId::Kw,
            ParityOutcome::MusidOk
        ),
        1
    );
    assert!(
        !legacy.calls().contains(&"legacy-kuwo/url".to_string()),
        "musid 命中后旧链不重试，实际: {:?}",
        legacy.calls()
    );
}

/// kugou（切片 7）：搜索面 + 流地址面 Migrated（与 kuwo 同形；登录/扫码
/// 恒旧路径——登录链路本就不读影子 map，流地址面匿名硬退化在真实 provider
/// 侧由 musid_kugou_slice.rs 钉住）。
#[tokio::test]
async fn v2_registry_kugou_routes_search_and_song_url() {
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Kg,
        name: "legacy-kugou",
        search: Script::Value(vec![song_item("G1", SourceId::Kg, "旧路", "歌手")]),
        url: Script::Value("legacy.example".to_string()),
        lyric: Script::Value(lyric("[00:01.00]legacy")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Kg,
        name: "musid-kugou",
        search: Script::Value(vec![song_item("G1", SourceId::Kg, "双栈", "歌手")]),
        url: Script::Value("musid.example".to_string()),
        lyric: Script::Value(lyric("[00:01.00]musid-stub")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = wired_manager(&[SourceId::Kg], legacy.clone(), musid.clone());

    let result = manager
        .search_scoped("kw", 1, 20, Some(SourceId::Kg))
        .await
        .expect("musid 路径搜索");
    assert_eq!(result.items[0].name, "双栈");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Search,
            SourceId::Kg,
            ParityOutcome::MusidOk
        ),
        1
    );

    let url = manager
        .get_song_url(
            &song_item("G1", SourceId::Kg, "双栈", "歌手"),
            Quality::Low128,
        )
        .await
        .expect("musid 解析");
    assert_eq!(url.url, "https://musid.example/audio.mp3");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::SongUrl,
            SourceId::Kg,
            ParityOutcome::MusidOk
        ),
        1
    );
    assert!(
        !legacy.calls().contains(&"legacy-kugou/url".to_string()),
        "musid 命中后旧链不重试，实际: {:?}",
        legacy.calls()
    );
}

/// kugou 歌词面（Migrated，切片 7）：musid 歌词槽位先于内置 fallback 命中
/// ——账面从 V1Routed 改 Migrated 后运行行为不变（V1 起即放行），本用例
/// 把"三面全路由"的第三面钉进 manager 级门禁（逐字降级的产物形态在
/// musid_kugou_slice.rs 的指纹 parity 用例钉住）。
#[cfg(feature = "musid-dli-adapter-lyrics")]
#[tokio::test]
async fn v2_registry_kugou_lyric_face_hits_the_musid_slot() {
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Kg,
        name: "legacy-kugou",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Value(lyric("[00:01.00]legacy")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Kg,
        name: "musid-kugou",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Value(lyric("[00:01.00]musid-stub")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = wired_manager(&[SourceId::Kg], legacy.clone(), musid.clone());

    let data = manager
        .get_lyric_with_fallback(&song_item("G1", SourceId::Kg, "双栈", "歌手"))
        .await
        .expect("歌词面 Migrated：musid 槽位命中");
    assert_eq!(data.lyric, "[00:01.00]musid-stub");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Lyric,
            SourceId::Kg,
            ParityOutcome::MusidOk
        ),
        1
    );
    assert!(
        !legacy.calls().contains(&"legacy-kugou/lyric".to_string()),
        "musid 有词时内置 fallback 零调用，实际: {:?}",
        legacy.calls()
    );
}

// ---------------------------------------------------------------------------
// V2 切片 8-11（tx/wy/qianqian/joox，v2-batch-2）：注册表放行的 manager
// 行为（桩源双栈用例，与切片 1-7 同形；三面 V1 起即放行，账面改 Migrated
// 不改运行行为——本组把"三面全路由"钉进 manager 级门禁；真网对账靠 probe
// 计数与各 musid_<source>_slice.rs 的真实 provider 用例）。
// ---------------------------------------------------------------------------

/// tx（切片 8）：搜索面 + 流地址面 Migrated——musid 先行命中、旧链零调用
/// （与 kugou 同形；登录/扫码/微信扫码恒旧路径——登录链路本就不读影子
/// map，wechat_login 双栈期恒 false §2.8）。
#[tokio::test]
async fn v2_registry_tx_routes_search_and_song_url() {
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Tx,
        name: "legacy-tx",
        search: Script::Value(vec![song_item("T1", SourceId::Tx, "旧路", "歌手")]),
        url: Script::Value("legacy.example".to_string()),
        lyric: Script::Value(lyric("[00:01.00]legacy")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Tx,
        name: "musid-tx",
        search: Script::Value(vec![song_item("T1", SourceId::Tx, "双栈", "歌手")]),
        url: Script::Value("musid.example".to_string()),
        lyric: Script::Value(lyric("[00:01.00]musid-stub")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = wired_manager(&[SourceId::Tx], legacy.clone(), musid.clone());

    let result = manager
        .search_scoped("kw", 1, 20, Some(SourceId::Tx))
        .await
        .expect("musid 路径搜索");
    assert_eq!(result.items[0].name, "双栈");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Search,
            SourceId::Tx,
            ParityOutcome::MusidOk
        ),
        1
    );

    let url = manager
        .get_song_url(
            &song_item("T1", SourceId::Tx, "双栈", "歌手"),
            Quality::Low128,
        )
        .await
        .expect("musid 解析");
    assert_eq!(url.url, "https://musid.example/audio.mp3");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::SongUrl,
            SourceId::Tx,
            ParityOutcome::MusidOk
        ),
        1
    );
    assert!(
        !legacy.calls().contains(&"legacy-tx/url".to_string()),
        "musid 命中后旧链不重试，实际: {:?}",
        legacy.calls()
    );
}

/// wy（切片 9）：搜索面 + 流地址面 Migrated——musid 先行命中、旧链零调用。
/// wy 的 V1 载体用例（上方 V1 组）在本片后继续全绿 = Migrated 行为与
/// V1Routed 一致的双证；登录态不对称（P5）的计数解读口径见 §11.8 (b)。
#[tokio::test]
async fn v2_registry_wy_routes_search_and_song_url() {
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Value(vec![song_item("W1", SourceId::Wy, "旧路", "歌手")]),
        url: Script::Value("legacy.example".to_string()),
        lyric: Script::Value(lyric("[00:01.00]legacy")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Value(vec![song_item("W1", SourceId::Wy, "双栈", "歌手")]),
        url: Script::Value("musid.example".to_string()),
        lyric: Script::Value(lyric("[00:01.00]musid-stub")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = wired_manager(&[SourceId::Wy], legacy.clone(), musid.clone());

    let result = manager
        .search_scoped("kw", 1, 20, Some(SourceId::Wy))
        .await
        .expect("musid 路径搜索");
    assert_eq!(result.items[0].name, "双栈");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Search,
            SourceId::Wy,
            ParityOutcome::MusidOk
        ),
        1
    );

    let url = manager
        .get_song_url(
            &song_item("W1", SourceId::Wy, "双栈", "歌手"),
            Quality::Low128,
        )
        .await
        .expect("musid 解析");
    assert_eq!(url.url, "https://musid.example/audio.mp3");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::SongUrl,
            SourceId::Wy,
            ParityOutcome::MusidOk
        ),
        1
    );
    assert!(
        !legacy.calls().contains(&"legacy-wy/url".to_string()),
        "musid 命中后旧链不重试，实际: {:?}",
        legacy.calls()
    );
}

/// qianqian（切片 10，用户启用源）：搜索面 + 流地址面 Migrated——musid
/// 先行命中、旧链零调用；空 tsid 双失败链在 musid_qianqian_slice.rs 的
/// manager 级真实用例钉住。
#[tokio::test]
async fn v2_registry_qianqian_routes_search_and_song_url() {
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Qianqian,
        name: "legacy-qianqian",
        search: Script::Value(vec![song_item("Q1", SourceId::Qianqian, "旧路", "歌手")]),
        url: Script::Value("legacy.example".to_string()),
        lyric: Script::Value(lyric("[00:01.00]legacy")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Qianqian,
        name: "musid-qianqian",
        search: Script::Value(vec![song_item("Q1", SourceId::Qianqian, "双栈", "歌手")]),
        url: Script::Value("musid.example".to_string()),
        lyric: Script::Value(lyric("[00:01.00]musid-stub")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = wired_manager(&[SourceId::Qianqian], legacy.clone(), musid.clone());

    let result = manager
        .search_scoped("kw", 1, 20, Some(SourceId::Qianqian))
        .await
        .expect("musid 路径搜索");
    assert_eq!(result.items[0].name, "双栈");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Search,
            SourceId::Qianqian,
            ParityOutcome::MusidOk
        ),
        1
    );

    let url = manager
        .get_song_url(
            &song_item("Q1", SourceId::Qianqian, "双栈", "歌手"),
            Quality::Low128,
        )
        .await
        .expect("musid 解析");
    assert_eq!(url.url, "https://musid.example/audio.mp3");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::SongUrl,
            SourceId::Qianqian,
            ParityOutcome::MusidOk
        ),
        1
    );
    assert!(
        !legacy.calls().contains(&"legacy-qianqian/url".to_string()),
        "musid 命中后旧链不重试，实际: {:?}",
        legacy.calls()
    );
}

/// joox（切片 11，用户启用源）：搜索面 + 流地址面 Migrated——musid 先行
/// 命中、旧链零调用；封面面不接 musid（get_cover_url 走歌曲模型回填，
/// 恒旧路径语义）。
#[tokio::test]
async fn v2_registry_joox_routes_search_and_song_url() {
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Joox,
        name: "legacy-joox",
        search: Script::Value(vec![song_item("X1", SourceId::Joox, "旧路", "歌手")]),
        url: Script::Value("legacy.example".to_string()),
        lyric: Script::Value(lyric("[00:01.00]legacy")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Joox,
        name: "musid-joox",
        search: Script::Value(vec![song_item("X1", SourceId::Joox, "双栈", "歌手")]),
        url: Script::Value("musid.example".to_string()),
        lyric: Script::Value(lyric("[00:01.00]musid-stub")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = wired_manager(&[SourceId::Joox], legacy.clone(), musid.clone());

    let result = manager
        .search_scoped("kw", 1, 20, Some(SourceId::Joox))
        .await
        .expect("musid 路径搜索");
    assert_eq!(result.items[0].name, "双栈");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Search,
            SourceId::Joox,
            ParityOutcome::MusidOk
        ),
        1
    );

    let url = manager
        .get_song_url(
            &song_item("X1", SourceId::Joox, "双栈", "歌手"),
            Quality::Low128,
        )
        .await
        .expect("musid 解析");
    assert_eq!(url.url, "https://musid.example/audio.mp3");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::SongUrl,
            SourceId::Joox,
            ParityOutcome::MusidOk
        ),
        1
    );
    assert!(
        !legacy.calls().contains(&"legacy-joox/url".to_string()),
        "musid 命中后旧链不重试，实际: {:?}",
        legacy.calls()
    );
}

/// qianqian 歌词面（Migrated，切片 10）：musid 歌词槽位先于内置 fallback
/// 命中——账面从 V1Routed 改 Migrated 后运行行为不变（V1 起即放行），
/// 本用例把"三面全路由"的第三面钉进 manager 级门禁（纯 LRC 无逐字，
/// legacy 千千本就无逐字，无降级登记）。
#[cfg(feature = "musid-dli-adapter-lyrics")]
#[tokio::test]
async fn v2_registry_qianqian_lyric_face_hits_the_musid_slot() {
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Qianqian,
        name: "legacy-qianqian",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Value(lyric("[00:01.00]legacy")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Qianqian,
        name: "musid-qianqian",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Value(lyric("[00:01.00]musid-stub")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = wired_manager(&[SourceId::Qianqian], legacy.clone(), musid.clone());

    let data = manager
        .get_lyric_with_fallback(&song_item("Q1", SourceId::Qianqian, "双栈", "歌手"))
        .await
        .expect("歌词面 Migrated：musid 槽位命中");
    assert_eq!(data.lyric, "[00:01.00]musid-stub");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Lyric,
            SourceId::Qianqian,
            ParityOutcome::MusidOk
        ),
        1
    );
    assert!(
        !legacy
            .calls()
            .contains(&"legacy-qianqian/lyric".to_string()),
        "musid 有词时内置 fallback 零调用，实际: {:?}",
        legacy.calls()
    );
}

/// joox 歌词面（Migrated，切片 11）：与 qianqian 同形——musid 歌词槽位
/// 先于内置 fallback 命中（纯 LRC，musid lyric_live base64 解码后无逐字，
/// legacy 本就无逐字，无降级登记）。
#[cfg(feature = "musid-dli-adapter-lyrics")]
#[tokio::test]
async fn v2_registry_joox_lyric_face_hits_the_musid_slot() {
    let legacy = Arc::new(ScriptedSource {
        id: SourceId::Joox,
        name: "legacy-joox",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Value(lyric("[00:01.00]legacy")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = Arc::new(ScriptedSource {
        id: SourceId::Joox,
        name: "musid-joox",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Value(lyric("[00:01.00]musid-stub")),
        playlists: false,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = wired_manager(&[SourceId::Joox], legacy.clone(), musid.clone());

    let data = manager
        .get_lyric_with_fallback(&song_item("X1", SourceId::Joox, "双栈", "歌手"))
        .await
        .expect("歌词面 Migrated：musid 槽位命中");
    assert_eq!(data.lyric, "[00:01.00]musid-stub");
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Lyric,
            SourceId::Joox,
            ParityOutcome::MusidOk
        ),
        1
    );
    assert!(
        !legacy.calls().contains(&"legacy-joox/lyric".to_string()),
        "musid 有词时内置 fallback 零调用，实际: {:?}",
        legacy.calls()
    );
}
