use lx_core::model::playlist::{Playlist, PlaylistCategory};
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::{FetchError, SearchError};
use serde_json::Value;

use crate::http;
use crate::http::SendWithRetry;

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
    let json: Value =
        super::with_cookie(http::client().post("https://u.y.qq.com/cgi-bin/musicu.fcg"))
            .header("Referer", "https://y.qq.com/")
            .json(&body)
            .send_with_retry(crate::http::RETRY_ATTEMPTS)
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
    get_detail_with_meta(id).await.map(|(_, songs)| songs)
}

/// 歌单详情 + 元数据：链接直解需要歌单名与封面。
pub async fn get_detail_with_meta(id: &str) -> Result<(Playlist, Vec<SongInfo>), FetchError> {
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
    let json: Value =
        super::with_cookie(http::client().post("https://u.y.qq.com/cgi-bin/musicu.fcg"))
            .header("Referer", "https://y.qq.com/")
            .json(&body)
            .send_with_retry(crate::http::RETRY_ATTEMPTS)
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
    let songs = items
        .iter()
        .filter_map(super::search::parse_song)
        .collect::<Vec<_>>();
    let detail = &json["req"]["data"];
    let name = detail["dirinfo"]["title"]
        .as_str()
        .or_else(|| detail["dissname"].as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("QQ 歌单 {id}"));
    let mut playlist = Playlist::new(id, name, SourceId::Tx);
    playlist.song_count = songs.len() as u32;
    playlist.cover_url = detail["dirinfo"]["picurl"]
        .as_str()
        .filter(|url| !url.is_empty())
        .map(str::to_string);
    playlist.description = detail["desc"].as_str().map(str::to_string);
    playlist.play_count = value_u64(&detail["visitnum"]);
    playlist.creator = detail["nickname"]
        .as_str()
        .or_else(|| detail["hostname"].as_str())
        .map(str::to_string);
    playlist.link = Some(format!("https://y.qq.com/n/ryqq/playlist/{id}"));
    Ok((playlist, songs))
}

/// 按专辑 MID 获取专辑曲目，避免走“歌手+专辑名搜索”兜底。
pub async fn get_album_songs(
    album_mid: &str,
    page: u32,
    limit: u32,
) -> Result<Vec<SongInfo>, FetchError> {
    let limit = limit.clamp(1, 1000);
    let begin = limit * page.saturating_sub(1);
    let body = serde_json::json!({
        "comm": { "ct": 24, "cv": 10000 },
        "albumSonglist": {
            "method": "GetAlbumSongList",
            "param": { "albumMid": album_mid, "albumID": 0, "begin": begin, "num": limit, "order": 2 },
            "module": "music.musichallAlbum.AlbumSongList"
        }
    });
    let json: Value =
        super::with_cookie(http::client().post("https://u.y.qq.com/cgi-bin/musicu.fcg"))
            .header("Referer", "https://y.qq.com/")
            .json(&body)
            .send_with_retry(crate::http::RETRY_ATTEMPTS)
            .await
            .map_err(|error| FetchError::Network(error.to_string()))?
            .json()
            .await
            .map_err(|error| FetchError::Parse(error.to_string()))?;
    let req = &json["albumSonglist"];
    if req["code"].as_i64().unwrap_or(-1) != 0 {
        return Err(FetchError::Other("QQ 专辑曲目请求失败".to_string()));
    }
    let songs = req["data"]["songList"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| super::search::parse_song(&item["songInfo"]))
        .collect::<Vec<_>>();
    if songs.is_empty() {
        return Err(FetchError::NotFound);
    }
    Ok(songs)
}

/// 当前登录账号的个人歌单。
pub async fn get_user_playlists(page: u32, limit: u32) -> Result<Vec<Playlist>, FetchError> {
    if !super::session::is_logged_in() {
        return Err(FetchError::Other("请先在设置页登录 QQ 音乐".to_string()));
    }
    let uin = super::session::snapshot()
        .user_id
        .filter(|value| !value.is_empty())
        .ok_or_else(|| FetchError::Other("QQ 登录缺少账号 ID，请重新扫码".to_string()))?;
    let begin = limit.max(1) * page.saturating_sub(1);
    let url = format!(
        "https://c.y.qq.com/rsc/fcgi-bin/fcg_user_created_diss.fcg?format=json&hostuin={uin}&sin={begin}&ein={}&g_tk=5381&loginUin={uin}",
        begin + limit.max(1) - 1
    );
    let json: Value = super::with_cookie(http::client().get(url))
        .header("Referer", "https://y.qq.com/")
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))?;
    if json["code"].as_i64().unwrap_or(-1) != 0 {
        return Err(FetchError::Other(
            "获取 QQ 个人歌单失败，登录可能已失效".to_string(),
        ));
    }
    let items = json["data"]["disslist"]
        .as_array()
        .or_else(|| json["data"]["list"].as_array())
        .ok_or_else(|| FetchError::Other("QQ 个人歌单列表为空".to_string()))?;
    Ok(items.iter().filter_map(parse_user_playlist).collect())
}

fn parse_user_playlist(item: &Value) -> Option<Playlist> {
    let id = {
        let id = value_string(&item["dissid"]);
        if id.is_empty() {
            value_string(&item["tid"])
        } else {
            id
        }
    };
    let name = item["dissname"]
        .as_str()
        .or_else(|| item["title"].as_str())?
        .trim()
        .to_string();
    if id.is_empty() || name.is_empty() {
        return None;
    }
    let mut playlist = Playlist::new(id.clone(), name, SourceId::Tx);
    playlist.cover_url = item["logo"]
        .as_str()
        .or_else(|| item["picurl"].as_str())
        .filter(|url| !url.is_empty())
        .map(|url| url.replace("http://", "https://"));
    playlist.song_count = value_u64(&item["songnum"])
        .or_else(|| value_u64(&item["song_count"]))
        .unwrap_or_default() as u32;
    playlist.play_count = value_u64(&item["listennum"]).or_else(|| value_u64(&item["visitnum"]));
    playlist.creator = item["creatorname"]
        .as_str()
        .or_else(|| item["creator"]["name"].as_str())
        .map(str::to_string);
    playlist.link = Some(format!("https://y.qq.com/n/ryqq/playlist/{id}"));
    Some(playlist)
}

/// 歌单分类目录：老版 `fcg_get_diss_tag_conf` 接口（与 music-lib 一致）。
pub async fn get_categories() -> Result<Vec<PlaylistCategory>, FetchError> {
    let json: Value = super::with_cookie(http::client()
        .get("https://c.y.qq.com/splcloud/fcgi-bin/fcg_get_diss_tag_conf.fcg?format=json&inCharset=utf8&outCharset=utf-8"))
        .header("Referer", "https://y.qq.com/")
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))?;
    if json["code"].as_i64().unwrap_or_default() != 0 {
        return Err(FetchError::Other("QQ 歌单分类请求失败".to_string()));
    }
    let mut categories = vec![PlaylistCategory {
        id: String::new(),
        name: "全部".to_string(),
        source: SourceId::Tx,
        group: Some("全部".to_string()),
        count: 0,
        hot: true,
        extra: Default::default(),
    }];
    for group in json["data"]["categories"].as_array().into_iter().flatten() {
        let group_name = group["categoryGroupName"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string();
        for item in group["items"].as_array().into_iter().flatten() {
            // usable=0 的分类已经下架；10000000 是「全部」的哨兵值。
            if item["usable"].as_i64().unwrap_or_default() == 0 {
                continue;
            }
            let id = item["categoryId"].as_i64().unwrap_or_default();
            let name = item["categoryName"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_string();
            if id == 0 || id == 10_000_000 || name.is_empty() {
                continue;
            }
            let mut category = PlaylistCategory::new(id.to_string(), name, SourceId::Tx);
            category.group = (!group_name.is_empty()).then(|| group_name.clone());
            categories.push(category);
        }
    }
    Ok(categories)
}

/// 分类歌单：`fcg_get_diss_by_tag`（分类为空时用「全部」的哨兵 ID）。
pub async fn get_category_list(category: &str, page: u32) -> Result<Vec<Playlist>, FetchError> {
    let category_id = if category.trim().is_empty() {
        "10000000"
    } else {
        category.trim()
    };
    let limit = 30u32;
    let offset = (page.max(1) - 1) * limit;
    let url = format!(
        "https://c.y.qq.com/splcloud/fcgi-bin/fcg_get_diss_by_tag.fcg?picmid=1&rnd=0.1&g_tk=5381&loginUin=0&hostUin=0&format=json&inCharset=utf8&outCharset=utf-8&notice=0&platform=yqq.json&needNewCode=0&categoryId={category_id}&sortId=5&sin={offset}&ein={}",
        offset + limit - 1
    );
    let json: Value = super::with_cookie(http::client().get(url))
        .header("Referer", "https://y.qq.com/")
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))?;
    if json["code"].as_i64().unwrap_or_default() != 0 {
        return Err(FetchError::Other("QQ 分类歌单请求失败".to_string()));
    }
    Ok(json["data"]["list"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = value_string(&item["dissid"]);
            let name = item["dissname"].as_str()?.trim().to_string();
            if id.is_empty() || name.is_empty() {
                return None;
            }
            let mut playlist = Playlist::new(id.clone(), name, SourceId::Tx);
            playlist.cover_url = item["imgurl"]
                .as_str()
                .filter(|url| !url.is_empty())
                .map(|url| url.replace("http://", "https://"));
            playlist.song_count = value_u64(&item["song_count"])
                .or_else(|| value_u64(&item["song_num"]))
                .unwrap_or_default() as u32;
            playlist.play_count = value_u64(&item["listennum"]);
            playlist.creator = item["creator"]["name"].as_str().map(str::to_string);
            playlist.description = non_empty_string(&item["introduction"]);
            playlist.link = Some(format!("https://y.qq.com/n/ryqq/playlist/{id}"));
            Some(playlist)
        })
        .collect())
}

/// 歌单搜索：老版 `client_music_search_songlist`，响应是 JSONP，需要剥壳。
pub async fn search_playlists(keyword: &str, page: u32) -> Result<Vec<Playlist>, SearchError> {
    let url = format!(
        "http://c.y.qq.com/soso/fcgi-bin/client_music_search_songlist?query={}&page_no={}&num_per_page=20&format=json&remoteplace=txt.yqq.playlist&flag_qc=0",
        urlencoding::encode(keyword),
        page.saturating_sub(1)
    );
    let text = super::with_cookie(http::client().get(url))
        .header("Referer", "https://y.qq.com/portal/search.html")
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| SearchError::Network(error.to_string()))?
        .text()
        .await
        .map_err(|error| SearchError::Network(error.to_string()))?;
    let json: Value = serde_json::from_str(&strip_jsonp(&text))
        .map_err(|error| SearchError::Parse(error.to_string()))?;
    Ok(json["data"]["list"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = value_string(&item["dissid"]);
            let name = item["dissname"].as_str()?.trim().to_string();
            if id.is_empty() || name.is_empty() {
                return None;
            }
            let mut playlist = Playlist::new(id.clone(), name, SourceId::Tx);
            playlist.cover_url = item["imgurl"]
                .as_str()
                .filter(|url| !url.is_empty())
                .map(|url| url.replace("http://", "https://"));
            playlist.song_count = value_u64(&item["song_count"]).unwrap_or_default() as u32;
            playlist.play_count = value_u64(&item["listennum"]);
            playlist.creator = item["creator"]["name"].as_str().map(str::to_string);
            playlist.link = Some(format!("https://y.qq.com/n/ryqq/playlist/{id}"));
            Some(playlist)
        })
        .collect())
}

/// 剥掉 JSONP 外壳：`callback({...})` → `{...}`。
pub(super) fn strip_jsonp(body: &str) -> String {
    let trimmed = body.trim();
    match (trimmed.find('('), trimmed.rfind(')')) {
        (Some(start), Some(end)) if end > start => trimmed[start + 1..end].trim().to_string(),
        _ => trimmed.to_string(),
    }
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
