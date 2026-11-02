//! When a mandate is due: its wake digest, cadence and live-cycle state.
//!
//! The digest hashes what a cycle reacts to: the watched tasks (the
//! mandate's `watch` ids and tags plus the subjects of the last checkpoint's
//! actions), decision requests on the assignment and those tasks, notices in
//! the mandate's workspaces or global scope, and the assignment's
//! non-resident comments. A mandate is due when that digest differs from the
//! last checkpoint's, its heartbeat elapsed, its deadline passed since the
//! last cycle, it never cycled, or its last cycle died; and only once the
//! cadence floor since the last cycle start allows it and no cycle is live.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_types::record::{EffectiveStatus, Notice, NoticeKind, NoticeScope};
use orbit_types::task::{Task, TaskComment, TaskHistoryEntry};
use orbit_types::workflow::{
    RESIDENT_CYCLE_GRANTED_EVENT, ResidentCheckpoint, ResidentComment, ResidentCycleGrant,
    ResidentLiveCycle, ResidentMandate, resident_authority_revision,
};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::cycle::{CYCLE_CHANGED, live_cycle_from_history};
use crate::OrbitRuntime;
use crate::application::notice::{NoticeListFilter, NoticeStatusFilter};

/// Most watched tasks one digest reads.
const MAX_WATCHED_TASKS: usize = 200;

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
    /// `sha256:<hex>` over everything a cycle reacts to.
    pub(crate) digest: String,
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
}

impl ResidentDue {
    /// The scheduler member fingerprint: the last cycle it follows and what
    /// made it due, so each cycle is its own member episode.
    pub(crate) fn fingerprint(&self) -> String {
        let after = self
            .last_cycle
            .as_ref()
            .map_or("none", |cycle| cycle.live.cycle.as_str());
        let why = if self.reasons.iter().any(|reason| reason == "heartbeat") {
            "heartbeat"
        } else {
            self.digest.as_str()
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

impl OrbitRuntime {
    /// Whether `notice` reaches a mandate covering `workspaces`.
    fn notice_in_scope(&self, notice: &Notice, workspaces: &BTreeSet<String>) -> bool {
        notice.scope == NoticeScope::Global
            || workspaces.contains(&notice.workspace.id)
            || workspaces.contains(&notice.workspace.name)
    }

    /// The notices in a mandate's scope (this workspace, the mandate's
    /// workspaces homed on this owner, and global ones), any state.
    fn resident_notices(&self, mandate: &ResidentMandate) -> Result<Vec<Notice>, OrbitError> {
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
    fn resident_watched_tasks(
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

    /// Whether the live cycle's run can no longer act: its lease expired, or
    /// its run is terminal and its owner stopped (or the run is gone).
    pub(crate) fn resident_cycle_dead(&self, live: &ResidentLiveCycle, now: DateTime<Utc>) -> bool {
        if live.expires_at <= now {
            return true;
        }
        match self.show_job_run(&live.run_id) {
            Ok(run) => {
                run.state.is_terminal()
                    && crate::application::job::run_owner_liveness(&run)
                        == crate::application::job::RunOwnerLiveness::Stopped
            }
            Err(_) => true,
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

        let watched = self.resident_watched_tasks(&mandate, last)?;
        let mut decisions = Vec::new();
        for id in
            std::iter::once(task.id.to_string()).chain(watched.iter().map(|t| t.id.to_string()))
        {
            for request in self.get_task_decision_requests(&id)? {
                decisions.push(json!([request.id, request.state]));
            }
        }
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
        let digest = digest_of(&json!({
            "mandate": mandate,
            "authority_revision": resident_authority_revision(history),
            "tasks": watched
                .iter()
                .map(|task| json!([task.id, task.status, task.updated_at]))
                .collect::<Vec<_>>(),
            "decisions": decisions,
            "notices": notices
                .iter()
                .map(|notice| json!([notice.id, notice.revision, notice.status]))
                .collect::<Vec<_>>(),
            "notice_error": notice_error,
            "comments": wake_comments(&comments, &cycles),
        }));

        let mut reasons = Vec::new();
        let mut withheld = None;
        let mut dead_cycle = None;
        if let Some(live) = live {
            if self.resident_cycle_dead(&live, now) {
                reasons.push(format!("recovery:{}", live.cycle));
                dead_cycle = Some(live);
            } else {
                withheld = Some("cycle_live".to_string());
            }
        }
        match (last, &last_cycle) {
            (None, None) => reasons.push("first_cycle".into()),
            (Some(last), _) if last.wake.digest != digest => reasons.push("digest_changed".into()),
            (None, Some(_)) => reasons.push("digest_changed".into()),
            _ => {}
        }
        let since = last_cycle
            .as_ref()
            .map_or(task.created_at, |cycle| cycle.at);
        if now - since >= Duration::minutes(i64::from(mandate.cadence.heartbeat_minutes)) {
            reasons.push("heartbeat".into());
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
            reasons,
            withheld,
            last_cycle,
            dead_cycle,
            episode_start: since,
            maintenance,
            notices,
        })
    }
}
