//! mg 歌词获取（LRC + TRC）
//!
//! Phase 6 简化：只取 LRC + TRC（不实现 MRC TEA 解密）

use lx_core::model::lyric::LyricData;
use lx_core::model::song::SongInfo;
use lx_core::traits::source::FetchError;

use super::super::http;

/// GET 获取文本内容
async fn fetch_text(client: &reqwest::Client, url: &str) -> Result<String, FetchError> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    resp.text()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))
}

pub async fn get_lyric(song: &SongInfo) -> Result<LyricData, FetchError> {
    let client = http::client();

    // LRC 歌词
    let lyric = match song.extra.get("lrcUrl") {
        Some(lrc_url) if !lrc_url.is_empty() => {
            fetch_text(&client, lrc_url).await.unwrap_or_default()
        }
        _ => String::new(),
    };

    // TRC 翻译歌词
    let tlyric = match song.extra.get("trcUrl") {
        Some(trc_url) if !trc_url.is_empty() => {
            fetch_text(&client, trc_url).await.ok()
        }
        _ => None,
    };

    Ok(LyricData {
        lyric,
        tlyric,
        rlyric: None,
        lxlyric: None,
        raw_lrc: None,
    })
}
