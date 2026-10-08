use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

use crate::storage::Storage;
use crate::sync::{
    self, LocalPushCollection, NeteaseSyncPreview, NeteaseSyncReport, PushPlanPreview, PushReport,
    PushTargetOption, SyncControl,
};
use lx_core::model::source::SourceId;
use lx_core::sync::{SyncCollectionKind, SyncPlan};

use crate::sync::kind_label;

/// 推送选单第一项：不选已有歌单，按本地歌单名在远端新建。
const PUSH_CREATE_LABEL: &str = "＋ 新建歌单";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPhase {
    Preparing,
    Preview,
    Running,
    Done,
    Failed,
    Cancelled,
    /// 推送模式：选择远端目标歌单。
    PushPick,
    /// 推送模式：Diff 预览，等待确认。
    PushDiff,
}

#[derive(Debug, Clone)]
pub struct SyncUiState {
    pub phase: SyncPhase,
    pub preview: Option<NeteaseSyncPreview>,
    pub report: Option<NeteaseSyncReport>,
    pub error: Option<String>,
    /// 推送模式的状态；`None` 表示当前是只读刷新流程。
    pub push: Option<PushUiState>,
}
impl Default for SyncUiState {
    fn default() -> Self {
        Self {
            phase: SyncPhase::Preparing,
            preview: None,
            report: None,
            error: None,
            push: None,
        }
    }
}

/// 推送流程的界面状态。
#[derive(Debug, Clone)]
pub struct PushUiState {
    pub local: LocalPushCollection,
    /// 远端候选目标；不含「新建歌单」虚项（由 `selected == 0` 表示）。
    pub targets: Vec<PushTargetOption>,
    /// 选单光标；0 = 新建歌单，1.. = `targets[selected - 1]`。
    pub selected: usize,
    pub plan: Option<(PushPlanPreview, SyncPlan)>,
    pub report: Option<PushReport>,
}

pub struct SyncOverlay {
    pub state: Arc<Mutex<SyncUiState>>,
    pub control: SyncControl,
    task: Option<JoinHandle<()>>,
    pub selected: usize,
    pub source: SourceId,
}
impl SyncOverlay {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(SyncUiState::default())),
            control: SyncControl::default(),
            task: None,
            selected: 0,
            source: SourceId::Wy,
        }
    }
    pub fn start_for(
        &mut self,
        source: SourceId,
        storage: Arc<Storage>,
        rt: &tokio::runtime::Runtime,
    ) {
        self.source = source;
        self.control = SyncControl::default();
        self.selected = 0;
        let state = Arc::clone(&self.state);
        let control = self.control.clone();
        *state.lock().unwrap_or_else(|e| e.into_inner()) = SyncUiState::default();
        self.task = Some(rt.spawn(async move {
            match sync::preview_source(&storage, source).await {
                Ok(preview) => {
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    s.preview = Some(preview);
                    s.phase = SyncPhase::Preview;
                }
                Err(error) => {
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    s.error = Some(error);
                    s.phase = SyncPhase::Failed;
                }
            }
            let _ = control;
        }));
    }
    pub fn confirm(&mut self, storage: Arc<Storage>, rt: &tokio::runtime::Runtime) {
        self.confirm_for(self.source, storage, rt);
    }
    pub fn confirm_for(
        &mut self,
        source: SourceId,
        storage: Arc<Storage>,
        rt: &tokio::runtime::Runtime,
    ) {
        let phase = self.state.lock().unwrap_or_else(|e| e.into_inner()).phase;
        if !matches!(
            phase,
            SyncPhase::Preview | SyncPhase::Failed | SyncPhase::Cancelled
        ) {
            return;
        }
        self.control
            .cancelled
            .store(false, std::sync::atomic::Ordering::Release);
        self.control
            .done
            .store(0, std::sync::atomic::Ordering::Release);
        let state = Arc::clone(&self.state);
        let control = self.control.clone();
        {
            let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
            s.phase = SyncPhase::Running;
            s.error = None;
        }
        self.task = Some(rt.spawn(async move {
            match sync::sync_source_with_control(&storage, source, &control).await {
                Ok(report) => {
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    s.report = Some(report);
                    s.phase = SyncPhase::Done;
                }
                Err(error) => {
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    s.error = Some(error.clone());
                    s.phase = if error == "同步已取消" {
                        SyncPhase::Cancelled
                    } else {
                        SyncPhase::Failed
                    };
                }
            }
        }));
    }
    pub fn retry(&mut self, storage: Arc<Storage>, rt: &tokio::runtime::Runtime) {
        self.start_for(self.source, storage, rt);
    }
    pub fn cancel(&mut self) {
        self.control.cancel();
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(
            s.phase,
            SyncPhase::Preparing | SyncPhase::Running | SyncPhase::PushPick | SyncPhase::PushDiff
        ) {
            s.phase = SyncPhase::Cancelled;
        }
    }

    // ───────────────── 推送模式（本地 → 远端写回） ─────────────────

    /// 启动推送流程：预检登录并拉取远端候选目标。
    pub fn start_push_for(
        &mut self,
        source: SourceId,
        local: LocalPushCollection,
        rt: &tokio::runtime::Runtime,
    ) {
        self.source = source;
        self.control = SyncControl::default();
        self.selected = 0;
        let state = Arc::clone(&self.state);
        *state.lock().unwrap_or_else(|e| e.into_inner()) = SyncUiState {
            phase: SyncPhase::Preparing,
            push: Some(PushUiState {
                local,
                targets: Vec::new(),
                selected: 0,
                plan: None,
                report: None,
            }),
            ..SyncUiState::default()
        };
        self.task = Some(rt.spawn(async move {
            match sync::push_list_targets(source).await {
                Ok(targets) => {
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(push) = s.push.as_mut() {
                        // 收藏的默认去处是红心歌单；普通歌单默认停在「新建歌单」。
                        push.selected = if push.local.is_favorites {
                            targets
                                .iter()
                                .position(|target| target.kind == SyncCollectionKind::Favorites)
                                .map_or(0, |index| index + 1)
                        } else {
                            0
                        };
                        push.targets = targets;
                    }
                    s.phase = SyncPhase::PushPick;
                }
                Err(error) => {
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    s.error = Some(error);
                    s.phase = SyncPhase::Failed;
                }
            }
        }));
    }

    /// 选单光标移动（含「新建歌单」虚项，共 targets.len() + 1 项）。
    pub fn push_move(&self, delta: isize) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(push) = s.push.as_mut() else {
            return;
        };
        // 收藏没有「新建歌单」虚项，光标只在真实目标之间移动。
        let total = push.targets.len() + usize::from(!push.local.is_favorites);
        let current = isize::try_from(push.selected).unwrap_or(0);
        push.selected = (current + delta).rem_euclid(isize::try_from(total).unwrap_or(1)) as usize;
    }

    /// 当前光标是否停在「新建歌单」虚项上。
    pub fn push_on_create_option(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push
            .as_ref()
            .is_some_and(|push| push.selected == 0 && !push.local.is_favorites)
    }

    /// 本地歌单名（「新建歌单」用默认名）。
    fn push_local_name(&self) -> String {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push
            .as_ref()
            .map(|push| push.local.name.clone())
            .unwrap_or_default()
    }

    /// 直接切换界面阶段（推送 Diff 与选单之间来回、后台任务完成后由任务自身
    /// 改状态的路径之外，主循环也需要在按键层面推进阶段）。
    pub fn set_phase(&self, phase: SyncPhase) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).phase = phase;
    }

    /// 为「新建歌单」选项生成计划：先建远端歌单，再走通用 plan。
    pub fn push_plan_create(&mut self, rt: &tokio::runtime::Runtime) {
        let name = self.push_local_name();
        let state = Arc::clone(&self.state);
        self.set_phase(SyncPhase::Preparing);
        self.task = Some(rt.spawn(async move {
            match sync::push_create_target(SourceId::Wy, &name).await {
                Ok(target) => spawn_plan(state, target).await,
                Err(error) => {
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    s.error = Some(error);
                    s.phase = SyncPhase::Failed;
                }
            }
        }));
    }
    /// 为当前选中的远端目标生成 Diff 计划。
    pub fn push_plan_selected(&mut self, rt: &tokio::runtime::Runtime) {
        let target = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let Some(push) = s.push.as_ref() else {
                return;
            };
            let index = if push.local.is_favorites {
                push.selected
            } else {
                push.selected.wrapping_sub(1)
            };
            push.targets.get(index).cloned()
        };
        let Some(target) = target else {
            return;
        };
        let state = Arc::clone(&self.state);
        self.set_phase(SyncPhase::Preparing);
        self.task = Some(rt.spawn(async move {
            spawn_plan(state, target).await;
        }));
    }

    /// 执行已生成的推送计划。
    pub fn push_confirm(&mut self, rt: &tokio::runtime::Runtime) {
        let plan = {
            let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            s.push
                .as_ref()
                .and_then(|push| push.plan.as_ref().map(|(_, plan)| plan.clone()))
        };
        let Some(plan) = plan else {
            return;
        };
        let state = Arc::clone(&self.state);
        let control = self.control.clone();
        {
            let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
            s.phase = SyncPhase::Running;
            s.error = None;
        }
        self.control
            .cancelled
            .store(false, std::sync::atomic::Ordering::Release);
        self.task = Some(rt.spawn(async move {
            match sync::push_execute(SourceId::Wy, &plan, &control).await {
                Ok(report) => {
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(push) = s.push.as_mut() {
                        push.report = Some(report);
                    }
                    s.phase = SyncPhase::Done;
                }
                Err(error) => {
                    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                    s.error = Some(error.clone());
                    s.phase = if error == "同步已取消" {
                        SyncPhase::Cancelled
                    } else {
                        SyncPhase::Failed
                    };
                }
            }
        }));
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer, ctx: &crate::context::AppContext) {
        Clear.render(area, buf);
        let push_mode = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push
            .is_some();
        let title = if push_mode {
            format!("推送写回 · {}", self.source.display_name())
        } else {
            format!("同步预览 Diff · {}", self.source.display_name())
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(crate::theme::accent(ctx)))
            .title(title);
        let inner = block.inner(area);
        block.render(area, buf);
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mut lines = Vec::new();
        match s.phase {
            SyncPhase::Preparing => lines.push(Line::from(if s.push.is_some() {
                "正在准备推送：检查登录并读取远端歌单..."
            } else {
                "正在读取网易云远程歌单并与缓存比对..."
            })),
            SyncPhase::Preview => {
                if let Some(p) = s.preview {
                    lines.push(Line::from(Span::styled(
                        "本次只把网易云歌单读进远程缓存，不会修改任何远端或本地数据。",
                        Style::new().add_modifier(Modifier::BOLD),
                    )));
                    lines.push(Line::from(""));
                    lines.push(Line::from(format!(
                        "远程歌单   {} 个 · 红心 {} 个",
                        p.playlists, p.favorites
                    )));
                    lines.push(Line::from(format!("歌曲合计   {} 首", p.songs)));
                    lines.push(Line::from(format!(
                        "缓存变化   新增 {} · 更新 {} · 移除 {}",
                        p.added, p.updated, p.removed
                    )));
                    if !p.failed.is_empty() {
                        lines.push(Line::from(Span::styled(
                            format!("未取到 {} 个歌单，将保留缓存里的旧数据", p.failed.len()),
                            Style::new().fg(crate::theme::yellow(ctx)),
                        )));
                    }
                    lines.push(Line::from(""));
                    let room = inner
                        .height
                        .saturating_sub(8 + p.failed.len().min(5) as u16)
                        as usize;
                    for d in p.rows.iter().take(room) {
                        lines.push(Line::from(format!(
                            "  {} {}  {}→{}  [{}]",
                            d.kind, d.name, d.cached, d.remote, d.status
                        )));
                    }
                    if !p.failed.is_empty() {
                        lines.push(Line::from(""));
                        lines.push(Line::from("未取到的歌单："));
                        for (name, reason) in p.failed.iter().take(5) {
                            lines.push(Line::from(format!("  ? {name} — {reason}")));
                        }
                        if p.failed.len() > 5 {
                            lines.push(Line::from(format!("  ... 还有 {} 个", p.failed.len() - 5)));
                        }
                    }
                    lines.push(Line::from(""));
                    lines.push(Line::from("[Enter/S] 确认刷新    [Esc] 取消"));
                }
            }
            SyncPhase::Running => {
                let (done, total) = self.control.progress();
                lines.push(Line::from(if s.push.is_some() {
                    format!(
                        "正在推送「{}」，追加 {} 首（只新增，不删除远端歌曲）",
                        s.push
                            .as_ref()
                            .map(|push| push.local.name.clone())
                            .unwrap_or_default(),
                        total
                    )
                } else {
                    "正在从网易云读取远程歌单，不会写入本地歌单。".to_string()
                }));
                lines.push(Line::from(format!("进度  {done}/{total}")));
                let width = inner.width.saturating_sub(4) as usize;
                let filled = width
                    .saturating_mul(done.min(total))
                    .checked_div(total)
                    .unwrap_or_default();
                lines.push(Line::from(format!(
                    "[{}{}]",
                    "#".repeat(filled),
                    "-".repeat(width.saturating_sub(filled))
                )));
                lines.push(Line::from("[Esc] 取消"));
            }
            SyncPhase::Done => {
                if let Some(push) = s.push.as_ref().filter(|push| push.report.is_some()) {
                    let report = push.report.as_ref().expect("report checked above");
                    lines.push(Line::from(format!(
                        "「{}」已推送到网易云「{}」",
                        push.local.name, report.target_name
                    )));
                    lines.push(Line::from(format!(
                        "新增 {} · 远端已有 {} · 无法匹配 {}",
                        report.added, report.already_present, report.unmatched
                    )));
                    if !report.failed.is_empty() {
                        lines.push(Line::from(Span::styled(
                            format!("{} 首推送失败", report.failed.len()),
                            Style::new().fg(crate::theme::yellow(ctx)),
                        )));
                        for (song, artist, reason) in report.failed.iter().take(5) {
                            lines.push(Line::from(format!("  ✗ {song} - {artist} — {reason}")));
                        }
                        if report.failed.len() > 5 {
                            lines.push(Line::from(format!(
                                "  ... 还有 {} 个",
                                report.failed.len() - 5
                            )));
                        }
                    }
                    lines.push(Line::from("[Enter/Esc] 返回"));
                } else if let Some(r) = s.report {
                    lines.push(Line::from("网易云远程歌单已刷新"));
                    lines.push(Line::from(format!(
                        "歌单 {} 个 · 红心 {} 个 · 歌曲 {} 首",
                        r.playlists, r.favorites, r.songs
                    )));
                    lines.push(Line::from(format!(
                        "新增 {} · 更新 {} · 移除 {}",
                        r.added, r.updated, r.removed
                    )));
                    if !r.failed.is_empty() {
                        lines.push(Line::from(Span::styled(
                            format!("{} 个歌单未取到，已保留缓存里的旧数据", r.failed.len()),
                            Style::new().fg(crate::theme::yellow(ctx)),
                        )));
                        for (name, reason) in r.failed.iter().take(5) {
                            lines.push(Line::from(format!("  ? {name} — {reason}")));
                        }
                    }
                    lines.push(Line::from("[Enter/Esc] 返回"));
                }
            }
            SyncPhase::PushPick => {
                if let Some(push) = s.push.as_ref() {
                    lines.push(Line::from(Span::styled(
                        format!(
                            "把本地「{}」（{} 首）推送到网易云，选择目标：",
                            push.local.name,
                            push.local.songs.len()
                        ),
                        Style::new().add_modifier(Modifier::BOLD),
                    )));
                    lines.push(Line::from(""));
                    let mut options: Vec<(String, String)> = Vec::new();
                    if !push.local.is_favorites {
                        options.push((
                            PUSH_CREATE_LABEL.to_string(),
                            "按本地歌单名新建".to_string(),
                        ));
                    }
                    options.extend(push.targets.iter().map(|target| {
                        (
                            target.name.clone(),
                            format!("（{}，{} 首）", kind_label(target.kind), target.song_count),
                        )
                    }));
                    for (index, (name, detail)) in options.iter().enumerate() {
                        let mark = if push.selected == index { "▸" } else { " " };
                        let mut line = Line::from(format!("{mark} {name} {detail}"));
                        if push.selected == index {
                            line = line.style(Style::new().fg(crate::theme::accent(ctx)));
                        }
                        lines.push(line);
                    }
                    lines.push(Line::from(""));
                    lines.push(Line::from("[↑/↓] 选择    [Enter] 下一步    [Esc] 取消推送"));
                }
            }
            SyncPhase::PushDiff => {
                if let Some(preview) = s
                    .push
                    .as_ref()
                    .and_then(|push| push.plan.as_ref().map(|(preview, _)| preview))
                {
                    let local_name = s
                        .push
                        .as_ref()
                        .map(|push| push.local.name.clone())
                        .unwrap_or_default();
                    lines.push(Line::from(Span::styled(
                        format!(
                            "本地「{}」→ 远端{}「{}」",
                            local_name, preview.target_kind, preview.target_name
                        ),
                        Style::new().add_modifier(Modifier::BOLD),
                    )));
                    lines.push(Line::from(""));
                    lines.push(Line::from(format!(
                        "本地 {} 首 · 远端 {} 首",
                        preview.local_songs, preview.remote_songs
                    )));
                    lines.push(Line::from(format!(
                        "将新增 {} 首 · 远端已有 {} 首 · 无法匹配 {} 首",
                        preview.additions, preview.matched, preview.unmatched
                    )));
                    if preview.additions == 0 {
                        lines.push(Line::from(Span::styled(
                            "没有需要新增的歌曲，推送不会修改远端。",
                            Style::new().fg(crate::theme::yellow(ctx)),
                        )));
                    } else if !preview.samples.is_empty() {
                        lines.push(Line::from(""));
                        lines.push(Line::from("待添加样例："));
                        for sample in &preview.samples {
                            lines.push(Line::from(format!("  + {sample}")));
                        }
                        if preview.additions > preview.samples.len() {
                            lines.push(Line::from(format!(
                                "  ... 还有 {} 首",
                                preview.additions - preview.samples.len()
                            )));
                        }
                    }
                    lines.push(Line::from(""));
                    lines.push(Line::from(
                        "[Enter/S] 确认推送（只追加，不删除远端歌曲）    [Esc] 返回选单",
                    ));
                }
            }
            SyncPhase::Failed | SyncPhase::Cancelled => {
                lines.push(Line::from(if s.phase == SyncPhase::Cancelled {
                    "同步已取消"
                } else {
                    "同步失败"
                }));
                lines.push(Line::from(s.error.unwrap_or_else(|| "未知错误".into())));
                lines.push(Line::from(""));
                lines.push(Line::from("[R] 重试预览    [Esc] 返回"));
            }
        }
        Paragraph::new(lines)
            .style(Style::new().fg(crate::theme::text(ctx)))
            .render(inner, buf);
    }
}

/// 在后台为 `target` 生成推送 Diff 计划并推进界面状态。
async fn spawn_plan(state: Arc<Mutex<SyncUiState>>, target: PushTargetOption) {
    let local = {
        let s = state.lock().unwrap_or_else(|e| e.into_inner());
        match s.push.as_ref() {
            Some(push) => push.local.clone(),
            None => return,
        }
    };
    match sync::push_plan(SourceId::Wy, &local, &target).await {
        Ok((preview, plan)) => {
            let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(push) = s.push.as_mut() {
                push.plan = Some((preview, plan));
            }
            s.phase = SyncPhase::PushDiff;
        }
        Err(error) => {
            let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
            s.error = Some(error);
            s.phase = SyncPhase::Failed;
        }
    }
}
