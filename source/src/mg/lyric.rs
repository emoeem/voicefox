//! mg 歌词获取（MRC 逐字歌词 + LRC + TRC）

use crate::http::SendWithRetry;
use std::sync::OnceLock;

use lx_core::model::lyric::LyricData;
use lx_core::model::song::SongInfo;
use lx_core::traits::source::FetchError;

use super::super::http;
use super::crypto::decrypt_mrc;

const REFERER: &str = "https://app.c.nf.migu.cn/";
const USER_AGENT: &str = "Mozilla/5.0 (Linux; Android 5.1.1; Nexus 6 Build/LYZ28E) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/59.0.3071.115 Mobile Safari/537.36";

/// GET 获取原始内容。MRC 可能是 UTF-16、压缩或 Base64 容器，不能先按 UTF-8 解码。
async fn fetch_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, FetchError> {
    let resp = client
        .get(url)
        .header("Referer", REFERER)
        .header("User-Agent", USER_AGENT)
        .header("channel", "0146921")
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?
        .error_for_status()
        .map_err(|e| FetchError::Network(e.to_string()))?;

    resp.bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|e| FetchError::Network(e.to_string()))
}

async fn fetch_text(client: &reqwest::Client, url: &str) -> Result<String, FetchError> {
    client
        .get(url)
        .header("Referer", REFERER)
        .header("User-Agent", USER_AGENT)
        .header("channel", "0146921")
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?
        .error_for_status()
        .map_err(|e| FetchError::Network(e.to_string()))?
        .text()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))
}

pub async fn get_lyric(song: &SongInfo) -> Result<LyricData, FetchError> {
    let client = http::client();

    let (lyric, lxlyric, raw_lrc) = match song.extra.get("mrcUrl") {
        Some(mrc_url) if !mrc_url.is_empty() => match fetch_bytes(&client, mrc_url).await {
            Ok(content) => match decrypt_mrc(&content) {
                Ok(mrc) => match parse_mrc(&mrc) {
                    Some((lyric, lxlyric)) => (lyric, Some(lxlyric), Some(mrc)),
                    None => {
                        tracing::warn!("Migu MRC did not contain timed lyric lines");
                        fetch_lrc(&client, song).await?
                    }
                },
                Err(error) => {
                    tracing::warn!("decode Migu MRC failed: {error}");
                    fetch_lrc(&client, song).await?
                }
            },
            Err(error) => {
                tracing::warn!("fetch Migu MRC failed: {error}");
                // 网络类失败不能吞成空歌词（会被负缓存误标为无词）；
                // 没有 LRC 兜底链接时直接把错误传出去。
                if has_lrc_url(song) {
                    fetch_lrc(&client, song).await?
                } else {
                    return Err(error);
                }
            }
        },
        _ => fetch_lrc(&client, song).await?,
    };

    // TRC 翻译歌词
    let tlyric = match song.extra.get("trcUrl") {
        Some(trc_url) if !trc_url.is_empty() => fetch_text(&client, trc_url).await.ok(),
        _ => None,
    };

    Ok(LyricData {
        lyric,
        tlyric,
        rlyric: None,
        lxlyric,
        raw_lrc,
    })
}

async fn fetch_lrc(
    client: &reqwest::Client,
    song: &SongInfo,
) -> Result<(String, Option<String>, Option<String>), FetchError> {
    let lyric = match song.extra.get("lrcUrl") {
        // 网络失败向上传播，避免被 get_lyric_with_fallback 负缓存误标
        Some(lrc_url) if !lrc_url.is_empty() => fetch_text(client, lrc_url).await?,
        _ => String::new(),
    };
    Ok((lyric, None, None))
}

fn has_lrc_url(song: &SongInfo) -> bool {
    song.extra.get("lrcUrl").is_some_and(|url| !url.is_empty())
}

fn parse_mrc(content: &str) -> Option<(String, String)> {
    static LINE: OnceLock<regex::Regex> = OnceLock::new();
    static WORDS: OnceLock<regex::Regex> = OnceLock::new();
    let line =
        LINE.get_or_init(|| regex::Regex::new(r"^\s*\[(\d+),\d+\]").expect("valid MRC line regex"));
    let words = WORDS
        .get_or_init(|| regex::Regex::new(r"\((-?\d+),(-?\d+)\)").expect("valid MRC word regex"));
    let mut lrc_lines = Vec::new();
    let mut lx_lines = Vec::new();

    for value in content.lines() {
        let Some(captures) = line.captures(value) else {
            continue;
        };
        let Some(millis) = captures
            .get(1)
            .and_then(|value| value.as_str().parse::<u64>().ok())
        else {
            continue;
        };
        let Some(header) = captures.get(0) else {
            continue;
        };
        let body = &value[header.end()..];
        let text = words.replace_all(body, "");
        let timestamp = format_timestamp(millis);
        lrc_lines.push(format!("[{timestamp}]{text}"));

        let mut normalized = String::new();
        let mut previous_end = 0;
        for marker in words.captures_iter(body) {
            let Some(raw_marker) = marker.get(0) else {
                continue;
            };
            let start = marker
                .get(1)
                .and_then(|value| value.as_str().parse::<i64>().ok())
                .unwrap_or(millis as i64);
            let duration = marker
                .get(2)
                .and_then(|value| value.as_str().parse::<i64>().ok())
                .unwrap_or_default()
                .unsigned_abs();
            normalized.push_str(&format!(
                "<{},{}>{}",
                start.saturating_sub(millis as i64).max(0),
                duration,
                &body[previous_end..raw_marker.start()]
            ));
            previous_end = raw_marker.end();
        }
        if !normalized.is_empty() {
            lx_lines.push(format!("[{timestamp}]{normalized}"));
        }
    }

    (!lrc_lines.is_empty()).then(|| (lrc_lines.join("\n"), lx_lines.join("\n")))
}

fn format_timestamp(millis: u64) -> String {
    let minutes = millis / 60_000;
    let seconds = (millis % 60_000) / 1_000;
    let fraction = millis % 1_000;
    format!("{minutes:02}:{seconds:02}.{fraction:03}")
}

#[cfg(test)]
mod tests {
    use super::parse_mrc;

    #[test]
    fn converts_mrc_words_to_line_lrc() {
        let (lyric, lxlyric) = parse_mrc("[61234,1200]你(61234,600)好(61834,600)").unwrap();
        assert_eq!(lyric, "[01:01.234]你好");
        assert_eq!(lxlyric, "[01:01.234]<0,600>你<600,600>好");
    }
}
