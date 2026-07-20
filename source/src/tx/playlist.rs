use lx_core::model::playlist::Playlist;
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::FetchError;
use serde_json::Value;

use crate::http;

pub async fn get_list(page: u32) -> Result<Vec<Playlist>, FetchError> {
    let body = serde_json::json!({
        "comm": { "cv": 1602, "ct": 20 },
        "playlist": {
            "method": "get_playlist_by_tag",
            "param": {
                "id": 10000000,
                "sin": 36 * page.saturating_sub(1),
                "size": 36,
                "order": 5,
                "cur_page": page
            },
            "module": "playlist.PlayListPlazaServer"
        }
    });
    let json: Value = http::client()
        .post("https://u.y.qq.com/cgi-bin/musicu.fcg")
        .header("Referer", "https://y.qq.com/")
        .json(&body)
        .send()
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))?;
    if json["code"].as_i64() != Some(0) || json["playlist"]["code"].as_i64() != Some(0) {
        return Err(FetchError::Other("QQ 热门歌单请求失败".to_string()));
    }
    let items = json["playlist"]["data"]["v_playlist"]
        .as_array()
        .ok_or_else(|| FetchError::Parse("QQ 热门歌单列表为空".to_string()))?;
    Ok(items.iter().filter_map(parse_playlist).collect())
}

pub async fn get_detail(id: &str) -> Result<Vec<SongInfo>, FetchError> {
    let body = serde_json::json!({
        "comm": {
            "ct": 20,
            "cv": 1859,
            "uin": 0,
            "format": "json"
        },
        "req": {
            "module": "music.srfDissInfo.aiDissInfo",
            "method": "uniform_get_Dissinfo",
            "param": {
                "disstid": id.parse::<u64>().unwrap_or_default(),
                "enc_host_uin": "",
                "tag": 1,
                "userInfo": 1,
                "song_begin": 0,
                "song_num": 1000,
                "onlysonglist": 0
            }
        }
    });
    let json: Value = http::client()
        .post("https://u.y.qq.com/cgi-bin/musicu.fcg")
        .header("Referer", "https://y.qq.com/")
        .json(&body)
        .send()
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))?;
    if json["code"].as_i64() != Some(0)
        || json["req"]["code"].as_i64() != Some(0)
        || json["req"]["data"]["code"].as_i64() != Some(0)
    {
        return Err(FetchError::Other("QQ 歌单详情请求失败".to_string()));
    }
    let items = json["req"]["data"]["songlist"]
        .as_array()
        .ok_or_else(|| FetchError::Parse("QQ 歌单歌曲列表为空".to_string()))?;
    Ok(items.iter().filter_map(super::search::parse_song).collect())
}

fn parse_playlist(item: &Value) -> Option<Playlist> {
    let id = value_string(&item["tid"]);
    let name = item["title"].as_str()?.trim().to_string();
    if id.is_empty() || name.is_empty() {
        return None;
    }
    Some(Playlist {
        id,
        name,
        source: SourceId::Tx,
        cover_url: non_empty_string(&item["cover_url_medium"]),
        song_count: item["song_ids"]
            .as_array()
            .map(|items| items.len() as u32)
            .unwrap_or_default(),
        description: non_empty_string(&item["desc"]),
        play_count: value_u64(&item["access_num"]),
    })
}

fn non_empty_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn value_string(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_u64().map(|value| value.to_string()))
        .unwrap_or_default()
}

fn value_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}
