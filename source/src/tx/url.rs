//! QQ音乐播放 URL 获取
//!
//! 流程:
//! 1. POST https://u.y.qq.com/cgi-bin/musicu.fcg 获取 vkey/purl
//! 2. 拼接: http://dl.stream.qqmusic.qq.com/{purl}

use crate::http::SendWithRetry;
use lx_core::model::song::SongInfo;
use lx_core::model::source::Quality;
use lx_core::traits::source::{FetchError, SongUrl};
use serde_json::Value;

use super::super::http;

pub async fn get_song_url(song: &SongInfo, quality: Quality) -> Result<SongUrl, FetchError> {
    let client = http::client();
    let song_mid = song
        .extra
        .get("songmid")
        .map(String::as_str)
        .unwrap_or(&song.id);
    let guid = format!("{}", std::process::id());

    let candidates = match quality {
        Quality::Flac24 => vec![
            ("AI00", "flac"),
            ("Q001", "flac"),
            ("Q000", "flac"),
            ("F000", "flac"),
            ("O801", "ogg"),
            ("M800", "mp3"),
            ("M500", "mp3"),
        ],
        Quality::Flac => vec![
            ("Q001", "flac"),
            ("Q000", "flac"),
            ("F000", "flac"),
            ("O801", "ogg"),
            ("M800", "mp3"),
            ("M500", "mp3"),
        ],
        Quality::High320 => vec![("M800", "mp3"), ("M500", "mp3")],
        Quality::Low128 => vec![("M500", "mp3")],
    };
    let filenames: Vec<String> = candidates
        .iter()
        .map(|(prefix, ext)| format!("{}{}{}.{}", prefix, song_mid, song_mid, ext))
        .collect();
    let songmids = vec![song_mid.to_string(); filenames.len()];
    let songtypes = vec![0; filenames.len()];

    let body = serde_json::json!({
        "comm": {"cv": 4747474, "ct": 24, "format": "json", "inCharset": "utf-8", "outCharset": "utf-8", "notice": 0, "platform": "yqq.json", "needNewCode": 1, "uin": 0},
        "req_1": {"module": "music.vkey.GetVkey", "method": "UrlGetVkey", "param": {
            "guid": guid, "songmid": songmids, "songtype": songtypes, "uin": "0", "loginflag": 1, "platform": "20", "filename": filenames
        }}
    });

    let body_str = serde_json::to_string(&body).map_err(|e| FetchError::Parse(e.to_string()))?;
    let resp = super::with_cookie(client.post("https://u.y.qq.com/cgi-bin/musicu.fcg"))
        .header("User-Agent", "QQMusic 14090508(android 12)")
        .header("Referer", "https://y.qq.com/")
        .header("Content-Type", "application/json")
        .body(body_str)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;
    let text = resp
        .text()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;
    let json: Value = serde_json::from_str(&text).map_err(|e| FetchError::Parse(e.to_string()))?;
    let infos = json["req_1"]["data"]["midurlinfo"]
        .as_array()
        .ok_or(FetchError::NotFound)?;

    for filename in &filenames {
        if let Some(info) = infos
            .iter()
            .find(|v| v["filename"].as_str() == Some(filename.as_str()))
        {
            let purl = info["purl"].as_str().unwrap_or("").trim_start_matches('/');
            if !purl.is_empty() {
                let actual_quality =
                    filename_quality(filename, song_mid, &candidates).unwrap_or(quality);
                let size_key = match actual_quality {
                    Quality::Low128 => "size_128mp3",
                    Quality::High320 => "size_320mp3",
                    Quality::Flac => "size_flac",
                    Quality::Flac24 => "size_hires",
                };
                let size = song
                    .extra
                    .get(size_key)
                    .and_then(|s| s.parse::<u64>().ok())
                    .filter(|&s| s > 0);
                return Ok(SongUrl {
                    url: format!("https://ws.stream.qqmusic.qq.com/{}", purl),
                    quality: actual_quality,
                    duration: song.duration,
                    cover_url: song.cover_url.clone(),
                    qualities: song.qualities.iter().copied().collect(),
                    headers: vec![],
                    size,
                    size_is_advisory: matches!(actual_quality, Quality::Flac | Quality::Flac24),
                    md5: None,
                    candidate_urls: vec![],
                    max_chunk_size: 0,
                });
            }
        }
    }
    Err(FetchError::NotFound)
}

fn filename_quality(
    filename: &str,
    song_mid: &str,
    candidates: &[(&str, &str)],
) -> Option<Quality> {
    candidates.iter().find_map(|(prefix, ext)| {
        let expected = format!("{}{}{}.{}", prefix, song_mid, song_mid, ext);
        if expected != filename {
            return None;
        }
        Some(match *prefix {
            "M500" => Quality::Low128,
            "M800" => Quality::High320,
            "AI00" => Quality::Flac24,
            _ => Quality::Flac,
        })
    })
}
