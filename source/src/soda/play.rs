//! 汽水播放地址解析：从 SEO 返回的 `video_model` 里挑一路可用的流。
//!
//! `video_model` 是嵌套（且经常被二次字符串编码）的 JSON，里面散落着多档
//! 码率的播放地址。这里的做法与 music-lib 一致：递归遍历整棵树，凡是带
//! `main_play_url` / `play_auth` 这类字段的对象都收成一个候选，再按码率挑
//! 最接近请求档位的一路。

use lx_core::model::source::Quality;
use serde_json::Value;

use super::api::{ApiError, value_string};

/// 一路可用的音频流。
#[derive(Debug, Clone, Default)]
pub struct SodaStream {
    pub url: String,
    /// 非空表示音频经过 CENC 加密，播放前必须解密。
    pub play_auth: Option<String>,
    pub bitrate: u64,
    pub size: Option<u64>,
    pub format: String,
}

impl SodaStream {
    pub fn is_encrypted(&self) -> bool {
        self.play_auth
            .as_deref()
            .is_some_and(|auth| !auth.trim().is_empty())
    }

    /// 码率对应的音质档位（用于回写实际拿到的规格）。
    pub fn quality(&self) -> Quality {
        if self.format.eq_ignore_ascii_case("flac") || self.bitrate >= 1000 {
            return Quality::Flac;
        }
        if self.bitrate >= 256 {
            return Quality::High320;
        }
        Quality::Low128
    }
}

fn json_string(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(found) = value.get(*key) {
            let text = value_string(found);
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

fn json_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    for key in keys {
        if let Some(found) = value.get(*key) {
            if let Some(number) = found.as_u64() {
                return Some(number);
            }
            if let Some(text) = found.as_str().and_then(|text| text.parse().ok()) {
                return Some(text);
            }
        }
    }
    None
}

/// 收集 `value` 树里所有带播放地址的节点。
fn collect(value: &Value, inherited_auth: Option<&str>, out: &mut Vec<SodaStream>) {
    match value {
        Value::Object(map) => {
            let auth = json_string(value, &["play_auth", "PlayAuth"])
                .or_else(|| inherited_auth.map(str::to_string));
            let url = json_string(
                value,
                &[
                    "main_play_url",
                    "MainPlayUrl",
                    "main_url",
                    "MainUrl",
                    "play_url",
                    "PlayURL",
                    "backup_play_url",
                    "BackupPlayUrl",
                ],
            );
            if let Some(url) = url.filter(|url| url.starts_with("http")) {
                out.push(SodaStream {
                    url,
                    play_auth: auth.clone(),
                    bitrate: json_u64(value, &["bitrate", "Bitrate", "br", "BR", "bit_rate"])
                        .unwrap_or_default(),
                    size: json_u64(value, &["size", "Size", "file_size", "FileSize"]),
                    format: json_string(value, &["format", "Format", "vtype", "VType"])
                        .unwrap_or_default(),
                });
            }
            for child in map.values() {
                collect(child, auth.as_deref(), out);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect(child, inherited_auth, out);
            }
        }
        Value::String(text) => {
            // 有些字段把整个 video_model 又编码成字符串，最多剥两层。
            if text.trim_start().starts_with('{') || text.trim_start().starts_with('[') {
                if let Ok(nested) = serde_json::from_str::<Value>(text) {
                    collect(&nested, inherited_auth, out);
                }
            }
        }
        _ => {}
    }
}

/// 从 SEO 响应里挑一路流；优先能覆盖请求档位的最接近档位。
pub(super) fn pick_stream(response: &Value, quality: Quality) -> Result<SodaStream, ApiError> {
    let mut candidates = Vec::new();
    collect(&response["track_player"], None, &mut candidates);
    if candidates.is_empty() {
        collect(response, None, &mut candidates);
    }
    if candidates.is_empty() {
        return Err(ApiError::Other("汽水没有返回可用的播放地址".to_string()));
    }
    // 明文流优先：能直接播，不需要解密与缓存。
    if let Some(plain) = candidates.iter().find(|stream| !stream.is_encrypted()) {
        return Ok(plain.clone());
    }
    // 都是加密流时，按码率挑最接近请求档位的一路。
    let target = match quality {
        Quality::Flac24 | Quality::Flac => u64::MAX,
        Quality::High320 => 320,
        Quality::Low128 => 128,
    };
    candidates.sort_by_key(|stream| {
        let bitrate = if stream.bitrate == 0 {
            128
        } else {
            stream.bitrate
        };
        bitrate.abs_diff(target.min(bitrate.max(target)))
    });
    Ok(candidates.remove(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_streams_from_nested_and_encoded_models() {
        let inner = serde_json::json!({
            "video_list": [
                { "main_play_url": "https://cdn/low.m4a", "bitrate": 128 },
                { "main_play_url": "https://cdn/high.m4a", "bitrate": 320, "play_auth": "auth" }
            ]
        });
        let response = serde_json::json!({
            "track_player": { "video_model": inner.to_string() }
        });
        let mut candidates = Vec::new();
        collect(&response["track_player"], None, &mut candidates);
        assert_eq!(candidates.len(), 2);
        // 字符串里编码的 JSON 也能被剥开。
        assert!(
            candidates
                .iter()
                .any(|stream| stream.url.ends_with("high.m4a"))
        );
    }

    #[test]
    fn prefers_plain_streams_over_encrypted_ones() {
        let response = serde_json::json!({
            "track_player": {
                "video_model": {
                    "list": [
                        { "main_play_url": "https://cdn/enc.m4a", "play_auth": "secret", "bitrate": 320 },
                        { "main_play_url": "https://cdn/plain.m4a", "bitrate": 128 }
                    ]
                }
            }
        });
        let picked = pick_stream(&response, Quality::Flac).unwrap();
        assert_eq!(picked.url, "https://cdn/plain.m4a");
        assert!(!picked.is_encrypted());
    }

    #[test]
    fn picks_the_closest_bitrate_when_everything_is_encrypted() {
        let response = serde_json::json!({
            "track_player": {
                "video_model": {
                    "list": [
                        { "main_play_url": "https://cdn/128.m4a", "play_auth": "a", "bitrate": 128 },
                        { "main_play_url": "https://cdn/320.m4a", "play_auth": "a", "bitrate": 320 },
                        { "main_play_url": "https://cdn/flac", "play_auth": "a", "bitrate": 1200, "format": "flac" }
                    ]
                }
            }
        });
        assert_eq!(
            pick_stream(&response, Quality::High320).unwrap().url,
            "https://cdn/320.m4a"
        );
        let lossless = pick_stream(&response, Quality::Flac).unwrap();
        assert_eq!(lossless.quality(), Quality::Flac);
    }

    #[test]
    fn reports_an_error_when_no_stream_exists() {
        let response = serde_json::json!({ "track_player": {} });
        assert!(pick_stream(&response, Quality::Low128).is_err());
    }

    #[test]
    fn quality_maps_from_bitrate_and_format() {
        let mut stream = SodaStream {
            url: "u".to_string(),
            bitrate: 320,
            ..SodaStream::default()
        };
        assert_eq!(stream.quality(), Quality::High320);
        stream.bitrate = 128;
        assert_eq!(stream.quality(), Quality::Low128);
        stream.bitrate = 900;
        stream.format = "flac".to_string();
        assert_eq!(stream.quality(), Quality::Flac);
    }
}
