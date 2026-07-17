use std::collections::HashSet;
use std::sync::OnceLock;

use lx_core::model::leaderboard::LeaderboardInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::{SearchError, SearchResult};
use regex::Regex;
use serde_json::Value;

use crate::http;

pub async fn get_boards() -> Result<Vec<LeaderboardInfo>, SearchError> {
    match get_periods().await {
        Ok(periods) if !periods.is_empty() => Ok(periods
            .into_iter()
            .map(|period| {
                let mut board = LeaderboardInfo::new(period.id, period.name, SourceId::Tx);
                board.update = Some(period.period);
                board
            })
            .collect()),
        _ => get_boards_fallback().await,
    }
}

pub async fn get_list(board_id: &str, page: u32, limit: u32) -> Result<SearchResult, SearchError> {
    let period = get_periods()
        .await?
        .into_iter()
        .find(|period| period.id == board_id)
        .map(|period| period.period)
        .ok_or_else(|| SearchError::Other("QQ 音乐未返回该榜单的当前周期".to_string()))?;
    let request_limit = page.saturating_mul(limit).clamp(limit, 300);
    let body = serde_json::json!({
        "toplist": {
            "module": "musicToplist.ToplistInfoServer",
            "method": "GetDetail",
            "param": {
                "topid": board_id.parse::<u64>().unwrap_or_default(),
                "num": request_limit,
                "period": period
            }
        },
        "comm": {
            "uin": 0,
            "format": "json",
            "ct": 20,
            "cv": 1859
        }
    });
    let json: Value = http::client()
        .post("https://u.y.qq.com/cgi-bin/musicu.fcg")
        .header(
            "User-Agent",
            "Mozilla/5.0 (compatible; MSIE 9.0; Windows NT 6.1; WOW64; Trident/5.0)",
        )
        .json(&body)
        .send()
        .await
        .map_err(|error| SearchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| SearchError::Parse(error.to_string()))?;
    let code = json["code"].as_i64().unwrap_or(-1);
    let toplist_code = json["toplist"]["code"].as_i64().unwrap_or(-1);
    if code != 0 || toplist_code != 0 {
        return Err(SearchError::Api(format!(
            "QQ 榜单请求失败: code={code}, toplist.code={toplist_code}"
        )));
    }

    let raw_items = json["toplist"]["data"]["songInfoList"]
        .as_array()
        .ok_or_else(|| SearchError::Parse("QQ 榜单歌曲列表为空".to_string()))?;
    let all_items = raw_items
        .iter()
        .filter_map(super::search::parse_song)
        .collect::<Vec<_>>();
    let total = all_items.len() as u32;
    let offset = page.saturating_sub(1).saturating_mul(limit) as usize;
    let items = all_items
        .into_iter()
        .skip(offset)
        .take(limit as usize)
        .collect::<Vec<_>>();
    Ok(SearchResult {
        items,
        total,
        has_more: offset.saturating_add(limit as usize) < total as usize,
    })
}

#[derive(Debug)]
struct PeriodInfo {
    id: String,
    name: String,
    period: String,
}

async fn get_periods() -> Result<Vec<PeriodInfo>, SearchError> {
    let html = http::client()
        .get("https://c.y.qq.com/node/pc/wk_v15/top.html")
        .send()
        .await
        .map_err(|error| SearchError::Network(error.to_string()))?
        .text()
        .await
        .map_err(|error| SearchError::Network(error.to_string()))?;
    static PERIOD_RE: OnceLock<Regex> = OnceLock::new();
    let regex = PERIOD_RE.get_or_init(|| {
        Regex::new(r#"data-listname="([^"]+)" data-tid="[^"]*/(\d+)" data-date="([^"]+)""#)
            .expect("valid QQ leaderboard regex")
    });
    let mut seen = HashSet::new();
    let periods = regex
        .captures_iter(&html)
        .filter_map(|captures| {
            let id = captures.get(2)?.as_str().to_string();
            if !seen.insert(id.clone()) {
                return None;
            }
            Some(PeriodInfo {
                name: captures.get(1)?.as_str().to_string(),
                id,
                period: captures.get(3)?.as_str().to_string(),
            })
        })
        .collect::<Vec<_>>();
    if periods.is_empty() {
        return Err(SearchError::Parse("QQ 榜单周期页解析失败".to_string()));
    }
    Ok(periods)
}

async fn get_boards_fallback() -> Result<Vec<LeaderboardInfo>, SearchError> {
    let json: Value = http::client()
        .get("https://c.y.qq.com/v8/fcg-bin/fcg_myqq_toplist.fcg?g_tk=1928093487&inCharset=utf-8&outCharset=utf-8&notice=0&format=json&uin=0&needNewCode=1&platform=h5")
        .send()
        .await
        .map_err(|error| SearchError::Network(error.to_string()))?
        .json()
        .await
        .map_err(|error| SearchError::Parse(error.to_string()))?;
    if json["code"].as_i64().unwrap_or(-1) != 0 {
        return Err(SearchError::Api("QQ 榜单目录请求失败".to_string()));
    }
    let raw_boards = json["data"]["topList"]
        .as_array()
        .ok_or_else(|| SearchError::Parse("QQ 榜单目录为空".to_string()))?;
    let boards = raw_boards
        .iter()
        .filter(|board| board["id"].as_i64() != Some(201))
        .filter_map(|board| {
            let id = board["id"].as_u64()?.to_string();
            let mut name = board["topTitle"].as_str()?.trim().to_string();
            name = name.strip_prefix("巅峰榜·").unwrap_or(&name).to_string();
            if !name.ends_with('榜') {
                name.push('榜');
            }
            Some(LeaderboardInfo::new(id, name, SourceId::Tx))
        })
        .collect::<Vec<_>>();
    if boards.is_empty() {
        return Err(SearchError::Parse("QQ 未返回可用榜单".to_string()));
    }
    Ok(boards)
}
