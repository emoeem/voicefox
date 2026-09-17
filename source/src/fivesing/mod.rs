//! 5sing（酷狗旗下原创音乐平台）音源。
//!
//! - 搜索/歌单搜索：`search.5sing.kugou.com/home/json`，`type=0` 搜歌、`type=1` 搜歌单；
//! - 单曲详情/歌词：`mobileapi.5sing.kugou.com/song/newget`；
//! - 播放地址：`song/getSongUrl`（SQ/HQ/LQ 三档 + 各自的备用地址）；
//! - 歌单曲目：先取歌单元数据拿作者的 userId，再解析歌单页 HTML。
//!
//! 歌曲 ID 由「数字 ID + 类型」组成（类型是 `yc` 原创 / `fc` 翻唱 / `bz` 伴奏），
//! 这里用 `id|type` 的形式表示，与 music-lib 一致。

pub mod api;

use async_trait::async_trait;

use lx_core::model::lyric::LyricData;
use lx_core::model::playlist::Playlist;
use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{
    FetchError, MusicSource, ParsedLink, SearchError, SearchResult, SongUrl, SourceCapabilities,
};

/// 5sing 的直链需要 Referer 才放行，播放与下载都要带上。
const MEDIA_REFERER: &str = "http://5sing.kugou.com/";

pub struct FivesingSource;

impl FivesingSource {
    pub fn new() -> Self {
        Self
    }
}

impl Default for FivesingSource {
    fn default() -> Self {
        Self::new()
    }
}

/// 拆出 `(song_id, song_type)`：优先取 extra，其次解析 `id|type`。
fn split_song_id(song: &SongInfo) -> Option<(String, String)> {
    let id = song
        .extra
        .get("songid")
        .cloned()
        .unwrap_or_else(|| song.id.clone());
    let kind = song
        .extra
        .get("songtype")
        .cloned()
        .or_else(|| song.id.split_once('|').map(|(_, kind)| kind.to_string()))
        .unwrap_or_default();
    let id = id.split('|').next().unwrap_or(&id).to_string();
    if id.is_empty() || kind.is_empty() {
        return None;
    }
    Some((id, kind))
}

#[async_trait]
impl MusicSource for FivesingSource {
    fn id(&self) -> SourceId {
        SourceId::Fivesing
    }

    fn name(&self) -> &str {
        "5sing"
    }

    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities {
            playlists: true,
            playlist_search: true,
            link_parse: true,
            ..Default::default()
        }
    }

    async fn search(
        &self,
        keyword: &str,
        _page: u32,
        _limit: u32,
    ) -> Result<SearchResult, SearchError> {
        let items = api::search_songs(keyword)
            .await
            .map_err(api::ApiError::into_search)?;
        Ok(SearchResult {
            total: items.len() as u32,
            has_more: false,
            items,
        })
    }

    async fn get_song_url(&self, song: &SongInfo, quality: Quality) -> Result<SongUrl, FetchError> {
        let (id, kind) = split_song_id(song).ok_or(FetchError::NotFound)?;
        let link = api::fetch_audio_links(&id, &kind)
            .await
            .map_err(api::ApiError::into_fetch)?;
        // 档位从高到低挑第一个可用的；备用地址作为候选交给下载引擎。
        let picked = link
            .pick(quality)
            .ok_or_else(|| FetchError::Other("5sing 未返回可用地址".to_string()))?;
        Ok(SongUrl {
            url: picked.url.clone(),
            quality: picked.quality,
            duration: song.duration,
            cover_url: song.cover_url.clone(),
            qualities: link.available_qualities(),
            headers: vec![("Referer".to_string(), MEDIA_REFERER.to_string())],
            size: None,
            size_is_advisory: true,
            md5: None,
            candidate_urls: picked.backups.clone(),
            max_chunk_size: 0,
        })
    }

    async fn get_lyric(&self, song: &SongInfo) -> Result<LyricData, FetchError> {
        let (id, kind) = split_song_id(song).ok_or(FetchError::NotFound)?;
        let lyric = api::fetch_lyric(&id, &kind)
            .await
            .map_err(api::ApiError::into_fetch)?
            .ok_or(FetchError::NotFound)?;
        Ok(LyricData {
            lyric,
            ..LyricData::default()
        })
    }

    async fn get_cover_url(&self, song: &SongInfo) -> Result<String, FetchError> {
        Ok(song.cover_url.clone().unwrap_or_default())
    }

    fn supported_qualities(&self) -> Vec<Quality> {
        vec![Quality::Low128, Quality::High320, Quality::Flac]
    }

    async fn search_playlists(
        &self,
        keyword: &str,
        _page: u32,
    ) -> Result<Vec<Playlist>, SearchError> {
        api::search_playlists(keyword)
            .await
            .map_err(api::ApiError::into_search)
    }

    async fn get_playlist_detail(&self, id: &str, _page: u32) -> Result<Vec<SongInfo>, FetchError> {
        let (_, songs) = api::fetch_playlist(id)
            .await
            .map_err(api::ApiError::into_fetch)?;
        Ok(songs)
    }

    async fn parse_link(&self, link: &str) -> Result<ParsedLink, FetchError> {
        match api::link_target(link)
            .ok_or_else(|| FetchError::Other("无法识别 5sing 链接".to_string()))?
        {
            api::FivesingLink::Song { id, kind } => {
                let song = api::fetch_song(&id, &kind)
                    .await
                    .map_err(api::ApiError::into_fetch)?;
                Ok(ParsedLink::Song(Box::new(song)))
            }
            api::FivesingLink::Playlist { id } => {
                let (playlist, songs) = api::fetch_playlist(&id)
                    .await
                    .map_err(api::ApiError::into_fetch)?;
                Ok(ParsedLink::Playlist {
                    playlist: Box::new(playlist),
                    songs,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_song_id_and_type() {
        let mut song = SongInfo::new(
            "12345|yc".to_string(),
            SourceId::Fivesing,
            "歌".to_string(),
            "人".to_string(),
        );
        assert_eq!(
            split_song_id(&song),
            Some(("12345".to_string(), "yc".to_string()))
        );
        // extra 优先于 id 里的组合。
        song.extra.insert("songid".to_string(), "999".to_string());
        song.extra.insert("songtype".to_string(), "fc".to_string());
        assert_eq!(
            split_song_id(&song),
            Some(("999".to_string(), "fc".to_string()))
        );
        // 缺类型时无法解析。
        let plain = SongInfo::new(
            "1".to_string(),
            SourceId::Fivesing,
            "歌".to_string(),
            "人".to_string(),
        );
        assert_eq!(split_song_id(&plain), None);
    }
}
