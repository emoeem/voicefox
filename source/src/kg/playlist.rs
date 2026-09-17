use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use lx_core::model::playlist::{Playlist, PlaylistCategory};
use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{FetchError, SearchError};
use regex::Regex;
use serde_json::Value;

use crate::http;
use crate::http::SendWithRetry;

/// 酷狗移动端接口要求的 UA 与 Referer。
pub(super) const MOBILE_UA: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 13_2_3 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/13.0.3 Mobile/15E148 Safari/604.1";
pub(super) const MOBILE_REFERER: &str = "http://m.kugou.com";

pub async fn get_list(page: u32) -> Result<Vec<Playlist>, FetchError> {
    let url = format!(
        "http://www2.kugou.kugou.com/yueku/v9/special/getSpecial?is_ajax=1&cdn=cdn&t=6&c=&p={page}&pagesize=36"
    );
    let json: Value = super::with_cookie(http::client().get(url))
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
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
    get_detail_with_title(raw_id).await.map(|(_, songs)| songs)
}

/// 取歌单曲目，顺带解析页面标题作为歌单名。
///
/// 酷狗的歌单页把歌单名放在 `<title>` 里（形如「歌单名-酷狗音乐」），
/// 链接直解需要一个展示名，这里复用同一个页面请求，不额外发请求。
pub async fn get_detail_with_title(
    raw_id: &str,
) -> Result<(Option<String>, Vec<SongInfo>), FetchError> {
    let id = raw_id.strip_prefix("id_").unwrap_or(raw_id);
    let url = format!("http://www2.kugou.kugou.com/yueku/v9/special/single/{id}-5-9999.html");
    let html = super::with_cookie(http::client().get(url))
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .text()
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?;
    let values = embedded_song_data(&html)?;
    let songs = values.iter().filter_map(parse_song).collect();
    Ok((page_title(&html), songs))
}

/// 从歌单页 HTML 中取标题，并去掉站点后缀。
pub(super) fn page_title(html: &str) -> Option<String> {
    let regex = Regex::new(r"(?is)<title>(.*?)</title>").ok()?;
    let raw = regex
        .captures(html)
        .and_then(|captures| captures.get(1))
        .map(|capture| capture.as_str())?;
    let cleaned = raw
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"");
    let name = cleaned
        .split(['-', '_', '|'])
        .next()
        .unwrap_or(&cleaned)
        .trim()
        .to_string();
    (!name.is_empty()).then_some(name)
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
        creator: None,
        link: None,
        extra: Default::default(),
    })
}

/// 关键词搜索歌单：`/api/v3/search/special`。
pub async fn search_playlists(keyword: &str, page: u32) -> Result<Vec<Playlist>, SearchError> {
    let url = format!(
        "http://mobilecdn.kugou.com/api/v3/search/special?keyword={}&platform=WebFilter&format=json&page={}&pagesize=10&filter=0",
        urlencoding::encode(keyword),
        page.max(1)
    );
    let json: Value = super::with_cookie(http::client().get(url))
        .header("User-Agent", MOBILE_UA)
        .header("Referer", MOBILE_REFERER)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| SearchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| SearchError::Parse(error.to_string()))?;
    Ok(json["data"]["info"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = value_string(&item["specialid"]);
            let name = item["specialname"].as_str()?.trim().to_string();
            if id.is_empty() || name.is_empty() {
                return None;
            }
            let mut playlist = Playlist::new(id.clone(), name, SourceId::Kg);
            playlist.cover_url = item["imgurl"]
                .as_str()
                .filter(|url| !url.is_empty())
                .map(|url| url.replace("{size}", "240"));
            playlist.song_count = value_u64(&item["songcount"]).unwrap_or_default() as u32;
            playlist.play_count = value_u64(&item["playcount"]);
            playlist.creator = item["nickname"].as_str().map(str::to_string);
            playlist.description = item["intro"].as_str().map(str::to_string);
            playlist.link = Some(format!("https://www.kugou.com/yy/special/single/{id}.html"));
            Some(playlist)
        })
        .collect())
}

/// 歌单分类目录：`/api/v3/tag/list`。
///
/// 酷狗的分类 ID 用 `id:tagid` 组合表示（`tagid` 是「特殊标签」，
/// 0 表示普通分类），拼接规则与 music-lib 保持一致，便于两端互相排查。
pub async fn get_categories() -> Result<Vec<PlaylistCategory>, FetchError> {
    let json: Value = super::with_cookie(
        http::client().get("http://mobilecdnbj.kugou.com/api/v3/tag/list?pid=0&apiver=2&plat=0"),
    )
    .header("User-Agent", MOBILE_UA)
    .header("Referer", MOBILE_REFERER)
    .send_with_retry(crate::http::RETRY_ATTEMPTS)
    .await
    .map_err(|error| FetchError::Network(error.to_string()))?
    .json()
    .await
    .map_err(|error| FetchError::Parse(error.to_string()))?;
    if json["status"].as_i64() != Some(1) || json["errcode"].as_i64().unwrap_or_default() != 0 {
        return Err(FetchError::Other("酷狗歌单分类请求失败".to_string()));
    }

    let mut categories = vec![PlaylistCategory {
        id: String::new(),
        name: "全部".to_string(),
        source: SourceId::Kg,
        group: Some("全部".to_string()),
        count: 0,
        hot: true,
        extra: Default::default(),
    }];
    for group in json["data"]["info"].as_array().into_iter().flatten() {
        let group_name = group["name"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string();
        let group_id = group["id"].as_i64().unwrap_or_default();
        if group_id > 0 && !group_name.is_empty() {
            let mut category =
                PlaylistCategory::new(format!("{group_id}:0"), group_name.clone(), SourceId::Kg);
            category.group = Some(group_name.clone());
            categories.push(category);
        }
        for child in group["children"].as_array().into_iter().flatten() {
            let name = child["name"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_string();
            let id = child["id"].as_i64().unwrap_or_default();
            if id <= 0 || name.is_empty() {
                continue;
            }
            let tag_id = child["special_tag_id"].as_i64().unwrap_or_default();
            let mut category = PlaylistCategory::new(format!("{id}:{tag_id}"), name, SourceId::Kg);
            category.group = (!group_name.is_empty()).then(|| group_name.clone());
            category.hot = child["is_hot"].as_i64() == Some(1);
            categories.push(category);
        }
    }
    Ok(categories)
}

/// 指定分类下的歌单：`/api/v3/tag/specialList`。
pub async fn get_category_list(category: &str, page: u32) -> Result<Vec<Playlist>, FetchError> {
    let (id, tag_id) = parse_category_id(category);
    if id.is_empty() {
        return Err(FetchError::Other("酷狗歌单分类 ID 无效".to_string()));
    }
    let url = format!(
        "http://mobilecdnbj.kugou.com/api/v3/tag/specialList?plat=0&page={}&tagid={tag_id}&pagesize=30&ugc=1&id={id}&sort=2",
        page.max(1)
    );
    let json: Value = super::with_cookie(http::client().get(url))
        .header("User-Agent", MOBILE_UA)
        .header("Referer", MOBILE_REFERER)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))?;
    if json["status"].as_i64() != Some(1) || json["errcode"].as_i64().unwrap_or_default() != 0 {
        return Err(FetchError::Other("酷狗分类歌单请求失败".to_string()));
    }
    Ok(json["data"]["info"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = value_string(&item["specialid"]);
            let id = if id.is_empty() || id == "0" {
                value_string(&item["global_specialid"])
            } else {
                id
            };
            let name = item["specialname"].as_str()?.trim().to_string();
            if id.is_empty() || name.is_empty() {
                return None;
            }
            let mut playlist = Playlist::new(id.clone(), name, SourceId::Kg);
            playlist.cover_url = item["imgurl"]
                .as_str()
                .filter(|url| !url.is_empty())
                .map(|url| url.replace("{size}", "240"));
            playlist.song_count = value_u64(&item["songcount"]).unwrap_or_default() as u32;
            playlist.play_count = value_u64(&item["playcount"]);
            playlist.creator = item["username"]
                .as_str()
                .or_else(|| item["singername"].as_str())
                .map(str::to_string);
            playlist.description = item["intro"]
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string);
            playlist.link = Some(format!("https://www.kugou.com/yy/special/single/{id}.html"));
            playlist
                .extra
                .insert("category_id".to_string(), category.to_string());
            Some(playlist)
        })
        .collect())
}

/// 解析 `id:tagid`；缺省 tagid 按 0 处理。
fn parse_category_id(category: &str) -> (String, String) {
    let (id, tag_id) = category
        .split_once(':')
        .map_or((category, "0"), |(id, tag)| (id, tag));
    (id.trim().to_string(), {
        let tag = tag_id.trim();
        if tag.is_empty() {
            "0".to_string()
        } else {
            tag.to_string()
        }
    })
}

/// 我的歌单：`m.kugou.com/plist/index/{userid}`，需要登录 cookie。
pub async fn get_user_playlists(page: u32, limit: u32) -> Result<Vec<Playlist>, FetchError> {
    let cookie = super::session::cookie_header()
        .ok_or_else(|| FetchError::Other("请先在设置页登录酷狗".to_string()))?;
    let user_id = super::session::user_id()
        .ok_or_else(|| FetchError::Other("酷狗登录缺少账号 ID，请重新扫码".to_string()))?;
    let url = format!(
        "http://m.kugou.com/plist/index/{user_id}?json=true&page={}&pagesize={}",
        page.max(1),
        limit.clamp(1, 100)
    );
    let json: Value = super::with_cookie(http::client().get(url))
        .header("User-Agent", MOBILE_UA)
        .header("Referer", MOBILE_REFERER)
        .header("Cookie", cookie)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| FetchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| FetchError::Parse(error.to_string()))?;

    // 两种返回形态：老接口给 `data.info`，新版给 `plist.list.info`。
    let items = json["data"]["info"]
        .as_array()
        .or_else(|| json["plist"]["list"]["info"].as_array())
        .or_else(|| json["plist"]["list"].as_array());
    let items = items
        .ok_or_else(|| FetchError::Other("获取酷狗个人歌单失败，登录可能已失效".to_string()))?;
    Ok(items.iter().filter_map(parse_user_playlist).collect())
}

/// 个人歌单条目：字段名在新旧接口间有差异，逐个兼容。
fn parse_user_playlist(item: &Value) -> Option<Playlist> {
    let id = value_string(&item["listid"]);
    let id = if id.is_empty() || id == "0" {
        value_string(&item["specialid"])
    } else {
        id
    };
    let id = if id.is_empty() || id == "0" {
        value_string(&item["global_specialid"])
    } else {
        id
    };
    let name = item["specialname"]
        .as_str()
        .or_else(|| item["name"].as_str())?
        .trim()
        .to_string();
    if id.is_empty() || name.is_empty() {
        return None;
    }
    let mut playlist = Playlist::new(id.clone(), name, SourceId::Kg);
    playlist.cover_url =
        first_string(item, &["imgurl", "pic"]).map(|url| url.replace("{size}", "240"));
    playlist.song_count = value_u64(&item["songcount"])
        .or_else(|| value_u64(&item["count"]))
        .unwrap_or_default() as u32;
    playlist.play_count = value_u64(&item["playcount"]);
    playlist.creator = first_string(item, &["list_create_username", "nickname", "username"]);
    playlist.description = first_string(item, &["intro"]);
    playlist.link = Some(format!("https://www.kugou.com/yy/special/single/{id}.html"));
    Some(playlist)
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
    use super::{embedded_song_data, page_title, parse_category_id};

    #[test]
    fn parses_embedded_song_json_without_executing_script() {
        let html = r#"<script>global.data = [{"audio_id":1,"songname":"Song"}];</script>"#;
        let songs = embedded_song_data(html).unwrap();
        assert_eq!(songs.len(), 1);
        assert_eq!(songs[0]["songname"], "Song");
    }

    #[test]
    fn page_title_drops_the_site_suffix() {
        let html = "<html><head><title>华语流行精选-酷狗音乐</title></head></html>";
        assert_eq!(page_title(html).as_deref(), Some("华语流行精选"));
        assert_eq!(page_title("<html></html>"), None);
    }

    #[test]
    fn category_id_parses_optional_tag() {
        assert_eq!(
            parse_category_id("12:3"),
            ("12".to_string(), "3".to_string())
        );
        assert_eq!(parse_category_id("12"), ("12".to_string(), "0".to_string()));
        assert_eq!(
            parse_category_id(" 12 : "),
            ("12".to_string(), "0".to_string())
        );
    }
}
