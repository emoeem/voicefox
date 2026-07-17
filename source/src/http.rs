//! HTTP 客户端封装 — TODO: Phase 2
//!
//! 职责：统一 UA、超时、重试逻辑、代理支持
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

#[derive(Debug, Clone)]
struct NetworkOptions {
    proxy_url: String,
    timeout: Duration,
}

impl Default for NetworkOptions {
    fn default() -> Self {
        Self {
            proxy_url: String::new(),
            timeout: Duration::from_secs(15),
        }
    }
}

fn options() -> &'static RwLock<NetworkOptions> {
    static OPTIONS: OnceLock<RwLock<NetworkOptions>> = OnceLock::new();
    OPTIONS.get_or_init(|| RwLock::new(NetworkOptions::default()))
}

pub(crate) fn configure(proxy_url: &str, timeout_secs: u64) {
    let mut options = options().write().unwrap();
    options.proxy_url = proxy_url.trim().to_string();
    options.timeout = Duration::from_secs(timeout_secs.clamp(1, 300));
}

pub fn client() -> reqwest::Client {
    let options = options().read().unwrap().clone();
    let mut builder = reqwest::Client::builder()
        .timeout(options.timeout)
        .user_agent("Mozilla/5.0 (compatible; voicefox/0.1)");
    if !options.proxy_url.is_empty() {
        match reqwest::Proxy::all(&options.proxy_url) {
            Ok(proxy) => builder = builder.proxy(proxy),
            Err(error) => tracing::warn!("invalid proxy URL: {error}"),
        }
    }
    builder.build().expect("failed to build HTTP client")
}
