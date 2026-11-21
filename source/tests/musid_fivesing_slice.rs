//! V2 切片 2（fivesing）：**真实** musid-dli `FivesingProvider` 的行为对账
//! （与切片 1 jamendo 同形，评审 P1——复刻桩只测适配层自身的映射/门控行为，
//! 本文件挂 `musid-dli-adapter-fivesing` 门、构造真实 provider，把桩与真实
//! 实现的一致性钉进确定性门禁）。与 jamendo 的关键差异：fivesing 三个面都
//! 是**网络现取**（search/newget/getSongUrl 端点），确定性门禁内不可真跑
//! ——本文件钉住真实 provider 的离线行为（无 kind 后缀 → `split_track_id`
//! 失败 → 动态层触发点）、descriptor 账面依据（§12.4 行 2）、manager 级
//! 双侧真实链路（musid Provider 错误 → legacy NotFound，MusidErrLegacyErr）
//! 与歌词面指纹 parity；三个面的真网行为由运行期 probe 对账覆盖
//! （docs §12.3）。装配形态与 `musid_bootstrap::assemble!` 同形。

#![cfg(feature = "musid-dli-adapter-fivesing")]

use std::sync::Arc;

use lx_core::model::song::SongInfo;
use lx_core::model::source::{Quality, SourceId};
use lx_core::traits::source::{FetchError, MusicSource};
use lx_source::adapter_musid::{AdapterMusicSource, TransportInjection};
use lx_source::manager::{MusidRouteConfig, SourceManager};
use lx_source::parity::{ParityFace, ParityOutcome};
use musid_dli::provider::{AuthCapability, MusicProvider, ProviderCapabilities};

/// 真实 provider 按装配点同形拆三面（assemble! 的独立重放）。
fn real_adapter() -> AdapterMusicSource {
    let provider = Arc::new(musid_dli::source_fivesing::FivesingProvider::new());
    AdapterMusicSource::new(
        SourceId::Fivesing,
        format!("{}（musid-dli）", provider.name()),
        Some(provider.clone()),
        Some(provider.clone()),
        Some(provider),
        TransportInjection::new(None, 15),
    )
}

fn count_of(manager: &SourceManager, face: ParityFace, outcome: ParityOutcome) -> u64 {
    manager
        .musid_parity_snapshot()
        .into_iter()
        .find(|record| {
            record.face == face && record.source == SourceId::Fivesing && record.outcome == outcome
        })
        .map(|record| record.count)
        .unwrap_or(0)
}

/// 真实 descriptor pin：注册表 fivesing 行（三面全 Migrated + 集合面/链接
/// 直解不设行项）的依据逐位对上真实 provider——track_search/stream_resolve/
/// lyrics/playlists/link_resolve true，authentication=false +
/// `AuthCapability::None`（5sing 这些端点没有账号系统，上游注册测试 pin），
/// 专辑/榜单 false（双侧同缺），supported_qualities 未覆写（缺省空）。
#[test]
fn real_descriptor_pins_the_v2_fivesing_ledger_row() {
    let provider = musid_dli::source_fivesing::FivesingProvider::new();
    assert_eq!(
        provider.capabilities(),
        ProviderCapabilities {
            track_search: true,
            stream_resolve: true,
            lyrics: true,
            playlists: true,
            link_resolve: true,
            ..ProviderCapabilities::default()
        },
        "fivesing 真实 descriptor 漂移即红（§12.4 账面依据）"
    );
    assert_eq!(provider.auth_capability(), AuthCapability::None);
    assert!(
        provider.supported_qualities().is_empty(),
        "fivesing 未覆写 supported_qualities（musid-dli MusicProvider 缺省空）→ 适配层翻译产物为空"
    );
}

/// legacy 搜到的歌若缺 kind 后缀（id 无 `|` 且 extra 无 songid/songtype）：
/// 真实 provider 的 `split_track_id` 在**任何网络调用之前**失败
/// （source_fivesing.rs:154-169，Provider 类错误）→ `FetchError::Other` →
/// manager 按 V1 语义立即旧链重试并计 MusidErrLegacyOk——能力缺失回落
/// 规则的动态层触发点（docs §12.2），用户无感知。错误文本保留 musid 侧
/// 语义供诊断（非 NotFound，与 legacy 同形失败的 NotFound 区分）。
#[tokio::test]
async fn real_provider_fails_offline_for_ids_without_the_kind_suffix() {
    let adapter = real_adapter();
    let song = SongInfo::new(
        "15622111".to_string(),
        SourceId::Fivesing,
        "晴天".to_string(),
        "Vk".to_string(),
    );
    let error = adapter
        .get_song_url(&song, Quality::High320)
        .await
        .expect_err("真实 provider 无 kind 后缀必须离线失败");
    assert!(matches!(error, FetchError::Other(_)), "实际: {error}");
    assert!(
        error.to_string().contains("song type"),
        "split_track_id 的 Provider 错误文本: {error}"
    );
}

/// manager 级双侧真实链路（零网络）：真实 legacy 源 + 真实 musid 影子源 +
/// 注册表放行（三面 Migrated）→ 无 kind 后缀的歌在流地址面两栈同请求都
/// 失败——musid 侧 `split_track_id` Provider 错误（离线）、legacy 侧
/// `split_song_id` 返回 None → NotFound（fivesing/mod.rs:96，离线）——
/// 计 MusidErrLegacyErr，legacy 错误原样向上。替换模式的完整失败链一次
/// 走通（真实 provider 的搜索/歌词/resolve 成功路径是网络面，由运行期
/// probe 对账覆盖，docs §12.3）。
#[tokio::test]
async fn manager_falls_back_to_the_real_legacy_source_when_musid_fails_offline() {
    let mut manager = SourceManager::new(SourceId::Kw, &[SourceId::Fivesing]);
    manager.register(Arc::new(lx_source::fivesing::FivesingSource::new()));
    let registered = lx_source::musid_bootstrap::register_available(
        &manager,
        &TransportInjection::new(None, 15),
    );
    assert!(
        registered.contains(&SourceId::Fivesing),
        "真实装配分支编译并生效"
    );
    manager.set_musid_routes(MusidRouteConfig {
        stack: true,
        search: true,
        song_url: true,
        lyric: true,
        parity_probe: false,
    });

    let song = SongInfo::new(
        "15622111".to_string(),
        SourceId::Fivesing,
        "晴天".to_string(),
        "Vk".to_string(),
    );
    let error = manager
        .get_song_url(&song, Quality::High320)
        .await
        .expect_err("两栈同请求都失败");
    // legacy 侧 split_song_id None → FetchError::NotFound（fivesing/mod.rs:96），
    // 但内置先行臂把它收敛为错误文本（NotFound 的 Display 是 "not found"，
    // core/src/traits/source.rs:331），统一出口再包 Other——既有旧代码行为
    // 逐字保持（manager.rs get_song_url_inner 的 builtin_error 聚合）。
    let legacy_not_found = matches!(&error, FetchError::Other(text) if text == "not found");
    assert!(
        legacy_not_found,
        "用户拿到 legacy 失败文本（musid 的 Provider 错误只进计数）: {error:?}"
    );
    assert_eq!(
        count_of(
            &manager,
            ParityFace::SongUrl,
            ParityOutcome::MusidErrLegacyErr
        ),
        1,
        "两栈同请求都失败 → MusidErrLegacyErr（§11.5 流地址面替换模式三态）"
    );
    assert_eq!(
        count_of(&manager, ParityFace::SongUrl, ParityOutcome::MusidOk),
        0
    );
}

/// 歌词面指纹 parity（§11.6/§12.3 口径）：legacy 侧 = `fetch_lyric` 的产物
/// 形状（`LyricData { lyric: dynamicWords(trim), ..default }`，
/// fivesing/mod.rs:119-129）；musid 侧 = 真实 provider 的构造形状
/// （`LyricDocument { format: Lrc, lines: parse_lrc(original), original, .. }`
/// ，source_fivesing.rs:608-614；`parse_lrc` 是 crate 内部函数，桩按同一
/// 结构字面量承载——`document_to_lyric_data` 在原文含 `[mm:ss.xx]` 标签时
/// `lyric` 取 original 逐字（trim），指纹不依赖 lines）经适配层
/// `lyric_document_to_lx_data`。两侧指纹必等（compare_lyrics），lxlyric
/// 恒 None（musid-dli 无 YRC/QRC 序列化器，逐字不比）。
///
/// 依赖歌词转换变体（适配层 `lyric_document_to_lx_data` 挂该 feature 门），
/// 与文件的其余用例分开——单 fivesing 子 feature 构建下本模块编译为空。
#[cfg(feature = "musid-dli-adapter-lyrics")]
mod lyric_parity {
    use std::sync::Arc;

    use async_trait::async_trait;
    use lx_core::model::lyric::LyricData;
    use lx_core::model::song::SongInfo;
    use lx_core::model::source::SourceId;
    use lx_core::traits::source::MusicSource;
    use lx_source::adapter_musid::{AdapterMusicSource, TransportInjection};
    use musid_dli::Error as MdError;
    use musid_dli::models::{LyricDocument, LyricFormat, Track};
    use musid_dli::provider::{LyricsProvider as MdLyricsProvider, MusicProvider};

    /// 形状取自 musid-dli tests/fixtures/fivesing/song_info.json 的 live
    /// `dynamicWords`（2026-11-18 逐字捕获：CRLF LRC、64 行、首行
    /// "[00:03.14]作词：周杰伦"）。歌词正文不逐字复制进本仓（完整文本留在
    /// 上游 fixture），保留首行 credit + 同形状（CRLF、`[mm:ss.xx]` 标签）
    /// 的占位正文——指纹 parity（去空白行集 + 首行 `[...]` 标签）只依赖
    /// 两侧文本一致，不依赖具体词句。
    const LRC_TEXT: &str =
        "[00:03.14]作词：周杰伦\r\n[00:31.06]示例正文行一\r\n[00:34.58]示例正文行二\r\n";

    #[tokio::test]
    async fn lyric_face_fingerprint_matches_the_legacy_path() {
        struct LyricsStub;

        impl MusicProvider for LyricsStub {
            fn id(&self) -> &str {
                "fivesing"
            }
            fn name(&self) -> &str {
                "5sing"
            }
            fn capabilities(&self) -> musid_dli::provider::ProviderCapabilities {
                musid_dli::provider::ProviderCapabilities {
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
            SourceId::Fivesing,
            "5sing（musid-dli）".to_string(),
            None,
            None,
            Some(Arc::new(LyricsStub) as Arc<dyn MdLyricsProvider>),
            TransportInjection::new(None, 15),
        );
        let song = SongInfo::new(
            "15622111|fc".to_string(),
            SourceId::Fivesing,
            "晴天".to_string(),
            "Vk".to_string(),
        );
        let musid = adapter.get_lyric(&song).await.expect("歌词面映射");
        assert_eq!(
            musid.lyric,
            LRC_TEXT.trim(),
            "原文含 [mm:ss.xx] 标签 → lyric 取 original 逐字（trim，§2.10）"
        );
        assert_eq!(musid.lxlyric, None, "musid-dli 无 YRC/QRC 序列化器");

        let legacy = LyricData {
            lyric: LRC_TEXT.trim().to_string(),
            ..LyricData::default()
        };
        assert!(
            lx_source::parity::compare_lyrics(&legacy, &musid),
            "歌词面指纹 parity 命中（去空白行集 + 首行 [00:03.14] 标签）"
        );
    }
}
