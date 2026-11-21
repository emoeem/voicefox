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
//! bilibili 不装配：`bili` 恒走旧路径（独立存储 + 扫码登录链路，§11）。

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
    #[cfg(not(any(
        feature = "musid-dli-adapter-netease",
        feature = "musid-dli-adapter-qqmusic",
        feature = "musid-dli-adapter-kugou",
        feature = "musid-dli-adapter-kuwo",
        feature = "musid-dli-adapter-migu",
        feature = "musid-dli-adapter-jamendo",
        feature = "musid-dli-adapter-apple",
        feature = "musid-dli-adapter-fivesing",
        feature = "musid-dli-adapter-qianqian",
        feature = "musid-dli-adapter-joox",
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
    #[cfg(feature = "musid-dli-adapter-qianqian")]
    register_built(manager, &mut registered, qianqian(transport));
    #[cfg(feature = "musid-dli-adapter-joox")]
    register_built(manager, &mut registered, joox(transport));

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

#[cfg(feature = "musid-dli-adapter-qianqian")]
fn qianqian(transport: &TransportInjection) -> Option<Arc<dyn MusicSource>> {
    assemble!(
        transport,
        musid_dli::source_qianqian::QianqianProvider,
        SourceId::Qianqian
    )
}

#[cfg(feature = "musid-dli-adapter-joox")]
fn joox(transport: &TransportInjection) -> Option<Arc<dyn MusicSource>> {
    assemble!(
        transport,
        musid_dli::source_joox::JooxProvider,
        SourceId::Joox
    )
}
