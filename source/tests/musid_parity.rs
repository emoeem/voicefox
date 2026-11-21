//! V1 双栈 parity 对账测试（feature `musid-dli-adapter`）。
//!
//! 覆盖：聚合搜索 probe 真双跑的对照计数（docs/MUSID_DLI_ADAPTER.md §11
//! 的"产物经 adapter 归一后对照计数，恒展示旧路径结果"）、§11 归一规则在
//! 端到端链路上的行为、以及 probe 与替换模式的计数分离。
//!
//! 单条歌曲/歌词的归一规则单元断言在 `source/src/parity.rs` 的 tests 里。

#![cfg(feature = "musid-dli-adapter")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use lx_core::model::lyric::LyricData;
use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{FetchError, MusicSource, SearchError, SearchResult, SongUrl};
use lx_source::manager::{MusidRouteConfig, SourceManager};
use lx_source::parity::{ParityFace, ParityOutcome};

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

fn song_item(id: &str, source: SourceId, name: &str, singer: &str) -> SongInfo {
    let mut song = SongInfo::new(id.to_string(), source, name.to_string(), singer.to_string());
    song.duration = Duration::from_secs(180);
    song
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
/// 到达 `min`（桩源查询微秒级完成，真实等待上限 1s 只兜调度抖动；
/// `start_paused` 测试里 sleep 推进虚拟时间，同样即时）。
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

fn legacy_wy(items: Vec<SongInfo>) -> Arc<ScriptedSource> {
    Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Value(items),
        url: Script::Fail,
        lyric: Script::Fail,
        calls: Arc::new(Mutex::new(Vec::new())),
    })
}

fn musid_wy(items: Vec<SongInfo>) -> Arc<ScriptedSource> {
    Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Value(items),
        url: Script::Fail,
        lyric: Script::Fail,
        calls: Arc::new(Mutex::new(Vec::new())),
    })
}

fn probe_manager(legacy: Arc<ScriptedSource>, musid: Arc<ScriptedSource>) -> SourceManager {
    let mut manager = SourceManager::new(SourceId::Kw, &[SourceId::Wy]);
    manager.register(legacy);
    manager.register_musid_source(musid);
    // probe 影子模式：面开关全关（搜索面只靠 probe 参与），旧路径权威。
    manager.set_musid_routes(MusidRouteConfig {
        stack: true,
        search: false,
        song_url: false,
        lyric: false,
        parity_probe: true,
    });
    manager
}

/// 聚合搜索 probe 真双跑：影子与旧路径并行，产物经归一对照计数，
/// 聚合结果恒为旧路径产物。
#[tokio::test]
async fn search_all_probe_shadow_counts_match_and_keeps_legacy_result() {
    let legacy_items = vec![
        song_item("1", SourceId::Wy, "晴天", "周杰伦"),
        song_item("2", SourceId::Wy, "七里香", "周杰伦"),
    ];
    let musid_items = vec![
        song_item("2", SourceId::Wy, "七里香", "周杰伦"),
        song_item("1", SourceId::Wy, "晴天", "周杰伦"),
    ];
    let legacy = legacy_wy(legacy_items.clone());
    let musid = musid_wy(musid_items);
    let manager = probe_manager(legacy.clone(), musid.clone());

    let result = manager.search_all("kw", 1, 20).await.expect("聚合搜索");
    let names: Vec<&str> = result.items.iter().map(|song| song.name.as_str()).collect();
    // 聚合结果经 sort_search_results 排序；关键词与歌名无关时按名称序。
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "恒展示旧路径产物（经聚合排序）");
    assert!(names.contains(&"晴天") && names.contains(&"七里香"));
    assert_eq!(
        settled_count(
            &manager,
            ParityFace::Search,
            SourceId::Wy,
            ParityOutcome::ParityMatch,
            1
        )
        .await,
        1,
        "id 集一致 + 交集内归一等价 → ParityMatch（后台对账落账）"
    );
    assert_eq!(
        count_of(
            &manager,
            ParityFace::Search,
            SourceId::Wy,
            ParityOutcome::ParityMismatch
        ),
        0
    );
    assert_eq!(
        legacy.calls(),
        vec!["legacy-wy/search".to_string()],
        "旧路径任务照常执行（权威）"
    );
    assert_eq!(
        musid.calls(),
        vec!["musid-wy/search".to_string()],
        "影子任务真双跑一次"
    );
}

/// 归一不等（同名不同歌手）→ ParityMismatch；聚合结果仍是旧路径产物。
#[tokio::test]
async fn search_all_probe_counts_mismatch_on_normalized_difference() {
    let legacy = legacy_wy(vec![song_item("1", SourceId::Wy, "晴天", "周杰伦")]);
    let musid = musid_wy(vec![song_item("1", SourceId::Wy, "晴天", "费玉清")]);
    let manager = probe_manager(legacy, musid);

    let result = manager.search_all("kw", 1, 20).await.expect("聚合搜索");
    assert_eq!(result.items[0].singer, "周杰伦", "恒展示旧路径结果");
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

/// id 集不一致（同条数不同 id）→ ParityMismatch（id 精确）。
#[tokio::test]
async fn search_all_probe_counts_mismatch_on_id_gap() {
    let legacy = legacy_wy(vec![song_item("1", SourceId::Wy, "晴天", "周杰伦")]);
    let musid = musid_wy(vec![song_item("9", SourceId::Wy, "晴天", "周杰伦")]);
    let manager = probe_manager(legacy, musid);

    manager.search_all("kw", 1, 20).await.expect("聚合搜索");
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

/// 影子 Err/超时/旧路径该源失败：无双方产物，不作对账结论，不计。
#[tokio::test(start_paused = true)]
async fn search_all_probe_skips_one_sided_or_failed_shadows() {
    // 影子挂死（10s 每源预算切断）：不计，旧路径结果不受影响。
    let legacy = legacy_wy(vec![song_item("1", SourceId::Wy, "晴天", "周杰伦")]);
    let musid_hang = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Hang,
        url: Script::Fail,
        lyric: Script::Fail,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = probe_manager(legacy, musid_hang);
    let result = manager.search_all("kw", 1, 20).await.expect("旧路径结果");
    assert_eq!(result.items[0].name, "晴天");
    // 挂死的影子（10s 预算）在后台被切断后仍无产物 → 不计；虚拟时间推进
    // 越过影子预算，让后台对账真正得出结论（而非窗口未到）。
    tokio::time::sleep(std::time::Duration::from_secs(11)).await;
    assert!(manager.musid_parity_snapshot().is_empty(), "影子无产物不计");

    // 旧路径该源失败：影子有产物也不对照（无双方产物）。
    let legacy_fail = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "legacy-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Fail,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let musid = musid_wy(vec![song_item("1", SourceId::Wy, "晴天", "周杰伦")]);
    let manager = probe_manager(legacy_fail, musid);
    let error = manager
        .search_all("kw", 1, 20)
        .await
        .expect_err("旧路径失败");
    assert!(error.to_string().contains("所有音源搜索失败"));
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert!(manager.musid_parity_snapshot().is_empty(), "单侧产物不计");
}

/// 替换模式与 probe 计数分离：替换模式不产 parity_match/mismatch，
/// probe 模式不产 musid_err_legacy_*（各自独立的运行形态）。
#[tokio::test]
async fn replacement_and_probe_counters_stay_in_separate_modes() {
    // 替换模式：musid 失败 → 旧路径成功 → MusidErrLegacyOk，无 parity 对账。
    let legacy = legacy_wy(vec![song_item("L1", SourceId::Wy, "旧路", "歌手")]);
    let musid_fail = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Fail,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let mut manager = SourceManager::new(SourceId::Kw, &[SourceId::Wy]);
    manager.register(legacy);
    manager.register_musid_source(musid_fail);
    manager.set_musid_routes(MusidRouteConfig {
        stack: true,
        search: true,
        song_url: true,
        lyric: true,
        parity_probe: false,
    });
    manager.search_all("kw", 1, 20).await.expect("回退后有产物");
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
            ParityOutcome::ParityMatch
        ),
        0,
        "替换模式不做对照计数"
    );

    // probe 模式（本场景单侧失败）：musid 失败无对照 → 不计；ParityMatch/
    // ParityMismatch 是 probe 的对照结论，MusidErrLegacyOk 是替换模式的回退
    // 归因（probe 只在双 Err 时记 MusidErrLegacyErr，见 §11.5 搜索面 probe 列）。
    let legacy = legacy_wy(vec![song_item("L1", SourceId::Wy, "旧路", "歌手")]);
    let musid_fail = Arc::new(ScriptedSource {
        id: SourceId::Wy,
        name: "musid-wy",
        search: Script::Fail,
        url: Script::Fail,
        lyric: Script::Fail,
        calls: Arc::new(Mutex::new(Vec::new())),
    });
    let manager = probe_manager(legacy, musid_fail);
    manager.search_all("kw", 1, 20).await.expect("旧路径结果");
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert!(manager.musid_parity_snapshot().is_empty(), "影子失败不计");
}

/// 计数快照的稳定性：多次累计同键同 outcome，快照按序聚合。
#[tokio::test]
async fn counters_accumulate_across_requests() {
    let legacy = legacy_wy(vec![song_item("1", SourceId::Wy, "晴天", "周杰伦")]);
    let musid = musid_wy(vec![song_item("1", SourceId::Wy, "晴天", "周杰伦")]);
    let manager = probe_manager(legacy, musid);
    for _ in 0..3 {
        manager.search_all("kw", 1, 20).await.expect("聚合搜索");
    }
    assert_eq!(
        settled_count(
            &manager,
            ParityFace::Search,
            SourceId::Wy,
            ParityOutcome::ParityMatch,
            3
        )
        .await,
        3
    );
    let snapshot = manager.musid_parity_snapshot();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].face, ParityFace::Search);
    assert_eq!(snapshot[0].source, SourceId::Wy);
    assert_eq!(snapshot[0].outcome, ParityOutcome::ParityMatch);
    assert_eq!(snapshot[0].count, 3);
}
