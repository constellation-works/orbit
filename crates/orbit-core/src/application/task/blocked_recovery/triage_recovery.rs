//! The mechanical recoveries a blocked-task triager applies itself, instead
//! of reporting them as a human's to-do:
//!
//! - **resume_run**: resume the episode's own failed run from its
//!   checkpoints through [`OrbitRuntime::submit_resume_run`], the path
//!   `orbit job resume` takes. Each resume counts within the
//!   [`FinalRecoveryRequeueBound`] final recovery and the triage requeue
//!   share.
//! - **rerun_ci**: re-request the checks that failed on the task's pull
//!   request at the head the triager inspected, through the runtime's
//!   [`TaskPrForge`](crate::runtime::task_pr_forge::TaskPrForge), then requeue
//!   the task within that bound so its next run continues the publication. A
//!   failure the base-red path already owns is left to it.
//! - **friction_reported**: link an existing friction record, or file one
//!   during the task, and record its id on the episode. The task stays
//!   blocked for the human action.
//!
//! Every precondition is checked before the recovery acts, and a refusal is
//! an error that changes nothing and records nothing. The caller holds the
//! task write lock and has already confirmed the episode awaits triage.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::governance::friction::normalize_title;
use orbit_store::contracts::FrictionAddParams;
use orbit_types::record::FrictionStatus;
use orbit_types::task::{Task, TaskHistoryEntry};
use serde_json::{Value, json};

use super::super::final_recovery::FinalRecoveryRequeueBound;
use super::super::helpers::SYSTEM_ACTOR_LABEL;
use super::triage::{
    BlockTriageDecision, BlockTriageRequest, TriageEpisode, TriageFriction, TriageWrite,
    operator_only_requeue, quote, requeue_summary, triage_header,
};
use crate::OrbitRuntime;

/// History event a triage resume appends; the requeue bound counts it with
/// final-recovery requeues.
pub const BLOCKED_TASK_TRIAGE_RESUMED_EVENT: &str = "blocked_task_triage_resumed";

/// History events whose `failed_head` or `head` note field marks a head's
/// failure as inherited from the base, which the base-red path owns.
const BASE_RED_EVENTS: [(&str, &str); 2] = [
    ("pr_auto_fix_base_red", "failed_head"),
    ("ci_base_suspect", "head"),
];

/// A refusal: nothing changed and nothing was recorded.
fn refused(task: &Task, disposition: &str, reason: impl std::fmt::Display) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "{disposition} refused for {}: {reason}; nothing was changed or recorded",
        task.id
    ))
}

impl OrbitRuntime {
    /// Apply `decision`, one of the mechanical recoveries, to `task`'s
    /// qualifying `episode`, and return what the triage writes. Every
    /// refusal is an error that leaves the task as it was.
    pub(super) fn apply_triage_recovery(
        &self,
        task: &Task,
        episode: &TriageEpisode,
        blocker: &str,
        decision: &BlockTriageDecision,
        request: &BlockTriageRequest,
    ) -> Result<TriageWrite, OrbitError> {
        match decision {
            BlockTriageDecision::ResumeRun { run_id, reason } => {
                self.triage_resume_run(task, episode, blocker, run_id, reason, request)
            }
            BlockTriageDecision::RerunCi { head_sha, reason } => {
                self.triage_rerun_ci(task, episode, blocker, head_sha, reason, request)
            }
            BlockTriageDecision::FrictionReported {
                friction,
                human_action,
            } => self.triage_friction(task, episode, blocker, friction, human_action, request),
            BlockTriageDecision::Requeue { .. }
            | BlockTriageDecision::Propose { .. }
            | BlockTriageDecision::Report { .. } => Err(OrbitError::Execution(
                "only a mechanical recovery is applied".into(),
            )),
        }
    }

    /// The requeue bound's count before one more requeue or resume, refused
    /// when unreadable (fail closed) or exhausted.
    fn triage_bound(
        &self,
        task: &Task,
        disposition: &str,
    ) -> Result<(usize, FinalRecoveryRequeueBound), OrbitError> {
        let bound = FinalRecoveryRequeueBound::default();
        let recent = self
            .recent_requeues(&task.id, bound.window)
            .map_err(|error| {
                refused(
                    task,
                    disposition,
                    format!("the shared requeue count cannot be read ({error})"),
                )
            })?;
        if recent >= bound.max_requeues {
            return Err(refused(
                task,
                disposition,
                format!(
                    "the task was already requeued or resumed {recent} time(s) in the last {} \
                     hour(s) (limit {} per {} hour(s), shared with final recovery)",
                    bound.window.num_hours(),
                    bound.max_requeues,
                    bound.window.num_hours()
                ),
            ));
        }
        Ok((recent, bound))
    }

    fn triage_resume_run(
        &self,
        task: &Task,
        episode: &TriageEpisode,
        blocker: &str,
        run_id: &str,
        reason: &str,
        request: &BlockTriageRequest,
    ) -> Result<TriageWrite, OrbitError> {
        const DISPOSITION: &str = "resume_run";
        let Some(episode_run) = episode.run_id.as_deref() else {
            return Err(refused(task, DISPOSITION, "the episode names no run"));
        };
        if run_id != episode_run {
            return Err(refused(
                task,
                DISPOSITION,
                format!("run {run_id} is not the episode's run {episode_run}"),
            ));
        }
        if let Some(reason) = operator_only_requeue(self, task) {
            return Err(refused(task, DISPOSITION, reason));
        }
        let (recent, bound) = self.triage_bound(task, DISPOSITION)?;
        // The resume path validates resumability before it inserts a run or
        // readmits the task, so its refusal changes nothing either.
        let invoke = self
            .submit_resume_run(run_id, Some(&request.requested_by), None)
            .map_err(|error| refused(task, DISPOSITION, error))?;
        let count = recent + 1;
        let summary = requeue_summary(count, bound);
        Ok(TriageWrite {
            disposition: DISPOSITION,
            outcome: "resumed",
            comment: format!(
                "{} {summary}\nblocker:\n{}\nreason: {reason}\nresumed_run_id: {}",
                triage_header(episode, request, DISPOSITION, "resumed"),
                quote(blocker),
                invoke.run_id
            ),
            history: vec![history_entry(
                BLOCKED_TASK_TRIAGE_RESUMED_EVENT,
                format!("source_run_id={run_id} resumed_run_id={}", invoke.run_id),
            )],
            readmitted: true,
            applied: Some(json!({
                "source_run_id": run_id,
                "resumed_run_id": invoke.run_id,
                "count": count,
                "max_requeues": bound.max_requeues,
                "window_hours": bound.window.num_hours(),
                "summary": summary,
            })),
            ..Default::default()
        })
    }

    fn triage_rerun_ci(
        &self,
        task: &Task,
        episode: &TriageEpisode,
        blocker: &str,
        head_sha: &str,
        reason: &str,
        request: &BlockTriageRequest,
    ) -> Result<TriageWrite, OrbitError> {
        const DISPOSITION: &str = "rerun_ci";
        // The rerun ends in a requeue, so the requeue's own preconditions
        // hold first.
        if let Some(reason) = operator_only_requeue(self, task) {
            return Err(refused(task, DISPOSITION, reason));
        }
        if let Some(reason) = self.unsettled_run(task)? {
            return Err(refused(task, DISPOSITION, reason));
        }
        let (recent, bound) = self.triage_bound(task, DISPOSITION)?;
        let number = task
            .github_pr_number()
            .and_then(|number| number.parse::<u64>().ok())
            .ok_or_else(|| refused(task, DISPOSITION, "the task records no pull request"))?;
        let repo_root = &self.paths().repo_root;
        let forge = self.task_pr_forge();
        let blockers = forge
            .pull_request_blockers(repo_root, number)
            .map_err(|error| {
                refused(
                    task,
                    DISPOSITION,
                    format!("the checks of pull request #{number} cannot be read ({error})"),
                )
            })?
            .ok_or_else(|| {
                refused(
                    task,
                    DISPOSITION,
                    format!("pull request #{number} is not open, or the forge cannot report it"),
                )
            })?;
        if blockers.head_sha != head_sha {
            return Err(refused(
                task,
                DISPOSITION,
                format!(
                    "pull request #{number} moved to head {}, not the inspected head {head_sha}",
                    blockers.head_sha
                ),
            ));
        }
        if blockers.failed_required_checks.is_empty() {
            return Err(refused(
                task,
                DISPOSITION,
                format!("no required check failed on head {head_sha} of #{number}"),
            ));
        }
        if let Some(event) = base_red_owner(&self.get_task_history(&task.id)?, head_sha) {
            return Err(refused(
                task,
                DISPOSITION,
                format!(
                    "`{event}` records head {head_sha}'s failure as inherited from the base, \
                     which the base-red path resolves"
                ),
            ));
        }
        let rerun = forge
            .rerun_failed_checks(repo_root, number, head_sha)
            .map_err(|error| refused(task, DISPOSITION, error))?;
        let count = recent + 1;
        let failed = blockers.failed_required_checks.join(", ");
        let requested = rerun
            .requested
            .iter()
            .map(|(id, name)| format!("{name} (run {id})"))
            .collect::<Vec<_>>()
            .join(", ");
        let uncertain = rerun
            .uncertain
            .as_ref()
            .map(|(id, name, error)| format!("{name} (run {id}): {error}"));
        let detail = match &uncertain {
            Some(error) => {
                format!("request outcome uncertain: {error}; inspect the workflow before retrying")
            }
            None => "all requests acknowledged".to_string(),
        };
        Ok(TriageWrite {
            disposition: DISPOSITION,
            outcome: "requeued",
            comment: format!(
                "{} {}\nblocker:\n{}\nrepair: requested reruns for failed checks ({failed}) on head \
                 {head_sha} of pull request #{number}; {reason}\nacknowledged: {requested}\n{detail}",
                triage_header(episode, request, DISPOSITION, "requeued"),
                requeue_summary(count, bound),
                quote(blocker),
            ),
            requeue: Some((count, bound)),
            applied: Some(json!({
                "pr_number": number,
                "head_sha": head_sha,
                "failed_required_checks": blockers.failed_required_checks,
                "rerun": rerun.requested.iter().map(|(_, name)| name).collect::<Vec<_>>(),
                "requests": rerun.requested.iter().map(|(id, name)| json!({"run_id": id, "name": name})).collect::<Vec<_>>(),
                "uncertain": rerun.uncertain.as_ref().map(|(id, name, error)| json!({"run_id": id, "name": name, "error": error})),
            })),
            ..Default::default()
        })
    }

    fn triage_friction(
        &self,
        task: &Task,
        episode: &TriageEpisode,
        blocker: &str,
        friction: &TriageFriction,
        human_action: &str,
        request: &BlockTriageRequest,
    ) -> Result<TriageWrite, OrbitError> {
        const DISPOSITION: &str = "friction_reported";
        let store = crate::runtime::friction::store_for(self)?;
        let (record, filed) = match friction {
            TriageFriction::Existing(id) => (
                store
                    .show(id)?
                    .ok_or_else(|| {
                        refused(task, DISPOSITION, format!("no friction record {id} exists"))
                    })?
                    .record,
                false,
            ),
            TriageFriction::File { title, body, model } => (
                store
                    .add(FrictionAddParams {
                        model: model.clone(),
                        title: Some(normalize_title(title)?),
                        body: body.clone(),
                        tags: Vec::new(),
                        status: FrictionStatus::Open,
                        during_task: Some(task.id.clone()),
                        created_at: Utc::now(),
                    })?
                    .record,
                true,
            ),
        };
        let title = record.title.as_deref().unwrap_or("untitled");
        Ok(TriageWrite {
            disposition: DISPOSITION,
            outcome: "reported",
            comment: format!(
                "{}\nblocker:\n{}\nfriction: {} ({title}){}\nhuman_action: {human_action}",
                triage_header(episode, request, DISPOSITION, "reported"),
                quote(blocker),
                record.id,
                if filed { ", filed by this triage" } else { "" }
            ),
            marker_fields: format!(" friction={}", record.id),
            applied: Some(json!({
                "friction_id": record.id,
                "filed": filed,
            })),
            ..Default::default()
        })
    }
}

/// A system history entry with `note`, stamped now.
fn history_entry(event: &str, note: String) -> TaskHistoryEntry {
    TaskHistoryEntry {
        at: Utc::now(),
        by: SYSTEM_ACTOR_LABEL.to_string(),
        event: event.to_string(),
        note: Some(note),
        from_status: None,
        to_status: None,
        operation_id: None,
        decision: None,
    }
}

/// The base-red event in `history` that records `head`'s failure as
/// inherited from the base, if any.
fn base_red_owner(history: &[TaskHistoryEntry], head: &str) -> Option<&'static str> {
    BASE_RED_EVENTS.iter().find_map(|(event, field)| {
        history
            .iter()
            .any(|entry| {
                entry.event == *event
                    && entry
                        .note
                        .as_deref()
                        .and_then(|note| serde_json::from_str::<Value>(note).ok())
                        .is_some_and(|note| note[*field] == head)
            })
            .then_some(*event)
    })
}
