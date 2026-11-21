//! V2 切片 7（kugou）：**真实** musid-dli `KugouProvider` 的行为对账
//! （与切片 1/2/3 同形，评审 P1）。kugou 的流地址面要求登录会话
//! （login_parts 无会话即硬错误，source_kugou.rs:239-258），搜索/歌词面是
//! 网络现取——确定性门禁内不可真跑。本文件钉住：
//! (a) descriptor 账面依据（§12.4 行 7：authentication=true 折叠 Cookie 的
//!     诊断位 + artists search-based fallback）；
//! (b) 流地址面的**匿名硬退化**——影子源恒匿名 new()，缺 hash 的歌在
//!     select_hash 先于 login_parts 离线 NotFound（:519-536、:717）；
//! (c) manager 级真实双侧链路（缺 hash 歌在两栈同请求都失败 →
//!     MusidErrLegacyErr，legacy 错误原样向上）；
//! (d) 歌词面逐字降级的指纹 parity（lxlyric 恒 None，正文指纹对照）。
//! 真网行为由运行期 probe 对账覆盖（docs §12.3）。装配形态与
//! `musid_bootstrap::assemble!` 同形。

#![cfg(feature = "musid-dli-adapter-kugou")]

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
    let provider = Arc::new(musid_dli::source_kugou::KugouProvider::new());
    AdapterMusicSource::new(
        SourceId::Kg,
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
            record.face == face && record.source == SourceId::Kg && record.outcome == outcome
        })
        .map(|record| record.count)
        .unwrap_or(0)
}

/// 真实 descriptor pin：注册表 kugou 行（三面全 Migrated + 登录/扫码恒旧
/// 路径 + 集合面不设行项）的依据逐位对上真实 provider——track_search/
/// stream_resolve/lyrics/authentication/playlists/charts/link_resolve/
/// albums/artists 全 true（artists=true 为 search-based fallback），
/// 无 auth_capability 覆写 → authentication bool 折叠
/// `AuthCapability::Cookie`（影子源恒匿名 new()，Cookie 只是 descriptor
/// 事实登记），supported_qualities 未覆写（缺省空）。
#[test]
fn real_descriptor_pins_the_v2_kugou_ledger_row() {
    let provider = musid_dli::source_kugou::KugouProvider::new();
    assert_eq!(
        provider.capabilities(),
        ProviderCapabilities {
            track_search: true,
            stream_resolve: true,
            lyrics: true,
            authentication: true,
            playlists: true,
            charts: true,
            link_resolve: true,
            albums: true,
            artists: true,
            ..ProviderCapabilities::default()
        },
        "kugou 真实 descriptor 漂移即红（§12.4 账面依据）"
    );
    assert_eq!(
        provider.auth_capability(),
        AuthCapability::Cookie,
        "authentication=true 的缺省折叠（Cookie 机制登记；登录链路恒旧路径）"
    );
    assert!(
        provider.supported_qualities().is_empty(),
        "kugou 未覆写 supported_qualities（缺省空）→ 适配层翻译产物为空"
    );
}

/// 流地址面的匿名硬退化（docs §12.2 kugou 块）：缺 hash 的歌在 select_hash
/// 处先于 login_parts 离线 NotFound（source_kugou.rs:717）→ 适配层
/// FetchError::NotFound。带 hash 的歌在匿名下会推进到 login_parts 硬错误
/// （"Kugou playback requires login"）——同属离线可复现的失败侧，一并 pin
/// （两态都触发动态层回落旧链，用户无感知；V1Routed 时该面已如此路由）。
#[tokio::test]
async fn resolve_fails_offline_without_a_hash_and_anonymously_with_one() {
    let adapter = real_adapter();
    let hashless = SongInfo::new(
        "123456".to_string(),
        SourceId::Kg,
        "晴天".to_string(),
        "周杰伦".to_string(),
    );
    let error = adapter
        .get_song_url(&hashless, Quality::High320)
        .await
        .expect_err("musid 无 hash 必须离线失败");
    assert!(matches!(error, FetchError::NotFound), "实际: {error:?}");

    // 带 hash：select_hash 命中 → login_parts 无会话硬错误（匿名退化）。
    let mut with_hash = SongInfo::new(
        "123456".to_string(),
        SourceId::Kg,
        "晴天".to_string(),
        "周杰伦".to_string(),
    );
    with_hash
        .extra
        .insert("FileHash".to_string(), "ABCDEF0123".to_string());
    let error = adapter
        .get_song_url(&with_hash, Quality::High320)
        .await
        .expect_err("影子源恒匿名：login_parts 硬错误");
    assert!(
        error.to_string().contains("login"),
        "musid 登录硬错误文本（英文，§9 表：不可按中文子串匹配）: {error}"
    );
}

/// manager 级双侧真实链路（零网络）：真实 legacy 源 + 真实 musid 影子源 +
/// 注册表放行（三面 Migrated）→ 缺 hash 的歌在流地址面两栈同请求都失败
/// ——musid 侧 select_hash None → Error::NotFound（离线）、legacy 侧
/// kg/url.rs select_hash 同键同回落 → NotFound（离线）——计
/// MusidErrLegacyErr，legacy 错误原样向上。替换模式的完整失败链一次走通
/// （真实 provider 的搜索/歌词/resolve 成功路径是网络面或登录面，由运行期
/// probe 对账覆盖）。
#[tokio::test]
async fn manager_falls_back_to_the_real_legacy_source_when_musid_fails_offline() {
    let mut manager = SourceManager::new(SourceId::Kw, &[SourceId::Kg]);
    manager.register(Arc::new(lx_source::kg::KgSource::new()));
    let registered = lx_source::musid_bootstrap::register_available(
        &manager,
        &TransportInjection::new(None, 15),
    );
    assert!(registered.contains(&SourceId::Kg), "真实装配分支编译并生效");
    manager.set_musid_routes(MusidRouteConfig {
        stack: true,
        search: true,
        song_url: true,
        lyric: true,
        parity_probe: false,
    });

    let song = SongInfo::new(
        "123456".to_string(),
        SourceId::Kg,
        "晴天".to_string(),
        "周杰伦".to_string(),
    );
    let error = manager
        .get_song_url(&song, Quality::High320)
        .await
        .expect_err("两栈同请求都失败");
    // legacy 侧 select_hash None → FetchError::NotFound（kg/url.rs:22-44），
    // 内置先行臂把它收敛为错误文本（NotFound 的 Display 是 "not found"），
    // 统一出口再包 Other——既有旧代码行为逐字保持（manager.rs
    // get_song_url_inner 的 builtin_error 聚合，fivesing/migu 切片同款）。
    let legacy_not_found = matches!(&error, FetchError::Other(text) if text == "not found");
    assert!(
        legacy_not_found,
        "用户拿到 legacy 失败文本（musid 的 NotFound 只进计数）: {error:?}"
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

/// 歌词面指纹 parity（§11.6/§12.3 口径）——本片最重要的行为差异登记的
/// 确定性证据：legacy 侧 = `get_lyric` 的 KRC 产物形状（`LyricData { lyric,
/// lxlyric: Some(逐字), .. }`，kg/lyric.rs:105-134 parse_krc）；musid 侧 =
/// 真实 provider 的 KRC 文档形状（KRC 解密链产物，`LyricDocument { format:
/// Krc, lines: parse_krc(原文) 带词级时间, original, .. }`，source_kugou.rs:
/// 974-1005——decrypt_krc/parse_krc 是 crate 内部函数，桩按同一结构字面量
/// 承载词级时间形状）经适配层 `lyric_document_to_lx_data`。两侧正文指纹必
/// 等（compare_lyrics），**lxlyric 恒 None（逐字降级登记，docs §12.4 行 7）**
/// ——legacy 的 KRC 逐字在路由后降级为纯 LRC 正文（词级时间只在 musid
/// LyricDocument.lines[].words，不伪造 lxlyric，§2.10）。
///
/// 依赖歌词转换变体（适配层 `lyric_document_to_lx_data` 挂该 feature 门），
/// 与文件的其余用例分开——单 kugou 子 feature 构建下本模块编译为空。
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
    use musid_dli::models::{LyricDocument, LyricFormat, LyricLine, LyricWord, Track};
    use musid_dli::provider::{
        LyricsProvider as MdLyricsProvider, MusicProvider, ProviderCapabilities,
    };

    /// LRC 正文占位（与 fivesing/kuwo 切片同款口径：正文不逐字复制，保留
    /// `[mm:ss.xx]` 标签形状——指纹 parity 只依赖两侧文本一致）。
    const LRC_TEXT: &str =
        "[00:03.14]作词：周杰伦\r\n[00:31.06]示例正文行一\r\n[00:34.58]示例正文行二\r\n";

    #[tokio::test]
    async fn lyric_face_fingerprint_matches_with_the_registered_word_level_degradation() {
        struct LyricsStub;

        impl MusicProvider for LyricsStub {
            fn id(&self) -> &str {
                "kugou"
            }
            fn name(&self) -> &str {
                "酷狗音乐"
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
                // KRC 解密链产物的形状（source_kugou.rs parse_krc）：词级
                // 时间在 lines[].words，format=Krc，original 是渲染后 LRC。
                Ok(Some(LyricDocument {
                    format: LyricFormat::Krc,
                    lines: vec![LyricLine {
                        start_ms: 3140,
                        end_ms: None,
                        text: "作词：周杰伦".to_string(),
                        words: vec![LyricWord {
                            start_ms: 3140,
                            duration_ms: 300,
                            text: "作".to_string(),
                        }],
                        translations: Default::default(),
                        romanization: None,
                    }],
                    original: LRC_TEXT.to_string(),
                    translation: None,
                    romanization: None,
                }))
            }
        }

        let adapter = AdapterMusicSource::new(
            SourceId::Kg,
            "酷狗音乐（musid-dli）".to_string(),
            None,
            None,
            Some(Arc::new(LyricsStub) as Arc<dyn MdLyricsProvider>),
            TransportInjection::new(None, 15),
        );
        let song = SongInfo::new(
            "123456".to_string(),
            SourceId::Kg,
            "晴天".to_string(),
            "周杰伦".to_string(),
        );
        let musid = adapter.get_lyric(&song).await.expect("歌词面映射");
        assert_eq!(
            musid.lyric,
            LRC_TEXT.trim(),
            "原文含 [mm:ss.xx] 标签 → lyric 取 original 逐字（trim，§2.10）"
        );
        assert_eq!(
            musid.lxlyric, None,
            "逐字降级登记：musid-dli 无 YRC/QRC 序列化器，KRC 逐字降为纯 LRC 正文"
        );

        // legacy 产物形状（kg/lyric.rs:105-134 parse_krc）：正文 + KRC 逐字。
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
