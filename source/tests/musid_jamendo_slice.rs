//! V2 切片 1（jamendo）：**真实** musid-dli `JamendoProvider` 的行为对账
//! （评审 P1——复刻桩只测适配层自身的映射/门控行为，本文件挂
//! `musid-dli-adapter-jamendo` 门、构造真实 provider，把桩与真实实现的
//! 一致性钉进确定性门禁）。真实 provider 的离线 resolve
//! （`extra["stream_url"]` 内嵌取址，musid-dli source_jamendo.rs:402-444）
//! 与歌词 stub 错误映射都不需要网络；搜索面需真网，不在确定性门禁内
//! （运行期 probe 对账覆盖，docs §12.3）。装配形态与
//! `musid_bootstrap::assemble!` 同形（同一 provider 实例拆三面）。

#![cfg(feature = "musid-dli-adapter-jamendo")]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{FetchError, MusicSource};
use lx_source::adapter_musid::{AdapterMusicSource, TransportInjection};
use lx_source::manager::{MusidRouteConfig, SourceManager};
use lx_source::parity::{ParityFace, ParityOutcome};
use musid_dli::models::{Track, TrackId};
use musid_dli::provider::{
    AuthCapability, LyricsProvider as MdLyricsProvider, MusicProvider, ProviderCapabilities,
};

/// 真实 provider 按装配点同形拆三面（assemble! 的独立重放）。
fn real_adapter() -> AdapterMusicSource {
    let provider = Arc::new(musid_dli::source_jamendo::JamendoProvider::new());
    AdapterMusicSource::new(
        SourceId::Jamendo,
        format!("{}（musid-dli）", provider.name()),
        Some(provider.clone()),
        Some(provider.clone()),
        Some(provider),
        TransportInjection::new(None, 15),
    )
}

/// musid 搜到的歌（extra 携带搜索时嵌入的取址三元组）：与
/// `source/tests/fixtures/musid/track_jamendo.json` 的样本值同源
/// （musid-dli tests/fixtures/jamendo/track_parse.json 记录的 2026-11-18
/// 捕获重建形状，原始捕获未随上游仓提交——provenance 限定见 §12.3）。
fn musid_sourced_song() -> SongInfo {
    let mut song = SongInfo::new(
        "1538710".to_string(),
        SourceId::Jamendo,
        "Love Recycle".to_string(),
        "LoveRecycle".to_string(),
    );
    song.album_name = "LoveRecycle".to_string();
    song.album_id = "176100".to_string();
    song.duration = Duration::from_secs(245);
    song.cover_url = Some(
        "https://usercontent.jamendo.com?type=album&id=176100&width=300&trackid=1538710"
            .to_string(),
    );
    song.qualities.insert(Quality::Low128);
    song.extra
        .insert("track_id".to_string(), "1538710".to_string());
    song.extra.insert(
        "stream_url".to_string(),
        "https://prod-1.storage.jamendo.com/download/track/1538710/mp32/".to_string(),
    );
    song.extra
        .insert("stream_format".to_string(), "Mp3".to_string());
    song.extra
        .insert("stream_quality".to_string(), "Low".to_string());
    song
}

fn count_of(manager: &SourceManager, face: ParityFace, outcome: ParityOutcome) -> u64 {
    manager
        .musid_parity_snapshot()
        .into_iter()
        .find(|record| {
            record.face == face && record.source == SourceId::Jamendo && record.outcome == outcome
        })
        .map(|record| record.count)
        .unwrap_or(0)
}

/// 真实 provider 离线 resolve：adapter 组 Track（extra 透传）→ 真实
/// `JamendoProvider::resolve` 取内嵌地址 → 实际档位回填 + enrich。桩语义
/// 与真实实现漂移时本测试必红（评审 P1 的核心证据）。
#[tokio::test]
async fn real_provider_resolves_the_embedded_stream_offline() {
    let adapter = real_adapter();
    let song = musid_sourced_song();
    let url = adapter
        .get_song_url(&song, Quality::Flac24)
        .await
        .expect("真实 provider 离线 resolve");
    assert_eq!(
        url.url,
        "https://prod-1.storage.jamendo.com/download/track/1538710/mp32/"
    );
    assert_eq!(
        url.quality,
        Quality::Low128,
        "真实 resolve 忽略请求档位（曲库恒最优），实际档位回填（§2.4）"
    );
    assert_eq!(
        url.cover_url.as_deref(),
        Some("https://usercontent.jamendo.com?type=album&id=176100&width=300&trackid=1538710"),
        "enrich_song_url_from_song 从歌曲模型补齐封面（§3 第 13 条）"
    );
    assert_eq!(url.qualities, vec![Quality::Low128]);
    assert_eq!(url.duration, Duration::from_secs(245));
    assert!(
        url.headers.is_empty() && url.candidate_urls.is_empty() && url.size.is_none(),
        "真实 resolve 产物无 headers/备用/size（与 StreamInfo 构造逐字段一致）"
    );
}

/// legacy 搜到的歌（extra 无 stream_url）：真实 provider 报
/// "track carries no embedded stream URL"（Provider 类）→
/// FetchError::Other → manager 旧链兜底（§12.2 动态层触发点）。
#[tokio::test]
async fn real_provider_fails_without_embedded_url_into_the_fallback_path() {
    let adapter = real_adapter();
    let mut song = SongInfo::new(
        "1538710".to_string(),
        SourceId::Jamendo,
        "Love Recycle".to_string(),
        "LoveRecycle".to_string(),
    );
    song.extra
        .insert("track_id".to_string(), "1538710".to_string());
    let error = adapter
        .get_song_url(&song, Quality::High320)
        .await
        .expect_err("真实 provider 无内嵌地址必须失败");
    assert!(matches!(error, FetchError::Other(_)), "实际: {error}");
    assert!(
        error.to_string().contains("stream URL"),
        "真实 provider 的错误语义（非 NotFound）: {error}"
    );
}

/// 真实歌词 stub 的错误映射：§12.2 静态层（jamendo 歌词 LegacyOnly）的
/// 登记依据用真实实现钉住——`unsupported_operation("the CC catalog
/// exposes no lyrics")` 经 §9 表映射为 `Other("该音源不支持…")`，会污染
/// 旧路径的恒 NotFound 语义，故不路由。
#[tokio::test]
async fn real_lyrics_stub_error_is_the_registered_pollution_reason() {
    let provider = musid_dli::source_jamendo::JamendoProvider::new();
    assert!(
        !provider.capabilities().lyrics,
        "歌词 descriptor false（真实 provider 位）"
    );
    let track = Track {
        id: TrackId::new("jamendo", "1538710"),
        title: "Love Recycle".to_string(),
        artists: vec!["LoveRecycle".to_string()],
        album: None,
        album_id: None,
        duration: None,
        artwork_url: None,
        isrc: None,
        available_qualities: vec![],
        audio: None,
        extra: BTreeMap::new(),
    };
    let error = MdLyricsProvider::lyrics(&provider, &track)
        .await
        .expect_err("真实歌词 stub 恒拒绝");
    let mapped = lx_source::adapter_musid::fetch_error_from_error(&error);
    let polluted = matches!(&mapped, FetchError::Other(text) if text.contains("该音源不支持"));
    assert!(
        polluted,
        "UnsupportedOperation → Other（非 NotFound 的语义污染正是 LegacyOnly 的理由）: {mapped:?}"
    );
    assert!(
        error
            .to_string()
            .contains("the CC catalog exposes no lyrics"),
        "真实 stub 文本: {error}"
    );
}

/// 真实 descriptor pin：注册表 jamendo 行（Migrated×2 + 歌词 LegacyOnly +
/// 集合面不设行项）的依据逐位对上真实 provider——track_search/stream_resolve
/// true，集合面/歌词/authentication 全 false，supported_qualities 缺省空。
#[test]
fn real_descriptor_pins_the_v2_jamendo_ledger_row() {
    let provider = musid_dli::source_jamendo::JamendoProvider::new();
    assert_eq!(
        provider.capabilities(),
        ProviderCapabilities {
            track_search: true,
            stream_resolve: true,
            ..ProviderCapabilities::default()
        },
        "jamendo 真实 descriptor 漂移即红（§12.4 账面依据）"
    );
    assert_eq!(provider.auth_capability(), AuthCapability::None);
    assert!(
        provider.supported_qualities().is_empty(),
        "jamendo 未覆写 supported_qualities（musid-dli MusicProvider 缺省空）→ 适配层翻译产物为空"
    );
}

/// manager 级路由：真实影子源装配进独立 map + 注册表放行流地址面 →
/// get_song_url 命中真实 provider（零网络），计 MusidOk；歌词面（LegacyOnly）
/// musid 零调用。真实 provider 的全链路（路由判定 → adapter 映射 → resolve）
/// 一次走通。
#[tokio::test]
async fn manager_routes_song_url_through_the_real_jamendo_provider() {
    let manager = SourceManager::new(SourceId::Kw, &[SourceId::Jamendo]);
    let registered = lx_source::musid_bootstrap::register_available(
        &manager,
        &TransportInjection::new(None, 15),
    );
    assert!(
        registered.contains(&SourceId::Jamendo),
        "真实装配分支编译并生效"
    );
    // 切片 2（fivesing）起账面有两个 Migrated 源：单 jamendo 子 feature
    // 构建下另一 Migrated 源未装配属预期 warn 形态（§12.1 生效前提），按
    // "不含本切片源"断言；全等空断言由 musid_bootstrap 的
    // migrated_slices_assemble_without_ledger_gap（双 Migrated 组合构建）
    // 钉住（评审 P4：账面/构建无脱节的强口径不变）。
    assert!(
        !lx_source::adapter_musid::unregistered_migrated_sources(&registered)
            .contains(&SourceId::Jamendo),
        "jamendo 已装配，不得出现在账面/构建脱节 warn 输入里"
    );
    manager.set_musid_routes(MusidRouteConfig {
        stack: true,
        search: true,
        song_url: true,
        lyric: true,
        parity_probe: false,
    });

    let url = manager
        .get_song_url(&musid_sourced_song(), Quality::Flac24)
        .await
        .expect("真实 provider 经 manager 路由命中");
    assert_eq!(
        url.url,
        "https://prod-1.storage.jamendo.com/download/track/1538710/mp32/"
    );
    assert_eq!(url.quality, Quality::Low128);
    assert_eq!(
        count_of(&manager, ParityFace::SongUrl, ParityOutcome::MusidOk),
        1,
        "真实 provider 的流地址面路由命中计 MusidOk"
    );
    assert_eq!(
        count_of(&manager, ParityFace::Lyric, ParityOutcome::MusidOk),
        0,
        "歌词面 LegacyOnly：真实影子源歌词零调用"
    );
}
