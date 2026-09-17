use std::collections::HashSet;

use lx_core::model::playlist::{Playlist, PlaylistCategory};
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::{FetchError, SearchError};
use regex::Regex;
use serde_json::Value;

use crate::http;
use crate::http::SendWithRetry;

const PLAYLIST_PAGE_SIZE: usize = 36;
const MAX_PLAYLIST_REQUESTS: u32 = 4;
const DETAIL_REQUEST_SIZE: u32 = 1000;
const MAX_DETAIL_PAGES: u32 = 100;

pub async fn get_list(page: u32) -> Result<Vec<Playlist>, FetchError> {
    let mut playlists = Vec::with_capacity(PLAYLIST_PAGE_SIZE);
    let mut seen = HashSet::new();

    // Kuwo sometimes ignores rn=36 and returns only ten entries. Pull the next
    // server pages until the TUI has a complete logical page.
    for offset in 0..MAX_PLAYLIST_REQUESTS {
        let items = fetch_list_page(page.saturating_add(offset)).await?;
        let item_count = items.len();
        for playlist in items.iter().filter_map(parse_playlist) {
            if seen.insert(playlist.id.clone()) {
                playlists.push(playlist);
            }
            if playlists.len() >= PLAYLIST_PAGE_SIZE {
                return Ok(playlists);
            }
        }
        if item_count == 0 || item_count >= PLAYLIST_PAGE_SIZE {
            break;
        }
    }

    Ok(playlists)
}

async fn fetch_list_page(page: u32) -> Result<Vec<Value>, FetchError> {
    let url = format!(
        "http://wapi.kuwo.cn/api/pc/classify/playlist/getRcmPlayList?loginUid=0&loginSid=0&appUid=76039576&pn={page}&rn=36&order=hot"
    );
    let json: Value = http::client()
        .get(url)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
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
    Ok(items.clone())
}

pub async fn get_detail(raw_id: &str, page: u32) -> Result<Vec<SongInfo>, FetchError> {
    let (digest, id) = parse_id(raw_id);
    let id = if digest == Some("5") {
        resolve_digest_five_id(id).await?
    } else {
        id.to_string()
    };
    let first_page = page.saturating_sub(1);
    let json = fetch_detail_page(&id, first_page, DETAIL_REQUEST_SIZE).await?;
    let raw_items = json["musiclist"]
        .as_array()
        .ok_or_else(|| FetchError::Parse("酷我歌单歌曲列表为空".to_string()))?;
    let total = value_u64(&json["total"]).unwrap_or(raw_items.len() as u64) as usize;
    let mut songs = Vec::with_capacity(total.min(DETAIL_REQUEST_SIZE as usize));
    let mut seen = HashSet::new();
    append_unique_songs(&mut songs, &mut seen, raw_items);

    // Some Kuwo nodes cap a response at ten tracks even when rn=1000. Continue
    // with the effective page size returned by the first response.
    let effective_page_size = raw_items.len() as u32;
    if effective_page_size == 0 || songs.len() >= total {
        return Ok(songs);
    }
    let page_count = (total as u32)
        .div_ceil(effective_page_size)
        .min(MAX_DETAIL_PAGES);
    for offset in 1..page_count {
        let json =
            fetch_detail_page(&id, first_page.saturating_add(offset), effective_page_size).await?;
        let Some(items) = json["musiclist"].as_array() else {
            break;
        };
        if items.is_empty() {
            break;
        }
        append_unique_songs(&mut songs, &mut seen, items);
        if songs.len() >= total {
            break;
        }
    }
    Ok(songs)
}

async fn fetch_detail_page(id: &str, page: u32, page_size: u32) -> Result<Value, FetchError> {
    let url = format!(
        "http://nplserver.kuwo.cn/pl.svc?op=getlistinfo&pid={id}&pn={page}&rn={page_size}&encode=utf8&keyset=pl2012&identity=kuwo&pcmp4=1&vipver=MUSIC_9.0.5.0_W1&newver=1"
    );
    let json: Value = http::client()
        .get(url)
        .header("Referer", "http://www.kuwo.cn/")
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))?;
    if json["result"].as_str() != Some("ok") {
        return Err(FetchError::Other("酷我歌单详情请求失败".to_string()));
    }
    Ok(json)
}

/// 歌单详情 + 元数据（链接直解需要歌单名）。
pub async fn get_detail_with_meta(raw_id: &str) -> Result<(Playlist, Vec<SongInfo>), FetchError> {
    let (digest, id) = parse_id(raw_id);
    let id = if digest == Some("5") {
        resolve_digest_five_id(id).await?
    } else {
        id.to_string()
    };
    let json = fetch_detail_page(&id, 0, DETAIL_REQUEST_SIZE).await?;
    let songs = json["musiclist"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(super::leaderboard::parse_song)
        .collect::<Vec<_>>();
    if songs.is_empty() {
        return Err(FetchError::NotFound);
    }
    let name = json["title"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("酷我歌单 {id}"));
    let mut playlist = Playlist::new(id.clone(), name, SourceId::Kw);
    playlist.song_count = value_u64(&json["total"]).unwrap_or(songs.len() as u64) as u32;
    playlist.play_count = value_u64(&json["playnum"]);
    playlist.creator = non_empty_string(&json["uname"]);
    playlist.description = non_empty_string(&json["info"]);
    playlist.cover_url = non_empty_string(&json["pic"]);
    playlist.link = Some(format!("http://www.kuwo.cn/playlist_detail/{id}"));
    Ok((playlist, songs))
}

/// 歌单搜索：与歌曲搜索同一个 legacy 路由，`ft=playlist`。
pub async fn search_playlists(keyword: &str, page: u32) -> Result<Vec<Playlist>, SearchError> {
    let url = format!(
        "http://search.kuwo.cn/r.s?all={}&ft=playlist&itemset=web_2013&client=kt&pcmp4=1&geo=c&vipver=1&pn={}&rn=30&rformat=json&encoding=utf8",
        urlencoding::encode(keyword),
        page.saturating_sub(1)
    );
    let text = http::client()
        .get(url)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| SearchError::Network(error.to_string()))?
        .text()
        .await
        .map_err(|error| SearchError::Network(error.to_string()))?;
    let json = parse_kuwo_json(&text)?;
    Ok(json["abslist"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(parse_searched_playlist)
        .collect())
}

/// 酷我的 `search.kuwo.cn/r.s` 返回的是 **JS 对象字面量**（键不带引号、
/// 字符串用单引号），不是合法 JSON。这里做一次最小改写再交给 serde：
/// 给裸键加双引号、单引号转双引号，并去掉尾部的分号。
fn parse_kuwo_json(text: &str) -> Result<Value, SearchError> {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        return Ok(value);
    }
    let quoted_keys = Regex::new(r#"([{,]\s*)([A-Za-z_][A-Za-z0-9_]*)\s*:"#)
        .map_err(|error| SearchError::Parse(error.to_string()))?
        .replace_all(text, r#"$1"$2":"#);
    // 去掉键的引号后可能多出一个冒号，这里补齐成 `":"`。
    let quoted_keys = quoted_keys.replace("\"\"", "\"");
    let single = Regex::new(r"'([^']*)'")
        .map_err(|error| SearchError::Parse(error.to_string()))?
        .replace_all(&quoted_keys, r#""$1""#);
    let trimmed = single.trim().trim_end_matches(';');
    serde_json::from_str(trimmed).map_err(|error| SearchError::Parse(error.to_string()))
}

/// 搜索结果字段在不同版本里大小写不一致，两种写法都取一次。
fn field<'a>(item: &'a Value, name: &str) -> Option<&'a Value> {
    item.get(name)
        .or_else(|| item.get(name.to_ascii_uppercase()))
        .or_else(|| item.get(name.to_ascii_lowercase()))
}

fn parse_searched_playlist(item: &Value) -> Option<Playlist> {
    let id = field(item, "playlistid")
        .map(value_string)
        .unwrap_or_default();
    let name = field(item, "name")
        .and_then(Value::as_str)
        .map(str::trim)?
        .to_string();
    if id.is_empty() || name.is_empty() {
        return None;
    }
    let mut playlist = Playlist::new(id.clone(), name, SourceId::Kw);
    playlist.cover_url = field(item, "pic")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|value| value.replace("_150.", "_700."));
    playlist.song_count = field(item, "songnum")
        .and_then(value_u64)
        .unwrap_or_default() as u32;
    playlist.creator = field(item, "nickname")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    playlist.description = field(item, "intro")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    playlist.link = Some(format!("http://www.kuwo.cn/playlist_detail/{id}"));
    Some(playlist)
}

/// 歌单分类目录：`/api/pc/classify/playlist/getTagList`。
pub async fn get_categories() -> Result<Vec<PlaylistCategory>, FetchError> {
    let json: Value = http::client()
        .get("http://wapi.kuwo.cn/api/pc/classify/playlist/getTagList?cmd=rcm_keyword_playlist&user=0&prod=kwplayer_pc_9.1.1.2&vipver=9.1.1.2&source=kwplayer_pc_9.1.1.2&loginUid=0&loginSid=0&appUid=38668888")
        .header("Referer", "http://www.kuwo.cn/")
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))?;
    if json["code"].as_i64() != Some(200) {
        return Err(FetchError::Other("酷我歌单分类请求失败".to_string()));
    }
    let mut categories = vec![PlaylistCategory {
        id: String::new(),
        name: "全部".to_string(),
        source: SourceId::Kw,
        group: Some("全部".to_string()),
        count: 0,
        hot: true,
        extra: Default::default(),
    }];
    for group in json["data"].as_array().into_iter().flatten() {
        let group_name = group["name"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string();
        for item in group["data"].as_array().into_iter().flatten() {
            let id = value_string(&item["id"]);
            let name = item["name"].as_str().unwrap_or_default().trim().to_string();
            // `digest` 非 10000 的是旧分类，接口保留但不再有内容。
            let digest = value_string(&item["digest"]);
            if id.is_empty() || name.is_empty() || (!digest.is_empty() && digest != "10000") {
                continue;
            }
            let mut category = PlaylistCategory::new(id, name, SourceId::Kw);
            category.group = (!group_name.is_empty()).then(|| group_name.clone());
            category.hot = item["extend"]
                .as_str()
                .is_some_and(|value| value.to_ascii_uppercase().contains("HOT"));
            categories.push(category);
        }
    }
    Ok(categories)
}

/// 分类歌单：`getTagPlayList`；分类为空时退回热门推荐。
pub async fn get_category_list(category: &str, page: u32) -> Result<Vec<Playlist>, FetchError> {
    let category = category.trim();
    let url = if category.is_empty() {
        format!(
            "http://wapi.kuwo.cn/api/pc/classify/playlist/getRcmPlayList?loginUid=0&loginSid=0&appUid=38668888&pn={}&rn=36&order=hot",
            page.max(1)
        )
    } else {
        format!(
            "http://wapi.kuwo.cn/api/pc/classify/playlist/getTagPlayList?loginUid=0&loginSid=0&appUid=38668888&pn={}&rn=36&id={}",
            page.max(1),
            urlencoding::encode(category)
        )
    };
    let json: Value = http::client()
        .get(url)
        .header("Referer", "http://www.kuwo.cn/")
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))?;
    if json["code"].as_i64() != Some(200) {
        return Err(FetchError::Other("酷我分类歌单请求失败".to_string()));
    }
    Ok(json["data"]["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let mut playlist = parse_playlist(item)?;
            playlist
                .extra
                .insert("category_id".to_string(), category.to_string());
            Some(playlist)
        })
        .collect())
}

fn append_unique_songs(songs: &mut Vec<SongInfo>, seen: &mut HashSet<String>, items: &[Value]) {
    for song in items.iter().filter_map(super::leaderboard::parse_song) {
        if seen.insert(song.id.clone()) {
            songs.push(song);
        }
    }
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
        creator: None,
        link: None,
        extra: Default::default(),
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
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
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

#[cfg(test)]
mod tests {
    use super::parse_kuwo_json;

    #[test]
    fn parses_the_legacy_js_object_literal() {
        // 酷我返回的是 JS 对象字面量：键无引号、字符串用单引号、末尾带分号。
        let raw = r#"{total:'10',abslist:[{playlistid:'123',name:'华语',songnum:'5',intro:'介绍',nickname:'作者'}]};"#;
        let json = parse_kuwo_json(raw).unwrap();
        assert_eq!(json["total"].as_str(), Some("10"));
        let first = &json["abslist"][0];
        assert_eq!(first["playlistid"].as_str(), Some("123"));
        assert_eq!(first["name"].as_str(), Some("华语"));
        // 合法 JSON 原样通过。
        assert!(parse_kuwo_json(r#"{"a":1}"#).is_ok());
    }
}
