use lx_core::model::playlist::Playlist;
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::FetchError;
use serde_json::Value;

use crate::http;

pub async fn get_list(page: u32) -> Result<Vec<Playlist>, FetchError> {
    let url = format!(
        "http://wapi.kuwo.cn/api/pc/classify/playlist/getRcmPlayList?loginUid=0&loginSid=0&appUid=76039576&pn={page}&rn=36&order=hot"
    );
    let json: Value = http::client()
        .get(url)
        .send()
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))?;
    if json["code"].as_i64() != Some(200) {
        return Err(FetchError::Other("酷我热门歌单请求失败".to_string()));
    }
    let items = json["data"]["data"]
        .as_array()
        .ok_or_else(|| FetchError::Parse("酷我热门歌单列表为空".to_string()))?;
    Ok(items.iter().filter_map(parse_playlist).collect())
}

pub async fn get_detail(raw_id: &str, page: u32) -> Result<Vec<SongInfo>, FetchError> {
    let (digest, id) = parse_id(raw_id);
    let id = if digest == Some("5") {
        resolve_digest_five_id(id).await?
    } else {
        id.to_string()
    };
    let url = format!(
        "http://nplserver.kuwo.cn/pl.svc?op=getlistinfo&pid={id}&pn={}&rn=1000&encode=utf8&keyset=pl2012&identity=kuwo&pcmp4=1&vipver=MUSIC_9.0.5.0_W1&newver=1",
        page.saturating_sub(1)
    );
    let json: Value = http::client()
        .get(url)
        .send()
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))?;
    if json["result"].as_str() != Some("ok") {
        return Err(FetchError::Other("酷我歌单详情请求失败".to_string()));
    }
    let items = json["musiclist"]
        .as_array()
        .ok_or_else(|| FetchError::Parse("酷我歌单歌曲列表为空".to_string()))?;
    Ok(items
        .iter()
        .filter_map(super::leaderboard::parse_song)
        .collect())
}

fn parse_playlist(item: &Value) -> Option<Playlist> {
    let id = value_string(&item["id"]);
    let name = item["name"].as_str()?.trim().to_string();
    if id.is_empty() || name.is_empty() {
        return None;
    }
    let digest = value_string(&item["digest"]);
    Some(Playlist {
        id: if digest.is_empty() {
            id
        } else {
            format!("digest-{digest}__{id}")
        },
        name,
        source: SourceId::Kw,
        cover_url: non_empty_string(&item["img"]),
        song_count: value_u64(&item["total"]).unwrap_or_default() as u32,
        description: non_empty_string(&item["desc"]),
        play_count: value_u64(&item["listencnt"]),
    })
}

fn parse_id(raw_id: &str) -> (Option<&str>, &str) {
    let Some((prefix, id)) = raw_id.split_once("__") else {
        return (None, raw_id);
    };
    (prefix.strip_prefix("digest-"), id)
}

async fn resolve_digest_five_id(id: &str) -> Result<String, FetchError> {
    let url = format!(
        "http://qukudata.kuwo.cn/q.k?op=query&cont=ninfo&node={id}&pn=0&rn=1&fmt=json&src=mbox&level=2"
    );
    let json: Value = http::client()
        .get(url)
        .send()
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))?;
    let resolved = json["child"]
        .as_array()
        .and_then(|items| items.first())
        .map(|item| value_string(&item["sourceid"]))
        .filter(|value| !value.is_empty())
        .ok_or(FetchError::NotFound)?;
    Ok(resolved)
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
