use lx_core::model::playlist::Playlist;
use lx_core::model::playlist::PlaylistCategory;
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::{FetchError, SearchError};
use serde_json::Value;

use crate::http;
use crate::http::SendWithRetry;

/// 热门歌单列表；`category` 为空表示「全部」。
pub async fn get_list(category: &str, page: u32) -> Result<Vec<Playlist>, FetchError> {
    let category = if category.trim().is_empty() {
        "全部"
    } else {
        category.trim()
    };
    let offset = 30 * page.saturating_sub(1);
    let url = format!(
        "https://music.163.com/api/playlist/list?cat={}&order=hot&limit=30&offset={offset}",
        urlencoding::encode(category)
    );
    let json: Value = super::with_cookie(http::client().get(url))
        .header("Referer", "https://music.163.com/")
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))?;
    if json["code"].as_i64() != Some(200) {
        return Err(FetchError::Other("网易云热门歌单请求失败".to_string()));
    }
    let items = json["playlists"]
        .as_array()
        .ok_or_else(|| FetchError::Parse("网易云热门歌单列表为空".to_string()))?;
    Ok(items.iter().filter_map(parse_playlist).collect())
}

/// 歌单分类目录：`/api/playlist/catalogue` 返回分类分组与子分类。
///
/// 与热门歌单、歌单搜索一样走公开接口，和网易云音源现有的取数方式保持一致；
/// 接口异常时返回空列表，界面上表现为「没有分类可选」，不影响其它功能。
pub async fn get_categories() -> Result<Vec<PlaylistCategory>, FetchError> {
    let json: Value =
        super::with_cookie(http::client().get("https://music.163.com/api/playlist/catalogue"))
            .header("Referer", "https://music.163.com/")
            .send_with_retry(crate::http::RETRY_ATTEMPTS)
            .await
            .map_err(|error| FetchError::Network(error.to_string()))?
            .json()
            .await
            .map_err(|error| FetchError::Parse(error.to_string()))?;
    if json["code"].as_i64() != Some(200) {
        return Err(FetchError::Other("网易云歌单分类请求失败".to_string()));
    }

    let groups = json["categories"].as_object().cloned().unwrap_or_default();
    let mut categories = Vec::new();
    categories.push(PlaylistCategory {
        id: "全部".to_string(),
        name: "全部".to_string(),
        source: SourceId::Wy,
        group: Some("全部".to_string()),
        count: json["all"]["resourceCount"].as_u64().unwrap_or_default() as u32,
        hot: json["all"]["hot"].as_bool().unwrap_or(true),
        extra: Default::default(),
    });
    for item in json["sub"].as_array().into_iter().flatten() {
        let name = item["name"].as_str().unwrap_or_default().trim().to_string();
        if name.is_empty() {
            continue;
        }
        let group = item["category"]
            .as_i64()
            .map(|category| category.to_string())
            .and_then(|key| groups.get(&key).and_then(Value::as_str).map(str::to_string));
        categories.push(PlaylistCategory {
            id: name.clone(),
            name,
            source: SourceId::Wy,
            group,
            count: item["resourceCount"].as_u64().unwrap_or_default() as u32,
            hot: item["hot"].as_bool().unwrap_or(false),
            extra: Default::default(),
        });
    }
    Ok(categories)
}

pub async fn get_detail(id: &str, page: u32) -> Result<Vec<SongInfo>, FetchError> {
    let requested = page.saturating_mul(1000).max(1000);
    let url = format!("https://music.163.com/api/v3/playlist/detail?id={id}&n={requested}&s=0");
    let json: Value = super::with_cookie(http::client().get(url))
        .header("Referer", "https://music.163.com/")
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))?;
    if json["code"].as_i64() != Some(200) {
        return Err(FetchError::Other("网易云歌单详情请求失败".to_string()));
    }
    let items = json["playlist"]["tracks"]
        .as_array()
        .ok_or_else(|| FetchError::Parse("网易云歌单歌曲列表为空".to_string()))?;
    Ok(items.iter().filter_map(super::search::parse_song).collect())
}

/// 我的歌单：需要登录 cookie。
///
/// 用公开的 `api/user/playlist` 而不是 weapi 版本：voicefox 的网易云实现
/// 一直走公开接口（热门歌单、歌单搜索同理），少一套加密实现也少一处失效点。
pub async fn get_user_playlists(page: u32, limit: u32) -> Result<Vec<Playlist>, FetchError> {
    let cookie = super::session::cookie_header()
        .ok_or_else(|| FetchError::Other("请先在设置页登录网易云".to_string()))?;
    let uid = user_id(&cookie).await?;
    let offset = limit.max(1) * page.saturating_sub(1);
    let url = format!(
        "https://music.163.com/api/user/playlist?uid={uid}&limit={}&offset={offset}&includeVideo=true",
        limit.max(1)
    );
    let json: Value = super::with_cookie(http::client().get(url))
        .header("Referer", "https://music.163.com/")
        .header("Cookie", cookie)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))?;
    if json["code"].as_i64() != Some(200) {
        return Err(FetchError::Other("获取网易云个人歌单失败".to_string()));
    }
    Ok(json["playlist"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(parse_playlist)
        .collect())
}

/// 取当前登录账号的 uid：`api/nuser/account/get` 同时返回账号与昵称。
async fn user_id(cookie: &str) -> Result<String, FetchError> {
    let json: Value =
        super::with_cookie(http::client().get("https://music.163.com/api/nuser/account/get"))
            .header("Referer", "https://music.163.com/")
            .header("Cookie", cookie)
            .send_with_retry(crate::http::RETRY_ATTEMPTS)
            .await
            .map_err(|error| FetchError::Network(error.to_string()))?
            .json()
            .await
            .map_err(|error| FetchError::Parse(error.to_string()))?;
    let uid = json["account"]["id"]
        .as_i64()
        .or_else(|| json["profile"]["userId"].as_i64())
        .filter(|uid| *uid > 0)
        .map(|uid| uid.to_string());
    uid.ok_or_else(|| FetchError::Other("网易云登录已失效，请重新扫码".to_string()))
}

pub async fn search_list(keyword: &str, page: u32) -> Result<Vec<Playlist>, SearchError> {
    let offset = 30 * page.saturating_sub(1);
    let url = format!(
        "https://music.163.com/api/search/get/web?csrf_token=&s={}&type=1000&limit=30&offset={offset}",
        urlencoding::encode(keyword)
    );
    let json: Value = super::with_cookie(http::client().get(url))
        .header("Referer", "https://music.163.com/")
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|e| SearchError::Network(e.to_string()))?
        .json()
        .await
        .map_err(|e| SearchError::Parse(e.to_string()))?;
    if json["code"].as_i64() != Some(200) {
        return Err(SearchError::Api("网易云歌单搜索失败".to_string()));
    }
    Ok(json["result"]["playlists"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(parse_playlist)
        .collect())
}

fn parse_playlist(item: &Value) -> Option<Playlist> {
    let id = value_string(&item["id"]);
    let name = item["name"].as_str()?.trim().to_string();
    if id.is_empty() || name.is_empty() {
        return None;
    }
    Some(Playlist {
        id,
        name,
        source: SourceId::Wy,
        cover_url: non_empty_string(&item["coverImgUrl"]),
        song_count: value_u64(&item["trackCount"]).unwrap_or_default() as u32,
        description: non_empty_string(&item["description"]),
        play_count: value_u64(&item["playCount"]),
        creator: None,
        link: None,
        extra: Default::default(),
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
