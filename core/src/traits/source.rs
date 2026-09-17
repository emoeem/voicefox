use async_trait::async_trait;
use std::time::Duration;

use crate::model::leaderboard::LeaderboardInfo;
use crate::model::login::{QrLoginResult, QrLoginSession};
use crate::model::lyric::LyricData;
use crate::model::playlist::{Album, Artist, Playlist, PlaylistCategory};
use crate::model::song::SongInfo;
use crate::model::source::{Quality, SourceId};

/// 搜索结果
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub items: Vec<SongInfo>,
    pub total: u32,
    pub has_more: bool,
}

/// 链接直解的解析结果。
///
/// 对齐 music-lib 的 `Parse` / `ParsePlaylist` / `ParseAlbum`：一个链接可能
/// 指向单曲、歌单或专辑，界面按变体决定是替换当前列表还是进入详情页。
#[derive(Debug, Clone)]
pub enum ParsedLink {
    Song(Box<SongInfo>),
    Playlist {
        playlist: Box<Playlist>,
        songs: Vec<SongInfo>,
    },
    Album {
        playlist: Box<Playlist>,
        songs: Vec<SongInfo>,
    },
}

/// 音源能力声明。
///
/// 接口本身用默认实现表示「不支持」，但界面需要提前知道该不该展示入口
/// （例如没有歌单分类的音源不该出现分类标签栏），因此每个音源显式声明
/// 自己支持哪些能力。字段对齐 music-lib 的 provider 接口拆分。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceCapabilities {
    /// 热门 / 推荐歌单。
    pub playlists: bool,
    /// 按关键词搜索歌单。
    pub playlist_search: bool,
    /// 歌单分类目录（语种 / 风格 / 场景）。
    pub playlist_categories: bool,
    /// 专辑搜索与曲目。
    pub album: bool,
    /// 歌手页（歌手歌曲 / 专辑）。
    pub artist: bool,
    /// 排行榜。
    pub leaderboard: bool,
    /// 链接直解（单曲 / 歌单 / 专辑链接）。
    pub link_parse: bool,
    /// 支持配置登录 cookie。
    pub login: bool,
    /// 支持扫码登录。
    pub qr_login: bool,
    /// 读取账号下的个人歌单（需要登录）。
    pub user_playlists: bool,
    /// 区分 VIP 曲目 / VIP 账号。
    pub vip_account: bool,
}

/// 播放 URL 结果
///
/// 字段命名与 MusicBot-Go 的 `platform.DownloadInfo` 对齐：
/// - `size` / `size_is_advisory`：完整性校验
/// - `md5`：二次校验（部分平台 API 会返回）
/// - `candidate_urls`：备用 CDN 地址，主地址失败时依次尝试
/// - `max_chunk_size`：某些 CDN（如 googlevideo）要求严格有界 Range
#[derive(Debug, Clone, Default)]
pub struct SongUrl {
    pub url: String,
    pub quality: Quality,
    pub duration: Duration,
    pub cover_url: Option<String>,
    pub qualities: Vec<Quality>,
    pub headers: Vec<(String, String)>,
    /// 音源声明的文件大小（字节）。`None` 表示音源未提供或不可靠。
    pub size: Option<u64>,
    /// 为 `true` 时，仅在实际大小**小于**声明值时视为完整性失败；
    /// 音源经常少报字节（如 QQ 音乐 FLAC 少 15 字节）的场景用这个标志。
    pub size_is_advisory: bool,
    /// 音源提供的 MD5 校验值，用于下载后二次校验（目前仅网易云官方
    /// `song/enhance/player/url/v1` 会返回）。
    pub md5: Option<String>,
    /// 备用 CDN 地址。下载引擎在主地址失败时按顺序尝试，并额外为每个地址
    /// 补上网易云 CDN 的 m8/m801/m804/m704 → m7/m701 节点改写。
    pub candidate_urls: Vec<String>,
    /// 某些 CDN（如 googlevideo）拒绝 HEAD、plain GET、open-ended Range
    /// 和超上限的单个 Range 请求，必须严格按此值分片。`0` 表示不启用。
    /// 启用时下载引擎不会做 HEAD 探测，也不会回退到单连接下载，
    /// 因此音源必须同时给出 `size`（否则无法推算有界 Range）。
    pub max_chunk_size: u64,
}

/// 音源统一接口
#[async_trait]
pub trait MusicSource: Send + Sync {
    /// 音源唯一标识
    fn id(&self) -> SourceId;
    /// 音源显示名称
    fn name(&self) -> &str;

    /// 本音源支持的能力，供界面决定入口显隐。
    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities::default()
    }

    /// 搜索歌曲
    async fn search(
        &self,
        keyword: &str,
        page: u32,
        limit: u32,
    ) -> Result<SearchResult, SearchError>;
    /// 获取播放 URL
    async fn get_song_url(&self, song: &SongInfo, quality: Quality) -> Result<SongUrl, FetchError>;
    /// 获取歌词
    async fn get_lyric(&self, song: &SongInfo) -> Result<LyricData, FetchError>;
    /// 获取封面 URL
    async fn get_cover_url(&self, song: &SongInfo) -> Result<String, FetchError>;

    /// 支持的音质列表
    fn supported_qualities(&self) -> Vec<Quality>;

    // --- 可选实现 ---
    /// 解析平台链接（单曲 / 歌单 / 专辑）。不支持链接直解的音源返回错误。
    async fn parse_link(&self, _link: &str) -> Result<ParsedLink, FetchError> {
        Err(FetchError::Other("该音源不支持链接直解".to_string()))
    }
    /// 歌单分类目录。未实现的音源返回空列表。
    async fn get_playlist_categories(&self) -> Result<Vec<PlaylistCategory>, FetchError> {
        Ok(vec![])
    }
    async fn get_playlists(&self, _tag_id: &str, _page: u32) -> Result<Vec<Playlist>, FetchError> {
        Ok(vec![])
    }
    /// 按关键词搜索歌单。未实现的音源返回“不支持”，调用方应回退到热门歌单。
    async fn search_playlists(
        &self,
        _keyword: &str,
        _page: u32,
    ) -> Result<Vec<Playlist>, SearchError> {
        Err(SearchError::Other("该音源不支持歌单搜索".to_string()))
    }
    async fn get_playlist_detail(
        &self,
        _id: &str,
        _page: u32,
    ) -> Result<Vec<SongInfo>, FetchError> {
        Ok(vec![])
    }
    /// 获取歌手歌曲。未实现专用接口的音源会回退到搜索并按歌手过滤。
    async fn get_artist_songs(
        &self,
        artist: &Artist,
        page: u32,
        limit: u32,
    ) -> Result<SearchResult, SearchError> {
        let result = self.search(&artist.name, page, limit).await?;
        let artist_name = normalize_artist_name(&artist.name);
        let items = result
            .items
            .into_iter()
            .filter(|song| {
                normalize_artist_name(&song.singer).contains(&artist_name)
                    || artist_name.contains(&normalize_artist_name(&song.singer))
            })
            .collect::<Vec<_>>();
        Ok(SearchResult {
            total: items.len() as u32,
            has_more: result.has_more,
            items,
        })
    }
    /// 获取歌手专辑。默认从歌手歌曲结果中按专辑去重。
    async fn get_artist_albums(
        &self,
        artist: &Artist,
        page: u32,
        limit: u32,
    ) -> Result<Vec<Album>, SearchError> {
        let result = self.get_artist_songs(artist, page, limit.max(100)).await?;
        let mut albums = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for song in result.items {
            if song.album_name.trim().is_empty() {
                continue;
            }
            let key = if song.album_id.trim().is_empty() {
                format!("name:{}", song.album_name.trim().to_lowercase())
            } else {
                format!("id:{}", song.album_id)
            };
            if !seen.insert(key) {
                continue;
            }
            albums.push(Album {
                id: if song.album_id.trim().is_empty() {
                    song.album_name.clone()
                } else {
                    song.album_id.clone()
                },
                name: song.album_name,
                source: song.source,
                cover_url: song.cover_url,
                artist: artist.name.clone(),
            });
        }
        Ok(albums)
    }
    /// 获取专辑曲目。默认搜索歌手和专辑名，再按专辑标识或名称过滤。
    async fn get_album_songs(
        &self,
        album: &Album,
        page: u32,
        limit: u32,
    ) -> Result<SearchResult, SearchError> {
        let keyword = format!("{} {}", album.artist, album.name);
        let result = self.search(&keyword, page, limit).await?;
        let artist_name = normalize_artist_name(&album.artist);
        let album_name = album.name.trim().to_lowercase();
        let items = result
            .items
            .into_iter()
            .filter(|song| {
                let artist_matches = artist_name.is_empty()
                    || normalize_artist_name(&song.singer).contains(&artist_name);
                let album_matches = (!album.id.trim().is_empty()
                    && !song.album_id.trim().is_empty()
                    && song.album_id == album.id)
                    || (!album_name.is_empty()
                        && song.album_name.trim().to_lowercase() == album_name);
                artist_matches && album_matches
            })
            .collect::<Vec<_>>();
        Ok(SearchResult {
            total: items.len() as u32,
            has_more: result.has_more,
            items,
        })
    }
    async fn get_leaderboard_boards(&self) -> Result<Vec<LeaderboardInfo>, SearchError> {
        Err(SearchError::Other("该音源不支持排行榜".to_string()))
    }
    async fn get_leaderboard(
        &self,
        _id: &str,
        _page: u32,
        _limit: u32,
    ) -> Result<SearchResult, SearchError> {
        Err(SearchError::Other("该音源不支持排行榜".to_string()))
    }
    /// 账号下的个人歌单（需要登录）。未实现的音源返回空列表。
    async fn get_user_playlists(
        &self,
        _page: u32,
        _limit: u32,
    ) -> Result<Vec<Playlist>, FetchError> {
        Ok(vec![])
    }
    /// 创建扫码登录会话。未实现的音源返回错误。
    async fn create_qr_login(&self) -> Result<QrLoginSession, FetchError> {
        Err(FetchError::Other("该音源不支持扫码登录".to_string()))
    }
    /// 轮询扫码状态；登录成功后实现方负责把 cookie 写入本地会话存储。
    async fn check_qr_login(&self, _key: &str) -> Result<QrLoginResult, FetchError> {
        Err(FetchError::Other("该音源不支持扫码登录".to_string()))
    }
    /// 是否为 VIP 账号。未实现或未登录时返回 `false`。
    async fn is_vip_account(&self) -> Result<bool, FetchError> {
        Ok(false)
    }
    /// 退出登录并清除本地 cookie。
    fn logout(&self) -> Result<(), FetchError> {
        Ok(())
    }
    /// 是否已登录（存在可用的会话 cookie）。
    fn is_logged_in(&self) -> bool {
        false
    }
}

fn normalize_artist_name(value: &str) -> String {
    value
        .split(['、', ',', '&', '/', '|'])
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// 搜索错误
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("network error: {0}")]
    Network(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("api error: {0}")]
    Api(String),
    #[error("{0}")]
    Other(String),
}

/// 获取错误
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("network error: {0}")]
    Network(String),
    #[error("not found")]
    NotFound,
    #[error("too many requests")]
    TooManyRequests,
    #[error("parse error: {0}")]
    Parse(String),
    #[error("{0}")]
    Other(String),
}
