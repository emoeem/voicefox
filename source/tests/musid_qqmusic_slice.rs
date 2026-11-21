//! V2 切片 8（qqmusic/tx）：**真实** musid-dli `QqMusicProvider` 的行为对账
//! （与切片 1-7 同形，评审 P1）。tx 三个面都是网络现取（musicu.fcg 签名
//! POST：搜索/vkey resolve/歌词 GetPlayLyricInfo），确定性门禁内不可真跑
//! ——本文件钉住真实 provider 的 descriptor 账面依据（§12.4 行 8：
//! authentication=true 折叠 Cookie 的诊断位、无 ArtistProvider 的歌手面
//! 缺口、微信扫码双栈期恒 false）与歌词面的离线失败形态（extra 无 songId
//! 数值 → InvalidInput，任何网络调用之前，source_qqmusic.rs:873-877）。
//! 流地址面的 vkey 请求链（stream_vkey_ok/stream_vkey_empty_sip 形状）与
//! 登录态不对称（musid 恒匿名 vs cookie 登录的 VIP 收窄，P5 口径）由运行
//! 期 probe 对账覆盖（docs §12.3）；搜索/流地址/歌词的 fixture 映射与
//! parity 归一在 adapter_musid.rs::v2_registry 的 tx 用例 pin。装配形态与
//! `musid_bootstrap::assemble!` 同形。

#![cfg(feature = "musid-dli-adapter-qqmusic")]

use std::sync::Arc;

use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::{FetchError, MusicSource};
use lx_source::adapter_musid::{AdapterMusicSource, TransportInjection};
use musid_dli::provider::{AuthCapability, MusicProvider, ProviderCapabilities};

/// 真实 provider 按装配点同形拆三面（assemble! 的独立重放）。
fn real_adapter() -> AdapterMusicSource {
    let provider = Arc::new(musid_dli::source_qqmusic::QqMusicProvider::new());
    AdapterMusicSource::new(
        SourceId::Tx,
        format!("{}（musid-dli）", provider.name()),
        Some(provider.clone()),
        Some(provider.clone()),
        Some(provider),
        TransportInjection::new(None, 15),
    )
}

/// 真实 descriptor pin：注册表 tx 行（三面全 Migrated + 登录/扫码/歌手/
/// 集合面恒旧路径）的依据逐位对上真实 provider——track_search/
/// stream_resolve/lyrics/authentication/playlists/albums/charts/
/// link_resolve true，artists 缺省 false（musid 无 ArtistProvider 实现），
/// 无 auth_capability 覆写 → authentication bool 折叠
/// `AuthCapability::Cookie`（影子源恒匿名 new()，Cookie 只是 descriptor
/// 事实登记），supported_qualities 未覆写（缺省空）。
#[test]
fn real_descriptor_pins_the_v2_qqmusic_ledger_row() {
    let provider = musid_dli::source_qqmusic::QqMusicProvider::new();
    assert_eq!(
        provider.capabilities(),
        ProviderCapabilities {
            track_search: true,
            stream_resolve: true,
            lyrics: true,
            authentication: true,
            playlists: true,
            albums: true,
            charts: true,
            link_resolve: true,
            ..ProviderCapabilities::default()
        },
        "qqmusic 真实 descriptor 漂移即红（§12.4 账面依据）"
    );
    assert_eq!(
        provider.auth_capability(),
        AuthCapability::Cookie,
        "authentication=true 的缺省折叠（Cookie 机制登记；登录/扫码链路恒旧路径）"
    );
    assert!(
        provider.supported_qualities().is_empty(),
        "qqmusic 未覆写 supported_qualities（缺省空）→ 适配层翻译产物为空"
    );
}

/// 歌词面离线失败形态（musid 侧数据前提）：extra 无 songId 数值（songId
/// 键缺失或非数字）→ `Error::InvalidInput`（source_qqmusic.rs:873-877，
/// 任何网络调用之前）→ 适配层 `FetchError::Other`（§9 表 InvalidInput →
/// Other）。legacy 同请求走网络（songId 兜底 0 再 POST，tx/lyric.rs:31-59
/// 无离线守卫）——该形态在 manager 级不可确定性复现双失败，只在此 pin
/// musid 侧触发点；真实链路由运行期 probe 对账覆盖。依赖歌词转换变体
/// （缺变体时适配层歌词面在 provider 之前短路，与 qianqian/joox/migu
/// 歌词用例同门）。
#[cfg(feature = "musid-dli-adapter-lyrics")]
#[tokio::test]
async fn lyric_face_fails_offline_without_a_numeric_song_id() {
    let adapter = real_adapter();
    let mut song = SongInfo::new(
        "003aAYrm3GE0Ac".to_string(),
        SourceId::Tx,
        "晴天".to_string(),
        "周杰伦".to_string(),
    );
    song.extra
        .insert("strMediaMid".to_string(), "003aAYrm3GE0Ac".to_string());
    let error = adapter
        .get_lyric(&song)
        .await
        .expect_err("无数值 songId 必须离线失败");
    assert!(matches!(error, FetchError::Other(_)), "实际: {error:?}");
    assert!(
        error.to_string().contains("song id"),
        "InvalidInput 的 musid 侧语义文本（英文，§9 表：不可按中文子串匹配）: {error}"
    );
}
