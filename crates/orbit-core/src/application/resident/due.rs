//! When a mandate is due: its wake items, cadence and live-cycle state.
//!
//! The wake items are what a cycle reacts to, one keyed state each: the
//! mandate and its authority revision; the assignment's settled decision
//! requests; one hash over the tasks the mandate's `watch` names (ids and
//! tags), where any change counts; each task the last checkpoint acted on
//! and each still-open task earlier cycles acted on, by state class, review
//! decision and settled decision requests only; each open incident or
//! maintenance notice in scope; and the assignment's non-resident comments.
//! Observe records them in the cycle's grant, finalize records the state of
//! the tasks the cycle acted on, and the next due check compares item by
//! item, so run churn, edits and the resident's own actions on tasks it
//! acted on do not wake it.
//!
//! A mandate is due when such a qualifying change happened since the last
//! checkpoint's observation, its planned next wake (clamped between the
//! floor and the heartbeat) or its heartbeat came, its deadline passed since
//! the last cycle, it never cycled, its last cycle died, or a manager's
//! `wake`, `resume` or `release` came after the last cycle started; and only
//! once the cadence floor since the last cycle start allows it and no cycle
//! is live. A mandate is stalled when it has not checkpointed for twice its
//! heartbeat, or its last three checkpoints are all `incomplete`.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::record::{EffectiveStatus, Notice, NoticeKind, NoticeScope};
use orbit_types::task::{DecisionRequestState, Task, TaskComment, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{
    RESIDENT_CYCLE_GRANTED_EVENT, RESIDENT_WAKE_SETTLED_EVENT, ResidentCheckpoint, ResidentComment,
    ResidentCycleGrant, ResidentLiveCycle, ResidentMandate, ResidentWakeItems, ResidentWakeSettled,
    resident_authority_revision,
};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::cycle::{CYCLE_CHANGED, live_cycle_from_history};
use crate::OrbitRuntime;
use crate::application::notice::{NoticeListFilter, NoticeStatusFilter};

/// Most watched tasks one digest reads.
const MAX_WATCHED_TASKS: usize = 200;

/// Consecutive `incomplete` checkpoints that make a mandate stalled.
const STALLED_INCOMPLETE_RUN: usize = 3;

/// One started cycle, as the assignment's history records it.
#[derive(Debug, Clone)]
pub(crate) struct StartedCycle {
    pub(crate) live: ResidentLiveCycle,
    pub(crate) at: DateTime<Utc>,
}

/// Whether a mandate is due, and the facts the cycle steps reuse.
#[derive(Debug, Clone)]
pub(crate) struct ResidentDue {
    pub(crate) mandate: ResidentMandate,
    /// `sha256:<hex>` over the wake items.
    pub(crate) digest: String,
    /// What a cycle reacts to, one keyed state each; observe records them
    /// in the grant as the next due check's baseline.
    pub(crate) wake_items: ResidentWakeItems,
    /// Why a cycle would wake; empty when it would not.
    pub(crate) reasons: Vec<String>,
    /// Why a due mandate may not cycle yet (`cycle_live`, `cadence_floor`).
    pub(crate) withheld: Option<String>,
    /// The last cycle started, whether or not it is still live.
    pub(crate) last_cycle: Option<StartedCycle>,
    /// The published live cycle when its run stopped or its lease expired.
    pub(crate) dead_cycle: Option<ResidentLiveCycle>,
    /// Since when the mandate has been waiting for its next cycle.
    pub(crate) episode_start: DateTime<Utc>,
    /// Maintenance notices open in the mandate's scope (ids), or
    /// `notices_unreadable` when they could not be read.
    pub(crate) maintenance: Vec<String>,
    /// The notices in scope, for the brief.
    pub(crate) notices: Vec<Notice>,
    /// When the pending manager wake (`wake`, `resume`, `release`) was
    /// written, if one is pending.
    pub(crate) wake_at: Option<DateTime<Utc>>,
}

impl ResidentDue {
    /// The scheduler member fingerprint: the last cycle it follows and what
    /// made it due (a manager wake, the heartbeat, the planned next wake, a
    /// qualifying change, or else the digest), so each cycle is its own
    /// member episode.
    pub(crate) fn fingerprint(&self) -> String {
        let after = self
            .last_cycle
            .as_ref()
            .map_or("none", |cycle| cycle.live.cycle.as_str());
        let why = if let Some(at) = self.wake_at {
            format!("wake:{}", at.to_rfc3339())
        } else if self.reasons.iter().any(|reason| reason == "heartbeat") {
            "heartbeat".to_string()
        } else if self.reasons.iter().any(|reason| reason == "next_wake") {
            "next_wake".to_string()
        } else if self.reasons.iter().any(|reason| reason == "digest_changed") {
            // Not the digest: churn that does not qualify still moves it,
            // and must not retire a member a qualifying change admitted.
            "changed".to_string()
        } else {
            self.digest.clone()
        };
        format!("after:{after}|{why}")
    }
}

/// Every cycle start the history records, oldest first.
pub(crate) fn started_cycles(history: &[TaskHistoryEntry]) -> Vec<StartedCycle> {
    history
        .iter()
        .filter(|entry| entry.event == CYCLE_CHANGED)
        .filter_map(|entry| {
            serde_json::from_str::<Option<ResidentLiveCycle>>(entry.note.as_deref()?)
                .ok()
                .flatten()
                .map(|live| StartedCycle { live, at: entry.at })
        })
        .collect()
}

/// The grant recorded for `cycle`, if observe recorded one.
pub(crate) fn cycle_grant(history: &[TaskHistoryEntry], cycle: &str) -> Option<ResidentCycleGrant> {
    history
        .iter()
        .rev()
        .filter(|entry| entry.event == RESIDENT_CYCLE_GRANTED_EVENT)
        .filter_map(|entry| serde_json::from_str::<ResidentCycleGrant>(entry.note.as_deref()?).ok())
        .find(|grant| grant.cycle == cycle)
}

/// Whether `cycle` fell back to `continuity: rebuilt` after an expired or
/// unresumable conversation reference.
pub(crate) fn continuity_rebuilt(history: &[TaskHistoryEntry], cycle: &str) -> bool {
    history.iter().any(|entry| {
        entry.event == orbit_types::workflow::RESIDENT_CONTINUITY_REBUILT_EVENT
            && entry.note.as_deref() == Some(cycle)
    })
}

/// The mandate's checkpoints, oldest first.
pub(crate) fn checkpoints(task_id: &str, comments: &[TaskComment]) -> Vec<ResidentCheckpoint> {
    comments
        .iter()
        .filter_map(
            |comment| match ResidentComment::parse(&comment.message).ok()? {
                ResidentComment::Checkpoint(record) if record.mandate == task_id => Some(*record),
                _ => None,
            },
        )
        .collect()
}

/// Why an active mandate is stalled, and since when.
#[derive(Debug, Clone)]
pub(crate) struct ResidentStall {
    pub(crate) reason: String,
    pub(crate) since: DateTime<Utc>,
    pub(crate) last_checkpoint_at: Option<DateTime<Utc>>,
}

/// Whether a mandate is stalled at `now`: no checkpoint (or, never having
/// checkpointed, no first one since the task was created) for twice its
/// heartbeat, or its last three checkpoints all `incomplete`. Callers ask
/// only for an active mandate: a paused or taken-over one is meant to wait.
pub(crate) fn resident_stall(
    task: &Task,
    mandate: &ResidentMandate,
    comments: &[TaskComment],
    now: DateTime<Utc>,
) -> Option<ResidentStall> {
    let written: Vec<(DateTime<Utc>, String)> = comments
        .iter()
        .filter_map(
            |comment| match ResidentComment::parse(&comment.message).ok()? {
                ResidentComment::Checkpoint(record) if record.mandate == task.id.as_str() => {
                    Some((comment.at, record.outcome))
                }
                _ => None,
            },
        )
        .collect();
    let last_checkpoint_at = written.last().map(|(at, _)| *at);
    let tail: Vec<&(DateTime<Utc>, String)> =
        written.iter().rev().take(STALLED_INCOMPLETE_RUN).collect();
    if tail.len() == STALLED_INCOMPLETE_RUN
        && tail.iter().all(|(_, outcome)| outcome == "incomplete")
    {
        return Some(ResidentStall {
            reason: format!("its last {STALLED_INCOMPLETE_RUN} cycles were all incomplete"),
            since: tail.last().map_or(now, |(at, _)| *at),
            last_checkpoint_at,
        });
    }
    let since = last_checkpoint_at.unwrap_or(task.created_at);
    let limit = Duration::minutes(2 * i64::from(mandate.cadence.heartbeat_minutes));
    (now - since >= limit).then(|| ResidentStall {
        reason: match last_checkpoint_at {
            Some(_) => format!(
                "no checkpoint for over twice its {}-minute heartbeat",
                mandate.cadence.heartbeat_minutes
            ),
            None => format!(
                "no first checkpoint within twice its {}-minute heartbeat",
                mandate.cadence.heartbeat_minutes
            ),
        },
        since,
        last_checkpoint_at,
    })
}

fn digest_of(value: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    format!(
        "sha256:{}",
        orbit_types::identity::lower_hex(Sha256::digest(bytes))
    )
}

/// Comments a cycle reacts to: everything but the resident's own
/// checkpoints and requests, and, while a cycle was live, only what a human
/// wrote (an agent's comments inside a cycle are the cycle's own work).
fn wake_comments(comments: &[TaskComment], cycles: &[StartedCycle]) -> Vec<serde_json::Value> {
    comments
        .iter()
        .filter(|comment| {
            match ResidentComment::parse(&comment.message) {
                Ok(ResidentComment::Answer(_)) => return true,
                Ok(ResidentComment::Checkpoint(_) | ResidentComment::Request(_)) => return false,
                Err(_) => {}
            }
            comment.by.starts_with("human")
                || !cycles
                    .iter()
                    .any(|cycle| comment.at >= cycle.at && comment.at <= cycle.live.expires_at)
        })
        .map(|comment| {
            json!([
                comment.at,
                comment.by,
                orbit_types::identity::lower_hex(Sha256::digest(comment.message.as_bytes()))
            ])
        })
        .collect()
}

/// A task's state class in the wake items. Moving between `proposed`,
/// `backlog` and `in-progress` is drain churn, not a state the resident acts
/// on, so those share one class.
fn task_wake_class(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Proposed | TaskStatus::Backlog | TaskStatus::InProgress => "active",
        TaskStatus::Blocked => "blocked",
        TaskStatus::Review => "review",
        TaskStatus::Someday => "someday",
        TaskStatus::Done => "done",
        TaskStatus::Archived => "archived",
        TaskStatus::Rejected => "rejected",
    }
}

/// The class a wake item value starts with.
fn wake_item_class(value: &str) -> &str {
    value.split(' ').next().unwrap_or_default()
}

/// Whether a task item's class is closed, so earlier cycles' subjects stop
/// being carried in the watched set.
fn wake_class_closed(class: &str) -> bool {
    matches!(class, "done" | "archived" | "rejected")
}

/// The settled (answered, superseded or expired) decision requests among
/// `requests`, as one stable segment. An open request is the resident's own
/// question or one it already sees in its brief; it wakes on its answer.
fn settled_decisions(requests: &[orbit_types::task::DecisionRequest]) -> String {
    let mut settled: Vec<String> = requests
        .iter()
        .filter(|request| request.state != DecisionRequestState::Open)
        .map(|request| format!("{}={}", request.id, request.state.as_str()))
        .collect();
    settled.sort();
    if settled.is_empty() {
        String::new()
    } else {
        format!("decisions:{}", settled.join(","))
    }
}

/// Whether the wake items changed in a way the mandate acts on since
/// `baseline`: an item present in both changed state, a newly present item
/// needs attention (a task the resident acted on that is `blocked` or in
/// `review`, or any other new key such as a notice opening), or an item
/// other than such a task is gone (a notice cleared). A task that only
/// joined or left the set of tasks the resident acted on, merely active or
/// closed, does not count.
fn wake_items_changed(baseline: &ResidentWakeItems, current: &ResidentWakeItems) -> bool {
    let changed_or_new = current.iter().any(|(key, value)| match baseline.get(key) {
        Some(before) => before != value,
        None if key.starts_with("task:") => {
            matches!(wake_item_class(value), "blocked" | "review")
        }
        None => true,
    });
    changed_or_new
        || baseline
            .keys()
            .any(|key| !key.starts_with("task:") && !current.contains_key(key))
}

/// When the mandate's next unprompted cycle is due after the last cycle
/// started at `started`: its last checkpoint's planned `next_wake`, clamped
/// between the floor and the heartbeat, or the heartbeat when the last
/// cycle left no checkpoint of its own or ended `incomplete` (a synthesized
/// or failed cycle's plan is not the resident's).
fn planned_wake_at(
    mandate: &ResidentMandate,
    started: &StartedCycle,
    last: Option<&ResidentCheckpoint>,
) -> DateTime<Utc> {
    let floor = started.at + Duration::minutes(i64::from(mandate.cadence.floor_minutes));
    let heartbeat = started.at + Duration::minutes(i64::from(mandate.cadence.heartbeat_minutes));
    match last {
        Some(checkpoint)
            if checkpoint.cycle == started.live.cycle && checkpoint.outcome != "incomplete" =>
        {
            checkpoint
                .next_wake
                .not_before
                .clamp(floor, heartbeat.max(floor))
        }
        _ => heartbeat,
    }
}

impl OrbitRuntime {
    /// Whether `notice` reaches a mandate covering `workspaces`.
    fn notice_in_scope(&self, notice: &Notice, workspaces: &BTreeSet<String>) -> bool {
        notice.scope == NoticeScope::Global
            || workspaces.contains(&notice.workspace.id)
            || workspaces.contains(&notice.workspace.name)
    }

    /// The notices in a mandate's scope (this workspace, the mandate's
    /// workspaces homed on this owner, and global ones), any state.
    pub(super) fn resident_notices(
        &self,
        mandate: &ResidentMandate,
    ) -> Result<Vec<Notice>, OrbitError> {
        let mut workspaces: BTreeSet<String> = mandate.workspaces.iter().cloned().collect();
        workspaces.insert(self.workspace_id()?);
        let listing = self.list_notices(&NoticeListFilter {
            status: NoticeStatusFilter::All,
            all_workspaces: true,
            ..Default::default()
        })?;
        Ok(listing
            .notices
            .into_iter()
            .filter(|notice| self.notice_in_scope(notice, &workspaces))
            .collect())
    }

    /// The tasks whose changes wake the mandate, capped.
    pub(super) fn resident_watched_tasks(
        &self,
        mandate: &ResidentMandate,
        last: Option<&ResidentCheckpoint>,
    ) -> Result<Vec<Task>, OrbitError> {
        let mut ids: BTreeSet<String> = mandate.watch.tasks.iter().cloned().collect();
        if let Some(last) = last {
            ids.extend(last.actions.iter().map(|action| action.subject.clone()));
        }
        let mut tasks = Vec::new();
        if !mandate.watch.tags.is_empty() {
            for task in self.list_task_metadata()? {
                if task.tags.iter().any(|tag| mandate.watch.tags.contains(tag)) {
                    ids.remove(task.id.as_str());
                    tasks.push(task);
                }
            }
        }
        for id in ids {
            // A subject may name a run or a missing task; it simply does not count.
            if let Ok(task) = self.get_task(&id) {
                tasks.push(task);
            }
        }
        tasks.sort_by(|left, right| left.id.cmp(&right.id));
        tasks.dedup_by(|left, right| left.id == right.id);
        tasks.truncate(MAX_WATCHED_TASKS);
        Ok(tasks)
    }

    /// The tasks whose state feeds the wake items, each with whether the
    /// mandate's `watch` names it (by id or tag): explicitly watched tasks
    /// first, then the last checkpoint's subjects, then the still-open tasks
    /// `baseline` carried from earlier cycles; capped.
    fn resident_wake_tasks(
        &self,
        mandate: &ResidentMandate,
        last: Option<&ResidentCheckpoint>,
        baseline: Option<&ResidentWakeItems>,
    ) -> Result<Vec<(Task, bool)>, OrbitError> {
        let mut explicit = Vec::new();
        let mut ids: BTreeSet<String> = mandate.watch.tasks.iter().cloned().collect();
        if !mandate.watch.tags.is_empty() {
            for task in self.list_task_metadata()?.iter() {
                if task.tags.iter().any(|tag| mandate.watch.tags.contains(tag)) {
                    ids.remove(task.id.as_str());
                    explicit.push(task.clone());
                }
            }
        }
        for id in ids {
            // A subject may name a run or a missing task; it simply does not count.
            if let Ok(task) = self.get_task(&id) {
                explicit.push(task);
            }
        }
        explicit.sort_by(|left, right| left.id.cmp(&right.id));
        explicit.dedup_by(|left, right| left.id == right.id);
        let mut seen: BTreeSet<String> = explicit.iter().map(|task| task.id.to_string()).collect();
        let mut tasks: Vec<(Task, bool)> = explicit.into_iter().map(|task| (task, true)).collect();
        let subjects: BTreeSet<String> = last
            .map(|last| {
                last.actions
                    .iter()
                    .map(|action| action.subject.clone())
                    .collect()
            })
            .unwrap_or_default();
        let carried: BTreeSet<String> = baseline
            .into_iter()
            .flatten()
            .filter_map(|(key, value)| {
                let id = key.strip_prefix("task:")?;
                (!wake_class_closed(wake_item_class(value))).then(|| id.to_string())
            })
            .collect();
        for id in subjects.into_iter().chain(carried) {
            if tasks.len() >= MAX_WATCHED_TASKS {
                break;
            }
            if !seen.insert(id.clone()) {
                continue;
            }
            if let Ok(task) = self.get_task(&id) {
                tasks.push((task, false));
            }
        }
        tasks.truncate(MAX_WATCHED_TASKS);
        Ok(tasks)
    }

    /// One watched task's wake item: its state class, review decision and
    /// settled decision requests, plus `updated_at` when the mandate watches
    /// it explicitly (every change to an explicitly watched task wakes).
    pub(crate) fn resident_task_wake_item(
        &self,
        task: &Task,
        explicit: bool,
    ) -> Result<String, OrbitError> {
        let mut value = task_wake_class(task.status).to_string();
        if let Some(pr_status) = task.pr_status.as_deref().filter(|pr| !pr.is_empty()) {
            value.push_str(&format!(" pr:{pr_status}"));
        }
        if explicit {
            value.push_str(&format!(" at:{}", task.updated_at.to_rfc3339()));
        }
        let settled = settled_decisions(&self.get_task_decision_requests(task.id.as_str())?);
        if !settled.is_empty() {
            value.push(' ');
            value.push_str(&settled);
        }
        Ok(value)
    }

    /// The baseline the next due check compares against: the items observe
    /// recorded for `cycle`, overlaid with the state finalize recorded for
    /// the tasks that cycle acted on. `None` for a grant written before
    /// items were recorded.
    fn resident_wake_baseline(
        history: &[TaskHistoryEntry],
        cycle: &str,
    ) -> Option<ResidentWakeItems> {
        let mut items = cycle_grant(history, cycle)?.wake_items?;
        if let Some(settled) = history
            .iter()
            .rev()
            .filter(|entry| entry.event == RESIDENT_WAKE_SETTLED_EVENT)
            .filter_map(|entry| {
                serde_json::from_str::<ResidentWakeSettled>(entry.note.as_deref()?).ok()
            })
            .find(|settled| settled.cycle == cycle)
        {
            items.extend(settled.items);
        }
        Some(items)
    }

    /// Record, for `cycle`'s finished checkpoint, the state of the tasks it
    /// acted on, so the resident's own actions never wake its next cycle.
    pub(crate) fn record_resident_wake_settled(
        &self,
        task_id: &str,
        cycle: &str,
    ) -> Result<(), OrbitError> {
        let comments = self.get_task_comments(task_id)?;
        let Some(checkpoint) = checkpoints(task_id, &comments)
            .into_iter()
            .rev()
            .find(|checkpoint| checkpoint.cycle == cycle)
        else {
            return Ok(());
        };
        let Ok(mandate) = super::resident_mandate_from_artifact(self, task_id)? else {
            return Ok(());
        };
        let subjects: BTreeSet<&str> = checkpoint
            .actions
            .iter()
            .map(|action| action.subject.as_str())
            .collect();
        let mut items = ResidentWakeItems::new();
        for id in subjects {
            let Ok(task) = self.get_task(id) else {
                continue;
            };
            // A task `watch` names is part of the `watch` hash, which keeps
            // what observe saw.
            if mandate.watch.tasks.iter().any(|watched| watched == id)
                || task.tags.iter().any(|tag| mandate.watch.tags.contains(tag))
            {
                continue;
            }
            items.insert(
                format!("task:{id}"),
                self.resident_task_wake_item(&task, false)?,
            );
        }
        if items.is_empty() {
            return Ok(());
        }
        let note = serde_json::to_string(&ResidentWakeSettled {
            cycle: cycle.to_string(),
            items,
        })
        .map_err(|error| OrbitError::Execution(format!("serialize wake items: {error}")))?;
        self.append_resident_history(task_id, RESIDENT_WAKE_SETTLED_EVENT, note)
    }

    /// Whether the live cycle's run can no longer act: its lease expired, or
    /// its run is terminal and its owner stopped, or the run is gone. Any
    /// other lookup failure is an error, so the cycle stays live for this
    /// tick instead of being taken over beside a run that may still act.
    pub(crate) fn resident_cycle_dead(
        &self,
        live: &ResidentLiveCycle,
        now: DateTime<Utc>,
    ) -> Result<bool, OrbitError> {
        if live.expires_at <= now {
            return Ok(true);
        }
        match self.show_job_run(&live.run_id) {
            Ok(run) => Ok(run.state.is_terminal()
                && crate::application::job::run_owner_liveness(&run)
                    == crate::application::job::RunOwnerLiveness::Stopped),
            Err(OrbitError::NotFound {
                kind: NotFoundKind::JobRun,
                ..
            }) => Ok(true),
            Err(error) => Err(error),
        }
    }

    /// The due state of an active mandate at `now`.
    pub(crate) fn resident_due(
        &self,
        task: &Task,
        mandate: ResidentMandate,
        history: &[TaskHistoryEntry],
        now: DateTime<Utc>,
    ) -> Result<ResidentDue, OrbitError> {
        let comments = self.get_task_comments(&task.id)?;
        let checkpoints = checkpoints(&task.id, &comments);
        let last = checkpoints.last();
        let cycles = started_cycles(history);
        let last_cycle = cycles.last().cloned();
        let live = live_cycle_from_history(history)?;

        let baseline = last.and_then(|last| Self::resident_wake_baseline(history, &last.cycle));
        let watched = self.resident_wake_tasks(&mandate, last, baseline.as_ref())?;
        let (notices, notice_error) = match self.resident_notices(&mandate) {
            Ok(notices) => (notices, None),
            Err(error) => (Vec::new(), Some(error.to_string())),
        };
        let maintenance: Vec<String> = if notice_error.is_some() {
            vec!["notices_unreadable".into()]
        } else {
            notices
                .iter()
                .filter(|notice| {
                    notice.kind == NoticeKind::Maintenance && notice.status == EffectiveStatus::Open
                })
                .map(|notice| notice.id.clone())
                .collect()
        };
        let mut wake_items = ResidentWakeItems::new();
        wake_items.insert("mandate".into(), digest_of(&json!(mandate)));
        wake_items.insert(
            "authority".into(),
            resident_authority_revision(history).to_string(),
        );
        wake_items.insert(
            "assignment".into(),
            settled_decisions(&self.get_task_decision_requests(&task.id)?),
        );
        // Tasks `watch` names never leave the set, so one hash keeps the
        // grant small; any change to one of them wakes.
        let mut explicit_items = Vec::new();
        for (watched_task, explicit) in &watched {
            let item = self.resident_task_wake_item(watched_task, *explicit)?;
            if *explicit {
                explicit_items.push(json!([watched_task.id, item]));
            } else {
                wake_items.insert(format!("task:{}", watched_task.id), item);
            }
        }
        wake_items.insert("watch".into(), digest_of(&json!(explicit_items)));
        // Info notices stay in the brief but never wake: only an incident or
        // maintenance notice opening or clearing is a state the mandate acts
        // on. Only open ones are items, so clearing one removes its key.
        for notice in notices.iter().filter(|notice| {
            notice.kind != NoticeKind::Info && notice.status == EffectiveStatus::Open
        }) {
            wake_items.insert(format!("notice:{}", notice.id), "open".into());
        }
        if notice_error.is_some() {
            wake_items.insert("notices".into(), "unreadable".into());
        }
        wake_items.insert(
            "comments".into(),
            digest_of(&json!(wake_comments(&comments, &cycles))),
        );
        let digest = digest_of(&json!(wake_items));

        let mut reasons = Vec::new();
        let mut withheld = None;
        let mut dead_cycle = None;
        if let Some(live) = live {
            if self.resident_cycle_dead(&live, now)? {
                reasons.push(format!("recovery:{}", live.cycle));
                dead_cycle = Some(live);
            } else {
                withheld = Some("cycle_live".to_string());
            }
        }
        let changed = match (last, &baseline) {
            (Some(_), Some(baseline)) => wake_items_changed(baseline, &wake_items),
            (Some(last), None) => last.wake.digest != digest,
            (None, _) => last_cycle.is_some(),
        };
        if last.is_none() && last_cycle.is_none() {
            reasons.push("first_cycle".into());
        } else if changed {
            reasons.push("digest_changed".into());
        }
        let since = last_cycle
            .as_ref()
            .map_or(task.created_at, |cycle| cycle.at);
        if now - since >= Duration::minutes(i64::from(mandate.cadence.heartbeat_minutes)) {
            reasons.push("heartbeat".into());
        } else if let Some(cycle) = &last_cycle
            && now >= planned_wake_at(&mandate, cycle, last)
        {
            reasons.push("next_wake".into());
        }
        let control = super::control::resident_control_from_history(history)
            .map_err(OrbitError::Execution)?;
        let last_cycle_at = last_cycle.as_ref().map(|cycle| cycle.at);
        let pending_wake = control.pending_wake(last_cycle_at).cloned();
        if pending_wake.is_some() {
            reasons.push("wake".into());
            if control.pending_status_request(last_cycle_at) {
                reasons.push("status_request".into());
            }
        }
        if let Some(deadline) = mandate.deadline
            && deadline <= now
            && last_cycle.as_ref().is_none_or(|cycle| cycle.at < deadline)
        {
            reasons.push("deadline".into());
        }
        if withheld.is_none()
            && !reasons.is_empty()
            && let Some(cycle) = &last_cycle
            && now - cycle.at < Duration::minutes(i64::from(mandate.cadence.floor_minutes))
        {
            withheld = Some("cadence_floor".into());
        }
        Ok(ResidentDue {
            mandate,
            digest,
            wake_items,
            reasons,
            withheld,
            last_cycle,
            dead_cycle,
            episode_start: since,
            maintenance,
            notices,
            wake_at: pending_wake.map(|(_, at)| at),
        })
    }
}
