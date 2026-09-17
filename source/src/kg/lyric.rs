//! kg 歌词获取（两步流程：搜索 → 下载）
//!
//! Step 1: GET http://lyrics.kugou.com/search → 获取 id + accesskey
//! Step 2: GET http://lyrics.kugou.com/download → base64 解码 → KRC/LRC 文本

use crate::http::SendWithRetry;
use std::sync::OnceLock;

use lx_core::model::lyric::LyricData;
use lx_core::model::song::SongInfo;
use lx_core::traits::source::FetchError;

use super::super::crypto;
use super::super::http;

/// Step 1: 搜索歌词，获取 id 和 accesskey
async fn search_lyric(
    client: &reqwest::Client,
    song: &SongInfo,
) -> Result<(String, String), FetchError> {
    let hash: String = song
        .extra
        .get("SQFileHash")
        .or_else(|| song.extra.get("HQFileHash"))
        .or_else(|| song.extra.get("FileHash"))
        .map(|h| h.to_lowercase())
        .unwrap_or_default();

    let keyword = format!("{}+{}", song.name, song.singer);
    let encoded_keyword = urlencoding::encode(&keyword);
    let duration_secs = song.duration.as_secs();

    let url = format!(
        "http://lyrics.kugou.com/search?ver=1&man=yes&client=pc&keyword={}&hash={}&timelength={}",
        encoded_keyword, hash, duration_secs
    );

    let resp = super::with_cookie(client.get(&url))
        .header("KG-RC", "1")
        .header("KG-THash", "expand_search_manager.cpp:852736169:451")
        .header("User-Agent", "KuGou2012-9020-ExpandSearchManager")
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?
        .error_for_status()
        .map_err(|e| FetchError::Network(e.to_string()))?;

    let text = resp
        .text()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| FetchError::Parse(e.to_string()))?;

    let candidates = match &json["candidates"] {
        serde_json::Value::Array(arr) if !arr.is_empty() => arr,
        _ => return Err(FetchError::NotFound),
    };

    let first = &candidates[0];
    let id = first["id"].as_str().unwrap_or("").to_string();
    let accesskey = first["accesskey"].as_str().unwrap_or("").to_string();

    if id.is_empty() {
        return Err(FetchError::NotFound);
    }

    Ok((id, accesskey))
}

/// Step 2: 下载歌词（LRC 格式），base64 解码
async fn download_lyric(
    client: &reqwest::Client,
    id: &str,
    accesskey: &str,
    format: &str,
) -> Result<Vec<u8>, FetchError> {
    let url = format!(
        "http://lyrics.kugou.com/download?ver=1&client=pc&id={}&accesskey={}&fmt={}&charset=utf8",
        id, accesskey, format
    );

    let resp = super::with_cookie(client.get(&url))
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?
        .error_for_status()
        .map_err(|e| FetchError::Network(e.to_string()))?;

    let text = resp
        .text()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| FetchError::Parse(e.to_string()))?;

    let content = json["content"].as_str().ok_or(FetchError::NotFound)?;

    crypto::base64_decode(content)
        .map_err(|e| FetchError::Parse(format!("base64 decode failed: {}", e)))
}

pub async fn get_lyric(song: &SongInfo) -> Result<LyricData, FetchError> {
    let client = http::client();

    // 搜索失败（含网络错误）向上传播，调用方据此区分“确认无词”与“暂时不可用”
    let (id, accesskey) = search_lyric(&client, song).await?;

    if let Ok(encrypted) = download_lyric(&client, &id, &accesskey, "krc").await
        && let Ok(krc) = super::crypto::decrypt_krc(&encrypted)
    {
        let parsed = parse_krc(&krc);
        if lyric_has_content(&parsed) {
            return Ok(parsed);
        }
    }

    let lyric = download_lyric(&client, &id, &accesskey, "lrc")
        .await
        .ok()
        .and_then(|decoded| String::from_utf8(decoded).ok())
        .unwrap_or_default();

    Ok(LyricData {
        lyric,
        tlyric: None,
        rlyric: None,
        lxlyric: None,
        raw_lrc: None,
    })
}

fn parse_krc(content: &str) -> LyricData {
    static LINE: OnceLock<regex::Regex> = OnceLock::new();
    static WORDS: OnceLock<regex::Regex> = OnceLock::new();
    let content = content.replace('\r', "");
    let (content, rlyric_lines, tlyric_lines) = extract_language_metadata(&content);
    let line =
        LINE.get_or_init(|| regex::Regex::new(r"^\[(\d+),\d+\]").expect("valid KRC line regex"));
    let words = WORDS.get_or_init(|| {
        regex::Regex::new(r"<(-?\d+),(-?\d+)(?:,-?\d+)?>").expect("valid KRC word regex")
    });
    let mut timestamps = Vec::new();
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
        let timestamp = format_timestamp(millis);
        let body = &value[header.end()..];
        let text = decode_entities(words.replace_all(body, "").as_ref());
        timestamps.push(millis);
        lrc_lines.push(format!("[{timestamp}]{text}"));

        let normalized = words.replace_all(body, |captures: &regex::Captures<'_>| {
            let start = captures.get(1).map_or("0", |value| value.as_str());
            let duration = captures.get(2).map_or("0", |value| value.as_str());
            format!("<{start},{duration}>")
        });
        lx_lines.push(format!(
            "[{timestamp}]{}",
            decode_entities(normalized.as_ref())
        ));
    }

    LyricData {
        lyric: lrc_lines.join("\n"),
        tlyric: timed_language_lyric(tlyric_lines, &timestamps),
        rlyric: timed_language_lyric(rlyric_lines, &timestamps),
        lxlyric: (!lx_lines.is_empty()).then(|| lx_lines.join("\n")),
        raw_lrc: Some(content),
    }
}

fn extract_language_metadata(content: &str) -> (String, Option<Vec<String>>, Option<Vec<String>>) {
    static LANGUAGE: OnceLock<regex::Regex> = OnceLock::new();
    let language = LANGUAGE.get_or_init(|| {
        regex::Regex::new(r"(?m)^\[language:([A-Za-z0-9+/=]+)\]\n?").expect("valid language regex")
    });
    let mut rlyric = None;
    let mut tlyric = None;

    if let Some(captures) = language.captures(content)
        && let Some(encoded) = captures.get(1)
        && let Ok(decoded) = crypto::base64_decode(encoded.as_str())
        && let Ok(json) = serde_json::from_slice::<serde_json::Value>(&decoded)
        && let Some(items) = json["content"].as_array()
    {
        for item in items {
            let Some(lines) = decode_language_lines(&item["lyricContent"]) else {
                continue;
            };
            match item["type"].as_i64() {
                Some(0) => rlyric = Some(lines),
                Some(1) => tlyric = Some(lines),
                _ => {}
            }
        }
    }

    (language.replace(content, "").into_owned(), rlyric, tlyric)
}

fn decode_language_lines(value: &serde_json::Value) -> Option<Vec<String>> {
    value.as_array().map(|lines| {
        lines
            .iter()
            .map(|line| {
                line.as_array().map_or_else(
                    || line.as_str().unwrap_or_default().to_string(),
                    |parts| {
                        parts
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .collect::<String>()
                    },
                )
            })
            .collect()
    })
}

fn timed_language_lyric(lines: Option<Vec<String>>, timestamps: &[u64]) -> Option<String> {
    let lyric = lines?
        .into_iter()
        .zip(timestamps.iter().copied())
        .map(|(text, millis)| format!("[{}]{}", format_timestamp(millis), decode_entities(&text)))
        .collect::<Vec<_>>()
        .join("\n");
    (!lyric.trim().is_empty()).then_some(lyric)
}

fn format_timestamp(millis: u64) -> String {
    let minutes = millis / 60_000;
    let seconds = (millis % 60_000) / 1_000;
    let fraction = millis % 1_000;
    format!("{minutes:02}:{seconds:02}.{fraction:03}")
}

fn decode_entities(value: &str) -> String {
    value
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#039;", "'")
}

fn lyric_has_content(data: &LyricData) -> bool {
    !data.lyric.trim().is_empty()
        || data
            .lxlyric
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use crate::crypto;
    use serde_json::json;

    use super::parse_krc;

    #[test]
    fn parses_krc_words_and_language_metadata() {
        let metadata = json!({
            "version": 1,
            "content": [
                {"type": 0, "lyricContent": [["ni", " hao"], ["zai", " jian"]]},
                {"type": 1, "lyricContent": [["你", "好"], ["再", "见"]]}
            ]
        });
        let encoded = crypto::base64_encode(metadata.to_string().as_bytes());
        let content = format!(
            "[language:{encoded}]\n\
             [61234,1200]<0,600,0>你<600,600,0>好\n\
             [62434,1200]<0,600,0>再<600,600,0>见"
        );
        let parsed = parse_krc(&content);

        assert_eq!(parsed.lyric, "[01:01.234]你好\n[01:02.434]再见");
        assert_eq!(
            parsed.lxlyric.as_deref(),
            Some(
                "[01:01.234]<0,600>你<600,600>好\n\
                 [01:02.434]<0,600>再<600,600>见"
            )
        );
        assert_eq!(
            parsed.tlyric.as_deref(),
            Some("[01:01.234]你好\n[01:02.434]再见")
        );
        assert_eq!(
            parsed.rlyric.as_deref(),
            Some("[01:01.234]ni hao\n[01:02.434]zai jian")
        );
    }
}
