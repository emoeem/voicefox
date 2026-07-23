//! 网易云音乐歌词获取
//!
//! POST https://interface3.music.163.com/eapi/song/lyric/v1
//! 使用 eapi 加密

use lx_core::model::lyric::LyricData;
use lx_core::model::song::SongInfo;
use lx_core::traits::source::FetchError;
use serde_json::Value;

use super::super::http;
use super::crypto;

/// 从响应中提取歌词字段（.lyric）
fn extract_lyric(root: &Value, path: &str) -> Option<String> {
    // path like "lrc.lyric" → root["lrc"]["lyric"]
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = root;
    for part in &parts {
        current = current.get(part)?;
    }
    current.as_str().map(|s| s.to_string())
}

/// 修正 YRC 时间标签：[mm:ss:ms] → [mm:ss.ms]
fn fix_yrc_timestamps(yrc: &str) -> String {
    let re = regex::Regex::new(r"\[(\d+):(\d+):(\d+)\]").unwrap();
    re.replace_all(yrc, "[$1:$2.$3]").to_string()
}

pub async fn get_lyric(song: &SongInfo) -> Result<LyricData, FetchError> {
    let url = "/api/song/lyric/v1";
    let data = serde_json::json!({
        "id": song.id,
        "cp": false,
        "tv": 0,
        "lv": 0,
        "rv": 0,
        "kv": 0,
        "yv": 0,
        "ytv": 0,
        "yrv": 0,
    });

    let encrypted = crypto::eapi(url, &data);

    let client = http::client();
    let resp = client
        .post("https://interface3.music.163.com/eapi/song/lyric/v1")
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
        )
        .header("origin", "https://music.163.com")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!("params={}", encrypted))
        .send()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    let text = resp
        .text()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    let json: Value = serde_json::from_str(&text).map_err(|e| FetchError::Parse(e.to_string()))?;

    // 检查响应码
    let code = json["code"].as_i64().unwrap_or(0);
    if code != 200 {
        // 歌词获取失败不报错，返回空
        return Ok(LyricData::default());
    }

    let lrc = extract_lyric(&json, "lrc.lyric").unwrap_or_default();
    let tlyric = extract_lyric(&json, "tlyric.lyric");
    let rlyric = extract_lyric(&json, "romalrc.lyric");
    let lxlyric = extract_lyric(&json, "yrc.lyric").map(|y| fix_yrc_timestamps(&y));

    Ok(LyricData {
        lyric: lrc.clone(),
        tlyric,
        rlyric,
        lxlyric,
        raw_lrc: Some(lrc),
    })
}
