//! 酷狗排行榜，接口结构参考 lx-music-desktop kg/leaderboard.js。

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{SearchError, SearchResult};
use serde_json::Value;

use crate::http;

pub async fn get_list(rank_id: &str, page: u32, limit: u32) -> Result<SearchResult, SearchError> {
    let url = format!(
        "http://mobilecdnbj.kugou.com/api/v3/rank/song?version=9108&ranktype=1&plat=0&pagesize={limit}&area_code=1&page={page}&rankid={rank_id}&with_res_tag=0&show_portrait_mv=1"
    );
    let json: Value = http::client()
        .get(url)
        .send()
        .await
        .map_err(|error| SearchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| SearchError::Parse(error.to_string()))?;
    if json["errcode"].as_i64().unwrap_or(-1) != 0 {
        return Err(SearchError::Api(
            json["error"]
                .as_str()
                .unwrap_or("酷狗榜单请求失败")
                .to_string(),
        ));
    }

    let total = json["data"]["total"].as_u64().unwrap_or(0) as u32;
    let raw_items = json["data"]["info"]
        .as_array()
        .ok_or_else(|| SearchError::Parse("榜单歌曲列表为空".to_string()))?;
    let mut items = Vec::with_capacity(raw_items.len());
    for item in raw_items {
        let id = value_string(&item["audio_id"]);
        let name = item["songname"].as_str().unwrap_or_default().to_string();
        let singer = item["authors"]
            .as_array()
            .map(|authors| {
                authors
                    .iter()
                    .filter_map(|author| author["author_name"].as_str())
                    .collect::<Vec<_>>()
                    .join("、")
            })
            .unwrap_or_default();
        let mut song = SongInfo::new(id, SourceId::Kg, name, singer);
        song.album_name = item["remark"].as_str().unwrap_or_default().to_string();
        song.album_id = value_string(&item["album_id"]);
        song.duration = Duration::from_secs(item["duration"].as_u64().unwrap_or_default());

        let mut qualities = BTreeSet::new();
        let mut extra = HashMap::new();
        add_quality(
            item,
            "filesize",
            "hash",
            "FileHash",
            Quality::Low128,
            &mut qualities,
            &mut extra,
        );
        add_quality(
            item,
            "320filesize",
            "320hash",
            "HQFileHash",
            Quality::High320,
            &mut qualities,
            &mut extra,
        );
        add_quality(
            item,
            "sqfilesize",
            "sqhash",
            "SQFileHash",
            Quality::Flac,
            &mut qualities,
            &mut extra,
        );
        add_quality(
            item,
            "filesize_high",
            "hash_high",
            "ResFileHash",
            Quality::Flac24,
            &mut qualities,
            &mut extra,
        );
        song.qualities = qualities;
        song.extra = extra;
        items.push(song);
    }

    Ok(SearchResult {
        has_more: page.saturating_mul(limit) < total,
        total,
        items,
    })
}

#[allow(clippy::too_many_arguments)]
fn add_quality(
    item: &Value,
    size_key: &str,
    hash_key: &str,
    extra_key: &str,
    quality: Quality,
    qualities: &mut BTreeSet<Quality>,
    extra: &mut HashMap<String, String>,
) {
    if item[size_key].as_u64().unwrap_or_default() > 0 {
        qualities.insert(quality);
    }
    if let Some(hash) = item[hash_key].as_str().filter(|hash| !hash.is_empty()) {
        extra.insert(extra_key.to_string(), hash.to_string());
    }
}

fn value_string(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_u64().map(|value| value.to_string()))
        .unwrap_or_default()
}
