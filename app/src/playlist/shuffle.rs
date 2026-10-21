//! 随机播放调度器
//!
//! 每首歌在播放时抽取 `key = lastPlayed + U(0, α·n)`，下一首取冷却期外 key 最小者。
//! 前方窗口生成后固定，只因点歌、插队与列表增删而改变。插队栈按出栈顺序排在窗口之前，
//! 同样参与冷却期计算。
//!
//! `lastPlayed` 与 jitter 按歌曲身份全局保存：歌曲离开列表期间时钟对其暂停，再次进入时
//! 延续离开时的进度。列表规模变化时 jitter 按比例缩放，距上次播放的"年龄"只缩小不放大。
//! 平移后的 `lastPlayed` 只决定优先级，冷却期按真实播放的 tick 判断。
//!
//! 逻辑时钟 `tick` 在任何播放模式下都随播放推进，窗口与插队栈只在随机模式下存在。
//! 播放历史同样只属于随机模式：非随机模式下历史只含当前曲，切回随机模式后从当前曲重新累积。

use std::borrow::Cow;
use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use lx_core::model::song::SongInfo;
use lx_core::model::source::SourceId;
use lx_source::manager::{builtin_source_for_js, is_local_file_song};
use rand::Rng;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// jitter 的幅度系数
const ALPHA: f64 = 1.0;
/// 前方窗口长度
const WINDOW: usize = 20;
/// 不在当前列表中的歌最多保留的条目数
pub const ABSENT_CAP: usize = 5000;
/// 播放历史的最大长度，即"上一首"最多能回退的深度
const HISTORY_CAP: usize = 200;
const STATE_VERSION: u32 = 1;

/// 歌曲身份。
///
/// 有文件路径的本地歌曲按路径区分，CUE 分轨另按轨号区分。其余按平台与 id 区分，
/// id 为空时退回歌名与歌手。
///
/// JS 音源的搜索结果同样标记为 `SourceId::Local`，真实平台记在 `extra["source"]`。
/// 平台对应内置音源时按该音源计，与内置音源搜到的同一首歌视为同一首，否则按平台标记区分。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SongKey(Arc<str>);

impl SongKey {
    pub fn of(song: &SongInfo) -> Self {
        let local_file = is_local_file_song(song);
        let key = match &song.file_path {
            Some(path) if local_file => match song.extra.get("cue_track") {
                // CUE 各轨共用同一个音频文件，按轨号区分
                Some(track) => format!("local\0{}\0{track}", path.to_string_lossy()),
                None => format!("local\0{}", path.to_string_lossy()),
            },
            _ => {
                // 标记为 Local 却不是本地文件的即 JS 音源结果，真实平台记在 extra["source"]
                let platform = if song.source == SourceId::Local && !local_file {
                    builtin_source_for_js(song).map_or_else(
                        || {
                            let tag = song.extra.get("source").map_or("", String::as_str);
                            Cow::Owned(format!("js:{}", tag.to_ascii_lowercase()))
                        },
                        |source| Cow::Borrowed(source.as_str()),
                    )
                } else {
                    Cow::Borrowed(song.source.as_str())
                };
                if song.id.is_empty() {
                    format!("{platform}\0\0{}\0{}", song.name, song.singer)
                } else {
                    format!("{platform}\0{}", song.id)
                }
            }
        };
        Self(key.into())
    }
}

impl Serialize for SongKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SongKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(|key| Self(key.into()))
    }
}

/// `next()` 选出下一首的方式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Advance {
    /// 插队栈栈顶
    Insert,
    /// 回退后沿历史前进
    Replay,
    /// 窗口队首
    Natural,
}

struct Song {
    /// 选歌优先级所用的 lastPlayed：最近一次提交时的 tick，离开列表或列表规模变化后会按比例平移，
    /// 因此为浮点数
    last: f64,
    /// 最近一次提交的真实 tick，从未播放过的歌为 None
    played: Option<u64>,
    /// 已提交状态的 jitter，key = last + jit
    jit: f64,
    /// 抽取 jit 时的列表规模
    cn: usize,
    /// 冷却期判断所用的播放时刻，窗口或插队栈中的歌为其最后一次规划位置
    plan_last: f64,
    /// 堆中有效条目的版本号，None 表示不参与选歌
    live: Option<u64>,
    /// 离开列表时的 tick 与列表规模
    left: Option<(u64, usize)>,
}

impl Song {
    /// 冷却期判断所用的已提交播放时刻，从未播放过的歌不受冷却期约束
    fn played_at(&self) -> f64 {
        self.played.map_or(f64::NEG_INFINITY, |t| t as f64)
    }

    fn rescale(&mut self, n: usize) {
        self.jit *= n as f64 / self.cn as f64;
        self.cn = n;
    }
}

/// 堆条目。同一首歌只有版本号与 `Song::live` 相同的条目有效，其余在出堆时丢弃。
#[derive(PartialEq)]
struct Candidate {
    key: f64,
    ver: u64,
    song: SongKey,
}

impl Eq for Candidate {}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key
            .total_cmp(&other.key)
            .then(self.ver.cmp(&other.ver))
            .then_with(|| self.song.cmp(&other.song))
    }
}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// 列表规模从 `n_old` 变为 `n` 时歌曲年龄的缩放系数，只缩小不放大
fn age_factor(n: usize, n_old: usize) -> f64 {
    (n as f64 / n_old.max(1) as f64).min(1.0)
}

pub struct ShuffleScheduler {
    rng: StdRng,
    /// 逻辑时钟，每提交一首播放加一
    tick: u64,
    /// 随机模式是否激活
    random: bool,
    /// 当前列表中的歌
    ctx: HashSet<SongKey>,
    /// 全局状态，包括已离开列表的歌
    songs: HashMap<SongKey, Song>,
    heap: BinaryHeap<Reverse<Candidate>>,
    next_ver: u64,
    /// 前方窗口及各条目规划时抽取的 jitter
    window: Vec<(SongKey, f64)>,
    /// 窗口最后一格的规划 tick
    plan_tick: u64,
    /// 插队栈，栈顶在末尾
    stack: Vec<SongKey>,
    /// 插队栈中各歌曲入栈时抽取的 jitter
    stack_jit: HashMap<SongKey, f64>,
    /// 播放历史，非随机模式下只含当前曲
    history: VecDeque<SongKey>,
    /// 当前曲在 history 中的位置
    hpos: Option<usize>,
    /// 每次可能改变持久化内容的操作加一
    revision: u64,
}

impl ShuffleScheduler {
    pub fn new(rng: StdRng, random: bool) -> Self {
        Self {
            rng,
            tick: 0,
            random,
            ctx: HashSet::new(),
            songs: HashMap::new(),
            heap: BinaryHeap::new(),
            next_ver: 0,
            window: Vec::new(),
            plan_tick: 0,
            stack: Vec::new(),
            stack_jit: HashMap::new(),
            history: VecDeque::new(),
            hpos: None,
            revision: 0,
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn n(&self) -> usize {
        self.ctx.len()
    }

    /// 冷却期 `max(min(2, n), ⌊n/4⌋)`，n ≥ 2 时同一首不会紧接着重播
    fn cooldown(&self) -> usize {
        let n = self.n();
        n.min(2).max(n / 4).max(1)
    }

    fn draw_jitter(&mut self) -> f64 {
        self.rng.r#gen::<f64>() * ALPHA * self.n().max(1) as f64
    }

    fn push(&mut self, song: &SongKey, base: f64, jit: f64) {
        self.next_ver += 1;
        let ver = self.next_ver;
        if let Some(state) = self.songs.get_mut(song) {
            state.live = Some(ver);
        }
        self.heap.push(Reverse(Candidate {
            key: base + jit,
            ver,
            song: song.clone(),
        }));
        // 失效条目只在出堆时丢弃，堆明显大于歌曲数时整体清理一次
        if self.heap.len() > 2 * self.songs.len() + 64 {
            let songs = &self.songs;
            self.heap.retain(|Reverse(c)| Self::is_live(songs, c));
        }
    }

    fn is_live(songs: &HashMap<SongKey, Song>, candidate: &Candidate) -> bool {
        songs
            .get(&candidate.song)
            .is_some_and(|state| state.live == Some(candidate.ver))
    }

    fn withdraw(&mut self, song: &SongKey) {
        if let Some(state) = self.songs.get_mut(song) {
            state.live = None;
        }
    }

    /// 恢复为已提交状态，不重抽 jitter
    fn restore(&mut self, song: &SongKey) {
        let Some(state) = self.songs.get_mut(song) else {
            return;
        };
        state.plan_last = state.played_at();
        let (last, jit) = (state.last, state.jit);
        self.push(song, last, jit);
    }

    /// 已加入 ctx 的歌按当前 n 登记状态
    fn enter(&mut self, song: &SongKey) {
        let n = self.n().max(1);
        let tick = self.tick as f64;
        if let Some(state) = self.songs.get_mut(song) {
            // 缺席期间时钟暂停，离开时的年龄按规模缩放后延续
            if let Some((left, ln)) = state.left.take() {
                let age = (left as f64 - state.last) * age_factor(n, ln);
                state.last = tick - age;
            }
            state.rescale(n);
        } else {
            // 新歌没有真实播放记录，不受冷却期约束。虚拟 lastPlayed 只决定优先级，取在冷却期与 n 之间
            let cooldown = self.cooldown();
            let spread = self.n().saturating_sub(cooldown) as f64;
            let last = tick - cooldown as f64 - self.rng.r#gen::<f64>() * spread;
            let jit = self.draw_jitter();
            self.songs.insert(
                song.clone(),
                Song {
                    last,
                    played: None,
                    jit,
                    cn: n,
                    plan_last: f64::NEG_INFINITY,
                    live: None,
                    left: None,
                },
            );
        }
        self.restore(song);
    }

    /// 已移出 ctx 的歌：从未播放过的歌直接遗忘，其余记录离开时刻
    fn leave(&mut self, song: &SongKey, n: usize) {
        let tick = self.tick;
        match self.songs.get_mut(song) {
            Some(state) if state.played.is_some() => state.left = Some((tick, n.max(1))),
            _ => {
                self.songs.remove(song);
            }
        }
    }

    fn refill(&mut self) {
        while self.random && self.window.len() < WINDOW {
            let vt = self.plan_tick + 1;
            let cooldown = self.cooldown() as f64;
            let mut held = Vec::new();
            let mut pick = None;
            while let Some(Reverse(candidate)) = self.heap.pop() {
                if !Self::is_live(&self.songs, &candidate) {
                    continue;
                }
                if vt as f64 - self.songs[&candidate.song].plan_last >= cooldown {
                    pick = Some(candidate.song);
                    break;
                }
                held.push(candidate);
            }
            let pick = match pick {
                Some(song) => song,
                // held 按出堆顺序排列，首项 key 最小
                None if !held.is_empty() => {
                    tracing::debug!("shuffle cooldown relaxed");
                    held.remove(0).song
                }
                None => break,
            };
            for candidate in held {
                self.heap.push(Reverse(candidate));
            }
            let jit = self.draw_jitter();
            if let Some(state) = self.songs.get_mut(&pick) {
                state.plan_last = vt as f64;
            }
            self.push(&pick, vt as f64, jit);
            self.window.push((pick, jit));
            self.plan_tick = vt;
        }
    }

    fn commit(&mut self, song: &SongKey, jit: f64) {
        self.tick += 1;
        let n = self.n().max(1);
        if let Some(state) = self.songs.get_mut(song) {
            state.last = self.tick as f64;
            state.played = Some(self.tick);
            state.jit = jit;
            state.cn = n;
        }
        if !self.random {
            self.history.clear();
        }
        self.history.push_back(song.clone());
        if self.history.len() > HISTORY_CAP {
            self.history.pop_front();
        }
        self.hpos = Some(self.history.len() - 1);
    }

    /// 播放历史只保留当前曲
    fn truncate_history(&mut self) {
        let current = self.current().cloned();
        self.history.clear();
        self.history.extend(current);
        self.hpos = self.history.len().checked_sub(1);
    }

    /// 清空窗口，其中的歌按原 jitter 恢复为已提交状态，key 保持不变
    fn rollback_window(&mut self) {
        let window = std::mem::take(&mut self.window);
        let mut seen = HashSet::new();
        for (song, _) in window {
            if seen.insert(song.clone()) && self.ctx.contains(&song) {
                self.restore(&song);
            }
        }
    }

    /// 按"插队栈（出栈顺序）+ 窗口"重新登记每首歌的预计播放 tick。
    ///
    /// 窗口从第一个违反冷却期的位置截断后补满尾部，截断点之前原样保留。
    /// 插队栈中的歌不受冷却期约束，但会占位并约束窗口。
    fn replan(&mut self) {
        let cooldown = self.cooldown() as f64;
        let mut seen: HashMap<SongKey, (u64, f64)> = HashMap::new();
        let mut vt = self.tick;
        for song in self.stack.iter().rev() {
            if self.ctx.contains(song) {
                vt += 1;
                let jit = self.stack_jit.get(song).copied().unwrap_or(0.0);
                seen.insert(song.clone(), (vt, jit));
            }
        }
        let mut keep = Vec::new();
        for (song, jit) in &self.window {
            let prev = seen
                .get(song)
                .map_or(self.songs[song].played_at(), |&(t, _)| t as f64);
            if (vt + 1) as f64 - prev < cooldown {
                break;
            }
            vt += 1;
            keep.push((song.clone(), *jit));
            seen.insert(song.clone(), (vt, *jit));
        }
        let mut touched = HashSet::new();
        let affected: Vec<SongKey> = self
            .window
            .iter()
            .map(|(song, _)| song)
            .chain(&self.stack)
            .filter(|song| touched.insert((*song).clone()))
            .cloned()
            .collect();
        for song in &affected {
            if !self.ctx.contains(song) {
                continue;
            }
            match seen.get(song) {
                Some(&(t, jit)) => {
                    if let Some(state) = self.songs.get_mut(song) {
                        state.plan_last = t as f64;
                    }
                    self.push(song, t as f64, jit);
                }
                None => self.restore(song),
            }
        }
        self.window = keep;
        self.plan_tick = vt;
        self.refill();
    }

    /// 打乱窗口前 min(K, 冷却期) 项的顺序。
    ///
    /// 这一段内没有重复的歌，但每项仍受自身上一次播放的冷却期约束。均匀置换违反约束时
    /// 逐位重排：第 i 位从允许放在 i 位的剩余项中随机选取。原顺序合法，因此每一位都有可选项。
    fn shuffle_head(&mut self) {
        let cooldown = self.cooldown();
        let m = self.window.len().min(cooldown);
        let mut head = self.window[..m].to_vec();
        head.shuffle(&mut self.rng);
        let mut base = self.tick;
        let mut planned = HashMap::new();
        for song in self.stack.iter().rev() {
            if self.ctx.contains(song) {
                base += 1;
                planned.insert(song, base);
            }
        }
        let songs = &self.songs;
        // 第 i 位对应 tick base+1+i
        let earliest = |song: &SongKey| {
            planned
                .get(song)
                .map_or(songs[song].played_at(), |&t| t as f64)
                + cooldown as f64
                - base as f64
                - 1.0
        };
        if head
            .iter()
            .enumerate()
            .any(|(i, (song, _))| (i as f64) < earliest(song))
        {
            let mut pool = head;
            head = Vec::with_capacity(m);
            for i in 0..m {
                let allowed: Vec<usize> = pool
                    .iter()
                    .enumerate()
                    .filter(|(_, (song, _))| earliest(song) <= i as f64)
                    .map(|(k, _)| k)
                    .collect();
                let k = allowed.choose(&mut self.rng).copied().unwrap_or(0);
                head.push(pool.remove(k));
            }
        }
        self.window.splice(..m, head);
    }

    /// 选出下一首并提交。列表为空或不在随机模式时返回 None。
    pub fn next(&mut self) -> Option<(SongKey, Advance)> {
        self.revision += 1;
        if let Some(song) = self.stack.pop() {
            let jit = self.stack_jit.remove(&song).unwrap_or(0.0);
            self.commit(&song, jit);
            self.refill();
            return Some((song, Advance::Insert));
        }
        let start = self.hpos.map_or(0, |pos| pos + 1);
        if let Some(i) = (start..self.history.len()).find(|&i| self.ctx.contains(&self.history[i]))
        {
            self.hpos = Some(i);
            return Some((self.history[i].clone(), Advance::Replay));
        }
        self.hpos = self.history.len().checked_sub(1);
        self.refill();
        if self.window.is_empty() {
            return None;
        }
        let (song, jit) = self.window.remove(0);
        self.commit(&song, jit);
        self.refill();
        Some((song, Advance::Natural))
    }

    /// 沿播放历史回退一首，跳过已不在列表中的歌。
    ///
    /// 已在历史起点时重播起点那首，游标不动。当前曲也已不在列表中时返回 None。
    pub fn prev(&mut self) -> Option<SongKey> {
        let pos = self.hpos?;
        let Some(i) = (0..pos)
            .rev()
            .find(|&i| self.ctx.contains(&self.history[i]))
        else {
            let current = &self.history[pos];
            return self.ctx.contains(current).then(|| current.clone());
        };
        self.hpos = Some(i);
        self.revision += 1;
        Some(self.history[i].clone())
    }

    /// 插队：下一次 `next()` 播放该歌，连续插队按后进先出播放
    pub fn insert(&mut self, song: &SongKey) {
        if !self.random || !self.ctx.contains(song) {
            return;
        }
        self.revision += 1;
        self.stack.retain(|s| s != song);
        self.stack.push(song.clone());
        self.window.retain(|(s, _)| s != song);
        let jit = self.draw_jitter();
        self.stack_jit.insert(song.clone(), jit);
        self.replan();
    }

    /// 撤销对该歌的插队，该歌不在插队栈中时无操作
    pub fn cancel_insert(&mut self, song: &SongKey) {
        if !self.stack.contains(song) {
            return;
        }
        self.revision += 1;
        self.stack.retain(|s| s != song);
        self.stack_jit.remove(song);
        self.restore(song);
        self.replan();
    }

    /// 点歌：提交该歌并以它为起点重新生成窗口
    pub fn jump(&mut self, song: &SongKey) {
        if !self.random || !self.ctx.contains(song) {
            return;
        }
        self.revision += 1;
        self.rollback_window();
        self.stack.retain(|s| s != song);
        self.stack_jit.remove(song);
        let jit = self.draw_jitter();
        self.commit(song, jit);
        let tick = self.tick as f64;
        if let Some(state) = self.songs.get_mut(song) {
            state.plan_last = tick;
        }
        self.push(song, tick, jit);
        self.replan();
        self.shuffle_head();
        self.replan();
    }

    /// 非随机模式下播放了该歌：推进时钟并登记，使切回随机模式时它不被视作久未播放。
    /// 播放历史只记为当前曲
    pub fn observe(&mut self, song: &SongKey) {
        if self.random || !self.ctx.contains(song) {
            return;
        }
        self.revision += 1;
        let jit = self.draw_jitter();
        self.commit(song, jit);
        let tick = self.tick as f64;
        if let Some(state) = self.songs.get_mut(song) {
            state.plan_last = tick;
        }
        self.push(song, tick, jit);
    }

    pub fn add(&mut self, song: SongKey) {
        if self.attach(song) {
            self.revision += 1;
            // n 变大可能使冷却期变长
            self.replan();
        }
    }

    pub fn remove(&mut self, song: &SongKey) {
        if self.detach(song) {
            self.revision += 1;
            self.replan();
        }
    }

    /// 把歌加入列表并登记状态，不重新规划。返回列表是否变化
    fn attach(&mut self, song: SongKey) -> bool {
        if !self.ctx.insert(song.clone()) {
            return false;
        }
        self.enter(&song);
        true
    }

    /// 把歌移出列表、窗口与插队栈，不重新规划。返回列表是否变化
    fn detach(&mut self, song: &SongKey) -> bool {
        if !self.ctx.remove(song) {
            return false;
        }
        self.withdraw(song);
        self.leave(song, self.n());
        self.window.retain(|(s, _)| s != song);
        self.stack.retain(|s| s != song);
        self.stack_jit.remove(song);
        true
    }

    /// 替换列表，然后从 `start` 开始播放：随机模式下为点歌，否则为登记。
    ///
    /// 歌曲集合变化时插队栈随旧列表一起丢弃，集合不变时只处理 `start`。
    /// 历史保留，回退时跳过不在新列表中的歌。
    pub fn set_list(&mut self, keys: impl IntoIterator<Item = SongKey>, start: Option<&SongKey>) {
        let mut order = Vec::new();
        let mut new = HashSet::new();
        for key in keys {
            if new.insert(key.clone()) {
                order.push(key);
            }
        }
        let changed = new != self.ctx;
        if changed {
            self.revision += 1;
            let n_old = self.n();
            self.rollback_window();
            self.stack.clear();
            self.stack_jit.clear();
            let leaving: Vec<SongKey> = self.ctx.difference(&new).cloned().collect();
            for song in &leaving {
                self.withdraw(song);
                self.leave(song, n_old);
            }
            let added: HashSet<SongKey> = new.difference(&self.ctx).cloned().collect();
            self.ctx = new;
            let n = self.n().max(1);
            let factor = age_factor(n, n_old);
            let tick = self.tick as f64;
            for song in &order {
                if added.contains(song) {
                    self.enter(song);
                    continue;
                }
                // 留下的歌同样按新的 n 缩放年龄与 jitter
                if let Some(state) = self.songs.get_mut(song) {
                    state.last = tick - (tick - state.last) * factor;
                    state.rescale(n);
                }
                self.restore(song);
            }
        }
        match start.filter(|song| self.ctx.contains(*song)) {
            Some(song) if self.random => self.jump(song),
            Some(song) => self.observe(song),
            None if changed && self.random => self.replan(),
            None => {}
        }
    }

    /// 当前曲 `from` 在开始播放前被替换为 `to`（如 B 站视频展开为分 P）。
    ///
    /// 列表按差集逐首增删，插队栈保留。`to` 接替 `from` 已登记的这次播放，时钟不推进。
    /// `from` 不是当前曲时只同步列表。
    pub fn replace_current(
        &mut self,
        keys: impl IntoIterator<Item = SongKey>,
        from: &SongKey,
        to: &SongKey,
    ) {
        let mut order = Vec::new();
        let mut new = HashSet::new();
        for key in keys {
            if new.insert(key.clone()) {
                order.push(key);
            }
        }
        let leaving: Vec<SongKey> = self.ctx.difference(&new).cloned().collect();
        let mut changed = false;
        for song in &leaving {
            changed |= self.detach(song);
        }
        for song in order {
            changed |= self.attach(song);
        }
        if self.hand_over(from, to) || changed {
            self.revision += 1;
            self.replan();
        }
    }

    /// 当前曲为 `from` 时由 `to` 接替其已登记的播放，返回是否接替
    fn hand_over(&mut self, from: &SongKey, to: &SongKey) -> bool {
        let Some(pos) = self.hpos else {
            return false;
        };
        if from == to || &self.history[pos] != from || !self.ctx.contains(to) {
            return false;
        }
        let Some(prev) = self.songs.get(from) else {
            return false;
        };
        let (last, played, jit, cn) = (prev.last, prev.played, prev.jit, prev.cn);
        let n = self.n().max(1);
        if let Some(state) = self.songs.get_mut(to) {
            if last > state.last {
                (state.last, state.jit, state.cn) = (last, jit, cn);
                state.rescale(n);
            }
            state.played = state.played.max(played);
        }
        self.history[pos] = to.clone();
        self.stack.retain(|s| s != to);
        self.stack_jit.remove(to);
        self.restore(to);
        true
    }

    /// 切换随机模式。切入时按当前 key 生成窗口，不提交当前曲。
    /// 切出时丢弃窗口与插队栈，其中的歌恢复为已提交状态，播放历史只保留当前曲。当前曲不变。
    pub fn set_random(&mut self, on: bool) {
        if on == self.random {
            return;
        }
        self.revision += 1;
        self.random = on;
        if on {
            self.replan();
        } else {
            self.rollback_window();
            // 插队栈中的歌不一定在窗口里，同样带着插队时登记的预计 tick
            for song in std::mem::take(&mut self.stack) {
                if self.ctx.contains(&song) {
                    self.restore(&song);
                }
            }
            self.stack_jit.clear();
            self.truncate_history();
        }
    }

    /// 不在当前列表中的歌最多保留 `cap` 首，按离开时刻从旧到新遗忘
    pub fn prune(&mut self, cap: usize) {
        let mut absent: Vec<(f64, f64, SongKey)> = self
            .songs
            .iter()
            .filter(|(song, _)| !self.ctx.contains(*song))
            .map(|(song, state)| {
                let left = state.left.map_or(state.last, |(t, _)| t as f64);
                (left, state.last, song.clone())
            })
            .collect();
        if absent.len() <= cap {
            return;
        }
        self.revision += 1;
        absent.sort_by(|a, b| {
            a.0.total_cmp(&b.0)
                .then(a.1.total_cmp(&b.1))
                .then_with(|| a.2.cmp(&b.2))
        });
        let excess = absent.len() - cap;
        for (_, _, song) in absent.into_iter().take(excess) {
            self.songs.remove(&song);
        }
    }

    pub fn state(&self) -> ShuffleState {
        let mut ctx: Vec<SongKey> = self.ctx.iter().cloned().collect();
        ctx.sort();
        ShuffleState {
            version: STATE_VERSION,
            tick: self.tick,
            random: self.random,
            songs: self
                .songs
                .iter()
                .map(|(song, state)| {
                    (
                        song.clone(),
                        SavedSong {
                            last: state.last,
                            played: state.played,
                            jit: state.jit,
                            cn: state.cn,
                            left: state.left.map(|(t, _)| t),
                            ln: state.left.map(|(_, n)| n),
                        },
                    )
                })
                .collect(),
            ctx,
            window: self.window.clone(),
            stack: self
                .stack
                .iter()
                .map(|song| {
                    (
                        song.clone(),
                        self.stack_jit.get(song).copied().unwrap_or(0.0),
                    )
                })
                .collect(),
            history: self.history.iter().cloned().collect(),
            hpos: self.hpos,
        }
    }

    /// 从持久化状态恢复。版本不符时返回 None。
    ///
    /// 列表中的歌全部恢复为已提交状态后执行一次 replan。保存时的窗口满足冷却期约束，
    /// replan 会原样保留它。
    pub fn from_state(state: ShuffleState, rng: StdRng) -> Option<Self> {
        if state.version != STATE_VERSION {
            return None;
        }
        let mut scheduler = Self::new(rng, state.random);
        scheduler.tick = state.tick;
        scheduler.ctx = state
            .ctx
            .into_iter()
            .filter(|song| state.songs.contains_key(song))
            .collect();
        for (song, saved) in state.songs {
            let present = scheduler.ctx.contains(&song);
            let left = match (saved.left, present) {
                (_, true) => None,
                (Some(t), false) if saved.played.is_some() => {
                    Some((t, saved.ln.unwrap_or(1).max(1)))
                }
                // 缺席却没有离开记录或从未播放的条目不符合不变式，丢弃
                _ => continue,
            };
            scheduler.songs.insert(
                song,
                Song {
                    last: saved.last,
                    played: saved.played,
                    jit: saved.jit,
                    cn: saved.cn.max(1),
                    plan_last: f64::NEG_INFINITY,
                    live: None,
                    left,
                },
            );
        }
        let present: Vec<SongKey> = scheduler.ctx.iter().cloned().collect();
        for song in &present {
            scheduler.restore(song);
        }
        scheduler.history = state.history.into();
        scheduler.hpos = state
            .hpos
            .filter(|_| !scheduler.history.is_empty())
            .map(|pos| pos.min(scheduler.history.len() - 1));
        if scheduler.random {
            for (song, jit) in state.stack {
                if scheduler.ctx.contains(&song) && !scheduler.stack.contains(&song) {
                    scheduler.stack_jit.insert(song.clone(), jit);
                    scheduler.stack.push(song);
                }
            }
            scheduler.window = state
                .window
                .into_iter()
                .filter(|(song, _)| scheduler.ctx.contains(song))
                .collect();
            scheduler.replan();
        }
        Some(scheduler)
    }

    /// 历史游标所指的歌，即调度器认为正在播放的歌
    pub fn current(&self) -> Option<&SongKey> {
        self.hpos.map(|pos| &self.history[pos])
    }

    /// 该歌是否在插队栈中
    #[cfg(test)]
    pub(crate) fn is_inserted(&self, song: &SongKey) -> bool {
        self.stack.contains(song)
    }

    /// 接下来将依次播放的歌：插队栈、回退后待重放的历史、窗口
    #[cfg(test)]
    fn up_next(&self) -> Vec<SongKey> {
        let start = self.hpos.map_or(0, |pos| pos + 1);
        self.stack
            .iter()
            .rev()
            .chain(
                self.history
                    .iter()
                    .skip(start)
                    .filter(|s| self.ctx.contains(*s)),
            )
            .chain(self.window.iter().map(|(s, _)| s))
            .cloned()
            .collect()
    }
}

/// `shuffle_state.json` 的内容
#[derive(Serialize, Deserialize)]
pub struct ShuffleState {
    version: u32,
    tick: u64,
    random: bool,
    songs: BTreeMap<SongKey, SavedSong>,
    ctx: Vec<SongKey>,
    window: Vec<(SongKey, f64)>,
    stack: Vec<(SongKey, f64)>,
    history: Vec<SongKey>,
    hpos: Option<usize>,
}

#[derive(Serialize, Deserialize)]
struct SavedSong {
    last: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    played: Option<u64>,
    jit: f64,
    cn: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    left: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ln: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    fn key(name: &str) -> SongKey {
        SongKey(name.into())
    }

    fn keys(range: std::ops::Range<usize>) -> Vec<SongKey> {
        range.map(|i| key(&format!("s{i}"))).collect()
    }

    fn scheduler(seed: u64, songs: Vec<SongKey>) -> ShuffleScheduler {
        let mut s = ShuffleScheduler::new(StdRng::seed_from_u64(seed), true);
        s.set_list(songs, None);
        s
    }

    fn sorted_ctx(s: &ShuffleScheduler) -> Vec<SongKey> {
        let mut ctx: Vec<_> = s.ctx.iter().cloned().collect();
        ctx.sort();
        ctx
    }

    fn round_trip(s: &ShuffleScheduler, seed: u64) -> ShuffleScheduler {
        let json = serde_json::to_string(&s.state()).unwrap();
        let state = serde_json::from_str(&json).unwrap();
        ShuffleScheduler::from_state(state, StdRng::seed_from_u64(seed)).unwrap()
    }

    /// 插队栈与窗口只含列表中的歌，规划序列中每一项与该歌上一次播放相隔不小于冷却期
    fn check_plan(s: &ShuffleScheduler) {
        assert!(s.window.iter().all(|(w, _)| s.ctx.contains(w)));
        assert!(s.stack.iter().all(|w| s.ctx.contains(w)));
        assert!(s.stack.iter().all(|w| s.stack_jit.contains_key(w)));
        if !s.random {
            assert!(s.window.is_empty() && s.stack.is_empty());
            return;
        }
        let cd = s.cooldown() as f64;
        let mut vt = s.tick;
        let mut planned = HashMap::new();
        for w in s.stack.iter().rev() {
            vt += 1;
            planned.insert(w, vt as f64);
        }
        for (w, _) in &s.window {
            vt += 1;
            let prev = planned.get(w).copied().unwrap_or(s.songs[w].played_at());
            assert!(vt as f64 - prev >= cd, "planned gap below cooldown");
            planned.insert(w, vt as f64);
        }
        if s.ctx.iter().any(|w| !s.stack.contains(w)) {
            assert!(!s.window.is_empty());
        }
        assert!(s.songs.values().all(|state| state.last <= s.tick as f64));
        assert!(s.songs.values().all(|state| state.played <= Some(s.tick)));
    }

    #[test]
    fn song_key_follows_identity_rules() {
        let mut local = SongInfo::new("a".into(), SourceId::Local, "歌".into(), "人".into());
        local.file_path = Some("/music/a.flac".into());
        let mut same_file = local.clone();
        same_file.id = "other".into();
        assert_eq!(SongKey::of(&local), SongKey::of(&same_file));

        let cue = |track: &str| {
            let mut song = local.clone();
            song.id = format!("/music/album.cue#{track}");
            song.extra.insert("cue_track".into(), track.into());
            song
        };
        assert_ne!(SongKey::of(&cue("01")), SongKey::of(&cue("02")));
        assert_ne!(SongKey::of(&cue("01")), SongKey::of(&local));
        assert_eq!(SongKey::of(&cue("01")), SongKey::of(&cue("01")));

        let kw = SongInfo::new("1".into(), SourceId::Kw, "歌".into(), "人".into());
        let kg = SongInfo::new("1".into(), SourceId::Kg, "歌".into(), "人".into());
        assert_ne!(SongKey::of(&kw), SongKey::of(&kg));

        let anonymous = SongInfo::new(String::new(), SourceId::Kw, "歌".into(), "人".into());
        let other = SongInfo::new(String::new(), SourceId::Kw, "歌".into(), "别人".into());
        assert_ne!(SongKey::of(&anonymous), SongKey::of(&other));
        assert_ne!(SongKey::of(&anonymous), SongKey::of(&kw));

        // JS 音源结果按 extra["source"] 的平台区分，对应内置音源时与其视为同一首
        let js = |platform: &str| {
            let mut song = SongInfo::new("1".into(), SourceId::Local, "歌".into(), "人".into());
            song.extra.insert("source".into(), platform.into());
            song
        };
        assert_eq!(SongKey::of(&js("KW")), SongKey::of(&kw));
        assert_eq!(SongKey::of(&js("kg")), SongKey::of(&kg));
        assert_ne!(SongKey::of(&js("custom")), SongKey::of(&js("other")));
        let plain_local = SongInfo::new("1".into(), SourceId::Local, "歌".into(), "人".into());
        assert_ne!(SongKey::of(&js("custom")), SongKey::of(&plain_local));
        assert_ne!(SongKey::of(&plain_local), SongKey::of(&kw));
        // 只有 Local 歌曲的 extra["source"] 才是 JS 平台标记
        let mut tagged_kw = kw.clone();
        tagged_kw.extra.insert("source".into(), "wy".into());
        assert_eq!(SongKey::of(&tagged_kw), SongKey::of(&kw));
    }

    #[test]
    fn natural_playback_respects_cooldown_and_reaches_every_song() {
        for n in [1, 2, 3, 5, 20, 200] {
            let mut s = scheduler(n as u64, keys(0..n));
            let mut last = HashMap::new();
            let mut max_gap = 0;
            for _ in 0..(60 * n).max(200) {
                let cd = s.cooldown() as u64;
                let (song, kind) = s.next().unwrap();
                assert_eq!(kind, Advance::Natural);
                if let Some(prev) = last.insert(song, s.tick) {
                    let gap = s.tick - prev;
                    assert!(n < 2 || gap >= cd, "n={n} gap {gap} < cooldown {cd}");
                    max_gap = max_gap.max(gap);
                }
                check_plan(&s);
            }
            assert_eq!(last.len(), n);
            assert!(max_gap <= 4 * n as u64, "n={n} max gap {max_gap}");
        }
    }

    #[test]
    fn natural_playback_keeps_the_window() {
        let mut s = scheduler(1, keys(0..200));
        for _ in 0..500 {
            let before = s.up_next();
            let (song, _) = s.next().unwrap();
            let after = s.up_next();
            assert_eq!(song, before[0]);
            assert_eq!(after[..WINDOW - 1], before[1..]);
        }
    }

    #[test]
    fn inserts_play_last_in_first_out() {
        let mut s = scheduler(2, keys(0..50));
        s.next();
        let (a, b) = (key("s10"), key("s20"));
        s.insert(&a);
        s.insert(&b);
        assert_eq!(s.up_next()[..2], [b.clone(), a.clone()]);
        assert!(!s.window.iter().any(|(w, _)| *w == a || *w == b));
        assert_eq!(s.next(), Some((b, Advance::Insert)));
        assert_eq!(s.next(), Some((a, Advance::Insert)));
        check_plan(&s);
    }

    #[test]
    fn cancelled_insert_restores_the_plan() {
        let mut s = scheduler(20, keys(0..30));
        s.next();
        let up_next = s.up_next();
        let target = s.window[5].0.clone();
        s.insert(&target);
        s.cancel_insert(&target);
        assert!(!s.is_inserted(&target));
        assert_eq!(s.up_next()[..5], up_next[..5]);
        check_plan(&s);
    }

    #[test]
    fn inserting_the_song_just_played_replays_it() {
        let mut s = scheduler(3, keys(0..2));
        let (song, _) = s.next().unwrap();
        s.insert(&song);
        assert_eq!(s.next(), Some((song, Advance::Insert)));
        check_plan(&s);
    }

    #[test]
    fn jump_commits_the_target_and_leaves_skipped_songs_unplayed() {
        let mut s = scheduler(4, keys(0..200));
        s.next();
        let skipped: Vec<SongKey> = s.window[..5].iter().map(|(w, _)| w.clone()).collect();
        let before: Vec<f64> = skipped.iter().map(|w| s.songs[w].last).collect();
        let target = s.window[5].0.clone();
        let tick = s.tick;
        s.jump(&target);
        assert_eq!(s.current(), Some(&target));
        assert_eq!(s.tick, tick + 1);
        let after: Vec<f64> = skipped.iter().map(|w| s.songs[w].last).collect();
        assert_eq!(before, after);
        check_plan(&s);
    }

    #[test]
    fn prev_replays_history_and_repeats_the_start() {
        let mut s = scheduler(5, keys(0..30));
        let played: Vec<SongKey> = (0..4).map(|_| s.next().unwrap().0).collect();
        let window = s.window.clone();
        assert_eq!(s.prev(), Some(played[2].clone()));
        assert_eq!(s.prev(), Some(played[1].clone()));
        assert_eq!(s.prev(), Some(played[0].clone()));
        let revision = s.revision;
        assert_eq!(s.prev(), Some(played[0].clone()));
        assert_eq!(s.revision, revision);
        assert_eq!(s.current(), Some(&played[0]));
        assert_eq!(s.window, window);
        let tick = s.tick;
        assert_eq!(s.next(), Some((played[1].clone(), Advance::Replay)));
        assert_eq!(s.tick, tick);
        // 回退期间插队仍然优先，播放后回到历史最新位置
        let inserted = s.window[3].0.clone();
        s.insert(&inserted);
        assert_eq!(s.next(), Some((inserted, Advance::Insert)));
        assert_eq!(s.next().unwrap().1, Advance::Natural);
    }

    #[test]
    fn prev_skips_songs_that_left_the_list() {
        let mut s = scheduler(6, keys(0..30));
        let played: Vec<SongKey> = (0..3).map(|_| s.next().unwrap().0).collect();
        s.remove(&played[1]);
        assert_eq!(s.prev(), Some(played[0].clone()));
        s.remove(&played[0]);
        assert_eq!(s.prev(), None);
    }

    #[test]
    fn absent_song_resumes_its_progress() {
        let a = keys(0..40);
        let b = keys(100..140);
        let mut s = scheduler(7, a.clone());
        for _ in 0..100 {
            s.next();
        }
        let ages: Vec<f64> = a.iter().map(|w| s.tick as f64 - s.songs[w].last).collect();
        s.set_list(b.clone(), Some(&b[0]));
        for _ in 0..500 {
            s.next();
        }
        s.set_list(a.clone(), None);
        let resumed: Vec<f64> = a.iter().map(|w| s.tick as f64 - s.songs[w].last).collect();
        for (before, after) in ages.iter().zip(&resumed) {
            assert!((before - after).abs() < 1e-9, "{before} != {after}");
        }
        assert!(b.iter().all(|w| s.songs[w].left.is_some()));
        check_plan(&s);
    }

    #[test]
    fn songs_never_played_are_forgotten_when_leaving() {
        let mut s = scheduler(8, keys(0..10));
        let (played, _) = s.next().unwrap();
        s.set_list(keys(20..30), None);
        assert_eq!(s.songs.len(), 11);
        assert!(s.songs[&played].left.is_some());
    }

    #[test]
    fn list_size_changes_scale_age_down_only() {
        let big = keys(0..400);
        let small: Vec<SongKey> = big[..40].to_vec();
        let mut s = scheduler(9, big.clone());
        for _ in 0..300 {
            s.next();
        }
        let tick = s.tick as f64;
        let ages: Vec<f64> = small.iter().map(|w| tick - s.songs[w].last).collect();
        s.set_list(small.clone(), None);
        for (w, age) in small.iter().zip(&ages) {
            let scaled = tick - s.songs[w].last;
            assert!((scaled - age * 0.1).abs() < 1e-9);
            assert_eq!(s.songs[w].cn, 40);
        }
        let tick = s.tick as f64;
        let ages: Vec<f64> = small.iter().map(|w| tick - s.songs[w].last).collect();
        s.set_list(big, None);
        for (w, age) in small.iter().zip(&ages) {
            assert!((tick - s.songs[w].last - age).abs() < 1e-9);
        }
        check_plan(&s);
    }

    #[test]
    fn plays_outside_random_mode_are_registered() {
        let list = keys(0..40);
        let mut s = scheduler(10, list.clone());
        s.set_random(false);
        assert!(s.window.is_empty());
        s.set_list(list.clone(), Some(&list[0]));
        for w in &list[1..10] {
            s.observe(w);
        }
        assert_eq!(s.tick, 10);
        s.set_random(true);
        check_plan(&s);
        // 最近顺序播放的 5 首仍在冷却期内，不会出现在窗口前 5 位
        let head: Vec<SongKey> = s.window[..5].iter().map(|(w, _)| w.clone()).collect();
        assert!(head.iter().all(|w| !list[5..10].contains(w)));
    }

    #[test]
    fn same_song_set_keeps_the_insert_stack() {
        let list = keys(0..50);
        let mut s = scheduler(11, list.clone());
        s.next();
        s.insert(&list[7]);
        s.set_list(list.iter().rev().cloned(), Some(&list[3]));
        assert_eq!(s.current(), Some(&list[3]));
        assert_eq!(s.up_next()[0], list[7]);

        let mut other = list.clone();
        other.push(key("new"));
        s.set_list(other, None);
        assert!(s.stack.is_empty());
        check_plan(&s);
    }

    #[test]
    fn leaving_random_mode_restores_inserted_songs() {
        let mut s = scheduler(15, keys(0..200));
        s.next();
        let song = sorted_ctx(&s)
            .into_iter()
            .find(|w| !s.window.iter().any(|(x, _)| x == w))
            .unwrap();
        let committed = (s.songs[&song].last, s.songs[&song].jit);
        s.insert(&song);
        assert!(s.songs[&song].plan_last > committed.0);
        s.set_random(false);
        let state = &s.songs[&song];
        assert_eq!((state.last, state.jit), committed);
        assert_eq!(state.plan_last, state.played_at());
        let live: Vec<f64> = s
            .heap
            .iter()
            .filter(|Reverse(c)| c.song == song && ShuffleScheduler::is_live(&s.songs, c))
            .map(|Reverse(c)| c.key)
            .collect();
        assert_eq!(live, vec![committed.0 + committed.1]);
    }

    #[test]
    fn replacing_the_current_song_hands_over_its_play() {
        let mut s = scheduler(16, keys(0..30));
        for _ in 0..10 {
            s.next();
        }
        let from = s.next().unwrap().0;
        let tick = s.tick;
        let inserted = s.window[3].0.clone();
        s.insert(&inserted);
        let parts = [key("v-p1"), key("v-p2")];
        let list: Vec<SongKey> = sorted_ctx(&s)
            .into_iter()
            .filter(|w| w != &from)
            .chain(parts.clone())
            .collect();
        let revision = s.revision;
        s.replace_current(list.clone(), &from, &parts[0]);
        assert_eq!(s.revision, revision + 1);

        assert_eq!(s.tick, tick);
        assert_eq!(s.current(), Some(&parts[0]));
        assert_eq!(s.songs[&parts[0]].last, tick as f64);
        assert_eq!(s.songs[&parts[0]].played, Some(tick));
        assert!(s.songs[&parts[1]].played.is_none());
        assert!(s.songs[&from].left.is_some());
        assert_eq!(s.stack, vec![inserted.clone()]);
        check_plan(&s);
        assert_eq!(s.next(), Some((inserted, Advance::Insert)));

        // 被替换的歌不是当前曲时只同步列表
        let current = s.current().cloned();
        s.replace_current(list, &key("other"), &parts[1]);
        assert_eq!(s.current().cloned(), current);
        assert!(s.songs[&parts[1]].played.is_none());
    }

    #[test]
    fn leaving_random_mode_keeps_only_the_current_song_in_history() {
        let mut s = scheduler(17, keys(0..30));
        let played: Vec<SongKey> = (0..4).map(|_| s.next().unwrap().0).collect();
        for _ in 0..2 {
            s.prev();
        }
        s.set_random(false);
        assert_eq!(s.history, [played[1].clone()]);
        s.set_random(true);
        // 刚切回随机模式时"上一首"原地重播当前曲，下一首由窗口给出
        assert_eq!(s.current(), Some(&played[1]));
        assert_eq!(s.prev(), Some(played[1].clone()));
        assert_eq!(s.next().unwrap().1, Advance::Natural);
    }

    #[test]
    fn plays_outside_random_mode_do_not_build_history() {
        let list = keys(0..10);
        let mut s = scheduler(19, list.clone());
        s.set_random(false);
        for _ in 0..30 {
            s.observe(&list[4]);
        }
        s.observe(&list[5]);
        assert_eq!(s.history, [list[5].clone()]);
        s.set_random(true);
        assert_eq!(s.prev(), Some(list[5].clone()));
        assert_eq!(s.prev(), Some(list[5].clone()));
    }

    #[test]
    fn shrinking_to_recent_songs_keeps_the_cooldown() {
        let mut s = scheduler(18, keys(0..2000));
        let mut real_last = HashMap::new();
        for _ in 0..500 {
            let song = s.next().unwrap().0;
            real_last.insert(song, s.tick);
        }
        let mut recent: Vec<(u64, SongKey)> =
            real_last.iter().map(|(w, &t)| (t, w.clone())).collect();
        recent.sort_unstable_by(|a, b| b.cmp(a));
        s.set_list(recent.into_iter().take(100).map(|(_, w)| w), None);
        let cd = s.cooldown() as u64;
        for _ in 0..300 {
            let song = s.next().unwrap().0;
            assert!(s.tick - real_last[&song] >= cd);
            real_last.insert(song, s.tick);
        }
    }

    #[test]
    fn prune_keeps_the_most_recently_absent_songs() {
        let mut list = keys(0..3);
        let mut s = scheduler(12, list.clone());
        for group in 1..5 {
            for song in &list {
                s.jump(song);
            }
            list = keys(group * 10..group * 10 + 3);
            s.set_list(list.clone(), None);
        }
        // 4 组各 3 首已播放后离开，最早离开的一组最先遗忘
        let absent = |s: &ShuffleScheduler| s.songs.keys().filter(|w| !s.ctx.contains(*w)).count();
        assert_eq!(absent(&s), 12);
        s.prune(6);
        assert_eq!(absent(&s), 6);
        assert!(keys(0..3).iter().all(|w| !s.songs.contains_key(w)));
        assert!(keys(30..33).iter().all(|w| s.songs.contains_key(w)));
    }

    #[test]
    fn state_round_trip_preserves_up_next() {
        let mut s = scheduler(13, keys(0..60));
        for _ in 0..30 {
            s.next();
        }
        s.insert(&key("s3"));
        s.insert(&key("s9"));
        s.prev();
        s.set_list(keys(10..70), None);
        s.next();
        let restored = round_trip(&s, 99);
        assert_eq!(restored.up_next(), s.up_next());
        assert_eq!(restored.current(), s.current());
        assert_eq!(restored.tick, s.tick);
        check_plan(&restored);
    }

    #[test]
    fn empty_and_single_song_lists() {
        let mut s = scheduler(14, Vec::new());
        assert_eq!(s.next(), None);
        assert_eq!(s.prev(), None);
        s.add(key("only"));
        for _ in 0..5 {
            assert_eq!(s.next(), Some((key("only"), Advance::Natural)));
        }
        s.remove(&key("only"));
        assert_eq!(s.next(), None);
        s.add(key("again"));
        assert_eq!(s.next().unwrap().0, key("again"));
    }

    #[test]
    fn two_songs_alternate() {
        let mut s = scheduler(15, keys(0..2));
        let mut prev = s.next().unwrap().0;
        for _ in 0..100 {
            let (song, _) = s.next().unwrap();
            assert_ne!(song, prev);
            prev = song;
        }
    }

    /// 随机混合各种操作，每步检查规划序列，并按真实 tick 检查冷却期这一硬约束
    #[test]
    fn mixed_operations_keep_invariants() {
        for (n, seed) in [(2, 21), (3, 22), (5, 23), (20, 24), (200, 25)] {
            let mut rng = StdRng::seed_from_u64(seed);
            let mut s = scheduler(seed, keys(0..n));
            let mut next_id = n;
            let mut seen = keys(0..n);
            let mut real_last: HashMap<SongKey, u64> = HashMap::new();
            for step in 0..10_000 {
                let roll = rng.gen_range(0..100);
                match roll {
                    0..=82 => {
                        let cd = s.cooldown() as u64;
                        let expected = s.up_next().first().cloned();
                        let Some((song, kind)) = s.next() else {
                            continue;
                        };
                        assert_eq!(Some(&song), expected.as_ref());
                        if kind == Advance::Natural
                            && let Some(&prev) = real_last.get(&song)
                        {
                            assert!(s.n() < 2 || s.tick - prev >= cd, "n={n} step {step}");
                        }
                        if kind != Advance::Replay {
                            real_last.insert(song, s.tick);
                        }
                    }
                    83..=85 => {
                        let (window, stack) = (s.window.clone(), s.stack.clone());
                        s.prev();
                        assert_eq!((&s.window, &s.stack), (&window, &stack));
                    }
                    86..=88 => {
                        let ctx = sorted_ctx(&s);
                        if let Some(target) = ctx.choose(&mut rng).cloned() {
                            s.jump(&target);
                            assert_eq!(s.current(), Some(&target));
                            real_last.insert(target, s.tick);
                        }
                    }
                    89..=92 => {
                        let ctx = sorted_ctx(&s);
                        if let Some(target) = ctx.choose(&mut rng).cloned() {
                            s.insert(&target);
                            assert_eq!(s.up_next()[0], target);
                        }
                    }
                    93..=94 => {
                        let ctx = sorted_ctx(&s);
                        if let Some(gone) = ctx.choose(&mut rng) {
                            s.remove(gone);
                        }
                        let fresh = key(&format!("s{next_id}"));
                        next_id += 1;
                        seen.push(fresh.clone());
                        s.add(fresh);
                    }
                    95 => {
                        // 缩小到最近播过的歌：换算后的年龄都很小，冷却资格仍按真实 tick
                        let mut recent: Vec<(u64, SongKey)> =
                            real_last.iter().map(|(w, &t)| (t, w.clone())).collect();
                        recent.sort_unstable_by(|a, b| b.cmp(a));
                        let list = recent.into_iter().take((n / 10).max(2)).map(|(_, w)| w);
                        s.set_list(list, None);
                    }
                    96 => {
                        let ctx = sorted_ctx(&s);
                        let mut list: Vec<SongKey> =
                            ctx.choose_multiple(&mut rng, n / 2).cloned().collect();
                        let pool = &seen[seen.len().saturating_sub(4 * n)..];
                        list.extend(pool.choose_multiple(&mut rng, n / 4).cloned());
                        while list.len() < n.max(1) {
                            let fresh = key(&format!("s{next_id}"));
                            next_id += 1;
                            seen.push(fresh.clone());
                            list.push(fresh);
                        }
                        let start = list.choose(&mut rng).cloned().unwrap();
                        s.set_list(list, Some(&start));
                        assert_eq!(s.current(), Some(&start));
                        real_last.insert(start, s.tick);
                    }
                    97..=98 => {
                        s.set_random(false);
                        let ctx = sorted_ctx(&s);
                        for song in ctx.iter().take(rng.gen_range(1..=20)) {
                            let tick = s.tick;
                            s.observe(song);
                            assert_eq!(s.tick, tick + 1);
                            real_last.insert(song.clone(), s.tick);
                        }
                        check_plan(&s);
                        s.set_random(true);
                    }
                    _ => {
                        let up_next = s.up_next();
                        let current = s.current().cloned();
                        s.prune(ABSENT_CAP);
                        s = round_trip(&s, seed + step);
                        assert_eq!(s.up_next(), up_next, "n={n} step {step}");
                        assert_eq!(s.current().cloned(), current);
                    }
                }
                check_plan(&s);
            }
        }
    }
}
