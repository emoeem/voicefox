//! QQ音乐播放 URL 获取
//!
//! 流程:
//! 1. POST https://u.y.qq.com/cgi-bin/musicu.fcg 获取 vkey/purl
//! 2. 拼接: http://dl.stream.qqmusic.qq.com/{purl}

use lx_core::model::song::SongInfo;
use lx_core::model::source::Quality;
use lx_core::traits::source::{FetchError, SongUrl};
use serde_json::Value;

use super::super::http;
use super::crypto;

pub async fn get_song_url(song: &SongInfo, quality: Quality) -> Result<SongUrl, FetchError> {
    let client = http::client();

    let body = serde_json::json!({
        "comm": {
            "ct": "11",
            "cv": "14090508",
            "v": "14090508",
            "tmeAppID": "qqmusic",
            "OpenUDID": "0",
            "tmeLoginType": "0",
            "uin": "0"
        },
        "req": {
            "module": "music.vkey.GetVkey",
            "method": "CgiGetVkey",
            "param": {
                "guid": "0",
                "songmid": [song.id],
                "songtype": [0],
                "uin": "0",
                "loginflag": 1,
                "platform": "20"
            }
        }
    });

    let body_str = serde_json::to_string(&body).map_err(|e| FetchError::Parse(e.to_string()))?;
    let sign = crypto::zzc_sign(&body_str);

    let url = format!("https://u.y.qq.com/cgi-bin/musicu.fcg?sign={}", sign);

    let resp = client
        .post(&url)
        .header("User-Agent", "QQMusic 14090508(android 12)")
        .header("Content-Type", "application/json")
        .body(body_str)
        .send()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    let text = resp
        .text()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    let json: Value = serde_json::from_str(&text).map_err(|e| FetchError::Parse(e.to_string()))?;

    let req_code = json["req"]["code"].as_i64().unwrap_or(-1);
    if req_code != 0 {
        return Err(FetchError::NotFound);
    }

    let midurlinfo = &json["req"]["data"]["midurlinfo"];
    let purl = midurlinfo[0]["purl"].as_str().unwrap_or("");

    if purl.is_empty() {
        return Err(FetchError::NotFound);
    }

    let play_url = format!("http://dl.stream.qqmusic.qq.com/{}", purl);

    let qualities: Vec<Quality> = song.qualities.iter().copied().collect();

    Ok(SongUrl {
        url: play_url,
        quality,
        duration: song.duration,
        cover_url: song.cover_url.clone(),
        qualities,
        headers: vec![],
    })
}
