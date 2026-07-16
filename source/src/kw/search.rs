//! kw 搜索 API
//!
//! GET http://search.kuwo.cn/r.s?client=kt&all={keyword}&pn={page-1}&rn={limit}&...

use std::collections::BTreeSet;
use std::time::Duration;

use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{SearchError, SearchResult};
use serde_json::Value;

use super::super::http;

/// 从 bitrate 值映射到 Quality
fn bitrate_to_quality(bitrate: u32) -> Option<Quality> {
    match bitrate {
        4000 => Some(Quality::Flac24),
        2000 => Some(Quality::Flac),
        320 => Some(Quality::High320),
        128 => Some(Quality::Low128),
        _ => None,
    }
}

/// 解析 N_MINFO 音质信息字符串
/// 格式: "level:xxxx,bitrate:4000,format:flac,size:50.1M;level:xxxx,bitrate:320,format:mp3,size:10.2M"
fn parse_qualities(n_minfo: &str) -> BTreeSet<Quality> {
    let mut qualities = BTreeSet::new();
    for part in n_minfo.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let mut bitrate = 0u32;
        for kv in part.split(',') {
            let kv = kv.trim();
            if let Some((k, v)) = kv.split_once(':') {
                if k == "bitrate" {
                    bitrate = v.parse().unwrap_or(0);
                    break;
                }
            }
        }
        if let Some(q) = bitrate_to_quality(bitrate) {
            qualities.insert(q);
        }
    }
    qualities
}

/// 从 JSON Value 提取 u32（兼容字符串和数字类型）
fn json_u32(value: &Value) -> Option<u32> {
    value
        .as_u64()
        .map(|v| v as u32)
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

pub async fn search(keyword: &str, page: u32, limit: u32) -> Result<SearchResult, SearchError> {
    let encoded_keyword = urlencoding::encode(keyword);
    let url = format!(
        "http://search.kuwo.cn/r.s?client=kt&all={}&pn={}&rn={}&uid=794762570&ver=kwplayer_ar_9.2.2.1&vipver=1&show_copyright_off=1&newver=1&ft=music&cluster=0&strategy=2012&encoding=utf8&rformat=json&vermerge=1&mobi=1&issubtitle=1",
        encoded_keyword,
        page.saturating_sub(1),
        limit
    );

    let client = http::client();
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| SearchError::Network(e.to_string()))?;

    let text = resp
        .text()
        .await
        .map_err(|e| SearchError::Network(e.to_string()))?;

    let json: Value =
        serde_json::from_str(&text).map_err(|e| SearchError::Parse(e.to_string()))?;

    let total = json_u32(&json["TOTAL"]).unwrap_or(0);
    let show_count = json_u32(&json["SHOW"]).unwrap_or(0);

    let abslist = match &json["abslist"] {
        Value::Array(arr) => arr,
        _ => {
            return Ok(SearchResult {
                items: vec![],
                total: 0,
                has_more: false,
            });
        }
    };

    let mut items = Vec::with_capacity(abslist.len());

    for item in abslist {
        // 歌曲 ID：去掉 "MUSIC_" 前缀
        let music_rid = item["MUSICRID"].as_str().unwrap_or("");
        let song_id = music_rid
            .strip_prefix("MUSIC_")
            .unwrap_or(music_rid)
            .to_string();

        let name = item["SONGNAME"].as_str().unwrap_or("").to_string();

        // 歌手名：& 替换为 、
        let artist = item["ARTIST"]
            .as_str()
            .unwrap_or("")
            .replace('&', "、");

        let album = item["ALBUM"].as_str().unwrap_or("").to_string();
        let album_id = item["ALBUMID"].as_str().unwrap_or("").to_string();

        // 时长：秒 → Duration
        let duration_secs = item["DURATION"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        // 音质列表
        let n_minfo = item["N_MINFO"].as_str().unwrap_or("");
        let qualities = parse_qualities(n_minfo);

        let mut song = SongInfo::new(song_id, SourceId::Kw, name, artist);
        song.album_name = album;
        song.album_id = album_id;
        song.duration = Duration::from_secs(duration_secs);
        song.qualities = qualities;

        items.push(song);
    }

    // 判断是否还有更多结果
    let has_more = items.len() >= limit as usize && show_count >= limit;

    Ok(SearchResult {
        items,
        total,
        has_more,
    })
}
