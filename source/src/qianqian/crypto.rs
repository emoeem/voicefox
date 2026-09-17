//! 千千音乐（91q）接口签名。
//!
//! 规则与 music-lib 一致：把参数按 key 升序拼成 `k=v&k=v`，末尾接 Secret，
//! 取 MD5 作为 `sign`。签名用**未编码**的原始值，最后统一做 URL 编码。

use std::time::{SystemTime, UNIX_EPOCH};

use md5::Digest;

/// 千千音乐 web 端 appid。
pub const APP_ID: &str = "16073360";
const SECRET: &str = "0b50b02fd0d73a9c4c8c3a781c30845f";

pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";
pub const REFERER: &str = "https://music.91q.com/player";

/// 生成带签名的查询串（含 `timestamp` 与 `sign`）。
pub fn signed_query(params: &[(&str, &str)]) -> String {
    signed_query_with_extra(params, &[])
}

/// tracklink 的 `rate` 是业务参数，但历史 Web 接口的签名明文不包含它。
pub fn signed_tracklink_query(tsid: &str, rate: &str) -> String {
    signed_query_with_extra(&[("TSID", tsid), ("appid", APP_ID)], &[("rate", rate)])
}

fn signed_query_with_extra(params: &[(&str, &str)], extra: &[(&str, &str)]) -> String {
    let mut pairs: Vec<(String, String)> = params
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
    pairs.push(("timestamp".to_string(), unix_seconds().to_string()));
    pairs.sort_by(|left, right| left.0.cmp(&right.0));

    let joined = pairs
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&");
    let sign = hex::encode(md5::Md5::digest(format!("{joined}{SECRET}").as_bytes()));
    pairs.extend(
        extra
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string())),
    );
    pairs.push(("sign".to_string(), sign));

    pairs
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                urlencoding::encode(key),
                urlencoding::encode(value)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_is_stable_for_the_same_inputs() {
        let query = signed_query(&[("word", "周杰伦"), ("type", "1"), ("appid", APP_ID)]);
        assert!(query.contains("appid=16073360"));
        assert!(query.contains("sign="));
        assert!(query.contains("timestamp="));
        // 参数按 key 升序排列，appid 在最前。
        assert!(query.starts_with("appid="));
    }
}
