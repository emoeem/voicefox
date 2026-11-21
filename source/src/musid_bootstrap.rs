//! musid-dli 按源子 feature → [`AdapterMusicSource`] 的装配点（V1，opt-in）。
//!
//! 每个 `musid-dli-adapter-<source>` 子 feature 对应一个装配分支；全部
//! provider 走 musid-dli 0.2.0 的无参 `new()`（内置 `provider_default`
//! 传输配置：15s 总超时、无代理）。voicefox 的代理/超时注入缺口维持
//! docs/MUSID_DLI_ADAPTER.md §4/§11 的登记：0.2.0 的 `transport` 模块与
//! `with_transport` 均 `pub(crate)`，[`TransportInjection`] 只记录预算
//! 供诊断展示；proxy/超时继续只驱动 voicefox 内置音源
//! （`lx_source::http::configure`）。升级前置两步（上游公开 transport
//! feature + voicefox 开 source-* 子 feature）缺一不可，不做 path 依赖
//! 或 fork 绕过。
//!
//! bilibili（V2 切片 4 起）：装配分支与其它源同形，但 `BilibiliProvider`
//! 恒匿名 `new()`（§11.8 双栈零凭据流动）——SESSDATA 扫码登录/会话/用户
//! 数据完全走 voicefox 旧路径（manager 登录链路不读影子 map），musid 只
//! 路由搜索/流地址两面（歌词面双侧皆无，注册表 LegacyOnly）。

use std::sync::Arc;

use lx_core::model::source::SourceId;
use lx_core::traits::source::MusicSource;
use musid_dli::provider::MusicProvider;

use crate::adapter_musid::{AdapterMusicSource, TransportInjection};
use crate::manager::SourceManager;

/// 装配并注册本次构建启用的全部 musid 影子源，返回已注册的
/// [`SourceId`]（供启动日志/诊断对照）。
pub fn register_available(
    manager: &SourceManager,
    transport: &TransportInjection,
) -> Vec<SourceId> {
    // 子 feature 全关：装配体为空，参数按 cfg 静默（类型引用保住 import）。
    // V4-s1 起 joox 不在本列——它由 `SourceManager::new` 直装主注册表。
    #[cfg(not(any(
        feature = "musid-dli-adapter-netease",
        feature = "musid-dli-adapter-qqmusic",
        feature = "musid-dli-adapter-kugou",
        feature = "musid-dli-adapter-kuwo",
        feature = "musid-dli-adapter-migu",
        feature = "musid-dli-adapter-jamendo",
        feature = "musid-dli-adapter-apple",
        feature = "musid-dli-adapter-fivesing",
        feature = "musid-dli-adapter-bilibili",
        feature = "musid-dli-adapter-qianqian",
    )))]
    {
        let _ = manager;
        let _ = transport;
        let _ = std::marker::PhantomData::<(AdapterMusicSource, dyn MusicProvider)>;
    }
    #[allow(unused_mut)]
    let mut registered = Vec::new();
    #[allow(dead_code)]
    fn register_built(
        manager: &SourceManager,
        registered: &mut Vec<SourceId>,
        adapter: Option<Arc<dyn MusicSource>>,
    ) {
        if let Some(adapter) = adapter {
            let id = adapter.id();
            manager.register_musid_source(adapter);
            registered.push(id);
        }
    }

    #[cfg(feature = "musid-dli-adapter-netease")]
    register_built(manager, &mut registered, netease(transport));
    #[cfg(feature = "musid-dli-adapter-qqmusic")]
    register_built(manager, &mut registered, qqmusic(transport));
    #[cfg(feature = "musid-dli-adapter-kugou")]
    register_built(manager, &mut registered, kugou(transport));
    #[cfg(feature = "musid-dli-adapter-kuwo")]
    register_built(manager, &mut registered, kuwo(transport));
    #[cfg(feature = "musid-dli-adapter-migu")]
    register_built(manager, &mut registered, migu(transport));
    #[cfg(feature = "musid-dli-adapter-jamendo")]
    register_built(manager, &mut registered, jamendo(transport));
    #[cfg(feature = "musid-dli-adapter-apple")]
    register_built(manager, &mut registered, apple(transport));
    #[cfg(feature = "musid-dli-adapter-fivesing")]
    register_built(manager, &mut registered, fivesing(transport));
    #[cfg(feature = "musid-dli-adapter-bilibili")]
    register_built(manager, &mut registered, bilibili(transport));
    #[cfg(feature = "musid-dli-adapter-qianqian")]
    register_built(manager, &mut registered, qianqian(transport));

    // V4-s1（§14.1）：joox 不在此影子注册——legacy 已删，适配面由
    // `SourceManager::new` 直接装配进主注册表（唯一实现路径），见 joox()
    // 的装配分支。

    registered
}

/// 共用装配：同一个 provider 实例按 capability trait 拆成三个面（Arc 克隆
/// 共享同一分配），`Arc<T>` → 各 trait object。
#[allow(unused_macros)]
macro_rules! assemble {
    ($transport:expr, $provider:ty, $source:expr) => {{
        let provider = Arc::new(<$provider as std::default::Default>::default());
        let adapter: Option<Arc<dyn MusicSource>> = Some(Arc::new(AdapterMusicSource::new(
            $source,
            format!("{}（musid-dli）", provider.name()),
            Some(provider.clone()),
            Some(provider.clone()),
            Some(provider),
            ($transport).clone(),
        )));
        adapter
    }};
}

#[cfg(feature = "musid-dli-adapter-netease")]
fn netease(transport: &TransportInjection) -> Option<Arc<dyn MusicSource>> {
    assemble!(
        transport,
        musid_dli::source_netease::NeteaseProvider,
        SourceId::Wy
    )
}

#[cfg(feature = "musid-dli-adapter-qqmusic")]
fn qqmusic(transport: &TransportInjection) -> Option<Arc<dyn MusicSource>> {
    assemble!(
        transport,
        musid_dli::source_qqmusic::QqMusicProvider,
        SourceId::Tx
    )
}

#[cfg(feature = "musid-dli-adapter-kugou")]
fn kugou(transport: &TransportInjection) -> Option<Arc<dyn MusicSource>> {
    assemble!(
        transport,
        musid_dli::source_kugou::KugouProvider,
        SourceId::Kg
    )
}

#[cfg(feature = "musid-dli-adapter-kuwo")]
fn kuwo(transport: &TransportInjection) -> Option<Arc<dyn MusicSource>> {
    assemble!(
        transport,
        musid_dli::source_kuwo::KuwoProvider,
        SourceId::Kw
    )
}

#[cfg(feature = "musid-dli-adapter-migu")]
fn migu(transport: &TransportInjection) -> Option<Arc<dyn MusicSource>> {
    assemble!(
        transport,
        musid_dli::source_migu::MiguProvider,
        SourceId::Mg
    )
}

#[cfg(feature = "musid-dli-adapter-jamendo")]
fn jamendo(transport: &TransportInjection) -> Option<Arc<dyn MusicSource>> {
    assemble!(
        transport,
        musid_dli::source_jamendo::JamendoProvider,
        SourceId::Jamendo
    )
}

#[cfg(feature = "musid-dli-adapter-apple")]
fn apple(transport: &TransportInjection) -> Option<Arc<dyn MusicSource>> {
    assemble!(
        transport,
        musid_dli::source_apple::AppleMusicProvider,
        SourceId::Apple
    )
}

#[cfg(feature = "musid-dli-adapter-fivesing")]
fn fivesing(transport: &TransportInjection) -> Option<Arc<dyn MusicSource>> {
    assemble!(
        transport,
        musid_dli::source_fivesing::FivesingProvider,
        SourceId::Fivesing
    )
}

/// bilibili（V2 切片 4）：与其它源同形的匿名装配——`BilibiliProvider` 的
/// SESSDATA 会话槽在无参 `new()` 下恒空，登录态曲目走旧路径（§11.8 (a)
/// 匿名退化口径，注册表 bili 行登记）。
#[cfg(feature = "musid-dli-adapter-bilibili")]
fn bilibili(transport: &TransportInjection) -> Option<Arc<dyn MusicSource>> {
    assemble!(
        transport,
        musid_dli::source_bilibili::BilibiliProvider,
        SourceId::Bili
    )
}

#[cfg(feature = "musid-dli-adapter-qianqian")]
fn qianqian(transport: &TransportInjection) -> Option<Arc<dyn MusicSource>> {
    assemble!(
        transport,
        musid_dli::source_qianqian::QianqianProvider,
        SourceId::Qianqian
    )
}

/// joox（V4-s1 切片 1，docs/MUSID_DLI_ADAPTER.md §14.1）：legacy 实现已删，
/// 本装配分支是 joox 的**唯一**实现路径——集合面（歌单/专辑/链接直解）
/// 与 search/url/lyrics 四槽同实例装配（musid `JooxProvider` 按 trait 拆
/// 面：CollectionProvider/PlaylistProvider/AlbumProvider/LinkResolver 均
/// 为 M4 的 legacy 逐字移植，§12.7 行 11 的账面依据）。子 feature 自本
/// 切片起进 lx-source default。
#[cfg(feature = "musid-dli-adapter-joox")]
pub(crate) fn joox(transport: &TransportInjection) -> Option<Arc<dyn MusicSource>> {
    let provider =
        Arc::new(<musid_dli::source_joox::JooxProvider as std::default::Default>::default());
    let adapter = AdapterMusicSource::new(
        SourceId::Joox,
        format!("{}（musid-dli）", provider.name()),
        Some(provider.clone()),
        Some(provider.clone()),
        Some(provider.clone()),
        transport.clone(),
    )
    .with_collections(
        provider.clone(),
        provider.clone(),
        provider.clone(),
        provider,
    );
    Some(Arc::new(adapter))
}

// V2 切片 1（jamendo，docs/MUSID_DLI_ADAPTER.md §12）：真实 provider 的
// 编译期覆盖（评审 P1）——装配分支在全仓任何门禁里不可见时"写错路径也不
// 会红"；本测试模块挂 `musid-dli-adapter-jamendo` 门，子 feature 一开就
// 编译 `musid_dli::source_jamendo::JamendoProvider` 路径并 pin 装配产物
// （id/显示名/descriptor 位）。真实 provider 的行为对账见
// `source/tests/musid_jamendo_slice.rs`。
#[cfg(all(test, feature = "musid-dli-adapter-jamendo"))]
mod jamendo_slice_tests {
    use super::*;
    use lx_core::traits::source::SourceCapabilities;

    #[test]
    fn jamendo_assemble_branch_builds_the_real_provider() {
        let adapter = jamendo(&TransportInjection::new(None, 15))
            .expect("jamendo 子 feature 在：装配分支必须产出适配源");
        assert_eq!(adapter.id(), SourceId::Jamendo);
        assert_eq!(adapter.name(), "Jamendo（musid-dli）");
        // 真实 descriptor 位 pin：jamendo capabilities = track_search/
        // stream_resolve true，集合面（playlists/albums/link_resolve）与
        // 歌词面恒 false（container endpoints 匿名挂起 + 歌词 stub），
        // 经 §2.8 粒度表映射后 voicefox 位全 default——注册表 jamendo 行
        // 的"集合面不设行项"依据即此。
        assert_eq!(
            adapter.capabilities(),
            SourceCapabilities::default(),
            "真实 jamendo descriptor 的集合/登录位必须全 false（漂移即红）"
        );
        // supported_qualities 缺省空（musid-dli TrackSearch 缺省实现，
        // jamendo 未覆写）→ 适配层翻译产物为空。
        assert!(adapter.supported_qualities().is_empty());
    }

    #[test]
    fn jamendo_slice_is_registered_and_covers_the_migrated_ledger_row() {
        let manager = crate::manager::SourceManager::new(SourceId::Kw, &[SourceId::Jamendo]);
        let transport = TransportInjection::new(None, 15);
        let registered = register_available(&manager, &transport);
        assert!(
            registered.contains(&SourceId::Jamendo),
            "jamendo 子 feature 在：register_available 必须装配 jamendo 影子源"
        );
        // 账面 Migrated 的源在本次构建全部装配（P4：账面与构建形态无脱节）。
        // 切片 2（fivesing）起账面有两个 Migrated 源：单子 feature 构建下
        // 另一 Migrated 源未装配属预期 warn 形态（§12.1 生效前提），故按
        // "不含本切片源"断言；全等空断言由下方双组合测试钉住。
        assert!(
            !crate::adapter_musid::unregistered_migrated_sources(&registered)
                .contains(&SourceId::Jamendo),
            "jamendo 已装配，不得出现在账面/构建脱节 warn 输入里"
        );
    }
}

// V2 切片 2（fivesing，docs/MUSID_DLI_ADAPTER.md §12.4）：与 jamendo 同形的
// 真实 provider 编译期覆盖——子 feature 一开就编译
// `musid_dli::source_fivesing::FivesingProvider` 路径并 pin 装配产物
// （id/显示名/descriptor 位/supported_qualities）与 register_available 装配。
// 真实 provider 的行为对账见 `source/tests/musid_fivesing_slice.rs`。
#[cfg(all(test, feature = "musid-dli-adapter-fivesing"))]
mod fivesing_slice_tests {
    use super::*;

    #[test]
    fn fivesing_assemble_branch_builds_the_real_provider() {
        let adapter = fivesing(&TransportInjection::new(None, 15))
            .expect("fivesing 子 feature 在：装配分支必须产出适配源");
        assert_eq!(adapter.id(), SourceId::Fivesing);
        assert_eq!(adapter.name(), "5sing（musid-dli）");
        // 真实 descriptor 位 pin：fivesing capabilities =
        // track_search/stream_resolve/lyrics/playlists/link_resolve true
        // （musid-dli source_fivesing.rs:494-503），authentication=false +
        // AuthCapability::None（无账号系统，上游注册测试 pin）。经 §2.8
        // 粒度表映射：playlists + link_parse true；歌词/搜索/流地址三面无
        // voicefox 位；login/qr_login false（authentication=false 且非
        // Cookie 机制）；面级 flags 装配点未探测，playlist_search 等 false
        // （§11.3）——这正是注册表 fivesing 行"集合面不设行项/歌单面恒旧
        // 路径"的登记依据（影子源不实现集合方法，capabilities 位只是诊断
        // 对照）。
        let capabilities = adapter.capabilities();
        assert!(capabilities.playlists);
        assert!(capabilities.link_parse);
        assert!(
            !capabilities.playlist_search && !capabilities.playlist_categories,
            "面级 flags 装配点未探测（§11.3），漂移即红"
        );
        assert!(!capabilities.login && !capabilities.qr_login, "无登录态");
        assert!(
            !capabilities.album && !capabilities.artist && !capabilities.leaderboard,
            "专辑/歌手/榜单双侧同缺（§12.4 行 2）"
        );
        // supported_qualities：musid-dli FivesingProvider 未覆写（缺省空）
        // → 适配层翻译产物为空（legacy 侧声明 [Low128, High320, Flac]，
        // 差异随 descriptor 走，不进路由判定）。
        assert!(adapter.supported_qualities().is_empty());
    }

    #[test]
    fn fivesing_slice_is_registered_and_covers_the_migrated_ledger_row() {
        let manager = crate::manager::SourceManager::new(SourceId::Kw, &[SourceId::Fivesing]);
        let transport = TransportInjection::new(None, 15);
        let registered = register_available(&manager, &transport);
        assert!(
            registered.contains(&SourceId::Fivesing),
            "fivesing 子 feature 在：register_available 必须装配 fivesing 影子源"
        );
        // 单子 feature 构建下另一 Migrated 源（jamendo）未装配属预期 warn
        // 形态（§12.1 生效前提），按"不含本切片源"断言；全等空断言由下方
        // 双组合测试钉住。
        assert!(
            !crate::adapter_musid::unregistered_migrated_sources(&registered)
                .contains(&SourceId::Fivesing),
            "fivesing 已装配，不得出现在账面/构建脱节 warn 输入里"
        );
    }
}

// 双 Migrated 切片组合（jamendo + fivesing 子 feature 同开）：账面与构建
// 形态零脱节的全等断言（P4 助手的运行期形态）。单子 feature 构建下的部分
// 装配由上面两个切片测试的"不含本切片源"口径覆盖；CI 的 fivesing 组合步
// 跑到本测试。
#[cfg(all(
    test,
    feature = "musid-dli-adapter-jamendo",
    feature = "musid-dli-adapter-fivesing"
))]
#[test]
fn migrated_slices_assemble_without_ledger_gap() {
    let manager =
        crate::manager::SourceManager::new(SourceId::Kw, &[SourceId::Jamendo, SourceId::Fivesing]);
    let registered = register_available(&manager, &TransportInjection::new(None, 15));
    assert!(
        registered.contains(&SourceId::Jamendo) && registered.contains(&SourceId::Fivesing),
        "两个 Migrated 切片的装配分支都编译并生效"
    );
    // 全量 Migrated 账面（切片 1-11 起共 11 源）要求全等空——单/双子
    // feature 组合构建下其余 Migrated 源未装配属预期 warn 形态（§12.1
    // 生效前提），全等空断言只在"全部 Migrated 子 feature 同开"的组合构建
    // 下成立；CI 的十一源组合步（+ 歌词变体）跑到本分支。
    let all_migrated = [
        SourceId::Jamendo,
        SourceId::Fivesing,
        SourceId::Apple,
        SourceId::Bili,
        SourceId::Mg,
        SourceId::Kw,
        SourceId::Kg,
        SourceId::Tx,
        SourceId::Wy,
        SourceId::Qianqian,
        SourceId::Joox,
    ];
    let gaps = crate::adapter_musid::unregistered_migrated_sources(&registered);
    if all_migrated
        .iter()
        .all(|source| registered.contains(source))
    {
        assert_eq!(
            gaps,
            Vec::new(),
            "注册表 Migrated 行必须全部有影子源（否则启动 warn）"
        );
    } else {
        assert!(
            !gaps.contains(&SourceId::Jamendo) && !gaps.contains(&SourceId::Fivesing),
            "本组合构建装配的两个切片源不得进账面/构建脱节 warn 输入"
        );
    }
}

// V2 切片 3（apple，docs/MUSID_DLI_ADAPTER.md §12.4）：与 jamendo/fivesing
// 同形的真实 provider 编译期覆盖——子 feature 一开就编译
// `musid_dli::source_apple::AppleMusicProvider` 路径并 pin 装配产物
// （id/显示名/descriptor 位/preview-only 档位声明）与 register_available
// 装配。真实 provider 的行为对账见 `source/tests/musid_apple_slice.rs`。
#[cfg(all(test, feature = "musid-dli-adapter-apple"))]
mod apple_slice_tests {
    use super::*;
    use lx_core::model::source::Quality;

    #[test]
    fn apple_assemble_branch_builds_the_real_provider() {
        let adapter = apple(&TransportInjection::new(None, 15))
            .expect("apple 子 feature 在：装配分支必须产出适配源");
        assert_eq!(adapter.id(), SourceId::Apple);
        assert_eq!(adapter.name(), "Apple Music（musid-dli）");
        // 真实 descriptor 位 pin：apple capabilities =
        // track_search/stream_resolve/lyrics/playlists/albums/link_resolve
        // true（musid-dli source_apple.rs:586-610），authentication=false +
        // 无 auth_capability 覆写 → AuthCapability::None（登录态恒
        // LoggedOut，source_apple.rs:783-798 注释 pin）。经 §2.8 粒度表
        // 映射：playlists/album/link_parse true；login/qr_login false；
        // 歌手/榜单双侧皆无。面级 flags 装配点未探测（§11.3）。
        let capabilities = adapter.capabilities();
        assert!(capabilities.playlists);
        assert!(capabilities.album);
        assert!(capabilities.link_parse);
        assert!(
            !capabilities.playlist_search && !capabilities.playlist_categories,
            "面级 flags 装配点未探测（§11.3），漂移即红"
        );
        assert!(!capabilities.login && !capabilities.qr_login, "无登录态");
        assert!(
            !capabilities.artist && !capabilities.leaderboard,
            "歌手/榜单双侧皆无（§12.4 行 3）"
        );
        // supported_qualities：descriptor 只声明 preview-only 单档
        // [Low]（source_apple.rs:615-617）→ 适配层翻译产物 [Low128]。
        assert_eq!(adapter.supported_qualities(), vec![Quality::Low128]);
    }

    #[test]
    fn apple_slice_is_registered_and_covers_the_migrated_ledger_row() {
        let manager = crate::manager::SourceManager::new(SourceId::Kw, &[SourceId::Apple]);
        let transport = TransportInjection::new(None, 15);
        let registered = register_available(&manager, &transport);
        assert!(
            registered.contains(&SourceId::Apple),
            "apple 子 feature 在：register_available 必须装配 apple 影子源"
        );
        // 单子 feature 构建下其余 Migrated 源未装配属预期 warn 形态
        // （§12.1 生效前提），按"不含本切片源"断言；全等空断言由
        // migrated_slices_assemble_without_ledger_gap 的七源组合分支钉住。
        assert!(
            !crate::adapter_musid::unregistered_migrated_sources(&registered)
                .contains(&SourceId::Apple),
            "apple 已装配，不得出现在账面/构建脱节 warn 输入里"
        );
    }
}

// V2 切片 4（bilibili，docs/MUSID_DLI_ADAPTER.md §12.4）：三件结构前置的
// 装配件——子 feature + 装配分支（恒匿名）+ 本测试的 descriptor pin。登录
// 链路（SESSDATA）不在影子源上：AuthenticationProvider 实现只在 musid 侧
// 内部，voicefox 登录方法不读影子 map。真实 provider 的行为对账见
// `source/tests/musid_bilibili_slice.rs`。
#[cfg(all(test, feature = "musid-dli-adapter-bilibili"))]
mod bilibili_slice_tests {
    use super::*;

    #[test]
    fn bilibili_assemble_branch_builds_the_real_provider() {
        let adapter = bilibili(&TransportInjection::new(None, 15))
            .expect("bilibili 子 feature 在：装配分支必须产出适配源");
        assert_eq!(adapter.id(), SourceId::Bili);
        assert_eq!(adapter.name(), "哔哩哔哩（musid-dli）");
        // 真实 descriptor 位 pin：bilibili capabilities =
        // track_search/stream_resolve/authentication/playlists/charts true
        // （musid-dli source_bilibili.rs:907-916），无 auth_capability 覆写
        // → authentication bool 折叠 AuthCapability::Cookie（traits.rs:82-88
        // 缺省推导）。经 §2.8 粒度表映射：playlists/leaderboard true，
        // login=true（Cookie 折叠——诊断对照位，登录链路仍恒旧路径），
        // qr_login=false（Cookie ≠ Qr 注册），album/artist/link_parse/
        // playlist_search/playlist_categories false。
        let capabilities = adapter.capabilities();
        assert!(capabilities.playlists);
        assert!(capabilities.leaderboard);
        assert!(
            capabilities.login,
            "authentication=true 折叠 Cookie → 诊断位 login=true（§2.8；不改变登录链路走向）"
        );
        assert!(
            !capabilities.qr_login,
            "musid bili 无 Qr 注册 → qr_login=false；扫码恒走 voicefox 旧路径"
        );
        assert!(
            !capabilities.album && !capabilities.artist && !capabilities.link_parse,
            "集合面（除歌单/榜单）不路由登记（§12.4 行 4）"
        );
        // supported_qualities 未覆写（缺省空）→ 适配层翻译产物为空。
        assert!(adapter.supported_qualities().is_empty());
    }

    #[test]
    fn bilibili_slice_is_registered_and_covers_the_migrated_ledger_row() {
        let manager = crate::manager::SourceManager::new(SourceId::Kw, &[SourceId::Bili]);
        let transport = TransportInjection::new(None, 15);
        let registered = register_available(&manager, &transport);
        assert!(
            registered.contains(&SourceId::Bili),
            "bilibili 子 feature 在：register_available 必须装配 bilibili 影子源"
        );
        assert!(
            !crate::adapter_musid::unregistered_migrated_sources(&registered)
                .contains(&SourceId::Bili),
            "bilibili 已装配，不得出现在账面/构建脱节 warn 输入里"
        );
    }
}

// V2 切片 5（migu，docs/MUSID_DLI_ADAPTER.md §12.4）：与 apple 同形的真实
// provider 编译期覆盖。真实 provider 的行为对账见
// `source/tests/musid_migu_slice.rs`。
#[cfg(all(test, feature = "musid-dli-adapter-migu"))]
mod migu_slice_tests {
    use super::*;

    #[test]
    fn migu_assemble_branch_builds_the_real_provider() {
        let adapter = migu(&TransportInjection::new(None, 15))
            .expect("migu 子 feature 在：装配分支必须产出适配源");
        assert_eq!(adapter.id(), SourceId::Mg);
        assert_eq!(adapter.name(), "咪咕音乐（musid-dli）");
        // 真实 descriptor 位 pin：migu capabilities =
        // track_search/stream_resolve/lyrics/playlists/albums/charts/
        // link_resolve true（musid-dli source_migu.rs:1388-1397），
        // authentication=false + 无 auth_capability 覆写 →
        // AuthCapability::None（匿名 sid 设备会话不置 login 位，§2.8
        // 机制细分）。经 §2.8 粒度表映射：playlists/album/leaderboard/
        // link_parse true；login/qr_login false；artist false（musid 无
        // ArtistProvider，voicefox artist 位恒旧路径）。
        let capabilities = adapter.capabilities();
        assert!(capabilities.playlists);
        assert!(capabilities.album);
        assert!(capabilities.leaderboard);
        assert!(capabilities.link_parse);
        assert!(
            !capabilities.login && !capabilities.qr_login,
            "匿名 sid 机制（AuthCapability::None 折叠）不置 login 位（§2.8）"
        );
        assert!(
            !capabilities.artist,
            "musid 无 ArtistProvider 实现——voicefox artist 位恒旧路径（登记缺口）"
        );
        assert!(
            !capabilities.playlist_search && !capabilities.playlist_categories,
            "面级 flags 装配点未探测（§11.3），漂移即红"
        );
        // supported_qualities 未覆写（缺省空）→ 适配层翻译产物为空。
        assert!(adapter.supported_qualities().is_empty());
    }

    #[test]
    fn migu_slice_is_registered_and_covers_the_migrated_ledger_row() {
        let manager = crate::manager::SourceManager::new(SourceId::Kw, &[SourceId::Mg]);
        let transport = TransportInjection::new(None, 15);
        let registered = register_available(&manager, &transport);
        assert!(
            registered.contains(&SourceId::Mg),
            "migu 子 feature 在：register_available 必须装配 migu 影子源"
        );
        assert!(
            !crate::adapter_musid::unregistered_migrated_sources(&registered)
                .contains(&SourceId::Mg),
            "migu 已装配，不得出现在账面/构建脱节 warn 输入里"
        );
    }
}

// V2 切片 6（kuwo，docs/MUSID_DLI_ADAPTER.md §12.4）：与 migu 同形的真实
// provider 编译期覆盖。真实 provider 的行为对账见
// `source/tests/musid_kuwo_slice.rs`。
#[cfg(all(test, feature = "musid-dli-adapter-kuwo"))]
mod kuwo_slice_tests {
    use super::*;

    #[test]
    fn kuwo_assemble_branch_builds_the_real_provider() {
        let adapter = kuwo(&TransportInjection::new(None, 15))
            .expect("kuwo 子 feature 在：装配分支必须产出适配源");
        assert_eq!(adapter.id(), SourceId::Kw);
        assert_eq!(adapter.name(), "酷我音乐（musid-dli）");
        // 真实 descriptor 位 pin：kuwo capabilities =
        // track_search/stream_resolve/lyrics/playlists/albums/charts/
        // link_resolve true（musid-dli source_kuwo.rs:1918-1930），
        // authentication=false + auth_capability() 覆写
        // AuthCapability::Anonymous（kuwo web token 非账号登录，
        // source_kuwo.rs:1936-1940 上游测试 pin）。经 §2.8 机制细分：
        // Anonymous 非 Cookie 机制 → 不置 login 位（§2.8 口径，误置会让
        // 设置页出现对不上的登录入口）。
        let capabilities = adapter.capabilities();
        assert!(capabilities.playlists);
        assert!(capabilities.album);
        assert!(capabilities.leaderboard);
        assert!(capabilities.link_parse);
        assert!(
            !capabilities.login && !capabilities.qr_login,
            "Anonymous web token 不置 login 位（§2.8 机制细分；musid-dli 上游测试 pin）"
        );
        assert!(
            !capabilities.artist,
            "musid 无 ArtistProvider 实现——voicefox artist 位恒旧路径（登记缺口）"
        );
        assert!(adapter.supported_qualities().is_empty());
    }

    #[test]
    fn kuwo_slice_is_registered_and_covers_the_migrated_ledger_row() {
        let manager = crate::manager::SourceManager::new(SourceId::Kw, &[SourceId::Kw]);
        let transport = TransportInjection::new(None, 15);
        let registered = register_available(&manager, &transport);
        assert!(
            registered.contains(&SourceId::Kw),
            "kuwo 子 feature 在：register_available 必须装配 kuwo 影子源"
        );
        assert!(
            !crate::adapter_musid::unregistered_migrated_sources(&registered)
                .contains(&SourceId::Kw),
            "kuwo 已装配，不得出现在账面/构建脱节 warn 输入里"
        );
    }
}

// V2 切片 7（kugou，docs/MUSID_DLI_ADAPTER.md §12.4）：与 kuwo 同形的真实
// provider 编译期覆盖。真实 provider 的行为对账见
// `source/tests/musid_kugou_slice.rs`。
#[cfg(all(test, feature = "musid-dli-adapter-kugou"))]
mod kugou_slice_tests {
    use super::*;

    #[test]
    fn kugou_assemble_branch_builds_the_real_provider() {
        let adapter = kugou(&TransportInjection::new(None, 15))
            .expect("kugou 子 feature 在：装配分支必须产出适配源");
        assert_eq!(adapter.id(), SourceId::Kg);
        assert_eq!(adapter.name(), "酷狗音乐（musid-dli）");
        // 真实 descriptor 位 pin：kugou capabilities =
        // track_search/stream_resolve/lyrics/authentication/playlists/
        // charts/link_resolve/albums/artists 全 true（musid-dli
        // source_kugou.rs:648-663；artists=true 为 search-based fallback）。
        // 无 auth_capability 覆写 → authentication bool 折叠 Cookie。
        // 经 §2.8 粒度表映射：playlists/album/artist/leaderboard/
        // link_parse/login 全 true——login 是诊断对照位，登录/扫码链路
        // 恒旧路径（影子源恒匿名 new()，§11.8 零凭据流动）。
        let capabilities = adapter.capabilities();
        assert!(capabilities.playlists);
        assert!(capabilities.album);
        assert!(capabilities.artist);
        assert!(capabilities.leaderboard);
        assert!(capabilities.link_parse);
        assert!(
            capabilities.login,
            "authentication=true 折叠 Cookie → 诊断位 login=true（不改变登录链路走向）"
        );
        assert!(
            !capabilities.qr_login,
            "musid kugou 无 Qr 注册 → qr_login=false；扫码恒走 voicefox 旧路径"
        );
        assert!(adapter.supported_qualities().is_empty());
    }

    #[test]
    fn kugou_slice_is_registered_and_covers_the_migrated_ledger_row() {
        let manager = crate::manager::SourceManager::new(SourceId::Kw, &[SourceId::Kg]);
        let transport = TransportInjection::new(None, 15);
        let registered = register_available(&manager, &transport);
        assert!(
            registered.contains(&SourceId::Kg),
            "kugou 子 feature 在：register_available 必须装配 kugou 影子源"
        );
        assert!(
            !crate::adapter_musid::unregistered_migrated_sources(&registered)
                .contains(&SourceId::Kg),
            "kugou 已装配，不得出现在账面/构建脱节 warn 输入里"
        );
    }
}

// V2 切片 8（qqmusic/tx，docs/MUSID_DLI_ADAPTER.md §12.4）：与 kuwo 同形的
// 真实 provider 编译期覆盖。真实 provider 的行为对账见
// `source/tests/musid_qqmusic_slice.rs`。
#[cfg(all(test, feature = "musid-dli-adapter-qqmusic"))]
mod qqmusic_slice_tests {
    use super::*;

    #[test]
    fn qqmusic_assemble_branch_builds_the_real_provider() {
        let adapter = qqmusic(&TransportInjection::new(None, 15))
            .expect("qqmusic 子 feature 在：装配分支必须产出适配源");
        assert_eq!(adapter.id(), SourceId::Tx);
        assert_eq!(adapter.name(), "QQ音乐（musid-dli）");
        // 真实 descriptor 位 pin：qqmusic capabilities =
        // track_search/stream_resolve/lyrics/authentication/playlists/
        // albums/charts/link_resolve true（musid-dli source_qqmusic.rs
        // :760-772），无 auth_capability 覆写 → authentication bool 折叠
        // Cookie。经 §2.8 粒度表映射：playlists/album/leaderboard/
        // link_parse/login 全 true——login 是诊断对照位，登录/扫码链路
        // （QQ/微信双扫码，voicefox wechat_login 双栈期恒 false，§2.8）
        // 恒旧路径（影子源恒匿名 new()，§11.8 零凭据流动）。
        let capabilities = adapter.capabilities();
        assert!(capabilities.playlists);
        assert!(capabilities.album);
        assert!(capabilities.leaderboard);
        assert!(capabilities.link_parse);
        assert!(
            capabilities.login,
            "authentication=true 折叠 Cookie → 诊断位 login=true（不改变登录链路走向）"
        );
        assert!(
            !capabilities.qr_login && !capabilities.wechat_login,
            "musid 无 Qr 注册且无微信数据源 → 扫码（含微信）恒走 voicefox 旧路径（§2.8）"
        );
        assert!(
            !capabilities.artist,
            "musid 无 ArtistProvider 实现——voicefox tx artist 位恒旧路径（登记缺口）"
        );
        // supported_qualities 未覆写（缺省空）→ 适配层翻译产物为空。
        assert!(adapter.supported_qualities().is_empty());
    }

    #[test]
    fn qqmusic_slice_is_registered_and_covers_the_migrated_ledger_row() {
        let manager = crate::manager::SourceManager::new(SourceId::Kw, &[SourceId::Tx]);
        let transport = TransportInjection::new(None, 15);
        let registered = register_available(&manager, &transport);
        assert!(
            registered.contains(&SourceId::Tx),
            "qqmusic 子 feature 在：register_available 必须装配 tx 影子源"
        );
        // 单子 feature 组合构建下其余 Migrated 源未装配属预期 warn 形态
        // （§12.1 生效前提），按"不含本切片源"断言；全等空断言由
        // migrated_slices_assemble_without_ledger_gap 的十一源组合分支钉住。
        assert!(
            !crate::adapter_musid::unregistered_migrated_sources(&registered)
                .contains(&SourceId::Tx),
            "tx 已装配，不得出现在账面/构建脱节 warn 输入里"
        );
    }
}

// V2 切片 9（netease/wy，docs/MUSID_DLI_ADAPTER.md §12.4）：与 qqmusic 同形
// 的真实 provider 编译期覆盖。真实 provider 的行为对账见
// `source/tests/musid_netease_slice.rs`。
#[cfg(all(test, feature = "musid-dli-adapter-netease"))]
mod netease_slice_tests {
    use super::*;

    #[test]
    fn netease_assemble_branch_builds_the_real_provider() {
        let adapter = netease(&TransportInjection::new(None, 15))
            .expect("netease 子 feature 在：装配分支必须产出适配源");
        assert_eq!(adapter.id(), SourceId::Wy);
        assert_eq!(adapter.name(), "网易云音乐（musid-dli）");
        // 真实 descriptor 位 pin：netease capabilities =
        // track_search/stream_resolve/lyrics/authentication/playlists/
        // albums/artists/charts/link_resolve 全 true（musid-dli
        // source_netease.rs:443-456 全家桶），无 auth_capability 覆写 →
        // 折叠 Cookie。经 §2.8 粒度表映射：playlists/album/artist/
        // leaderboard/link_parse/login 全 true——artist 位只是 descriptor
        // 诊断对照（musid 有 ArtistProvider），集合面不路由（V2 四面范围
        // 不接，wy 歌单同步的 by-id 详情前置=上游公开 detail trait，§11.4
        // 第 4 条）；登录/扫码/用户歌单恒旧路径（影子源恒匿名，wy 登录态
        // 依赖最重——musid 恒匿名 vs cookie 登录的系统性收窄是 P5 口径，
        // probe 对账计数解读先剥离该不对称，§11.8 (b)）。
        let capabilities = adapter.capabilities();
        assert!(capabilities.playlists);
        assert!(capabilities.album);
        assert!(capabilities.artist);
        assert!(capabilities.leaderboard);
        assert!(capabilities.link_parse);
        assert!(
            capabilities.login,
            "authentication=true 折叠 Cookie → 诊断位 login=true（不改变登录链路走向）"
        );
        assert!(
            !capabilities.qr_login,
            "musid 无 Qr 注册 → qr_login=false；扫码恒走 voicefox 旧路径"
        );
        assert!(adapter.supported_qualities().is_empty());
    }

    #[test]
    fn netease_slice_is_registered_and_covers_the_migrated_ledger_row() {
        let manager = crate::manager::SourceManager::new(SourceId::Kw, &[SourceId::Wy]);
        let transport = TransportInjection::new(None, 15);
        let registered = register_available(&manager, &transport);
        assert!(
            registered.contains(&SourceId::Wy),
            "netease 子 feature 在：register_available 必须装配 wy 影子源"
        );
        assert!(
            !crate::adapter_musid::unregistered_migrated_sources(&registered)
                .contains(&SourceId::Wy),
            "wy 已装配，不得出现在账面/构建脱节 warn 输入里"
        );
    }
}

// V2 切片 10（qianqian，docs/MUSID_DLI_ADAPTER.md §12.4）：与 migu 同形的
// 真实 provider 编译期覆盖。真实 provider 的行为对账见
// `source/tests/musid_qianqian_slice.rs`。
#[cfg(all(test, feature = "musid-dli-adapter-qianqian"))]
mod qianqian_slice_tests {
    use super::*;
    use lx_core::model::source::Quality;

    #[test]
    fn qianqian_assemble_branch_builds_the_real_provider() {
        let adapter = qianqian(&TransportInjection::new(None, 15))
            .expect("qianqian 子 feature 在：装配分支必须产出适配源");
        assert_eq!(adapter.id(), SourceId::Qianqian);
        assert_eq!(adapter.name(), "千千音乐（musid-dli）");
        // 真实 descriptor 位 pin：qianqian capabilities =
        // track_search/stream_resolve/lyrics/playlists/albums/link_resolve
        // true（musid-dli source_qianqian.rs:732-742），authentication=false
        // + 无 auth_capability 覆写 → AuthCapability::None——登录面双侧皆无
        // 认证位，不设登录行项。经 §2.8 粒度表映射：playlists/album/
        // link_parse true；歌手面 musid 无 ArtistProvider（artists 位缺省
        // false，voicefox artist 位恒旧路径登记缺口）；榜单面双侧皆无
        // （musid 无 ChartProvider）。
        let capabilities = adapter.capabilities();
        assert!(capabilities.playlists);
        assert!(capabilities.album);
        assert!(capabilities.link_parse);
        assert!(
            !capabilities.login && !capabilities.qr_login,
            "authentication=false + 无覆写 → 无登录位（登录面双侧皆无）"
        );
        assert!(
            !capabilities.artist,
            "musid 无 ArtistProvider 实现——voicefox artist 位恒旧路径（登记缺口）"
        );
        assert!(
            !capabilities.leaderboard,
            "榜单面双侧皆无（musid 无 ChartProvider，voicefox 无 leaderboard 位）"
        );
        assert!(
            !capabilities.playlist_search && !capabilities.playlist_categories,
            "面级 flags 装配点未探测（§11.3），漂移即红"
        );
        // supported_qualities：descriptor 声明上游档序 Low128/High320/Flac
        // （source_qianqian.rs:743-750）→ 适配层翻译产物
        // [Low128, High320, Flac]——与 legacy qianqian/mod.rs:81-83 同表。
        assert_eq!(
            adapter.supported_qualities(),
            vec![Quality::Low128, Quality::High320, Quality::Flac]
        );
    }

    #[test]
    fn qianqian_slice_is_registered_and_covers_the_migrated_ledger_row() {
        let manager = crate::manager::SourceManager::new(SourceId::Kw, &[SourceId::Qianqian]);
        let transport = TransportInjection::new(None, 15);
        let registered = register_available(&manager, &transport);
        assert!(
            registered.contains(&SourceId::Qianqian),
            "qianqian 子 feature 在：register_available 必须装配 qianqian 影子源"
        );
        assert!(
            !crate::adapter_musid::unregistered_migrated_sources(&registered)
                .contains(&SourceId::Qianqian),
            "qianqian 已装配，不得出现在账面/构建脱节 warn 输入里"
        );
    }
}

// V2 切片 11（joox，docs/MUSID_DLI_ADAPTER.md §12.4）：与 qianqian 同形的
// 真实 provider 编译期覆盖。真实 provider 的行为对账见
// `source/tests/musid_joox_slice.rs`。
#[cfg(all(test, feature = "musid-dli-adapter-joox"))]
mod joox_slice_tests {
    use super::*;
    use lx_core::model::source::Quality;

    #[test]
    fn joox_assemble_branch_builds_the_real_provider() {
        let adapter = joox(&TransportInjection::new(None, 15))
            .expect("joox 子 feature 在：装配分支必须产出适配源");
        assert_eq!(adapter.id(), SourceId::Joox);
        assert_eq!(adapter.name(), "JOOX（musid-dli）");
        // 真实 descriptor 位 pin：joox capabilities =
        // track_search/stream_resolve/lyrics/playlists/albums/link_resolve
        // true（musid-dli source_joox.rs:706-716），authentication=false +
        // 无 auth_capability 覆写 → None——登录面双侧皆无认证位。经 §2.8
        // 粒度表映射：playlists/album/link_parse true；榜单/歌手双侧皆无
        // （musid 无 ChartProvider/ArtistProvider）；封面面不接 musid
        // ArtworkProvider（legacy 走歌曲模型回填，适配层同口径）。
        let capabilities = adapter.capabilities();
        assert!(capabilities.playlists);
        assert!(capabilities.album);
        assert!(capabilities.link_parse);
        assert!(
            !capabilities.login && !capabilities.qr_login,
            "authentication=false + 无覆写 → 无登录位（登录面双侧皆无）"
        );
        assert!(
            !capabilities.artist && !capabilities.leaderboard,
            "榜单/歌手双侧皆无（§12.4 行 11）"
        );
        // supported_qualities：descriptor 声明上游档序 Low128/High320
        // （source_joox.rs:717-720）→ 适配层翻译产物 [Low128, High320]——
        // 与 legacy joox/mod.rs:171-173 同表。
        assert_eq!(
            adapter.supported_qualities(),
            vec![Quality::Low128, Quality::High320]
        );
    }

    #[test]
    fn joox_slice_is_primary_and_never_shadow_registered() {
        // V4-s1（§14.1）：legacy joox 已删，适配面是唯一实现——manager 主
        // 注册表无条件装配（`SourceManager::new` 的 joox 分支），影子注册
        // 不再有 joox 行：register_available 不装配它，账面/构建脱节 warn
        // 也显式跳过它。
        let manager = crate::manager::SourceManager::new(SourceId::Kw, &[SourceId::Joox]);
        let transport = TransportInjection::new(None, 15);
        let registered = register_available(&manager, &transport);
        assert!(
            !registered.contains(&SourceId::Joox),
            "joox 是主注册表源（manager::new 直装），不走影子注册"
        );
        assert!(
            !crate::adapter_musid::unregistered_migrated_sources(&registered)
                .contains(&SourceId::Joox),
            "joox 主注册表装配，不得出现在账面/构建脱节 warn 输入里"
        );
    }
}
