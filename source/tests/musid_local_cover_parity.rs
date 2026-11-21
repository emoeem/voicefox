//! V3 切片 s3（docs/MUSID_DLI_ADAPTER.md §13.3）：本地封面解析对账与回落。
//! musid `local_cover::read_cover`（L19 全量：内嵌首标签封面进解码校验
//! 缓存，键 sha256(音频路径字节 + 图片字节) 命名 `{hash}.jpg`，与 voicefox
//! `cover_cache::embedded_cover_path` 同配方；同目录外挂封面检出不复制）
//! 接入 `LocalSource::get_cover_url` 回落：`song.cover_url` 缺席时解析
//! 内嵌/外挂封面。本文件在运行期合成语料上钉住：
//!
//! 1. 键配方同源：voicefox 扫描管线写出的内嵌缓存路径与 musid
//!    `read_cover` 在同一缓存目录下产出的是**同一个文件**（同名同哈希）；
//! 2. 外挂封面检出：无内嵌标签 + 同目录 cover.png → get_cover_url 落
//!    外挂文件路径（新能力，本仓原实现恒 NotFound）；
//! 3. 优先级不变：`song.cover_url` 在则原样返回（扫描期产物优先，
//!    EmbeddedFirst 语义由 musid read_cover 承担）；
//! 4. 双缺恒 NotFound（行为兼容）。
//!
//! 门禁与 CI：挂 `musid-dli-adapter-artwork-local` 子 feature 门（musid
//! 侧 union：artwork + local-cover，local-cover 自带 local），随 CI 的
//! 「V3 本地封面对账」步进入 musid-stack job。

#![cfg(feature = "musid-dli-adapter-artwork-local")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::MusicSource;
use lx_source::local::LocalSource;
use lx_source::local::scanner;
use musid_dli::local_cover::{CoverPriority, read_cover};
use musid_dli::tag_write::{MetadataEdit, write_metadata};

/// 唯一临时目录（musid_local_parity 同款）。
struct FixtureDir(PathBuf);
impl FixtureDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "musid_cover_parity_{}_{}_{}",
            process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("时钟在 UNIX_EPOCH 之后")
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("临时目录可创建");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write_wav(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, wav_bytes(3000)).expect("WAV stub 可写");
        path
    }
}

/// 最小 PCM WAV stub（musid 测试同款配方）。
fn wav_bytes(duration_ms: u64) -> Vec<u8> {
    const SAMPLE_RATE: u32 = 44_100;
    const BYTES_PER_SECOND: u32 = SAMPLE_RATE * 2; // mono, 16-bit
    let data_len = (BYTES_PER_SECOND as u128 * duration_ms as u128 / 1000) as u32;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&BYTES_PER_SECOND.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.resize(wav.len() + data_len as usize, 0);
    wav
}

/// 68 字节 1x1 RGBA PNG（zlib/struct 生成，chunk CRC 与 image crate 解码
/// 均验证；musid local_cover 的 probe_dimensions 探针过）。
const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0B, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x60, 0x00, 0x02, 0x00,
    0x00, 0x05, 0x00, 0x01, 0x7A, 0x5E, 0xAB, 0x3F, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44,
    0xAE, 0x42, 0x60, 0x82,
];

fn local_song(audio: &Path) -> SongInfo {
    let mut song = SongInfo::new(
        audio.to_string_lossy().to_string(),
        SourceId::Local,
        "song".into(),
        String::new(),
    );
    song.file_path = Some(audio.to_path_buf());
    song
}

/// musid 同款 ffmpeg 构建 MP3 fixture（musid-dli 仓
/// `tests/fixtures/local/spec_mp3_cbr128.mp3` 的逐字拷贝，出处见
/// source/tests/fixtures/local/README.md）。WAV 无封面槽，内嵌用例需要
/// ID3v2 容器。
fn mp3_fixture() -> Vec<u8> {
    include_bytes!("fixtures/local/spec_mp3_cbr128.mp3").to_vec()
}

/// 1. 键配方同源：voicefox 扫描管线的内嵌缓存路径 == musid read_cover
///    在同一缓存目录的产物（同名同哈希）。内嵌封面需要 ID3v2 容器
///    （WAV 无封面槽，musid local_cover.rs 测试注释同口径），用 musid
///    同款 ffmpeg 构建 MP3 fixture（source/tests/fixtures/local/）。
#[test]
fn embedded_cache_key_recipe_matches_across_stacks() {
    let dir = FixtureDir::new("embedded_recipe");
    let audio = dir.0.join("song.mp3");
    fs::write(&audio, mp3_fixture()).expect("MP3 fixture 可写");
    write_metadata(
        &audio,
        &MetadataEdit {
            title: None,
            artist: None,
            album: None,
            cover: Some(TINY_PNG.to_vec()),
            lyrics: None,
        },
    )
    .expect("内嵌封面可写");

    // voicefox 扫描管线：扫描产物 cover_url 即缓存路径。
    let report = scanner::scan_directory_report(dir.path(), 0);
    assert_eq!(report.songs.len(), 1);
    let scanned_cover = report.songs[0]
        .song
        .cover_url
        .clone()
        .expect("扫描管线提取内嵌封面");

    // musid read_cover：同一缓存目录、EmbeddedFirst。
    let cache_dir = lx_source::cover_cache::cache_dir();
    let musid_cover =
        read_cover(&audio, &cache_dir, CoverPriority::EmbeddedFirst).expect("musid 侧解析内嵌封面");

    assert_eq!(
        scanned_cover, musid_cover,
        "内嵌缓存键配方必须同源（sha256(音频路径字节 + 图片字节) → 同名文件）"
    );
    assert!(Path::new(&musid_cover).exists(), "缓存文件真实存在");
}

/// 2. 外挂封面检出：无内嵌标签 + 同目录 cover.png → get_cover_url 落
///    外挂文件路径（musid 与 voicefox 入口一致）。
#[tokio::test]
async fn external_cover_is_resolved_when_no_embedded_art() {
    let dir = FixtureDir::new("external");
    let audio = dir.write_wav("song.wav");
    let external = dir.0.join("cover.png");
    fs::write(&external, TINY_PNG).expect("外挂封面可写");

    let source = LocalSource::new();
    let song = local_song(&audio);
    let resolved = source.get_cover_url(&song).await.expect("外挂封面回落成功");
    assert_eq!(
        Path::new(&resolved),
        external,
        "外挂封面按检出路径原样返回（不复制不校验）"
    );

    // musid 直读同路径（对账）。
    assert_eq!(
        read_cover(
            &audio,
            &lx_source::cover_cache::cache_dir(),
            CoverPriority::EmbeddedFirst
        )
        .map(|path| path.to_string_lossy().to_string())
        .expect("musid 侧外挂检出"),
        resolved,
        "两侧解析产物一致"
    );
}

/// 3. 优先级不变：`song.cover_url` 在则原样返回（模型封面优先，
///    EmbeddedFirst 由 musid read_cover 承担内嵌 vs 外挂的次序）。
#[tokio::test]
async fn model_cover_url_still_wins_unchanged() {
    let dir = FixtureDir::new("precedence");
    let audio = dir.write_wav("song.wav");
    fs::write(dir.0.join("cover.png"), TINY_PNG).expect("外挂封面可写");

    let source = LocalSource::new();
    let mut song = local_song(&audio);
    song.cover_url = Some("https://cover.example/model.jpg".to_string());
    let resolved = source.get_cover_url(&song).await.expect("模型封面优先");
    assert_eq!(resolved, "https://cover.example/model.jpg");
}

/// 4. 双缺恒 NotFound（行为兼容，feature 下亦然）。
#[tokio::test]
async fn absent_everywhere_still_not_found() {
    let dir = FixtureDir::new("absent");
    let audio = dir.write_wav("bare.wav");

    let source = LocalSource::new();
    let song = local_song(&audio);
    assert!(matches!(
        source.get_cover_url(&song).await,
        Err(lx_core::traits::source::FetchError::NotFound)
    ));
}

/// 5. 内嵌优先于外挂（EmbeddedFirst）：同目录两者俱在时 musid read_cover
///    落内嵌缓存（voicefox 模型封面已由扫描期提取，此用例钉 musid 面
///    自身的次序语义，防回落路径误配 ExternalFirst）。
#[test]
fn embedded_first_priority_is_pinned_in_read_cover() {
    let dir = FixtureDir::new("embedded_first");
    let audio = dir.0.join("song.mp3");
    fs::write(&audio, mp3_fixture()).expect("MP3 fixture 可写");
    write_metadata(
        &audio,
        &MetadataEdit {
            title: None,
            artist: None,
            album: None,
            cover: Some(TINY_PNG.to_vec()),
            lyrics: None,
        },
    )
    .expect("内嵌封面可写");
    fs::write(dir.0.join("cover.png"), TINY_PNG).expect("外挂封面可写");

    let cache_dir = lx_source::cover_cache::cache_dir();
    let embedded =
        read_cover(&audio, &cache_dir, CoverPriority::EmbeddedFirst).expect("内嵌优先命中缓存");
    let external =
        read_cover(&audio, &cache_dir, CoverPriority::ExternalFirst).expect("外挂优先命中外挂文件");

    // 内嵌走解码校验缓存（cache_dir 下 {hash}.jpg），外挂按原路径返回
    // （不复制）——两条面产物必须可区分且各归各位。
    assert!(
        embedded.starts_with(&cache_dir),
        "内嵌封面落在缓存目录: {}",
        embedded.display()
    );
    assert_eq!(external, dir.0.join("cover.png"), "外挂封面按检出路径返回");
    assert_ne!(
        embedded.starts_with(&cache_dir),
        external.starts_with(&cache_dir),
        "内嵌（缓存）与外挂（原路径）产物可区分"
    );
}
