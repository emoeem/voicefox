//! musid-dli（crates.io 0.2.0）与 lx-core / lx-source 的模型映射层（V0）+ 双栈
//! 适配源（V1）。
//!
//! feature `musid-dli-adapter` 门控，默认构建（V4 起）带 joox/qianqian 适配边。V0 硬边界：
//! 纯映射函数与 fixture 测试，不接 `SourceManager`。V1（opt-in）在本文件追加
//! [`AdapterMusicSource`]：把 musid-dli provider 包装成 voicefox
//! `MusicSource`（搜索/流地址/歌词三面），由 `SourceManager::register_musid_source`
//! 注册进**独立影子 map**（绝不覆盖内置注册），路由开关见
//! `docs/MUSID_DLI_ADAPTER.md` §11。
//!
//! 契约要点（细节见设计文档）：
//! - 命名互转唯一通道是 `musid_dli::models` 的 namespace 双向表；查不到
//!   （`soda`/`local`/`youtube-music`/`lx-js`/跨语言串名）一律显式报错，
//!   绝不兜底改名；
//! - `TrackId { provider, id }` 拆成 `SongInfo.id + source`：平台原 ID 永不
//!   重编（整合硬要求），专辑 id 不模仿 voicefox 的"缺 ID 回落专辑名"默认；
//! - `StreamInfo -> SongUrl` 的伴随身字段（expires/format/range/结构化备用
//!   端点/降档备注）挂在 [`ResolvedUrl`]，不静默丢；
//! - 质量档位 `Standard -> Low128` 是有损降档、`Unknown` 掉出集合，均以
//!   `notes` 登记；resolve 结果以实际返回档位回填；
//! - strict chunk 契约：`max_chunk_size > 0` 而 `content_length` 缺失时
//!   降级为 0（禁用）并登记，两侧引擎失败语义一致，适配层不掩盖；
//! - md5 门只吃 MD5（或缺省 legacy）摘要，Sha1/Sha256 丢弃并登记；
//! - 备用 URL 顺序契约与 musid-dli `ordered_fallback_urls` 逐字一致
//!   （legacy 先、结构化后、trim、去空、keep-first 去重、primary 不含）。
//!
//! Transport 注入缺口（如实上报）：musid-dli 0.2.0 的 `transport` 模块是
//! `pub(crate)`，`with_transport` 同样未公开，voicefox 的代理/超时预算在
//! 0.2.0 上无法注入；V0 以 [`TransportInjection`] 显式占位记录预算，
//! proxy/超时继续只驱动 voicefox 内置音源。升级前置见设计文档 §4。

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use crate::session::SourceSession;
use lx_core::model::leaderboard::LeaderboardInfo;
use lx_core::model::login::{
    QrLoginKind as LxQrLoginKind, QrLoginResult, QrLoginSession as LxQrLoginSession,
    QrLoginStatus as LxQrLoginStatus,
};
use lx_core::model::lyric::LyricData;
use lx_core::model::playlist::{Album, Artist, Playlist, PlaylistCategory};
use lx_core::model::song::SongInfo;
use lx_core::model::source::{AudioProperties, Quality, SourceId};
use lx_core::traits::source::{
    FetchError, MusicSource, ParsedLink, SearchError, SearchResult, SongUrl, SourceCapabilities,
};
use musid_dli::models::{
    Album as MdAlbum, Artist as MdArtist, AudioFormat, AudioSpec, ByteRange, Chart, ChartEntry,
    ChecksumAlgorithm, FallbackEndpoint, LyricSearchResult as MdLyricSearchResult,
    Playlist as MdPlaylist, PlaylistCategory as MdPlaylistCategory, StreamInfo, StreamQuality,
    Track, TrackId, provider_to_voicefox_source, voicefox_source_to_provider,
};
use musid_dli::provider::{
    AlbumProvider as MdAlbumProvider, CollectionProvider as MdCollectionProvider,
    LinkResolver as MdLinkResolver, LyricsProvider as MdLyricsProvider,
    PlaylistProvider as MdPlaylistProvider, StreamResolver as MdStreamResolver,
    TrackSearch as MdTrackSearch,
};
use musid_dli::provider::{
    AuthCapability, AuthState, ProviderCapabilities, QrLoginKind, QrLoginPoll, QrLoginSession,
    QrLoginStatus, ResolveOptions, SearchPage, SearchQuery,
};
use musid_dli::{Error as MdError, ProviderSession};

/// musid-dli 的 `Track.isrc` 在 voicefox `SongInfo` 上没有专属槽位，落 extra。
pub const EXTRA_ISRC: &str = "isrc";
/// musid-dli `ChartEntry.rank` 的落点（字符串化）。
pub const EXTRA_RANK: &str = "rank";
/// ProviderSession.expires_at 的 extra 键（voicefox 会话无过期槽位）。
pub const EXTRA_EXPIRES_AT_UNIX: &str = "expires_at_unix";
/// ProviderSession.updated_at 的 extra 键。
pub const EXTRA_UPDATED_AT_UNIX: &str = "updated_at_unix";
/// ProviderSession.region 的 extra 键。
pub const EXTRA_REGION: &str = "region";
/// tokens 进 SourceSession.extra 时的键前缀（防与普通 extra 键撞名）。
pub const TOKEN_KEY_PREFIX: &str = "token:";

/// `StreamQuality::Standard` 的降档备注（voicefox 无 Standard 槽位）。
pub const NOTE_QUALITY_STANDARD: &str =
    "quality Standard folded to Low128: voicefox has no Standard slot";
/// `StreamQuality::Unknown` 的降档备注。
pub const NOTE_QUALITY_UNKNOWN: &str =
    "quality Unknown degraded to Low128: voicefox has no Unknown slot";
/// 非 MD5 校验值被丢弃的备注（绝不可喂 md5 门）。
pub const NOTE_CHECKSUM_DROPPED: &str =
    "checksum dropped: Sha1/Sha256 cannot feed the voicefox md5 gate";
/// strict chunk 被禁用的备注（有 chunk 界但无声明 size，两侧引擎都会拒）。
pub const NOTE_STRICT_CHUNK_DISABLED: &str =
    "strict chunk disabled (max_chunk_size=0): source gave a chunk bound without a declared size";

/// 适配层错误：只表达"映射本身失败"，网络/平台错误走 §9 错误映射。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdapterError {
    /// provider id 经 namespace 表查不到 voicefox `SourceId`
    /// （`youtube-music`/`lx-js`，或把 voicefox 名当 provider 名用）。
    #[error("musid-dli provider `{0}` has no voicefox SourceId mapping (namespace table)")]
    UnmappedProvider(String),
    /// 集合模型缺 provider（voicefox 的 source 槽位没有"未声明"状态）。
    #[error(
        "musid-dli collection model lacks a provider id; voicefox source slot has no undeclared state"
    )]
    MissingProvider,
    /// 逐字歌词负载无法解析（musid-dli `Error::InvalidInput` 的适配面）。
    #[error("invalid lyric payload: {0}")]
    InvalidLyric(String),
}

// ---------------------------------------------------------------------------
// namespace（命名互转唯一通道）
// ---------------------------------------------------------------------------

/// voicefox `SourceId` 到 musid-dli provider id。voicefox-only 的
/// `soda`/`local` 返回 `None`。
pub fn source_id_to_provider(source: SourceId) -> Option<&'static str> {
    voicefox_source_to_provider(source.as_str())
}

/// musid-dli provider id 到 voicefox `SourceId`。musid-dli-only 的
/// `youtube-music`/`lx-js`（以及误传 voicefox 名）返回 `None`。
pub fn provider_to_source_id(provider: &str) -> Option<SourceId> {
    source_id_from_name(provider_to_voicefox_source(provider)?)
}

/// voicefox 短名到 `SourceId` 枚举（namespace 表产物的落点）。
fn source_id_from_name(name: &str) -> Option<SourceId> {
    SourceId::all_known()
        .iter()
        .copied()
        .find(|source| source.as_str() == name)
}

/// 必须能映射的版本：查不到即 `AdapterError::UnmappedProvider`。
pub fn require_provider(source: SourceId) -> Result<&'static str, AdapterError> {
    source_id_to_provider(source)
        .ok_or_else(|| AdapterError::UnmappedProvider(source.as_str().to_string()))
}

// ---------------------------------------------------------------------------
// 质量档位
// ---------------------------------------------------------------------------

/// musid-dli `StreamQuality` 到 voicefox `Quality`。
///
/// `Standard` 无 voicefox 档位：降档 `Low128`（有损，调用方按需登记）；
/// `Unknown` 不产档位（返回 `None`）， qualities 集合里直接掉出。
pub fn quality_from_stream(quality: StreamQuality) -> Option<Quality> {
    match quality {
        StreamQuality::Low | StreamQuality::Standard => Some(Quality::Low128),
        StreamQuality::High => Some(Quality::High320),
        StreamQuality::Lossless => Some(Quality::Flac),
        StreamQuality::HiRes => Some(Quality::Flac24),
        StreamQuality::Unknown => None,
    }
}

/// voicefox `Quality` 到 musid-dli `StreamQuality`（无歧义反向）。
pub fn stream_quality_from_quality(quality: Quality) -> StreamQuality {
    match quality {
        Quality::Low128 => StreamQuality::Low,
        Quality::High320 => StreamQuality::High,
        Quality::Flac => StreamQuality::Lossless,
        Quality::Flac24 => StreamQuality::HiRes,
    }
}

// ---------------------------------------------------------------------------
// 实测编码参数（键对键零换名）
// ---------------------------------------------------------------------------

/// musid-dli `AudioSpec` 到 voicefox `AudioProperties`，字段同名同义。
pub fn audio_spec_to_properties(spec: AudioSpec) -> AudioProperties {
    AudioProperties {
        bitrate: spec.bitrate,
        sample_rate: spec.sample_rate,
        bit_depth: spec.bit_depth,
        lossless: spec.lossless,
    }
}

/// voicefox `AudioProperties` 到 musid-dli `AudioSpec`（反向同构）。
pub fn audio_properties_to_spec(properties: AudioProperties) -> AudioSpec {
    AudioSpec {
        bitrate: properties.bitrate,
        sample_rate: properties.sample_rate,
        bit_depth: properties.bit_depth,
        lossless: properties.lossless,
    }
}

// ---------------------------------------------------------------------------
// Track <-> SongInfo
// ---------------------------------------------------------------------------

/// 歌手列表到 voicefox 单串（`、` join）。
pub fn artists_to_singer(artists: &[String]) -> String {
    artists.join("、")
}

/// voicefox 歌手串切回列表：只按 `'、'` 切（voicefox `SongInfo.singer` 的
/// 约定格式是"歌手A、歌手B"，适配层自己的 `artists_to_singer` 也只用 `'、'`
/// join，因此 `'、'` 单字符切分对适配层产物是无损往返）。
///
/// voicefox `normalize_artist_name` 的归一集合（`、 , & / |`，
/// `core/src/traits/source.rs:303-311`）只用于匹配归一，从不用于重构歌手
/// 列表：拿全集切分会把 "Earth, Wind & Fire" 撕成 3 个、"AC/DC" 撕成 2 个，
/// 破坏 `Track.artists` 身份数据（musid-dli matcher 评分与 tag_write 都
/// 消费它）。歌手名本身含 `'、'` 时在 voicefox 单串模型里本就无法区分，
/// 这是固有歧义（设计文档 §3 缺口 6），与切分集无关。
pub fn singer_to_artists(singer: &str) -> Vec<String> {
    singer
        .split('、')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

/// musid-dli `Album` 到 voicefox `Playlist` 形（V4-s1 parse_link 的 album
/// 分支与 legacy 同构：专辑以歌单形状进详情页）。源经 `Album.id.provider`
/// namespace 表推（查不到为 None → 落装配点源由调用方决定，此处直接报
/// UnmappedProvider 与其它转换同口径）。`artists` 为 String 元素，首位落
/// creator 槽（legacy 专辑分支以首个艺人充当，artwork/曲目数同槽位映射）。
pub fn album_to_lx_playlist(album: &MdAlbum) -> Result<Playlist, AdapterError> {
    let source = provider_to_source_id(&album.id.provider)
        .ok_or_else(|| AdapterError::UnmappedProvider(album.id.provider.clone()))?;
    Ok(Playlist {
        id: album.id.id.clone(),
        name: album.title.clone(),
        source,
        cover_url: album.artwork_url.clone(),
        song_count: album.track_count.unwrap_or(0),
        description: None,
        play_count: None,
        creator: album.artists.first().cloned(),
        link: None,
        extra: std::collections::HashMap::new(),
    })
}

/// musid-dli `Track` 到 voicefox `SongInfo`。
///
/// provider 经 namespace 表转 `SourceId`，查不到报错（绝不改名）；`id` 原样
/// 透传；`isrc` 落 `extra["isrc"]`（反向会还原进 `isrc` 槽位）。
pub fn track_to_song(track: &Track) -> Result<SongInfo, AdapterError> {
    let source = provider_to_source_id(&track.id.provider)
        .ok_or_else(|| AdapterError::UnmappedProvider(track.id.provider.clone()))?;
    let mut song = SongInfo::new(
        track.id.id.clone(),
        source,
        track.title.clone(),
        artists_to_singer(&track.artists),
    );
    song.album_name = track.album.clone().unwrap_or_default();
    song.album_id = track.album_id.clone().unwrap_or_default();
    song.duration = track.duration.unwrap_or(Duration::ZERO);
    song.cover_url = track.artwork_url.clone();
    song.qualities = track
        .sorted_qualities()
        .into_iter()
        .filter_map(quality_from_stream)
        .collect();
    song.audio = track.audio.map(audio_spec_to_properties);
    song.extra = track
        .extra
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    if let Some(isrc) = &track.isrc {
        song.extra.insert(EXTRA_ISRC.to_string(), isrc.clone());
    }
    Ok(song)
}

/// voicefox `SongInfo` 到 musid-dli `Track`（反向）。
///
/// provider 来自歌曲的 `source` 字段（不推断）；空串按 musid-dli 语义折回
/// `None`；`ZERO` 时长折回 `None`；`extra["isrc"]` 还原进 `isrc` 槽位。
pub fn song_to_track(song: &SongInfo) -> Result<Track, AdapterError> {
    let provider = require_provider(song.source)?.to_string();
    let mut extra: BTreeMap<String, String> = song
        .extra
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let isrc = extra.remove(EXTRA_ISRC);
    Ok(Track {
        id: TrackId::new(provider, song.id.clone()),
        title: song.name.clone(),
        artists: singer_to_artists(&song.singer),
        album: (!song.album_name.trim().is_empty()).then(|| song.album_name.clone()),
        album_id: (!song.album_id.trim().is_empty()).then(|| song.album_id.clone()),
        duration: (song.duration != Duration::ZERO).then_some(song.duration),
        artwork_url: song.cover_url.clone(),
        isrc,
        available_qualities: song
            .qualities
            .iter()
            .copied()
            .map(stream_quality_from_quality)
            .collect(),
        audio: song.audio.map(audio_properties_to_spec),
        extra,
    })
}

// ---------------------------------------------------------------------------
// StreamInfo -> SongUrl（+ 伴随字段）
// ---------------------------------------------------------------------------

/// musid-dli `StreamInfo` 的 voicefox 映射产物与旁路元数据。
///
/// voicefox `SongUrl` 没有槽位的 musid-dli 语义都挂在这里，绝不静默丢：
/// 过期时间（G4）、容器格式、DASH 描述范围（G3）、结构化备用端点逐条元数据、
/// 以及所有显式降档/丢弃登记。`SongUrl` 未实现 `PartialEq`，因此本结构
/// 只派生 `Debug/Clone`；逐字段断言由测试自行完成。
#[derive(Debug, Clone)]
pub struct ResolvedUrl {
    /// 主映射产物；`quality` 以实际返回档位回填。
    pub url: SongUrl,
    /// unix 秒：URL 过期时刻（musid-dli `StreamCache` 的缓存键语义）。
    /// 播放器可据此提前重解析；适配层不缓存过期值。
    pub expires_at_unix: Option<u64>,
    /// 容器格式（下载/NCM 场景才需要；播放路径不声明容器格式）。
    pub format: AudioFormat,
    /// DASH 描述元数据（G3：两侧下载引擎都不消费）。
    pub index_range: Option<ByteRange>,
    /// DASH 描述元数据（G3）。
    pub init_range: Option<ByteRange>,
    /// 结构化备用端点（顺序与 `SongUrl.candidate_urls` 一致；legacy 字符串
    /// 被提升为无元数据的端点）。per-candidate headers/expiry/size/quality
    /// 留给 V1 下载引擎切片消费。
    pub fallback_endpoints: Vec<FallbackEndpoint>,
    /// 显式降档/丢弃登记（Standard 降档、Unknown 降档、SHA 校验丢弃、
    /// strict chunk 禁用）。
    pub notes: Vec<&'static str>,
}

/// musid-dli `StreamInfo` 到 voicefox `SongUrl`（含伴随字段）。
///
/// 本函数恒成功：所有"voicefox 无槽位"的信息都进 [`ResolvedUrl`] 旁路，
/// 所有降档决定都进 `notes`。
pub fn stream_to_resolved_url(info: &StreamInfo) -> ResolvedUrl {
    let mut notes: Vec<&'static str> = Vec::new();
    let quality = match info.quality {
        StreamQuality::Unknown => {
            notes.push(NOTE_QUALITY_UNKNOWN);
            Quality::Low128
        }
        StreamQuality::Standard => {
            notes.push(NOTE_QUALITY_STANDARD);
            Quality::Low128
        }
        StreamQuality::Low => Quality::Low128,
        StreamQuality::High => Quality::High320,
        StreamQuality::Lossless => Quality::Flac,
        StreamQuality::HiRes => Quality::Flac24,
    };
    // md5 门：musid-dli 的 `md5_checksum()` 只放行缺省（legacy=MD5）与显式
    // MD5，且先 trim（空白填充可过校验、纯空白跳过）。Sha1/Sha256 在这里
    // 丢弃并登记，绝不可把 SHA 值误喂 md5 门。
    let md5 = match info.checksum_algorithm {
        Some(ChecksumAlgorithm::Sha1) | Some(ChecksumAlgorithm::Sha256) => {
            if info
                .checksum
                .as_deref()
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false)
            {
                notes.push(NOTE_CHECKSUM_DROPPED);
            }
            None
        }
        _ => info.md5_checksum().map(str::to_owned),
    };
    // strict chunk 契约：voicefox 侧"启用必须同时给 size"；musid-dli 引擎
    // 对同样的组合直接 fail。这里禁用（0）并登记，让调用方显式看到降级。
    let max_chunk_size = match info.max_chunk_size {
        Some(size) if size > 0 && info.content_length.is_none() => {
            notes.push(NOTE_STRICT_CHUNK_DISABLED);
            0
        }
        Some(size) => size,
        None => 0,
    };
    let url = SongUrl {
        url: info.url.clone(),
        quality,
        duration: info.duration.unwrap_or(Duration::ZERO),
        // musid-dli `StreamInfo` 上没有 cover_url 与"全部可用档位"的来源
        // 字段：voicefox 内置源两处都从歌曲模型补齐（wy/url.rs、kg/url.rs；
        // 封面有真实消费面 app/src/download/manager.rs 封面嵌入、
        // application/src/services/playback.rs 播放侧），这里先按缺省留空，
        // 由调用点用 [`enrich_song_url_from_song`] 补齐（缺口登记见设计
        // 文档 §2.5/§3 第 13、14 条）。
        cover_url: None,
        qualities: Vec::new(),
        headers: info.headers.clone(),
        size: info.content_length,
        size_is_advisory: info.size_is_advisory,
        md5,
        candidate_urls: info.ordered_fallback_urls(),
        max_chunk_size,
    };
    ResolvedUrl {
        url,
        expires_at_unix: info.expires_at_unix,
        format: info.format,
        index_range: info.index_range,
        init_range: info.init_range,
        fallback_endpoints: info.ordered_fallback_endpoints(),
        notes,
    }
}

/// voicefox 内置源 producer 行为对齐：`SongUrl.cover_url` 与
/// `SongUrl.qualities` 在 musid-dli `StreamInfo` 上没有来源字段（缺口登记
/// 见设计文档 §3 第 13、14 条），voicefox 内置源从歌曲模型补齐
/// （`source/src/wy/url.rs`、`source/src/kg/url.rs`；封面消费面：
/// `app/src/download/manager.rs` 封面嵌入优先 `url.cover_url`、
/// `application/src/services/playback.rs` 播放侧同样回落）。V1 双栈接入时
/// 在 `get_song_url` 调用点用本函数补齐；V0 只提供映射本身，不接调用点。
pub fn enrich_song_url_from_song(url: &mut SongUrl, song: &SongInfo) {
    if url.cover_url.is_none() {
        url.cover_url = song.cover_url.clone();
    }
    url.qualities = song.qualities.iter().copied().collect();
}

/// voicefox `SongUrl` 到 musid-dli `StreamInfo`（下载组装方向的反向映射，
/// `musid_dli::download::download_request_from_stream` 的入参前一步）。
///
/// `SongUrl.md5` 是 voicefox 唯一承载的算法（MD5），因此
/// `checksum_algorithm` 恒用缺省（`None` = legacy MD5 语义）；`0` 的
/// `max_chunk_size` 折回 `None`（off）；`ZERO` 时长折回 `None`。
pub fn song_url_to_stream(url: &SongUrl) -> StreamInfo {
    StreamInfo {
        url: url.url.clone(),
        quality: stream_quality_from_quality(url.quality),
        // voicefox 侧不声明容器格式；musid-dli 消费方按 Unknown 处理。
        format: AudioFormat::Unknown,
        duration: (url.duration != Duration::ZERO).then_some(url.duration),
        expires_at_unix: None,
        content_length: url.size,
        checksum: url.md5.clone(),
        checksum_algorithm: None,
        headers: url.headers.clone(),
        fallback_urls: url.candidate_urls.clone(),
        fallback_endpoints: Vec::new(),
        index_range: None,
        init_range: None,
        max_chunk_size: (url.max_chunk_size > 0).then_some(url.max_chunk_size),
        size_is_advisory: url.size_is_advisory,
    }
}

// ---------------------------------------------------------------------------
// 集合模型
// ---------------------------------------------------------------------------

/// musid-dli `Playlist` 到 voicefox `Playlist`。
///
/// `track_count: None` 折成 `song_count: 0`（voicefox 无"未知计数"表达，
/// UI 层注意 0 与未知同形）；`tracks` 不跨边界（voicefox 按页拉取）。
pub fn playlist_to_lx(playlist: &MdPlaylist) -> Result<Playlist, AdapterError> {
    let source = provider_to_source_id(&playlist.id.provider)
        .ok_or_else(|| AdapterError::UnmappedProvider(playlist.id.provider.clone()))?;
    Ok(Playlist {
        id: playlist.id.id.clone(),
        name: playlist.title.clone(),
        source,
        cover_url: playlist.artwork_url.clone(),
        song_count: playlist.track_count.unwrap_or(0),
        description: playlist.description.clone(),
        play_count: playlist.play_count,
        creator: playlist.creator.clone(),
        link: playlist.link.clone(),
        extra: playlist
            .extra
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    })
}

/// musid-dli `PlaylistCategory` 到 voicefox `PlaylistCategory`。
///
/// `provider` 是必填：voicefox 的 source 槽位没有"未声明"状态。缺失报
/// `AdapterError::MissingProvider`，存在但经 namespace 表查不到报
/// `AdapterError::UnmappedProvider`（两者分开，不选默认源，避免静默挂错源）。
pub fn playlist_category_to_lx(
    category: &MdPlaylistCategory,
) -> Result<PlaylistCategory, AdapterError> {
    let Some(provider) = category.provider.as_deref() else {
        return Err(AdapterError::MissingProvider);
    };
    let source = provider_to_source_id(provider)
        .ok_or_else(|| AdapterError::UnmappedProvider(provider.to_string()))?;
    Ok(PlaylistCategory {
        id: category.id.clone(),
        name: category.name.clone(),
        source,
        group: category.group.clone(),
        count: category.count,
        hot: category.hot,
        extra: category
            .extra
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    })
}

/// musid-dli `Album` 到 voicefox `Album`。
///
/// `artists: Vec` 合并成单串 `artist`（`、` join，多歌手信息有损，反向只能
/// 整串）；`track_count` 无 voicefox 槽位，丢弃。
pub fn album_to_lx(album: &MdAlbum) -> Result<Album, AdapterError> {
    let source = provider_to_source_id(&album.id.provider)
        .ok_or_else(|| AdapterError::UnmappedProvider(album.id.provider.clone()))?;
    Ok(Album {
        id: album.id.id.clone(),
        name: album.title.clone(),
        source,
        cover_url: album.artwork_url.clone(),
        artist: artists_to_singer(&album.artists),
    })
}

/// musid-dli `Artist` 到 voicefox `Artist`（1:1）。
pub fn artist_to_lx(artist: &MdArtist) -> Result<Artist, AdapterError> {
    let source = provider_to_source_id(&artist.id.provider)
        .ok_or_else(|| AdapterError::UnmappedProvider(artist.id.provider.clone()))?;
    Ok(Artist {
        id: artist.id.id.clone(),
        name: artist.name.clone(),
        source,
        cover_url: artist.artwork_url.clone(),
    })
}

/// musid-dli `Chart` 到 voicefox `LeaderboardInfo`。
///
/// `updated_description`（"每日更新"）即 voicefox 的 `update` 字段；
/// `description` 是自由文本，voicefox 无槽位，丢弃。
pub fn chart_to_leaderboard(chart: &Chart) -> Result<LeaderboardInfo, AdapterError> {
    let source = provider_to_source_id(&chart.id.provider)
        .ok_or_else(|| AdapterError::UnmappedProvider(chart.id.provider.clone()))?;
    Ok(LeaderboardInfo {
        id: chart.id.id.clone(),
        name: chart.title.clone(),
        source,
        update: chart.updated_description.clone(),
    })
}

/// musid-dli `ChartEntry` 到 voicefox `SongInfo`（track 走 §2.2 映射，
/// `rank` 字符串化进 `extra["rank"]`）。
pub fn chart_entry_to_song(entry: &ChartEntry) -> Result<SongInfo, AdapterError> {
    let mut song = track_to_song(&entry.track)?;
    song.extra
        .insert(EXTRA_RANK.to_string(), entry.rank.to_string());
    Ok(song)
}

/// musid-dli `SearchPage` 到 voicefox `SearchResult`。
///
/// `total: None` 按 items 数收敛（避免"有结果却显示 0 条"）；`Some` 收敛到
/// u32。0 与未知同形的缺口照实保留（`SearchResult` 无 extra 槽位可登记）；
/// 任一 item 的 provider 查不到即整体报错（不静默丢行）。
pub fn search_page_to_result(page: &SearchPage) -> Result<SearchResult, AdapterError> {
    let items = page
        .items
        .iter()
        .map(track_to_song)
        .collect::<Result<Vec<_>, _>>()?;
    let total = page
        .total
        .unwrap_or(page.items.len() as u64)
        .min(u64::from(u32::MAX)) as u32;
    Ok(SearchResult {
        items,
        total,
        has_more: page.has_more,
    })
}

// ---------------------------------------------------------------------------
// 错误映射（§9 速查表的实现）
// ---------------------------------------------------------------------------

/// musid-dli `Error` 到 voicefox `FetchError`。
///
/// 文本取 `Error` 的 `Display`（`ErrorDetail` 是消毒过的分类文本，可安全
/// 上屏；消毒契约：不得把 voicefox 侧原始 reqwest 错误塞进 `ErrorDetail`）。
/// `SearchError`/`FetchError` 没有登录类变体，登录/风控/版权类一律
/// `Other`（musid-dli `Display` 是英文分类句，如 `"login required"`、
/// `"vip membership required"`；适配层透传原文，中文展示由 voicefox UI
/// 层按语义变体翻译，不能按中文"登录"子串分流）。
pub fn fetch_error_from_error(error: &MdError) -> FetchError {
    let message = error.to_string();
    match error.category() {
        musid_dli::error::ErrorCategory::Network
        | musid_dli::error::ErrorCategory::Timeout
        | musid_dli::error::ErrorCategory::Unavailable
        | musid_dli::error::ErrorCategory::Io => FetchError::Network(message),
        musid_dli::error::ErrorCategory::RateLimited => FetchError::TooManyRequests,
        musid_dli::error::ErrorCategory::NotFound
        | musid_dli::error::ErrorCategory::ExpiredResource => FetchError::NotFound,
        musid_dli::error::ErrorCategory::InvalidResponse
        | musid_dli::error::ErrorCategory::Serialization => FetchError::Parse(message),
        musid_dli::error::ErrorCategory::Unsupported
        | musid_dli::error::ErrorCategory::UnsupportedOperation
        | musid_dli::error::ErrorCategory::UnsupportedQuality => {
            FetchError::Other(format!("该音源不支持：{message}"))
        }
        _ => FetchError::Other(message),
    }
}

/// musid-dli `Error` 到 voicefox `SearchError`。
///
/// 与 [`fetch_error_from_error`] 同表；`SearchError` 无 `NotFound`/
/// `TooManyRequests` 变体，这两类以带文本的 `Other` 透传。
pub fn search_error_from_error(error: &MdError) -> SearchError {
    let message = error.to_string();
    match error.category() {
        musid_dli::error::ErrorCategory::Network
        | musid_dli::error::ErrorCategory::Timeout
        | musid_dli::error::ErrorCategory::Unavailable
        | musid_dli::error::ErrorCategory::Io => SearchError::Network(message),
        musid_dli::error::ErrorCategory::RateLimited => SearchError::Other(message),
        musid_dli::error::ErrorCategory::NotFound
        | musid_dli::error::ErrorCategory::ExpiredResource => SearchError::Other(message),
        musid_dli::error::ErrorCategory::InvalidResponse => SearchError::Api(message),
        musid_dli::error::ErrorCategory::Serialization => SearchError::Parse(message),
        musid_dli::error::ErrorCategory::Unsupported
        | musid_dli::error::ErrorCategory::UnsupportedOperation
        | musid_dli::error::ErrorCategory::UnsupportedQuality => {
            SearchError::Other(format!("该音源不支持：{message}"))
        }
        _ => SearchError::Other(message),
    }
}

// ---------------------------------------------------------------------------
// 能力位 / 登录态
// ---------------------------------------------------------------------------

/// voicefox 独有能力位的判定输入（musid-dli 侧靠"接口是否存在"决定）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderFaceFlags {
    /// provider 实现了 `PlaylistProvider::search_playlists`。
    pub has_search_playlists: bool,
    /// provider 的 `categories()` 不会返回 Unsupported。
    pub has_categories: bool,
    /// provider 实现了 `PlaylistProvider::user_playlists`。
    pub has_user_playlists: bool,
}

/// musid-dli `ProviderCapabilities` + `AuthCapability` 到 voicefox
/// `SourceCapabilities`。
///
/// 粒度表（M3 §5.4 pin + 本适配层的机制细分）：`track_search`/
/// `stream_resolve`/`lyrics` 在 voicefox 无位（内置音源恒有）；
/// `wechat_login`/`vip_account` 在 musid-dli 无数据源，双栈期恒 false；
/// `download` 位 registry 不外露、`artwork` 位 voicefox 无对应位，均忽略。
///
/// `login` 位的语义是"支持配置登录 cookie"（`core/src/traits/source.rs:57-58`），
/// 因此只对 cookie 类机制置 true：`capabilities.authentication`（传统
/// authentication 位，`auth_capability()` 缺省把它折叠成
/// `AuthCapability::Cookie`）或显式 `AuthCapability::Cookie`。
/// **矛盾声明的处理约定**：`authentication=true` 与覆写 `auth_capability()`
/// 为非 Cookie 机制同时出现时按 authentication 位胜出（musid-dli 侧该
/// bool 的语义即"Cookie session 是真实机制"）；当前 12 个 provider 无此
/// 矛盾组合（kuwo 匿名 token 场景 authentication=false + Anonymous、
/// fivesing false + None，musid-dli source_kuwo.rs:1918-1941、
/// source_fivesing.rs:494-511 有测试 pin），V1 调用点如遇矛盾组合须先在
/// musid-dli 侧修 descriptor 再接线。
/// `Anonymous`（kuwo web token / migu sid 式匿名设备会话）、`OAuth`、
/// `Password`、`Token` 不置 login：这些机制在 voicefox 没有 cookie 配置
/// 入口语义，误置 true 会让设置页出现对不上的登录入口（V1 统一登录时再
/// 评估是否上浮这些机制）。
pub fn capabilities_from_provider(
    capabilities: ProviderCapabilities,
    auth: AuthCapability,
    flags: ProviderFaceFlags,
) -> SourceCapabilities {
    SourceCapabilities {
        playlists: capabilities.playlists,
        playlist_search: flags.has_search_playlists,
        playlist_categories: flags.has_categories,
        album: capabilities.albums,
        artist: capabilities.artists,
        leaderboard: capabilities.charts,
        link_parse: capabilities.link_resolve,
        login: capabilities.authentication || auth == AuthCapability::Cookie,
        qr_login: auth == AuthCapability::Qr,
        // musid-dli 没有 WeChat 扫码入口位（QrLoginKind::WeChat 只出现在
        // session.kind）；双栈期微信扫码仍走 voicefox 内置 tx 源。
        wechat_login: false,
        user_playlists: flags.has_user_playlists,
        // musid-dli 无对应接口。
        vip_account: false,
    }
}

/// musid-dli `AuthState` 七态到 voicefox 布尔登录位。
///
/// "有可用会话"为 true（`Authenticated`/`Refreshing`）；过期/失效/要求
/// 重授权为 false（由 voicefox UI 触发重新登录）。`AuthState` 状态机本身
/// 是 musid-dli 内部资产，不引入 voicefox。
pub fn is_logged_in_from_auth_state(state: AuthState) -> bool {
    matches!(state, AuthState::Authenticated | AuthState::Refreshing)
}

// ---------------------------------------------------------------------------
// 会话（账号/会话身份保留硬要求）
// ---------------------------------------------------------------------------

/// musid-dli `ProviderSession` 到 voicefox `SourceSession`。
///
/// `cookies` 直通；`tokens` 进 `extra`（键加 `token:` 前缀防撞）；
/// `display_name` 到 `user_name`、`user_id` 直通（musid-dli 两字段分存，
/// voicefox `user_name` 只存 display_name 值，身份不丢）；
/// `expires_at`/`updated_at`/`region` 进 `extra` 自定义键。
pub fn source_session_from_provider(session: &ProviderSession) -> SourceSession {
    let mut extra = BTreeMap::new();
    for (name, token) in &session.tokens {
        extra.insert(format!("{TOKEN_KEY_PREFIX}{name}"), token.clone());
    }
    if let Some(expires_at) = session.expires_at {
        extra.insert(EXTRA_EXPIRES_AT_UNIX.to_string(), expires_at.to_string());
    }
    if let Some(updated_at) = session.updated_at {
        extra.insert(EXTRA_UPDATED_AT_UNIX.to_string(), updated_at.to_string());
    }
    if let Some(region) = &session.region {
        extra.insert(EXTRA_REGION.to_string(), region.clone());
    }
    SourceSession {
        cookies: session.cookies.clone(),
        extra,
        user_name: session.display_name.clone(),
        user_id: session.user_id.clone(),
    }
}

/// voicefox `SourceSession` 到 musid-dli `ProviderSession`（反向）。
///
/// `token:` 前缀键还原进 `tokens`；`expires_at_unix`/`updated_at_unix`/
/// `region` 还原进对应槽位；其余 extra 键在 musid-dli 侧没有通用槽位，
/// 无法表达，丢弃（voicefox 侧副本不丢，双栈期两份会话各自保真）。
pub fn provider_session_from_source_session(session: &SourceSession) -> ProviderSession {
    let mut tokens = BTreeMap::new();
    let mut expires_at = None;
    let mut updated_at = None;
    let mut region = None;
    for (key, value) in &session.extra {
        if let Some(name) = key.strip_prefix(TOKEN_KEY_PREFIX) {
            tokens.insert(name.to_string(), value.clone());
        } else if key == EXTRA_EXPIRES_AT_UNIX {
            expires_at = value.parse().ok();
        } else if key == EXTRA_UPDATED_AT_UNIX {
            updated_at = value.parse().ok();
        } else if key == EXTRA_REGION {
            region = Some(value.clone());
        }
    }
    ProviderSession {
        cookies: session.cookies.clone(),
        tokens,
        user_id: session.user_id.clone(),
        display_name: session.user_name.clone(),
        expires_at,
        updated_at,
        region,
    }
}

// ---------------------------------------------------------------------------
// 扫码登录（G8/G9 关闭后零转换面）
// ---------------------------------------------------------------------------

/// musid-dli `QrLoginStatus` 到 voicefox `QrLoginStatus`：九态逐一同名，
/// `is_transient`/`is_terminal` 两侧逐字一致，逐变体搬运。
pub fn qr_login_status(status: QrLoginStatus) -> LxQrLoginStatus {
    match status {
        QrLoginStatus::Waiting => LxQrLoginStatus::Waiting,
        QrLoginStatus::Scanned => LxQrLoginStatus::Scanned,
        QrLoginStatus::Success => LxQrLoginStatus::Success,
        QrLoginStatus::Expired => LxQrLoginStatus::Expired,
        QrLoginStatus::RiskControl => LxQrLoginStatus::RiskControl,
        QrLoginStatus::ServerError => LxQrLoginStatus::ServerError,
        QrLoginStatus::Failed => LxQrLoginStatus::Failed,
        QrLoginStatus::NetworkError => LxQrLoginStatus::NetworkError,
        QrLoginStatus::InvalidSession => LxQrLoginStatus::InvalidSession,
    }
}

/// musid-dli `QrLoginKind` 到 voicefox `QrLoginKind`（wire 一致，零转换）。
pub fn qr_login_kind(kind: QrLoginKind) -> LxQrLoginKind {
    match kind {
        QrLoginKind::Standard => LxQrLoginKind::Standard,
        QrLoginKind::WeChat => LxQrLoginKind::WeChat,
    }
}

/// musid-dli `QrLoginSession` 到 voicefox `QrLoginSession`。
///
/// 唯一差异是 voicefox 多 `source`：由发起 provider 的 id 经 namespace 表
/// 回填；`youtube-music`/`lx-js` 无映射时报错（该对不可用）。
pub fn qr_login_session_from_md(
    session: &QrLoginSession,
    provider: &str,
) -> Result<LxQrLoginSession, AdapterError> {
    let source = provider_to_source_id(provider)
        .ok_or_else(|| AdapterError::UnmappedProvider(provider.to_string()))?;
    Ok(LxQrLoginSession {
        source,
        kind: qr_login_kind(session.kind),
        key: session.key.clone(),
        url: session.url.clone(),
        image_png: session.image_png.clone(),
        expires_in: session.expires_in,
    })
}

/// musid-dli `QrLoginPoll` 到 voicefox `QrLoginResult`。
///
/// `user_name` 用 `QrLoginPoll::user_name()` 折叠（display_name 优先回退
/// user_id，两者皆 None 保持 None，绝不折叠成空串）。cookie 永不经此对象。
pub fn qr_login_result_from_poll(poll: &QrLoginPoll) -> QrLoginResult {
    QrLoginResult {
        status: qr_login_status(poll.status),
        message: poll.message.clone(),
        user_name: poll.user_name(),
    }
}

// ---------------------------------------------------------------------------
// Transport 注入占位（硬发现的如实上报面）
// ---------------------------------------------------------------------------

/// voicefox 代理/超时预算的显式占位。
///
/// musid-dli 0.2.0 的 `transport` 模块是 `pub(crate)`（其 `src/lib.rs:40-54`），
/// provider 的 `with_transport` 同样未公开：voicefox 的代理/超时在 0.2.0 上
/// **无法注入**。本结构只记录宿主预算（设置页/诊断可读），不做任何注入；
/// 上游公开 transport 接缝后这里升级为真正的构造函数
/// （`docs/MUSID_DLI_ADAPTER.md` §4/§8）。proxy/超时当前只驱动 voicefox
/// 内置音源（`lx_source::http::configure`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportInjection {
    proxy_url: Option<String>,
    timeout_secs: u64,
}

impl TransportInjection {
    /// 记录 voicefox 的代理与总超时预算（与 `lx_source::http::configure`
    /// 的入参同形）。
    pub fn new(proxy_url: Option<String>, timeout_secs: u64) -> Self {
        Self {
            proxy_url,
            timeout_secs,
        }
    }

    /// 已记录的代理地址（`None` = 不走代理）。
    pub fn proxy_url(&self) -> Option<&str> {
        self.proxy_url.as_deref()
    }

    /// 已记录的总超时（秒）。
    pub fn timeout_secs(&self) -> u64 {
        self.timeout_secs
    }
}

// ---------------------------------------------------------------------------
// V1 双栈适配源（AdapterMusicSource：musid-dli provider -> voicefox MusicSource）
// ---------------------------------------------------------------------------

/// 当前 unix 秒（过期 URL 判定用）。
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

/// 歌曲到 musid `Track` 的入参保真映射：`song.source` 直译；JS 音源搜到的
/// 歌（`source=Local` + `extra[source]` 平台标记）按平台标记还原 provider
/// （其 `id` 即该平台的歌曲 id）；真本地文件歌曲（文件路径当 id、无平台
/// 标记）在 manager 侧就早退不会路由到 musid，这里防御性报错（绝不把
/// 文件路径当平台 id 发出去）。
fn adapter_track_for_song(song: &SongInfo) -> Result<Track, AdapterError> {
    if song.source != SourceId::Local {
        return song_to_track(song);
    }
    let Some(source) = crate::manager::builtin_source_for_js(song) else {
        return Err(AdapterError::UnmappedProvider("local".to_string()));
    };
    let mut mapped = song.clone();
    mapped.source = source;
    song_to_track(&mapped)
}

/// musid-dli provider 到 voicefox `MusicSource` 的适配源（V1 双栈影子源）。
///
/// 影子语义（docs §11，P2/P10 口径）：
/// - 不参与健康检测、capabilities 聚合、设置页入口显隐与登录链路——
///   `SourceManager` 只把本包装放进独立影子 map；
/// - `capabilities()` 仅回填 provider descriptor 位（诊断对照），
///   `SourceManager::capabilities` 不读影子 map；
/// - 登录/会话四面（扫码、cookie、logout）恒走旧路径：本实现不覆写任何
///   登录方法（保持 trait 默认错误）、`is_logged_in` 恒 false，musid-dli
///   `ProviderSession`/`FileSessionStore` 不接线（V2 按 §5 步骤 2 再接）；
/// - 链接直解不覆写：host-side 域路由保留在 voicefox（§10）；
/// - 流地址面按 §2.4 显式传档位（禁用 `ResolveOptions::default()=High`），
///   产物经 `stream_to_resolved_url` + `enrich_song_url_from_song` 补齐
///   cover_url/qualities（§3 第 13、14 条），`expires_at_unix` 已过期的
///   地址视为失败（G4：绝不透传播放器），降档/丢弃 notes 走 debug 日志；
/// - 歌词面复用 `lyric_document_to_lx_data`（lxlyric 恒 None，§2.10），
///   需要 `musid-dli-adapter-lyrics` 变体。
pub struct AdapterMusicSource {
    source: SourceId,
    display_name: String,
    search: Option<Arc<dyn MdTrackSearch>>,
    stream: Option<Arc<dyn MdStreamResolver>>,
    /// 歌词面 provider：歌词转换变体（`musid-dli-adapter-lyrics`）关闭时
    /// 不读——V1 歌词面依赖该变体的 `lyric_document_to_lx_data` 归一转换，
    /// 缺变体时歌词面不生效（启动 warn 由 bootstrap 负责）。V4 起该变体
    /// 进 lx-source default（joox/qianqian 主注册表形态的歌词面回归修复，
    /// 优化轮 P1-1）。
    #[cfg_attr(not(feature = "musid-dli-adapter-lyrics"), expect(dead_code))]
    lyrics: Option<Arc<dyn MdLyricsProvider>>,
    /// 歌词文档缓存（V3 s2，§13.2）：musid `LyricsCache` 文档域，键
    /// (provider, provider 侧资源 id)。正域缓存命中词与确认无词
    /// （`Some(None)`），负域不接——manager 链级负缓存（网络错误不投毒、
    /// 全链确认无词才记）在 adapter 之上承担策略；本缓存只省重复取词。
    /// 缓存透明：probe 影子与主路径共用本面，缓存前后对账结论不变
    /// （单侧 Err 不计结论、双缺两侧都 NotFound、内容对照以真实产物为准）。
    #[cfg(feature = "musid-dli-adapter-lyrics-cache")]
    lyrics_cache: musid_dli::lyrics_cache::LyricsCache,
    /// 集合面 provider（V4-s1，§14.1）：歌单详情/专辑/链接直解，joox 起
    /// 装配；未装配时对应 face 走 trait 默认（不支持）。
    #[cfg(feature = "musid-dli-adapter")]
    collection: Option<Arc<dyn MdCollectionProvider>>,
    #[cfg(feature = "musid-dli-adapter")]
    playlists: Option<Arc<dyn MdPlaylistProvider>>,
    #[cfg(feature = "musid-dli-adapter")]
    albums: Option<Arc<dyn MdAlbumProvider>>,
    #[cfg(feature = "musid-dli-adapter")]
    links: Option<Arc<dyn MdLinkResolver>>,
    /// voicefox 代理/超时预算记录（§4：0.2.0 无法注入，仅登记供诊断）。
    transport: TransportInjection,
}

impl AdapterMusicSource {
    /// 由装配点构造（`musid_bootstrap`，按源子 feature 装配具体 provider）。
    pub fn new(
        source: SourceId,
        display_name: String,
        search: Option<Arc<dyn MdTrackSearch>>,
        stream: Option<Arc<dyn MdStreamResolver>>,
        lyrics: Option<Arc<dyn MdLyricsProvider>>,
        transport: TransportInjection,
    ) -> Self {
        Self {
            source,
            display_name,
            search,
            stream,
            lyrics,
            #[cfg(feature = "musid-dli-adapter-lyrics-cache")]
            lyrics_cache: musid_dli::lyrics_cache::LyricsCache::new(
                // 优化轮 P3-10：文档域 TTL 从 musid 默认 24h 收敛到 600s——
                // 缓存的「确认无词」随 manager 链级负缓存（600s）同窗过期，
                // 上游新上架的词最长 10 分钟内可达（原 24h 窗口里恒不可见）。
                // 正域 600s 仍是净收益（原先完全不缓存）。
                musid_dli::lyrics_cache::LyricsCacheConfig {
                    document_ttl: std::time::Duration::from_secs(600),
                    ..musid_dli::lyrics_cache::LyricsCacheConfig::default()
                },
            ),
            #[cfg(feature = "musid-dli-adapter")]
            collection: None,
            #[cfg(feature = "musid-dli-adapter")]
            playlists: None,
            #[cfg(feature = "musid-dli-adapter")]
            albums: None,
            #[cfg(feature = "musid-dli-adapter")]
            links: None,
            transport,
        }
    }

    /// 装配集合面（V4-s1，§14.1）：四个槽共用同一 provider 实例（musid
    /// 按 trait 拆面，装配形态与 search/stream/lyrics 三槽同形）。未装配
    /// 的源集合面走 trait 默认（不支持）。
    #[cfg(feature = "musid-dli-adapter")]
    pub fn with_collections(
        mut self,
        collection: Arc<dyn MdCollectionProvider>,
        playlists: Arc<dyn MdPlaylistProvider>,
        albums: Arc<dyn MdAlbumProvider>,
        links: Arc<dyn MdLinkResolver>,
    ) -> Self {
        self.collection = Some(collection);
        self.playlists = Some(playlists);
        self.albums = Some(albums);
        self.links = Some(links);
        self
    }

    /// 已登记的 voicefox 网络（代理/超时）预算。
    pub fn transport_injection(&self) -> &TransportInjection {
        &self.transport
    }
}

#[async_trait]
impl MusicSource for AdapterMusicSource {
    fn id(&self) -> SourceId {
        self.source
    }

    fn name(&self) -> &str {
        &self.display_name
    }

    /// 诊断对照用能力位（不进 `SourceManager::capabilities` 聚合）。
    /// V4-s1/s2（§14.1/§14.3）起面级 flags 按源登记：joox/qianqian 的
    /// search_playlists 与 qianqian 的 categories 在 musid provider 上有
    /// 真实实现（legacy 逐字移植），装配后相应位如实为 true（playlists/
    /// album/link_parse 位来自 provider descriptor）。
    fn capabilities(&self) -> SourceCapabilities {
        let Some(search) = &self.search else {
            return SourceCapabilities::default();
        };
        // V4-s1/s2 + 优化轮 P3-6：面级 flags 由**槽位装配**推导（搜索歌单
        // 面随 self.playlists 槽存在性），categories 位按源登记（musid
        // descriptor 无 categories 位：qianqian 覆写了真实实现，joox 的
        // categories 返回 Unsupported——两源装配形态不同）。
        let mut flags = ProviderFaceFlags {
            has_search_playlists: self.playlists.is_some(),
            ..ProviderFaceFlags::default()
        };
        if self.source == SourceId::Qianqian {
            flags.has_categories = true;
        }
        capabilities_from_provider(search.capabilities(), search.auth_capability(), flags)
    }

    async fn search(
        &self,
        keyword: &str,
        page: u32,
        limit: u32,
    ) -> Result<SearchResult, SearchError> {
        let Some(search) = &self.search else {
            return Err(SearchError::Other(
                "musid-dli provider 未注册搜索面".to_string(),
            ));
        };
        let page = search
            .search(&SearchQuery {
                keyword: keyword.to_string(),
                page,
                page_size: limit,
            })
            .await
            .map_err(|error| search_error_from_error(&error))?;
        search_page_to_result(&page).map_err(|error| SearchError::Other(error.to_string()))
    }

    async fn get_song_url(&self, song: &SongInfo, quality: Quality) -> Result<SongUrl, FetchError> {
        let Some(stream) = &self.stream else {
            return Err(FetchError::Other(
                "musid-dli provider 未注册流地址面".to_string(),
            ));
        };
        let track =
            adapter_track_for_song(song).map_err(|error| FetchError::Other(error.to_string()))?;
        // 显式传档位（§2.4）：不用 ResolveOptions::default()=High。
        let options = ResolveOptions {
            quality: stream_quality_from_quality(quality),
        };
        let info = stream
            .resolve(&track, &options)
            .await
            .map_err(|error| fetch_error_from_error(&error))?;
        let resolved = stream_to_resolved_url(&info);
        // G4：过期 URL 绝不透传播放器——已过期视为失败，调用方回落旧链。
        if let Some(expires_at) = resolved.expires_at_unix
            && now_unix() >= expires_at
        {
            return Err(FetchError::Other(
                "musid-dli 解析的播放地址已过期".to_string(),
            ));
        }
        let mut url = resolved.url;
        enrich_song_url_from_song(&mut url, song);
        for note in &resolved.notes {
            tracing::debug!(target: "musid_adapter", source = self.source.as_str(), note);
        }
        Ok(url)
    }

    async fn get_lyric(&self, song: &SongInfo) -> Result<LyricData, FetchError> {
        #[cfg(feature = "musid-dli-adapter-lyrics")]
        {
            let Some(lyrics) = &self.lyrics else {
                return Err(FetchError::Other(
                    "musid-dli provider 未注册歌词面".to_string(),
                ));
            };
            let track = adapter_track_for_song(song)
                .map_err(|error| FetchError::Other(error.to_string()))?;
            // V3 s2（§13.2）：文档域缓存先行。`Some(Some(doc))` 命中直接归一
            // 返回；`Some(None)` 是缓存的确认无词（与新鲜 Ok(None) 同形，
            // 都归 NotFound，probe 对账行为不变）；`None` 未缓存才走 provider。
            #[cfg(feature = "musid-dli-adapter-lyrics-cache")]
            if let Some(cached) = self
                .lyrics_cache
                .document_get(&track.id.provider, &track.id.id)
            {
                return match cached {
                    Some(document) => Ok(lyric_document_to_lx_data(&document)),
                    None => Err(FetchError::NotFound),
                };
            }
            match lyrics.lyrics(&track).await {
                Ok(Some(document)) => {
                    #[cfg(feature = "musid-dli-adapter-lyrics-cache")]
                    self.lyrics_cache.document_put(
                        &track.id.provider,
                        &track.id.id,
                        Some(&document),
                    );
                    Ok(lyric_document_to_lx_data(&document))
                }
                Ok(None) => {
                    #[cfg(feature = "musid-dli-adapter-lyrics-cache")]
                    self.lyrics_cache
                        .document_put(&track.id.provider, &track.id.id, None);
                    Err(FetchError::NotFound)
                }
                // 错误不缓存：网络抖动不投毒，manager 链级负缓存（全链确认
                // 无词才记、600s TTL）在 adapter 之上承担策略（§13.2）。
                Err(error) => Err(fetch_error_from_error(&error)),
            }
        }
        #[cfg(not(feature = "musid-dli-adapter-lyrics"))]
        {
            let _ = song;
            Err(FetchError::Other(
                "musid 歌词面需要 musid-dli-adapter-lyrics feature（歌词转换变体）".to_string(),
            ))
        }
    }

    /// V1 封面面不接 musid `ArtworkProvider`：搜索产物自带 cover_url，这里
    /// 只回填歌曲模型上的封面（与消费面 fallback 口径一致）。
    async fn get_cover_url(&self, song: &SongInfo) -> Result<String, FetchError> {
        song.cover_url
            .clone()
            .filter(|url| !url.trim().is_empty())
            .ok_or(FetchError::NotFound)
    }

    /// provider descriptor 的档位声明经 §2.4 翻译（`Unknown` 掉出）。
    fn supported_qualities(&self) -> Vec<Quality> {
        self.search
            .as_ref()
            .map(|provider| provider.supported_qualities())
            .unwrap_or_default()
            .into_iter()
            .filter_map(quality_from_stream)
            .collect()
    }

    // ------------------------------------------------------------------
    // V4-s1/s2 集合面（docs/MUSID_DLI_ADAPTER.md §14.1/§14.3；joox = 切片
    // 1、qianqian = 切片 2）：musid CollectionProvider/PlaylistProvider/
    // AlbumProvider/LinkResolver 的四槽路由与模型转换，cfg(any(joox,
    // qianqian)) 门控——两源的 musid provider 均为 M4 的 legacy 逐字移植
    // （fixture 钉住），槽位未装配时走「不支持」错误（trait 默认语义）。
    // qianqian 另加浏览面 categories/playlists（仅其 musid provider 有真
    // 实实现）。
    // ------------------------------------------------------------------

    /// 关键词歌单搜索（musid `PlaylistProvider::search_playlists`；joox 的
    /// 同名 musid 实现是 legacy 的逐字移植——search type=0 读
    /// section_list[].editor_playlist，空结果恒 Ok(vec![]) 不报错；
    /// qianqian 同口径）。
    #[cfg(any(
        feature = "musid-dli-adapter-joox",
        feature = "musid-dli-adapter-qianqian"
    ))]
    async fn search_playlists(
        &self,
        keyword: &str,
        page: u32,
    ) -> Result<Vec<Playlist>, SearchError> {
        let Some(playlists) = &self.playlists else {
            return Err(SearchError::Other("该音源不支持歌单搜索".to_string()));
        };
        let items = playlists
            .search_playlists(keyword, page)
            .await
            .map_err(|error| search_error_from_error(&error))?;
        items
            .iter()
            .map(|item| playlist_to_lx(item).map_err(|error| SearchError::Other(error.to_string())))
            .collect()
    }

    /// 歌单曲目（musid `CollectionProvider::playlist_tracks`；空曲目归
    /// NotFound 与 legacy `get_playlist_detail` 的空守卫同口径）。`_page`
    /// 不裁剪：musid 端点返回全量（与 legacy 全量语义一致）。
    #[cfg(any(
        feature = "musid-dli-adapter-joox",
        feature = "musid-dli-adapter-qianqian"
    ))]
    async fn get_playlist_detail(&self, id: &str, _page: u32) -> Result<Vec<SongInfo>, FetchError> {
        let Some(collection) = &self.collection else {
            return Ok(vec![]);
        };
        let tracks = collection
            .playlist_tracks(id)
            .await
            .map_err(|error| fetch_error_from_error(&error))?;
        if tracks.is_empty() {
            return Err(FetchError::NotFound);
        }
        tracks
            .iter()
            .map(track_to_song)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| FetchError::Other(error.to_string()))
    }

    /// 专辑曲目（musid `AlbumProvider::album_tracks`；OpenJOOX 专辑是特殊
    /// 歌单，与 legacy 同一接口）。分页语义：musid 端点返回全量曲目，
    /// `_page`/`_limit` 不截断（has_more 恒 false）——与 legacy 行为一致；
    /// 接口层面的分页裁剪属 musid-dli 上游能力，落地前如实不裁。
    #[cfg(any(
        feature = "musid-dli-adapter-joox",
        feature = "musid-dli-adapter-qianqian"
    ))]
    async fn get_album_songs(
        &self,
        album: &lx_core::model::playlist::Album,
        _page: u32,
        _limit: u32,
    ) -> Result<SearchResult, SearchError> {
        let Some(albums) = &self.albums else {
            return Err(SearchError::Other("该音源不支持专辑曲目".to_string()));
        };
        let tracks = albums
            .album_tracks(&album.id)
            .await
            .map_err(|error| search_error_from_error(&error))?;
        let items = tracks
            .iter()
            .map(track_to_song)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| SearchError::Other(error.to_string()))?;
        Ok(SearchResult {
            total: items.len() as u32,
            has_more: false,
            items,
        })
    }

    /// 链接直解（musid `LinkResolver::resolve_link`；song/playlist/album
    /// 三分支与 legacy parse_link 同形——playlist 分支的曲目取
    /// `playlist_tracks`，album 分支走 `album_tracks`，与 legacy 的
    /// 「详情 + 曲目一次取齐」产物形状一致）。
    #[cfg(any(
        feature = "musid-dli-adapter-joox",
        feature = "musid-dli-adapter-qianqian"
    ))]
    async fn parse_link(&self, link: &str) -> Result<ParsedLink, FetchError> {
        let Some(links) = &self.links else {
            return Err(FetchError::Other("该音源不支持链接直解".to_string()));
        };
        match links.resolve_link(link).await {
            Ok(musid_dli::provider::ResolvedLink::Track(track)) => Ok(ParsedLink::Song(Box::new(
                track_to_song(&track)
                    .map_err(|error| FetchError::Other(format!("链接曲目映射失败: {error}")))?,
            ))),
            Ok(musid_dli::provider::ResolvedLink::Playlist(playlist)) => {
                let songs = self
                    .playlist_songs_of(&playlist)
                    .await
                    .map_err(|error| fetch_error_from_error(&error))?;
                let converted = playlist_to_lx(&playlist)
                    .map_err(|error| FetchError::Other(error.to_string()))?;
                Ok(ParsedLink::Playlist {
                    playlist: Box::new(converted),
                    songs,
                })
            }
            Ok(musid_dli::provider::ResolvedLink::Album(album)) => {
                let songs = self
                    .album_songs_of(&album)
                    .await
                    .map_err(|error| fetch_error_from_error(&error))?;
                let converted = album_to_lx_playlist(&album)
                    .map_err(|error| FetchError::Other(error.to_string()))?;
                Ok(ParsedLink::Album {
                    playlist: Box::new(converted),
                    songs,
                })
            }
            Ok(other) => Err(FetchError::Other(format!(
                "链接指向不支持的资源形态: {other:?}"
            ))),
            Err(error) => Err(fetch_error_from_error(&error)),
        }
    }

    /// 歌单分类目录（V4-s2，§14.3；musid `PlaylistProvider::categories`，
    /// qianqian 专属——「全部」lead + 分组目录为 legacy 逐字移植；
    /// `playlist_category_to_lx` 为既有 V0 转换，provider 必填语义不变）。
    #[cfg(feature = "musid-dli-adapter-qianqian")]
    async fn get_playlist_categories(&self) -> Result<Vec<PlaylistCategory>, FetchError> {
        let Some(playlists) = &self.playlists else {
            // 未装配集合槽位：与 trait 默认（Ok(vec![])）一致，UI 依
            // capabilities 位隐藏入口。
            return Ok(vec![]);
        };
        let categories = playlists
            .categories()
            .await
            .map_err(|error| fetch_error_from_error(&error))?;
        categories
            .iter()
            .map(|category| {
                playlist_category_to_lx(category)
                    .map_err(|error| FetchError::Other(error.to_string()))
            })
            .collect()
    }

    /// 分类歌单列表（V4-s2，§14.3；musid `PlaylistProvider::playlists`；
    /// tag_id = subCateId 空值表示全部，pageSize 30 与 legacy 同值——musid
    /// 侧逐字移植硬编码）。
    #[cfg(feature = "musid-dli-adapter-qianqian")]
    async fn get_playlists(&self, tag_id: &str, page: u32) -> Result<Vec<Playlist>, FetchError> {
        let Some(playlists) = &self.playlists else {
            return Ok(vec![]);
        };
        let items = playlists
            .playlists(tag_id, page, 30)
            .await
            .map_err(|error| fetch_error_from_error(&error))?;
        items
            .iter()
            .map(|item| playlist_to_lx(item).map_err(|error| FetchError::Other(error.to_string())))
            .collect()
    }
}

impl AdapterMusicSource {
    /// 歌单实体的曲目：实体自带 tracks 用之，否则按 id 二次取
    /// （musid `CollectionProvider::playlist_tracks`）。
    #[cfg(any(
        feature = "musid-dli-adapter-joox",
        feature = "musid-dli-adapter-qianqian"
    ))]
    async fn playlist_songs_of(
        &self,
        playlist: &MdPlaylist,
    ) -> Result<Vec<SongInfo>, musid_dli::Error> {
        if !playlist.tracks.is_empty() {
            return playlist
                .tracks
                .iter()
                .map(track_to_song)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| musid_dli::Error::InvalidInput(error.to_string()));
        }
        let Some(collection) = &self.collection else {
            return Ok(vec![]);
        };
        let tracks = collection.playlist_tracks(&playlist.id.id).await?;
        tracks
            .iter()
            .map(track_to_song)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| musid_dli::Error::InvalidInput(error.to_string()))
    }

    /// 专辑实体的曲目：musid `Album` 不携带 tracks，按 id 取
    /// （musid `AlbumProvider::album_tracks`）。
    #[cfg(any(
        feature = "musid-dli-adapter-joox",
        feature = "musid-dli-adapter-qianqian"
    ))]
    async fn album_songs_of(&self, album: &MdAlbum) -> Result<Vec<SongInfo>, musid_dli::Error> {
        let Some(albums) = &self.albums else {
            return Ok(vec![]);
        };
        let tracks = albums.album_tracks(&album.id.id).await?;
        tracks
            .iter()
            .map(track_to_song)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| musid_dli::Error::InvalidInput(error.to_string()))
    }
}

// ---------------------------------------------------------------------------
// V2 逐源路由注册表（docs/MUSID_DLI_ADAPTER.md §12；manager `musid_route` 消费）
// ---------------------------------------------------------------------------

/// 单个源单个面的路由账面（三态）。
///
/// V2 逐源迁移 = 每源一行、每面一格的注册表；`manager::musid_route` 在
/// V1 判定（总闸 && 面开关 && 命名表可映射 && 非 Local/Soda/Bili &&
/// enabled && 影子源已注册）之上追加本表的放行结论。行内各面**独立**
/// 改账：切片落地时把对过账的面改 [`FaceRoute::Migrated`]、语义不等价的
/// 面改 [`FaceRoute::LegacyOnly`]，其余源不受影响（每源独立可回退）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaceRoute {
    /// V1 双栈已路由、V2 逐源对账未做：路由放行，运行行为与 V1 完全一致
    /// （V1 代码面上 10 源全部"已接"，差异只在子 feature 与运行期 config）。
    /// 该源的对账切片落地时逐面改 [`FaceRoute::Migrated`] 或
    /// [`FaceRoute::LegacyOnly`]。
    V1Routed,
    /// V2 切片已落地：对账 fixture 与回落规则已登记（docs §12.4），路由放行。
    Migrated,
    /// 恒旧路径：musid 侧能力缺失或语义不等价，路由判定拒绝并登记原因。
    /// 这是"能力缺失回落旧路径（不是 Unsupported 直接抛给用户）"的静态层
    /// ——LegacyOnly 面上 musid 的"该音源不支持"类错误永远到不了用户。
    LegacyOnly(&'static str),
}

impl FaceRoute {
    /// 路由判定用：该面是否允许经 musid-dli。
    pub fn routes_to_musid(self) -> bool {
        matches!(self, FaceRoute::V1Routed | FaceRoute::Migrated)
    }
}

/// 注册表行：一个源的三个运行时面各自的账面。
///
/// 集合面（歌单/专辑/歌手/榜单/链接直解）与登录/会话面不在 V2 迁移序列
/// （§12.4），不设行项：影子源本就不实现 fat-trait 集合方法、登录方法
/// （V1 边界 §11.8），manager 路径上不可达，恒旧路径。歌曲详情面无独立
/// 运行时开关（§11.4 第 4 条），由搜索/流地址承载面携带，同样不设行项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceRouteEntry {
    pub source: SourceId,
    /// 对账切片指针（盘点切片 id；已落地切片注明状态）。
    pub slice: &'static str,
    pub search: FaceRoute,
    pub song_url: FaceRoute,
    pub lyric: FaceRoute,
}

/// 逐源路由注册表（V2 初始账面，2026-11-21）。行覆盖 §1 命名表的全部
/// 11 个映射源；`soda`/`local` 不在表内（命名表拒绝 + manager 显式排除，
/// 恒旧路径）。测试 pin"行数 = 命名表映射源数"，防漏行。
const V2_ROUTE_REGISTRY: &[SourceRouteEntry] = &[
    // 切片 1（v2-jamendo，已落地，docs §12.4）。可路由面：搜索 + 流地址。
    // 歌词面 LegacyOnly 的登记依据：musid 侧 LyricsProvider 是显式
    // unsupported stub（musid-dli source_jamendo.rs "the CC catalog exposes
    // no lyrics"），voicefox 旧路径 get_lyric 恒 NotFound
    // （jamendo/mod.rs:421-424）——若路由，无词语义会从 NotFound 污染成
    // Other("该音源不支持…")，故不路由（能力缺失回落规则 §12.2 静态层）。
    // 集合面：musid capabilities 恒 false（container endpoints 匿名挂起，
    // source_jamendo.rs:364-368），影子源不实现集合方法，无行项。
    SourceRouteEntry {
        source: SourceId::Jamendo,
        slice: "v2-jamendo（已落地）",
        search: FaceRoute::Migrated,
        song_url: FaceRoute::Migrated,
        lyric: FaceRoute::LegacyOnly(
            "musid 歌词面是显式 unsupported stub（CC 曲库无词）；旧路径恒 \
             NotFound，路由会把无词语义污染成 Other",
        ),
    },
    // 切片 2（v2-fivesing，本次落地，docs §12.4）。三面全路由：musid
    // FivesingProvider 的 track_search/stream_resolve/lyrics 全 true
    // （musid-dli source_fivesing.rs:494-503，authentication=false 无登录态
    // + AuthCapability::None，上游注册测试 pin）。流地址面是**现取**语义
    // （resolve 恒走 getSongUrl 网络端点，source_fivesing.rs:546-596），无
    // jamendo 式"搜索时快照"动态层盲区。两侧差异逐条登记（§12.2）：
    // (a) headers——legacy 恒挂 MEDIA_REFERER（fivesing/mod.rs:110，播放侧
    //     playback.rs:123 消费 url.headers）、musid headers 恒空
    //     （source_fivesing.rs:377）；上游 fixture README 明文 live probe
    //     结论（2026-11-18）：CDN 已不再强制 Referer，musid 对齐 live 行为，
    //     legacy 头是历史残留、无害，不做改写。
    // (b) getSongUrl 直链内嵌时间戳+hash 会过期且无 expires 信号 → G4 门
    //     不设防；两侧同是现取，同病，无回退盲区。
    // (c) 搜索面 qualities 簿记——legacy 产物不携带档位（api.rs
    //     search_songs）、musid 静态声明三档（fs_parse_search_song），
    //     songs_equivalent 的 qualities 集合比对（parity.rs:180）系统性
    //     false → probe 搜索面 ParityMatch/Mismatch 对该源无鉴别力（身份
    //     字段 id/name/singer/时长全对齐，adapter 单测 pin 唯一分歧字段）。
    // 保持旧路径：歌单面（musid PlaylistProvider 存在且
    //     capabilities.playlists=true，但 V1 影子源不实现 fat-trait 集合
    //     方法、manager 路径不可达，search_playlists/fetch_playlist 恒旧
    //     路径，登记可后续片）；链接直解（musid LinkResolver 存在但 V1/V2
    //     四面范围不接，parse_link 恒旧路径）；专辑/歌手/榜单双侧同缺
    //     （musid 无对应 provider，voicefox fivesing/mod.rs:70-74 仅
    //     playlists/playlist_search/link_parse），不设行项。
    SourceRouteEntry {
        source: SourceId::Fivesing,
        slice: "v2-fivesing（已落地）",
        search: FaceRoute::Migrated,
        song_url: FaceRoute::Migrated,
        lyric: FaceRoute::Migrated,
    },
    // 切片 3（v2-apple，本次落地，docs §12.4）。三面全路由：musid
    // AppleMusicProvider 的 track_search/stream_resolve/lyrics 全 true
    // （musid-dli source_apple.rs:586-610；authentication=false + 无
    // auth_capability 覆写 → AuthCapability::None，登录态恒 LoggedOut，
    // source_apple.rs:783-798 有注释 pin）。
    // preview-only 契约两侧同形：musid resolve 忽略请求档位，恒回 30 秒
    // M4a 试听（quality Low、duration 30s、descriptor supported_qualities
    // = [Low]，source_apple.rs:589-617 有注释 pin 该契约）；voicefox
    // apple/mod.rs:75-99 同样恒 Low128 试听（"Apple Music 仅提供 30 秒
    // 试听片段"）、supported_qualities [Low128]（:117-119）——路由后用户
    // 可感知行为不变，quality 可严格断言双侧均 Low/Low128。
    // 逐条登记的差异（§12.2 apple 块）：
    // (a) SongUrl.duration——legacy 带整曲时长（apple/mod.rs:89
    //     detail.song.duration）、musid 带 preview 物理时长 30s；
    // (b) 搜索面 qualities 簿记——legacy parse_song 不填档位
    //     （apple/api.rs parse_song）、musid 静态 [Low]→{Low128}，
    //     songs_equivalent 的 qualities 集合比对系统性 false → probe
    //     搜索面对 apple 无鉴别力（同 fivesing 口径：簿记差异，不是歌曲
    //     身份差异；adapter 单测 pin 唯一分歧字段）；
    // (c) 歌词面 PlainText 无时间戳（catalog 无 timing）→
    //     document_to_lyric_data 的 lyric 为空、原文落 raw_lrc——musid
    //     槽位产物过不了 lyric_has_content 内容门，动态层恒回落旧路径
    //     （MusidErrLegacyOk），无错误语义污染（V1Routed 时该面已路由，
    //     本切片不改运行行为）。
    // 保持旧路径：歌单分类/歌单（musid PlaylistProvider categories/
    //     playlists 恒 Unsupported、search_playlists 存在；voicefox
    //     apple/mod.rs:121-153 恒旧路径）；专辑面（musid AlbumProvider、
    //     albums=true；voicefox get_album_songs 恒旧路径）；歌手/榜单双侧
    //     皆无（musid artists/charts 缺省 false）不设行项；链接直解不接
    //     （parse_link 恒旧路径）。
    SourceRouteEntry {
        source: SourceId::Apple,
        slice: "v2-apple（已落地）",
        search: FaceRoute::Migrated,
        song_url: FaceRoute::Migrated,
        lyric: FaceRoute::Migrated,
    },
    // 切片 4（v2-bilibili，本次落地，docs §12.4）。三件结构前置已落地：
    // 子 feature `musid-dli-adapter-bilibili`（source/Cargo.toml）+
    // musid_bootstrap 装配分支（BilibiliProvider::new 匿名）+ manager
    // matches! 放开 Bili——路由判定完全由本行承载。搜索 + 流地址
    // Migrated：musid BilibiliProvider track_search/stream_resolve true
    // （musid-dli source_bilibili.rs:907-916）。
    // 歌词面 LegacyOnly：双侧皆无——musid LyricsProvider 硬 stub
    // （"no lyric API exists on Bilibili"，capabilities.lyrics=false，
    // source_bilibili.rs:1094-1099）；voicefox bili/mod.rs:497-499 恒
    // FetchError::NotFound。不路由：旧路径 NotFound 语义不被 musid
    // UnsupportedOperation 污染（同 jamendo 歌词面口径）。
    // 登录/会话/用户数据恒旧路径（本切片的硬边界）：musid
    // authentication=true（Cookie/SESSDATA）但影子源恒匿名 new()（§11.8
    // 双栈零凭据流动）；manager 登录链路（create_qr_login/check_qr_login/
    // logout/is_logged_in）不读影子 map（V1 边界），voicefox bili 扫码
    // 登录 + SESSDATA 独立存储完全走旧路径。musid resolve 匿名可用性
    // （playurl 带 try_look=1 试看参数，DASH 四分支 fixture pin）的真网
    // 验证属运行期 probe 对账口径（本切片不实测，如实登记）。
    // 流地址面 DASH 形状（adapter 单测 pin）：headers（Referer + UA）
    // 两侧同形（legacy bili/url.rs:61-64 恒挂、musid StreamInfo.headers
    // 恒挂）；backupUrl 镜像进 fallback_endpoints → candidate_urls 折叠；
    // index/init range 走 G3 伴随字段（musid 不产 → None，descriptive-
    // only）；format=Aac 不反推档位（quality 来自带宽挑选）。
    // 保持旧路径：歌单/榜单/收藏夹（musid PlaylistProvider :1337/
    // ChartProvider :1395 存在，集合面 V2 四面范围不路由）；user 数据
    // （fav_folders）；链接直解（voicefox parse_link 复用搜索，恒旧路径）。
    SourceRouteEntry {
        source: SourceId::Bili,
        slice: "v2-bilibili（已落地）",
        search: FaceRoute::Migrated,
        song_url: FaceRoute::Migrated,
        lyric: FaceRoute::LegacyOnly(
            "双侧皆无歌词面（musid 硬 stub no lyric API + voicefox bili 恒 NotFound）",
        ),
    },
    // 切片 5（v2-migu，本次落地，docs §12.4）。三面全路由：musid
    // MiguProvider track_search/stream_resolve/lyrics 全 true（musid-dli
    // source_migu.rs:1388-1397）。登录面：musid authentication=false +
    // 无 auth_capability 覆写 → AuthCapability::None；voicefox mg/mod.rs
    // 亦无 login/qr_login 位——双侧匿名（musid 侧匿名 sid 设备会话属
    // AuthCapability::Anonymous 家族，provider/traits.rs:51-53），不设
    // 登录行项。搜索面 qualities 两侧同表（formatType PQ/HQ/SQ/ZQ*，
    // mg/song.rs format_to_quality 与 musid format_to_quality 逐档同形
    // 折档后相等）——无 fivesing/apple 式簿记分歧，parity 可全对齐。
    // 歌词面 MRC/lrc/trc 链：extra 键（mrcUrl/lrcUrl/trcUrl）经
    // track_to_song 原样透传回 musid；lxlyric 恒 None（musid 无 YRC/QRC
    // 序列化器）——legacy parse_mrc 产逐字，路由后逐字降级为纯 LRC 正文
    // + tlyric 保留（trc → translation），compare_lyrics 正文指纹对照
    // 不比逐字（§11.6 口径，同 kugou/kuwo 登记）。
    // 保持旧路径：榜单/专辑/歌单/链接直解（musid ChartProvider :1274/
    // AlbumProvider :1302/PlaylistProvider :1155/LinkResolver :1323 全
    // 存在且 capabilities true）——集合面 V2 四面范围不路由，musid 侧
    // 可用性登记供后续片；歌手面双侧皆无（musid artists 位缺省 false、
    // 无 ArtistProvider 实现；voicefox mg/mod.rs artist=true 位是搜索
    // 归一类能力，恒旧路径；后续片接 ArtistProvider 需先在 musid-dli
    // 侧补实现）。
    SourceRouteEntry {
        source: SourceId::Mg,
        slice: "v2-migu（已落地）",
        search: FaceRoute::Migrated,
        song_url: FaceRoute::Migrated,
        lyric: FaceRoute::Migrated,
    },
    // 切片 6（v2-kuwo，本次落地，docs §12.4）。三面全路由：musid
    // KuwoProvider track_search/stream_resolve/lyrics 全 true（musid-dli
    // source_kuwo.rs:1918-1930 前块）。登录面：authentication=false +
    // auth_capability() 覆写为 AuthCapability::Anonymous（kuwo web token
    // 非账号登录，source_kuwo.rs:1936-1940 有测试 pin；§2.8 机制细分
    // 口径 Anonymous 不置 voicefox login 位）——voicefox kw/mod.rs 亦无
    // login 位，恒旧路径。搜索面 N_MINFO 档位两侧同表（bitrate 128/320/
    // 2000/4000 → Low/High/Lossless/HiRes，折档后与 legacy 128/320/2000/
    // 4000 → Low128/High320/Flac/Flac24 相等）——qualities parity 可全
    // 对齐。流地址面双通道（mobile/web 并发，无档位请求降级 320k 再
    // 128k；lossless 请求 web 为主、移动端档位 URL 进候选）与 legacy
    // kw/url.rs:228-256 同形；headers：移动端通道两侧同挂 okhttp UA
    // （legacy kw/url.rs 恒挂、musid 移动端 StreamInfo.headers 恒挂）。
    // 歌词面 lrcx 逐字降级同 migu/kugou 口径（legacy kw/lyric.rs:93-107
    // 先 lrcx=1 逐字；musid LyricDocument 无 lxlyric 序列化器），正文
    // 指纹对照不比逐字。
    // 保持旧路径：登录（Anonymous web token，双侧无账号登录入口）；
    // 歌手面（musid 无 ArtistProvider 实现、artists 位缺省 false，
    // voicefox kw/mod.rs artist=true 位恒旧路径并登记缺口）；榜单/专辑/
    // 歌单/链接（musid ChartProvider :2151/AlbumProvider :1879/
    // PlaylistProvider :2216/LinkResolver :2197 全存在）集合面不路由登记。
    SourceRouteEntry {
        source: SourceId::Kw,
        slice: "v2-kuwo（已落地）",
        search: FaceRoute::Migrated,
        song_url: FaceRoute::Migrated,
        lyric: FaceRoute::Migrated,
    },
    // 切片 7（v2-kugou，本次落地，docs §12.4）。三面全路由：musid
    // KugouProvider track_search/stream_resolve/lyrics 全 true（musid-dli
    // source_kugou.rs:648-663，artists=true 为 search-based fallback）。
    // 登录/扫码面恒旧路径（本片最重要的边界）：musid authentication=true
    // （Cookie）→ 影子源 capabilities 诊断位 login=true，但登录/扫码链路
    // 不读影子 map（V1 边界）且影子源恒匿名 new()（§11.8 双栈零凭据
    // 流动）。**匿名退化在 kugou 流地址面是硬性的**：musid resolve 在
    // login_parts() 处无会话即失败（"Kugou playback requires login"，
    // source_kugou.rs:239-258）——影子源恒匿名 → 流地址面 musid 槽位
    // 在双栈下必然失败、动态层回落旧链现取（MusidErrLegacyOk）；该面
    // V1Routed 时已如此路由，本切片不改运行行为。缺 hash 的歌在
    // select_hash 处先于 login_parts 离线 NotFound（source_kugou.rs:
    // 519-536、:717）——manager 级真实双侧链路的确定性触发点。
    // 逐字歌词降级（本片登记的最重要行为差异）：musid 无 YRC/QRC 序列
    // 化器、lxlyric 恒 None（§2.10）；legacy kg/lyric.rs parse_krc 产
    // 逐字 lxlyric——歌词面路由接受"逐字 → 纯 LRC 正文"降级（KRC 词级
    // 时间在 musid LyricDocument.lines[].words，不伪造 lxlyric），
    // compare_lyrics 正文指纹对照不比逐字（§11.6）。片级决策：本切片
    // 按 ask 落地三面全路由，降级如实登记于此与 §12.2/§12.4。
    // 搜索面 qualities 两侧同表（FileSize/HQFileSize/SQFileSize/
    // ResFileSize > 0 → 四档），parity 全对齐。
    // 保持旧路径：登录/扫码（voicefox kg/login.rs + session.rs 扫码链路）；
    // 歌手（musid artists=true 但 ArtistProvider 是 search-based
    // fallback，voicefox artist 位恒旧路径）；榜单/专辑/歌单/链接
    // （musid ChartProvider :1007/AlbumProvider :1239/PlaylistProvider
    // :1688/LinkResolver :1135 全存在）集合面不路由登记。
    SourceRouteEntry {
        source: SourceId::Kg,
        slice: "v2-kugou（已落地）",
        search: FaceRoute::Migrated,
        song_url: FaceRoute::Migrated,
        lyric: FaceRoute::Migrated,
    },
    // 切片 8（v2-qqmusic，本次落地，docs §12.4）。三面全路由：musid
    // QqMusicProvider 的 track_search/stream_resolve/lyrics 全 true
    // （musid-dli source_qqmusic.rs:760-772）。登录/扫码面恒旧路径（本片
    // 硬边界）：musid authentication=true（Cookie）→ 影子源 capabilities
    // 诊断位 login=true，但登录链路不读影子 map（V1 边界 §11.8）且影子源
    // 恒匿名 new()；微信扫码（voicefox wechat_login 位）双栈期恒 false
    // （§2.8）。musid 匿名面对 cookie 登录用户的 VIP 曲目/档位变窄是 P5
    // 已登记口径（§11.8 (a)）——tx 用户基数最大，probe 对账的系统性
    // mismatch 噪声须先剥离登录态不对称再解读（§11.8 (b)）。
    // 流地址面：musid resolve 走 vkey 请求链（source_qqmusic.rs:809-863，
    // 上游 stream_vkey_ok/stream_vkey_empty_sip 形状 pin），strMediaMid/
    // songId/size_* 键经 extra 透传回 musid（适配层 fixture 单测 pin）；
    // 无档位尺寸声明时 Low 恒在（两侧同口径），size_128mp3 缺席时 musid
    // 恒产 Low 而 legacy 不产 Low128——搜索面 parity 用 size 全正的真实
    // 形状对账（簿记分歧随 track_to_song 折档集合承载，见 §12.2 qqmusic
    // 块）。歌词面：musid GetPlayLyricInfo crypt/qrc 0（plain LRC）→
    // lxlyric 恒 None；legacy crypt:1/qrc:1 产 QRC 逐字——路由后逐字
    // 降级为纯 LRC 正文 + 翻译/罗马音保留（§11.6 compare_lyrics 不比逐字，
    // 同 kugou/kuwo/migu 口径）。歌词离线失败形态：extra 无 songId 数值 →
    // InvalidInput（source_qqmusic.rs:873-877）。
    // 保持旧路径：登录/扫码（tx/login.rs + session.rs，QQ/微信双扫码）；
    // 歌手面（musid 无 ArtistProvider、artists 位缺省 false，voicefox
    // tx/mod.rs artist=true 恒旧路径并登记缺口）；榜单/专辑/歌单/链接
    // （musid ChartProvider :1467/AlbumProvider :1408/PlaylistProvider
    // :1313/LinkResolver :913 全存在且 capabilities true）集合面不路由
    // 登记。
    SourceRouteEntry {
        source: SourceId::Tx,
        slice: "v2-qqmusic（已落地）",
        search: FaceRoute::Migrated,
        song_url: FaceRoute::Migrated,
        lyric: FaceRoute::Migrated,
    },
    // 切片 9（v2-netease，本次落地，docs §12.4）。三面全路由：musid
    // NeteaseProvider 的 track_search/stream_resolve/lyrics/artists 等
    // 全 true（musid-dli source_netease.rs:443-456 全家桶）。wy 是主源且
    // 登录态依赖最重——放最后：登录/扫码面恒旧路径（voicefox
    // wy/mod.rs login+qr_login+user_playlists；musid authentication=true
    // 但影子源恒匿名 new()，§11.8 零凭据流动）；**musid 恒匿名 vs
    // voicefox cookie 登录的结果集/档位系统性收窄是 P5 已登记口径，
    // probe 对账会产生系统性 mismatch 噪声，解读计数时须先剥离登录态
    // 不对称（§11.8 (b)）**。流地址面：musid eapi
    // song/enhance/player/url/v1（source_netease.rs:494-601，带
    // outer-url 死兜底 HEAD 探测与 expires_at_unix 提取——后者使 G4
    // 过期门对该源**设防**，与 jamendo/fivesing 的 None 形态不同）；
    // pl_level 键经 extra 透传。歌词面 eapi song/lyric/v1 → lxlyric 恒
    // None（musid 无 YRC/QRC 序列化器）；legacy wy/lyric.rs 的 yrc 逐字
    // 路由后降级为纯 LRC 正文（compare_lyrics 不比逐字，同批内 kugou/
    // kuwo/migu/qqmusic 口径）。搜索面 ±1s 时长容差与折档后 qualities
    // 集合相等的 parity 归一在适配层单测 pin（legacy Low128 恒在 =
    // musid Low 恒在，两侧同口径无簿记分歧）。
    // 保持旧路径：登录/扫码/用户歌单；集合面（musid ArtistProvider
    // :766/ChartProvider :809/AlbumProvider :735/PlaylistProvider :652/
    // LinkResolver :840 全存在且 capabilities 全 true）不路由登记——
    // wy 的歌单同步若依赖 by-id 详情（songs_from_detail 类路径），前置
    // 是 musid-dli 0.2.0 无公开 per-provider detail trait（track_details
    // 是 crate 内部方法，§11.4 第 4 条），需上游先公开。
    SourceRouteEntry {
        source: SourceId::Wy,
        slice: "v2-netease（已落地）",
        search: FaceRoute::Migrated,
        song_url: FaceRoute::Migrated,
        lyric: FaceRoute::Migrated,
    },
    // 切片 10（v2-qianqian，本次落地，docs §12.4）。用户启用源。三面全
    // 路由：musid QianqianProvider 的 track_search/stream_resolve/lyrics/
    // playlists/albums/link_resolve true（musid-dli source_qianqian.rs
    // :732-742），authentication=false + 无 auth_capability 覆写 →
    // AuthCapability::None——登录面双侧皆无认证位，不设行项。
    // 流地址面 tracklink 双分支两侧同形（source_qianqian.rs:779-825 vs
    // musid-dli source_qianqian.rs tracklink 分支）：free_128 完整文件分支 + vip_nopath 阶梯
    // 耗尽退 trail_audio_info 试听分支；VIP 曲目匿名恒 path-less →
    // NotFound（上游 live-pinned）——与 legacy 同病同回落，无回退盲区；
    // expireTime 进 expires_at_unix（G4 对该源设防）。**空 tsid 的歌在
    // 两栈都在任何网络调用之前失败**（musid :791-793 / legacy url.rs
    // :39-41 同键同 NotFound）——manager 级真实双侧链路的确定性触发点
    // （musid_qianqian_slice.rs pin MusidErrLegacyErr）。搜索面 rate
    // 表两侧同形（3000/320/128|64 → Lossless/High/Low 折档后相等）。
    // 歌词面 song_infoandlrc 类形状（signed_get song/info + lyric 文本
    // 下载）→ lxlyric 恒 None，纯 LRC（legacy 千千本就无逐字，无降级）。
    // 保持旧路径：歌手面（musid 无 ArtistProvider、artists 位缺省
    // false，musid 无 ArtistProvider——artist 位恒 false（voicefox 上游无歌手面））；
    // 榜单面双侧皆无（voicefox qianqian capabilities 无 leaderboard 位，
    // musid 无 ChartProvider）不设；专辑/歌单/链接（musid AlbumProvider
    // :999/PlaylistProvider :874/LinkResolver :1015 且 capabilities true）
    // 集合面不路由登记——musid 侧 search_playlist_refused.json（歌单搜索
    // 被拒形状）在后续接集合面时须处理该语义（envelope 恒不按 state/
    // errno 把关、data:[] 解析为空页，source_qianqian.rs:762-764）。
    // 用户启用源：V4 起恒适配面（主注册表），parity 计数无对账对象——观测口径为运行期 probe 与 MusidErrLegacyOk（仅对仍双栈的源有意义，§12.3）。
    SourceRouteEntry {
        source: SourceId::Qianqian,
        slice: "v2-qianqian（已落地）",
        search: FaceRoute::Migrated,
        song_url: FaceRoute::Migrated,
        lyric: FaceRoute::Migrated,
    },
    // 切片 11（v2-joox，本次落地，docs §12.4）。用户启用源。三面全路由：
    // musid JooxProvider 的 track_search/stream_resolve/lyrics/playlists/
    // albums/link_resolve true（musid-dli source_joox.rs:706-716），
    // authentication=false + 无 auth_capability 覆写 → None——登录面双侧
    // 皆无认证位，不设行项。搜索面 search_songs 形状两侧同构（tracks 嵌
    // 套数组、id/album_id 加密串、normalize_id 两侧逐字同实现）；歌手
    // join 分歧（legacy " / " vs musid Track.artists 向量经 '、' join）
    // 经 parity 归一（filter_str 分隔符集含 /）无损，单测 pin。流地址面
    // songinfo_stream 形状两侧同构（select_candidate 与 legacy 同序：
    // sort desc → 第一个 ≤ 请求档 → 首选兜底；size/md5/expiry 两侧同弃，
    // size_is_advisory=true）；kbps 档两侧同表（320/192/128 → High/Low/
    // Low）。**空 songid 的歌 musid 侧在任何网络调用之前离线 NotFound**
    // （source_joox.rs:754-756）；legacy 同请求走 fetch_song_detail
    // 网络（无离线守卫）——manager 级双失败链不可确定性复现，joox 的
    // 流地址面失败侧对账由 adapter 级离线用例 + 运行期 probe 承载。
    // 歌词面 lyric_live 形状（web_lyric base64 字段，decode_lyric_field
    // live 偏差上游已登记）→ lxlyric 恒 None，纯 LRC（legacy 本就无逐
    // 字）。封面面不接 musid ArtworkProvider：歌曲模型回填（musid-dli
    // get_cover_url 走歌曲模型回填，适配层 get_cover_url 同口径
    // （adapter_musid.rs enrich_song_url_from_song 消费侧），不设行项。
    // source_joox.rs 无 ArtworkProvider）。榜单/歌手双侧皆无（musid joox descriptor 仅
    // playlists/playlist_search/album/link_parse；musid 无 ChartProvider/
    // ArtistProvider）不设；专辑/歌单/链接（musid AlbumProvider :873/
    // PlaylistProvider :839/LinkResolver :908 且 capabilities true）集合
    // 面不路由登记——musid 侧 playlist_dead_404.html 死链形状在后续接
    // 集合面时须处理。用户启用源：翻源后同 qianqian 盯 MusidErrLegacyOk。
    SourceRouteEntry {
        source: SourceId::Joox,
        slice: "v2-joox（已落地）",
        search: FaceRoute::Migrated,
        song_url: FaceRoute::Migrated,
        lyric: FaceRoute::Migrated,
    },
];

/// 注册表行（`soda`/`local` 与任何未列源返回 `None`）。
pub fn source_route_entry(source: SourceId) -> Option<&'static SourceRouteEntry> {
    V2_ROUTE_REGISTRY.iter().find(|row| row.source == source)
}

/// 路由判定入口（`manager::musid_route` 消费）：面 + 源的放行结论。
/// 无行（`soda`/`local`）= 恒旧路径。
pub fn face_route(face: crate::parity::ParityFace, source: SourceId) -> FaceRoute {
    let Some(row) = source_route_entry(source) else {
        return FaceRoute::LegacyOnly("源不在 §1 命名表（soda/local），恒旧路径");
    };
    match face {
        crate::parity::ParityFace::Search => row.search,
        crate::parity::ParityFace::SongUrl => row.song_url,
        crate::parity::ParityFace::Lyric => row.lyric,
    }
}

/// 注册表中标记为 [`FaceRoute::Migrated`] 的源（账面侧）。
///
/// **生效前提**（P4 登记，docs §12.1）：行的放行还要求对应
/// `musid-dli-adapter-<source>` 子 feature 在本次构建中启用（影子源被
/// `musid_bootstrap` 装配）。子 feature 未启用时影子源不在 map 里，
/// 路由判定返回 None → 该源静默走旧路径、parity 计数恒零——账面
/// "已迁移"与运行时能力可能脱节，本函数与
/// [`unregistered_migrated_sources`] 供 `bootstrap_musid_stack` 启动时
/// 显式 warn 暴露该脱节。
pub fn migrated_route_sources() -> Vec<SourceId> {
    V2_ROUTE_REGISTRY
        .iter()
        .filter(|row| {
            row.search == FaceRoute::Migrated
                || row.song_url == FaceRoute::Migrated
                || row.lyric == FaceRoute::Migrated
        })
        .map(|row| row.source)
        .collect()
}

/// 账面 Migrated 但本次构建未装配影子源的源（`bootstrap_musid_stack`
/// 启动 warn 的输入；`registered` 是 `musid_bootstrap::register_available`
/// 的返回值）。
pub fn unregistered_migrated_sources(registered: &[SourceId]) -> Vec<SourceId> {
    migrated_route_sources()
        .into_iter()
        .filter(|source| {
            // V4-s1/s2（§14.1/§14.3）：joox 与 qianqian 的 legacy 已删，适
            // 配面是**唯一**实现——manager 主注册表无条件装配（不走影子注
            // 册），故不进本 warn。
            !registered.contains(source)
                && *source != SourceId::Joox
                && *source != SourceId::Qianqian
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 歌词（复用 musid-dli 内置转换器；feature musid-dli-adapter-lyrics）
// ---------------------------------------------------------------------------

/// musid-dli 在线歌词候选（`LyricSearchResult`）到 voicefox 跨源补全候选的
/// 映射产物（设计文档 §2.10）。`song` 供
/// `SourceManager::get_lyric_with_fallback` 链路组 `SongInfo`，`match_score`
/// 直接替代 voicefox `find_music` 的分数用于排序；voicefox 负缓存键
/// `(source, extra[source], id)`（`manager.rs:1015-1019`）在适配层换算为
/// `(provider, raw_id)` 对接 musid-dli NegativeCache。
#[derive(Debug, Clone)]
pub struct LyricSearchCandidate {
    /// provider+id 经 namespace 表回 `SourceId` 后组出的歌曲模型。
    pub song: SongInfo,
    /// musid-dli 的匹配置信度（0-100），原样透传供排序。
    pub match_score: Option<u8>,
}

/// musid-dli `LyricSearchResult` 到跨源补全候选。
///
/// `LyricSearchResult` 是独立模型（provider/id/title/artists/album/
/// duration_ms/match_score，musid-dli `models/lyrics.rs:57-67`），不是
/// `Track`，不能机械套用 Track→SongInfo 规则，因此是专属函数：`duration_ms`
/// 直折 `Duration::from_millis`（`None`→`ZERO`，与 Track 的 None→ZERO 规则
/// 同口径）、`album` 缺省折空串、`match_score` 原样透传。provider 查不到
/// （`youtube-music`/`lx-js`）报错，绝不改名。
pub fn lyric_search_candidate_from_md(
    result: &MdLyricSearchResult,
) -> Result<LyricSearchCandidate, AdapterError> {
    let source = provider_to_source_id(&result.provider)
        .ok_or_else(|| AdapterError::UnmappedProvider(result.provider.clone()))?;
    let mut song = SongInfo::new(
        result.id.clone(),
        source,
        result.title.clone(),
        artists_to_singer(&result.artists),
    );
    song.album_name = result.album.clone().unwrap_or_default();
    song.duration = result
        .duration_ms
        .map(Duration::from_millis)
        .unwrap_or(Duration::ZERO);
    Ok(LyricSearchCandidate {
        song,
        match_score: result.match_score,
    })
}

/// musid-dli `LyricDocument` 到 voicefox `LyricData`。
///
/// 直接复用 `musid_dli::lyrics_convert::document_to_lyric_data`（不重写）：
/// `lyric` 仅在原文含 mm:ss.xx 括号时信原始文本否则渲染解析行、
/// `tlyric`/`rlyric` 原样、`lxlyric` 恒 `None`（musid-dli 无 YRC/QRC
/// 序列化器，逐字时间只在 `lines[].words`，伪造即假数据）、`raw_lrc`
/// 承载非空 original。逐行 translations/romanization 与 `format` 无
/// voicefox 槽位，显式丢弃（M3 §4 pin 的有损点）。
#[cfg(feature = "musid-dli-adapter-lyrics")]
pub fn lyric_document_to_lx_data(
    document: &musid_dli::models::LyricDocument,
) -> lx_core::model::lyric::LyricData {
    let mirror = musid_dli::lyrics_convert::document_to_lyric_data(document);
    lx_core::model::lyric::LyricData {
        lyric: mirror.lyric,
        tlyric: mirror.tlyric,
        rlyric: mirror.rlyric,
        lxlyric: None,
        raw_lrc: mirror.raw_lrc,
    }
}

/// voicefox `LyricData` 到 musid-dli `LyricDocument`（反向，复用
/// `lyric_data_to_document`）。无法解析/无逐字时间的 `lxlyric` 是硬错误
/// （`Error::InvalidInput`），映射为 [`AdapterError::InvalidLyric`]。
#[cfg(feature = "musid-dli-adapter-lyrics")]
pub fn lyric_data_to_md_document(
    data: &lx_core::model::lyric::LyricData,
    format: musid_dli::models::LyricFormat,
) -> Result<musid_dli::models::LyricDocument, AdapterError> {
    let mirror = musid_dli::lyrics_convert::LyricData {
        lyric: data.lyric.clone(),
        tlyric: data.tlyric.clone(),
        rlyric: data.rlyric.clone(),
        lxlyric: data.lxlyric.clone(),
        raw_lrc: data.raw_lrc.clone(),
    };
    musid_dli::lyrics_convert::lyric_data_to_document(&mirror, format)
        .map_err(|error| AdapterError::InvalidLyric(error.to_string()))
}

/// musid-dli 文档行到 voicefox `LyricLine`（复用 `lyric_lines_from_document`）：
/// 行时长 end_ms 有值用 end_ms-start_ms，否则下一行 start-本行，末行 0
/// （voicefox 原生规则）；空白行文本回退逐字文本拼接。
#[cfg(feature = "musid-dli-adapter-lyrics")]
pub fn lyric_lines_to_lx(
    lines: &[musid_dli::models::LyricLine],
) -> Vec<lx_core::model::lyric::LyricLine> {
    musid_dli::lyrics_convert::lyric_lines_from_document(lines)
        .into_iter()
        .map(|line| lx_core::model::lyric::LyricLine {
            timestamp: line.timestamp,
            text: line.text,
            duration: line.duration,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// fixture 测试：musid-dli 模型 wire JSON -> musid-dli 模型 -> voicefox 类型
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use musid_dli::models::Chart as MdChart;
    use serde_json::{Value, from_str, from_value, json};

    /// fixture 落位 `source/tests/fixtures/musid/`（voicefox 仓内相对路径；
    /// 形状与样本值来源见各文件 `derivation_note` 与设计文档 §7）。
    /// `include_str!`/`concat!` 只吃字面量，走宏展开。
    macro_rules! fixture {
        ($name:literal) => {
            include_str!(concat!("../tests/fixtures/musid/", $name))
        };
    }

    fn netease_track_fixture() -> Track {
        from_str::<Track>(fixture!("track_netease.json")).expect("track fixture must parse")
    }

    fn netease_stream_fixture() -> StreamInfo {
        from_str::<StreamInfo>(fixture!("stream_netease.json")).expect("stream fixture must parse")
    }

    // --- namespace ---

    #[test]
    fn namespace_round_trips_and_rejects_gap_sources() {
        let pairs: &[(&str, SourceId)] = &[
            ("netease", SourceId::Wy),
            ("qqmusic", SourceId::Tx),
            ("kugou", SourceId::Kg),
            ("kuwo", SourceId::Kw),
            ("migu", SourceId::Mg),
            ("bilibili", SourceId::Bili),
            ("jamendo", SourceId::Jamendo),
            ("applemusic", SourceId::Apple),
            ("fivesing", SourceId::Fivesing),
            ("qianqian", SourceId::Qianqian),
            ("joox", SourceId::Joox),
        ];
        for (provider, source) in pairs {
            assert_eq!(source_id_to_provider(*source), Some(*provider));
            assert_eq!(provider_to_source_id(provider), Some(*source));
            assert_eq!(require_provider(*source), Ok(*provider));
        }
        // voicefox-only / musid-dli-only / 跨语言串名：显式拒绝。
        assert_eq!(source_id_to_provider(SourceId::Soda), None);
        assert_eq!(source_id_to_provider(SourceId::Local), None);
        assert_eq!(provider_to_source_id("youtube-music"), None);
        assert_eq!(provider_to_source_id("lx-js"), None);
        assert_eq!(provider_to_source_id("wy"), None);
        assert_eq!(voicefox_source_to_provider("netease"), None);
        assert_eq!(
            require_provider(SourceId::Soda),
            Err(AdapterError::UnmappedProvider("soda".to_string()))
        );
    }

    // --- Track -> SongInfo（fixture） ---

    #[test]
    fn track_fixture_maps_to_song_info() {
        let track = netease_track_fixture();
        let song = track_to_song(&track).expect("netease provider must map");
        assert_eq!(song.source, SourceId::Wy);
        assert_eq!(song.id, "789");
        assert_eq!(song.name, "老接口");
        assert_eq!(song.singer, "周杰伦、费玉清");
        assert_eq!(song.album_name, "老专辑");
        assert_eq!(song.album_id, "42");
        assert_eq!(song.duration, Duration::from_secs(258));
        assert_eq!(song.cover_url.as_deref(), Some("https://p"));
        assert_eq!(
            song.qualities,
            [Quality::High320, Quality::Flac, Quality::Flac24]
                .into_iter()
                .collect()
        );
        assert_eq!(song.audio, None);
        assert_eq!(
            song.extra.get("pl_level").map(String::as_str),
            Some("lossless")
        );
        assert!(!song.extra.contains_key(EXTRA_ISRC));
    }

    #[test]
    fn track_isrc_travels_via_extra_and_back() {
        let mut track = netease_track_fixture();
        track.isrc = Some("CNW000000000".to_string());
        let song = track_to_song(&track).expect("netease provider must map");
        assert_eq!(
            song.extra.get(EXTRA_ISRC).map(String::as_str),
            Some("CNW000000000")
        );
        let rebuilt = song_to_track(&song).expect("wy source must map");
        assert_eq!(rebuilt.isrc.as_deref(), Some("CNW000000000"));
        assert!(
            !rebuilt.extra.contains_key(EXTRA_ISRC),
            "isrc 还原进槽位后不再留在 extra"
        );
    }

    #[test]
    fn extra_isrc_reserved_key_round_trip_is_lossy() {
        // 保留键碰撞（设计文档 §3 第 15 条）：extra 里自带 "isrc" 而 isrc
        // 槽位为 None 的 Track，反向映射会把 extra["isrc"] 提升进 isrc 槽位
        // 并从 extra 删除，往返不保真。musid-dli 现有 provider 不产
        // extra["isrc"]（isrc 有专属槽位），登记为边角缺口而非现行缺陷；
        // V1 如需严格保真可改为保留键检测报错。
        let mut track = netease_track_fixture();
        track.isrc = None;
        track.extra.insert(EXTRA_ISRC.to_string(), "X".to_string());
        let song = track_to_song(&track).expect("netease provider must map");
        assert_eq!(song.extra.get(EXTRA_ISRC).map(String::as_str), Some("X"));

        let rebuilt = song_to_track(&song).expect("wy source must map");
        assert_eq!(
            rebuilt.isrc.as_deref(),
            Some("X"),
            "extra['isrc'] 被提升进 isrc 槽位"
        );
        assert!(!rebuilt.extra.contains_key(EXTRA_ISRC));
        assert_ne!(rebuilt, track, "键碰撞往返不保真是登记过的有损行为");
    }

    #[test]
    fn unmapped_providers_and_sources_are_rejected() {
        let mut track = netease_track_fixture();
        track.id = TrackId::new("youtube-music", "yt1");
        assert!(matches!(
            track_to_song(&track),
            Err(AdapterError::UnmappedProvider(ref provider)) if provider == "youtube-music"
        ));
        let song = SongInfo::new(
            "1".to_string(),
            SourceId::Soda,
            "名".to_string(),
            "A".to_string(),
        );
        assert!(matches!(
            song_to_track(&song),
            Err(AdapterError::UnmappedProvider(ref provider)) if provider == "soda"
        ));
    }

    #[test]
    fn song_to_track_defaults_are_lossless_reverse() {
        let mut song = SongInfo::new(
            "1".to_string(),
            SourceId::Wy,
            "名".to_string(),
            "A、B".to_string(),
        );
        let track = song_to_track(&song).expect("wy source must map");
        assert_eq!(track.id, TrackId::new("netease", "1"));
        assert_eq!(
            track.artists,
            vec!["A", "B"],
            "适配层自身约定分隔符 '、' 无损切分"
        );
        assert_eq!(track.album, None, "空串按 musid-dli 语义折回 None");
        assert_eq!(track.album_id, None);
        assert_eq!(track.duration, None, "ZERO 时长折回 None");
        assert_eq!(track.available_qualities, Vec::<StreamQuality>::new());

        // 反向切分只按 '、'：voicefox 归一分隔符集合（、, & / |）只用于匹配
        // 归一，拿全集切分会把含分隔符的单一歌手名撕碎（身份数据损坏）。
        assert_eq!(
            singer_to_artists("Earth, Wind & Fire"),
            vec!["Earth, Wind & Fire"]
        );
        assert_eq!(singer_to_artists("AC/DC"), vec!["AC/DC"]);
        assert_eq!(
            singer_to_artists("  A 、 B  "),
            vec!["A", "B"],
            "trim 后去空"
        );

        song.album_name = "专辑".to_string();
        song.album_id = "9".to_string();
        song.duration = Duration::from_secs(200);
        song.qualities = [Quality::Low128, Quality::Flac24].into_iter().collect();
        let track = song_to_track(&song).expect("wy source must map");
        assert_eq!(track.album.as_deref(), Some("专辑"));
        assert_eq!(track.album_id.as_deref(), Some("9"));
        assert_eq!(track.duration, Some(Duration::from_secs(200)));
        assert_eq!(
            track.available_qualities,
            vec![StreamQuality::Low, StreamQuality::HiRes]
        );
    }

    #[test]
    fn track_round_trip_through_song_info() {
        let track = netease_track_fixture();
        let song = track_to_song(&track).expect("netease provider must map");
        let rebuilt = song_to_track(&song).expect("wy source must map");
        assert_eq!(rebuilt, track);
    }

    // --- StreamInfo -> SongUrl（fixture + 契约） ---

    #[test]
    fn stream_fixture_maps_to_song_url_with_sidecar() {
        let info = netease_stream_fixture();
        let resolved = stream_to_resolved_url(&info);
        assert!(resolved.notes.is_empty(), "标准档位/MD5/有 size：无降级");
        let url = &resolved.url;
        assert_eq!(
            url.url,
            "http://m801.music.126.net/20260101120000/9e2c8a42b0e1f3a7c5d6081112aab3cc/jdymusic/186016.mp3"
        );
        assert_eq!(url.quality, Quality::High320);
        assert_eq!(url.duration, Duration::from_secs(258));
        assert_eq!(url.size, Some(8523134));
        assert!(!url.size_is_advisory);
        assert_eq!(url.md5.as_deref(), Some("0123456789abcdef0123456789abcdef"));
        assert_eq!(
            url.headers,
            vec![("Referer".to_string(), "https://music.163.com".to_string())]
        );
        assert_eq!(url.max_chunk_size, 0);
        // 备用 URL 顺序契约：legacy 先（trim、去空）、结构化后、primary 不含。
        assert_eq!(
            url.candidate_urls,
            vec![
                "http://m701.music.126.net/20260101120000/9e2c8a42b0e1f3a7c5d6081112aab3cc/jdymusic/186016.mp3".to_string(),
                "http://m804.music.126.net/20260101120000/9e2c8a42b0e1f3a7c5d6081112aab3cc/jdymusic/186016.mp3".to_string(),
                "http://m810.music.126.net/20260101120000/9e2c8a42b0e1f3a7c5d6081112aab3cc/jdymusic/186016.flac".to_string(),
            ]
        );
        // 旁路：legacy 提升为无元数据端点，结构化条目保留逐条元数据。
        let endpoints = &resolved.fallback_endpoints;
        assert_eq!(endpoints.len(), 3);
        assert!(endpoints[0].headers.is_empty() && endpoints[0].quality.is_none());
        assert!(endpoints[1].headers.is_empty() && endpoints[1].quality.is_none());
        assert_eq!(
            endpoints[2].headers,
            vec![("User-Agent".to_string(), "voicefox".to_string())]
        );
        assert_eq!(endpoints[2].content_length, Some(24567890));
        assert_eq!(endpoints[2].quality, Some(StreamQuality::Lossless));
        // 伴随字段：voicefox 无槽位的 musid-dli 语义不静默丢。
        assert_eq!(resolved.expires_at_unix, None);
        assert_eq!(resolved.format, AudioFormat::Mp3);
        assert_eq!(resolved.index_range, None);
        assert_eq!(resolved.init_range, None);
    }

    #[test]
    fn legacy_stream_json_without_new_fields_maps() {
        // musid-dli models/stream.rs T1 的 legacy 形状（checksum 无算法字段
        // = MD5、无 fallback_endpoints、无 size_is_advisory）。
        let legacy = r#"{
            "url": "https://cdn.example/a.mp3",
            "quality": "High",
            "format": "Mp3",
            "content_length": 123,
            "checksum": "abc",
            "headers": [["Referer", "https://x.example"]],
            "fallback_urls": ["https://cdn.example/b.mp3"],
            "max_chunk_size": 1048576
        }"#;
        let info: StreamInfo = from_str(legacy).expect("legacy json loads");
        let resolved = stream_to_resolved_url(&info);
        assert!(resolved.notes.is_empty());
        assert_eq!(resolved.url.md5.as_deref(), Some("abc"), "缺省算法 = MD5");
        assert_eq!(resolved.url.size, Some(123));
        assert_eq!(
            resolved.url.max_chunk_size, 1048576,
            "有 size 时 chunk 界生效"
        );
        assert_eq!(
            resolved.url.candidate_urls,
            vec!["https://cdn.example/b.mp3"]
        );
    }

    #[test]
    fn sha_checksum_is_dropped_with_note() {
        let payload = r#"{
            "url": "https://cdn.example/a.flac",
            "quality": "Lossless",
            "format": "Flac",
            "content_length": 42,
            "checksum": "deadbeef",
            "checksum_algorithm": "sha1"
        }"#;
        let info: StreamInfo = from_str(payload).expect("sha1 json loads");
        let resolved = stream_to_resolved_url(&info);
        assert_eq!(resolved.url.md5, None, "SHA1 值绝不可喂 md5 门");
        assert_eq!(resolved.notes, vec![NOTE_CHECKSUM_DROPPED]);
    }

    #[test]
    fn strict_chunk_is_disabled_without_declared_size() {
        let payload = r#"{
            "url": "https://cdn.example/a.mp3",
            "quality": "High",
            "max_chunk_size": 1048576
        }"#;
        let info: StreamInfo = from_str(payload).expect("chunk json loads");
        let resolved = stream_to_resolved_url(&info);
        assert_eq!(
            resolved.url.max_chunk_size, 0,
            "无 size 时禁用 strict chunk"
        );
        assert_eq!(resolved.notes, vec![NOTE_STRICT_CHUNK_DISABLED]);
    }

    #[test]
    fn unknown_and_standard_qualities_degrade_with_notes() {
        let unknown: StreamInfo = from_value(json!({
            "url": "https://cdn.example/a.mp3",
            "quality": "Unknown"
        }))
        .expect("unknown json loads");
        let resolved = stream_to_resolved_url(&unknown);
        assert_eq!(resolved.url.quality, Quality::Low128);
        assert_eq!(resolved.notes, vec![NOTE_QUALITY_UNKNOWN]);
        assert_eq!(resolved.url.duration, Duration::ZERO, "缺省时长折 ZERO");

        let standard: StreamInfo = from_value(json!({
            "url": "https://cdn.example/a.mp3",
            "quality": "Standard"
        }))
        .expect("standard json loads");
        let resolved = stream_to_resolved_url(&standard);
        assert_eq!(resolved.url.quality, Quality::Low128);
        assert_eq!(resolved.notes, vec![NOTE_QUALITY_STANDARD]);
    }

    #[test]
    fn song_url_rebuilds_stream_info_for_download_assembly() {
        let info = netease_stream_fixture();
        let resolved = stream_to_resolved_url(&info);
        let rebuilt = song_url_to_stream(&resolved.url);
        assert_eq!(rebuilt.url, info.url);
        assert_eq!(rebuilt.quality, StreamQuality::High);
        assert_eq!(rebuilt.duration, Some(Duration::from_secs(258)));
        assert_eq!(rebuilt.content_length, Some(8523134));
        assert_eq!(
            rebuilt.md5_checksum().map(str::to_owned),
            resolved.url.md5,
            "md5 门语义在反向组装后保持"
        );
        assert_eq!(rebuilt.checksum_algorithm, None, "voicefox 只承载 MD5");
        assert_eq!(rebuilt.headers, info.headers);
        assert_eq!(rebuilt.fallback_urls, resolved.url.candidate_urls);
        assert_eq!(rebuilt.max_chunk_size, None);
        assert!(!rebuilt.size_is_advisory);
        // 可直接交给 musid-dli 下载适配器的形状检查（download_request_from_stream
        // 的入参面）；V0 只做映射，不接引擎。
        assert!(rebuilt.ordered_fallback_urls() == resolved.url.candidate_urls);
    }

    // --- 集合模型（fixture） ---

    #[test]
    fn playlist_fixture_maps_with_track_count_and_extra() {
        let md_playlist = from_str::<MdPlaylist>(fixture!("playlist_netease.json"))
            .expect("playlist fixture must parse");
        let playlist = playlist_to_lx(&md_playlist).expect("netease provider must map");
        assert_eq!(playlist.source, SourceId::Wy);
        assert_eq!(playlist.id, "1");
        assert_eq!(playlist.name, "热歌榜");
        assert_eq!(playlist.song_count, 145);
        assert_eq!(playlist.description.as_deref(), Some("每天更新的热门歌单"));
        assert_eq!(playlist.play_count, Some(7));
        assert_eq!(playlist.creator.as_deref(), Some("creator"));
        assert_eq!(
            playlist.link.as_deref(),
            Some("https://music.163.com/playlist?id=1")
        );
        assert_eq!(playlist.extra.get("hot").map(String::as_str), Some("1"));
    }

    #[test]
    fn playlist_category_maps_and_requires_provider() {
        let category = from_str::<MdPlaylistCategory>(fixture!("category_kuwo.json"))
            .expect("category fixture must parse");
        let mapped = playlist_category_to_lx(&category).expect("kuwo provider must map");
        assert_eq!(mapped.source, SourceId::Kw);
        assert_eq!(mapped.id, "10000");
        assert_eq!(mapped.name, "热歌");
        assert_eq!(mapped.group.as_deref(), Some("hot"));
        assert_eq!(mapped.count, 30);
        assert!(mapped.hot);

        let mut missing = category.clone();
        missing.provider = None;
        assert_eq!(
            playlist_category_to_lx(&missing),
            Err(AdapterError::MissingProvider)
        );
        let mut unmapped = category.clone();
        unmapped.provider = Some("youtube-music".to_string());
        assert_eq!(
            playlist_category_to_lx(&unmapped),
            Err(AdapterError::UnmappedProvider("youtube-music".to_string()))
        );
    }

    #[test]
    fn album_and_artist_fixtures_map_with_joined_artists() {
        let album =
            from_str::<MdAlbum>(fixture!("album_jamendo.json")).expect("album fixture must parse");
        let mapped = album_to_lx(&album).expect("jamendo provider must map");
        assert_eq!(mapped.source, SourceId::Jamendo);
        assert_eq!(mapped.id, "176100");
        assert_eq!(mapped.name, "LoveRecycle");
        assert_eq!(mapped.artist, "LoveRecycle");
        assert_eq!(
            mapped.cover_url.as_deref(),
            Some("https://cover.example/176100.jpg")
        );

        let artist = from_str::<MdArtist>(fixture!("artist_jamendo.json"))
            .expect("artist fixture must parse");
        let mapped = artist_to_lx(&artist).expect("jamendo provider must map");
        assert_eq!(mapped.source, SourceId::Jamendo);
        assert_eq!(mapped.id, "508325");
        assert_eq!(mapped.name, "LoveRecycle");
        assert_eq!(mapped.cover_url, None);
    }

    #[test]
    fn chart_fixture_maps_update_and_entry_rank() {
        let chart =
            from_str::<MdChart>(fixture!("chart_qqmusic.json")).expect("chart fixture must parse");
        let leaderboard = chart_to_leaderboard(&chart).expect("qqmusic provider must map");
        assert_eq!(leaderboard.source, SourceId::Tx);
        assert_eq!(leaderboard.id, "4");
        assert_eq!(leaderboard.name, "巅峰榜");
        assert_eq!(leaderboard.update.as_deref(), Some("每日更新"));

        let entry = ChartEntry {
            rank: 3,
            track: netease_track_fixture(),
        };
        let song = chart_entry_to_song(&entry).expect("netease provider must map");
        assert_eq!(song.extra.get(EXTRA_RANK).map(String::as_str), Some("3"));
    }

    #[test]
    fn search_page_maps_totals_and_rejects_unmapped_items() {
        let page = SearchPage {
            items: vec![netease_track_fixture()],
            total: Some(5),
            has_more: true,
        };
        let result = search_page_to_result(&page).expect("netease item must map");
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.total, 5);
        assert!(result.has_more);

        // total 缺省按 items 数收敛（saturating 到 u32）。
        let mut unknown_total = page.clone();
        unknown_total.total = None;
        let result = search_page_to_result(&unknown_total).expect("netease item must map");
        assert_eq!(result.total, 1);

        // 任一 item 的 provider 查不到：整体报错，不静默丢行。
        let mut bad = page.clone();
        bad.items[0].id = TrackId::new("youtube-music", "yt1");
        assert!(matches!(
            search_page_to_result(&bad),
            Err(AdapterError::UnmappedProvider(ref provider)) if provider == "youtube-music"
        ));
    }

    // --- 错误映射 ---

    #[test]
    fn errors_map_to_fetch_and_search_semantics() {
        use musid_dli::error::ErrorDetail;
        let network = MdError::Network {
            provider: Some("netease".to_string()),
            detail: None,
        };
        assert!(matches!(
            fetch_error_from_error(&network),
            FetchError::Network(_)
        ));
        assert!(matches!(
            search_error_from_error(&network),
            SearchError::Network(_)
        ));

        let timeout = MdError::Timeout;
        assert!(matches!(
            fetch_error_from_error(&timeout),
            FetchError::Network(_)
        ));

        let rate_limited = MdError::RateLimited {
            provider: None,
            retry_after: None,
            detail: None,
        };
        assert!(matches!(
            fetch_error_from_error(&rate_limited),
            FetchError::TooManyRequests
        ));
        assert!(
            matches!(search_error_from_error(&rate_limited), SearchError::Other(text) if text.contains("rate limited"))
        );

        let not_found = MdError::NotFound;
        assert!(matches!(
            fetch_error_from_error(&not_found),
            FetchError::NotFound
        ));
        assert!(matches!(
            search_error_from_error(&not_found),
            SearchError::Other(_)
        ));

        let invalid = MdError::InvalidResponse {
            provider: None,
            code: Some(881),
            detail: None,
        };
        assert!(matches!(
            fetch_error_from_error(&invalid),
            FetchError::Parse(_)
        ));
        assert!(matches!(
            search_error_from_error(&invalid),
            SearchError::Api(_)
        ));

        let serialization = MdError::Serialization(from_str::<Value>("not-json").unwrap_err());
        assert!(matches!(
            fetch_error_from_error(&serialization),
            FetchError::Parse(_)
        ));
        assert!(matches!(
            search_error_from_error(&serialization),
            SearchError::Parse(_)
        ));

        let unsupported = MdError::Unsupported("netease".to_string());
        assert!(
            matches!(fetch_error_from_error(&unsupported), FetchError::Other(text) if text.contains("该音源不支持"))
        );
        assert!(
            matches!(search_error_from_error(&unsupported), SearchError::Other(text) if text.contains("该音源不支持"))
        );

        let auth = MdError::AuthenticationRequired {
            provider: None,
            detail: Some(ErrorDetail::new("login required")),
        };
        assert!(
            matches!(fetch_error_from_error(&auth), FetchError::Other(text) if text.contains("login required"))
        );

        let provider = MdError::Provider {
            provider: "netease".to_string(),
            message: "boom".to_string(),
        };
        assert!(
            matches!(fetch_error_from_error(&provider), FetchError::Other(text) if text.contains("boom"))
        );
    }

    // --- 能力位 / 登录态 ---

    #[test]
    fn capabilities_map_with_granularity_table() {
        let capabilities = ProviderCapabilities {
            track_search: true,
            stream_resolve: true,
            lyrics: true,
            playlists: true,
            albums: true,
            artists: true,
            charts: true,
            link_resolve: true,
            authentication: true,
            download: true,
            artwork: false,
        };
        let flags = ProviderFaceFlags {
            has_search_playlists: true,
            has_categories: true,
            has_user_playlists: true,
        };
        let mapped = capabilities_from_provider(capabilities, AuthCapability::Qr, flags);
        assert!(mapped.playlists && mapped.playlist_search && mapped.playlist_categories);
        assert!(mapped.album && mapped.artist && mapped.leaderboard);
        assert!(mapped.link_parse && mapped.qr_login);
        assert!(
            mapped.login,
            "authentication 位直接置 login（cookie 类语义）"
        );
        assert!(mapped.user_playlists);
        // musid-dli 无数据源：双栈期恒 false。
        assert!(!mapped.wechat_login);
        assert!(!mapped.vip_account);
    }

    #[test]
    fn login_bit_only_for_cookie_like_mechanisms() {
        // voicefox login 位语义是"支持配置登录 cookie"（traits/source.rs:57-58）：
        // Anonymous/OAuth/Password/Token 不是 cookie 配置入口，不置 login。
        let capabilities = ProviderCapabilities::default();
        let flags = ProviderFaceFlags::default();
        for mechanism in [
            AuthCapability::None,
            AuthCapability::Anonymous,
            AuthCapability::OAuth,
            AuthCapability::Password,
            AuthCapability::Token,
            AuthCapability::Other,
        ] {
            let mapped = capabilities_from_provider(capabilities, mechanism, flags);
            assert!(!mapped.login, "{mechanism:?} 不应被当成可配置 cookie 登录");
            assert!(!mapped.qr_login);
        }
        // 显式 Cookie 机制置 login；Qr 单独置 qr_login。
        let mapped = capabilities_from_provider(capabilities, AuthCapability::Cookie, flags);
        assert!(mapped.login);
        assert!(!mapped.qr_login);
        let qr_only = capabilities_from_provider(capabilities, AuthCapability::Qr, flags);
        assert!(qr_only.qr_login);
        assert!(!qr_only.login, "纯扫码 provider 不该出现 cookie 登录入口");

        // 矛盾声明的处理约定（设计文档 §2.8 登记）：authentication=true 与
        // 非 Cookie 机制同时声明时按 authentication 位胜出。musid-dli 现有
        // 12 个 provider 无此组合（kuwo false+Anonymous、fivesing false+None
        // 有上游测试 pin）；V1 调用点如遇矛盾组合须先在 musid-dli 侧修
        // descriptor。
        let contradictory = capabilities_from_provider(
            ProviderCapabilities {
                authentication: true,
                ..ProviderCapabilities::default()
            },
            AuthCapability::Anonymous,
            flags,
        );
        assert!(
            contradictory.login,
            "矛盾声明按 authentication 位胜出的登记约定"
        );
        assert!(!contradictory.qr_login);
    }

    #[test]
    fn auth_state_maps_to_boolean_login_flag() {
        assert!(is_logged_in_from_auth_state(AuthState::Authenticated));
        assert!(is_logged_in_from_auth_state(AuthState::Refreshing));
        for state in [
            AuthState::LoggedOut,
            AuthState::PendingQrScan,
            AuthState::Expired,
            AuthState::Invalidated,
            AuthState::ReauthorizationRequired,
        ] {
            assert!(!is_logged_in_from_auth_state(state));
        }
    }

    // --- 会话 ---

    #[test]
    fn provider_session_fixture_maps_to_source_session() {
        let session = from_str::<ProviderSession>(fixture!("session_netease.json"))
            .expect("session fixture must parse");
        let mapped = source_session_from_provider(&session);
        assert_eq!(
            mapped.cookies.get("MUSIC_U").map(String::as_str),
            Some("dummy-cookie-value")
        );
        assert_eq!(
            mapped.extra.get("token:refresh").map(String::as_str),
            Some("dummy-refresh-token")
        );
        assert_eq!(mapped.user_name.as_deref(), Some("测试账号"));
        assert_eq!(mapped.user_id.as_deref(), Some("42"));
        assert_eq!(
            mapped.extra.get(EXTRA_EXPIRES_AT_UNIX).map(String::as_str),
            Some("1800000000")
        );
        assert_eq!(
            mapped.extra.get(EXTRA_UPDATED_AT_UNIX).map(String::as_str),
            Some("1799990000")
        );
        assert_eq!(
            mapped.extra.get(EXTRA_REGION).map(String::as_str),
            Some("CN")
        );

        // 反向：身份与凭据保真往返。
        let rebuilt = provider_session_from_source_session(&mapped);
        assert_eq!(rebuilt.cookies, session.cookies);
        assert_eq!(rebuilt.tokens, session.tokens);
        assert_eq!(rebuilt.user_id, session.user_id);
        assert_eq!(rebuilt.display_name, session.display_name);
        assert_eq!(rebuilt.expires_at, session.expires_at);
        assert_eq!(rebuilt.updated_at, session.updated_at);
        assert_eq!(rebuilt.region, session.region);
    }

    // --- 扫码登录 ---

    #[test]
    fn qr_status_maps_nine_states_with_matching_transient_and_terminal() {
        let pairs = [
            (QrLoginStatus::Waiting, LxQrLoginStatus::Waiting),
            (QrLoginStatus::Scanned, LxQrLoginStatus::Scanned),
            (QrLoginStatus::Success, LxQrLoginStatus::Success),
            (QrLoginStatus::Expired, LxQrLoginStatus::Expired),
            (QrLoginStatus::RiskControl, LxQrLoginStatus::RiskControl),
            (QrLoginStatus::ServerError, LxQrLoginStatus::ServerError),
            (QrLoginStatus::Failed, LxQrLoginStatus::Failed),
            (QrLoginStatus::NetworkError, LxQrLoginStatus::NetworkError),
            (
                QrLoginStatus::InvalidSession,
                LxQrLoginStatus::InvalidSession,
            ),
        ];
        for (md, lx) in pairs {
            assert_eq!(qr_login_status(md), lx);
            assert_eq!(md.is_transient(), lx.is_transient());
            assert_eq!(md.is_terminal(), lx.is_terminal());
        }
    }

    #[test]
    fn qr_session_backfills_source_and_kind() {
        let session = QrLoginSession {
            key: "poll-key".to_string(),
            url: "https://h5.example/qr".to_string(),
            expires_in: 120,
            image_png: Some("data:image/png;base64,abc".to_string()),
            kind: QrLoginKind::WeChat,
        };
        // namespace 表的 provider 侧用 musid-dli 名（qqmusic），不是 voicefox 名（tx）。
        let mapped =
            qr_login_session_from_md(&session, "qqmusic").expect("qqmusic provider must map");
        assert_eq!(mapped.source, SourceId::Tx);
        assert_eq!(mapped.kind, LxQrLoginKind::WeChat);
        assert_eq!(mapped.key, "poll-key");
        assert_eq!(mapped.expires_in, 120);
        assert_eq!(
            mapped.image_png.as_deref(),
            Some("data:image/png;base64,abc")
        );

        assert_eq!(
            qr_login_session_from_md(&session, "youtube-music"),
            Err(AdapterError::UnmappedProvider("youtube-music".to_string()))
        );
    }

    #[test]
    fn qr_poll_folds_user_name_without_losing_absence() {
        let fold = |display_name: Option<&str>, user_id: Option<&str>| {
            qr_login_result_from_poll(&QrLoginPoll {
                status: QrLoginStatus::Success,
                message: "ok".to_string(),
                user_id: user_id.map(str::to_string),
                display_name: display_name.map(str::to_string),
            })
        };
        let named = fold(Some("名字"), Some("u1"));
        assert_eq!(
            named.user_name.as_deref(),
            Some("名字"),
            "display_name 优先"
        );
        let id_only = fold(None, Some("u1"));
        assert_eq!(id_only.user_name.as_deref(), Some("u1"), "回退 user_id");
        let anonymous = fold(None, None);
        assert_eq!(anonymous.user_name, None, "双缺保持 None，绝不折叠成空串");
        assert!(matches!(anonymous.status, LxQrLoginStatus::Success));
    }

    // --- Transport 占位 ---

    #[test]
    fn transport_injection_placeholder_records_budget() {
        let injection = TransportInjection::new(Some("http://127.0.0.1:7890".to_string()), 30);
        assert_eq!(injection.proxy_url(), Some("http://127.0.0.1:7890"));
        assert_eq!(injection.timeout_secs(), 30);
        assert_eq!(TransportInjection::new(None, 15).proxy_url(), None);
    }

    // --- 歌词跨源补全候选（LyricSearchResult 专属映射，非 Track 复用） ---

    #[test]
    fn lyric_search_result_maps_to_cross_source_candidate() {
        // namespace 表的 provider 侧用 musid-dli 名（kugou），不是 voicefox 名（kg）。
        let result = MdLyricSearchResult {
            provider: "kugou".to_string(),
            id: "abc123".to_string(),
            title: "晴天".to_string(),
            artists: vec!["周杰伦".to_string(), "费玉清".to_string()],
            album: Some("叶惠美".to_string()),
            duration_ms: Some(269000),
            match_score: Some(90),
        };
        let candidate = lyric_search_candidate_from_md(&result).expect("kugou provider must map");
        assert_eq!(candidate.song.source, SourceId::Kg);
        assert_eq!(candidate.song.id, "abc123");
        assert_eq!(candidate.song.name, "晴天");
        assert_eq!(candidate.song.singer, "周杰伦、费玉清");
        assert_eq!(candidate.song.album_name, "叶惠美");
        assert_eq!(candidate.song.duration, Duration::from_millis(269000));
        assert_eq!(
            candidate.match_score,
            Some(90),
            "match_score 原样透传供排序"
        );

        // 字段缺省口径与 Track 映射同规则：None 时长折 ZERO、None 专辑折空串。
        let mut sparse = result.clone();
        sparse.provider = "youtube-music".to_string();
        sparse.album = None;
        sparse.duration_ms = None;
        sparse.match_score = None;
        assert!(matches!(
            lyric_search_candidate_from_md(&sparse),
            Err(AdapterError::UnmappedProvider(ref provider)) if provider == "youtube-music"
        ));
        sparse.provider = "kugou".to_string();
        let sparse = lyric_search_candidate_from_md(&sparse).expect("kugou provider must map");
        assert_eq!(sparse.song.album_name, "");
        assert_eq!(sparse.song.duration, Duration::ZERO);
        assert_eq!(sparse.match_score, None);
    }

    #[test]
    fn song_url_enrichment_matches_builtin_producer_behavior() {
        // voicefox 内置源从歌曲模型补 cover_url/qualities（wy/url.rs、kg/url.rs）；
        // musid-dli StreamInfo 没有来源字段，由调用点用本函数补齐。
        let info = netease_stream_fixture();
        let mut url = stream_to_resolved_url(&info).url;
        assert_eq!(url.cover_url, None);
        assert!(url.qualities.is_empty());

        let mut song = SongInfo::new(
            "1".to_string(),
            SourceId::Wy,
            "名".to_string(),
            "A".to_string(),
        );
        song.cover_url = Some("https://cover.example/1.jpg".to_string());
        song.qualities = [Quality::Low128, Quality::Flac].into_iter().collect();
        enrich_song_url_from_song(&mut url, &song);
        assert_eq!(
            url.cover_url.as_deref(),
            Some("https://cover.example/1.jpg")
        );
        assert_eq!(url.qualities, vec![Quality::Low128, Quality::Flac]);

        // 已有 cover_url 不覆盖：fill-if-none 是给"流解析自带封面"的调用点
        // 留的口子。内置源实况是恒取 song.cover_url（wy/url.rs:246-262、
        // kg/url.rs:303-312，没有"解析出的封面"可优先）；因 StreamInfo 无
        // 封面来源字段、这里 url.cover_url 恒 None，fill-if-none 与无条件
        // 覆盖在现网行为上等价。
        url.cover_url = Some("https://resolved.example/cover.jpg".to_string());
        enrich_song_url_from_song(&mut url, &song);
        assert_eq!(
            url.cover_url.as_deref(),
            Some("https://resolved.example/cover.jpg")
        );
    }

    // --- 歌词（feature musid-dli-adapter-lyrics） ---

    #[cfg(feature = "musid-dli-adapter-lyrics")]
    mod lyrics {
        use super::*;
        use std::collections::BTreeMap;

        fn document() -> musid_dli::models::LyricDocument {
            let mut translations = BTreeMap::new();
            translations.insert("zh".to_string(), "line translation".to_string());
            musid_dli::models::LyricDocument {
                format: musid_dli::models::LyricFormat::Krc,
                original: "[00:01.00]你好\n[00:05.00]世界".to_string(),
                translation: Some("hello".to_string()),
                romanization: Some("nihao".to_string()),
                lines: vec![
                    musid_dli::models::LyricLine {
                        start_ms: 1000,
                        end_ms: None,
                        text: "你好".to_string(),
                        translations,
                        romanization: None,
                        words: vec![],
                    },
                    musid_dli::models::LyricLine {
                        start_ms: 5000,
                        end_ms: Some(6000),
                        text: "世界".to_string(),
                        translations: BTreeMap::new(),
                        romanization: None,
                        words: vec![],
                    },
                ],
            }
        }

        #[test]
        fn document_maps_to_lx_data_with_explicit_degradation() {
            let data = lyric_document_to_lx_data(&document());
            assert_eq!(
                data.lyric, "[00:01.00]你好\n[00:05.00]世界",
                "原文含 mm:ss.xx 括号，信原始文本"
            );
            assert_eq!(data.tlyric.as_deref(), Some("hello"));
            assert_eq!(data.rlyric.as_deref(), Some("nihao"));
            assert_eq!(data.lxlyric, None, "musid-dli 无 YRC/QRC 序列化器，恒 None");
            assert_eq!(
                data.raw_lrc.as_deref(),
                Some("[00:01.00]你好\n[00:05.00]世界")
            );
        }

        #[test]
        fn lyric_lines_follow_voicefox_duration_rules() {
            let lines = lyric_lines_to_lx(&document().lines);
            assert_eq!(lines.len(), 2);
            assert_eq!(lines[0].timestamp, 1000);
            assert_eq!(lines[0].duration, 4000, "无 end_ms 用下一行 start 差");
            assert_eq!(lines[1].timestamp, 5000);
            assert_eq!(lines[1].duration, 1000, "有 end_ms 用 end_ms-start_ms");
            let last_only = lyric_lines_to_lx(&document().lines[..1]);
            assert_eq!(last_only[0].duration, 0, "末行无 end_ms 时长为 0");
        }

        #[test]
        fn lx_data_round_trips_through_document() {
            let lx_data = lx_core::model::lyric::LyricData {
                lyric: "[00:01.00]你好\n[00:05.00]世界".to_string(),
                tlyric: Some("hello".to_string()),
                rlyric: None,
                lxlyric: None,
                raw_lrc: None,
            };
            let md_document =
                lyric_data_to_md_document(&lx_data, musid_dli::models::LyricFormat::Lrc)
                    .expect("LRC lyric must parse");
            assert_eq!(md_document.original, lx_data.lyric);
            assert_eq!(md_document.translation.as_deref(), Some("hello"));
            let back = lyric_document_to_lx_data(&md_document);
            assert_eq!(back.lyric, lx_data.lyric);
            assert_eq!(back.tlyric, lx_data.tlyric);
        }

        #[test]
        fn unparsable_lxlyric_is_a_hard_error() {
            let lx_data = lx_core::model::lyric::LyricData {
                lyric: "[00:01.00]你好".to_string(),
                tlyric: None,
                rlyric: None,
                lxlyric: Some("this is not a word timed payload".to_string()),
                raw_lrc: None,
            };
            let error = lyric_data_to_md_document(&lx_data, musid_dli::models::LyricFormat::Lrc)
                .expect_err("无法解析的逐字负载必须报错而不是静默降级");
            assert!(matches!(error, AdapterError::InvalidLyric(_)));
        }
    }

    // --- AdapterMusicSource（V1 双栈适配源） ---

    mod adapter {
        use super::*;
        use async_trait::async_trait;
        use musid_dli::provider::MusicProvider;
        use std::sync::Mutex;

        /// 行为可注入的 musid-dli provider 桩（搜索/流地址/歌词三面）。
        struct StubProvider {
            resolve_options: Mutex<Vec<ResolveOptions>>,
            stream: Option<StreamInfo>,
            #[cfg(feature = "musid-dli-adapter-lyrics")]
            document: Option<musid_dli::models::LyricDocument>,
        }

        impl MusicProvider for StubProvider {
            fn id(&self) -> &str {
                "netease"
            }
            fn name(&self) -> &str {
                "测试源"
            }
            fn capabilities(&self) -> ProviderCapabilities {
                ProviderCapabilities {
                    track_search: true,
                    stream_resolve: true,
                    lyrics: true,
                    ..Default::default()
                }
            }
        }

        #[async_trait]
        impl MdTrackSearch for StubProvider {
            async fn search(&self, _query: &SearchQuery) -> Result<SearchPage, MdError> {
                Ok(SearchPage {
                    items: vec![netease_track_fixture()],
                    total: Some(1),
                    has_more: false,
                })
            }
        }

        #[async_trait]
        impl MdStreamResolver for StubProvider {
            async fn resolve(
                &self,
                _track: &Track,
                options: &ResolveOptions,
            ) -> Result<StreamInfo, MdError> {
                self.resolve_options
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(*options);
                self.stream.clone().ok_or(MdError::NotFound)
            }
        }

        #[async_trait]
        #[cfg(feature = "musid-dli-adapter-lyrics")]
        impl MdLyricsProvider for StubProvider {
            async fn lyrics(
                &self,
                _track: &Track,
            ) -> Result<Option<musid_dli::models::LyricDocument>, MdError> {
                Ok(self.document.clone())
            }
        }

        fn adapter(stream: Option<StreamInfo>) -> AdapterMusicSource {
            adapter_with(Arc::new(StubProvider {
                resolve_options: Mutex::new(Vec::new()),
                stream,
                #[cfg(feature = "musid-dli-adapter-lyrics")]
                document: None,
            }))
        }

        /// 同一 provider 实例拆成三面（与 musid_bootstrap 的装配形态一致）。
        fn adapter_with(provider: Arc<StubProvider>) -> AdapterMusicSource {
            AdapterMusicSource::new(
                SourceId::Wy,
                "测试源（musid-dli）".to_string(),
                Some(provider.clone()),
                Some(provider.clone()),
                #[cfg(feature = "musid-dli-adapter-lyrics")]
                Some(provider),
                #[cfg(not(feature = "musid-dli-adapter-lyrics"))]
                None,
                TransportInjection::new(None, 15),
            )
        }

        fn wy_song() -> SongInfo {
            let mut song = SongInfo::new(
                "789".to_string(),
                SourceId::Wy,
                "老接口".to_string(),
                "周杰伦、费玉清".to_string(),
            );
            song.cover_url = Some("https://cover.example/789.jpg".to_string());
            song.qualities = [Quality::High320, Quality::Flac].into_iter().collect();
            song
        }

        #[tokio::test]
        async fn adapter_search_maps_page_to_result() {
            let adapter = adapter(None);
            let result = adapter.search("老接口", 1, 10).await.expect("搜索面映射");
            assert_eq!(result.items.len(), 1);
            assert_eq!(result.items[0].source, SourceId::Wy);
            assert_eq!(result.items[0].id, "789");
            assert_eq!(result.total, 1);
        }

        #[tokio::test]
        async fn adapter_resolve_passes_explicit_quality_and_enriches_from_song() {
            // 档位经桩记录：请求 Flac 必须显式映射为 Lossless（禁 Default=High）。
            let capture = Arc::new(StubProvider {
                resolve_options: Mutex::new(Vec::new()),
                stream: Some(netease_stream_fixture()),
                #[cfg(feature = "musid-dli-adapter-lyrics")]
                document: None,
            });
            let adapter = AdapterMusicSource::new(
                SourceId::Wy,
                "capture".to_string(),
                None,
                Some(Arc::clone(&capture) as Arc<dyn MdStreamResolver>),
                None,
                TransportInjection::new(None, 15),
            );
            let song = wy_song();
            let url = adapter
                .get_song_url(&song, Quality::Flac)
                .await
                .expect("流地址面映射");
            let captured = capture
                .resolve_options
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            assert_eq!(
                captured
                    .iter()
                    .map(|options| options.quality)
                    .collect::<Vec<_>>(),
                vec![StreamQuality::Lossless],
                "请求 Flac 必须显式映射为 Lossless，而不是 Default=High"
            );
            // enrich（§3 第 13、14 条）：cover_url/qualities 从歌曲模型补齐。
            assert_eq!(
                url.cover_url.as_deref(),
                Some("https://cover.example/789.jpg")
            );
            assert_eq!(
                url.qualities,
                vec![Quality::High320, Quality::Flac],
                "song.qualities 顺序回填"
            );
            assert_eq!(url.quality, Quality::High320, "实际档位回填");
        }

        #[tokio::test]
        async fn expired_stream_url_is_rejected_not_forwarded() {
            // G4：expires_at_unix 已过期的地址绝不透传播放器，视为失败回落。
            let mut info = netease_stream_fixture();
            info.expires_at_unix = Some(1);
            let adapter = adapter(Some(info));
            let error = adapter
                .get_song_url(&wy_song(), Quality::High320)
                .await
                .expect_err("过期地址必须报错");
            assert!(
                error.to_string().contains("已过期"),
                "过期错误语义: {error}"
            );
        }

        #[tokio::test]
        async fn js_sourced_song_keeps_platform_id_through_track() {
            // JS 音源搜到的歌（Local + extra[source]）：按平台标记还原 provider，
            // 其 id 即平台歌曲 id——不重编、不丢。
            let mut js_song = SongInfo::new(
                "789".to_string(),
                SourceId::Local,
                "老接口".to_string(),
                "周杰伦、费玉清".to_string(),
            );
            js_song.extra.insert("source".to_string(), "wy".to_string());
            let track = adapter_track_for_song(&js_song).expect("JS 歌必须能组 Track");
            assert_eq!(track.id, TrackId::new("netease", "789"));

            // 真本地文件（无平台标记）：manager 侧早退，这里防御性报错。
            let local = SongInfo::new(
                "/music/a.flac".to_string(),
                SourceId::Local,
                "本地歌".to_string(),
                "本地歌手".to_string(),
            );
            assert!(matches!(
                adapter_track_for_song(&local),
                Err(AdapterError::UnmappedProvider(provider)) if provider == "local"
            ));
        }

        #[tokio::test]
        async fn session_and_login_faces_stay_with_the_legacy_path() {
            // P10：V1 登录/会话四面全部走旧路径——适配源不覆写任何登录方法，
            // is_logged_in 恒 false，QR/直解保持 trait 默认错误。
            let adapter = adapter(None);
            assert!(!adapter.is_logged_in(), "musid 影子源恒未登录");
            assert!(adapter.logout().is_ok(), "logout 无会话可清，no-op");
            assert!(adapter.create_qr_login().await.is_err(), "扫码走旧路径");
            assert!(
                adapter.check_qr_login("key").await.is_err(),
                "扫码轮询走旧路径"
            );
            assert!(adapter.parse_link("https://music.163.com/x").await.is_err());
            // 封面面只回填歌曲模型上的 cover_url。
            assert_eq!(
                adapter
                    .get_cover_url(&wy_song())
                    .await
                    .expect("歌曲自带封面"),
                "https://cover.example/789.jpg"
            );
            let bare = SongInfo::new(
                "1".to_string(),
                SourceId::Wy,
                "名".to_string(),
                "歌手".to_string(),
            );
            assert!(matches!(
                adapter.get_cover_url(&bare).await,
                Err(FetchError::NotFound)
            ));
        }

        #[test]
        fn transport_budget_is_recorded_for_diagnostics() {
            let adapter = adapter(None);
            assert_eq!(adapter.transport_injection().timeout_secs(), 15);
            assert_eq!(adapter.transport_injection().proxy_url(), None);
        }

        #[cfg(feature = "musid-dli-adapter-lyrics")]
        #[tokio::test]
        async fn adapter_lyric_maps_document_and_not_found() {
            use musid_dli::models::{LyricDocument, LyricFormat, LyricLine};

            let document = LyricDocument {
                format: LyricFormat::Lrc,
                original: "[00:01.00]你好".to_string(),
                translation: None,
                romanization: None,
                lines: vec![LyricLine {
                    start_ms: 1000,
                    end_ms: None,
                    text: "你好".to_string(),
                    translations: BTreeMap::new(),
                    romanization: None,
                    words: vec![],
                }],
            };

            let with_lyrics = AdapterMusicSource::new(
                SourceId::Wy,
                "lyrics".to_string(),
                None,
                None,
                Some(Arc::new(StubProvider {
                    resolve_options: Mutex::new(Vec::new()),
                    stream: None,
                    document: Some(document),
                })),
                TransportInjection::new(None, 15),
            );
            let data = with_lyrics.get_lyric(&wy_song()).await.expect("歌词面映射");
            assert_eq!(data.lyric, "[00:01.00]你好");
            assert_eq!(data.lxlyric, None, "musid-dli 无 YRC/QRC 序列化器");

            let without_lyrics = AdapterMusicSource::new(
                SourceId::Wy,
                "none".to_string(),
                None,
                None,
                Some(Arc::new(StubProvider {
                    resolve_options: Mutex::new(Vec::new()),
                    stream: None,
                    document: None,
                })),
                TransportInjection::new(None, 15),
            );
            assert!(matches!(
                without_lyrics.get_lyric(&wy_song()).await,
                Err(FetchError::NotFound)
            ));
        }

        // --- V3 s2 歌词文档缓存（docs §13.2；musid-dli-adapter-lyrics-cache） ---

        /// 歌词面计数桩：只实现歌词 provider（search/stream 槽恒 None），
        /// 以调用次数钉住缓存行为。
        #[cfg(feature = "musid-dli-adapter-lyrics-cache")]
        struct CountingLyricStub {
            document: Option<musid_dli::models::LyricDocument>,
            calls: std::sync::atomic::AtomicUsize,
        }

        #[cfg(feature = "musid-dli-adapter-lyrics-cache")]
        impl musid_dli::provider::MusicProvider for CountingLyricStub {
            fn id(&self) -> &str {
                "netease"
            }
            fn name(&self) -> &str {
                "计数源"
            }
            fn capabilities(&self) -> ProviderCapabilities {
                ProviderCapabilities {
                    lyrics: true,
                    ..Default::default()
                }
            }
        }

        #[cfg(feature = "musid-dli-adapter-lyrics-cache")]
        #[async_trait]
        impl MdLyricsProvider for CountingLyricStub {
            async fn lyrics(
                &self,
                _track: &Track,
            ) -> Result<Option<musid_dli::models::LyricDocument>, MdError> {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(self.document.clone())
            }
        }

        #[cfg(feature = "musid-dli-adapter-lyrics-cache")]
        fn counting_adapter(
            document: Option<musid_dli::models::LyricDocument>,
        ) -> (AdapterMusicSource, Arc<CountingLyricStub>) {
            let stub = Arc::new(CountingLyricStub {
                document,
                calls: std::sync::atomic::AtomicUsize::new(0),
            });
            let adapter = AdapterMusicSource::new(
                SourceId::Wy,
                "缓存源（musid-dli）".to_string(),
                None,
                None,
                Some(Arc::clone(&stub) as Arc<dyn MdLyricsProvider>),
                TransportInjection::new(None, 15),
            );
            (adapter, stub)
        }

        #[cfg(feature = "musid-dli-adapter-lyrics-cache")]
        fn lrc_document() -> musid_dli::models::LyricDocument {
            use musid_dli::models::{LyricDocument, LyricFormat, LyricLine};
            LyricDocument {
                format: LyricFormat::Lrc,
                original: "[00:01.00]你好".to_string(),
                translation: None,
                romanization: None,
                lines: vec![LyricLine {
                    start_ms: 1000,
                    end_ms: None,
                    text: "你好".to_string(),
                    translations: BTreeMap::new(),
                    romanization: None,
                    words: vec![],
                }],
            }
        }

        #[cfg(feature = "musid-dli-adapter-lyrics-cache")]
        #[tokio::test]
        async fn lyrics_cache_hit_skips_the_second_provider_call() {
            let (adapter, stub) = counting_adapter(Some(lrc_document()));

            let first = adapter.get_lyric(&wy_song()).await.expect("首取有词");
            assert_eq!(first.lyric, "[00:01.00]你好");
            assert_eq!(stub.calls.load(std::sync::atomic::Ordering::SeqCst), 1);

            let second = adapter.get_lyric(&wy_song()).await.expect("缓存命中");
            assert_eq!(second.lyric, "[00:01.00]你好");
            assert_eq!(
                stub.calls.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "缓存命中不再打 provider"
            );
        }

        #[cfg(feature = "musid-dli-adapter-lyrics-cache")]
        #[tokio::test]
        async fn lyrics_cached_absence_answers_not_found_without_recalls() {
            let (adapter, stub) = counting_adapter(None);

            assert!(matches!(
                adapter.get_lyric(&wy_song()).await,
                Err(FetchError::NotFound)
            ));
            assert!(matches!(
                adapter.get_lyric(&wy_song()).await,
                Err(FetchError::NotFound)
            ));
            assert_eq!(
                stub.calls.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "确认无词缓存后不再打 provider"
            );
        }

        #[cfg(feature = "musid-dli-adapter-lyrics-cache")]
        #[tokio::test]
        async fn lyrics_errors_are_never_cached() {
            struct ErringStub;
            impl musid_dli::provider::MusicProvider for ErringStub {
                fn id(&self) -> &str {
                    "netease"
                }
                fn name(&self) -> &str {
                    "错误源"
                }
                fn capabilities(&self) -> ProviderCapabilities {
                    ProviderCapabilities {
                        lyrics: true,
                        ..Default::default()
                    }
                }
            }
            #[async_trait]
            impl MdLyricsProvider for ErringStub {
                async fn lyrics(
                    &self,
                    _track: &Track,
                ) -> Result<Option<musid_dli::models::LyricDocument>, MdError> {
                    Err(MdError::Provider {
                        provider: "netease".into(),
                        message: "boom".into(),
                    })
                }
            }

            let adapter = AdapterMusicSource::new(
                SourceId::Wy,
                "错误源（musid-dli）".to_string(),
                None,
                None,
                Some(Arc::new(ErringStub)),
                TransportInjection::new(None, 15),
            );

            assert!(adapter.get_lyric(&wy_song()).await.is_err());
            assert!(adapter.get_lyric(&wy_song()).await.is_err());
            // 错误不落缓存：第三次仍打到 provider（无计数桩，用行为验证——
            // 若被缓存，两次都会是同一个 Err 变体；此处 Network Err 每次
            // 重新产生，缓存命中会把它变成 NotFound，故 Err 变体即证据）。
            assert!(matches!(
                adapter.get_lyric(&wy_song()).await,
                Err(FetchError::Other(_))
            ));
        }
    }

    // --- V2 逐源路由注册表（docs §12；jamendo = 切片 1 模板） ---

    mod v2_registry {
        use super::*;
        use crate::parity::ParityFace;
        use async_trait::async_trait;
        use musid_dli::provider::MusicProvider;

        #[test]
        fn registry_ledger_pins_the_slice_states() {
            // 切片 1（jamendo）：搜索 + 流地址 Migrated；歌词 LegacyOnly 并
            // 登记原因（musid stub 的 Other 不许污染旧路径的 NotFound 语义）。
            let jamendo = source_route_entry(SourceId::Jamendo).expect("jamendo 在注册表");
            assert_eq!(jamendo.slice, "v2-jamendo（已落地）");
            assert_eq!(jamendo.search, FaceRoute::Migrated);
            assert_eq!(jamendo.song_url, FaceRoute::Migrated);
            assert!(matches!(jamendo.lyric, FaceRoute::LegacyOnly(_)));
            assert!(
                !face_route(ParityFace::Lyric, SourceId::Jamendo).routes_to_musid(),
                "jamendo 歌词面恒旧路径"
            );

            // 切片 2（fivesing）：三面全 Migrated（可路由依据 = musid
            // FivesingProvider 的 track_search/stream_resolve/lyrics 全 true、
            // authentication=false 无登录态，musid-dli source_fivesing.rs:494-511
            // 有测试 pin；两侧差异登记见本行注释与 docs §12.2/§12.4）。
            let fivesing = source_route_entry(SourceId::Fivesing).expect("fivesing 在注册表");
            assert_eq!(fivesing.slice, "v2-fivesing（已落地）");
            assert_eq!(fivesing.search, FaceRoute::Migrated);
            assert_eq!(fivesing.song_url, FaceRoute::Migrated);
            assert_eq!(fivesing.lyric, FaceRoute::Migrated);
            for face in [ParityFace::Search, ParityFace::SongUrl, ParityFace::Lyric] {
                assert!(
                    face_route(face, SourceId::Fivesing).routes_to_musid(),
                    "fivesing {} 三面全路由",
                    face.as_str()
                );
            }

            // 切片 3-7（apple/bilibili/migu/kuwo/kugou，本次落地）：账面
            // 逐源 pin——apple/migu/kuwo/kugou 三面全 Migrated（可路由
            // 依据与登记差异见各注册表行注释与 docs §12.2/§12.4）；
            // bilibili 搜索/流地址 Migrated、歌词 LegacyOnly（双侧皆无）。
            for source in [SourceId::Apple, SourceId::Mg, SourceId::Kw, SourceId::Kg] {
                let row = source_route_entry(source).expect("切片源在注册表");
                assert!(
                    row.slice.ends_with("（已落地）"),
                    "{} 行必须标已落地",
                    source.as_str()
                );
                for face in [ParityFace::Search, ParityFace::SongUrl, ParityFace::Lyric] {
                    assert_eq!(
                        face_route(face, source),
                        FaceRoute::Migrated,
                        "{} 切片已落地，三面全路由",
                        source.as_str()
                    );
                }
            }
            let bili = source_route_entry(SourceId::Bili).expect("bilibili 在注册表");
            assert_eq!(bili.slice, "v2-bilibili（已落地）");
            assert_eq!(bili.search, FaceRoute::Migrated);
            assert_eq!(bili.song_url, FaceRoute::Migrated);
            assert!(matches!(bili.lyric, FaceRoute::LegacyOnly(_)));
            assert!(
                face_route(ParityFace::Search, SourceId::Bili).routes_to_musid()
                    && face_route(ParityFace::SongUrl, SourceId::Bili).routes_to_musid(),
                "bilibili 搜索/流地址两面路由（三件结构前置已落地）"
            );
            assert!(
                !face_route(ParityFace::Lyric, SourceId::Bili).routes_to_musid(),
                "bilibili 歌词面恒旧路径（双侧皆无）"
            );

            // 切片 8-11（tx/wy/qianqian/joox，v2-batch-2 落地）：三面全
            // Migrated（可路由依据 = musid 侧三面 capabilities 全 true；
            // 登录/扫码、歌手、集合面恒旧路径——登记见各注册表行注释与
            // docs §12.2/§12.4；行为与 V1 一致——三面 V1 起即放行，本批
            // 只补对账与登记）。
            for source in [
                SourceId::Tx,
                SourceId::Wy,
                SourceId::Qianqian,
                SourceId::Joox,
            ] {
                let row = source_route_entry(source).expect("切片源在注册表");
                assert!(
                    row.slice.ends_with("（已落地）"),
                    "{} 行必须标已落地",
                    source.as_str()
                );
                for face in [ParityFace::Search, ParityFace::SongUrl, ParityFace::Lyric] {
                    assert_eq!(
                        face_route(face, source),
                        FaceRoute::Migrated,
                        "{} 切片已落地，三面全路由（与 V1 行为一致）",
                        source.as_str()
                    );
                }
            }

            // soda/local：无行 → LegacyOnly（命名表拒绝的镜像）。
            assert!(source_route_entry(SourceId::Soda).is_none());
            assert!(source_route_entry(SourceId::Local).is_none());
            for face in [ParityFace::Search, ParityFace::SongUrl, ParityFace::Lyric] {
                assert!(matches!(
                    face_route(face, SourceId::Soda),
                    FaceRoute::LegacyOnly(_)
                ));
                assert!(matches!(
                    face_route(face, SourceId::Local),
                    FaceRoute::LegacyOnly(_)
                ));
            }
        }

        #[test]
        fn registry_covers_every_namespace_mappable_source() {
            // 行覆盖 = §1 命名表的全部映射源（防漏行）：namespace 表新增
            // 映射而注册表漏行时，face_route 会把该源静默压成 LegacyOnly。
            let mapped: Vec<SourceId> = SourceId::all_known()
                .iter()
                .copied()
                .filter(|source| source_id_to_provider(*source).is_some())
                .collect();
            assert_eq!(mapped.len(), 11, "§1 命名表映射源数（V0 盘点口径）");
            // 行数本身也 pin（P5）：source_route_entry 用 find() 取首行，
            // 同源重复行不会被"可映射源⊆注册表"类断言抓到。
            assert_eq!(
                V2_ROUTE_REGISTRY.len(),
                mapped.len(),
                "注册表行数必须等于映射源数（重复行/缺行都算漂移）"
            );
            let mut seen: Vec<SourceId> = Vec::new();
            for row in V2_ROUTE_REGISTRY {
                assert!(
                    source_id_to_provider(row.source).is_some(),
                    "注册表行 {} 必须可映射（namespace 一致）",
                    row.source.as_str()
                );
                assert!(
                    !seen.contains(&row.source),
                    "注册表重复行：{}（find() 只取首行，重复行是账面歧义）",
                    row.source.as_str()
                );
                seen.push(row.source);
            }
            for source in mapped {
                assert!(
                    seen.contains(&source),
                    "{} 可映射但注册表缺行",
                    source.as_str()
                );
            }
        }

        #[test]
        fn migrated_route_sources_are_visible_to_the_build_wiring() {
            // P4：账面 Migrated 与构建形态可能脱节（子 feature 未启用时影子
            // 源不装配，运行时静默旧路径且 parity 恒零）。助手供
            // bootstrap_musid_stack 启动 warn 显式暴露该脱节。切片 1-11 落地
            // 后账面有 11 个 Migrated 源（注册表行序 = jamendo → fivesing →
            // apple → bilibili → migu → kuwo → kugou → tx → wy → qianqian
            // → joox；§1 命名表 11 个映射源全部完成逐源对账）。
            assert_eq!(
                migrated_route_sources(),
                vec![
                    SourceId::Jamendo,
                    SourceId::Fivesing,
                    SourceId::Apple,
                    SourceId::Bili,
                    SourceId::Mg,
                    SourceId::Kw,
                    SourceId::Kg,
                    SourceId::Tx,
                    SourceId::Wy,
                    SourceId::Qianqian,
                    SourceId::Joox,
                ]
            );
            assert_eq!(
                unregistered_migrated_sources(&[]),
                vec![
                    SourceId::Jamendo,
                    SourceId::Fivesing,
                    SourceId::Apple,
                    SourceId::Bili,
                    SourceId::Mg,
                    SourceId::Kw,
                    SourceId::Kg,
                    SourceId::Tx,
                    SourceId::Wy,
                ],
                "空注册列表 = 全部 Migrated 子 feature 未启用，warn 输入含九源\
                 （V4-s1/s2 起 joox 与 qianqian 不在其中：legacy 已删，适配面主注册表直装，§14.1/§14.3）"
            );
            assert_eq!(
                unregistered_migrated_sources(&[
                    SourceId::Jamendo,
                    SourceId::Fivesing,
                    SourceId::Apple,
                    SourceId::Bili,
                    SourceId::Mg,
                    SourceId::Kw,
                    SourceId::Kg,
                    SourceId::Tx,
                    SourceId::Wy,
                    SourceId::Qianqian,
                    SourceId::Joox,
                ]),
                Vec::<SourceId>::new(),
                "已装配的 Migrated 源不进 warn 输入"
            );
            // 部分装配形态（单子 feature 构建）：缺的 Migrated 源进 warn
            // 输入——bootstrap 切片测试按"不含本切片源"口径断言即源于此。
            assert_eq!(
                unregistered_migrated_sources(&[SourceId::Jamendo]),
                vec![
                    SourceId::Fivesing,
                    SourceId::Apple,
                    SourceId::Bili,
                    SourceId::Mg,
                    SourceId::Kw,
                    SourceId::Kg,
                    SourceId::Tx,
                    SourceId::Wy,
                ],
                "只装配 jamendo 时其余八个 Migrated 源是账面/构建脱节点\
                 （V4-s1/s2 起 joox 与 qianqian 恒主注册表装配，恒不进 warn）"
            );
        }

        fn jamendo_track_fixture() -> Track {
            from_str::<Track>(fixture!("track_jamendo.json"))
                .expect("jamendo track fixture must parse")
        }

        #[test]
        fn jamendo_track_fixture_maps_with_stream_extra_passthrough() {
            let track = jamendo_track_fixture();
            let song = track_to_song(&track).expect("jamendo provider must map");
            assert_eq!(song.source, SourceId::Jamendo);
            assert_eq!(song.id, "1538710");
            assert_eq!(song.name, "Love Recycle");
            assert_eq!(song.singer, "LoveRecycle");
            assert_eq!(song.album_name, "LoveRecycle");
            assert_eq!(song.album_id, "176100");
            assert_eq!(song.duration, Duration::from_secs(245));
            assert_eq!(
                song.qualities,
                [Quality::Low128].into_iter().collect(),
                "available_qualities [Low] 折档后进集合（§2.4 Low→Low128）"
            );
            // stream_url 伴随键经 extra 原样透传：流地址面路由的数据前提
            // （musid-dli JamendoProvider::resolve 从 extra["stream_url"]
            // 离线取址，source_jamendo.rs:402-444）。
            assert_eq!(
                song.extra.get("stream_url").map(String::as_str),
                Some("https://prod-1.storage.jamendo.com/download/track/1538710/mp32/")
            );
            assert_eq!(
                song.extra.get("stream_format").map(String::as_str),
                Some("Mp3")
            );
            assert_eq!(
                song.extra.get("stream_quality").map(String::as_str),
                Some("Low")
            );
            // 反向回组 Track：extra/id 保真（resolve 的数据链闭合——musid
            // 搜到的歌再请求地址时，extra 里的嵌入地址原样回到 musid）。
            let rebuilt = song_to_track(&song).expect("jamendo source must map");
            assert_eq!(rebuilt.id, track.id);
            assert_eq!(rebuilt.extra, track.extra);
        }

        #[test]
        fn jamendo_search_parity_normalized_equivalent() {
            // P2：legacy 侧用**真实的** `crate::jamendo::parse_track`
            // （jamendo/mod.rs:156-181，pub(crate) 可见性扩宽登记见 §12.5）
            // 从平台原始响应形状组出 SongInfo——legacy 或 musid 任一侧
            // parse 改字段，本测试必红。原始 item 与 musid-dli
            // tests/fixtures/jamendo/track_parse.json 的 "item" 同形（该
            // 文件自述为按 2026-11-18 live 捕获**重建**的形状记录，原始
            // 捕获未随上游仓提交——provenance 限定见 §12.3）。musid 侧：
            // track_to_song(voicefox 侧 wire 形状 fixture)。
            let platform_item = json!({
                "id": 1538710,
                "idstr": "",
                "type": "track",
                "query": "love",
                "score": 9.87,
                "status": 0,
                "uniqueId": 1538710,
                "artistId": 508325,
                "name": "Love Recycle",
                "duration": 245,
                "albumId": 176100,
                "artist": { "id": 508325, "name": "LoveRecycle" },
                "album": { "id": 176100, "name": "LoveRecycle" },
                "cover": {
                    "big": { "size": 300, "size300": "https://usercontent.jamendo.com?type=album&id=176100&width=300&trackid=1538710" },
                    "small": { "size": 200, "sizeN": "https://usercontent.jamendo.com?type=album&id=176100&width=200&trackid=1538710" }
                },
                "download": {
                    "mp3": "https://prod-1.storage.jamendo.com/download/track/1538710/mp32/",
                    "ogg": "https://prod-1.storage.jamendo.com/download/track/1538710/ogg/"
                },
                "stream": {
                    "mp3": "https://prod-1.storage.jamendo.com/?trackid=1538710&format=mp31",
                    "ogg": "https://prod-1.storage.jamendo.com/?trackid=1538710&format=ogg"
                }
            });
            let legacy = crate::jamendo::parse_track(&platform_item)
                .expect("真实 parse_track 必须能解析捕获形状");
            assert_eq!(legacy.id, "1538710");
            assert_eq!(legacy.source, SourceId::Jamendo);
            assert_eq!(legacy.name, "Love Recycle");
            assert_eq!(legacy.singer, "LoveRecycle");
            assert_eq!(legacy.album_name, "LoveRecycle");
            assert_eq!(legacy.album_id, "176100");
            assert_eq!(legacy.duration, Duration::from_secs(245));
            assert_eq!(
                legacy.qualities,
                [Quality::Low128].into_iter().collect(),
                "download 组 mp3 键 → Low128（两侧 pick_stream 档序一致）"
            );
            assert_eq!(
                legacy.extra.get("track_id").map(String::as_str),
                Some("1538710")
            );

            let musid = track_to_song(&jamendo_track_fixture()).expect("jamendo provider must map");
            assert!(
                crate::parity::songs_equivalent(&legacy, &musid),
                "归一对照命中：id 精确 + name/singer/album 归一相等 + 时长秒级一致 + qualities 折档集合相等；extra 差异（legacy 无 stream_*）不进对照面"
            );
            assert!(
                crate::parity::compare_search_items(
                    std::slice::from_ref(&legacy),
                    std::slice::from_ref(&musid)
                ),
                "搜索产物集对照命中（probe 模式 ParityMatch 的归一口径）"
            );

            // 失配样例：legacy 侧歌手被改 → 与 musid 产物不等价（probe 记
            // ParityMismatch）。
            let mut different = legacy;
            different.singer = "Someone Else".to_string();
            assert!(!crate::parity::songs_equivalent(&different, &musid));
        }

        /// 复刻 musid-dli JamendoProvider::resolve 的离线语义
        /// （source_jamendo.rs:402-444）：从 track.extra["stream_url"] 取址
        /// （无则 Provider 错误——运行期回落旧链的触发点）、忽略请求档位
        /// （曲库恒最优，已在搜索时嵌入 extra）。本桩只测 AdapterMusicSource
        /// 自身的映射/门控行为（不依赖子 feature）；与**真实**
        /// JamendoProvider 的一致性由 `source/tests/musid_jamendo_slice.rs`
        /// （挂 `musid-dli-adapter-jamendo` 门，跑真实 provider）钉住——桩
        /// 与真实实现漂移时那边必红。
        struct JamendoStreamStub;

        impl MusicProvider for JamendoStreamStub {
            fn id(&self) -> &str {
                "jamendo"
            }
            fn name(&self) -> &str {
                "Jamendo"
            }
            fn capabilities(&self) -> ProviderCapabilities {
                ProviderCapabilities {
                    track_search: true,
                    stream_resolve: true,
                    ..Default::default()
                }
            }
        }

        #[async_trait]
        impl MdStreamResolver for JamendoStreamStub {
            async fn resolve(
                &self,
                track: &Track,
                _options: &ResolveOptions,
            ) -> Result<StreamInfo, MdError> {
                let url =
                    track
                        .extra
                        .get("stream_url")
                        .cloned()
                        .ok_or_else(|| MdError::Provider {
                            provider: "jamendo".to_string(),
                            message: "track carries no embedded stream URL".to_string(),
                        })?;
                let achieved = match track.extra.get("stream_quality").map(String::as_str) {
                    Some("Lossless") => StreamQuality::Lossless,
                    Some("High") => StreamQuality::High,
                    _ => StreamQuality::Low,
                };
                Ok(StreamInfo {
                    url,
                    quality: achieved,
                    format: AudioFormat::Mp3,
                    duration: track.duration,
                    expires_at_unix: None,
                    content_length: None,
                    checksum: None,
                    headers: vec![],
                    fallback_urls: vec![],
                    index_range: None,
                    init_range: None,
                    max_chunk_size: None,
                    size_is_advisory: true,
                    checksum_algorithm: None,
                    fallback_endpoints: vec![],
                })
            }
        }

        fn jamendo_stream_adapter() -> AdapterMusicSource {
            AdapterMusicSource::new(
                SourceId::Jamendo,
                "Jamendo（musid-dli）".to_string(),
                None,
                Some(Arc::new(JamendoStreamStub) as Arc<dyn MdStreamResolver>),
                None,
                TransportInjection::new(None, 15),
            )
        }

        #[tokio::test]
        async fn jamendo_stream_face_resolves_offline_and_ignores_requested_quality() {
            // musid 搜到的歌（extra 带 stream_url）：离线 resolve；musid
            // 忽略请求档位（曲库恒最优），SongUrl.quality 以实际档位回填
            // （§2.4），cover_url/qualities 经 enrich 从歌曲模型补齐（§3
            // 第 13、14 条）。
            let adapter = jamendo_stream_adapter();
            let song = track_to_song(&jamendo_track_fixture()).expect("jamendo provider must map");
            let url = adapter
                .get_song_url(&song, Quality::Flac24)
                .await
                .expect("流地址面映射");
            assert_eq!(
                url.url,
                "https://prod-1.storage.jamendo.com/download/track/1538710/mp32/"
            );
            assert_eq!(
                url.quality,
                Quality::Low128,
                "实际档位回填（musid 忽略请求档位，恒返回嵌入的 Low）"
            );
            assert_eq!(
                url.cover_url.as_deref(),
                Some(
                    "https://usercontent.jamendo.com?type=album&id=176100&width=300&trackid=1538710"
                ),
                "enrich_song_url_from_song 从歌曲模型补齐封面"
            );
            assert_eq!(url.qualities, vec![Quality::Low128]);
            assert_eq!(url.duration, Duration::from_secs(245));
        }

        #[tokio::test]
        async fn jamendo_legacy_sourced_song_without_stream_url_fails_into_fallback() {
            // legacy 搜到的歌（extra 只有 track_id，无 stream_url）：musid
            // resolve 必然失败（Provider 类错误 → FetchError::Other）→
            // manager 按 V1 语义立即旧链重试并计 MusidErrLegacyOk——能力
            // 缺失回落规则的动态层（docs §12.2），用户无感知。
            let adapter = jamendo_stream_adapter();
            let mut song = SongInfo::new(
                "1538710".to_string(),
                SourceId::Jamendo,
                "Love Recycle".to_string(),
                "LoveRecycle".to_string(),
            );
            song.extra
                .insert("track_id".to_string(), "1538710".to_string());
            let error = adapter
                .get_song_url(&song, Quality::High320)
                .await
                .expect_err("musid 无嵌入地址必须失败");
            assert!(
                matches!(error, FetchError::Other(_)),
                "Provider 类错误映射 Other（非 NotFound），由 manager 回落旧链: {error}"
            );
            assert!(
                error.to_string().contains("stream URL"),
                "错误文本保留 musid 侧语义供诊断: {error}"
            );
        }

        // --- V2 切片 2（fivesing，docs §12.4）：三面全 Migrated ---

        fn fivesing_track_fixture() -> Track {
            from_str::<Track>(fixture!("track_fivesing.json"))
                .expect("fivesing track fixture must parse")
        }

        #[test]
        fn fivesing_track_fixture_maps_with_search_extra_passthrough() {
            let track = fivesing_track_fixture();
            let song = track_to_song(&track).expect("fivesing provider must map");
            assert_eq!(song.source, SourceId::Fivesing);
            assert_eq!(song.id, "15622111|fc", "id = songid|kind，两侧同形");
            assert_eq!(song.name, "晴天");
            assert_eq!(song.singer, "Vk");
            assert_eq!(song.album_name, "", "5sing 搜索条目无专辑字段");
            assert_eq!(song.duration, Duration::from_secs(261));
            assert_eq!(song.cover_url, None, "live 搜索项无 user 键（上游 README）");
            assert_eq!(
                song.qualities,
                [Quality::Low128, Quality::High320, Quality::Flac]
                    .into_iter()
                    .collect(),
                "fs_parse_search_song 的静态三档折档后进集合（§2.4）"
            );
            // songid/songtype 经 extra 原样透传：resolve/lyrics 面取址的
            // 数据前提（musid split_track_id 优先 extra，source_fivesing.rs:154-169）。
            assert_eq!(
                song.extra.get("songid").map(String::as_str),
                Some("15622111")
            );
            assert_eq!(song.extra.get("songtype").map(String::as_str), Some("fc"));
            // 反向回组 Track：id/extra 保真（resolve 的数据链闭合——musid
            // 搜到的歌再请求地址/歌词时，取址键原样回到 musid）。
            let rebuilt = song_to_track(&song).expect("fivesing source must map");
            assert_eq!(rebuilt.id, track.id);
            assert_eq!(rebuilt.extra, track.extra);
        }

        #[test]
        fn fivesing_search_parity_identity_fields_match_with_a_registered_qualities_gap() {
            // legacy 侧用**真实的** `crate::fivesing::api::parse_search_song`
            // （fivesing/api.rs，从 search_songs 内联闭包逐字搬出 + pub(crate)
            // 扩宽，登记见 §12.5）从平台原始响应形状组出 SongInfo——原始
            // item 与 musid-dli tests/fixtures/fivesing/search_songs.json 的
            // 第 1 项同形（2026-11-18 逐字 live 捕获，README）。musid 侧：
            // track_to_song（voicefox 侧 wire 形状 fixture）。任一侧 parse
            // 改字段，本测试必红。
            let platform_item = json!({
                "songId": 15622111,
                "typeEname": "fc",
                "songName": "<em class=\"keyword\">晴天</em>",
                "singer": "Vk",
                "songSize": 10462607,
                "type": 2,
                "typeName": "翻唱"
            });
            let legacy = crate::fivesing::api::parse_search_song(&platform_item)
                .expect("真实 parse_search_song 必须能解析捕获形状");
            assert_eq!(legacy.id, "15622111|fc");
            assert_eq!(legacy.source, SourceId::Fivesing);
            assert_eq!(legacy.name, "晴天");
            assert_eq!(legacy.singer, "Vk");
            assert_eq!(legacy.duration, Duration::from_secs(261));
            assert_eq!(legacy.cover_url, None);

            let musid =
                track_to_song(&fivesing_track_fixture()).expect("fivesing provider must map");
            // 归一对照的登记结论（docs §12.2）：身份字段（id/name/singer/
            // 时长）逐项对齐，唯一分歧字段是 qualities 簿记——legacy 产物
            // 不携带档位、musid 静态声明三档 → songs_equivalent 系统性
            // false → probe 搜索面 ParityMatch/Mismatch 对 fivesing 无鉴别
            // 力（与 §11.8 (b) 登录源噪声同类：簿记差异，不是歌曲身份差异）。
            assert!(legacy.qualities.is_empty());
            let mut legacy_with_musid_qualities = legacy.clone();
            legacy_with_musid_qualities.qualities = musid.qualities.clone();
            assert!(
                crate::parity::songs_equivalent(&legacy_with_musid_qualities, &musid),
                "补齐 qualities 后全部归一字段命中——qualities 是唯一分歧字段"
            );
            assert!(
                !crate::parity::songs_equivalent(&legacy, &musid),
                "登记口径：qualities 簿记分歧使 probe 搜索面对 fivesing 恒 ParityMismatch"
            );
        }

        /// 复刻 musid-dli `FivesingProvider::resolve` 的现取产物形状与档位
        /// 挑选语义（source_fivesing.rs:546-596）。真实 resolve 是网络现取
        /// （恒走 getSongUrl 端点），确定性门禁里由桩承载其产物：SQ/HQ/LQ
        /// 三档静态表（live get_song_url.json 的逐字 URL）+ 挑选规则（档位
        /// 降序取首个 `<= 请求档位`、请求超出曲库取最高档，:555-562）+
        /// headers 恒空（登记差异 (a)）+ 备用地址进 fallback_urls/
        /// fallback_endpoints（quality 回填所选档，:563-578）。真实 provider
        /// 的离线行为（无 kind 后缀 → split_track_id 失败进动态层）与
        /// descriptor 由 `source/tests/musid_fivesing_slice.rs`（挂
        /// `musid-dli-adapter-fivesing` 门，跑真实 provider）钉住；真网行为
        /// 由运行期 probe 对账覆盖（docs §12.3）。
        struct FivesingStreamStub;

        impl MusicProvider for FivesingStreamStub {
            fn id(&self) -> &str {
                "fivesing"
            }
            fn name(&self) -> &str {
                "5sing"
            }
            fn capabilities(&self) -> ProviderCapabilities {
                ProviderCapabilities {
                    track_search: true,
                    stream_resolve: true,
                    lyrics: true,
                    playlists: true,
                    link_resolve: true,
                    ..Default::default()
                }
            }
        }

        /// live `get_song_url.json`（2026-11-18 逐字捕获）的三档 URL 及其
        /// 各自的 cloud 备用地址（主站 + cloud 双 host，与捕获逐字一致）。
        fn fivesing_tier(quality: StreamQuality) -> (&'static str, &'static str) {
            match quality {
                StreamQuality::Lossless => (
                    "https://wsaudiobssdlbig.kugou.com/202611180449/7de097966984f81de648b88ca6827b47/bss/extname/wsaudio/e16fe9656014667c92d5c7a158251a0d.mp3",
                    "https://wsaudiobssdlbig.cloud.kugou.com/202611180449/8c6b6222a85405d54e8de4f1f15a1762/bss/extname/wsaudio/e16fe9656014667c92d5c7a158251a0d.mp3",
                ),
                StreamQuality::High => (
                    "https://wsaudiobssdlbig.kugou.com/202611180449/e9d6a589a75cb1c94df432294e53ac9e/bss/extname/wsaudio/045802510fa5527105812f85233b1d73.mp3",
                    "https://wsaudiobssdlbig.cloud.kugou.com/202611180449/4ad2152161e17c6618a543f7a3ae19e8/bss/extname/wsaudio/045802510fa5527105812f85233b1d73.mp3",
                ),
                _ => (
                    "https://wsaudiobssdlbig.kugou.com/202611180449/7deb93b917fd1eb6c3e4c223be3f4f7a/bss/extname/wsaudio/f9d989ccbb5ac496d548e2686e628aa4.mp3",
                    "https://wsaudiobssdlbig.cloud.kugou.com/202611180449/f0fe886812abbb326292f6c0c25cfae5/bss/extname/wsaudio/f9d989ccbb5ac496d548e2686e628aa4.mp3",
                ),
            }
        }

        #[async_trait]
        impl MdStreamResolver for FivesingStreamStub {
            async fn resolve(
                &self,
                track: &Track,
                options: &ResolveOptions,
            ) -> Result<StreamInfo, MdError> {
                // split_track_id 语义（source_fivesing.rs:154-169）：extra
                // 优先、否则 id；无 `|` 即 Provider 错误——真实 provider 的
                // 该失败路径由 musid_fivesing_slice.rs 钉住，桩不复刻分支。
                let raw = track
                    .extra
                    .get("songid")
                    .map(|id| {
                        format!(
                            "{id}|{}",
                            track.extra.get("songtype").cloned().unwrap_or_default()
                        )
                    })
                    .unwrap_or_else(|| track.id.id.clone());
                raw.split_once('|').ok_or_else(|| MdError::Provider {
                    provider: "fivesing".to_string(),
                    message: "track id is missing the song type".to_string(),
                })?;
                let mut tiers = [
                    StreamQuality::Lossless,
                    StreamQuality::High,
                    StreamQuality::Low,
                ];
                tiers.sort_by_key(|quality| std::cmp::Reverse(*quality));
                let picked = *tiers
                    .iter()
                    .find(|quality| **quality <= options.quality)
                    .unwrap_or(&tiers[0]);
                let (url, backup) = fivesing_tier(picked);
                Ok(StreamInfo {
                    url: url.to_string(),
                    quality: picked,
                    format: AudioFormat::Mp3,
                    duration: track.duration,
                    expires_at_unix: None,
                    content_length: None,
                    checksum: None,
                    headers: vec![],
                    fallback_urls: vec![backup.to_string()],
                    index_range: None,
                    init_range: None,
                    max_chunk_size: None,
                    size_is_advisory: true,
                    checksum_algorithm: None,
                    fallback_endpoints: vec![FallbackEndpoint {
                        url: backup.to_string(),
                        headers: vec![],
                        expires_at_unix: None,
                        content_length: None,
                        quality: Some(picked),
                    }],
                })
            }
        }

        fn fivesing_stream_adapter() -> AdapterMusicSource {
            AdapterMusicSource::new(
                SourceId::Fivesing,
                "5sing（musid-dli）".to_string(),
                None,
                Some(Arc::new(FivesingStreamStub) as Arc<dyn MdStreamResolver>),
                None,
                TransportInjection::new(None, 15),
            )
        }

        #[tokio::test]
        async fn fivesing_stream_face_maps_tier_pick_backfill_and_the_registered_header_gap() {
            // 现取产物映射：实际档位回填（§2.4）、备用地址进 candidate_urls、
            // cover_url/qualities 经 enrich 从歌曲模型补齐（§3 第 13、14
            // 条）、登记差异 (a)：headers 恒空（legacy 挂 MEDIA_REFERER；
            // 上游 live probe 证实 CDN 已不强制 Referer，musid 对齐 live
            // 行为——docs §12.2，不改写）。
            let adapter = fivesing_stream_adapter();
            let mut song =
                track_to_song(&fivesing_track_fixture()).expect("fivesing provider must map");
            // newget 详情路径的封面（live song_info.json user.I），enrich 的
            // 数据前提。
            song.cover_url = Some(
                "https://wsingbssdl.kugou.com/d78267d58dd6192eb32961d9c6b1f77a.jpg".to_string(),
            );

            let url = adapter
                .get_song_url(&song, Quality::Flac24)
                .await
                .expect("流地址面映射");
            assert_eq!(
                url.url,
                "https://wsaudiobssdlbig.kugou.com/202611180449/7de097966984f81de648b88ca6827b47/bss/extname/wsaudio/e16fe9656014667c92d5c7a158251a0d.mp3",
                "请求 Flac24（HiRes）超出曲库三档 → 挑 SQ（or-first 分支）"
            );
            assert_eq!(url.quality, Quality::Flac, "实际档位回填（SQ）");
            assert_eq!(
                url.candidate_urls,
                vec!["https://wsaudiobssdlbig.cloud.kugou.com/202611180449/8c6b6222a85405d54e8de4f1f15a1762/bss/extname/wsaudio/e16fe9656014667c92d5c7a158251a0d.mp3".to_string()],
                "squrl_backup 进候选（ordered_fallback_urls）"
            );
            assert!(
                url.headers.is_empty(),
                "登记差异 (a)：musid 侧 headers 恒空（legacy 挂 Referer；CDN 已不强制，docs §12.2）"
            );
            assert_eq!(
                url.cover_url.as_deref(),
                Some("https://wsingbssdl.kugou.com/d78267d58dd6192eb32961d9c6b1f77a.jpg"),
                "enrich_song_url_from_song 从歌曲模型补齐封面"
            );
            assert_eq!(
                url.qualities,
                vec![Quality::Low128, Quality::High320, Quality::Flac],
                "enrich 从歌曲模型 qualities（折档后的静态三档）收集"
            );
            assert_eq!(
                url.duration,
                Duration::from_secs(261),
                "track.duration 直通"
            );

            // 显式传档位（§2.4，禁 Default=High）的可观测证据：请求 Low128
            // 时挑选落到 LQ 档——若适配层用了缺省 High，挑选会落 HQ。
            let low = adapter
                .get_song_url(&song, Quality::Low128)
                .await
                .expect("低档请求");
            assert_eq!(low.quality, Quality::Low128, "LQ 档命中 → 实际档位回填");
            assert_eq!(
                low.url,
                "https://wsaudiobssdlbig.kugou.com/202611180449/7deb93b917fd1eb6c3e4c223be3f4f7a/bss/extname/wsaudio/f9d989ccbb5ac496d548e2686e628aa4.mp3"
            );
            assert_eq!(
                low.candidate_urls,
                vec!["https://wsaudiobssdlbig.cloud.kugou.com/202611180449/f0fe886812abbb326292f6c0c25cfae5/bss/extname/wsaudio/f9d989ccbb5ac496d548e2686e628aa4.mp3".to_string()]
            );
        }

        // --- V2 切片 3（apple，docs §12.4）：三面全 Migrated ---

        fn apple_track_fixture() -> Track {
            from_str::<Track>(fixture!("track_apple.json")).expect("apple track fixture must parse")
        }

        #[test]
        fn apple_track_fixture_maps_with_preview_extra_passthrough() {
            let track = apple_track_fixture();
            let song = track_to_song(&track).expect("apple provider must map");
            assert_eq!(song.source, SourceId::Apple);
            assert_eq!(song.id, "1440833098");
            assert_eq!(song.name, "晴天");
            assert_eq!(song.singer, "周杰伦");
            assert_eq!(song.album_name, "叶惠美");
            assert_eq!(song.duration, Duration::from_secs(269));
            assert_eq!(
                song.cover_url.as_deref(),
                Some("https://is1-ssl.mzstatic.com/image/thumb/Music125/v4/600x600bb.jpg"),
                "artwork {{w}}x{{h}} 模板已按 600 解析（apple_parse_song）"
            );
            assert_eq!(
                song.qualities,
                [Quality::Low128].into_iter().collect(),
                "preview-only 契约：available_qualities [Low] 折档后进集合（§2.4）"
            );
            // song_id/preview_only 经 extra 原样透传：resolve/lyrics 面取址
            // 的数据前提（musid apple resolve/lyrics 优先 extra["song_id"]
            // 回落 track.id，source_apple.rs:671-676、:706-710）。
            assert_eq!(
                song.extra.get("song_id").map(String::as_str),
                Some("1440833098")
            );
            assert_eq!(
                song.extra.get("preview_only").map(String::as_str),
                Some("true")
            );
            // 反向回组 Track：id/extra 保真（resolve 的数据链闭合）。
            let rebuilt = song_to_track(&song).expect("apple source must map");
            assert_eq!(rebuilt.id, track.id);
            assert_eq!(rebuilt.extra, track.extra);
        }

        #[test]
        fn apple_search_parity_preview_only_with_a_registered_qualities_gap() {
            // legacy 侧用**真实的** `crate::apple::api::parse_song`
            // （apple/api.rs，pub(crate) 扩宽登记见 §12.5）从平台原始响应
            // 形状组出 SongInfo——原始 item 与 musid-dli
            // tests/fixtures/apple/song_parse.json 的 search_item 同形（该
            // 目录 README 明文**全部为重建形状**，非 live 捕获；金值取自
            // 双侧共用的 inline parse 测试）。musid 侧：track_to_song
            // （voicefox 侧 wire 形状 fixture）。任一侧 parse 改字段必红。
            let platform_item = json!({
                "id": "1440833098",
                "attributes": {
                    "name": "晴天",
                    "artistName": "周杰伦",
                    "albumName": "叶惠美",
                    "durationInMillis": 269000,
                    "artwork": {
                        "url": "https://is1-ssl.mzstatic.com/image/thumb/Music125/v4/{w}x{h}bb.jpg"
                    }
                }
            });
            let legacy = crate::apple::api::parse_song(&platform_item)
                .expect("真实 parse_song 必须能解析捕获形状");
            assert_eq!(legacy.id, "1440833098");
            assert_eq!(legacy.source, SourceId::Apple);
            assert_eq!(legacy.name, "晴天");
            assert_eq!(legacy.singer, "周杰伦");
            assert_eq!(legacy.album_name, "叶惠美");
            assert_eq!(legacy.duration, Duration::from_secs(269));
            assert_eq!(
                legacy.extra.get("song_id").map(String::as_str),
                Some("1440833098")
            );

            let musid = track_to_song(&apple_track_fixture()).expect("apple provider must map");
            // 登记口径（docs §12.2 apple 块 (b)）：身份字段（id/name/singer/
            // 专辑/时长）逐项对齐，唯一分歧字段是 qualities 簿记——legacy
            // 产物不携带档位、musid 声明 preview-only 单档 [Low]→{Low128}
            // → songs_equivalent 系统性 false → probe 搜索面对 apple 无
            // 鉴别力（同 fivesing：簿记差异，不是歌曲身份差异）。
            assert!(legacy.qualities.is_empty());
            let mut legacy_with_musid_qualities = legacy.clone();
            legacy_with_musid_qualities.qualities = musid.qualities.clone();
            assert!(
                crate::parity::songs_equivalent(&legacy_with_musid_qualities, &musid),
                "补齐 qualities 后全部归一字段命中——qualities 是唯一分歧字段"
            );
            assert!(
                !crate::parity::songs_equivalent(&legacy, &musid),
                "登记口径：qualities 簿记分歧使 probe 搜索面对 apple 恒 ParityMismatch"
            );
        }

        /// 复刻 musid-dli `AppleMusicProvider::resolve` 的 preview-only 产物
        /// 形状（source_apple.rs:663-695 有注释 pin 该契约）：忽略请求档位
        /// 恒回 30 秒 M4a 试听、quality Low、duration 30s、headers 空、无
        /// 备用地址。本桩只测适配层自身的映射/门控行为；真实 provider 的
        /// descriptor 与离线行为由 `source/tests/musid_apple_slice.rs` 钉住。
        struct AppleStreamStub;

        impl MusicProvider for AppleStreamStub {
            fn id(&self) -> &str {
                "applemusic"
            }
            fn name(&self) -> &str {
                "Apple Music"
            }
            fn capabilities(&self) -> ProviderCapabilities {
                ProviderCapabilities {
                    track_search: true,
                    stream_resolve: true,
                    lyrics: true,
                    ..Default::default()
                }
            }
        }

        #[async_trait]
        impl MdStreamResolver for AppleStreamStub {
            async fn resolve(
                &self,
                _track: &Track,
                _options: &ResolveOptions,
            ) -> Result<StreamInfo, MdError> {
                Ok(StreamInfo {
                    url:
                        "https://audio-ssl.itunes.apple.com/itunes-assets/AudioPreview125/v4/preview.m4a"
                            .to_string(),
                    quality: StreamQuality::Low,
                    format: AudioFormat::M4a,
                    duration: Some(Duration::from_secs(30)),
                    expires_at_unix: None,
                    content_length: None,
                    checksum: None,
                    headers: vec![],
                    fallback_urls: vec![],
                    index_range: None,
                    init_range: None,
                    max_chunk_size: None,
                    size_is_advisory: true,
                    checksum_algorithm: None,
                    fallback_endpoints: vec![],
                })
            }
        }

        #[tokio::test]
        async fn apple_stream_face_pins_the_preview_only_quality_contract() {
            // preview-only 契约（docs §12.2 apple 块）：musid 忽略请求档位
            // 恒回 30s 试听 → SongUrl.quality 以实际档位 Low 回填 Low128，
            // 与 legacy apple/mod.rs:86-98 的恒 Low128 严格对齐（quality
            // parity 可严格断言双侧均 Low/Low128）；登记差异 (a)：musid
            // SongUrl.duration 是 preview 物理时长 30s，legacy 带整曲时长。
            let adapter = AdapterMusicSource::new(
                SourceId::Apple,
                "Apple Music（musid-dli）".to_string(),
                None,
                Some(Arc::new(AppleStreamStub) as Arc<dyn MdStreamResolver>),
                None,
                TransportInjection::new(None, 15),
            );
            let song = track_to_song(&apple_track_fixture()).expect("apple provider must map");

            let url = adapter
                .get_song_url(&song, Quality::Flac24)
                .await
                .expect("流地址面映射");
            assert_eq!(url.quality, Quality::Low128, "实际档位回填（恒 Low）");
            assert_eq!(
                url.url,
                "https://audio-ssl.itunes.apple.com/itunes-assets/AudioPreview125/v4/preview.m4a"
            );
            assert_eq!(
                url.duration,
                Duration::from_secs(30),
                "登记差异 (a)：musid 带 preview 物理时长（legacy 带整曲时长）"
            );
            assert!(url.headers.is_empty(), "musid 侧 headers 恒空");
            assert_eq!(
                url.qualities,
                vec![Quality::Low128],
                "enrich 从歌曲模型收集"
            );
            assert_eq!(
                url.cover_url.as_deref(),
                Some("https://is1-ssl.mzstatic.com/image/thumb/Music125/v4/600x600bb.jpg"),
                "enrich_song_url_from_song 从歌曲模型补齐封面"
            );

            // 低档请求同形：请求档位不影响产物（preview-only 双侧同病）。
            let low = adapter
                .get_song_url(&song, Quality::Low128)
                .await
                .expect("低档请求");
            assert_eq!(low.quality, Quality::Low128);
            assert_eq!(low.duration, Duration::from_secs(30));
        }

        // --- V2 切片 4（bilibili，docs §12.4）：搜索/流地址 Migrated ---

        fn bilibili_track_fixture() -> Track {
            from_str::<Track>(fixture!("track_bilibili.json"))
                .expect("bilibili track fixture must parse")
        }

        #[test]
        fn bilibili_track_fixture_maps_with_video_reference_extra_passthrough() {
            let track = bilibili_track_fixture();
            let song = track_to_song(&track).expect("bilibili provider must map");
            assert_eq!(song.source, SourceId::Bili);
            assert_eq!(song.id, "BV1xx411c7mD");
            assert_eq!(song.name, "测试视频", "<em> 高亮两侧同 strip_html");
            assert_eq!(song.singer, "UP主");
            assert_eq!(
                song.album_name, "测试视频",
                "bilibili 搜索产物以视频标题为专辑（两侧同形）"
            );
            assert_eq!(song.album_id, "BV1xx411c7mD");
            assert_eq!(song.duration, Duration::from_secs(180));
            assert_eq!(
                song.cover_url.as_deref(),
                Some("https://example.com/cover.jpg"),
                "// 前缀补 https:（normalize_cover 两侧同形）"
            );
            assert!(song.qualities.is_empty(), "bilibili 不声明逐条档位");
            // bvid/cid/aid 经 extra 原样透传：resolve 取址的数据前提
            // （musid bili resolve 优先 extra["bvid"]/["cid"]，回落 track.id
            // 后 resolve_cid，source_bilibili.rs:1018-1031）。
            assert_eq!(
                song.extra.get("bvid").map(String::as_str),
                Some("BV1xx411c7mD")
            );
            assert_eq!(song.extra.get("cid").map(String::as_str), Some("1001"));
            assert_eq!(song.extra.get("aid").map(String::as_str), Some("170001"));
            // 反向回组 Track：id/extra 保真。
            let rebuilt = song_to_track(&song).expect("bilibili source must map");
            assert_eq!(rebuilt.id, track.id);
            assert_eq!(rebuilt.extra, track.extra);
        }

        #[test]
        fn bilibili_search_parity_normalized_equivalent() {
            // legacy 侧用**真实的** `crate::bili::search::parse_song`
            // （bili/search.rs，pub(crate) 扩宽登记见 §12.5）从平台原始响应
            // 形状组出 SongInfo——原始 item 与 musid-dli
            // tests/fixtures/bilibili/search_title_em.json 的 result[0] 同形
            // （重建形状，README；金值两侧同源）。musid 侧：track_to_song
            // （voicefox 侧 wire 形状 fixture）。任一侧 parse 改字段必红。
            let platform_item = json!({
                "bvid": "BV1xx411c7mD",
                "title": "测试<em>视频</em>",
                "author": "UP主",
                "mid": 42,
                "cid": 1001,
                "aid": 170001,
                "duration": "03:00",
                "pic": "//example.com/cover.jpg"
            });
            let legacy = crate::bili::search::parse_song(&platform_item)
                .expect("真实 parse_song 必须能解析捕获形状");
            assert_eq!(legacy.id, "BV1xx411c7mD");
            assert_eq!(legacy.source, SourceId::Bili);
            assert_eq!(legacy.name, "测试视频");
            assert_eq!(legacy.singer, "UP主");
            assert_eq!(legacy.album_name, "测试视频");
            assert_eq!(legacy.duration, Duration::from_secs(180));
            assert_eq!(
                legacy.cover_url.as_deref(),
                Some("https://example.com/cover.jpg")
            );

            let musid =
                track_to_song(&bilibili_track_fixture()).expect("bilibili provider must map");
            assert!(
                crate::parity::songs_equivalent(&legacy, &musid),
                "归一对照命中：bilibili 搜索面无 fivesing/apple 式簿记分歧（双侧均无档位声明）"
            );
            assert!(
                crate::parity::compare_search_items(
                    std::slice::from_ref(&legacy),
                    std::slice::from_ref(&musid)
                ),
                "搜索产物集对照命中"
            );

            let mut different = legacy;
            different.singer = "另一位UP主".to_string();
            assert!(!crate::parity::songs_equivalent(&different, &musid));
        }

        /// musid-dli BilibiliProvider::resolve 的 DASH 产物形状
        /// （source_bilibili.rs:1016-1091 + choose_audio :852-893）：DASH
        /// 音频轨 baseUrl 为主、backupUrl 镜像进 fallback_endpoints（同
        /// Referer/UA 要求）、quality 由带宽挑选（>160k → High）、format
        /// 恒 Aac。本桩只测适配层自身的映射/门控行为；真实 provider 的
        /// descriptor 与离线行为由 `source/tests/musid_bilibili_slice.rs`
        /// 钉住。
        fn bilibili_dash_stream_info() -> StreamInfo {
            StreamInfo {
                url: "https://upos-sz-mirror08c.bilivideo.com/dash-audio.m4s".to_string(),
                quality: StreamQuality::High,
                format: AudioFormat::Aac,
                duration: Some(Duration::from_secs(180)),
                expires_at_unix: None,
                content_length: None,
                checksum: None,
                // DASH CDN 拒绝无 Referer 请求：两侧恒挂同一组头。
                headers: vec![
                    (
                        "Referer".to_string(),
                        "https://www.bilibili.com/".to_string(),
                    ),
                    (
                        "User-Agent".to_string(),
                        "Mozilla/5.0 (X11; Linux x86_64; rv:141.0) Gecko/20100101 Firefox/141.0"
                            .to_string(),
                    ),
                ],
                fallback_urls: vec![],
                index_range: None,
                init_range: None,
                max_chunk_size: None,
                size_is_advisory: false,
                checksum_algorithm: None,
                fallback_endpoints: vec![FallbackEndpoint {
                    url: "https://upos-sz-mirrorali.bilivideo.com/dash-audio.m4s".to_string(),
                    headers: vec![(
                        "Referer".to_string(),
                        "https://www.bilibili.com/".to_string(),
                    )],
                    expires_at_unix: None,
                    content_length: None,
                    quality: Some(StreamQuality::High),
                }],
            }
        }

        struct BilibiliStreamStub;

        impl MusicProvider for BilibiliStreamStub {
            fn id(&self) -> &str {
                "bilibili"
            }
            fn name(&self) -> &str {
                "哔哩哔哩"
            }
            fn capabilities(&self) -> ProviderCapabilities {
                ProviderCapabilities {
                    track_search: true,
                    stream_resolve: true,
                    authentication: true,
                    playlists: true,
                    charts: true,
                    ..Default::default()
                }
            }
        }

        #[async_trait]
        impl MdStreamResolver for BilibiliStreamStub {
            async fn resolve(
                &self,
                _track: &Track,
                _options: &ResolveOptions,
            ) -> Result<StreamInfo, MdError> {
                Ok(bilibili_dash_stream_info())
            }
        }

        #[tokio::test]
        async fn bilibili_stream_face_maps_dash_headers_candidates_and_g3_slots() {
            // DASH 伴随字段断言（docs §12.4 行 4）：headers 两侧同形原样
            // 透传（legacy bili/url.rs:61-64 恒挂 Referer+UA）；backupUrl
            // 镜像折叠进 candidate_urls（FallbackEndpoint 元数据旁路留给
            // 下载引擎切片，§2.6）；G3：index/init range 走伴随字段（musid
            // 不产 → None，descriptive-only）；format=Aac 不反推档位
            // （quality 来自带宽挑选的回填，与 format 无关）。
            let resolved = stream_to_resolved_url(&bilibili_dash_stream_info());
            assert_eq!(resolved.format, AudioFormat::Aac);
            assert_eq!(
                resolved.index_range, None,
                "G3：musid 不产 DASH range → 伴随字段 None（descriptive-only）"
            );
            assert_eq!(resolved.init_range, None);

            let adapter = AdapterMusicSource::new(
                SourceId::Bili,
                "哔哩哔哩（musid-dli）".to_string(),
                None,
                Some(Arc::new(BilibiliStreamStub) as Arc<dyn MdStreamResolver>),
                None,
                TransportInjection::new(None, 15),
            );
            let mut song =
                track_to_song(&bilibili_track_fixture()).expect("bilibili provider must map");
            song.cover_url = Some("https://example.com/cover.jpg".to_string());

            let url = adapter
                .get_song_url(&song, Quality::Low128)
                .await
                .expect("流地址面映射");
            assert_eq!(
                url.url,
                "https://upos-sz-mirror08c.bilivideo.com/dash-audio.m4s"
            );
            assert_eq!(
                url.quality,
                Quality::High320,
                "实际档位回填（带宽挑选 High；format=Aac 不反推档位）"
            );
            assert_eq!(
                url.headers,
                vec![
                    (
                        "Referer".to_string(),
                        "https://www.bilibili.com/".to_string()
                    ),
                    (
                        "User-Agent".to_string(),
                        "Mozilla/5.0 (X11; Linux x86_64; rv:141.0) Gecko/20100101 Firefox/141.0"
                            .to_string(),
                    ),
                ],
                "DASH CDN 头两侧同形（legacy 恒挂同值，bili/url.rs:61-64）"
            );
            assert_eq!(
                url.candidate_urls,
                vec!["https://upos-sz-mirrorali.bilivideo.com/dash-audio.m4s".to_string()],
                "backupUrl 镜像进候选（ordered_fallback_urls）"
            );
            assert_eq!(
                url.duration,
                Duration::from_secs(180),
                "track.duration 直通"
            );
            assert_eq!(
                url.cover_url.as_deref(),
                Some("https://example.com/cover.jpg"),
                "enrich 从歌曲模型补齐封面"
            );
        }

        // --- V2 切片 5（migu，docs §12.4）：三面全 Migrated ---

        fn migu_track_fixture() -> Track {
            from_str::<Track>(fixture!("track_migu.json")).expect("migu track fixture must parse")
        }

        #[test]
        fn migu_track_fixture_maps_with_mrc_extra_passthrough() {
            let track = migu_track_fixture();
            let song = track_to_song(&track).expect("migu provider must map");
            assert_eq!(song.source, SourceId::Mg);
            assert_eq!(song.id, "3790007");
            assert_eq!(song.name, "晴天");
            assert_eq!(song.singer, "周杰伦");
            assert_eq!(song.album_name, "叶惠美");
            assert_eq!(song.duration, Duration::from_secs(269));
            assert_eq!(song.cover_url.as_deref(), Some("https://img.example/a.jpg"));
            assert_eq!(
                song.qualities,
                [Quality::Low128, Quality::Flac].into_iter().collect(),
                "audioFormats PQ+SQ 折档后进集合（两侧 formatType 同表）"
            );
            // copyrightId/contentId 经 extra 原样透传：流地址/歌词面取址的
            // 数据前提（musid resource_id 优先 extra["contentId"]，
            // source_migu.rs:596-603；歌词面 mrcUrl/lrcUrl/trcUrl 同链）。
            assert_eq!(
                song.extra.get("copyrightId").map(String::as_str),
                Some("60054701923")
            );
            assert_eq!(
                song.extra.get("contentId").map(String::as_str),
                Some("600902000006889366")
            );
            // 反向回组 Track：id/extra 保真。
            let rebuilt = song_to_track(&song).expect("migu source must map");
            assert_eq!(rebuilt.id, track.id);
            assert_eq!(rebuilt.extra, track.extra);
        }

        #[test]
        fn migu_search_parity_normalized_equivalent() {
            // legacy 侧用**真实的** `crate::mg::song::parse_song`
            // （mg/song.rs，本就 pub(crate)）从平台原始响应形状组出
            // SongInfo——原始 item 与 musid-dli
            // tests/fixtures/migu/search_response.json 的
            // ok_numeric_total.resultList[0][0] 同形。musid 侧：
            // track_to_song（voicefox 侧 wire 形状 fixture）。
            let platform_item = json!({
                "songId": 3790007,
                "songName": "晴天",
                "singerList": [{ "name": "周杰伦" }],
                "album": "叶惠美",
                "copyrightId": "60054701923",
                "contentId": "600902000006889366",
                "duration": 269,
                "img3": "https://img.example/a.jpg",
                "audioFormats": [{ "formatType": "PQ" }, { "formatType": "SQ" }]
            });
            let legacy = crate::mg::song::parse_song(&platform_item)
                .expect("真实 parse_song 必须能解析捕获形状");
            assert_eq!(legacy.id, "3790007");
            assert_eq!(legacy.source, SourceId::Mg);
            assert_eq!(legacy.name, "晴天");
            assert_eq!(legacy.singer, "周杰伦");
            assert_eq!(legacy.album_name, "叶惠美");
            assert_eq!(legacy.duration, Duration::from_secs(269));
            assert_eq!(
                legacy.cover_url.as_deref(),
                Some("https://img.example/a.jpg")
            );
            assert_eq!(
                legacy.qualities,
                [Quality::Low128, Quality::Flac].into_iter().collect(),
                "legacy formatType 表 PQ/HQ/SQ/ZQ* 与 musid 同形折档"
            );

            let musid = track_to_song(&migu_track_fixture()).expect("migu provider must map");
            // migu 搜索面 qualities 完全对齐（两侧 formatType 同表）——
            // 无 fivesing/apple 式簿记分歧，归一对照可全等命中。
            assert!(
                crate::parity::songs_equivalent(&legacy, &musid),
                "归一对照命中（qualities 集合含内）"
            );
            assert!(
                crate::parity::compare_search_items(
                    std::slice::from_ref(&legacy),
                    std::slice::from_ref(&musid)
                ),
                "搜索产物集对照命中"
            );

            let mut different = legacy;
            different.name = "七里香".to_string();
            assert!(!crate::parity::songs_equivalent(&different, &musid));
        }

        // --- V2 切片 6（kuwo，docs §12.4）：三面全 Migrated ---

        fn kuwo_track_fixture() -> Track {
            from_str::<Track>(fixture!("track_kuwo.json")).expect("kuwo track fixture must parse")
        }

        #[test]
        fn kuwo_track_fixture_maps_with_nminfo_qualities() {
            let track = kuwo_track_fixture();
            let song = track_to_song(&track).expect("kuwo provider must map");
            assert_eq!(song.source, SourceId::Kw);
            assert_eq!(song.id, "228908");
            assert_eq!(song.name, "晴天");
            assert_eq!(
                song.singer, "周杰伦、费玉清",
                "musid 按 & 拆歌手、track_to_song 按 、 重join——与 legacy 的 & → 、 等价"
            );
            assert_eq!(song.album_name, "叶惠美");
            assert_eq!(song.album_id, "104004");
            assert_eq!(song.duration, Duration::from_secs(270));
            assert_eq!(
                song.qualities,
                [Quality::Low128, Quality::High320, Quality::Flac]
                    .into_iter()
                    .collect(),
                "N_MINFO bitrate 320/128/2000 → High/Low/Lossless 折档（22000/20900 不在两侧表中）"
            );
            // 反向回组 Track：id 保真。
            let rebuilt = song_to_track(&song).expect("kuwo source must map");
            assert_eq!(rebuilt.id, track.id);
        }

        #[test]
        fn kuwo_search_parity_normalized_equivalent() {
            // legacy 侧用**真实的** `crate::kw::search::parse_search_song`
            // （kw/search.rs，从 search 的内联逐项解析逐字搬出 + pub(crate)
            // 扩宽，登记见 §12.5）从平台原始响应形状组出 SongInfo——原始
            // item 与 musid-dli tests/fixtures/kuwo/search_page.json 的
            // abslist[0] 同形。musid 侧：track_to_song（voicefox 侧 wire
            // 形状 fixture）。任一侧 parse 改字段必红。
            let platform_item = json!({
                "MUSICRID": "MUSIC_228908",
                "SONGNAME": "晴天",
                "ARTIST": "周杰伦&费玉清",
                "ALBUM": "叶惠美",
                "ALBUMID": "104004",
                "DURATION": "270",
                "N_MINFO": "level:bcms,bitrate:22000,format:mgg,size:22.11Mb;level:zply,bitrate:20900,format:mflac,size:178.32Mb;level:high,bitrate:320,format:mp3,size:10.2M;level:standard,bitrate:128,format:mp3,size:4.1M;level:super,bitrate:2000,format:flac,size:30M"
            });
            let legacy = crate::kw::search::parse_search_song(&platform_item)
                .expect("真实 parse_search_song 必须能解析捕获形状");
            assert_eq!(legacy.id, "228908");
            assert_eq!(legacy.source, SourceId::Kw);
            assert_eq!(legacy.name, "晴天");
            assert_eq!(legacy.singer, "周杰伦、费玉清");
            assert_eq!(legacy.album_name, "叶惠美");
            assert_eq!(legacy.album_id, "104004");
            assert_eq!(legacy.duration, Duration::from_secs(270));
            assert_eq!(
                legacy.qualities,
                [Quality::Low128, Quality::High320, Quality::Flac]
                    .into_iter()
                    .collect()
            );

            let musid = track_to_song(&kuwo_track_fixture()).expect("kuwo provider must map");
            // kuwo 搜索面 qualities 完全对齐（两侧 bitrate 表同形）——
            // 归一对照可全等命中。
            assert!(
                crate::parity::songs_equivalent(&legacy, &musid),
                "归一对照命中（qualities 集合含内）"
            );
            assert!(
                crate::parity::compare_search_items(
                    std::slice::from_ref(&legacy),
                    std::slice::from_ref(&musid)
                ),
                "搜索产物集对照命中"
            );

            let mut different = legacy;
            different.album_name = "七里香".to_string();
            assert!(!crate::parity::songs_equivalent(&different, &musid));
        }

        // --- V2 切片 7（kugou，docs §12.4）：三面全 Migrated ---

        fn kugou_track_fixture() -> Track {
            from_str::<Track>(fixture!("track_kugou.json")).expect("kugou track fixture must parse")
        }

        #[test]
        fn kugou_track_fixture_maps_with_hash_extra_passthrough() {
            let track = kugou_track_fixture();
            let song = track_to_song(&track).expect("kugou provider must map");
            assert_eq!(song.source, SourceId::Kg);
            assert_eq!(song.id, "123456");
            assert_eq!(song.name, "晴天");
            assert_eq!(song.singer, "周杰伦");
            assert_eq!(song.album_name, "叶惠美");
            assert_eq!(song.album_id, "42");
            assert_eq!(song.duration, Duration::from_secs(269));
            assert_eq!(
                song.qualities,
                [Quality::Low128, Quality::High320, Quality::Flac24]
                    .into_iter()
                    .collect(),
                "FileSize/HQFileSize/ResFileSize > 0（SQ=0 缺席）折档后进集合"
            );
            // FileHash 系键经 extra 原样透传：流地址面取址的数据前提
            // （musid select_hash 按 tier 从 extra 取 hash，回落链
            // SQFileHash → HQFileHash → FileHash，source_kugou.rs:519-536）。
            assert_eq!(
                song.extra.get("FileHash").map(String::as_str),
                Some("ABCDEF0123")
            );
            assert_eq!(
                song.extra.get("HQFileHash").map(String::as_str),
                Some("HQ0123")
            );
            // 反向回组 Track：id/extra 保真（resolve 的数据链闭合）。
            let rebuilt = song_to_track(&song).expect("kugou source must map");
            assert_eq!(rebuilt.id, track.id);
            assert_eq!(rebuilt.extra, track.extra);
        }

        #[test]
        fn kugou_search_parity_normalized_equivalent() {
            // legacy 侧用**真实的** `crate::kg::search::parse_song`
            // （kg/search.rs，pub(crate) 扩宽登记见 §12.5）从平台原始响应
            // 形状组出 SongInfo。musid 侧：track_to_song（voicefox 侧 wire
            // 形状 fixture；样本值取自 musid-dli source_kugou.rs 的 inline
            // parse 测试——上游 tests/fixtures/kugou/ 无 search 形状文件，
            // 如实登记为重建形状）。任一侧 parse 改字段必红。
            let platform_item = json!({
                "Audioid": 123456,
                "SongName": "晴天",
                "Singers": [{ "name": "周杰伦" }],
                "AlbumName": "叶惠美",
                "AlbumID": 42,
                "Duration": 269,
                "FileSize": 4312832,
                "HQFileSize": 10781692,
                "SQFileSize": 0,
                "ResFileSize": 12345678,
                "FileHash": "ABCDEF0123",
                "HQFileHash": "HQ0123"
            });
            let legacy = crate::kg::search::parse_song(&platform_item)
                .expect("真实 parse_song 必须能解析捕获形状");
            assert_eq!(legacy.id, "123456");
            assert_eq!(legacy.source, SourceId::Kg);
            assert_eq!(legacy.name, "晴天");
            assert_eq!(legacy.singer, "周杰伦");
            assert_eq!(legacy.album_name, "叶惠美");
            assert_eq!(legacy.duration, Duration::from_secs(269));
            assert_eq!(
                legacy.qualities,
                [Quality::Low128, Quality::High320, Quality::Flac24]
                    .into_iter()
                    .collect()
            );
            assert_eq!(
                legacy.extra.get("FileHash").map(String::as_str),
                Some("ABCDEF0123")
            );

            let musid = track_to_song(&kugou_track_fixture()).expect("kugou provider must map");
            // kugou 搜索面 qualities 完全对齐（两侧 size>0 → 四档同表）。
            assert!(
                crate::parity::songs_equivalent(&legacy, &musid),
                "归一对照命中（qualities 集合含内）"
            );
            assert!(
                crate::parity::compare_search_items(
                    std::slice::from_ref(&legacy),
                    std::slice::from_ref(&musid)
                ),
                "搜索产物集对照命中"
            );

            let mut different = legacy;
            different.duration = Duration::from_secs(300);
            assert!(!crate::parity::songs_equivalent(&different, &musid));
        }

        // --- V2 切片 8（qqmusic/tx，docs §12.4）：三面全 Migrated ---

        fn tx_track_fixture() -> Track {
            from_str::<Track>(fixture!("track_qqmusic.json")).expect("tx track fixture must parse")
        }

        #[test]
        fn tx_track_fixture_maps_with_vkey_extra_passthrough() {
            let track = tx_track_fixture();
            let song = track_to_song(&track).expect("qqmusic provider must map");
            assert_eq!(song.source, SourceId::Tx);
            assert_eq!(song.id, "003aAYrm3GE0Ac");
            assert_eq!(song.name, "晴天");
            assert_eq!(song.singer, "周杰伦、杨瑞代");
            assert_eq!(song.album_name, "叶惠美");
            assert_eq!(song.album_id, "002Neh8l0uciQ1");
            assert_eq!(song.duration, Duration::from_secs(269));
            assert_eq!(
                song.qualities,
                [
                    Quality::Low128,
                    Quality::High320,
                    Quality::Flac,
                    Quality::Flac24
                ]
                .into_iter()
                .collect(),
                "size_128mp3/320mp3/flac/hires 全正 → 四档折档集合（Low 恒在 = legacy Low128 恒在同口径）"
            );
            // songId/strMediaMid/size_* 经 extra 原样透传：流地址/歌词面
            // 取址的数据前提（musid resolve 的 media_mid 优先 extra
            // ["strMediaMid"]，source_qqmusic.rs:815-819；content_length 读
            // size_* 键 :852；歌词面 songId 数值解析 :873-877）。
            assert_eq!(
                song.extra.get("songId").map(String::as_str),
                Some("483034200")
            );
            assert_eq!(
                song.extra.get("strMediaMid").map(String::as_str),
                Some("003aAYrm3GE0Ac")
            );
            // 反向回组 Track：id/extra 保真（resolve 的数据链闭合）。
            let rebuilt = song_to_track(&song).expect("qqmusic source must map");
            assert_eq!(rebuilt.id, track.id);
            assert_eq!(rebuilt.extra, track.extra);
        }

        #[test]
        fn tx_search_parity_normalized_equivalent() {
            // legacy 侧用**真实的** `crate::tx::search::parse_song`
            // （tx/search.rs:126，本就 pub(crate)）从平台原始响应形状组出
            // SongInfo——原始 item 与 musid-dli
            // tests/fixtures/qqmusic/search_songs.json data.song.list[0]
            // 同形（该目录为重建形状记录，provenance 见 docs §12.3 批内
            // 登记）。musid 侧：track_to_song（voicefox 侧 wire 形状
            // fixture）。任一侧 parse 改字段必红。
            let platform_item = json!({
                "id": 483034200,
                "mid": "003aAYrm3GE0Ac",
                "title": "晴天",
                "singer": [{ "name": "周杰伦" }, { "name": "杨瑞代" }],
                "album": { "mid": "002Neh8l0uciQ1", "name": "叶惠美" },
                "interval": 269,
                "file": {
                    "media_mid": "003aAYrm3GE0Ac",
                    "size_128mp3": 4312832,
                    "size_320mp3": 10781692,
                    "size_flac": 23706814,
                    "size_hires": 54915322
                }
            });
            let legacy = crate::tx::search::parse_song(&platform_item)
                .expect("真实 parse_song 必须能解析捕获形状");
            assert_eq!(legacy.id, "003aAYrm3GE0Ac");
            assert_eq!(legacy.source, SourceId::Tx);
            assert_eq!(legacy.name, "晴天");
            assert_eq!(legacy.singer, "周杰伦、杨瑞代");
            assert_eq!(legacy.album_name, "叶惠美");
            assert_eq!(legacy.duration, Duration::from_secs(269));
            assert_eq!(
                legacy.qualities,
                [
                    Quality::Low128,
                    Quality::High320,
                    Quality::Flac,
                    Quality::Flac24
                ]
                .into_iter()
                .collect()
            );

            let musid = track_to_song(&tx_track_fixture()).expect("qqmusic provider must map");
            // tx 搜索面 qualities 完全对齐（size>0 → 四档两侧同表；musid
            // 恒产 Low 与 legacy 恒产 Low128 在 size_128mp3 全正的真实形状
            // 下同集合——size_128mp3 缺席的簿记分歧登记在 §12.2 qqmusic 块，
            // 不进本对照）。
            assert!(
                crate::parity::songs_equivalent(&legacy, &musid),
                "归一对照命中（qualities 集合相等）"
            );
            assert!(
                crate::parity::compare_search_items(
                    std::slice::from_ref(&legacy),
                    std::slice::from_ref(&musid)
                ),
                "搜索产物集对照命中"
            );

            let mut different = legacy;
            different.name = "七里香".to_string();
            assert!(!crate::parity::songs_equivalent(&different, &musid));
        }

        // --- V2 切片 9（netease/wy，docs §12.4）：三面全 Migrated ---

        fn wy_track_fixture() -> Track {
            from_str::<Track>(fixture!("track_wy.json")).expect("wy track fixture must parse")
        }

        #[test]
        fn wy_track_fixture_maps_with_pl_level_extra_passthrough() {
            let track = wy_track_fixture();
            let song = track_to_song(&track).expect("netease provider must map");
            assert_eq!(song.source, SourceId::Wy);
            assert_eq!(song.id, "789");
            assert_eq!(song.name, "老接口");
            assert_eq!(song.singer, "周杰伦、费玉清");
            assert_eq!(song.album_name, "老专辑");
            assert_eq!(song.album_id, "42");
            assert_eq!(song.duration, Duration::from_secs(258));
            assert_eq!(
                song.qualities,
                [
                    Quality::Low128,
                    Quality::High320,
                    Quality::Flac,
                    Quality::Flac24
                ]
                .into_iter()
                .collect(),
                "musid Low 恒在 + h/sq/hr>0 → 四档；折档后与 legacy 恒产 Low128 + h/sq/hr 同集合（无簿记分歧）"
            );
            // pl_level 经 extra 原样透传（musid parse_track 记登录态档位
            // 上限，source_netease.rs:398-401）；resolve 侧 expires_at_unix
            // 由 URL 提取（source_netease.rs:527）——G4 过期门对 wy 设防。
            assert_eq!(
                song.extra.get("pl_level").map(String::as_str),
                Some("lossless")
            );
            // 反向回组 Track：id/extra 保真。
            let rebuilt = song_to_track(&song).expect("netease source must map");
            assert_eq!(rebuilt.id, track.id);
            assert_eq!(rebuilt.extra, track.extra);
        }

        #[test]
        fn wy_search_parity_normalized_equivalent() {
            // legacy 侧用**真实的** `crate::wy::search::parse_song`
            // （wy/search.rs:102，本就 pub(crate)）从平台原始响应形状组出
            // SongInfo。平台 item 形状 = eapi 搜索 v3 资源（ar/al/dt/h/sq/
            // hr），样本值取自 musid-dli
            // tests/fixtures/netease/song_detail_legacy.json songs[0]（老
            // 接口形状 artists/album/duration 的同值 v3 重排——legacy
            // parse_song 只认 v3 字段名，golden 值与 musid 侧 fixture 同源，
            // provenance 见 docs §12.3 批内登记）。musid 侧：
            // track_to_song（voicefox 侧 wire 形状 fixture）。任一侧 parse
            // 改字段必红。±1s 时长容差（parity.rs:174-179）与折档后
            // qualities 集合相等（:180）即本对照的口径。
            let platform_item = json!({
                "id": 789,
                "name": "老接口",
                "ar": [{ "id": 6452, "name": "周杰伦" }, { "id": 2, "name": "费玉清" }],
                "al": { "id": 42, "name": "老专辑", "picUrl": "https://p" },
                "dt": 258000,
                "h": { "size": 8123456 },
                "sq": { "size": 24567890 },
                "privilege": { "maxBrLevel": "hires", "plLevel": "lossless" }
            });
            let legacy = crate::wy::search::parse_song(&platform_item)
                .expect("真实 parse_song 必须能解析捕获形状");
            assert_eq!(legacy.id, "789");
            assert_eq!(legacy.source, SourceId::Wy);
            assert_eq!(legacy.name, "老接口");
            assert_eq!(legacy.singer, "周杰伦、费玉清");
            assert_eq!(legacy.album_name, "老专辑");
            assert_eq!(legacy.duration, Duration::from_secs(258));
            assert_eq!(
                legacy.qualities,
                [
                    Quality::Low128,
                    Quality::High320,
                    Quality::Flac,
                    Quality::Flac24
                ]
                .into_iter()
                .collect()
            );

            let musid = track_to_song(&wy_track_fixture()).expect("netease provider must map");
            assert!(
                crate::parity::songs_equivalent(&legacy, &musid),
                "归一对照命中（±1s 时长容差 + 折档后 qualities 集合相等）"
            );
            assert!(
                crate::parity::compare_search_items(
                    std::slice::from_ref(&legacy),
                    std::slice::from_ref(&musid)
                ),
                "搜索产物集对照命中"
            );

            let mut different = legacy;
            different.album_name = "叶惠美".to_string();
            assert!(!crate::parity::songs_equivalent(&different, &musid));
        }

        // --- V2 切片 10（qianqian，docs §12.4）：三面全 Migrated ---

        fn qianqian_track_fixture() -> Track {
            from_str::<Track>(fixture!("track_qianqian.json"))
                .expect("qianqian track fixture must parse")
        }

        #[test]
        fn qianqian_track_fixture_maps_with_tsid_extra_passthrough() {
            let track = qianqian_track_fixture();
            let song = track_to_song(&track).expect("qianqian provider must map");
            assert_eq!(song.source, SourceId::Qianqian);
            assert_eq!(song.id, "T10065400429");
            assert_eq!(song.name, "等晴天");
            assert_eq!(song.singer, "周深");
            assert_eq!(song.album_name, "等晴天");
            assert_eq!(song.album_id, "P10004348672");
            assert_eq!(song.duration, Duration::from_secs(268));
            assert_eq!(
                song.qualities,
                [Quality::Low128, Quality::High320, Quality::Flac]
                    .into_iter()
                    .collect(),
                "rateFileInfo 3000/320/128|64 → Lossless/High/Low 折档（两侧 rate 表同形）"
            );
            // tsid/album_asset_code 经 extra 原样透传：流地址/歌词面取址的
            // 数据前提（musid resolve/lyrics 的 TSID 优先 extra["tsid"]，
            // source_qianqian.rs:786-790、:835-839；空 tsid → 离线
            // NotFound）。
            assert_eq!(
                song.extra.get("tsid").map(String::as_str),
                Some("T10065400429")
            );
            assert_eq!(
                song.extra.get("album_asset_code").map(String::as_str),
                Some("P10004348672")
            );
            // 反向回组 Track：id/extra 保真。
            let rebuilt = song_to_track(&song).expect("qianqian source must map");
            assert_eq!(rebuilt.id, track.id);
            assert_eq!(rebuilt.extra, track.extra);
        }

        // V4-s2（§14.3）：legacy qianqian 已删，`qianqian_search_parity_
        // normalized_equivalent`（crate::qianqian::song::parse_song 双侧
        // 对账）随源注销——musid 侧形状由 track_qianqian.json fixture 映射
        // 用例与 musid 仓 tests/fixtures/qianqian 的 provider 测试继续钉住
        // （rateFileInfo 3000/320/128|64 折档两侧同表的对账证据在 musid
        // 仓 source_qianqian.rs 的 fixture 批里）。

        // --- V2 切片 11（joox，docs §12.4）：三面全 Migrated ---

        fn joox_track_fixture() -> Track {
            from_str::<Track>(fixture!("track_joox.json")).expect("joox track fixture must parse")
        }

        #[test]
        fn joox_track_fixture_maps_with_songid_extra_passthrough() {
            let track = joox_track_fixture();
            let song = track_to_song(&track).expect("joox provider must map");
            assert_eq!(song.source, SourceId::Joox);
            assert_eq!(song.id, "bLnv0PqDX_qAlIqapc+Okw==");
            assert_eq!(song.name, "晴天");
            assert_eq!(song.singer, "周杰倫");
            assert_eq!(song.album_name, "葉惠美");
            assert_eq!(song.album_id, "YiT3xohoUs5qeo9aJ2yjDw==");
            assert_eq!(song.duration, Duration::from_secs(269));
            assert!(
                song.qualities.is_empty(),
                "搜索/容器响应无 kbps 数据 → available_qualities 空（档位只在 fetch_song_detail，两侧同口径）"
            );
            // songid 经 extra 原样透传（加密串）：流地址/歌词面取址的数据
            // 前提（musid resolve/lyrics 的 songid 优先 extra["songid"]，
            // source_joox.rs:749-756、:797-804；空 songid → 离线
            // NotFound）。
            assert_eq!(
                song.extra.get("songid").map(String::as_str),
                Some("bLnv0PqDX_qAlIqapc+Okw==")
            );
            // 反向回组 Track：id/extra 保真。
            let rebuilt = song_to_track(&song).expect("joox source must map");
            assert_eq!(rebuilt.id, track.id);
            assert_eq!(rebuilt.extra, track.extra);
        }

        // V4-s1（§14.1）：legacy joox 已删，`joox_search_parity_normalized_
        // equivalent`（crate::joox::api::songs_from_search 双侧对账）随源
        // 注销——musid 侧形状由 track_joox.json fixture 映射用例与 musid
        // 仓 tests/fixtures/joox 的 provider 测试继续钉住；§12.2 joox 块
        // 的歌手 join 分歧（legacy " / " vs musid '、'）随 legacy 死亡
        // 收敛为 musid 口径（voicefox SongInfo 约定即 '、'）。

        // --- V4 集合面直接测试（优化轮 P2-1）：脚本桩四槽 + joox 装配，
        // --- 逐面钉路由/转换/空守卫/三分支 parse_link/未接线语义。

        /// 集合面脚本桩：四个 trait 各带可编程应答与调用记录。
        #[cfg(feature = "musid-dli-adapter-joox")]
        struct CollectionStub {
            playlists: std::sync::Mutex<Vec<musid_dli::models::Playlist>>,
            playlist_tracks:
                std::sync::Mutex<std::collections::HashMap<String, Vec<musid_dli::models::Track>>>,
            album_tracks:
                std::sync::Mutex<std::collections::HashMap<String, Vec<musid_dli::models::Track>>>,
            resolved: std::sync::Mutex<Option<musid_dli::provider::ResolvedLink>>,
            calls: std::sync::atomic::AtomicUsize,
        }

        #[cfg(feature = "musid-dli-adapter-joox")]
        impl Default for CollectionStub {
            fn default() -> Self {
                Self {
                    playlists: std::sync::Mutex::new(Vec::new()),
                    playlist_tracks: std::sync::Mutex::new(std::collections::HashMap::new()),
                    album_tracks: std::sync::Mutex::new(std::collections::HashMap::new()),
                    resolved: std::sync::Mutex::new(None),
                    calls: std::sync::atomic::AtomicUsize::new(0),
                }
            }
        }

        #[cfg(feature = "musid-dli-adapter-joox")]
        impl musid_dli::provider::MusicProvider for CollectionStub {
            fn id(&self) -> &str {
                "joox"
            }
            fn name(&self) -> &str {
                "集合桩"
            }
            fn capabilities(&self) -> ProviderCapabilities {
                ProviderCapabilities {
                    playlists: true,
                    albums: true,
                    link_resolve: true,
                    ..Default::default()
                }
            }
        }

        #[cfg(feature = "musid-dli-adapter-joox")]
        #[async_trait]
        impl MdCollectionProvider for CollectionStub {
            async fn playlist(&self, id: &str) -> Result<musid_dli::models::Playlist, MdError> {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let playlists = self.playlists.lock().unwrap_or_else(|e| e.into_inner());
                let mut playlist =
                    playlists
                        .first()
                        .cloned()
                        .unwrap_or_else(|| musid_dli::models::Playlist {
                            id: TrackId::new("joox", id),
                            title: "桩歌单".into(),
                            description: None,
                            artwork_url: None,
                            track_count: None,
                            tracks: vec![],
                            play_count: None,
                            creator: None,
                            link: None,
                            extra: Default::default(),
                        });
                playlist.id = TrackId::new("joox", id);
                Ok(playlist)
            }
            async fn playlist_tracks(
                &self,
                id: &str,
            ) -> Result<Vec<musid_dli::models::Track>, MdError> {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(self
                    .playlist_tracks
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(id)
                    .cloned()
                    .unwrap_or_default())
            }
        }

        #[cfg(feature = "musid-dli-adapter-joox")]
        #[async_trait]
        impl MdPlaylistProvider for CollectionStub {
            async fn categories(
                &self,
            ) -> Result<Vec<musid_dli::models::PlaylistCategory>, MdError> {
                Err(MdError::Unsupported("joox".into()))
            }
            async fn playlists(
                &self,
                _category: &str,
                _page: u32,
                _page_size: u32,
            ) -> Result<Vec<musid_dli::models::Playlist>, MdError> {
                Err(MdError::Unsupported("joox".into()))
            }
            async fn search_playlists(
                &self,
                _keyword: &str,
                _page: u32,
            ) -> Result<Vec<musid_dli::models::Playlist>, MdError> {
                Ok(self
                    .playlists
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone())
            }
            async fn user_playlists(
                &self,
                _page: u32,
                _page_size: u32,
            ) -> Result<Vec<musid_dli::models::Playlist>, MdError> {
                Err(MdError::Unsupported("joox".into()))
            }
        }

        #[cfg(feature = "musid-dli-adapter-joox")]
        #[async_trait]
        impl MdAlbumProvider for CollectionStub {
            async fn album(&self, id: &str) -> Result<MdAlbum, MdError> {
                let playlist = MdCollectionProvider::playlist(self, id).await?;
                Ok(MdAlbum {
                    id: playlist.id,
                    title: playlist.title,
                    artists: vec![],
                    artwork_url: playlist.artwork_url,
                    track_count: playlist.track_count,
                })
            }
            async fn album_tracks(
                &self,
                id: &str,
            ) -> Result<Vec<musid_dli::models::Track>, MdError> {
                Ok(self
                    .album_tracks
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(id)
                    .cloned()
                    .unwrap_or_default())
            }
        }

        #[cfg(feature = "musid-dli-adapter-joox")]
        #[async_trait]
        impl MdLinkResolver for CollectionStub {
            async fn resolve_link(
                &self,
                _url: &str,
            ) -> Result<musid_dli::provider::ResolvedLink, MdError> {
                self.resolved
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
                    .ok_or_else(|| MdError::InvalidInput("未编排链接".into()))
            }
        }

        #[cfg(feature = "musid-dli-adapter-joox")]
        fn collection_adapter(stub: &Arc<CollectionStub>) -> AdapterMusicSource {
            AdapterMusicSource::new(
                SourceId::Joox,
                "集合桩源（musid-dli）".to_string(),
                None,
                None,
                None,
                TransportInjection::new(None, 15),
            )
            .with_collections(
                Arc::clone(stub) as Arc<dyn MdCollectionProvider>,
                Arc::clone(stub) as Arc<dyn MdPlaylistProvider>,
                Arc::clone(stub) as Arc<dyn MdAlbumProvider>,
                Arc::clone(stub) as Arc<dyn MdLinkResolver>,
            )
        }

        #[cfg(feature = "musid-dli-adapter-joox")]
        fn stub_track(id: &str, title: &str) -> musid_dli::models::Track {
            musid_dli::models::Track {
                id: TrackId::new("joox", id),
                title: title.to_string(),
                artists: vec![],
                album: None,
                album_id: None,
                duration: Some(std::time::Duration::from_secs(200)),
                artwork_url: None,
                isrc: None,
                available_qualities: vec![],
                audio: None,
                extra: Default::default(),
            }
        }

        #[cfg(feature = "musid-dli-adapter-joox")]
        #[tokio::test]
        async fn collection_search_playlists_routes_and_converts() {
            let stub = Arc::new(CollectionStub::default());
            stub.playlists
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(musid_dli::models::Playlist {
                    id: TrackId::new("joox", "pl-1"),
                    title: "歌单一".into(),
                    description: Some("描述".into()),
                    artwork_url: Some("https://cover.example/1.jpg".into()),
                    track_count: Some(12),
                    tracks: vec![],
                    play_count: Some(88),
                    creator: Some("作者".into()),
                    link: Some("https://web.example/pl/1".into()),
                    extra: Default::default(),
                });
            let adapter = collection_adapter(&stub);

            let items = adapter
                .search_playlists("晴天", 1)
                .await
                .expect("搜索歌单面路由");
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].id, "pl-1");
            assert_eq!(items[0].name, "歌单一");
            assert_eq!(items[0].source, SourceId::Joox);
            assert_eq!(items[0].song_count, 12);
            assert_eq!(items[0].play_count, Some(88));
            assert_eq!(items[0].creator.as_deref(), Some("作者"));
            assert_eq!(items[0].link.as_deref(), Some("https://web.example/pl/1"));
        }

        #[cfg(feature = "musid-dli-adapter-joox")]
        #[tokio::test]
        async fn collection_detail_converts_tracks_and_empty_guards_to_not_found() {
            let stub = Arc::new(CollectionStub::default());
            stub.playlist_tracks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert("pl-1".into(), vec![stub_track("t-1", "曲目一")]);
            let adapter = collection_adapter(&stub);

            let songs = adapter
                .get_playlist_detail("pl-1", 1)
                .await
                .expect("详情面路由");
            assert_eq!(songs.len(), 1);
            assert_eq!(songs[0].id, "t-1");
            assert_eq!(songs[0].name, "曲目一");
            assert_eq!(songs[0].duration, std::time::Duration::from_secs(200));

            // 空曲目 → NotFound（legacy 空守卫同口径）。
            let stub2 = Arc::new(CollectionStub::default());
            let adapter2 = collection_adapter(&stub2);
            assert!(matches!(
                adapter2.get_playlist_detail("pl-empty", 1).await,
                Err(FetchError::NotFound)
            ));
        }

        #[cfg(feature = "musid-dli-adapter-joox")]
        #[tokio::test]
        async fn collection_album_songs_returns_search_result_shape() {
            let stub = Arc::new(CollectionStub::default());
            stub.album_tracks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(
                    "al-1".into(),
                    vec![stub_track("t-1", "曲一"), stub_track("t-2", "曲二")],
                );
            let adapter = collection_adapter(&stub);
            let album = lx_core::model::playlist::Album {
                id: "al-1".into(),
                name: "专辑".into(),
                source: SourceId::Joox,
                cover_url: None,
                artist: String::new(),
            };
            let result = adapter
                .get_album_songs(&album, 0, 200)
                .await
                .expect("专辑曲目面路由");
            assert_eq!(result.total, 2);
            assert!(!result.has_more);
            assert_eq!(result.items.len(), 2);
        }

        #[cfg(feature = "musid-dli-adapter-joox")]
        #[tokio::test]
        async fn collection_parse_link_maps_all_three_shapes() {
            // Track 分支。
            let stub = Arc::new(CollectionStub::default());
            *stub.resolved.lock().unwrap_or_else(|e| e.into_inner()) = Some(
                musid_dli::provider::ResolvedLink::Track(stub_track("t-9", "单曲")),
            );
            let adapter = collection_adapter(&stub);
            match adapter
                .parse_link("https://web.example/song/t-9")
                .await
                .expect("song 分支")
            {
                ParsedLink::Song(song) => assert_eq!(song.id, "t-9"),
                other => panic!("期望 Song 分支，实际 {other:?}"),
            }

            // Playlist 分支：实体不带 tracks → 二次取 playlist_tracks。
            *stub.resolved.lock().unwrap_or_else(|e| e.into_inner()) = Some(
                musid_dli::provider::ResolvedLink::Playlist(musid_dli::models::Playlist {
                    id: TrackId::new("joox", "pl-9"),
                    title: "链歌单".into(),
                    description: None,
                    artwork_url: None,
                    track_count: None,
                    tracks: vec![],
                    play_count: None,
                    creator: None,
                    link: None,
                    extra: Default::default(),
                }),
            );
            stub.playlist_tracks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert("pl-9".into(), vec![stub_track("t-8", "链曲目")]);
            match adapter
                .parse_link("https://web.example/playlist/pl-9")
                .await
                .expect("playlist 分支")
            {
                ParsedLink::Playlist { playlist, songs } => {
                    assert_eq!(playlist.id, "pl-9");
                    assert_eq!(songs.len(), 1);
                    assert_eq!(songs[0].id, "t-8");
                }
                other => panic!("期望 Playlist 分支，实际 {other:?}"),
            }

            // Album 分支：album_tracks 取曲。
            *stub.resolved.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(musid_dli::provider::ResolvedLink::Album(MdAlbum {
                    id: TrackId::new("joox", "al-9"),
                    title: "链专辑".into(),
                    artists: vec!["链作者".to_string()],
                    artwork_url: None,
                    track_count: None,
                }));
            stub.album_tracks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert("al-9".into(), vec![stub_track("t-7", "专辑曲目")]);
            match adapter
                .parse_link("https://web.example/album/al-9")
                .await
                .expect("album 分支")
            {
                ParsedLink::Album { playlist, songs } => {
                    assert_eq!(playlist.id, "al-9");
                    assert_eq!(playlist.name, "链专辑");
                    assert_eq!(playlist.creator.as_deref(), Some("链作者"));
                    assert_eq!(songs.len(), 1);
                }
                other => panic!("期望 Album 分支，实际 {other:?}"),
            }
        }

        #[cfg(feature = "musid-dli-adapter-joox")]
        #[tokio::test]
        async fn collection_browse_faces_fall_back_to_trait_defaults_when_unwired() {
            // 未 with_collections 的适配器：浏览面走 trait 默认（空列表），
            // search_playlists 走「不支持」错误，capabilities 位如实为假。
            let adapter = AdapterMusicSource::new(
                SourceId::Joox,
                "裸源（musid-dli）".to_string(),
                None,
                None,
                None,
                TransportInjection::new(None, 15),
            );
            assert!(
                adapter
                    .get_playlists("", 1)
                    .await
                    .expect("空列表")
                    .is_empty(),
                "未接线浏览面 = trait 默认空列表"
            );
            assert!(
                adapter
                    .get_playlist_categories()
                    .await
                    .expect("空列表")
                    .is_empty()
            );
            assert!(adapter.search_playlists("k", 1).await.is_err());
            let caps = adapter.capabilities();
            assert!(!caps.playlist_search, "槽位缺席 → 位为假（P3-6 推导）");
        }

        // --- 登记切片（reg-soda / reg-ytm，docs §12.4 末段）：不迁，断言
        // --- 随所在 feature 门进外层门禁（--lib）。

        #[cfg(feature = "musid-dli-adapter-reg-soda")]
        #[test]
        fn reg_soda_is_voicefox_only_and_never_routes() {
            // soda 是 voicefox-only 源：musid-dli 0.2.0 的 12 个 source 模块
            // 无 source_soda（src/lib.rs:40-104 cfg 表）→ 本 feature 没有
            // musid-dli/source-* 可挂；§1 命名表显式拒绝（命名表双向 None）；
            // 运行期路由判定显式排除 SourceId::Soda（manager.rs matches! 分
            // 支）；注册表亦无行（face_route 的 LegacyOnly 镜像）。voicefox
            // 自实现完整保留（soda/mod.rs search/get_song_url/get_lyric/
            // search_playlists + api.rs/crypto.rs/play.rs），四面恒旧路径，
            // 无 parity 对账；未来若 musid-dli 出 soda provider，需先扩
            // musid-dli namespace 表 + voicefox SourceId 映射 + 新子 feature，
            // 属上游变更。
            assert_eq!(source_id_to_provider(SourceId::Soda), None);
            assert_eq!(provider_to_source_id("soda"), None);
            assert!(source_route_entry(SourceId::Soda).is_none());
            for face in [ParityFace::Search, ParityFace::SongUrl, ParityFace::Lyric] {
                assert!(
                    matches!(face_route(face, SourceId::Soda), FaceRoute::LegacyOnly(_)),
                    "soda {} 恒旧路径（无行 → LegacyOnly 镜像）",
                    face.as_str()
                );
            }
        }

        #[cfg(feature = "musid-dli-adapter-reg-ytm")]
        #[test]
        fn reg_ytm_is_musid_dli_only_and_maps_to_no_source() {
            // youtube-music 是 musid-dli-only 源：voicefox SourceId 枚举无
            // 对应变体（Kw/Kg/Tx/Wy/Mg/Bili/Soda/Qianqian/Joox/Jamendo/
            // Fivesing/Apple/Local 十三变体）；musid 侧 RustyPipeProvider
            // 仅 TrackSearch+StreamResolver 两 trait、无 LyricsProvider，
            // 位于 `source-youtube-music` feature 后（依赖 rustypipe——
            // voicefox 不启用该 feature，rustypipe 不进 voicefox feature
            // 树）。命名表拒绝：provider_to_source_id("youtube-music")==None；
            // track 携带该 provider 时映射报 UnmappedProvider（§2.1 绝不
            // 静默改名）。voicefox 若未来引入 ytm 源，属新源接入（扩
            // SourceId + 命名表 + 评估 rustypipe 依赖），不在本迁移序列。
            assert_eq!(provider_to_source_id("youtube-music"), None);
            let ytm_track = Track {
                id: TrackId::new("youtube-music", "dQw4w9WgXcQ"),
                title: "登记用例占位".into(),
                artists: vec![],
                album: None,
                album_id: None,
                duration: None,
                artwork_url: None,
                isrc: None,
                available_qualities: vec![],
                audio: None,
                extra: Default::default(),
            };
            assert!(
                matches!(
                    track_to_song(&ytm_track),
                    Err(AdapterError::UnmappedProvider(_))
                ),
                "youtube-music provider 查表失败必须显式报错（不静默改名/丢弃）"
            );
        }
    }
}
