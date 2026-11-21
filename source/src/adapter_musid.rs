//! musid-dli（crates.io 0.2.0）与 lx-core / lx-source 的模型映射层（V0）+ 双栈
//! 适配源（V1）。
//!
//! feature `musid-dli-adapter` 门控，默认构建完全不引入 musid-dli。V0 硬边界：
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
    FetchError, MusicSource, SearchError, SearchResult, SongUrl, SourceCapabilities,
};
use musid_dli::models::{
    Album as MdAlbum, Artist as MdArtist, AudioFormat, AudioSpec, ByteRange, Chart, ChartEntry,
    ChecksumAlgorithm, FallbackEndpoint, LyricSearchResult as MdLyricSearchResult,
    Playlist as MdPlaylist, PlaylistCategory as MdPlaylistCategory, StreamInfo, StreamQuality,
    Track, TrackId, provider_to_voicefox_source, voicefox_source_to_provider,
};
use musid_dli::provider::{
    AuthCapability, AuthState, ProviderCapabilities, QrLoginKind, QrLoginPoll, QrLoginSession,
    QrLoginStatus, ResolveOptions, SearchPage, SearchQuery,
};
use musid_dli::provider::{
    LyricsProvider as MdLyricsProvider, StreamResolver as MdStreamResolver,
    TrackSearch as MdTrackSearch,
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
    /// 缺变体时歌词面不生效（启动 warn 由 bootstrap 负责）。
    #[cfg_attr(not(feature = "musid-dli-adapter-lyrics"), expect(dead_code))]
    lyrics: Option<Arc<dyn MdLyricsProvider>>,
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
            transport,
        }
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
    /// 面级 flags（search_playlists/categories/user_playlists）V1 装配点
    /// 未探测，按缺省 false 登记。
    fn capabilities(&self) -> SourceCapabilities {
        let Some(search) = &self.search else {
            return SourceCapabilities::default();
        };
        capabilities_from_provider(
            search.capabilities(),
            search.auth_capability(),
            ProviderFaceFlags::default(),
        )
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
            match lyrics.lyrics(&track).await {
                Ok(Some(document)) => Ok(lyric_document_to_lx_data(&document)),
                Ok(None) => Err(FetchError::NotFound),
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
    }
}
