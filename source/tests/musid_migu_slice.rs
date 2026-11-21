//! V2 切片 5（migu）：**真实** musid-dli `MiguProvider` 的行为对账
//! （与切片 1/2/3 同形，评审 P1）。migu 三个面都是网络现取
//! （resourceinfo/strategy 端点 + MRC/lrc/trc 下载），确定性门禁内不可
//! 真跑——本文件钉住真实 provider 的 descriptor 账面依据（§12.4 行 5）、
//! 离线失败路径（缺 resourceId 的歌在**两栈**都在任何网络调用之前失败 →
//! manager 级真实双侧链路 MusidErrLegacyErr）、歌词面确认无词形态（无
//! mrcUrl/lrcUrl 的 extra → Ok(None) → NotFound）；真网行为由运行期 probe
//! 对账覆盖（docs §12.3）。装配形态与 `musid_bootstrap::assemble!` 同形。

#![cfg(feature = "musid-dli-adapter-migu")]

use std::sync::Arc;

use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{FetchError, MusicSource};
use lx_source::adapter_musid::{AdapterMusicSource, TransportInjection};
use lx_source::manager::{MusidRouteConfig, SourceManager};
use lx_source::parity::{ParityFace, ParityOutcome};
use musid_dli::provider::{AuthCapability, MusicProvider, ProviderCapabilities};

/// 真实 provider 按装配点同形拆三面（assemble! 的独立重放）。
fn real_adapter() -> AdapterMusicSource {
    let provider = Arc::new(musid_dli::source_migu::MiguProvider::new());
    AdapterMusicSource::new(
        SourceId::Mg,
        format!("{}（musid-dli）", provider.name()),
        Some(provider.clone()),
        Some(provider.clone()),
        Some(provider),
        TransportInjection::new(None, 15),
    )
}

fn count_of(manager: &SourceManager, face: ParityFace, outcome: ParityOutcome) -> u64 {
    manager
        .musid_parity_snapshot()
        .into_iter()
        .find(|record| {
            record.face == face && record.source == SourceId::Mg && record.outcome == outcome
        })
        .map(|record| record.count)
        .unwrap_or(0)
}

/// 真实 descriptor pin：注册表 migu 行（三面全 Migrated + 集合面不设行项）
/// 的依据逐位对上真实 provider——track_search/stream_resolve/lyrics/
/// playlists/albums/charts/link_resolve true，authentication=false +
/// `AuthCapability::None`（匿名 sid 设备会话不是账号登录，§2.8 机制细分
/// 不置 voicefox login 位），无 ArtistProvider（artists 位缺省 false），
/// supported_qualities 未覆写（缺省空）。
#[test]
fn real_descriptor_pins_the_v2_migu_ledger_row() {
    let provider = musid_dli::source_migu::MiguProvider::new();
    assert_eq!(
        provider.capabilities(),
        ProviderCapabilities {
            track_search: true,
            stream_resolve: true,
            lyrics: true,
            playlists: true,
            albums: true,
            charts: true,
            link_resolve: true,
            ..ProviderCapabilities::default()
        },
        "migu 真实 descriptor 漂移即红（§12.4 账面依据）"
    );
    assert_eq!(provider.auth_capability(), AuthCapability::None);
    assert!(
        provider.supported_qualities().is_empty(),
        "migu 未覆写 supported_qualities（缺省空）→ 适配层翻译产物为空"
    );
}

/// 缺 resourceId 的歌（extra 无 contentId/copyrightId）：musid resolve 在
/// **任何网络调用之前**失败（resource_id None → Error::NotFound，
/// source_migu.rs:1495-1503）→ 适配层 FetchError::NotFound。与 legacy 同形
/// 失败（mg/url.rs:361-372 同键同回落）——manager 级双侧链路见下方
/// manager 用例。
#[tokio::test]
async fn resolve_fails_offline_for_songs_without_a_resource_id() {
    let adapter = real_adapter();
    let song = SongInfo::new(
        "3790007".to_string(),
        SourceId::Mg,
        "晴天".to_string(),
        "周杰伦".to_string(),
    );
    let error = adapter
        .get_song_url(&song, Quality::High320)
        .await
        .expect_err("musid 无 resourceId 必须离线失败");
    assert!(matches!(error, FetchError::NotFound), "实际: {error:?}");
}

/// manager 级双侧真实链路（零网络）：真实 legacy 源 + 真实 musid 影子源 +
/// 注册表放行（三面 Migrated）→ 缺 resourceId 的歌在流地址面两栈同请求
/// 都失败——musid 侧 resource_id None → Error::NotFound（离线）、legacy
/// 侧 mg/url.rs:372 同键同回落 → NotFound（离线）——计 MusidErrLegacyErr，
/// legacy 错误原样向上。替换模式的完整失败链一次走通（真实 provider 的
/// 搜索/歌词/resolve 成功路径是网络面，由运行期 probe 对账覆盖）。
#[tokio::test]
async fn manager_falls_back_to_the_real_legacy_source_when_musid_fails_offline() {
    let mut manager = SourceManager::new(SourceId::Kw, &[SourceId::Mg]);
    manager.register(Arc::new(lx_source::mg::MgSource::new()));
    let registered = lx_source::musid_bootstrap::register_available(
        &manager,
        &TransportInjection::new(None, 15),
    );
    assert!(registered.contains(&SourceId::Mg), "真实装配分支编译并生效");
    manager.set_musid_routes(MusidRouteConfig {
        stack: true,
        search: true,
        song_url: true,
        lyric: true,
        parity_probe: false,
    });

    let song = SongInfo::new(
        "3790007".to_string(),
        SourceId::Mg,
        "晴天".to_string(),
        "周杰伦".to_string(),
    );
    let error = manager
        .get_song_url(&song, Quality::High320)
        .await
        .expect_err("两栈同请求都失败");
    // legacy 侧 resource_id None → FetchError::NotFound（mg/url.rs:372），
    // 内置先行臂把它收敛为错误文本（NotFound 的 Display 是 "not found"），
    // 统一出口再包 Other——既有旧代码行为逐字保持（manager.rs
    // get_song_url_inner 的 builtin_error 聚合，fivesing 切片用例同款）。
    let legacy_not_found = matches!(&error, FetchError::Other(text) if text == "not found");
    assert!(
        legacy_not_found,
        "用户拿到 legacy 失败文本（musid 的 NotFound 只进计数）: {error:?}"
    );
    assert_eq!(
        count_of(
            &manager,
            ParityFace::SongUrl,
            ParityOutcome::MusidErrLegacyErr
        ),
        1,
        "两栈同请求都失败 → MusidErrLegacyErr（§11.5 流地址面替换模式三态）"
    );
    assert_eq!(
        count_of(&manager, ParityFace::SongUrl, ParityOutcome::MusidOk),
        0
    );
}

/// 歌词面确认无词形态：extra 无 mrcUrl/lrcUrl 的歌 → musid lyrics 走
/// fetch_lrc 兜底 → original 空 → Ok(None)（source_migu.rs:1810-1814）→
/// 适配层 FetchError::NotFound（与 legacy 同请求的空歌词/负缓存口径衔接，
/// §11.4 歌词面）。真实 provider 离线链路（无网络调用）。依赖歌词转换
/// 变体——缺变体时适配层歌词面在 provider 之前短路（feature gate 错误），
/// 与 fivesing/kuwo/kugou 的歌词用例同门。
#[cfg(feature = "musid-dli-adapter-lyrics")]
#[tokio::test]
async fn lyric_face_returns_not_found_for_songs_without_lyric_urls() {
    let adapter = real_adapter();
    let song = SongInfo::new(
        "3790007".to_string(),
        SourceId::Mg,
        "晴天".to_string(),
        "周杰伦".to_string(),
    );
    let error = adapter
        .get_lyric(&song)
        .await
        .expect_err("musid 无 mrcUrl/lrcUrl → 确认无词");
    assert!(matches!(error, FetchError::NotFound), "实际: {error:?}");
}
