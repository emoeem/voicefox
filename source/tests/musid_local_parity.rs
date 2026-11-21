//! V3 切片 s1（docs/MUSID_DLI_ADAPTER.md §13.1）：本地库能力双栈对账。
//! musid-dli 的本地扫描核心（`local_scan`）与歌词读（`local::read_lyrics`）
//! 是 voicefox `source/src/local/` 的逐字移植（parity L1/L2/L4/L5/L8/L9/
//! L12/L13/L15/L17/L18/L21/L24，LOCAL_LIBRARY_PARITY.md §1/§5）——本文件在
//! 同一批运行期合成的语料（PCM WAV stub + CUE + 外挂歌词，无真实用户目录）
//! 上把两侧产物逐字段对比，钉住「迁移前后扫描结果、曲目数量、CUE 轨、
//! 标签、歌词一致」（master plan §13.5）。voicefox 的库状态/索引 schema/
//! Watcher 调度/UI 留在本仓（§13.3 SourceManager 边界）：
//! `LocalSource::read_local_lyric` 在本 feature 下委托 musid 实现，行为由
//! 歌词四用例 + 委托 pin 用例钉住；扫描/诊断面是对账消费（比较测试），
//! 不替换本仓实现。
//!
//! 已知键法差异（LOCAL_LIBRARY_PARITY.md §2，不做相等断言）：增量复用键
//! voicefox 用源文件路径（一个单 FILE sheet 的全部分轨共享音频路径键，
//! 组成 Vec），musid 用 `source_path`（CUE 轨为 CUE 路径）。本文件只在
//! 普通音频上对账复用记账，CUE 对账只比条目内容。
//!
//! 门禁与 CI：挂 `musid-dli-adapter-local` 子 feature 门，随 CI 的
//! 「V3 本地库对账」步进入 musid-stack job。

#![cfg(feature = "musid-dli-adapter-local")]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::time::Duration;

use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use lx_core::traits::source::MusicSource;
use lx_source::local::LocalSource;
use lx_source::local::scanner;
use musid_dli::local::read_lyrics;
use musid_dli::local_scan::{ScanOptions, ScanState, duplicate_groups, scan_entries};
use musid_dli::tag_write::{MetadataEdit, write_metadata};

// ---------------------------------------------------------------------------
// 语料合成（musid local_scan/local 测试同款配方）
// ---------------------------------------------------------------------------

/// 唯一临时目录；不主动清理（产物进系统临时区由系统回收），每个测试独立
/// 子目录 + 纳秒后缀避免互相污染。
struct FixtureDir(PathBuf);
impl FixtureDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "musid_local_parity_{}_{}_{}",
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

    fn write_wav(&self, name: &str, duration_ms: u64) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, wav_bytes(duration_ms)).expect("WAV stub 可写");
        path
    }
}

/// 最小 PCM WAV stub：44100 Hz、单声道、16-bit（musid `local_scan.rs` 测试
/// 同款字节配方，lofty 读出 {706 kb/s, 44100 Hz, 16-bit, lossless}）。
fn wav_bytes(duration_ms: u64) -> Vec<u8> {
    const SAMPLE_RATE: u32 = 44_100;
    const BYTES_PER_SECOND: u32 = SAMPLE_RATE * 2; // mono, 16-bit
    let data_len = (BYTES_PER_SECOND as u128 * duration_ms as u128 / 1000) as u32;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&BYTES_PER_SECOND.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.resize(wav.len() + data_len as usize, 0);
    wav
}

/// 两轨 sheet：第二轨 INDEX 01 在 2 s，5 s 音频下轨时长 2000/3000 ms
/// （musid `SCAN_SHEET` 同款）。
const SHEET: &str = "\
TITLE \"Scan Album\"\r\n\
PERFORMER \"Scan Artist\"\r\n\
FILE \"scan album.wav\" WAVE\r\n\
  TRACK 01 AUDIO\r\n\
    TITLE \"First Track\"\r\n\
    INDEX 01 00:00:00\r\n\
  TRACK 02 AUDIO\r\n\
    TITLE \"Second Track\"\r\n\
    INDEX 01 00:02:00\r\n\
";

/// 双侧同参数扫描：voicefox 全量 report + musid (entries, summary)。
fn scan_both(
    dir: &Path,
) -> (
    scanner::ScanReport,
    Vec<musid_dli::local_scan::ScanEntry>,
    musid_dli::local_scan::ScanSummary,
) {
    let report = scanner::scan_directory_report(dir, 0);
    let options = ScanOptions {
        max_depth: 0,
        force: true,
        ..ScanOptions::default()
    };
    let (entries, summary) = scan_entries(dir, &options, &ScanState::default());
    (report, entries, summary)
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// musid 侧 ScanState 直构（按 source_path 分组，等价 ScanState::absorb）。
fn fold_state(entries: &[musid_dli::local_scan::ScanEntry]) -> ScanState {
    let mut state = ScanState::default();
    for entry in entries {
        state
            .entries
            .entry(entry.source_path.clone())
            .or_default()
            .push(entry.clone());
    }
    state
}

// ---------------------------------------------------------------------------
// 1. 纯音频扫描：条目集合、标签回填、实测规格、指纹、统计
// ---------------------------------------------------------------------------

#[test]
fn plain_scan_metadata_specs_and_fingerprints_match() {
    let dir = FixtureDir::new("plain");
    let alpha = dir.write_wav("alpha.wav", 5000);
    dir.write_wav("beta.wav", 3000);
    write_metadata(
        &alpha,
        &MetadataEdit {
            title: Some("Alpha Song".into()),
            artist: Some("Alpha Artist".into()),
            album: Some("Alpha Album".into()),
            cover: None,
            lyrics: None,
        },
    )
    .expect("标签可写");

    let (report, entries, summary) = scan_both(dir.path());
    assert_eq!(report.songs.len(), 2, "voicefox 侧两首");
    assert_eq!(entries.len(), 2, "musid 侧两条");

    // 逐条对比：以播放路径为键配对。
    for local in &report.songs {
        let play = local.file_path.as_path();
        let entry = entries
            .iter()
            .find(|entry| entry.play_path == play)
            .unwrap_or_else(|| panic!("musid 侧缺 {}", play.display()));
        let song = &local.song;
        assert_eq!(
            song.name,
            entry.metadata.title.as_deref().unwrap_or(""),
            "标题回填（缺标签取文件名干）必须一致: {}",
            play.display()
        );
        assert_eq!(
            song.singer,
            entry.metadata.artist.as_deref().unwrap_or(""),
            "艺术家缺省回填必须一致"
        );
        assert_eq!(
            song.album_name,
            entry.metadata.album.as_deref().unwrap_or(""),
            "专辑缺省回填必须一致"
        );
        assert_eq!(
            song.duration,
            Duration::from_millis(entry.metadata.duration_ms),
            "时长必须一致"
        );
        let audio = song.audio.as_ref().expect("voicefox 实测规格必有");
        let spec = entry.metadata.audio.as_ref().expect("musid 实测规格必有");
        assert_eq!(audio.bitrate, spec.bitrate, "码率必须一致");
        assert_eq!(audio.sample_rate, spec.sample_rate, "采样率必须一致");
        assert_eq!(audio.bit_depth, spec.bit_depth, "位深必须一致");
        assert_eq!(audio.lossless, spec.lossless, "无损判定必须一致");
        assert_eq!(audio.sample_rate, Some(44_100), "WAV stub 采样率");
        assert_eq!(audio.bit_depth, Some(16), "WAV stub 位深");
        assert!(audio.lossless, "WAV 按容器判无损");
        // 指纹同配方（size + mtime nanos）。
        assert_eq!(
            report
                .fingerprints
                .get(&canonical(play))
                .expect("voicefox 指纹必有")
                .size,
            entry.fingerprint.size,
            "指纹 size 必须一致: {}",
            play.display()
        );
    }

    // 无标签文件的缺省回填（L15）：标题取文件干，艺术家/专辑取未知缺省。
    let beta_local = report
        .songs
        .iter()
        .find(|local| {
            local
                .file_path
                .file_stem()
                .is_some_and(|stem| stem == "beta")
        })
        .expect("beta 在 voicefox 侧");
    let beta_entry = entries
        .iter()
        .find(|entry| {
            entry
                .play_path
                .file_stem()
                .is_some_and(|stem| stem == "beta")
        })
        .expect("beta 在 musid 侧");
    assert_eq!(beta_local.song.name, "beta");
    assert_eq!(beta_entry.metadata.title.as_deref(), Some("beta"));
    assert_eq!(beta_local.song.singer, "未知艺术家");
    assert_eq!(beta_entry.metadata.artist.as_deref(), Some("未知艺术家"));

    // 统计对账：两文件全新解析，零复用。
    assert_eq!(report.parsed, 2);
    assert_eq!(report.reused, 0);
    assert_eq!(summary.stats.parsed, 2);
    assert_eq!(summary.stats.reused, 0);
    assert_eq!(summary.stats.discovered, 2);
    assert_eq!(summary.stats.failed, 0);
}

// ---------------------------------------------------------------------------
// 2. CUE 分轨：id/container_track、播放路径、extras、轨时长
// ---------------------------------------------------------------------------

#[test]
fn cue_sheet_tracks_match_field_by_field() {
    let dir = FixtureDir::new("cue");
    let audio = dir.write_wav("scan album.wav", 5000);
    let cue = dir.0.join("scan album.cue");
    fs::write(&cue, SHEET).expect("CUE 可写");

    let (report, entries, _) = scan_both(dir.path());
    // 引用的音频自身也保留整轨条目（两侧同口径，musid local_scan 测试
    // 「the referenced audio also keeps its own plain full-track entry」）：
    // 1 整轨 + 2 分轨 = 3。
    assert_eq!(report.songs.len(), 3, "voicefox 侧 1 整轨 + 2 分轨");
    assert_eq!(entries.len(), 3, "musid 侧 1 整轨 + 2 分轨");

    // 分轨筛选：voicefox 以 cue_file extra 识别，musid 以 container_track。
    let mut voicefox: Vec<_> = report
        .songs
        .iter()
        .filter(|local| local.song.extra.contains_key("cue_file"))
        .cloned()
        .collect();
    voicefox.sort_by(|left, right| left.song.id.cmp(&right.song.id));
    let mut musid: Vec<_> = entries
        .iter()
        .filter(|entry| entry.container_track.is_some())
        .cloned()
        .collect();
    musid.sort_by(|left, right| left.container_track.cmp(&right.container_track));
    assert_eq!(voicefox.len(), 2, "voicefox 分轨两条");
    assert_eq!(musid.len(), 2, "musid 分轨两条");

    // 整轨条目对账：普通音频条目 identity 一致（source_path = play_path =
    // 音频）。
    let plain_local = report
        .songs
        .iter()
        .find(|local| !local.song.extra.contains_key("cue_file"))
        .expect("整轨条目在 voicefox 侧");
    let plain_entry = entries
        .iter()
        .find(|entry| entry.container_track.is_none())
        .expect("整轨条目在 musid 侧");
    assert_eq!(plain_local.file_path, audio, "整轨播放路径");
    assert_eq!(plain_entry.play_path, audio);
    assert_eq!(plain_entry.source_path, audio, "普通音频 source 即播放");
    assert_eq!(
        plain_local.song.duration,
        Duration::from_millis(plain_entry.metadata.duration_ms),
        "整轨时长一致"
    );

    let expected = [("#01", 0_u64, 2000_u64), ("#02", 2000, 3000)];
    let cue_lossy = cue.to_string_lossy().to_string();
    for (index, (suffix, start_ms, duration_ms)) in expected.iter().enumerate() {
        let local = &voicefox[index];
        let song = &local.song;
        let entry = &musid[index];

        assert_eq!(
            song.id,
            format!("{cue_lossy}{suffix}"),
            "CUE 轨 id 形状 {cue_lossy}#NN 必须一致"
        );
        assert_eq!(
            entry.container_track.as_deref(),
            Some(format!("{cue_lossy}{suffix}").as_str()),
            "musid container_track 形状一致"
        );
        assert_eq!(
            canonical(&local.file_path),
            canonical(&cue),
            "LocalSong.file_path 是来源 CUE（复用键，scanner.rs:177-192）；分轨共用一键"
        );
        assert_eq!(
            song.file_path.as_ref(),
            Some(&audio),
            "播放路径都是被引用音频"
        );
        assert_eq!(&entry.play_path, &audio, "musid play_path 同播放路径");
        assert_eq!(
            canonical(&entry.source_path),
            canonical(&cue),
            "musid source_path 是 CUE 本身（与 voicefox 复用键同一身份）"
        );
        assert_eq!(
            song.extra.get("cue_file").map(String::as_str),
            Some(cue_lossy.as_str()),
            "cue_file extra 一致"
        );
        assert_eq!(
            entry.extra.get("cue_file").map(String::as_str),
            Some(cue_lossy.as_str())
        );
        let number = suffix.trim_start_matches('#');
        assert_eq!(
            song.extra.get("cue_track").map(String::as_str),
            Some(number)
        );
        assert_eq!(
            entry.extra.get("cue_track").map(String::as_str),
            Some(number)
        );
        assert_eq!(
            song.extra.get("cue_start_ms").map(String::as_str),
            Some(start_ms.to_string().as_str()),
            "cue_start_ms extra 一致"
        );
        assert_eq!(
            entry.extra.get("cue_start_ms").map(String::as_str),
            Some(start_ms.to_string().as_str())
        );
        assert_eq!(entry.start_ms, *start_ms, "INDEX 01 起点一致");
        assert_eq!(
            song.duration,
            Duration::from_millis(*duration_ms),
            "轨时长 = 下一 INDEX 01 起点 - 本轨起点"
        );
        assert_eq!(
            song.name,
            if index == 0 {
                "First Track"
            } else {
                "Second Track"
            },
            "CUE TITLE 落标题"
        );
        assert_eq!(song.singer, "Scan Artist", "PERFORMER 落艺术家");
        assert_eq!(song.album_name, "Scan Album", "sheet TITLE 落专辑");
    }
}

// ---------------------------------------------------------------------------
// 3. 增量复用：两侧复用记账一致（普通音频，键法无分歧）
// ---------------------------------------------------------------------------

#[test]
fn incremental_reuse_accounting_matches() {
    let dir = FixtureDir::new("reuse");
    dir.write_wav("one.wav", 4000);
    dir.write_wav("two.wav", 4000);

    // 第一遍：全解析。
    let (first, _, _) = scan_both(dir.path());
    assert_eq!(first.reused, 0, "首扫零复用");

    // voicefox 增量：previous 以来源文件为 key（mod.rs:218-223 同款分组）。
    let previous: HashMap<PathBuf, Vec<_>> = first
        .songs
        .iter()
        .map(|local| (local.file_path.clone(), vec![local.clone()]))
        .collect();
    let previous_fingerprints: HashMap<_, _> = first.fingerprints.clone();
    let second =
        scanner::scan_directory_incremental(dir.path(), 0, &previous, &previous_fingerprints);

    // musid 增量：ScanState 折叠。
    let options = ScanOptions {
        max_depth: 0,
        force: true,
        ..ScanOptions::default()
    };
    let (entries1, _) = scan_entries(dir.path(), &options, &ScanState::default());
    let state = fold_state(&entries1);
    let (entries2, summary2) = scan_entries(dir.path(), &options, &state);

    assert_eq!(second.reused, 2, "voicefox 第二遍全复用");
    assert_eq!(summary2.stats.reused, 2, "musid 第二遍全复用");
    assert!(
        entries2.iter().all(|entry| entry.reused),
        "musid 条目级复用位全真"
    );
    assert_eq!(entries1.len(), entries2.len(), "复用不丢条目");
}

// ---------------------------------------------------------------------------
// 4. 疑似重复分组：归一化键两侧一致
// ---------------------------------------------------------------------------

#[test]
fn duplicate_group_keys_match() {
    let dir = FixtureDir::new("dup");
    let a = dir.write_wav("copy_a.wav", 4000);
    let b = dir.write_wav("copy_b.wav", 4000);
    for path in [&a, &b] {
        write_metadata(
            path,
            &MetadataEdit {
                title: Some("Same Song".into()),
                artist: Some("Same Artist".into()),
                album: Some("Same Album".into()),
                cover: None,
                lyrics: None,
            },
        )
        .expect("标签可写");
    }

    let (_, entries, _) = scan_both(dir.path());
    let musid_groups = duplicate_groups(&entries);
    assert_eq!(musid_groups.len(), 1, "musid 侧一组");
    assert_eq!(musid_groups[0].entries.len(), 2);

    let local = LocalSource::new();
    local.scan(&[dir.path().to_string_lossy().to_string()], 0);
    let voicefox_groups = local.duplicate_groups();
    assert_eq!(voicefox_groups.len(), 1, "voicefox 侧一组");
    assert_eq!(voicefox_groups[0].songs.len(), 2);
    assert_eq!(
        voicefox_groups[0].key, musid_groups[0].key,
        "归一化键（空白折叠 + 小写 + \\u{{1f}} 连接 + 精确时长）必须逐字节一致"
    );
}

// ---------------------------------------------------------------------------
// 5. 失败台账：损坏文件不挡扫描，路径集合一致
// ---------------------------------------------------------------------------

#[test]
fn corrupt_file_lands_in_both_ledgers_without_blocking() {
    let dir = FixtureDir::new("corrupt");
    dir.write_wav("good.wav", 3000);
    let corrupt = dir.0.join("corrupt.wav");
    fs::write(&corrupt, b"not audio at all").expect("损坏文件可写");

    let (report, entries, summary) = scan_both(dir.path());
    assert_eq!(report.songs.len(), 1, "voicefox 侧好文件进歌单");
    assert_eq!(entries.len(), 1, "musid 侧好文件进条目");
    assert_eq!(report.failures.len(), 1, "voicefox 台账一条");
    assert_eq!(
        canonical(&report.failures[0].path),
        canonical(&corrupt),
        "失败路径是损坏文件"
    );
    assert_eq!(summary.failures.len(), 1, "musid 台账一条");
    assert_eq!(canonical(&summary.failures[0].path), canonical(&corrupt));
    assert_eq!(summary.stats.failed, 1);
    assert_eq!(summary.stats.parsed, 1, "损坏文件不挡好文件");
}

// ---------------------------------------------------------------------------
// 6. 歌词读四用例：外挂 UTF-8 / 外挂 GB18030 / 内嵌 / 双缺
// ---------------------------------------------------------------------------

#[test]
fn lyric_sidecar_utf8_wins_on_both_sides() {
    let dir = FixtureDir::new("lyric_utf8");
    let audio = dir.write_wav("song.wav", 3000);
    let lrc = dir.0.join("song.lrc");
    fs::write(&lrc, "[00:01.00]utf8 sidecar\r\n[00:02.00]second line").expect("歌词可写");

    assert_eq!(
        read_lyrics(&audio).as_deref(),
        Some("[00:01.00]utf8 sidecar\r\n[00:02.00]second line"),
        "musid 侧外挂优先"
    );
    assert_eq!(
        voicefox_get_lyric_text(&audio).as_deref(),
        read_lyrics(&audio).as_deref(),
        "委托后的 voicefox 读与 musid 同源同文"
    );
}

#[test]
fn lyric_sidecar_gb18030_fallback_matches() {
    let dir = FixtureDir::new("lyric_gb18030");
    let audio = dir.write_wav("song.wav", 3000);
    let lrc = dir.0.join("song.lrc");
    let (bytes, _, had_errors) = encoding_rs::GB18030.encode("[00:01.00]简体外挂歌词");
    assert!(!had_errors, "GB18030 可编码测试文本");
    fs::write(&lrc, &*bytes).expect("歌词可写");

    let musid_text = read_lyrics(&audio).expect("GB18030 降级后必有文本");
    assert!(musid_text.contains("简体外挂歌词"), "GB18030 解码正确");
    assert_eq!(
        voicefox_get_lyric_text(&audio).as_deref(),
        Some(musid_text.as_str())
    );
}

#[test]
fn lyric_embedded_fallback_matches_when_no_sidecar() {
    let dir = FixtureDir::new("lyric_embedded");
    let audio = dir.write_wav("song.wav", 3000);
    write_metadata(
        &audio,
        &MetadataEdit {
            title: None,
            artist: None,
            album: None,
            cover: None,
            lyrics: Some("[00:01.00]embedded text".into()),
        },
    )
    .expect("内嵌歌词可写");

    assert_eq!(
        read_lyrics(&audio).as_deref(),
        Some("[00:01.00]embedded text")
    );
    assert_eq!(
        voicefox_get_lyric_text(&audio).as_deref(),
        Some("[00:01.00]embedded text")
    );
}

#[test]
fn lyric_absent_on_both_sides_yields_none() {
    let dir = FixtureDir::new("lyric_absent");
    let audio = dir.write_wav("bare.wav", 3000);
    assert_eq!(read_lyrics(&audio), None, "musid 侧双缺为 None");
    assert_eq!(
        voicefox_get_lyric_text(&audio).unwrap_or_default(),
        "",
        "voicefox 侧空歌词"
    );
}

/// voicefox 侧本地歌词读的入口就是 `LocalSource::get_lyric`（内部在
/// spawn_blocking 里调 `read_local_lyric`——本 feature 下即 musid 委托）。
#[tokio::test]
async fn voicefox_local_get_lyric_equals_musid_read_lyrics() {
    let dir = FixtureDir::new("lyric_delegation");
    let audio = dir.write_wav("song.wav", 3000);
    let lrc = dir.0.join("song.lrc");
    fs::write(&lrc, "[00:01.00]delegated").expect("歌词可写");

    let mut song = SongInfo::new(
        audio.to_string_lossy().to_string(),
        SourceId::Local,
        "song".into(),
        String::new(),
    );
    song.file_path = Some(audio.clone());

    let source = LocalSource::new();
    let data = source.get_lyric(&song).await.expect("本地歌词读不报错");
    assert_eq!(data.lyric, read_lyrics(&audio).unwrap_or_default());
}

#[tokio::test]
async fn voicefox_local_get_lyric_empty_for_bare_file() {
    let dir = FixtureDir::new("lyric_delegation_bare");
    let audio = dir.write_wav("bare.wav", 3000);

    let mut song = SongInfo::new(
        audio.to_string_lossy().to_string(),
        SourceId::Local,
        "bare".into(),
        String::new(),
    );
    song.file_path = Some(audio.clone());

    let source = LocalSource::new();
    let data = source.get_lyric(&song).await.expect("双缺不报错");
    assert!(data.lyric.is_empty(), "双缺歌词为空串");
}

/// 走 `LocalSource::get_lyric` 的便捷封装（测试用）：每次起独立
/// current_thread runtime，避免 tokio::test 与同步测试混排的问题。
fn voicefox_get_lyric_text(audio: &Path) -> Option<String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("测试 runtime 可建");
    rt.block_on(async {
        let mut song = SongInfo::new(
            audio.to_string_lossy().to_string(),
            SourceId::Local,
            "song".into(),
            String::new(),
        );
        song.file_path = Some(audio.to_path_buf());
        let source = LocalSource::new();
        let data = source.get_lyric(&song).await.expect("本地歌词读不报错");
        (!data.lyric.is_empty()).then_some(data.lyric)
    })
}
