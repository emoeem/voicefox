//! V2 切片 11（joox，用户启用源）：**真实** musid-dli `JooxProvider` 的
//! 行为对账（与切片 1-7 同形，评审 P1）。三个面都是网络现取（OpenJOOX
//! web_get_songinfo / web_lyric JSONP），确定性门禁内不可真跑——本文件
//! 钉住：(a) descriptor 账面依据（§12.4 行 11：authentication=false +
//! AuthCapability::None，上游档序 Low128/High320）；(b) musid 侧的**离线
//! 失败触发点**——空 songid 的歌在 resolve/lyrics 都在任何网络调用之前
//! NotFound（source_joox.rs:754-756、:802-804）；legacy 同请求无离线守卫
//! （fetch_song_detail/fetch_lyric 直接走网络）——manager 级双失败链不可
//! 确定性复现，joox 的流地址面失败侧对账由本文件的 adapter 级用例 + 运行
//! 期 probe 承载（登记见 docs §12.2 joox 块）。真网行为由运行期 probe
//! 对账覆盖（docs §12.3）；**用户启用源：翻源后同 qianqian 盯
//! MusidErrLegacyOk 计数**。搜索面的 fixture 映射与 parity 归一在
//! adapter_musid.rs::v2_registry 的 joox 用例 pin（legacy 侧消费真实的
//! `joox::api::songs_from_search`，可见性扩宽登记见 §12.7）；封面面不接
//! musid ArtworkProvider（legacy get_cover_url 走歌曲模型回填，适配层同
//! 口径）。装配形态与 `musid_bootstrap::assemble!` 同形。

#![cfg(feature = "musid-dli-adapter-joox")]

use std::sync::Arc;

use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{FetchError, MusicSource};
use lx_source::adapter_musid::{AdapterMusicSource, TransportInjection};
use musid_dli::provider::{AuthCapability, MusicProvider, ProviderCapabilities};

/// 真实 provider 按装配点同形拆三面（assemble! 的独立重放）。
fn real_adapter() -> AdapterMusicSource {
    let provider = Arc::new(musid_dli::source_joox::JooxProvider::new());
    AdapterMusicSource::new(
        SourceId::Joox,
        format!("{}（musid-dli）", provider.name()),
        Some(provider.clone()),
        Some(provider.clone()),
        Some(provider),
        TransportInjection::new(None, 15),
    )
}

/// 真实 descriptor pin：注册表 joox 行（三面全 Migrated + 榜单/歌手双侧
/// 皆无 + 封面面不接 ArtworkProvider + 集合面恒旧路径）的依据逐位对上
/// 真实 provider——track_search/stream_resolve/lyrics/playlists/albums/
/// link_resolve true，artists/charts 缺省 false，authentication=false +
/// `AuthCapability::None`（登录面双侧皆无认证位）。
#[test]
fn real_descriptor_pins_the_v2_joox_ledger_row() {
    let provider = musid_dli::source_joox::JooxProvider::new();
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
        "joox 真实 descriptor 漂移即红（§12.4 账面依据）"
    );
    assert_eq!(provider.auth_capability(), AuthCapability::None);
    assert_eq!(
        provider.supported_qualities(),
        vec![
            musid_dli::models::StreamQuality::Low,
            musid_dli::models::StreamQuality::High,
        ],
        "上游档序 Low128/High320（source_joox.rs:717-720）→ 适配层翻译产物同序两档"
    );
}

/// 流地址面 musid 侧离线失败触发点：空 songid（id 空且 extra 无 songid）
/// → `Error::NotFound`（source_joox.rs:754-756，任何网络调用之前）→ 适配
/// 层 `FetchError::NotFound`。带加密串 songid 的歌走 fetch_song_detail
/// 网络（select_candidate 与 legacy get_song_url 同序：sort desc → 第一个
/// ≤ 请求档 → 首选兜底），由运行期 probe 对账覆盖。
#[tokio::test]
async fn resolve_fails_offline_for_songs_without_a_songid() {
    let adapter = real_adapter();
    let song = SongInfo::new(
        "".to_string(),
        SourceId::Joox,
        "晴天".to_string(),
        "周杰倫".to_string(),
    );
    let error = adapter
        .get_song_url(&song, Quality::High320)
        .await
        .expect_err("musid 无 songid 必须离线失败");
    assert!(matches!(error, FetchError::NotFound), "实际: {error:?}");
}

/// 歌词面同触发点（依赖歌词转换变体——缺变体时适配层歌词面在 provider
/// 之前短路，与 migu/qianqian 歌词用例同门）：空 songid →
/// `Error::NotFound` 离线（source_joox.rs:802-804）→
/// `FetchError::NotFound`（legacy fetch_lyric 后的 `ok_or(NotFound)` 与
/// 负缓存同口径衔接，musid-dli source_joox.rs LyricsProvider 分支）。
#[cfg(feature = "musid-dli-adapter-lyrics")]
#[tokio::test]
async fn lyric_face_fails_offline_for_songs_without_a_songid() {
    let adapter = real_adapter();
    let song = SongInfo::new(
        "".to_string(),
        SourceId::Joox,
        "晴天".to_string(),
        "周杰倫".to_string(),
    );
    let error = adapter
        .get_lyric(&song)
        .await
        .expect_err("musid 无 songid 必须离线失败");
    assert!(matches!(error, FetchError::NotFound), "实际: {error:?}");
}
