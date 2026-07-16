//! kg 歌词获取（两步流程：搜索 → 下载）
//!
//! Step 1: GET http://lyrics.kugou.com/search → 获取 id + accesskey
//! Step 2: GET http://lyrics.kugou.com/download → base64 解码 → LRC 文本

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

    let resp = client
        .get(&url)
        .header("KG-RC", "1")
        .header("KG-THash", "expand_search_manager.cpp:852736169:451")
        .header("User-Agent", "KuGou2012-9020-ExpandSearchManager")
        .send()
        .await
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
) -> Result<String, FetchError> {
    let url = format!(
        "http://lyrics.kugou.com/download?ver=1&client=pc&id={}&accesskey={}&fmt=lrc&charset=utf8",
        id, accesskey
    );

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    let text = resp
        .text()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| FetchError::Parse(e.to_string()))?;

    let content = json["content"].as_str().ok_or(FetchError::NotFound)?;

    // base64 解码得到 LRC 文本
    let decoded = crypto::base64_decode(content)
        .map_err(|e| FetchError::Parse(format!("base64 decode failed: {}", e)))?;

    String::from_utf8(decoded).map_err(|e| FetchError::Parse(format!("utf8 decode failed: {}", e)))
}

pub async fn get_lyric(song: &SongInfo) -> Result<LyricData, FetchError> {
    let client = http::client();

    let (id, accesskey) = match search_lyric(&client, song).await {
        Ok(result) => result,
        Err(_) => {
            // 任何搜索错误都返回空歌词，不 panic
            return Ok(LyricData::default());
        }
    };

    let lyric = download_lyric(&client, &id, &accesskey)
        .await
        .unwrap_or_default();

    Ok(LyricData {
        lyric,
        tlyric: None,
        rlyric: None,
        lxlyric: None,
        raw_lrc: None,
    })
}
