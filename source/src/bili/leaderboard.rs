use std::time::Duration;

use lx_core::model::leaderboard::LeaderboardInfo;
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::{SearchError, SearchResult};
use serde_json::Value;

use super::BiliSource;

const MUSIC_HOT: &str =
    "https://api.bilibili.com/x/centralization/interface/music/hot/rank";
const REGION_RECOMMEND: &str =
    "https://api.bilibili.com/x/web-interface/region/feed/rcmd";
const POPULAR: &str =
    "https://api.bilibili.com/x/web-interface/popular";
const RANKING_V2: &str =
    "https://api.bilibili.com/x/web-interface/ranking/v2";

pub fn get_boards() -> Result<Vec<LeaderboardInfo>, SearchError> {
    Ok(vec![
        LeaderboardInfo::new("popular".to_string(), "全网热门".to_string(), SourceId::Bili),
        LeaderboardInfo::new("ranking".to_string(), "全站排行榜".to_string(), SourceId::Bili),
        LeaderboardInfo::new("music-hot".to_string(), "音乐热歌榜".to_string(), SourceId::Bili),
        LeaderboardInfo::new(
            "recommend".to_string(),
            "热门推荐".to_string(),
            SourceId::Bili,
        ),
    ])
}

pub async fn get_list(
    source: &BiliSource,
    id: &str,
    page: u32,
    limit: u32,
) -> Result<SearchResult, SearchError> {
    match id {
        "popular" => get_popular(source, page, limit).await,
        "ranking" => get_ranking(source, page, limit).await,
        "music-hot" => get_music_hot(source, page, limit).await,
        "recommend" => get_recommend(source, page, limit).await,
        _ => Err(SearchError::Other("未知的哔哩哔哩热门入口".to_string())),
    }
}

async fn get_music_hot(
    source: &BiliSource,
    page: u32,
    limit: u32,
) -> Result<SearchResult, SearchError> {
    let json = source
        .get_json(
            MUSIC_HOT,
            &[("plat", "2".to_string())],
            false,
        )
        .await
        .map_err(|e| SearchError::Network(format!("哔哩哔哩热歌榜网络错误: {e}")))?;
    let code = json["code"].as_i64().unwrap_or(-1);
    if code != 0 {
        let msg = json["message"].as_str().unwrap_or("unknown");
        return Err(SearchError::Api(format!(
            "哔哩哔哩热歌榜失败 (code={code}, msg={msg})",
        )));
    }
    let all = json["data"]["list"]
        .as_array()
        .ok_or_else(|| SearchError::Parse("哔哩哔哩热歌榜数据为空".to_string()))?
        .iter()
        .filter_map(parse_music_song)
        .collect::<Vec<_>>();
    page_slice(all, page, limit)
}

async fn get_recommend(
    source: &BiliSource,
    page: u32,
    limit: u32,
) -> Result<SearchResult, SearchError> {
    let json = source
        .get_json(
            REGION_RECOMMEND,
            &[
                ("display_id", page.max(1).to_string()),
                ("request_cnt", limit.to_string()),
                ("from_region", "1003".to_string()),
            ],
            false,
        )
        .await
        .map_err(|e| SearchError::Network(format!("哔哩哔哩热门推荐网络错误: {e}")))?;
    let code = json["code"].as_i64().unwrap_or(-1);
    if code != 0 {
        let msg = json["message"].as_str().unwrap_or("unknown");
        return Err(SearchError::Api(format!(
            "哔哩哔哩热门推荐失败 (code={code}, msg={msg})",
        )));
    }
    let items = json["data"]["archives"]
        .as_array()
        .ok_or_else(|| SearchError::Parse("哔哩哔哩热门推荐数据为空".to_string()))?
        .iter()
        .filter_map(parse_recommend_song)
        .take(limit as usize)
        .collect::<Vec<_>>();
    Ok(SearchResult {
        total: items.len() as u32 + if items.len() >= limit as usize { limit } else { 0 },
        has_more: items.len() >= limit as usize,
        items,
    })
}

async fn get_popular(
    source: &BiliSource,
    page: u32,
    limit: u32,
) -> Result<SearchResult, SearchError> {
    let json = source
        .get_json(
            POPULAR,
            &[
                ("ps", limit.min(50).max(1).to_string()),
                ("pn", page.max(1).to_string()),
            ],
            false,
        )
        .await
        .map_err(|e| SearchError::Network(format!("哔哩哔哩全网热门网络错误: {e}")))?;
    let code = json["code"].as_i64().unwrap_or(-1);
    tracing::debug!("bili popular response: code={code}");
    if code != 0 {
        let msg = json["message"].as_str().unwrap_or("unknown");
        return Err(SearchError::Api(format!(
            "哔哩哔哩全网热门失败 (code={code}, msg={msg})",
        )));
    }
    let list = match json["data"]["list"].as_array() {
        Some(list) => list,
        None => {
            tracing::warn!("bili popular: no list field, data keys: {:?}", json["data"].as_object().map(|o| o.keys().collect::<Vec<_>>()));
            return Err(SearchError::Parse("哔哩哔哩全网热门数据为空".to_string()));
        }
    };
    let items: Vec<_> = list.iter().filter_map(parse_popular_song).take(limit as usize).collect();
    tracing::debug!("bili popular: parsed {} items", items.len());
    Ok(SearchResult {
        total: json["data"]["no_more"].as_bool()
            .map(|no_more| if no_more { items.len() } else { items.len().saturating_add(1) } as u32)
            .unwrap_or(items.len() as u32),
        has_more: items.len() >= limit as usize,
        items,
    })
}

async fn get_ranking(
    source: &BiliSource,
    page: u32,
    limit: u32,
) -> Result<SearchResult, SearchError> {
    let json = source
        .signed_get(
            RANKING_V2,
            &[
                ("rid", "0".to_string()),
                ("type", "all".to_string()),
            ],
        )
        .await
        .map_err(|e| SearchError::Network(format!("哔哩哔哩全站排行榜网络错误: {e}")))?;
    let code = json["code"].as_i64().unwrap_or(-1);
    if code != 0 {
        let msg = json["message"].as_str().unwrap_or("unknown");
        return Err(SearchError::Api(format!(
            "哔哩哔哩全站排行榜失败 (code={code}, msg={msg})",
        )));
    }
    let all = json["data"]["list"]
        .as_array()
        .ok_or_else(|| SearchError::Parse("哔哩哔哩全站排行榜数据为空".to_string()))?
        .iter()
        .filter_map(parse_popular_song)
        .collect::<Vec<_>>();
    page_slice(all, page, limit)
}

fn parse_popular_song(value: &Value) -> Option<SongInfo> {
    let bvid = value["bvid"].as_str()?.to_string();
    if bvid.is_empty() {
        return None;
    }
    let title = value["title"].as_str().unwrap_or_default();
    let author = value["owner"]["name"]
        .as_str()
        .or_else(|| value["author"].as_str())
        .unwrap_or("哔哩哔哩用户");
    let mut song = SongInfo::new(
        bvid.clone(),
        SourceId::Bili,
        title.to_string(),
        author.to_string(),
    );
    song.album_name = title.to_string();
    song.cover_url = non_empty_url(value["pic"].as_str().or_else(|| value["cover"].as_str()));
    song.duration = value["duration"]
        .as_str()
        .and_then(|v| parse_duration_str(v))
        .or_else(|| value["duration"].as_u64().map(Duration::from_secs))
        .unwrap_or_default();
    song.extra.insert("bvid".to_string(), bvid);
    if let Some(cid) = value["cid"].as_u64() {
        song.extra.insert("cid".to_string(), cid.to_string());
    }
    Some(song)
}

fn parse_music_song(value: &Value) -> Option<SongInfo> {
    let bvid = value["bvid"].as_str()?.to_string();
    if bvid.is_empty() {
        return None;
    }
    let mut song = SongInfo::new(
        bvid.clone(),
        SourceId::Bili,
        value["music_title"].as_str().unwrap_or_default().to_string(),
        value["author"].as_str().unwrap_or_default().to_string(),
    );
    song.album_name = value["album"].as_str().unwrap_or_default().to_string();
    song.cover_url = non_empty_url(value["cover"].as_str());
    song.duration = value["duration"].as_u64().map(Duration::from_secs).unwrap_or_default();
    song.extra.insert("bvid".to_string(), bvid);
    if let Some(cid) = value["cid"].as_u64() {
        song.extra.insert("cid".to_string(), cid.to_string());
    }
    Some(song)
}

fn parse_recommend_song(value: &Value) -> Option<SongInfo> {
    let bvid = value["bvid"].as_str()?.to_string();
    if bvid.is_empty() {
        return None;
    }
    let author = value["owner"]["name"]
        .as_str()
        .or_else(|| value["author"]["name"].as_str())
        .unwrap_or("哔哩哔哩用户");
    let mut song = SongInfo::new(
        bvid.clone(),
        SourceId::Bili,
        value["title"].as_str().unwrap_or_default().to_string(),
        author.to_string(),
    );
    song.album_name = song.name.clone();
    song.cover_url = non_empty_url(value["pic"].as_str().or_else(|| value["cover"].as_str()));
    song.duration = value["duration"].as_u64().map(Duration::from_secs).unwrap_or_default();
    song.extra.insert("bvid".to_string(), bvid);
    if let Some(cid) = value["cid"].as_u64() {
        song.extra.insert("cid".to_string(), cid.to_string());
    }
    Some(song)
}

fn page_slice(
    all: Vec<SongInfo>,
    page: u32,
    limit: u32,
) -> Result<SearchResult, SearchError> {
    let offset = page.saturating_sub(1).saturating_mul(limit) as usize;
    let total = all.len() as u32;
    let items = all.into_iter().skip(offset).take(limit as usize).collect::<Vec<_>>();
    Ok(SearchResult {
        has_more: offset + items.len() < total as usize,
        total,
        items,
    })
}

fn non_empty_url(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            if value.starts_with("//") {
                format!("https:{value}")
            } else {
                value.to_string()
            }
        })
}

fn parse_duration_str(value: &str) -> Option<Duration> {
    let parts: Vec<u64> = value.split(':').filter_map(|p| p.parse().ok()).collect();
    match parts.len() {
        3 => Some(Duration::from_secs(parts[0] * 3600 + parts[1] * 60 + parts[2])),
        2 => Some(Duration::from_secs(parts[0] * 60 + parts[1])),
        1 => Some(Duration::from_secs(parts[0])),
        _ => None,
    }
}
