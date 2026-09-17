//! QQ音乐播放 URL 获取。
//!
//! 使用当前 `GetVkeyServer/CgiGetVkey` 接口。文件名必须使用 `media_mid`，
//! 返回的播放地址由 `sip[0] + purl` 组成，并携带服务端返回的 vkey。

use super::super::http;
use crate::http::SendWithRetry;
use lx_core::model::song::SongInfo;
use lx_core::model::source::Quality;
use lx_core::traits::source::{FetchError, SongUrl};
use serde_json::Value;

pub async fn get_song_url(song: &SongInfo, quality: Quality) -> Result<SongUrl, FetchError> {
    let client = http::client();
    let song_mid = song
        .extra
        .get("songmid")
        .map(String::as_str)
        .unwrap_or(&song.id);
    let media_mid = song
        .extra
        .get("strMediaMid")
        .map(String::as_str)
        .filter(|mid| !mid.is_empty())
        .unwrap_or(song_mid);
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
        .map(|(prefix, ext)| format!("{}{}.{}", prefix, media_mid, ext))
        .collect();
    let songmids = vec![song_mid.to_string(); filenames.len()];
    let songtypes = vec![0; filenames.len()];

    let body = serde_json::json!({
        "req_0": {"module": "vkey.GetVkeyServer", "method": "CgiGetVkey", "param": {
            "guid": guid, "songmid": songmids, "songtype": songtypes, "uin": "0",
            "loginflag": 1, "platform": "20", "filename": filenames
        }},
        "comm": {"uin": 0, "format": "json", "ct": 24, "cv": 0}
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
    let data = &json["req_0"]["data"];
    let infos = data["midurlinfo"].as_array().ok_or(FetchError::NotFound)?;
    let sip = data["sip"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .unwrap_or("");
    if sip.is_empty() {
        return Err(FetchError::NotFound);
    }

    for filename in &filenames {
        let Some(info) = infos
            .iter()
            .find(|v| v["filename"].as_str() == Some(filename.as_str()))
        else {
            continue;
        };
        let purl = info["purl"].as_str().unwrap_or("").trim_start_matches('/');
        let vkey = info["vkey"].as_str().unwrap_or("");
        if purl.is_empty() || vkey.is_empty() {
            continue;
        }
        let actual_quality = filename_quality(filename, media_mid, &candidates).unwrap_or(quality);
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
            url: format!(
                "{}{}?guid={}&vkey={}&uin=0&fromtag=120093",
                sip, purl, guid, vkey
            ),
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
    Err(FetchError::NotFound)
}

fn filename_quality(
    filename: &str,
    media_mid: &str,
    candidates: &[(&str, &str)],
) -> Option<Quality> {
    candidates.iter().find_map(|(prefix, ext)| {
        let expected = format!("{}{}.{}", prefix, media_mid, ext);
        (expected == filename).then_some(match *prefix {
            "M500" => Quality::Low128,
            "M800" => Quality::High320,
            "AI00" => Quality::Flac24,
            _ => Quality::Flac,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn qq_filename_uses_media_mid_once() {
        let candidates = [("M800", "mp3"), ("M500", "mp3")];
        assert_eq!(
            filename_quality("M800media123.mp3", "media123", &candidates),
            Some(Quality::High320)
        );
        assert_eq!(
            filename_quality("M800media123media123.mp3", "media123", &candidates),
            None
        );
    }
}
