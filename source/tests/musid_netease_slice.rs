//! V2 切片 9（netease/wy）：**真实** musid-dli `NeteaseProvider` 的行为对账
//! （与切片 1-7 同形，评审 P1）。wy 是主源且登录态依赖最重：三个面都是
//! 网络现取（eapi 加密 POST：搜索 batch / song/enhance/player/url/v1 /
//! song/lyric/v1），确定性门禁内不可真跑——本文件钉住真实 provider 的
//! descriptor 账面依据（§12.4 行 9 全家桶 capabilities + Cookie 诊断位）与
//! 搜索面的离线守卫（空关键词 → InvalidInput，任何网络调用之前，
//! source_netease.rs:461-463）。**登录态不对称是本片最重要的解读口径**：
//! musid 恒匿名 vs voicefox cookie 登录的结果集/档位系统性收窄（P5，
//! §11.8 (a)/(b)）——probe 对账计数在 wy 上必须先剥离该不对称再解读，
//! MusidErrLegacyOk 的常态高位不等于路由缺陷；真网行为由运行期 probe
//! 对账覆盖（docs §12.3）。搜索面的 fixture 映射与 parity 归一（±1s 时长
//! 容差、折档后 qualities 集合相等）在 adapter_musid.rs::v2_registry 的
//! wy 用例 pin。集合面前置登记：wy 歌单同步若依赖 by-id 详情
//! （songs_from_detail 类路径），前置是 musid-dli 0.2.0 无公开
//! per-provider detail trait（track_details 是 crate 内部方法，§11.4 第
//! 4 条），需上游先公开——本片不接集合面。装配形态与
//! `musid_bootstrap::assemble!` 同形。

#![cfg(feature = "musid-dli-adapter-netease")]

use std::sync::Arc;

use lx_core::model::source::SourceId;
use lx_core::traits::source::{MusicSource, SearchError};
use lx_source::adapter_musid::{AdapterMusicSource, TransportInjection};
use musid_dli::provider::{AuthCapability, MusicProvider, ProviderCapabilities};

/// 真实 provider 按装配点同形拆三面（assemble! 的独立重放）。
fn real_adapter() -> AdapterMusicSource {
    let provider = Arc::new(musid_dli::source_netease::NeteaseProvider::new());
    AdapterMusicSource::new(
        SourceId::Wy,
        format!("{}（musid-dli）", provider.name()),
        Some(provider.clone()),
        Some(provider.clone()),
        Some(provider),
        TransportInjection::new(None, 15),
    )
}

/// 真实 descriptor pin：注册表 wy 行（三面全 Migrated + 登录/扫码/用户
/// 歌单/集合面恒旧路径）的依据逐位对上真实 provider——track_search/
/// stream_resolve/lyrics/authentication/playlists/albums/artists/charts/
/// link_resolve 全 true（musid-dli source_netease.rs:443-456 全家桶），
/// 无 auth_capability 覆写 → authentication bool 折叠
/// `AuthCapability::Cookie`（影子源恒匿名 new()，Cookie 只是 descriptor
/// 事实登记），supported_qualities 未覆写（缺省空）。
#[test]
fn real_descriptor_pins_the_v2_netease_ledger_row() {
    let provider = musid_dli::source_netease::NeteaseProvider::new();
    assert_eq!(
        provider.capabilities(),
        ProviderCapabilities {
            track_search: true,
            stream_resolve: true,
            lyrics: true,
            authentication: true,
            playlists: true,
            albums: true,
            artists: true,
            charts: true,
            link_resolve: true,
            ..ProviderCapabilities::default()
        },
        "netease 真实 descriptor 漂移即红（§12.4 账面依据）"
    );
    assert_eq!(
        provider.auth_capability(),
        AuthCapability::Cookie,
        "authentication=true 的缺省折叠（Cookie 机制登记；登录/扫码/用户歌单恒旧路径）"
    );
    assert!(
        provider.supported_qualities().is_empty(),
        "netease 未覆写 supported_qualities（缺省空）→ 适配层翻译产物为空"
    );
}

/// 搜索面离线守卫：空关键词 → `Error::InvalidInput`（source_netease.rs
/// :461-463，任何网络调用之前）→ 适配层 `SearchError::Other`（§9 表
/// InvalidInput → Other）。musid 先行槽位对空关键词的失败发生在旧链重试
/// 之前（动态层，§12.2 第 2 层），本用例 pin 该触发点的错误分类与语义
/// 文本；普通关键词的搜索是网络面，由运行期 probe 对账覆盖。
#[tokio::test]
async fn search_face_fails_offline_for_an_empty_keyword() {
    let adapter = real_adapter();
    let error = adapter
        .search("", 1, 20)
        .await
        .expect_err("空关键词必须离线失败");
    assert!(matches!(error, SearchError::Other(_)), "实际: {error:?}");
    assert!(
        error.to_string().contains("keyword"),
        "InvalidInput 的 musid 侧语义文本（英文，§9 表）：{error}"
    );
}
