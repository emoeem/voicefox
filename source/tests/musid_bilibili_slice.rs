//! V2 切片 4（bilibili）：**真实** musid-dli `BilibiliProvider` 的行为对账
//! （与切片 1/2/3 同形，评审 P1）。本切片的三件结构前置（子 feature
//! `musid-dli-adapter-bilibili` / bootstrap 匿名装配分支 / manager 放开
//! matches!）由本文件与 bootstrap 切片测试钉住。bilibili 各面都是网络现取
//! （wbi 签名搜索 / playurl DASH），确定性门禁内不可真跑——本文件钉住真实
//! provider 的 descriptor 账面依据（§12.4 行 4）、离线守卫（空关键词 /
//! 错 provider track）；manager 级路由行为（搜索/流地址 MusidOk、歌词面
//! LegacyOnly 零调用）由 `musid_dual_stack.rs` 的注册表用例承载。真网行为
//! （匿名 resolve 带 try_look=1 的可用性）由运行期 probe 对账覆盖
//! （docs §12.3；本切片不实测，§12.2 如实登记）。装配形态与
//! `musid_bootstrap::assemble!` 同形。

#![cfg(feature = "musid-dli-adapter-bilibili")]

use std::sync::Arc;

use lx_core::model::source::SourceId;
use lx_core::traits::source::{MusicSource, SearchError};
use lx_source::adapter_musid::{AdapterMusicSource, TransportInjection};
use musid_dli::StreamResolver as _;
use musid_dli::models::StreamQuality;
use musid_dli::provider::{AuthCapability, MusicProvider, ProviderCapabilities};

/// 真实 provider 按装配点同形拆三面（assemble! 的独立重放）。
fn real_adapter() -> AdapterMusicSource {
    let provider = Arc::new(musid_dli::source_bilibili::BilibiliProvider::new());
    AdapterMusicSource::new(
        SourceId::Bili,
        format!("{}（musid-dli）", provider.name()),
        Some(provider.clone()),
        Some(provider.clone()),
        Some(provider),
        TransportInjection::new(None, 15),
    )
}

/// 真实 descriptor pin：注册表 bili 行（搜索/流地址 Migrated + 歌词
/// LegacyOnly + 集合面不设行项）的依据逐位对上真实 provider——
/// track_search/stream_resolve/authentication/playlists/charts true，
/// lyrics **false**（硬 stub，双侧皆无），无 auth_capability 覆写 →
/// authentication bool 折叠 `AuthCapability::Cookie`（traits.rs:82-88
/// 缺省推导；影子源恒匿名 new()，Cookie 机制只是 descriptor 事实登记，
/// 登录链路恒旧路径），supported_qualities 未覆写（缺省空）。
#[test]
fn real_descriptor_pins_the_v2_bilibili_ledger_row() {
    let provider = musid_dli::source_bilibili::BilibiliProvider::new();
    assert_eq!(
        provider.capabilities(),
        ProviderCapabilities {
            track_search: true,
            stream_resolve: true,
            authentication: true,
            playlists: true,
            charts: true,
            ..ProviderCapabilities::default()
        },
        "bilibili 真实 descriptor 漂移即红（§12.4 账面依据）"
    );
    assert_eq!(
        provider.auth_capability(),
        AuthCapability::Cookie,
        "authentication=true 的缺省折叠（Cookie/SESSDATA 机制登记；影子源恒匿名）"
    );
    assert!(
        provider.supported_qualities().is_empty(),
        "bilibili 未覆写 supported_qualities（缺省空）→ 适配层翻译产物为空"
    );
}

/// 空关键词在**任何网络调用之前**失败（source_bilibili.rs:922-925）→
/// 适配层 InvalidInput 映射 SearchError::Other（§9 表）。
#[tokio::test]
async fn empty_keyword_search_fails_offline() {
    let adapter = real_adapter();
    let error = adapter
        .search("", 1, 20)
        .await
        .expect_err("真实 provider 空关键词必须离线失败");
    assert!(matches!(error, SearchError::Other(_)), "实际: {error}");
}

/// 错 provider 的 track 在 resolve 守卫处**任何网络调用之前**失败
/// （source_bilibili.rs:1018-1021，InvalidInput）——守卫与 adapter 的
/// song_to_track 保真（song.source 直译 provider）共同保证 bilibili
/// 影子源不会替其它源取址。
#[tokio::test]
async fn resolve_guards_against_wrong_provider_tracks_offline() {
    let provider = musid_dli::source_bilibili::BilibiliProvider::new();
    let foreign = musid_dli::models::Track {
        id: musid_dli::models::TrackId::new("netease", "789"),
        title: "别家的歌".into(),
        artists: vec![],
        album: None,
        album_id: None,
        duration: None,
        artwork_url: None,
        isrc: None,
        available_qualities: vec![],
        audio: None,
        extra: Default::default(),
    };
    let error = provider
        .resolve(
            &foreign,
            &musid_dli::provider::ResolveOptions {
                quality: StreamQuality::High,
            },
        )
        .await
        .expect_err("错 provider track 必须离线失败");
    assert!(error.to_string().to_lowercase().contains("bilibili"));

    // 歌词面（LegacyOnly 的 musid 侧依据）：硬 stub 恒
    // UnsupportedOperation（"no lyric API exists on Bilibili"）——注册表
    // 拒绝该面后这类错误到不了用户（§12.2 静态层）；此处 pin stub 形态
    // 供账面复核。
    let lyric_error = musid_dli::provider::LyricsProvider::lyrics(
        &provider,
        &musid_dli::models::Track {
            id: musid_dli::models::TrackId::new("bilibili", "BV1xx411c7mD"),
            title: "测试视频".into(),
            artists: vec![],
            album: None,
            album_id: None,
            duration: None,
            artwork_url: None,
            isrc: None,
            available_qualities: vec![],
            audio: None,
            extra: Default::default(),
        },
    )
    .await
    .expect_err("musid bili 歌词面是硬 stub");
    assert!(
        lyric_error.to_string().contains("no lyric API"),
        "stub 文本: {lyric_error}"
    );
}
