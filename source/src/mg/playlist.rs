use std::collections::HashSet;

use lx_core::model::playlist::{Playlist, PlaylistCategory};
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::{FetchError, SearchError};
use serde_json::Value;

use crate::http;
use crate::http::SendWithRetry;

pub(super) const USER_AGENT: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 13_2_3 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/13.0.3 Mobile/15E148 Safari/604.1";

pub async fn get_list(page: u32) -> Result<Vec<Playlist>, FetchError> {
    let url = format!(
        "https://app.c.nf.migu.cn/pc/bmw/page-data/playlist-square-recommend/v1.0?templateVersion=2&pageNo={page}"
    );
    let json: Value = request(url).await?;
    if json["code"].as_str() != Some("000000") {
        return Err(FetchError::Other("咪咕热门歌单请求失败".to_string()));
    }
    let mut playlists = Vec::new();
    let mut seen = HashSet::new();
    collect_playlists(&json["data"]["contents"], &mut seen, &mut playlists);
    Ok(playlists)
}

pub async fn get_detail(id: &str, page: u32) -> Result<Vec<SongInfo>, FetchError> {
    const PAGE_SIZE: u32 = 50;
    let mut current_page = page.max(1);
    let mut songs = Vec::new();
    loop {
        let url = format!(
            "https://app.c.nf.migu.cn/MIGUM3.0/resource/playlist/song/v2.0?pageNo={current_page}&pageSize={PAGE_SIZE}&playlistId={id}"
        );
        let json: Value = request(url).await?;
        if json["code"].as_str() != Some("000000") {
            return Err(FetchError::Other("咪咕歌单详情请求失败".to_string()));
        }
        let items = json["data"]["songList"]
            .as_array()
            .ok_or_else(|| FetchError::Parse("咪咕歌单歌曲列表为空".to_string()))?;
        songs.extend(items.iter().filter_map(super::song::parse_song));

        let total = value_u64(&json["data"]["totalCount"]).unwrap_or(items.len() as u64);
        if items.is_empty() || u64::from(current_page) * u64::from(PAGE_SIZE) >= total {
            break;
        }
        current_page = current_page.saturating_add(1);
    }
    Ok(songs)
}

/// 歌单详情 + 元数据（链接直解需要歌单名）。
pub async fn get_detail_with_meta(id: &str) -> Result<(Playlist, Vec<SongInfo>), FetchError> {
    let url = format!(
        "https://app.c.nf.migu.cn/MIGUM3.0/resource/playlist/song/v2.0?pageNo=1&pageSize=50&playlistId={id}"
    );
    let json = request(url).await?;
    if json["code"].as_str() != Some("000000") {
        return Err(FetchError::Other("咪咕歌单详情请求失败".to_string()));
    }
    let songs = json["data"]["songList"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(super::song::parse_song)
        .collect::<Vec<_>>();
    if songs.is_empty() {
        return Err(FetchError::NotFound);
    }
    let detail = &json["data"];
    let name = ["name", "title", "playlistName"]
        .iter()
        .find_map(|key| detail[*key].as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("咪咕歌单 {id}"));
    let mut playlist = Playlist::new(id, name, SourceId::Mg);
    playlist.song_count = value_u64(&detail["totalCount"]).unwrap_or(songs.len() as u64) as u32;
    playlist.cover_url = ["imgUrl", "picUrl", "cover"]
        .iter()
        .find_map(|key| detail[*key].as_str())
        .filter(|url| !url.is_empty())
        .map(str::to_string);
    playlist.creator = ["userName", "ownerName", "nickName"]
        .iter()
        .find_map(|key| detail[*key].as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    playlist.link = Some(format!("https://music.migu.cn/v3/music/playlist/{id}"));
    Ok((playlist, songs))
}

/// 歌手/歌单/专辑搜索共用的入口：用 `searchSwitch` 指定要搜哪一类。
async fn search_all(
    keyword: &str,
    page: u32,
    limit: u32,
    songlist: bool,
    album: bool,
) -> Result<Value, FetchError> {
    let switch = if album {
        r#"{"song":0,"album":1,"singer":0,"tagSong":0,"mvSong":0,"songlist":0,"bestShow":1}"#
    } else if songlist {
        r#"{"song":0,"album":0,"singer":0,"tagSong":0,"mvSong":0,"songlist":1,"bestShow":1}"#
    } else {
        r#"{"song":1,"album":0,"singer":0,"tagSong":0,"mvSong":0,"songlist":0,"bestShow":1}"#
    };
    let url = format!(
        "http://pd.musicapp.migu.cn/MIGUM2.0/v1.0/content/search_all.do?ua=Android_migu&version=5.0.1&text={}&pageNo={}&pageSize={}&searchSwitch={}",
        urlencoding::encode(keyword),
        page.max(1),
        limit,
        urlencoding::encode(switch)
    );
    request(url).await
}

/// 歌单搜索。
pub async fn search_playlists(keyword: &str, page: u32) -> Result<Vec<Playlist>, SearchError> {
    let json = search_all(keyword, page, 30, true, false)
        .await
        .map_err(|error| SearchError::Other(error.to_string()))?;
    Ok(json["songListResultData"]["result"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(parse_searched_playlist)
        .collect())
}

fn parse_searched_playlist(item: &Value) -> Option<Playlist> {
    let id = value_string(&item["id"]);
    let name = item["name"].as_str()?.trim().to_string();
    if id.is_empty() || name.is_empty() {
        return None;
    }
    let mut playlist = Playlist::new(id.clone(), name, SourceId::Mg);
    playlist.cover_url = ["musicListPicUrl"]
        .iter()
        .find_map(|key| item[*key].as_str())
        .filter(|url| !url.is_empty())
        .map(str::to_string)
        .or_else(|| pick_image(&item["imgItems"]));
    playlist.song_count = value_u64(&item["musicNum"]).unwrap_or_default() as u32;
    playlist.play_count = value_u64(&item["playNum"]);
    playlist.creator = ["userName", "ownerName"]
        .iter()
        .find_map(|key| item[*key].as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    playlist.link = Some(format!("https://music.migu.cn/v3/music/playlist/{id}"));
    Some(playlist)
}

/// 图片列表里挑一张：优先 03（列表用图），否则取第一张。
pub(super) fn pick_image(items: &Value) -> Option<String> {
    let list = items.as_array()?;
    list.iter()
        .find(|item| item["imgSizeType"].as_str() == Some("03"))
        .or_else(|| list.first())
        .and_then(|item| item["img"].as_str())
        .filter(|url| !url.is_empty())
        .map(str::to_string)
}

/// 歌单分类：咪咕没有分类目录接口，官方搜索按关键词筛歌单，
/// 这里沿用 music-lib 的关键词表，分组用于界面归类。
const CATEGORY_KEYWORDS: &[(&str, &str, bool)] = &[
    ("华语", "语种", true),
    ("欧美", "语种", true),
    ("日语", "语种", false),
    ("韩语", "语种", false),
    ("粤语", "语种", false),
    ("流行", "风格", true),
    ("摇滚", "风格", true),
    ("民谣", "风格", true),
    ("电子", "风格", false),
    ("古典", "风格", false),
    ("爵士", "风格", false),
    ("乡村", "风格", false),
    ("轻音乐", "风格", false),
    ("影视原声", "场景", false),
    ("二次元", "场景", false),
    ("运动", "场景", false),
    ("睡前", "场景", false),
    ("驾车", "场景", false),
    ("怀旧", "场景", false),
    ("KTV", "场景", false),
];

pub fn get_categories() -> Vec<PlaylistCategory> {
    let mut categories = vec![PlaylistCategory {
        id: String::new(),
        name: "全部".to_string(),
        source: SourceId::Mg,
        group: Some("全部".to_string()),
        count: 0,
        hot: true,
        extra: Default::default(),
    }];
    for (name, group, hot) in CATEGORY_KEYWORDS {
        let mut category = PlaylistCategory::new(*name, *name, SourceId::Mg);
        category.group = Some((*group).to_string());
        category.hot = *hot;
        categories.push(category);
    }
    categories
}

/// 分类歌单：按分类关键词搜歌单；空分类按「华语」处理，与 music-lib 一致。
pub async fn get_category_list(category: &str, page: u32) -> Result<Vec<Playlist>, FetchError> {
    let keyword = if category.trim().is_empty() {
        "华语"
    } else {
        category.trim()
    };
    let json = search_all(keyword, page, 30, true, false).await?;
    let mut playlists = json["songListResultData"]["result"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(parse_searched_playlist)
        .collect::<Vec<_>>();
    for playlist in &mut playlists {
        playlist
            .extra
            .insert("category_id".to_string(), category.to_string());
    }
    Ok(playlists)
}

async fn request(url: String) -> Result<Value, FetchError> {
    http::client()
        .get(url)
        .header("Referer", "https://m.music.migu.cn/")
        .header("channel", "0146921")
        .header("User-Agent", USER_AGENT)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))
}

fn collect_playlists(value: &Value, seen: &mut HashSet<String>, playlists: &mut Vec<Playlist>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_playlists(item, seen, playlists);
            }
        }
        Value::Object(map) => {
            if map.get("resType").and_then(Value::as_str) == Some("2021") {
                let id = map.get("resId").map(value_string).unwrap_or_default();
                let name = map
                    .get("txt")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if !id.is_empty() && !name.is_empty() && seen.insert(id.clone()) {
                    playlists.push(Playlist {
                        id,
                        name,
                        source: SourceId::Mg,
                        cover_url: map
                            .get("img")
                            .and_then(Value::as_str)
                            .filter(|value| !value.is_empty())
                            .map(str::to_string),
                        song_count: 0,
                        description: map
                            .get("txt2")
                            .and_then(Value::as_str)
                            .filter(|value| !value.is_empty())
                            .map(str::to_string),
                        play_count: None,
                        creator: None,
                        link: None,
                        extra: Default::default(),
                    });
                }
            }
            for child in map.values() {
                if child.is_array() || child.is_object() {
                    collect_playlists(child, seen, playlists);
                }
            }
        }
        _ => {}
    }
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
