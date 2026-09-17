//! 千千音乐歌词：`/v1/song/info` 给出 LRC 文件地址，再下载文本。

use lx_core::model::lyric::LyricData;
use lx_core::model::song::SongInfo;
use lx_core::traits::source::FetchError;

use crate::http;
use crate::http::SendWithRetry;

use super::crypto::{APP_ID, REFERER, USER_AGENT, signed_query};
use super::song::{FetchErrorKind, field, signed_get, value_string};

pub async fn get_lyric(song: &SongInfo) -> Result<LyricData, FetchError> {
    let tsid = song
        .extra
        .get("tsid")
        .cloned()
        .unwrap_or_else(|| song.id.clone());
    if tsid.is_empty() {
        return Err(FetchError::NotFound);
    }
    let query = signed_query(&[("TSID", tsid.as_str()), ("appid", APP_ID)]);
    let json = signed_get(&format!("https://music.91q.com/v1/song/info?{query}"))
        .await
        .map_err(FetchErrorKind::into_fetch)?;
    let lyric_url = json["data"]
        .as_array()
        .and_then(|items| items.first())
        .and_then(|item| field(item, "lyric"))
        .map(value_string)
        .filter(|url| !url.is_empty())
        .ok_or(FetchError::NotFound)?;

    let text = http::client()
        .get(&lyric_url)
        .header("User-Agent", USER_AGENT)
        .header("Referer", REFERER)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .text()
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?;
    if text.trim().is_empty() {
        return Err(FetchError::NotFound);
    }
    Ok(LyricData {
        lyric: text,
        ..LyricData::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_lyric_url_from_the_first_item() {
        let json = serde_json::json!({
            "data": [{ "lyric": "https://cdn/lyric.lrc" }]
        });
        let url = json["data"]
            .as_array()
            .and_then(|items| items.first())
            .and_then(|item| field(item, "lyric"))
            .map(value_string);
        assert_eq!(url.as_deref(), Some("https://cdn/lyric.lrc"));
    }
}
