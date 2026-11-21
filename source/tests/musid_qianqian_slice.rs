//! V2 切片 10（qianqian，用户启用源）：**真实** musid-dli `QianqianProvider`
//! 的行为对账（与切片 1-7 同形，评审 P1）。搜索面（/v1/search）与流地址/
//! 歌词面的 tracklink/song/info 端点是网络现取——确定性门禁内不可真跑。
//! 本文件钉住：
//! - (a) descriptor 账面依据（§12.4 行 10：authentication=false
//!   + AuthCapability::None，上游档序 Low128/High320/Flac）；
//! - (b) 流地址/歌词面的**离线双失败触发点**——空 tsid 的歌在两栈都在任何
//!   网络调用之前失败（musid source_qianqian.rs:791-793/:840-842 与 legacy
//!   qianqian/url.rs:39-41、lyric.rs:19-21 同键同 NotFound）；
//! - (c) manager 级真实双侧链路（空 tsid 歌在流地址面两栈同请求都失败 →
//!   MusidErrLegacyErr，legacy 错误原样向上）。
//!
//! 真网行为由运行期 probe 对账覆盖（docs §12.3）；**用户启用源：翻源后
//! 首周盯 MusidErrLegacyOk 计数**（§12.3）。tracklink 的
//! free_128/vip_nopath 双分支（VIP 曲目匿名恒 path-less → NotFound，上游
//! live-pinned）两侧同病同回落——无回退盲区，expireTime 进
//! expires_at_unix（G4 对该源设防）。装配形态与 `musid_bootstrap::
//! assemble!` 同形。

#![cfg(feature = "musid-dli-adapter-qianqian")]

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
    let provider = Arc::new(musid_dli::source_qianqian::QianqianProvider::new());
    AdapterMusicSource::new(
        SourceId::Qianqian,
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
            record.face == face && record.source == SourceId::Qianqian && record.outcome == outcome
        })
        .map(|record| record.count)
        .unwrap_or(0)
}

/// 真实 descriptor pin：注册表 qianqian 行（三面全 Migrated + 歌手/榜单/
/// 集合面恒旧路径）的依据逐位对上真实 provider——track_search/
/// stream_resolve/lyrics/playlists/albums/link_resolve true，artists/
/// charts 缺省 false（musid 无 ArtistProvider/ChartProvider，voicefox
/// artist 位恒旧路径、榜单面双侧皆无），authentication=false +
/// `AuthCapability::None`（登录面双侧皆无认证位）。
#[test]
fn real_descriptor_pins_the_v2_qianqian_ledger_row() {
    let provider = musid_dli::source_qianqian::QianqianProvider::new();
    assert_eq!(
        provider.capabilities(),
        ProviderCapabilities {
            track_search: true,
            stream_resolve: true,
            lyrics: true,
            playlists: true,
            albums: true,
            link_resolve: true,
            ..ProviderCapabilities::default()
        },
        "qianqian 真实 descriptor 漂移即红（§12.4 账面依据）"
    );
    assert_eq!(provider.auth_capability(), AuthCapability::None);
    assert_eq!(
        provider.supported_qualities(),
        vec![
            musid_dli::models::StreamQuality::Low,
            musid_dli::models::StreamQuality::High,
            musid_dli::models::StreamQuality::Lossless,
        ],
        "上游档序 Low128/High320/Flac（source_qianqian.rs:743-750）→ 适配层翻译产物同序三档"
    );
}

/// 流地址/歌词面的离线双失败触发点：空 tsid（id 空且 extra 无 tsid）的歌
/// 在**两栈**都在任何网络调用之前失败——musid `resolve`/`lyrics` 的
/// `Error::NotFound`（source_qianqian.rs:791-793、:840-842）→ 适配层
/// `FetchError::NotFound`；legacy url.rs:39-41 / lyric.rs:19-21 同键同
/// NotFound。manager 级双失败链见下方 manager 用例。
#[tokio::test]
async fn resolve_and_lyric_fail_offline_for_songs_without_a_tsid() {
    let adapter = real_adapter();
    let song = SongInfo::new(
        "".to_string(),
        SourceId::Qianqian,
        "等晴天".to_string(),
        "周深".to_string(),
    );
    let error = adapter
        .get_song_url(&song, Quality::High320)
        .await
        .expect_err("musid 无 tsid 必须离线失败");
    assert!(matches!(error, FetchError::NotFound), "实际: {error:?}");
}

/// 歌词面同触发点（依赖歌词转换变体——缺变体时适配层歌词面在 provider
/// 之前短路，与 migu 歌词用例同门）：空 tsid → `Error::NotFound` 离线 →
/// `FetchError::NotFound`（与 legacy lyric.rs:19-21 同口径衔接负缓存）。
#[cfg(feature = "musid-dli-adapter-lyrics")]
#[tokio::test]
async fn lyric_face_fails_offline_for_songs_without_a_tsid() {
    let adapter = real_adapter();
    let song = SongInfo::new(
        "".to_string(),
        SourceId::Qianqian,
        "等晴天".to_string(),
        "周深".to_string(),
    );
    let error = adapter
        .get_lyric(&song)
        .await
        .expect_err("musid 无 tsid 必须离线失败");
    assert!(matches!(error, FetchError::NotFound), "实际: {error:?}");
}

/// manager 级双侧真实链路（零网络）：真实 legacy 源 + 真实 musid 影子源 +
/// 注册表放行（三面 Migrated）→ 空 tsid 的歌在流地址面两栈同请求都失败
/// ——musid 侧 tsid 空 → Error::NotFound（离线）、legacy 侧 url.rs:39-41
/// 同键同 NotFound（离线）——计 MusidErrLegacyErr，legacy 错误原样向上。
/// 替换模式的完整失败链一次走通（真实 provider 的搜索/歌词/resolve 成功
/// 路径是网络面，由运行期 probe 对账覆盖，docs §12.3）。
#[tokio::test]
async fn manager_falls_back_to_the_real_legacy_source_when_musid_fails_offline() {
    let mut manager = SourceManager::new(SourceId::Kw, &[SourceId::Qianqian]);
    manager.register(Arc::new(lx_source::qianqian::QianqianSource::new()));
    let registered = lx_source::musid_bootstrap::register_available(
        &manager,
        &TransportInjection::new(None, 15),
    );
    assert!(
        registered.contains(&SourceId::Qianqian),
        "真实装配分支编译并生效"
    );
    manager.set_musid_routes(MusidRouteConfig {
        stack: true,
        search: true,
        song_url: true,
        lyric: true,
        parity_probe: false,
    });

    let song = SongInfo::new(
        "".to_string(),
        SourceId::Qianqian,
        "等晴天".to_string(),
        "周深".to_string(),
    );
    let error = manager
        .get_song_url(&song, Quality::High320)
        .await
        .expect_err("两栈同请求都失败");
    // legacy 侧 tsid 空 → FetchError::NotFound（qianqian/url.rs:39-41），
    // 内置先行臂把它收敛为错误文本（NotFound 的 Display 是 "not found"），
    // 统一出口再包 Other——既有旧代码行为逐字保持（manager.rs
    // get_song_url_inner 的 builtin_error 聚合，fivesing/migu/kugou 切片
    // 同款）。
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
