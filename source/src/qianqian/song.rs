//! 千千音乐搜索与歌曲解析。

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{FetchError, SearchError, SearchResult};
use serde_json::Value;

use crate::http;
use crate::http::SendWithRetry;

use super::crypto::{APP_ID, REFERER, USER_AGENT, signed_query};

/// `/v1/search` 的 type 值。
pub(super) const TYPE_SONG: &str = "1";
pub(super) const TYPE_ALBUM: &str = "3";
pub(super) const TYPE_PLAYLIST: &str = "6";

/// 模块内部统一的错误类型，便于在 `FetchError` 与 `SearchError` 之间转换。
#[derive(Debug)]
pub(super) enum FetchErrorKind {
    Network(String),
    Parse(String),
}

impl FetchErrorKind {
    pub(super) fn into_fetch(self) -> FetchError {
        match self {
            FetchErrorKind::Network(message) => FetchError::Network(message),
            FetchErrorKind::Parse(message) => FetchError::Parse(message),
        }
    }

    pub(super) fn into_search(self) -> SearchError {
        match self {
            FetchErrorKind::Network(message) => SearchError::Network(message),
            FetchErrorKind::Parse(message) => SearchError::Parse(message),
        }
    }
}

/// 带签名的 GET。
///
/// 千千的接口**无视 `Accept-Encoding`** 直接返回 gzip（实测声明 identity 也会
/// 被压缩），所以这里手动判断魔数解压，而不是给 reqwest 打开 gzip 特性——
/// 那是全局行为，只有这一个平台需要。
pub(super) async fn signed_get(url: &str) -> Result<Value, FetchErrorKind> {
    let bytes = http::client()
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Referer", REFERER)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchErrorKind::Network(error.to_string()))?
        .bytes()
        .await
        .map_err(|error| FetchErrorKind::Network(error.to_string()))?;
    let decoded = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut decoder = flate2::read::GzDecoder::new(bytes.as_ref());
        let mut text = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut text)
            .map_err(|error| FetchErrorKind::Parse(error.to_string()))?;
        text
    } else {
        bytes.to_vec()
    };
    serde_json::from_slice(&decoded).map_err(|error| FetchErrorKind::Parse(error.to_string()))
}

/// 调用 `/v1/search`，返回整个响应体。
pub(super) async fn search(
    keyword: &str,
    search_type: &str,
    page: u32,
    limit: u32,
) -> Result<Value, FetchErrorKind> {
    let page = page.max(1).to_string();
    let limit = limit.max(1).to_string();
    let query = signed_query(&[
        ("word", keyword),
        ("type", search_type),
        ("pageNo", page.as_str()),
        ("pageSize", limit.as_str()),
        ("appid", APP_ID),
    ]);
    signed_get(&format!("https://music.91q.com/v1/search?{query}")).await
}

pub async fn search_songs(
    keyword: &str,
    page: u32,
    limit: u32,
) -> Result<SearchResult, SearchError> {
    let json = search(keyword, TYPE_SONG, page, limit)
        .await
        .map_err(FetchErrorKind::into_search)?;
    let items = json["data"]["typeTrack"]
        .as_array()
        .map(|items| items.iter().filter_map(parse_song).collect::<Vec<_>>())
        .unwrap_or_default();
    Ok(SearchResult {
        total: items.len() as u32,
        has_more: items.len() >= limit as usize,
        items,
    })
}

/// 兼容大小写两种字段名：接口在不同版本里返回过 `TSID` 与 `tsid`。
pub(super) fn field<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    value
        .get(name)
        .or_else(|| value.get(name.to_ascii_uppercase()))
        .or_else(|| value.get(name.to_ascii_lowercase()))
}

pub(super) fn value_string(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_i64().map(|value| value.to_string()))
        .or_else(|| value.as_u64().map(|value| value.to_string()))
        .unwrap_or_default()
}

pub(super) fn value_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

/// 歌手数组拼成「A、B」。
pub(super) fn join_artists(value: &Value) -> String {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|artist| artist["name"].as_str())
        .filter(|name| !name.trim().is_empty())
        .collect::<Vec<_>>()
        .join("、")
}

/// 音质档位：`rateFileInfo` 里有对应码率且体积大于 0 才算支持。
fn qualities_from_rates(value: &Value) -> BTreeSet<Quality> {
    let mut qualities = BTreeSet::new();
    let Some(rates) = value.as_object() else {
        return qualities;
    };
    for (rate, info) in rates {
        if value_u64(&info["size"]).unwrap_or_default() == 0 {
            continue;
        }
        match rate.as_str() {
            "3000" => {
                qualities.insert(Quality::Flac);
            }
            "320" => {
                qualities.insert(Quality::High320);
            }
            "128" | "64" => {
                qualities.insert(Quality::Low128);
            }
            _ => {}
        }
    }
    qualities
}

pub(super) fn parse_song(resource: &Value) -> Option<SongInfo> {
    let tsid = field(resource, "tsid").map(value_string)?;
    if tsid.is_empty() {
        return None;
    }
    let name = resource["title"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    if name.is_empty() {
        return None;
    }
    let singer = join_artists(&resource["artist"]);
    let mut song = SongInfo::new(tsid.clone(), SourceId::Qianqian, name, singer);
    song.album_name = resource["albumTitle"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    song.album_id = resource["albumAssetCode"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    song.duration = Duration::from_secs(value_u64(&resource["duration"]).unwrap_or_default());
    song.cover_url = resource["pic"]
        .as_str()
        .filter(|url| !url.is_empty())
        .map(str::to_string);
    song.qualities = qualities_from_rates(&resource["rateFileInfo"]);
    let mut extra = HashMap::new();
    extra.insert("tsid".to_string(), tsid);
    if let Some(album_code) = resource["albumAssetCode"]
        .as_str()
        .filter(|v| !v.is_empty())
    {
        extra.insert("album_asset_code".to_string(), album_code.to_string());
    }
    song.extra = extra;
    Some(song)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_track_payload_and_rates() {
        let item = serde_json::json!({
            "TSID": "T100",
            "title": "晴天",
            "albumTitle": "叶惠美",
            "albumAssetCode": "A1",
            "pic": "https://cdn/cover.jpg",
            "duration": 269,
            "artist": [{ "name": "周杰伦" }],
            "rateFileInfo": {
                "320": { "size": 10752000 },
                "128": { "size": 4300800 }
            }
        });
        let song = parse_song(&item).unwrap();
        assert_eq!(song.id, "T100");
        assert_eq!(song.singer, "周杰伦");
        assert_eq!(song.album_name, "叶惠美");
        assert_eq!(song.duration.as_secs(), 269);
        assert_eq!(song.extra.get("tsid").map(String::as_str), Some("T100"));
        assert!(song.qualities.contains(&Quality::High320));
        assert!(song.qualities.contains(&Quality::Low128));
        // 没有 3000 档时不应声称支持无损。
        assert!(!song.qualities.contains(&Quality::Flac));
    }

    #[test]
    fn rejects_tracks_without_an_id_or_title() {
        assert!(parse_song(&serde_json::json!({ "title": "无 ID" })).is_none());
        assert!(parse_song(&serde_json::json!({ "TSID": "T1" })).is_none());
    }
}
