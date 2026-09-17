//! 网易云音乐播放 URL 获取
//!
//! 主路径：官方 eapi 接口 `/api/song/enhance/player/url/v1`
//! （加密方式与搜索、歌词一致，见 `crypto::eapi`），一次请求即可拿到
//! 播放地址 + `size` + `md5`，下载侧因此能做完整性校验。
//!
//! 兜底：接口无版权/风控拿不到地址时，退回公开重定向 URL
//! `https://music.163.com/song/media/outer/url?id={id}.mp3`，代价是拿不到 size。

use serde_json::Value;

use lx_core::model::song::SongInfo;
use lx_core::model::source::Quality;
use lx_core::traits::source::{FetchError, SongUrl};

use crate::http::SendWithRetry;

use super::super::http;
use super::crypto;

/// eapi 签名用的路径，请求也发到同一路径。
const PLAYER_URL_API: &str = "/api/song/enhance/player/url/v1";
const PLAYER_URL_ENDPOINT: &str = "https://music.163.com/api/song/enhance/player/url/v1";

/// 请求音质对应的降级阶梯：高音质拿不到可播地址时逐级下调，
/// 避免「要 FLAC 但账号只有 320k」直接判定整首不可播。
fn level_ladder(quality: Quality) -> &'static [(&'static str, Quality)] {
    match quality {
        Quality::Flac24 => &[
            ("hires", Quality::Flac24),
            ("lossless", Quality::Flac),
            ("exhigh", Quality::High320),
            ("standard", Quality::Low128),
        ],
        Quality::Flac => &[
            ("lossless", Quality::Flac),
            ("exhigh", Quality::High320),
            ("standard", Quality::Low128),
        ],
        Quality::High320 => &[("exhigh", Quality::High320), ("standard", Quality::Low128)],
        Quality::Low128 => &[("standard", Quality::Low128)],
    }
}

/// 无损档位用 flac 容器，其余用 mp3。
fn encode_type(quality: Quality) -> &'static str {
    match quality {
        Quality::Flac | Quality::Flac24 => "flac",
        Quality::Low128 | Quality::High320 => "mp3",
    }
}

/// 官方接口返回的可播地址及其校验信息。
struct OfficialUrl {
    url: String,
    quality: Quality,
    size: Option<u64>,
    md5: Option<String>,
}

fn json_u64(value: &Value) -> Option<u64> {
    value.as_u64().filter(|&size| size > 0)
}

fn json_non_empty_str(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// 调官方接口拿地址。返回 `Ok(None)` 表示该档位不可播（应继续降级或走兜底）。
async fn fetch_official_url(
    song: &SongInfo,
    quality: Quality,
) -> Result<Option<OfficialUrl>, FetchError> {
    let client = http::client();
    let mut last_error: Option<FetchError> = None;

    for (level, achieved) in level_ladder(quality) {
        let data = serde_json::json!({
            "ids": format!("[{}]", song.id),
            "level": level,
            "encodeType": encode_type(*achieved),
        });
        let encrypted = crypto::eapi(PLAYER_URL_API, &data);

        let resp = match super::with_cookie(client.post(PLAYER_URL_ENDPOINT))
            .header(
                "User-Agent",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
            )
            .header("origin", "https://music.163.com")
            .header("Referer", "https://music.163.com/")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(format!("params={encrypted}"))
            .send_with_retry(crate::http::RETRY_ATTEMPTS)
            .await
        {
            Ok(resp) => resp,
            Err(error) => {
                last_error = Some(FetchError::Network(error.to_string()));
                continue;
            }
        };

        if !resp.status().is_success() {
            last_error = Some(FetchError::Network(format!("HTTP {}", resp.status())));
            continue;
        }

        // 接口偶发返回非 JSON（例如风控会返回拼接的 {"msg":"参数错误","code":400}），
        // 这种情况不当作整首失败，继续降级/走兜底。
        let text = match resp.text().await {
            Ok(text) => text,
            Err(error) => {
                last_error = Some(FetchError::Network(error.to_string()));
                continue;
            }
        };
        let json: Value = match serde_json::from_str(&text) {
            Ok(json) => json,
            Err(error) => {
                tracing::debug!("网易云 URL 接口返回非 JSON 响应（{level} 档位）: {error}");
                last_error = Some(FetchError::Parse(error.to_string()));
                continue;
            }
        };

        let Some(item) = json["data"].as_array().and_then(|items| items.first()) else {
            continue;
        };

        // 试听片段（freeTrialInfo 非空）不是完整音频，直接判为该档位不可用，
        // 否则会把 30 秒片段当成整首歌落盘。
        if !item["freeTrialInfo"].is_null() {
            tracing::debug!("网易云 {level} 档位只返回试听片段，跳过: {}", song.name);
            continue;
        }

        let Some(url) = json_non_empty_str(&item["url"]) else {
            continue;
        };

        return Ok(Some(OfficialUrl {
            url,
            quality: *achieved,
            size: json_u64(&item["size"]),
            md5: json_non_empty_str(&item["md5"]).map(|md5| md5.to_ascii_lowercase()),
        }));
    }

    if let Some(error) = last_error {
        tracing::warn!("网易云官方 URL 接口不可用，回退公开重定向地址: {error}");
    }
    Ok(None)
}

pub async fn get_song_url(song: &SongInfo, quality: Quality) -> Result<SongUrl, FetchError> {
    let qualities: Vec<Quality> = song.qualities.iter().copied().collect();

    if let Some(official) = fetch_official_url(song, quality).await? {
        return Ok(SongUrl {
            url: official.url,
            quality: official.quality,
            duration: song.duration,
            cover_url: song.cover_url.clone(),
            qualities,
            headers: vec![],
            size: official.size,
            size_is_advisory: false,
            md5: official.md5,
            // 备用 CDN 由下载引擎按 m8/m801/m804/m704 → m7/m701 改写补全。
            candidate_urls: vec![],
            max_chunk_size: 0,
        });
    }

    // 兜底：公开重定向 URL（reqwest 自动跟随 302），拿不到 size/md5。
    let url = format!(
        "https://music.163.com/song/media/outer/url?id={}.mp3",
        song.id
    );
    Ok(SongUrl {
        url,
        quality,
        duration: song.duration,
        cover_url: song.cover_url.clone(),
        qualities,
        headers: vec![],
        size: None,
        size_is_advisory: false,
        md5: None,
        candidate_urls: vec![],
        max_chunk_size: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_ladder_downgrades_towards_standard() {
        let ladder = level_ladder(Quality::Flac24);
        assert_eq!(ladder.first().unwrap().0, "hires");
        assert_eq!(ladder.last().unwrap().0, "standard");
        // 逐级下降，不会跳到更高档位。
        let levels: Vec<&str> = ladder.iter().map(|(level, _)| *level).collect();
        assert_eq!(levels, vec!["hires", "lossless", "exhigh", "standard"]);

        assert_eq!(level_ladder(Quality::Low128).len(), 1);
    }

    #[test]
    fn encode_type_follows_the_achieved_quality() {
        assert_eq!(encode_type(Quality::Flac), "flac");
        assert_eq!(encode_type(Quality::Flac24), "flac");
        assert_eq!(encode_type(Quality::High320), "mp3");
    }

    #[test]
    fn size_and_md5_parsing_ignores_empty_values() {
        assert_eq!(json_u64(&serde_json::json!(4276601)), Some(4276601));
        assert_eq!(json_u64(&serde_json::json!(0)), None);
        assert_eq!(json_u64(&serde_json::json!(null)), None);

        assert_eq!(
            json_non_empty_str(&serde_json::json!("A0634034446F904929E37DC2686BA91B")),
            Some("A0634034446F904929E37DC2686BA91B".to_string())
        );
        assert_eq!(json_non_empty_str(&serde_json::json!("  ")), None);
        assert_eq!(json_non_empty_str(&serde_json::json!(null)), None);
    }
}
