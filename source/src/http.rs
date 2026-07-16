//! HTTP 客户端封装 — TODO: Phase 2
//!
//! 职责：统一 UA、超时、重试逻辑、代理支持
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("Mozilla/5.0 (compatible; lx-tui/0.1)")
        .build()
        .expect("failed to build HTTP client")
}
