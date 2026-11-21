pub mod apple;
pub mod bili;
pub mod cover_cache;
pub mod js;
pub mod local;
pub mod manager;
pub mod session;
pub mod sync;

pub mod fivesing;
pub mod jamendo;
pub mod kg;
pub mod kw;
pub mod mg;
pub mod soda;
pub mod tx;
pub mod wy;

mod crypto;
mod filter;
pub mod http;

pub fn configure_network(proxy_url: &str, timeout_secs: u64) {
    http::configure(proxy_url, timeout_secs);
}

// musid-dli（crates.io 0.2.0）模型映射层：V0 只提供纯映射函数与 fixture
// 测试，不接 SourceManager 或任何业务路径（docs/MUSID_DLI_ADAPTER.md）。
// V1（opt-in）追加：AdapterMusicSource 双栈适配源、parity 对账、按源子
// feature 装配点（§11）。
#[cfg(feature = "musid-dli-adapter")]
pub mod adapter_musid;

/// V1 双栈 parity 对账计数与归一对照（内存计数、不落盘）。
#[cfg(feature = "musid-dli-adapter")]
pub mod parity;

/// V1 双栈按源子 feature 的装配点（影子源注册）。
#[cfg(feature = "musid-dli-adapter")]
pub mod musid_bootstrap;

/// 本次构建是否编译了 musid-dli 适配层。`cfg!` 只对本 crate 的 feature
/// 生效，宿主（app）拿它做「config 开而 feature 关」的启动 warn。
pub const MUSID_ADAPTER_ENABLED: bool = cfg!(feature = "musid-dli-adapter");

/// 歌词转换变体（`musid-dli-adapter-lyrics`）是否编译；缺它时歌词面
/// 双栈不生效。
pub const MUSID_LYRIC_CONVERSIONS_ENABLED: bool = cfg!(feature = "musid-dli-adapter-lyrics");

/// V1 双栈接线点（opt-in；默认构建（V4 起）带 joox/qianqian 适配边）。
///
/// - 总闸（`source.musid_stack`）关闭：四面全走旧路径，无任何注册动作；
/// - 总闸开启而构建未编译 `musid-dli-adapter`：启动时一次 warn，四面静默
///   走旧路径（config 形状不随 feature 变化）；
/// - 总闸开启且 feature 在：按本次构建启用的 `musid-dli-adapter-*` 子
///   feature 装配影子源（独立 map，不覆盖内置），并下发路由开关；
///   一个子 feature 都没开时 warn（四面仍走旧路径）。
///
/// voicefox 的代理/超时经 [`adapter_musid::TransportInjection`] 记录预算
/// （0.2.0 的 transport 注入缺口见 docs/MUSID_DLI_ADAPTER.md §4/§11），
/// proxy/超时继续只驱动内置音源（`lx_source::http::configure`）。
pub fn bootstrap_musid_stack(
    manager: &manager::SourceManager,
    source: &lx_core::model::config::SourceConfig,
    network: &lx_core::model::config::NetworkConfig,
) {
    #[cfg(feature = "musid-dli-adapter")]
    {
        if !source.musid_stack {
            return;
        }
        let transport = adapter_musid::TransportInjection::new(
            (!network.proxy_url.trim().is_empty()).then(|| network.proxy_url.clone()),
            network.timeout,
        );
        let registered = musid_bootstrap::register_available(manager, &transport);
        if registered.is_empty() {
            tracing::warn!(
                "source.musid_stack 已开启，但本次构建未启用任何 musid-dli-adapter-* 按源子 feature：搜索/流地址/歌词四面均走内置旧路径"
            );
        } else {
            tracing::info!(
                sources = ?registered,
                "musid-dli 双栈影子源已注册（opt-in）"
            );
        }
        // V2 逐源路由注册表（docs §12.1）的账面/构建脱节暴露（评审 P4）：
        // 账面标 Migrated 的源，其对应子 feature 未在本次构建启用时影子源
        // 不装配，运行时静默走旧路径且 parity 计数恒零——启动时逐源 warn。
        for gap in adapter_musid::unregistered_migrated_sources(&registered) {
            tracing::warn!(
                source = gap.as_str(),
                "musid 注册表账面已标记 Migrated 的源，本次构建未启用对应 musid-dli-adapter-* 子 feature：该源路由静默走旧路径，parity 计数恒零（§12.1 生效前提）"
            );
        }
        manager.set_musid_routes(manager::MusidRouteConfig {
            stack: true,
            search: source.musid_search,
            song_url: source.musid_song_url,
            lyric: source.musid_lyric,
            parity_probe: source.musid_parity_probe,
        });
        if source.musid_lyric && !MUSID_LYRIC_CONVERSIONS_ENABLED {
            tracing::warn!(
                "source.musid_lyric 已开启，但本次构建未启用 musid-dli-adapter-lyrics（歌词转换变体）：歌词面继续走旧路径"
            );
        }
        if source.musid_parity_probe {
            tracing::info!("musid parity 对账影子模式已开启：恒展示旧路径结果，仅内存计数对照");
        }
    }
    #[cfg(not(feature = "musid-dli-adapter"))]
    {
        let _ = manager;
        let _ = network;
        if source.musid_stack {
            tracing::warn!(
                "source.musid_stack 已开启，但本次构建未编译 musid-dli-adapter feature：搜索/流地址/歌词四面均走内置旧路径"
            );
        }
    }
}
