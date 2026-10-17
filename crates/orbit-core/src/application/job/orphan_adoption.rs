//! Adopt an owner-local delivery whose leaf died after it opened its pull
//! request.
//!
//! A `task_pr_pipeline` leaf watches the pull request it opened: completion
//! merges it under `--complete`, and `--auto-fix` repairs its failed required
//! checks and conflicts. When the leaf's worker dies after `pr_open`, the
//! orphan reconciler finalizes the run `interrupted`, and the task waits in
//! `review` (or `blocked` with `workflow_run_interrupted`, when it died before
//! promotion) with nothing watching its pull request. A claimed leaf needs
//! none of this: its claim's release keeps the candidate and pull request,
//! and the task's next claim continues them.
//!
//! Each owner drain pass resumes such a run once, through the ordinary
//! checkpoint resume (`orbit job resume`): the resumed run skips every step
//! that succeeded, so landing review, completion and auto-fix continue on the
//! same pull request and the head its checkpoints published. Retry lineage
//! carries the run's repair ledger and authority, and completion's own pins
//! still refuse a head the run did not publish. A run is adopted only when:
//!
//! - it was interrupted within [`ADOPTION_WINDOW`] (older ones are left to
//!   `orbit job resume`), not by an upgrade (the clock resumes those) and is
//!   not a claimed execution;
//! - its worker is dead, no retry resumed it yet and no earlier pass decided
//!   it;
//! - its `pr_open` succeeded and that pull request is still open on the same
//!   head branch;
//! - every task it carries still waits on it: in `review`, or `blocked` by a
//!   run interruption;
//! - the adopting drain holds the authority the run was started with: a run
//!   granted `completion: done` or `auto_fix` needs a drain granted the same;
//! - generic resume accepts it (ownership and lineage checks).
//!
//! Every final decision, adoption or skip, is audited once under
//! [`ORPHAN_ADOPTION_AUDIT`]. A missing authority, an unreadable forge or a
//! failed submission is not a decision: a later pass reconsiders the run.

use std::collections::BTreeSet;

use chrono::{DateTime, TimeDelta, Utc};
use orbit_common::OrbitError;
use orbit_engine::WORKFLOW_RUN_INTERRUPTED_EVENT;
use orbit_store::contracts::{AuditEventFilter, JobRunOrder, JobRunQuery};
use orbit_types::task::TaskStatus;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::job::{RunOwnerLiveness, run_owner_liveness};
use crate::runtime::run_input::carried_task_ids;
use crate::runtime::task_pr_forge::ForgePullRequest;

/// Audit tool name of every adoption decision about an interrupted leaf.
pub(crate) const ORPHAN_ADOPTION_AUDIT: &str = "pipeline.run.orphan_adoption";

/// The delivery leaf that opens and watches a pull request.
const DELIVERY_LEAF_JOB: &str = "task_pr_pipeline";

/// How long after its interruption a leaf is still adopted.
const ADOPTION_WINDOW: TimeDelta = TimeDelta::hours(24);

/// Interrupted delivery leaves read by one pass.
const ADOPTION_SCAN_LIMIT: usize = 64;

/// The authority the adopting drain was started with.
pub(crate) struct AdoptionAuthority<'a> {
    /// The drain run, recorded as the resume's actor.
    pub(crate) drain_run_id: &'a str,
    pub(crate) completion_done: bool,
    pub(crate) auto_fix: bool,
    /// Free leaf slots: at most this many runs are resumed.
    pub(crate) budget: usize,
}

/// Why a run is left alone, and whether that is final.
struct Skip {
    reason: &'static str,
    detail: String,
    decided: bool,
}

impl Skip {
    fn decided(reason: &'static str, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
            decided: true,
        }
    }

    fn deferred(reason: &'static str, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
            decided: false,
        }
    }
}

/// The open pull requests, read at most once per pass.
enum OpenPulls {
    Unread,
    Read(Vec<ForgePullRequest>),
    Failed(String),
}

impl OrbitRuntime {
    /// Resume each interrupted owner-local delivery leaf that still owns an
    /// open pull request, within `authority`. Returns the adopted and skipped
    /// runs.
    pub(crate) fn adopt_orphaned_deliveries(
        &self,
        authority: &AdoptionAuthority<'_>,
        now: DateTime<Utc>,
    ) -> Result<Value, OrbitError> {
        let mut adopted = Vec::new();
        let mut skipped = Vec::new();
        if self.is_write_free() || authority.budget == 0 {
            return Ok(json!({ "adopted": adopted, "skipped": skipped }));
        }
        // A worker that died since the last pass still reads as running.
        self.reconcile_stale_job_runs(Some(DELIVERY_LEAF_JOB))?;
        let interrupted = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            job_id: Some(DELIVERY_LEAF_JOB.to_string()),
            state: Some(JobRunState::Interrupted),
            order_by: JobRunOrder::Recency,
            limit: Some(ADOPTION_SCAN_LIMIT),
            include_steps: true,
            ..JobRunQuery::default()
        })?;
        let mut open = OpenPulls::Unread;
        for run in interrupted {
            if adopted.len() >= authority.budget {
                break;
            }
            // Outside the window, upgrade interruptions and decided runs are
            // not this pass's to consider at all.
            if run
                .finished_at
                .is_none_or(|at| now.signed_duration_since(at) > ADOPTION_WINDOW)
                || super::upgrade_resume::upgrade_interrupted(&run)
                || !self
                    .stores()
                    .jobs()
                    .job_run_retries(&run.run_id, 1)?
                    .is_empty()
                || self.orphan_adoption_decided(&run.run_id)?
                || run_owner_liveness(&run) == RunOwnerLiveness::Alive
            {
                continue;
            }
            let pull_request = match self.adoption_skip(&run, authority, &mut open)? {
                Ok(pull_request) => pull_request,
                Err(skip) => {
                    tracing::info!(
                        target: "orbit.core.drain",
                        run_id = %run.run_id,
                        reason = skip.reason,
                        detail = %skip.detail,
                        "drain left an interrupted delivery leaf alone",
                    );
                    if skip.decided {
                        self.audit_orphan_adoption(
                            &run,
                            authority.drain_run_id,
                            AuditEventStatus::Success,
                            json!({"decision": "skipped", "reason": skip.reason, "detail": skip.detail}),
                        );
                    }
                    skipped.push(json!({
                        "source_run_id": run.run_id,
                        "reason": skip.reason,
                        "detail": skip.detail,
                        "decided": skip.decided,
                    }));
                    continue;
                }
            };
            match self.submit_resume_run(&run.run_id, Some(authority.drain_run_id), None) {
                Ok(invoke) => {
                    tracing::info!(
                        target: "orbit.core.drain",
                        source_run_id = %run.run_id,
                        resumed_run_id = %invoke.run_id,
                        pr = pull_request,
                        "drain adopted the pull request of an interrupted delivery leaf",
                    );
                    self.audit_orphan_adoption(
                        &run,
                        authority.drain_run_id,
                        AuditEventStatus::Success,
                        json!({
                            "decision": "adopted",
                            "resumed_run_id": invoke.run_id,
                            "pr_number": pull_request,
                        }),
                    );
                    adopted.push(json!({
                        "source_run_id": run.run_id,
                        "resumed_run_id": invoke.run_id,
                        "pr_number": pull_request,
                        "task_ids": run.input.as_ref().map(carried_task_ids).unwrap_or_default(),
                    }));
                }
                // Not a decision: the next pass tries again.
                Err(error) => {
                    tracing::warn!(
                        target: "orbit.core.drain",
                        source_run_id = %run.run_id,
                        error = %error,
                        "failed to resume an interrupted delivery leaf",
                    );
                    skipped.push(json!({
                        "source_run_id": run.run_id,
                        "reason": "resume_failed",
                        "detail": error.to_string(),
                        "decided": false,
                    }));
                }
            }
        }
        Ok(json!({ "adopted": adopted, "skipped": skipped }))
    }

    /// The open pull request `run` still owns, or why it is not adopted.
    fn adoption_skip(
        &self,
        run: &JobRun,
        authority: &AdoptionAuthority<'_>,
        open: &mut OpenPulls,
    ) -> Result<Result<u64, Skip>, OrbitError> {
        let skip = |skip: Skip| Ok(Err(skip));
        match self.is_claimed_execution(&run.run_id) {
            Ok(false) => {}
            Ok(true) => {
                return skip(Skip::decided(
                    "claimed_execution",
                    "a claimed leaf's release keeps its pull request for the task's next claim",
                ));
            }
            Err(error) => return skip(Skip::deferred("claim_unreadable", error.to_string())),
        }
        let Some(state) = self.read_run_state(&run.run_id)? else {
            return skip(Skip::decided(
                "no_pull_request",
                "the run kept no checkpoints",
            ));
        };
        let opened = state.pipeline.get("pr_open").unwrap_or(&Value::Null);
        let (Some(number), Some(head)) = (
            opened
                .get("pr_number")
                .and_then(Value::as_str)
                .and_then(|number| number.parse::<u64>().ok()),
            opened.get("head").and_then(Value::as_str),
        ) else {
            return skip(Skip::decided(
                "no_pull_request",
                "the run was interrupted before it opened a pull request",
            ));
        };
        let input = run.input.clone().unwrap_or(Value::Null);
        let task_ids = carried_task_ids(&input);
        if task_ids.is_empty() {
            return skip(Skip::decided("no_task", "the run carries no task"));
        }
        if let Some(moved) = self.tasks_not_waiting_on(&task_ids)? {
            return skip(Skip::decided("task_moved", moved));
        }
        let wants_done = input.get("completion").and_then(Value::as_str) == Some("done");
        let wants_fix = input.get("auto_fix").and_then(Value::as_bool) == Some(true);
        if (wants_done && !authority.completion_done) || (wants_fix && !authority.auto_fix) {
            return skip(Skip::deferred(
                "authority",
                format!(
                    "the run was started with{}{}; this drain was not",
                    if wants_done { " --complete" } else { "" },
                    if wants_fix { " --auto-fix" } else { "" },
                ),
            ));
        }
        if matches!(open, OpenPulls::Unread) {
            let repo_root = self.context.paths().repo_root.clone();
            *open = match self.task_pr_forge().open_pull_requests(&repo_root) {
                Ok(pulls) => OpenPulls::Read(pulls),
                Err(error) => OpenPulls::Failed(error.to_string()),
            };
        }
        match open {
            OpenPulls::Read(pulls) => {
                if !pulls
                    .iter()
                    .any(|pull| pull.number == number && pull.head_branch == head)
                {
                    return skip(Skip::decided(
                        "pull_request_not_open",
                        format!("pull request #{number} is no longer open on '{head}'"),
                    ));
                }
            }
            OpenPulls::Failed(error) => {
                return skip(Skip::deferred("forge_unreadable", error.clone()));
            }
            OpenPulls::Unread => {}
        }
        if let Err(error) = self.plan_job_run_resume(&run.run_id) {
            return skip(Skip::decided("not_resumable", error.to_string()));
        }
        Ok(Ok(number))
    }

    /// Why one of `task_ids` no longer waits on its interrupted run, if one
    /// does not.
    fn tasks_not_waiting_on(
        &self,
        task_ids: &BTreeSet<String>,
    ) -> Result<Option<String>, OrbitError> {
        for task_id in task_ids {
            let task = self.get_task(task_id)?;
            let waiting = match task.status {
                TaskStatus::Review => true,
                TaskStatus::Blocked => self
                    .get_task_history(task_id)?
                    .iter()
                    .rev()
                    .find(|entry| entry.to_status.is_some())
                    .is_some_and(|entry| entry.event == WORKFLOW_RUN_INTERRUPTED_EVENT),
                _ => false,
            };
            if !waiting {
                return Ok(Some(format!(
                    "task {task_id} is {} and no longer waits on the run",
                    task.status
                )));
            }
        }
        Ok(None)
    }

    /// Whether an earlier pass already adopted or skipped `run_id`.
    fn orphan_adoption_decided(&self, run_id: &str) -> Result<bool, OrbitError> {
        let decisions = self
            .stores()
            .audit_events()
            .list_audit_events(&AuditEventFilter {
                tool_name: Some(ORPHAN_ADOPTION_AUDIT.to_string()),
                status: Some(AuditEventStatus::Success),
                job_run_id: Some(run_id.to_string()),
                limit: 1,
                ..AuditEventFilter::default()
            })?;
        Ok(!decisions.is_empty())
    }

    fn audit_orphan_adoption(
        &self,
        run: &JobRun,
        drain_run_id: &str,
        status: AuditEventStatus,
        decision: Value,
    ) {
        let mut arguments = json!({
            "run_id": run.run_id,
            "job_id": run.job_id,
            "drain_run_id": drain_run_id,
            "interrupted_at": run.finished_at.map(|at| at.to_rfc3339()),
        });
        if let (Some(arguments), Value::Object(decision)) = (arguments.as_object_mut(), decision) {
            arguments.extend(decision);
        }
        super::log_best_effort(
            "record orphan adoption audit",
            &run.run_id,
            self.record_pipeline_audit(
                ORPHAN_ADOPTION_AUDIT,
                Some(&run.run_id),
                Some(drain_run_id),
                status,
                arguments,
                None,
            ),
        );
    }
}
