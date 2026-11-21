//! V2 切片 6（kuwo）：**真实** musid-dli `KuwoProvider` 的行为对账
//! （与切片 1/2/3 同形，评审 P1）。kuwo 的搜索面是网络现取（r.s 端点），
//! 流地址面是 mobile/web 双通道并发、歌词面是 newlyric 加密链——确定性
//! 门禁内不可真跑。本文件钉住真实 provider 的 descriptor 账面依据
//! （§12.4 行 6：Anonymous web token 的 auth_capability 覆写）与离线守卫
//! （空关键词 / 错 provider track）；manager 级路由行为（搜索/流地址
//! MusidOk）由 `musid_dual_stack.rs` 的注册表用例承载；歌词面逐字降级的
//! 指纹 parity 见本文件尾部的歌词门模块。真网行为由运行期 probe 对账
//! 覆盖（docs §12.3）。装配形态与 `musid_bootstrap::assemble!` 同形。

#![cfg(feature = "musid-dli-adapter-kuwo")]

use std::sync::Arc;

use lx_core::model::source::SourceId;
use lx_core::traits::source::{MusicSource, SearchError};
use lx_source::adapter_musid::{AdapterMusicSource, TransportInjection};
use musid_dli::StreamResolver as _;
use musid_dli::models::StreamQuality;
use musid_dli::provider::{AuthCapability, MusicProvider, ProviderCapabilities};

/// 真实 provider 按装配点同形拆三面（assemble! 的独立重放）。
fn real_adapter() -> AdapterMusicSource {
    let provider = Arc::new(musid_dli::source_kuwo::KuwoProvider::new());
    AdapterMusicSource::new(
        SourceId::Kw,
        format!("{}（musid-dli）", provider.name()),
        Some(provider.clone()),
        Some(provider.clone()),
        Some(provider),
        TransportInjection::new(None, 15),
    )
}

/// 真实 descriptor pin：注册表 kuwo 行（三面全 Migrated + 集合面不设行项）
/// 的依据逐位对上真实 provider——track_search/stream_resolve/lyrics/
/// playlists/albums/charts/link_resolve true，authentication=false +
/// auth_capability() 显式覆写 `AuthCapability::Anonymous`（kuwo web token
/// 非账号登录，source_kuwo.rs:1936-1940 上游测试 pin；§2.8 机制细分口径
/// Anonymous 不置 voicefox login 位），无 ArtistProvider（artists 位缺省
/// false），supported_qualities 未覆写（缺省空）。
#[test]
fn real_descriptor_pins_the_v2_kuwo_ledger_row() {
    let provider = musid_dli::source_kuwo::KuwoProvider::new();
    assert_eq!(
        provider.capabilities(),
        ProviderCapabilities {
            track_search: true,
            stream_resolve: true,
            lyrics: true,
            playlists: true,
            albums: true,
            charts: true,
            link_resolve: true,
            ..ProviderCapabilities::default()
        },
        "kuwo 真实 descriptor 漂移即红（§12.4 账面依据）"
    );
    assert_eq!(
        provider.auth_capability(),
        AuthCapability::Anonymous,
        "Anonymous web token 的显式覆写（上游测试 pin；不置 login 位，§2.8）"
    );
    assert!(
        provider.supported_qualities().is_empty(),
        "kuwo 未覆写 supported_qualities（缺省空）→ 适配层翻译产物为空"
    );
}

/// 空关键词在**任何网络调用之前**失败（source_kuwo.rs:1943-1946）→ 适配层
/// InvalidInput 映射 SearchError::Other（§9 表）。
#[tokio::test]
async fn empty_keyword_search_fails_offline() {
    let adapter = real_adapter();
    let error = adapter
        .search("", 1, 20)
        .await
        .expect_err("真实 provider 空关键词必须离线失败");
    assert!(matches!(error, SearchError::Other(_)), "实际: {error}");
}

/// 错 provider 的 track 在 resolve 守卫处**任何网络调用之前**失败
/// （source_kuwo.rs:1957-1960，InvalidInput）——守卫与 adapter 的
/// song_to_track 保真（song.source 直译 provider）共同保证 kuwo 影子源
/// 不会替其它源取址。
#[tokio::test]
async fn resolve_guards_against_wrong_provider_tracks_offline() {
    let provider = musid_dli::source_kuwo::KuwoProvider::new();
    let foreign = musid_dli::models::Track {
        id: musid_dli::models::TrackId::new("netease", "789"),
        title: "别家的歌".into(),
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
    let error = provider
        .resolve(
            &foreign,
            &musid_dli::provider::ResolveOptions {
                quality: StreamQuality::High,
            },
        )
        .await
        .expect_err("错 provider track 必须离线失败");
    assert!(error.to_string().to_lowercase().contains("kuwo"));
}

/// 歌词面指纹 parity（§11.6/§12.3 口径）：legacy 侧 = `get_lyric` 的产物
/// 形状（`LyricData { lyric: raw LRC, lxlyric: Some(lrcx 逐字), .. }`，
/// kw/lyric.rs:93-107——先 lrcx=1 逐字、失败不阻断）；musid 侧 = 真实
/// provider 的 LRC 文档形状（newlyric 解密链产物，`LyricDocument { format:
/// Lrc, lines: parse_lrc(original), original, .. }`，source_kuwo.rs:2072
/// 起——与 fivesing 切片用例同款：parse_lrc 是 crate 内部函数，桩按同一
/// 结构字面量承载）经适配层 `lyric_document_to_lx_data`。两侧正文指纹必等
/// （compare_lyrics），**lxlyric 恒 None（逐字降级登记，docs §12.4 行 6）**
/// ——legacy 的 lrcx 逐字在路由后降级为纯 LRC 正文。
///
/// 依赖歌词转换变体（适配层 `lyric_document_to_lx_data` 挂该 feature 门），
/// 与文件的其余用例分开——单 kuwo 子 feature 构建下本模块编译为空。
#[cfg(feature = "musid-dli-adapter-lyrics")]
mod lyric_parity {
    use std::sync::Arc;

    use async_trait::async_trait;
    use lx_core::model::lyric::LyricData;
    use lx_core::model::song::SongInfo;
    use lx_core::model::source::SourceId;
    use lx_core::traits::source::MusicSource;
    use lx_source::adapter_musid::{AdapterMusicSource, TransportInjection};
    use lx_source::parity::compare_lyrics;
    use musid_dli::Error as MdError;
    use musid_dli::models::{LyricDocument, LyricFormat, Track};
    use musid_dli::provider::{
        LyricsProvider as MdLyricsProvider, MusicProvider, ProviderCapabilities,
    };

    /// LRC 形状占位正文（与 fivesing 切片同款口径：正文不逐字复制，保留
    /// `[mm:ss.xx]` 标签形状——指纹 parity 只依赖两侧文本一致）。
    const LRC_TEXT: &str =
        "[00:03.14]作词：周杰伦\r\n[00:31.06]示例正文行一\r\n[00:34.58]示例正文行二\r\n";

    #[tokio::test]
    async fn lyric_face_fingerprint_matches_with_the_registered_word_level_degradation() {
        struct LyricsStub;

        impl MusicProvider for LyricsStub {
            fn id(&self) -> &str {
                "kuwo"
            }
            fn name(&self) -> &str {
                "酷我音乐"
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
        impl MdLyricsProvider for LyricsStub {
            async fn lyrics(&self, _track: &Track) -> Result<Option<LyricDocument>, MdError> {
                Ok(Some(LyricDocument {
                    format: LyricFormat::Lrc,
                    lines: vec![],
                    original: LRC_TEXT.to_string(),
                    translation: None,
                    romanization: None,
                }))
            }
        }

        let adapter = AdapterMusicSource::new(
            SourceId::Kw,
            "酷我音乐（musid-dli）".to_string(),
            None,
            None,
            Some(Arc::new(LyricsStub) as Arc<dyn MdLyricsProvider>),
            TransportInjection::new(None, 15),
        );
        let song = SongInfo::new(
            "228908".to_string(),
            SourceId::Kw,
            "晴天".to_string(),
            "周杰伦、费玉清".to_string(),
        );
        let musid = adapter.get_lyric(&song).await.expect("歌词面映射");
        assert_eq!(
            musid.lyric,
            LRC_TEXT.trim(),
            "原文含 [mm:ss.xx] 标签 → lyric 取 original 逐字（trim，§2.10）"
        );
        assert_eq!(
            musid.lxlyric, None,
            "逐字降级登记：musid-dli 无 YRC/QRC 序列化器，lrcx 逐字降为纯 LRC 正文"
        );

        // legacy 产物形状（kw/lyric.rs:93-107）：正文 + lrcx 逐字。
        let legacy = LyricData {
            lyric: LRC_TEXT.trim().to_string(),
            lxlyric: Some("[00:03.140]<0,300>作<300,300>词".to_string()),
            ..LyricData::default()
        };
        assert!(
            compare_lyrics(&legacy, &musid),
            "歌词面指纹 parity 命中（去空白行集 + 首行 [00:03.14] 标签；逐字豁免不比）"
        );
    }
}
