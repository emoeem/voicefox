//! V2 切片 3（apple）：**真实** musid-dli `AppleMusicProvider` 的行为对账
//! （与切片 1/2 同形，评审 P1——复刻桩只测适配层自身的映射/门控行为，本
//! 文件挂 `musid-dli-adapter-apple` 门、构造真实 provider，把桩与真实实现
//! 的一致性钉进确定性门禁）。apple 三个面都是网络现取（AMP catalog 端点
//! 与每进程 JWT bootstrap），确定性门禁内不可真跑——本文件钉住真实
//! provider 的离线行为（空关键词搜索 / 错 provider track 的守卫）、
//! descriptor 账面依据（§12.4 行 3）与歌词面 PlainText 的空 lyric 形态
//! （登记差异 (c)：musid 槽位产物过不了内容门，动态层恒回落旧路径）；
//! 真网行为由运行期 probe 对账覆盖（docs §12.3）。装配形态与
//! `musid_bootstrap::assemble!` 同形。

#![cfg(feature = "musid-dli-adapter-apple")]

use std::sync::Arc;

use lx_core::model::source::SourceId;
use lx_core::traits::source::{MusicSource, SearchError};
use lx_source::adapter_musid::{AdapterMusicSource, TransportInjection};
use musid_dli::StreamResolver as _;
use musid_dli::models::StreamQuality;
use musid_dli::provider::{AuthCapability, MusicProvider, ProviderCapabilities};

/// 真实 provider 按装配点同形拆三面（assemble! 的独立重放）。
fn real_adapter() -> AdapterMusicSource {
    let provider = Arc::new(musid_dli::source_apple::AppleMusicProvider::new());
    AdapterMusicSource::new(
        SourceId::Apple,
        format!("{}（musid-dli）", provider.name()),
        Some(provider.clone()),
        Some(provider.clone()),
        Some(provider),
        TransportInjection::new(None, 15),
    )
}

/// 真实 descriptor pin：注册表 apple 行（三面全 Migrated + 集合面/链接
/// 直解不设行项）的依据逐位对上真实 provider——track_search/stream_resolve/
/// lyrics/playlists/albums/link_resolve true，authentication=false +
/// `AuthCapability::None`（登录态恒 LoggedOut，source_apple.rs:783-798），
/// 歌手/榜单 false（双侧皆无），supported_qualities = [Low]
/// （preview-only 契约的 descriptor 面，source_apple.rs:615-617）。
#[test]
fn real_descriptor_pins_the_v2_apple_ledger_row() {
    let provider = musid_dli::source_apple::AppleMusicProvider::new();
    assert_eq!(
        provider.capabilities(),
        ProviderCapabilities {
            track_search: true,
            stream_resolve: true,
            lyrics: true,
            playlists: true,
            albums: true,
            link_resolve: true,
            ..ProviderCapabilities::default()
        },
        "apple 真实 descriptor 漂移即红（§12.4 账面依据）"
    );
    assert_eq!(provider.auth_capability(), AuthCapability::None);
    assert_eq!(
        provider.supported_qualities(),
        vec![StreamQuality::Low],
        "preview-only 单档声明（适配层翻译产物 [Low128]，bootstrap 切片测试 pin）"
    );
}

/// 空关键词在**任何网络调用之前**失败（source_apple.rs:622-624）→ 适配层
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

/// 错 provider 的 track 在 resolve/lyrics 守卫处**任何网络调用之前**失败
/// （source_apple.rs:666-670、:701-705，InvalidInput）——守卫与 adapter 的
/// song_to_track 保真（song.source 直译 provider）共同保证 apple 影子源
/// 不会替其它源取址。
#[tokio::test]
async fn resolve_and_lyrics_guard_against_wrong_provider_tracks_offline() {
    let provider = musid_dli::source_apple::AppleMusicProvider::new();
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
    let resolve_error = provider
        .resolve(
            &foreign,
            &musid_dli::provider::ResolveOptions {
                quality: StreamQuality::High,
            },
        )
        .await
        .expect_err("错 provider track 必须离线失败");
    assert!(resolve_error.to_string().to_lowercase().contains("apple"));
    let lyric_error = musid_dli::provider::LyricsProvider::lyrics(&provider, &foreign)
        .await
        .expect_err("错 provider track 歌词面同样离线失败");
    assert!(lyric_error.to_string().to_lowercase().contains("apple"));
}

/// 歌词面 PlainText 形态（登记差异 (c)，docs §12.2 apple 块）：catalog
/// 歌词无时间戳（PlainText、lines 空）→ `document_to_lyric_data` 的
/// `trusted_or_rendered_lrc` 不信原文（无 [mm:ss.xx] 标签）、lines 渲染
/// 为空 → `lyric` 空、原文落 `raw_lrc`——musid 槽位产物过不了
/// `lyric_has_content` 内容门，动态层回落旧路径（legacy apple/mod.rs:101-110
/// 返回纯文本 lyric），用户无感知（V1Routed 时该面已路由，行为不变）。
/// 本用例用桩承载 document_to_lyric_data 的输入形状（真实 provider 的
/// apple_lyric_text 产物即此形状，lyrics.json fixture pin），把"PlainText
/// → 空 lyric + raw_lrc 原文"钉进确定性门禁。
#[cfg(feature = "musid-dli-adapter-lyrics")]
mod plain_text_lyric_shape {
    use super::*;
    use async_trait::async_trait;
    use lx_core::model::lyric::LyricData;
    use lx_core::model::song::SongInfo;
    use lx_source::parity::compare_lyrics;
    use musid_dli::models::{LyricDocument, LyricFormat, Track};
    use musid_dli::provider::LyricsProvider as MdLyricsProvider;

    /// 形状取自 musid-dli tests/fixtures/apple/lyrics.json（重建形状，
    /// README 明文；text 为占位正文）。PlainText：无时间戳、lines 空。
    const PLAIN_TEXT: &str = "故事的小黄花\n从出生那年就飘着";

    struct PlainTextLyricsStub;

    impl MusicProvider for PlainTextLyricsStub {
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
    impl MdLyricsProvider for PlainTextLyricsStub {
        async fn lyrics(&self, _track: &Track) -> Result<Option<LyricDocument>, musid_dli::Error> {
            Ok(Some(LyricDocument {
                format: LyricFormat::PlainText,
                lines: vec![],
                original: PLAIN_TEXT.to_string(),
                translation: None,
                romanization: None,
            }))
        }
    }

    #[tokio::test]
    async fn plain_text_lyric_yields_empty_lyric_with_raw_lrc_and_falls_back() {
        let adapter = AdapterMusicSource::new(
            SourceId::Apple,
            "Apple Music（musid-dli）".to_string(),
            None,
            None,
            Some(Arc::new(PlainTextLyricsStub) as Arc<dyn MdLyricsProvider>),
            TransportInjection::new(None, 15),
        );
        let song = SongInfo::new(
            "1440833098".to_string(),
            SourceId::Apple,
            "晴天".to_string(),
            "周杰伦".to_string(),
        );
        let musid = adapter.get_lyric(&song).await.expect("歌词面映射");
        assert_eq!(
            musid.lyric, "",
            "PlainText 无时间戳 + lines 空 → lyric 空（trusted_or_rendered_lrc None）"
        );
        assert_eq!(
            musid.raw_lrc.as_deref(),
            Some(PLAIN_TEXT),
            "原文落 raw_lrc（非空 original）"
        );
        assert_eq!(musid.lxlyric, None, "musid-dli 无 YRC/QRC 序列化器");

        // legacy 产物形状（apple/mod.rs:101-110）：纯文本进 lyric。
        let legacy = LyricData {
            lyric: PLAIN_TEXT.to_string(),
            ..LyricData::default()
        };
        assert!(
            !compare_lyrics(&legacy, &musid),
            "登记口径：PlainText 歌词的 musid 槽位产物过不了内容门（lyric 空），\
             probe 歌词面恒 ParityMismatch——动态层回落旧路径，用户拿到 legacy 纯文本"
        );
    }
}
