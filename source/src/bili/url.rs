use std::collections::BTreeSet;

use lx_core::model::song::SongInfo;
use lx_core::model::source::Quality;
use lx_core::traits::source::{FetchError, SongUrl};
use serde_json::Value;

use super::{BILI_REFERER, BiliSource, USER_AGENT};

const VIEW_ENDPOINT: &str = "https://api.bilibili.com/x/web-interface/view";
const PLAY_ENDPOINT: &str = "https://api.bilibili.com/x/player/wbi/playurl";

pub async fn get_song_url(
    source: &BiliSource,
    song: &SongInfo,
    quality: Quality,
) -> Result<SongUrl, FetchError> {
    let bvid = song
        .extra
        .get("bvid")
        .map(String::as_str)
        .unwrap_or(&song.id);
    let cid = match song.extra.get("cid").filter(|value| !value.is_empty()) {
        Some(cid) => cid.to_string(),
        None => resolve_cid(source, bvid).await?,
    };
    let json = source
        .signed_get(
            PLAY_ENDPOINT,
            &[
                ("bvid", bvid.to_string()),
                ("cid", cid),
                ("qn", quality_qn(quality).to_string()),
                ("fnval", "16".to_string()),
                ("fnver", "0".to_string()),
                ("fourk", "1".to_string()),
                ("try_look", "1".to_string()),
                ("voice_balance", "1".to_string()),
            ],
        )
        .await
        .map_err(FetchError::Network)?;
    if json["code"].as_i64() != Some(0) {
        return Err(FetchError::Other(
            json["message"]
                .as_str()
                .unwrap_or("哔哩哔哩播放地址请求失败")
                .to_string(),
        ));
    }
    let selected = choose_audio(&json, quality).ok_or(FetchError::NotFound)?;
    let mut qualities = BTreeSet::new();
    qualities.insert(Quality::Low128);
    if selected.max_bandwidth > 160_000 {
        qualities.insert(Quality::High320);
    }
    Ok(SongUrl {
        url: selected.url,
        quality: selected.quality,
        duration: song.duration,
        cover_url: song.cover_url.clone(),
        qualities: qualities.into_iter().collect(),
        headers: vec![
            ("Referer".to_string(), BILI_REFERER.to_string()),
            ("User-Agent".to_string(), USER_AGENT.to_string()),
        ],
    })
}

async fn resolve_cid(source: &BiliSource, bvid: &str) -> Result<String, FetchError> {
    let json = source
        .get_json(VIEW_ENDPOINT, &[("bvid", bvid.to_string())], false)
        .await
        .map_err(FetchError::Network)?;
    if json["code"].as_i64() != Some(0) {
        return Err(FetchError::Other("获取哔哩哔哩视频信息失败".to_string()));
    }
    json["data"]["pages"]
        .as_array()
        .and_then(|pages| pages.first())
        .and_then(|page| page["cid"].as_u64())
        .map(|cid| cid.to_string())
        .ok_or(FetchError::NotFound)
}

struct SelectedAudio {
    url: String,
    quality: Quality,
    max_bandwidth: u64,
}

fn choose_audio(json: &Value, requested: Quality) -> Option<SelectedAudio> {
    let mut candidates = Vec::new();
    append_audio_candidates(&json["data"]["dash"]["audio"], &mut candidates);
    append_audio_candidates(&json["data"]["dash"]["dolby"]["audio"], &mut candidates);
    append_audio_candidates(&json["data"]["dash"]["flac"]["audio"], &mut candidates);
    candidates.retain(|item| stream_url(item).is_some());

    let max_bandwidth = candidates
        .iter()
        .map(|item| item["bandwidth"].as_u64().unwrap_or_default())
        .max()
        .unwrap_or_default();
    let selected = match requested {
        Quality::Low128 => candidates.iter().min_by_key(|item| {
            item["bandwidth"]
                .as_u64()
                .unwrap_or_default()
                .abs_diff(128_000)
        }),
        Quality::High320 | Quality::Flac | Quality::Flac24 => candidates
            .iter()
            .max_by_key(|item| item["bandwidth"].as_u64().unwrap_or_default()),
    };
    if let Some(item) = selected {
        let bandwidth = item["bandwidth"].as_u64().unwrap_or_default();
        let url = stream_url(item)?;
        return Some(SelectedAudio {
            url,
            quality: if bandwidth > 160_000 {
                Quality::High320
            } else {
                Quality::Low128
            },
            max_bandwidth,
        });
    }

    let url = json["data"]["durl"]
        .as_array()
        .and_then(|items| items.first())
        .and_then(|item| item["url"].as_str())?
        .to_string();
    Some(SelectedAudio {
        url,
        quality: Quality::Low128,
        max_bandwidth: 128_000,
    })
}

fn append_audio_candidates<'a>(value: &'a Value, candidates: &mut Vec<&'a Value>) {
    if let Some(items) = value.as_array() {
        candidates.extend(items);
    } else if value.is_object() {
        candidates.push(value);
    }
}

fn stream_url(item: &Value) -> Option<String> {
    item["baseUrl"]
        .as_str()
        .or_else(|| item["base_url"].as_str())
        .or_else(|| {
            item["backupUrl"]
                .as_array()
                .or_else(|| item["backup_url"].as_array())
                .and_then(|urls| urls.first())
                .and_then(Value::as_str)
        })
        .filter(|url| !url.is_empty())
        .map(str::to_string)
}

fn quality_qn(quality: Quality) -> u32 {
    match quality {
        Quality::Low128 => 32,
        Quality::High320 | Quality::Flac | Quality::Flac24 => 80,
    }
}

#[cfg(test)]
mod tests {
    use lx_core::model::source::Quality;
    use serde_json::json;

    use super::choose_audio;

    #[test]
    fn requested_quality_selects_different_audio_streams() {
        let json = json!({
            "data": {
                "dash": {
                    "audio": [
                        {"bandwidth": 64_000, "baseUrl": "https://example.com/64"},
                        {"bandwidth": 128_000, "baseUrl": "https://example.com/128"},
                        {"bandwidth": 192_000, "baseUrl": "https://example.com/192"}
                    ]
                }
            }
        });

        assert_eq!(
            choose_audio(&json, Quality::Low128).unwrap().url,
            "https://example.com/128"
        );
        assert_eq!(
            choose_audio(&json, Quality::High320).unwrap().url,
            "https://example.com/192"
        );
    }
}
