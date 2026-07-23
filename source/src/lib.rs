pub mod js;
pub mod local;
pub mod manager;
pub mod bili;

// 音源模块
pub mod kg;
pub mod kw;
pub mod mg;
pub mod tx;
pub mod wy;

// 内部工具（不对外暴露细节）
mod crypto;
mod filter;
mod http;

pub fn configure_network(proxy_url: &str, timeout_secs: u64) {
    http::configure(proxy_url, timeout_secs);
}
