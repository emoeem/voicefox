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
pub mod joox;
pub mod kg;
pub mod kw;
pub mod mg;
pub mod qianqian;
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
#[cfg(feature = "musid-dli-adapter")]
pub mod adapter_musid;
