//! Adopt an owner-local delivery whose leaf died after it opened its pull
//! request.
//!
//! A `task_pr_pipeline` leaf watches the pull request it opened: completion
//! merges it under `--complete`, and `--auto-fix` repairs its failed required
//! checks and conflicts. When the leaf's worker dies after `pr_open` and
//! after the task reached `review`, the orphan reconciler finalizes the run
//! `interrupted` and the task waits in `review` with nothing watching its
//! pull request. Two other cases are someone else's: a leaf that died before
//! promotion leaves its task `blocked` by the interruption, which the
//! blocked-task recovery backstop decides, and a claimed leaf's release keeps
//! its candidate and pull request for the task's next claim.
//!
//! Each owner drain pass resumes such a run through the ordinary checkpoint
//! resume (`orbit job resume`): the resumed run skips every step that
//! succeeded, so landing review, completion and auto-fix continue on the same
//! pull request and the head its checkpoints published. Retry lineage
//! carries the run's repair ledger and authority, and completion's own pins
//! still refuse a head the run did not publish. A run is adopted only when:
//!
//! - it ended within [`ADOPTION_WINDOW`] of its last recorded activity (older
//!   ones are left to `orbit job resume`), not at an upgrade boundary (the
//!   clock resumes those), and is not a claimed execution;
//! - its worker is dead, no retry resumed it yet, no earlier pass decided it
//!   and no earlier run of its retry lineage was adopted: a leaf that keeps
//!   dying is adopted once, then left to an operator;
//! - its `pr_open` succeeded and that pull request is still open on the same
//!   head branch;
//! - every task it carries is still in `review`, checked again just before
//!   the resume is submitted;
//! - the adopting drain holds the authority the run was started with: a run
//!   granted `completion: done` or `auto_fix` needs a drain granted the same;
//! - generic resume accepts it (ownership and lineage checks).
//!
//! A pass adopts nothing while host resource pressure holds admission, and at
//! most as many runs as the drain has free slots.
//!
//! Every final decision, adoption or skip, is audited once under
//! [`ORPHAN_ADOPTION_AUDIT`]. A missing authority, an unreadable store or
//! forge, a resume that could not be planned for a reason other than its
//! validation, or a failed submission is not a decision: a later pass
//! reconsiders the run. No error ends the drain: one run's error defers that
//! run, and an error the whole pass hits is reported as its `pass_error`.

use std::collections::BTreeSet;

use chrono::{DateTime, TimeDelta, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{AuditEventFilter, JobRunOrder, JobRunQuery};
use orbit_types::task::TaskStatus;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::job::{RunOwnerLiveness, run_owner_liveness};
use crate::runtime::run_input::carried_task_ids;

/// Audit tool name of every adoption decision about an interrupted leaf.
pub(crate) const ORPHAN_ADOPTION_AUDIT: &str = "pipeline.run.orphan_adoption";

/// The delivery leaf that opens and watches a pull request.
const DELIVERY_LEAF_JOB: &str = "task_pr_pipeline";

/// How long after its last recorded activity an interrupted leaf is still
/// adopted.
const ADOPTION_WINDOW: TimeDelta = TimeDelta::hours(24);

/// Interrupted delivery leaves read by one pass.
const ADOPTION_SCAN_LIMIT: usize = 64;

/// Retry-lineage ancestors read when looking for an earlier adoption.
const LINEAGE_DEPTH_LIMIT: usize = 64;

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

    /// A store read for one run failed; a later pass reads it again.
    fn unreadable(what: &str, error: &OrbitError) -> Self {
        Self::deferred("store_unreadable", format!("{what}: {error}"))
    }
}

/// What one pass does with one interrupted run.
enum Verdict {
    /// Resume it; its pull request number.
    Adopt(u64),
    /// Not this pass's to consider or report: it has a retry, an earlier pass
    /// decided it, or its worker still runs.
    Ignore,
    Skip(Skip),
}

impl OrbitRuntime {
    /// Resume each interrupted owner-local delivery leaf that still owns an
    /// open pull request, within `authority`. Returns the adopted and skipped
    /// runs, and the pass's own error when it could not read its candidates.
    /// Never fails: the drain step that calls it must not end the drain.
    pub(crate) fn adopt_orphaned_deliveries(
        &self,
        authority: &AdoptionAuthority<'_>,
        now: DateTime<Utc>,
    ) -> Value {
        if self.is_write_free() || authority.budget == 0 {
            return json!({ "adopted": [], "skipped": [] });
        }
        if let Some(throttle) = self.resource_admission().throttle {
            return json!({ "adopted": [], "skipped": [], "resource_throttle": throttle });
        }
        // A worker that died since the last pass still reads as running.
        if let Err(error) = self.reconcile_stale_job_runs(Some(DELIVERY_LEAF_JOB)) {
            tracing::warn!(
                target: "orbit.core.drain",
                error = %error,
                "could not reconcile delivery leaves before adoption; using stored states",
            );
        }
        let interrupted = match self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            job_id: Some(DELIVERY_LEAF_JOB.to_string()),
            state: Some(JobRunState::Interrupted),
            order_by: JobRunOrder::Recency,
            limit: Some(ADOPTION_SCAN_LIMIT),
            include_steps: true,
            ..JobRunQuery::default()
        }) {
            Ok(runs) => runs,
            Err(error) => {
                tracing::warn!(
                    target: "orbit.core.drain",
                    error = %error,
                    "could not list interrupted delivery leaves; this pass adopts nothing",
                );
                return json!({
                    "adopted": [],
                    "skipped": [],
                    "pass_error": format!("list interrupted delivery leaves: {error}"),
                });
            }
        };
        let mut adopted = Vec::new();
        let mut skipped = Vec::new();
        let mut missing_authority = Vec::new();
        for run in interrupted {
            if adopted.len() >= authority.budget {
                break;
            }
            // Outside the window and upgrade interruptions are not this
            // pass's to consider at all.
            if run
                .finished_at
                .is_none_or(|at| now.signed_duration_since(at) > ADOPTION_WINDOW)
                || super::upgrade_resume::upgrade_interrupted(&run)
            {
                continue;
            }
            let verdict = match self.adoption_verdict(&run, authority) {
                Verdict::Adopt(pull_request) => self.submit_adoption(&run, authority, pull_request),
                verdict => verdict,
            };
            match verdict {
                Verdict::Ignore => {}
                Verdict::Adopt(pull_request) => {
                    let (resumed, task_ids) = self.adopted_retry(&run);
                    adopted.push(json!({
                        "source_run_id": run.run_id,
                        "resumed_run_id": resumed,
                        "pr_number": pull_request,
                        "task_ids": task_ids,
                    }));
                }
                Verdict::Skip(skip) => {
                    if skip.reason == "authority" {
                        missing_authority.push(run.run_id.clone());
                    } else if skip.decided {
                        tracing::info!(
                            target: "orbit.core.drain",
                            run_id = %run.run_id,
                            reason = skip.reason,
                            detail = %skip.detail,
                            "drain left an interrupted delivery leaf alone",
                        );
                        self.audit_orphan_adoption(
                            &run,
                            authority.drain_run_id,
                            json!({"decision": "skipped", "reason": skip.reason, "detail": skip.detail}),
                        );
                    } else {
                        tracing::warn!(
                            target: "orbit.core.drain",
                            run_id = %run.run_id,
                            reason = skip.reason,
                            detail = %skip.detail,
                            "drain deferred an interrupted delivery leaf to a later pass",
                        );
                    }
                    skipped.push(json!({
                        "source_run_id": run.run_id,
                        "reason": skip.reason,
                        "detail": skip.detail,
                        "decided": skip.decided,
                    }));
                }
            }
        }
        if !missing_authority.is_empty() {
            tracing::info!(
                target: "orbit.core.drain",
                runs = ?missing_authority,
                "interrupted delivery leaves need a drain started with their --complete or \
                 --auto-fix grant",
            );
        }
        json!({ "adopted": adopted, "skipped": skipped })
    }

    /// Whether `run` is adopted, and if not, why.
    fn adoption_verdict(&self, run: &JobRun, authority: &AdoptionAuthority<'_>) -> Verdict {
        let skip = |skip: Skip| Verdict::Skip(skip);
        match self.stores().jobs().job_run_retries(&run.run_id, 1) {
            Ok(retries) if !retries.is_empty() => return Verdict::Ignore,
            Ok(_) => {}
            Err(error) => return skip(Skip::unreadable("read the run's retries", &error)),
        }
        match self.orphan_adoption_decided(&run.run_id) {
            Ok(true) => return Verdict::Ignore,
            Ok(false) => {}
            Err(error) => return skip(Skip::unreadable("read earlier decisions", &error)),
        }
        if run_owner_liveness(run) == RunOwnerLiveness::Alive {
            return Verdict::Ignore;
        }
        match self.adopted_lineage_ancestor(run) {
            Ok(None) => {}
            Ok(Some(ancestor)) => {
                return skip(Skip::decided(
                    "lineage_adopted",
                    format!(
                        "run {ancestor} of this retry lineage was already adopted once; resume \
                         it with `orbit job resume` once the cause is fixed"
                    ),
                ));
            }
            Err(error) => return skip(Skip::unreadable("read the retry lineage", &error)),
        }
        match self.is_claimed_execution(&run.run_id) {
            Ok(false) => {}
            Ok(true) => {
                return skip(Skip::decided(
                    "claimed_execution",
                    "a claimed leaf's release keeps its pull request for the task's next claim",
                ));
            }
            Err(error) => return skip(Skip::unreadable("read the run's claim", &error)),
        }
        let state = match self.read_run_state(&run.run_id) {
            Ok(Some(state)) => state,
            Ok(None) => {
                return skip(Skip::decided(
                    "no_pull_request",
                    "the run kept no checkpoints",
                ));
            }
            Err(error) => return skip(Skip::unreadable("read the run's checkpoints", &error)),
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
        if let Err(skip) = self.tasks_wait_on_run(&task_ids) {
            return Verdict::Skip(skip);
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
        let repo_root = self.context.paths().repo_root.clone();
        match self.task_pr_forge().open_pull_request(&repo_root, number) {
            Ok(Some(pull)) if pull.head_branch == head => {}
            Ok(_) => {
                return skip(Skip::decided(
                    "pull_request_not_open",
                    format!("pull request #{number} is no longer open on '{head}'"),
                ));
            }
            Err(error) => return skip(Skip::deferred("forge_unreadable", error.to_string())),
        }
        match self.plan_job_run_resume(&run.run_id) {
            Ok(_) => Verdict::Adopt(number),
            // Validation is the planner's answer about this run; anything
            // else (a store read, the catalog, an unsettled claim journal)
            // may answer differently on a later pass.
            Err(error @ OrbitError::JobValidation(_)) => {
                skip(Skip::decided("not_resumable", error.to_string()))
            }
            Err(error) => skip(Skip::deferred("resume_unplannable", error.to_string())),
        }
    }

    /// Submit the resume of `run`, re-checking its tasks first: an operator
    /// or the recovery backstop may have moved one since the verdict.
    fn submit_adoption(
        &self,
        run: &JobRun,
        authority: &AdoptionAuthority<'_>,
        pull_request: u64,
    ) -> Verdict {
        let task_ids = run.input.as_ref().map(carried_task_ids).unwrap_or_default();
        if let Err(skip) = self.tasks_wait_on_run(&task_ids) {
            return Verdict::Skip(skip);
        }
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
                    run,
                    authority.drain_run_id,
                    json!({
                        "decision": "adopted",
                        "resumed_run_id": invoke.run_id,
                        "pr_number": pull_request,
                    }),
                );
                Verdict::Adopt(pull_request)
            }
            // Not a decision: the next pass tries again.
            Err(error) => Verdict::Skip(Skip::deferred("resume_failed", error.to_string())),
        }
    }

    /// The retry an adoption just submitted for `run`, and its tasks.
    fn adopted_retry(&self, run: &JobRun) -> (Value, BTreeSet<String>) {
        let resumed = self
            .stores()
            .jobs()
            .job_run_retries(&run.run_id, 1)
            .ok()
            .and_then(|retries| retries.into_iter().next())
            .map_or(Value::Null, |retry| Value::String(retry.run_id));
        let task_ids = run.input.as_ref().map(carried_task_ids).unwrap_or_default();
        (resumed, task_ids)
    }

    /// `Err` with the skip when one of `task_ids` no longer waits on its
    /// interrupted run in `review`. A task that no longer exists has moved
    /// on for good; any other unreadable task defers the run.
    fn tasks_wait_on_run(&self, task_ids: &BTreeSet<String>) -> Result<(), Skip> {
        for task_id in task_ids {
            let task = match self.get_task(task_id) {
                Ok(task) => task,
                Err(OrbitError::NotFound { .. }) => {
                    return Err(Skip::decided(
                        "task_moved",
                        format!("task {task_id} no longer exists"),
                    ));
                }
                Err(error) => return Err(Skip::unreadable("read the run's task", &error)),
            };
            if task.status != TaskStatus::Review {
                return Err(Skip::decided(
                    "task_moved",
                    format!(
                        "task {task_id} is {} and no longer waits on the run",
                        task.status
                    ),
                ));
            }
        }
        Ok(())
    }

    /// The earliest run of `run`'s retry lineage an earlier pass adopted, if
    /// any: a leaf that died again after its adoption is not adopted twice.
    fn adopted_lineage_ancestor(&self, run: &JobRun) -> Result<Option<String>, OrbitError> {
        let mut source = run.retry_source_run_id.clone();
        for _ in 0..LINEAGE_DEPTH_LIMIT {
            let Some(ancestor) = source else {
                return Ok(None);
            };
            if self.orphan_adoption_adopted(&ancestor)? {
                return Ok(Some(ancestor));
            }
            source = self
                .stores()
                .jobs()
                .get_job_run(&ancestor)?
                .and_then(|run| run.retry_source_run_id);
        }
        Ok(None)
    }

    /// Whether an earlier pass already adopted or skipped `run_id`.
    fn orphan_adoption_decided(&self, run_id: &str) -> Result<bool, OrbitError> {
        Ok(!self.orphan_adoption_decisions(run_id, 1)?.is_empty())
    }

    /// Whether an earlier pass adopted `run_id`.
    fn orphan_adoption_adopted(&self, run_id: &str) -> Result<bool, OrbitError> {
        Ok(self
            .orphan_adoption_decisions(run_id, 8)?
            .iter()
            .any(|decision| decision["decision"] == "adopted"))
    }

    fn orphan_adoption_decisions(
        &self,
        run_id: &str,
        limit: usize,
    ) -> Result<Vec<Value>, OrbitError> {
        let decisions = self
            .stores()
            .audit_events()
            .list_audit_events(&AuditEventFilter {
                tool_name: Some(ORPHAN_ADOPTION_AUDIT.to_string()),
                status: Some(AuditEventStatus::Success),
                job_run_id: Some(run_id.to_string()),
                limit,
                ..AuditEventFilter::default()
            })?;
        Ok(decisions
            .iter()
            .map(|event| {
                event
                    .arguments_json
                    .as_deref()
                    .and_then(|raw| serde_json::from_str(raw).ok())
                    .unwrap_or(Value::Null)
            })
            .collect())
    }

    fn audit_orphan_adoption(&self, run: &JobRun, drain_run_id: &str, decision: Value) {
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
                AuditEventStatus::Success,
                arguments,
                None,
            ),
        );
    }
}
