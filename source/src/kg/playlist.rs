use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use lx_core::model::playlist::Playlist;
use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::FetchError;
use regex::Regex;
use serde_json::Value;

use crate::http;

pub async fn get_list(page: u32) -> Result<Vec<Playlist>, FetchError> {
    let url = format!(
        "http://www2.kugou.kugou.com/yueku/v9/special/getSpecial?is_ajax=1&cdn=cdn&t=6&c=&p={page}&pagesize=36"
    );
    let json: Value = http::client()
        .get(url)
        .send()
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))?;
    if json["status"].as_i64() != Some(1) {
        return Err(FetchError::Other("酷狗热门歌单请求失败".to_string()));
    }
    let items = json["special_db"]
        .as_array()
        .ok_or_else(|| FetchError::Parse("酷狗热门歌单列表为空".to_string()))?;
    Ok(items.iter().filter_map(parse_playlist).collect())
}

pub async fn get_detail(raw_id: &str) -> Result<Vec<SongInfo>, FetchError> {
    let id = raw_id.strip_prefix("id_").unwrap_or(raw_id);
    let url = format!("http://www2.kugou.kugou.com/yueku/v9/special/single/{id}-5-9999.html");
    let html = http::client()
        .get(url)
        .send()
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .text()
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?;
    let values = embedded_song_data(&html)?;
    Ok(values.iter().filter_map(parse_song).collect())
}

fn parse_playlist(item: &Value) -> Option<Playlist> {
    let id = value_string(&item["specialid"]);
    let name = item["specialname"].as_str()?.trim().to_string();
    if id.is_empty() || name.is_empty() {
        return None;
    }
    Some(Playlist {
        id,
        name,
        source: SourceId::Kg,
        cover_url: first_string(item, &["img", "imgurl"]).map(|url| url.replace("{size}", "500")),
        song_count: value_u64(&item["song_count"])
            .or_else(|| value_u64(&item["songcount"]))
            .unwrap_or_default() as u32,
        description: first_string(item, &["intro"]),
        play_count: value_u64(&item["total_play_count"]).or_else(|| value_u64(&item["play_count"])),
    })
}

fn embedded_song_data(html: &str) -> Result<Vec<Value>, FetchError> {
    let regex = Regex::new(r"(?s)(?:global\.data|var\s+data)\s*=\s*(\[.*?\]);")
        .expect("valid Kugou playlist regex");
    let raw = regex
        .captures(html)
        .and_then(|captures| captures.get(1))
        .map(|capture| capture.as_str())
        .ok_or_else(|| FetchError::Parse("酷狗歌单页面中没有歌曲数据".to_string()))?;
    serde_json::from_str(raw).map_err(|error| FetchError::Parse(error.to_string()))
}

fn parse_song(item: &Value) -> Option<SongInfo> {
    let id = value_string(&item["audio_id"]);
    let id = if id.is_empty() {
        value_string(&item["songid"])
    } else {
        id
    };
    if id.is_empty() {
        return None;
    }
    let name = first_string(item, &["songname", "audio_name"]).unwrap_or_default();
    let singer = first_string(item, &["singername", "author_name"]).unwrap_or_default();
    let mut song = SongInfo::new(id, SourceId::Kg, name, singer);
    song.album_name = first_string(item, &["album_name"]).unwrap_or_default();
    song.album_id = value_string(&item["album_id"]);
    song.duration = Duration::from_millis(
        value_u64(&item["duration"])
            .or_else(|| value_u64(&item["timelength"]))
            .unwrap_or_default(),
    );
    song.cover_url = item
        .pointer("/trans_param/union_cover")
        .and_then(Value::as_str)
        .filter(|url| !url.is_empty())
        .map(|url| url.replace("{size}", "500"));

    let mut qualities = BTreeSet::new();
    let mut extra = HashMap::new();
    for (size_key, hash_key, extra_key, quality) in [
        ("filesize", "hash", "FileHash", Quality::Low128),
        ("filesize_320", "hash_320", "HQFileHash", Quality::High320),
        ("filesize_flac", "hash_flac", "SQFileHash", Quality::Flac),
        ("filesize_high", "hash_high", "ResFileHash", Quality::Flac24),
    ] {
        if value_u64(&item[size_key]).unwrap_or_default() > 0 {
            qualities.insert(quality);
        }
        if let Some(hash) = item[hash_key].as_str().filter(|hash| !hash.is_empty()) {
            extra.insert(extra_key.to_string(), hash.to_string());
        }
    }
    song.qualities = qualities;
    song.extra = extra;
    Some(song)
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| {
            value[*key]
                .as_str()
                .filter(|value| !value.trim().is_empty())
        })
        .map(str::to_string)
}

fn value_string(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_i64().map(|value| value.to_string()))
        .or_else(|| value.as_u64().map(|value| value.to_string()))
        .unwrap_or_default()
}

fn value_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::embedded_song_data;

    #[test]
    fn parses_embedded_song_json_without_executing_script() {
        let html = r#"<script>global.data = [{"audio_id":1,"songname":"Song"}];</script>"#;
        let songs = embedded_song_data(html).unwrap();
        assert_eq!(songs.len(), 1);
        assert_eq!(songs[0]["songname"], "Song");
    }
}
