use std::collections::BTreeSet;

use lx_core::model::song::SongInfo;
use lx_core::model::source::Quality;
use lx_core::traits::source::{FetchError, SongUrl};
use serde_json::Value;

use super::BiliSource;

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
    let url = choose_audio_url(&json).ok_or(FetchError::NotFound)?;
    let mut qualities = BTreeSet::new();
    qualities.insert(Quality::Low128);
    qualities.insert(Quality::High320);
    Ok(SongUrl {
        url,
        quality,
        duration: song.duration,
        cover_url: song.cover_url.clone(),
        qualities: qualities.into_iter().collect(),
    })
}

async fn resolve_cid(source: &BiliSource, bvid: &str) -> Result<String, FetchError> {
    let json = source
        .get_json(
            VIEW_ENDPOINT,
            &[("bvid", bvid.to_string())],
            false,
        )
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

fn choose_audio_url(json: &Value) -> Option<String> {
    let audio = json["data"]["dash"]["audio"].as_array()?;
    audio
        .iter()
        .max_by_key(|item| item["bandwidth"].as_u64().unwrap_or_default())
        .and_then(|item| {
            item["baseUrl"]
                .as_str()
                .or_else(|| item["base_url"].as_str())
        })
        .map(str::to_string)
}

fn quality_qn(quality: Quality) -> u32 {
    match quality {
        Quality::Low128 => 32,
        Quality::High320 | Quality::Flac | Quality::Flac24 => 80,
    }
}
