//! QQ音乐歌词获取
//!
//! POST https://u.y.qq.com/cgi-bin/musicu.fcg
//! 返回 base64 编码的歌词，需要解码

use lx_core::model::lyric::LyricData;
use lx_core::model::song::SongInfo;
use lx_core::traits::source::FetchError;
use serde_json::Value;

use super::super::crypto as common_crypto;
use super::super::http;
use super::crypto;

/// 尝试 base64 解码字符串字段
fn decode_lyric_field(json: &Value, field: &str) -> Option<String> {
    let encoded = json.get(field)?.as_str()?;
    if encoded.is_empty() {
        return None;
    }
    match common_crypto::base64_decode(encoded) {
        Ok(bytes) => String::from_utf8(bytes).ok(),
        Err(_) => None,
    }
}

pub async fn get_lyric(song: &SongInfo) -> Result<LyricData, FetchError> {
    let client = http::client();

    // 获取 songId（优先从 extra 取，否则用 id 本身）
    let song_id = song
        .extra
        .get("songId")
        .cloned()
        .unwrap_or_else(|| song.id.clone());

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
            "module": "music.musichallSong.PlayLyricInfo",
            "method": "GetPlayLyricInfo",
            "param": {
                "songID": song_id.parse::<i64>().unwrap_or(0),
                "crypt": 1,
                "qrc": 1,
                "trans": 1,
                "roma": 1
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
        return Ok(LyricData::default());
    }

    let data = &json["req"]["data"];

    let lyric = decode_lyric_field(data, "lyric").unwrap_or_default();
    let tlyric = decode_lyric_field(data, "trans");
    let rlyric = decode_lyric_field(data, "roma");
    let lxlyric = decode_lyric_field(data, "qrc"); // QRC 格式存为 lxlyric

    Ok(LyricData {
        lyric: lyric.clone(),
        tlyric,
        rlyric,
        lxlyric,
        raw_lrc: Some(lyric),
    })
}
