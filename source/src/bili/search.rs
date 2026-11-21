use std::sync::OnceLock;
use std::time::Duration;

use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::{SearchError, SearchResult};
use regex::Regex;
use reqwest::Url;
use serde_json::Value;

use super::{BILI_REFERER, BiliSource, USER_AGENT};
use crate::http;
use crate::http::SendWithRetry;

const SEARCH_ENDPOINT: &str = "https://api.bilibili.com/x/web-interface/wbi/search/type";
const VIEW_ENDPOINT: &str = "https://api.bilibili.com/x/web-interface/view";

#[derive(Debug, Clone, PartialEq, Eq)]
enum VideoReference {
    Bvid { bvid: String, page: Option<u32> },
    Aid { aid: String, page: Option<u32> },
}

pub async fn search(
    source: &BiliSource,
    keyword: &str,
    page: u32,
    limit: u32,
) -> Result<SearchResult, SearchError> {
    if let Some(reference) = resolve_video_reference(keyword).await? {
        return fetch_video(source, reference).await;
    }

    let json = source
        .signed_get(
            SEARCH_ENDPOINT,
            &[
                ("search_type", "video".to_string()),
                ("keyword", keyword.to_string()),
                ("page", page.max(1).to_string()),
                ("page_size", limit.clamp(1, 50).to_string()),
                ("tids", "3".to_string()),
            ],
        )
        .await
        .map_err(SearchError::Network)?;
    if json["code"].as_i64() != Some(0) {
        return Err(SearchError::Api(super::api_error(
            &json,
            "哔哩哔哩搜索失败",
        )));
    }
    parse_search_result(&json, page)
}

pub(crate) fn looks_like_video_reference(input: &str) -> bool {
    parse_video_reference(input).is_some() || short_video_url(input).is_some()
}

async fn resolve_video_reference(input: &str) -> Result<Option<VideoReference>, SearchError> {
    if let Some(reference) = parse_video_reference(input) {
        return Ok(Some(reference));
    }
    let Some(short_url) = short_video_url(input) else {
        return Ok(None);
    };

    let response = http::client()
        .get(short_url)
        .header("User-Agent", USER_AGENT)
        .header("Referer", BILI_REFERER)
        .send_with_retry(crate::http::RETRY_ATTEMPTS)
        .await
        .map_err(|error| SearchError::Network(format!("哔哩哔哩短链展开失败: {error}")))?;
    let final_url = response.url().to_string();
    parse_video_reference(&final_url)
        .map(Some)
        .ok_or_else(|| SearchError::Parse("哔哩哔哩短链中未找到 BV 号或 av 号".to_string()))
}

fn parse_video_reference(input: &str) -> Option<VideoReference> {
    static BVID: OnceLock<Regex> = OnceLock::new();
    static AID: OnceLock<Regex> = OnceLock::new();

    let page = parse_page(input);
    if let Some(value) = BVID
        .get_or_init(|| Regex::new(r"(?i)BV[0-9A-Za-z]{10}").expect("valid BVID regex"))
        .find(input)
        .map(|matched| matched.as_str())
    {
        let bvid = format!("BV{}", &value[2..]);
        return Some(VideoReference::Bvid { bvid, page });
    }
    AID.get_or_init(|| Regex::new(r"(?i)av([0-9]{1,20})").expect("valid aid regex"))
        .captures(input)
        .and_then(|captures| captures.get(1))
        .map(|matched| VideoReference::Aid {
            aid: matched.as_str().to_string(),
            page,
        })
}

fn parse_page(input: &str) -> Option<u32> {
    static PAGE: OnceLock<Regex> = OnceLock::new();

    PAGE.get_or_init(|| Regex::new(r"(?i)(?:[?&]|&amp;)p=([0-9]+)").expect("valid page regex"))
        .captures(input)
        .and_then(|captures| captures.get(1))
        .and_then(|matched| matched.as_str().parse::<u32>().ok())
        .filter(|page| *page > 0)
}

fn short_video_url(input: &str) -> Option<String> {
    static URLS: OnceLock<Regex> = OnceLock::new();

    URLS.get_or_init(|| {
        Regex::new(r#"https?://[^\s<>"'，。；！？）】]+"#).expect("valid URL regex")
    })
    .find_iter(input)
    .filter_map(|matched| {
        let value = matched.as_str().trim_end_matches(|character: char| {
            matches!(
                character,
                ',' | '.'
                    | ';'
                    | '!'
                    | '?'
                    | ')'
                    | ']'
                    | '}'
                    | '，'
                    | '。'
                    | '；'
                    | '！'
                    | '？'
                    | '）'
                    | '】'
            )
        });
        let url = Url::parse(value).ok()?;
        matches!(
            url.host_str(),
            Some("b23.tv" | "www.b23.tv" | "bili2233.cn" | "www.bili2233.cn")
        )
        .then(|| url.to_string())
    })
    .next()
}

async fn fetch_video(
    source: &BiliSource,
    reference: VideoReference,
) -> Result<SearchResult, SearchError> {
    let (params, requested_page) = match reference {
        VideoReference::Bvid { bvid, page } => (vec![("bvid", bvid)], page),
        VideoReference::Aid { aid, page } => (vec![("aid", aid)], page),
    };
    let json = source
        .get_json(VIEW_ENDPOINT, &params, false)
        .await
        .map_err(SearchError::Network)?;
    if json["code"].as_i64() != Some(0) {
        return Err(SearchError::Api(super::api_error(
            &json,
            "获取哔哩哔哩视频失败",
        )));
    }
    parse_video_result(&json, requested_page)
}

pub(crate) async fn fetch_video_parts(
    source: &BiliSource,
    bvid: &str,
) -> Result<SearchResult, SearchError> {
    fetch_video(
        source,
        VideoReference::Bvid {
            bvid: bvid.to_string(),
            page: None,
        },
    )
    .await
}

fn parse_video_result(
    json: &Value,
    requested_page: Option<u32>,
) -> Result<SearchResult, SearchError> {
    let data = &json["data"];
    let bvid = data["bvid"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| SearchError::Parse("哔哩哔哩视频 BV 号为空".to_string()))?;
    let aid = value_string(&data["aid"]);
    let title = data["title"]
        .as_str()
        .filter(|value| !value.is_empty())
        .unwrap_or(bvid);
    let singer = data["owner"]["name"]
        .as_str()
        .filter(|value| !value.is_empty())
        .unwrap_or("哔哩哔哩用户");
    let cover_url = normalize_cover(data["pic"].as_str());
    let raw_pages = data["pages"].as_array();
    let multiple_pages = raw_pages.is_some_and(|pages| pages.len() > 1);

    let mut items = raw_pages
        .into_iter()
        .flatten()
        .filter_map(|page| {
            let page_number = page["page"].as_u64().unwrap_or(1) as u32;
            if requested_page.is_some_and(|requested| requested != page_number) {
                return None;
            }
            let cid = value_string(&page["cid"]);
            if cid.is_empty() {
                return None;
            }
            let part = page["part"].as_str().unwrap_or_default().trim();
            let part_title = if part.is_empty() {
                format!("P{page_number}")
            } else {
                part.to_string()
            };
            // A Bilibili video groups all of its pages; each page is an individual playable song.
            let name = if multiple_pages {
                part_title.clone()
            } else {
                title.to_string()
            };
            let id = if multiple_pages {
                format!("{bvid}-p{page_number}")
            } else {
                bvid.to_string()
            };
            let mut song = SongInfo::new(id, SourceId::Bili, name, singer.to_string());
            song.album_name = title.to_string();
            song.album_id = bvid.to_string();
            song.duration = Duration::from_secs(page["duration"].as_u64().unwrap_or_default());
            song.cover_url = cover_url.clone();
            song.extra.insert("bvid".to_string(), bvid.to_string());
            song.extra.insert("cid".to_string(), cid);
            song.extra
                .insert("page".to_string(), page_number.to_string());
            song.extra.insert("bili_part_title".to_string(), part_title);
            if !aid.is_empty() {
                song.extra.insert("aid".to_string(), aid.clone());
            }
            Some(song)
        })
        .collect::<Vec<_>>();

    if items.is_empty() && requested_page.is_none() {
        let cid = value_string(&data["cid"]);
        if !cid.is_empty() {
            let mut song = SongInfo::new(
                bvid.to_string(),
                SourceId::Bili,
                title.to_string(),
                singer.to_string(),
            );
            song.album_name = title.to_string();
            song.album_id = bvid.to_string();
            song.duration = Duration::from_secs(data["duration"].as_u64().unwrap_or_default());
            song.cover_url = cover_url;
            song.extra.insert("bvid".to_string(), bvid.to_string());
            song.extra.insert("cid".to_string(), cid);
            song.extra.insert("page".to_string(), "1".to_string());
            if !aid.is_empty() {
                song.extra.insert("aid".to_string(), aid);
            }
            items.push(song);
        }
    }

    if items.is_empty() {
        return Err(if let Some(page) = requested_page {
            SearchError::Parse(format!("哔哩哔哩视频不存在 P{page}"))
        } else {
            SearchError::Parse("哔哩哔哩视频分 P 信息为空".to_string())
        });
    }
    Ok(SearchResult {
        total: items.len() as u32,
        has_more: false,
        items,
    })
}

fn parse_search_result(json: &Value, page: u32) -> Result<SearchResult, SearchError> {
    let data = &json["data"];
    let raw_items = data["result"]
        .as_array()
        .ok_or_else(|| SearchError::Parse("哔哩哔哩搜索结果为空".to_string()))?;
    let items = raw_items.iter().filter_map(parse_song).collect::<Vec<_>>();
    let total = data["numResults"].as_u64().unwrap_or(items.len() as u64) as u32;
    let page_size = data["pagesize"]
        .as_u64()
        .map(|value| value as u32)
        .filter(|value| *value > 0)
        .unwrap_or(raw_items.len() as u32);
    Ok(SearchResult {
        has_more: !items.is_empty() && page.saturating_mul(page_size) < total,
        total,
        items,
    })
}

/// `pub(crate)` 供 musid 适配层 V2 对账测试消费真实 legacy parse 产物
/// （docs/MUSID_DLI_ADAPTER.md §12.5：仅可见性扩宽，函数体零改动）。
pub(crate) fn parse_song(item: &Value) -> Option<SongInfo> {
    let bvid = item["bvid"].as_str()?.trim();
    if bvid.is_empty() {
        return None;
    }
    let title = strip_html(item["title"].as_str().unwrap_or_default());
    let mut song = SongInfo::new(
        bvid.to_string(),
        SourceId::Bili,
        title.clone(),
        item["author"]
            .as_str()
            .unwrap_or("哔哩哔哩用户")
            .to_string(),
    );
    song.album_name = title;
    song.album_id = bvid.to_string();
    song.duration = parse_duration(item["duration"].as_str().unwrap_or_default());
    song.cover_url = item["pic"]
        .as_str()
        .filter(|value| !value.is_empty())
        .map(|value| {
            if value.starts_with("//") {
                format!("https:{value}")
            } else {
                value.to_string()
            }
        });
    song.extra.insert("bvid".to_string(), bvid.to_string());
    if let Some(cid) = item["cid"].as_u64() {
        song.extra.insert("cid".to_string(), cid.to_string());
    }
    if let Some(aid) = item["aid"].as_u64() {
        song.extra.insert("aid".to_string(), aid.to_string());
    }
    Some(song)
}

fn strip_html(value: &str) -> String {
    static HTML_TAGS: OnceLock<Regex> = OnceLock::new();

    let regex = HTML_TAGS.get_or_init(|| Regex::new(r"<[^>]+>").expect("valid HTML tag regex"));
    let value = regex.replace_all(value, "").into_owned();
    value
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

fn parse_duration(value: &str) -> Duration {
    let mut parts = value.split(':').rev();
    let seconds = parts
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let minutes = parts
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let hours = parts
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    Duration::from_secs(hours * 3600 + minutes * 60 + seconds)
}

fn value_string(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_u64().map(|value| value.to_string()))
        .unwrap_or_default()
}

fn normalize_cover(value: Option<&str>) -> Option<String> {
    value.filter(|value| !value.is_empty()).map(|value| {
        if value.starts_with("//") {
            format!("https:{value}")
        } else {
            value.to_string()
        }
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::{
        VideoReference, looks_like_video_reference, parse_search_result, parse_video_reference,
        parse_video_result, short_video_url,
    };

    fn item(index: u32) -> serde_json::Value {
        json!({
            "bvid": format!("BV{index:010}"),
            "title": format!("song {index}"),
            "author": "artist",
            "duration": "03:00",
            "pic": "//example.com/cover.jpg"
        })
    }

    #[test]
    fn pagination_uses_server_page_size() {
        let json = json!({
            "data": {
                "result": (0..20).map(item).collect::<Vec<_>>(),
                "numResults": 45,
                "pagesize": 20
            }
        });

        assert!(parse_search_result(&json, 2).unwrap().has_more);
        assert!(!parse_search_result(&json, 3).unwrap().has_more);
    }

    #[test]
    fn search_result_uses_the_video_as_its_album() {
        let json = json!({
            "data": {
                "result": [{
                    "bvid": "BV1xx411c7mD",
                    "title": "测试<em>视频</em>",
                    "author": "UP主",
                    "mid": 42,
                    "duration": "03:00"
                }],
                "numResults": 1,
                "pagesize": 20
            }
        });

        let song = parse_search_result(&json, 1).unwrap().items.remove(0);

        assert_eq!(song.name, "测试视频");
        assert_eq!(song.album_name, "测试视频");
        assert_eq!(song.album_id, "BV1xx411c7mD");
        assert_eq!(song.singer, "UP主");
    }

    #[test]
    fn parses_plain_bvid_and_long_video_url() {
        assert_eq!(
            parse_video_reference("BV1xx411c7mD"),
            Some(VideoReference::Bvid {
                bvid: "BV1xx411c7mD".to_string(),
                page: None,
            })
        );
        assert_eq!(
            parse_video_reference(
                "https://www.bilibili.com/video/BV1xx411c7mD/?share_source=copy_web&p=2"
            ),
            Some(VideoReference::Bvid {
                bvid: "BV1xx411c7mD".to_string(),
                page: Some(2),
            })
        );
    }

    #[test]
    fn parses_av_url_and_detects_short_links() {
        assert_eq!(
            parse_video_reference("https://www.bilibili.com/video/av170001?p=3"),
            Some(VideoReference::Aid {
                aid: "170001".to_string(),
                page: Some(3),
            })
        );
        assert_eq!(
            short_video_url("复制链接 https://b23.tv/abc123，打开哔哩哔哩"),
            Some("https://b23.tv/abc123".to_string())
        );
        assert!(looks_like_video_reference(
            "复制链接 https://b23.tv/abc123，打开哔哩哔哩"
        ));
        assert!(!looks_like_video_reference("周杰伦 晴天"));
    }

    #[test]
    fn direct_video_result_exposes_each_part_for_selection() {
        let json = json!({
            "code": 0,
            "data": {
                "bvid": "BV1xx411c7mD",
                "aid": 170001,
                "title": "测试视频",
                "pic": "//example.com/cover.jpg",
                "owner": {"name": "UP主", "mid": 42},
                "pages": [
                    {"cid": 1001, "page": 1, "part": "第一段", "duration": 60},
                    {"cid": 1002, "page": 2, "part": "第二段", "duration": 90}
                ]
            }
        });

        let result = parse_video_result(&json, None).unwrap();

        assert_eq!(result.total, 2);
        assert!(!result.has_more);
        assert_eq!(result.items[0].name, "第一段");
        assert_eq!(result.items[1].extra["cid"], "1002");
        assert_eq!(result.items[1].extra["bili_part_title"], "第二段");
        assert_eq!(result.items[0].album_name, "测试视频");
        assert_eq!(result.items[0].album_id, "BV1xx411c7mD");
        assert_eq!(
            result.items[0].cover_url.as_deref(),
            Some("https://example.com/cover.jpg")
        );
    }

    #[test]
    fn expanded_parts_carry_no_quality_until_a_stream_is_chosen() {
        let json = json!({
            "code": 0,
            "data": {
                "bvid": "BV1xx411c7mD",
                "title": "测试视频",
                "owner": {"name": "UP主", "mid": 42},
                "pages": [
                    {"cid": 1001, "page": 1, "part": "第一段", "duration": 60}
                ]
            }
        });

        let result = parse_video_result(&json, None).unwrap();

        assert!(result.items[0].qualities.is_empty());
    }

    #[test]
    fn direct_video_url_selects_requested_part() {
        let json = json!({
            "code": 0,
            "data": {
                "bvid": "BV1xx411c7mD",
                "title": "测试视频",
                "owner": {"name": "UP主"},
                "pages": [
                    {"cid": 1001, "page": 1, "part": "第一段", "duration": 60},
                    {"cid": 1002, "page": 2, "part": "第二段", "duration": 90}
                ]
            }
        });

        let result = parse_video_result(&json, Some(2)).unwrap();

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].id, "BV1xx411c7mD-p2");
        assert_eq!(result.items[0].duration, Duration::from_secs(90));
        assert_eq!(result.items[0].name, "第二段");
        assert_eq!(result.items[0].album_name, "测试视频");
    }

    #[test]
    fn single_part_video_uses_the_video_title_as_its_song_name() {
        let json = json!({
            "data": {
                "bvid": "BV1xx411c7mD",
                "title": "测试视频",
                "owner": {"name": "UP主"},
                "pages": [{"cid": 1001, "page": 1, "part": "第一段", "duration": 60}]
            }
        });

        let song = parse_video_result(&json, None).unwrap().items.remove(0);

        assert_eq!(song.name, "测试视频");
        assert_eq!(song.album_name, "测试视频");
        assert_eq!(song.album_id, "BV1xx411c7mD");
        assert_eq!(song.extra["bili_part_title"], "第一段");
    }
}
