//! V1 双栈 parity 对账（feature `musid-dli-adapter`）。
//!
//! 对账是**纯诊断面**：内存计数、不落盘、只读快照供设置页/诊断展示；V1
//! 只计数不自动熔断（连续失败自动禁用 musid 是 V2 候选，避免 V1 行为不可
//! 预测）。归一规则复用 `crate::filter`（与 `SourceManager::find_music`
//! 同一套 find_music 归一），绝不按中文子串匹配错误。
//!
//! 计数口径（docs/MUSID_DLI_ADAPTER.md §11）：
//! - [`ParityOutcome::MusidOk`]：musid 路径成功给出产物；
//! - [`ParityOutcome::MusidErrLegacyOk`]：musid 失败（Err/空产出/过期），
//!   旧路径同请求成功（替换模式的回退归因）；
//! - [`ParityOutcome::MusidErrLegacyErr`]：两栈同请求都失败；
//! - [`ParityOutcome::ParityMatch`] / [`ParityOutcome::ParityMismatch`]：
//!   probe 影子模式的真双跑对照结论。
//!
//! 各面产出口径的取舍（如实登记，不是所有 outcome 在所有面都产生）：
//! - 搜索面：替换模式产 MusidOk/MusidErrLegacyOk/MusidErrLegacyErr；
//!   probe 模式真双跑，双 Ok 才对照（产 ParityMatch/Mismatch），单侧瞬时
//!   Err 不计（无双方产物可归一），双 Err 记 MusidErrLegacyErr。
//! - 流地址面：替换模式同搜索面；probe 模式**只做失败侧对账**——旧链
//!   成功不双跑 resolve（避免双倍平台配额），旧链失败才跑 musid（成功记
//!   MusidOk、失败记 MusidErrLegacyErr）。
//! - 歌词面：替换模式在 musid 槽位产 MusidOk，err 侧 legacy 归因只在
//!   `get_lyric_with_fallback` 主请求记录；probe 模式真双跑，双方都 Ok
//!   才对照（含"双方都确认无词 = ParityMatch"），单侧瞬时 Err 不计。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use lx_core::model::lyric::LyricData;
use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;

/// 对账面：四面中的三个运行时面。歌曲详情面无独立运行时开关（voicefox
/// fat-trait 无 by-id 详情方法、musid-dli 0.2.0 无公开 detail trait，详情
/// 由搜索/流地址承载面携带），不设枚举项（docs §11 如实登记）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ParityFace {
    /// 搜索面（单源 + 聚合）。
    Search,
    /// 流地址面。
    SongUrl,
    /// 歌词面。
    Lyric,
}

impl ParityFace {
    pub fn as_str(self) -> &'static str {
        match self {
            ParityFace::Search => "search",
            ParityFace::SongUrl => "song_url",
            ParityFace::Lyric => "lyric",
        }
    }
}

/// 计数口径（见模块文档）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ParityOutcome {
    MusidOk,
    MusidErrLegacyOk,
    MusidErrLegacyErr,
    ParityMatch,
    ParityMismatch,
}

impl ParityOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            ParityOutcome::MusidOk => "musid_ok",
            ParityOutcome::MusidErrLegacyOk => "musid_err_legacy_ok",
            ParityOutcome::MusidErrLegacyErr => "musid_err_legacy_err",
            ParityOutcome::ParityMatch => "parity_match",
            ParityOutcome::ParityMismatch => "parity_mismatch",
        }
    }
}

/// 单条计数快照（BTreeMap 序，供设置页/诊断稳定展示）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParityRecord {
    pub face: ParityFace,
    pub source: SourceId,
    pub outcome: ParityOutcome,
    pub count: u64,
}

/// 内存计数器（不落盘）。线程安全：聚合路径在 JoinSet 任务内记录，经
/// `Arc` 共享。
#[derive(Debug, Default)]
pub struct ParityCounters {
    counts: Mutex<HashMap<(ParityFace, SourceId, ParityOutcome), u64>>,
}

impl ParityCounters {
    pub fn record(&self, face: ParityFace, source: SourceId, outcome: ParityOutcome) {
        let mut counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        *counts.entry((face, source, outcome)).or_insert(0) += 1;
    }

    /// 单键读取（测试/诊断）。
    pub fn get(&self, face: ParityFace, source: SourceId, outcome: ParityOutcome) -> u64 {
        self.counts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(face, source, outcome))
            .copied()
            .unwrap_or(0)
    }

    /// 只读快照（face/source/outcome 按稳定字符串序，供设置页/诊断展示）。
    pub fn snapshot(&self) -> Vec<ParityRecord> {
        let counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        let mut records: Vec<ParityRecord> = counts
            .iter()
            .map(|((face, source, outcome), count)| ParityRecord {
                face: *face,
                source: *source,
                outcome: *outcome,
                count: *count,
            })
            .collect();
        records.sort_by(|a, b| {
            (a.face.as_str(), a.source.as_str(), a.outcome.as_str()).cmp(&(
                b.face.as_str(),
                b.source.as_str(),
                b.outcome.as_str(),
            ))
        });
        records
    }
}

// ---------------------------------------------------------------------------
// 归一对照规则
// ---------------------------------------------------------------------------

/// 归一文本：`filter_str`（去特殊字符 + 小写），与 find_music 同一套。
fn normalized(value: &str) -> String {
    crate::filter::filter_str(value)
}

/// 歌手归一：先 `sort_singer`（find_music 口径）再 `filter_str`。
fn normalized_singer(singer: &str) -> String {
    normalized(&crate::filter::sort_singer(singer))
}

/// 两首歌是否按 §11 归一规则等价：
/// - `id` 精确相等（平台原 ID，两侧都是同一平台的歌曲 id）；
/// - `name`/`singer`/`album` 经 filter_str(+sort_singer) 后相等；专辑任一侧
///   缺省（空串 = voicefox 的"未知"）视为缺失不比；
/// - 时长两侧都有值（非 ZERO）才比，差 ≤1s；
/// - `qualities` 集合相等（musid 侧产物在 `track_to_song` 已按 §2.4 折档，
///   Standard→Low128、Unknown 掉出，这里直接比折档后的集合）。
pub fn songs_equivalent(legacy: &SongInfo, musid: &SongInfo) -> bool {
    if legacy.id != musid.id {
        return false;
    }
    if normalized(&legacy.name) != normalized(&musid.name) {
        return false;
    }
    if normalized_singer(&legacy.singer) != normalized_singer(&musid.singer) {
        return false;
    }
    let legacy_album = legacy.album_name.trim();
    let musid_album = musid.album_name.trim();
    if !legacy_album.is_empty()
        && !musid_album.is_empty()
        && normalized(legacy_album) != normalized(musid_album)
    {
        return false;
    }
    if legacy.duration != Duration::ZERO && musid.duration != Duration::ZERO {
        let diff = (legacy.duration.as_millis() as i64 - musid.duration.as_millis() as i64).abs();
        if diff > 1000 {
            return false;
        }
    }
    legacy.qualities == musid.qualities
}

/// 搜索产物对照：条数一致且 musid 侧每条都能按 id 精确配对到 legacy 侧
/// 的一条等价歌曲。`total`/`has_more` 不对照（musid-dli `SearchPage.total:
/// None→items.len()` 的语义差如实豁免，docs §2.7）。
pub fn compare_search_items(legacy: &[SongInfo], musid: &[SongInfo]) -> bool {
    if legacy.len() != musid.len() {
        return false;
    }
    let legacy_by_id: HashMap<&str, &SongInfo> =
        legacy.iter().map(|song| (song.id.as_str(), song)).collect();
    musid.iter().all(|candidate| {
        matches!(
            legacy_by_id.get(candidate.id.as_str()),
            Some(song) if songs_equivalent(song, candidate)
        )
    })
}

/// 歌词 probe 的旧路径侧结论（评审 P4：oneshot 交给后台影子任务对账——
/// 影子不再阻塞用户路径）。
#[derive(Debug, Clone)]
pub enum LyricLegacyVerdict {
    /// 旧路径请求失败（瞬时错误；单侧失败不作对账结论）。
    Failed,
    /// 旧路径成功且确认无词。
    NoContent,
    /// 旧路径成功且有词（归一指纹，见 [`lyric_fingerprint`]）。
    Content((usize, Option<String>)),
}

/// 歌词归一指纹（去空白、行数、首行时间戳）：对 `lyric` 正文逐行去空白、
/// 去空行后取（行数，首行的 `[...]` 时间戳标签）。指纹相等才算对账命中；
/// 正文其余差异（翻译、逐字、行文本改写）V1 不比——musid-dli 无 YRC/QRC
/// 序列化器（lxlyric 恒 None），逐字对照无法成立。
pub fn lyric_fingerprint(data: &LyricData) -> (usize, Option<String>) {
    let lines: Vec<String> = data
        .lyric
        .lines()
        .map(|line| line.split_whitespace().collect::<String>())
        .filter(|line| !line.is_empty())
        .collect();
    let first_tag = lines
        .first()
        .and_then(|line| {
            line.strip_prefix('[')
                .and_then(|rest| rest.split_once(']'))
                .map(|(tag, _)| tag.to_string())
        })
        .or_else(|| lines.first().cloned());
    (lines.len(), first_tag)
}

/// 歌词对照：两侧指纹相等。
pub fn compare_lyrics(legacy: &LyricData, musid: &LyricData) -> bool {
    lyric_fingerprint(legacy) == lyric_fingerprint(musid)
}

/// 影子侧对照（评审 P4 后台对账用）：旧路径指纹（主路径算好后经 oneshot
/// 交接）vs musid 影子产物。
pub fn lyric_fingerprints_equivalent(legacy: &(usize, Option<String>), shadow: &LyricData) -> bool {
    lyric_fingerprint(shadow) == *legacy
}

#[cfg(test)]
mod tests {
    use super::*;
    use lx_core::model::source::Quality;

    fn song(id: &str, source: SourceId, name: &str, singer: &str) -> SongInfo {
        SongInfo::new(id.to_string(), source, name.to_string(), singer.to_string())
    }

    #[test]
    fn songs_equivalent_uses_find_music_normalization() {
        let mut legacy = song("100", SourceId::Wy, "晴天（Live）", "周杰伦、费玉清");
        legacy.album_name = "叶惠美".to_string();
        legacy.duration = Duration::from_secs(269);
        legacy.qualities = [Quality::High320, Quality::Flac].into_iter().collect();
        let mut musid = song("100", SourceId::Wy, "晴天 Live", "费玉清、周杰伦");
        musid.album_name = "叶 惠美".to_string();
        musid.duration = Duration::from_secs(270);
        musid.qualities = [Quality::Flac, Quality::High320].into_iter().collect();
        assert!(
            songs_equivalent(&legacy, &musid),
            "filter_str 去标点 + sort_singer 排序后应等价；时长差 1s 允许"
        );
    }

    #[test]
    fn songs_equivalent_rejects_id_or_field_or_duration_or_quality_gaps() {
        let mut legacy = song("100", SourceId::Wy, "晴天", "周杰伦");
        legacy.duration = Duration::from_secs(269);
        // id 精确：不同 id 直接不等。
        assert!(!songs_equivalent(
            &legacy,
            &song("101", SourceId::Wy, "晴天", "周杰伦")
        ));
        // 歌手不同（归一后不等）。
        assert!(!songs_equivalent(
            &legacy,
            &song("100", SourceId::Wy, "晴天", "费玉清")
        ));
        // 时长差 >1s（两侧都有值才比；缺省/ZERO 视为缺失不比）。
        let mut slow = song("100", SourceId::Wy, "晴天", "周杰伦");
        slow.duration = Duration::from_secs(300);
        assert!(!songs_equivalent(&legacy, &slow));
        let mut missing = song("100", SourceId::Wy, "晴天", "周杰伦");
        missing.duration = Duration::ZERO;
        assert!(songs_equivalent(&legacy, &missing), "缺省时长不比");
        // qualities 集合不等（adapter 产物已 §2.4 折档，直接比集合）。
        let mut more = song("100", SourceId::Wy, "晴天", "周杰伦");
        more.duration = Duration::from_secs(269);
        more.qualities = [Quality::Flac24].into_iter().collect();
        assert!(!songs_equivalent(&legacy, &more));
    }

    #[test]
    fn compare_search_items_requires_id_exact_and_count_match() {
        let legacy = vec![
            song("1", SourceId::Wy, "晴天", "周杰伦"),
            song("2", SourceId::Wy, "七里香", "周杰伦"),
        ];
        let musid = vec![
            song("2", SourceId::Wy, "七里香", "周杰伦"),
            song("1", SourceId::Wy, "晴天", "周杰伦"),
        ];
        assert!(
            compare_search_items(&legacy, &musid),
            "顺序无关，按 id 配对"
        );
        assert!(!compare_search_items(&legacy, &musid[..1]), "条数不一致");
        let mut other_id = musid.clone();
        other_id[1].id = "9".to_string();
        assert!(!compare_search_items(&legacy, &other_id), "id 集不一致");
        // total/has_more 不在对照面（SearchResult 层差异豁免）。
    }

    #[test]
    fn compare_lyrics_ignores_whitespace_and_counts_lines() {
        let legacy = LyricData {
            lyric: "[00:01.00]晴天\n[00:05.00]刮风这天".to_string(),
            ..LyricData::default()
        };
        let musid = LyricData {
            lyric: "[00:01.00] 晴天 \n[00:05.00]刮风这天\n".to_string(),
            ..LyricData::default()
        };
        assert!(compare_lyrics(&legacy, &musid), "去空白 + 首行时间戳一致");
        let shifted = LyricData {
            lyric: "[00:02.00]晴天\n[00:05.00]刮风这天".to_string(),
            ..LyricData::default()
        };
        assert!(!compare_lyrics(&legacy, &shifted), "首行时间戳不一致");
        let fewer = LyricData {
            lyric: "[00:01.00]晴天".to_string(),
            ..LyricData::default()
        };
        assert!(!compare_lyrics(&legacy, &fewer), "行数不一致");
    }

    #[test]
    fn counters_record_and_snapshot_in_order() {
        let counters = ParityCounters::default();
        counters.record(ParityFace::Search, SourceId::Wy, ParityOutcome::MusidOk);
        counters.record(ParityFace::Search, SourceId::Wy, ParityOutcome::MusidOk);
        counters.record(
            ParityFace::Lyric,
            SourceId::Kg,
            ParityOutcome::ParityMismatch,
        );
        assert_eq!(
            counters.get(ParityFace::Search, SourceId::Wy, ParityOutcome::MusidOk),
            2
        );
        assert_eq!(
            counters.get(ParityFace::Search, SourceId::Wy, ParityOutcome::ParityMatch),
            0,
            "未记录的键读 0"
        );
        let snapshot = counters.snapshot();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0].face, ParityFace::Lyric, "快照按 face 升序");
        assert_eq!(snapshot[0].outcome, ParityOutcome::ParityMismatch);
        assert_eq!(snapshot[1].outcome, ParityOutcome::MusidOk);
    }
}
