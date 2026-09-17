//! 千千音乐播放地址。
//!
//! `/v1/song/tracklink` 按 `rate` 返回对应档位的直链。与 music-lib 一致，
//! 从高到低依次尝试（3000 → 320 → 128 → 64），全部拿不到完整文件时退回
//! 试听片段（`trail_audio_info`），至少让用户能听到一段。

use lx_core::model::song::SongInfo;
use lx_core::model::source::Quality;
use lx_core::traits::source::{FetchError, SongUrl};
use serde_json::Value;

use super::crypto::signed_tracklink_query;
use super::song::{signed_get, value_u64};

/// 档位请求阶梯：请求音质越高，起手的 rate 越大。
fn rate_ladder(quality: Quality) -> &'static [&'static str] {
    match quality {
        Quality::Flac24 | Quality::Flac => &["3000", "320", "128", "64"],
        Quality::High320 => &["320", "128", "64"],
        Quality::Low128 => &["128", "64"],
    }
}

/// 实际拿到的档位：3000 视作无损。
fn quality_for_rate(rate: &str) -> Quality {
    match rate {
        "3000" => Quality::Flac,
        "320" => Quality::High320,
        _ => Quality::Low128,
    }
}

pub async fn get_song_url(song: &SongInfo, quality: Quality) -> Result<SongUrl, FetchError> {
    let tsid = song
        .extra
        .get("tsid")
        .map(String::as_str)
        .unwrap_or(&song.id);
    if tsid.is_empty() {
        return Err(FetchError::NotFound);
    }

    let mut preview: Option<(String, String)> = None;
    for rate in rate_ladder(quality) {
        let query = signed_tracklink_query(tsid, rate);
        let url = format!("https://music.91q.com/v1/song/tracklink?{query}");
        let json = match signed_get(&url).await {
            Ok(json) => json,
            Err(error) => {
                tracing::debug!("qianqian tracklink failed for rate {rate}: {error:?}");
                continue;
            }
        };
        let data = &json["data"];
        let path = data["path"].as_str().unwrap_or_default();
        if !path.is_empty() {
            return Ok(build_song_url(path, rate, data, song));
        }
        if preview.is_none()
            && let Some(trail) = data["trail_audio_info"]["path"].as_str()
            && !trail.is_empty()
        {
            preview = Some((trail.to_string(), (*rate).to_string()));
        }
    }

    let (url, rate) = preview.ok_or(FetchError::NotFound)?;
    tracing::debug!("qianqian only returned a preview clip at rate {rate}");
    Ok(build_song_url(&url, &rate, &Value::Null, song))
}

fn build_song_url(path: &str, rate: &str, data: &Value, song: &SongInfo) -> SongUrl {
    SongUrl {
        url: path.to_string(),
        quality: quality_for_rate(rate),
        duration: song.duration,
        cover_url: song.cover_url.clone(),
        qualities: vec![quality_for_rate(rate)],
        headers: Vec::new(),
        size: value_u64(&data["size"]).filter(|size| *size > 0),
        // 千千返回的 size 与实际落盘体积可能略有出入，只做参考。
        size_is_advisory: true,
        md5: None,
        candidate_urls: Vec::new(),
        max_chunk_size: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_starts_at_the_requested_quality() {
        assert_eq!(rate_ladder(Quality::Flac24)[0], "3000");
        assert_eq!(rate_ladder(Quality::High320)[0], "320");
        assert_eq!(rate_ladder(Quality::Low128)[0], "128");
        // 低档请求不会向上越级。
        assert!(!rate_ladder(Quality::Low128).contains(&"3000"));
    }

    #[test]
    fn rate_maps_back_to_a_quality() {
        assert_eq!(quality_for_rate("3000"), Quality::Flac);
        assert_eq!(quality_for_rate("320"), Quality::High320);
        assert_eq!(quality_for_rate("64"), Quality::Low128);
    }
}
