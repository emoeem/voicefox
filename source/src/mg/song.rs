use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use serde_json::Value;

fn value_string(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_i64().map(|value| value.to_string()))
        .or_else(|| value.as_u64().map(|value| value.to_string()))
        .unwrap_or_default()
}

fn format_to_quality(format_type: &str) -> Option<Quality> {
    match format_type {
        "PQ" => Some(Quality::Low128),
        "HQ" => Some(Quality::High320),
        "SQ" => Some(Quality::Flac),
        "ZQ" | "ZQ24" | "ZQ32" => Some(Quality::Flac24),
        _ => None,
    }
}

fn duration_seconds(value: &Value) -> u64 {
    if let Some(seconds) = value
        .as_u64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
    {
        return seconds;
    }
    let Some(value) = value.as_str() else {
        return 0;
    };
    let mut parts = value
        .split(':')
        .rev()
        .filter_map(|part| part.parse::<u64>().ok());
    let seconds = parts.next().unwrap_or(0);
    let minutes = parts.next().unwrap_or(0);
    let hours = parts.next().unwrap_or(0);
    hours.saturating_mul(3600) + minutes.saturating_mul(60) + seconds
}

fn first_string(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| value[*key].as_str().filter(|value| !value.is_empty()))
        .unwrap_or_default()
        .to_string()
}

fn singer_name(value: &Value) -> String {
    for key in ["singerList", "artists"] {
        if let Some(items) = value[key].as_array() {
            let names = items
                .iter()
                .filter_map(|item| item["name"].as_str())
                .filter(|name| !name.is_empty())
                .collect::<Vec<_>>();
            if !names.is_empty() {
                return names.join("、");
            }
        }
    }
    first_string(value, &["singer", "author", "txt2"])
}

fn cover_url(value: &Value) -> Option<String> {
    let direct = first_string(value, &["img3", "img2", "img1"]);
    if !direct.is_empty() {
        return Some(direct);
    }
    value["albumImgs"]
        .as_array()
        .and_then(|items| items.first())
        .and_then(|item| item["img"].as_str().or_else(|| item["webpImg"].as_str()))
        .filter(|url| !url.is_empty())
        .map(str::to_string)
}

pub(crate) fn parse_song(value: &Value) -> Option<SongInfo> {
    let song_id = value_string(&value["songId"]);
    if song_id.is_empty() {
        return None;
    }

    let name = first_string(value, &["songName", "name", "txt"]);
    let album_name = first_string(value, &["album", "albumName", "txt3"]);
    let album_id = value_string(&value["albumId"]);
    let copyright_id = value_string(&value["copyrightId"]);

    let formats = value["audioFormats"]
        .as_array()
        .or_else(|| value["newRateFormats"].as_array())
        .or_else(|| value["rateFormats"].as_array());
    let mut qualities = BTreeSet::new();
    if let Some(formats) = formats {
        for format in formats {
            if let Some(quality) =
                format_to_quality(format["formatType"].as_str().unwrap_or_default())
            {
                qualities.insert(quality);
            }
        }
    }

    let mut extra = HashMap::new();
    if !copyright_id.is_empty() {
        extra.insert("copyrightId".to_string(), copyright_id);
    }
    for (key, extra_key) in [
        ("lrcUrl", "lrcUrl"),
        ("mrcurl", "mrcUrl"),
        ("mrcUrl", "mrcUrl"),
        ("trcUrl", "trcUrl"),
    ] {
        if let Some(url) = value[key].as_str().filter(|url| !url.is_empty()) {
            extra.insert(extra_key.to_string(), url.to_string());
        }
    }

    let mut song = SongInfo::new(song_id, SourceId::Mg, name, singer_name(value));
    song.album_name = album_name;
    song.album_id = album_id;
    song.duration = Duration::from_secs(
        duration_seconds(&value["duration"]).max(duration_seconds(&value["length"])),
    );
    song.cover_url = cover_url(value);
    song.qualities = qualities;
    song.extra = extra;
    Some(song)
}
