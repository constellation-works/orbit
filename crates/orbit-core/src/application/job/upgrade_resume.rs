//! [ORB-14273] Clock auto-resume of runs an Orbit upgrade interrupted.
//!
//! An upgrade interrupts a pipeline worker at a step boundary
//! (`upgrade_quiesce`) or refuses its startup (`upgrade admission refused`).
//! Once the generation settles, the clock tick resumes such a run at most
//! once — but only a run the *current* upgrade interrupted, and only while
//! resuming it still means what it meant when it was interrupted.
//!
//! [ORB-14320] Every tick scans every interrupted run of the workspace, so
//! without those bounds the first deploy replayed the whole history of
//! upgrade interruptions: a drain whose window had closed weeks earlier
//! admitted new leaves under its old flags, and a routine run re-read its
//! long-dead child. A run is therefore skipped when:
//!
//! - it was interrupted more than [`UPGRADE_RESUME_WINDOW`] ago, which an
//!   interruption by the upgrade that just settled never is;
//! - it is a claimed execution, which only the owner's claim recovery may
//!   re-admit;
//! - it is a drain (or the ship wrapper around one) whose admissions were
//!   stopped or whose window has elapsed — a resumed drain's first pass
//!   admits before it re-reads the window;
//! - a newer run of the same drain, or of the same routine, superseded it;
//! - generic resume refuses it.
//!
//! Each run is decided once. A resume leaves a retry descendant; every
//! decision, resume or skip, leaves an [`UPGRADE_RESUME_AUDIT`] record naming
//! its reason, and a later tick does not reconsider a decided run.

use chrono::{DateTime, TimeDelta, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{AuditEventFilter, JobRunQuery};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{JobRun, JobRunState, PipelineState};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::job::{RunOwnerLiveness, run_owner_liveness};
use crate::runtime::upgrade_handover::UPGRADE_QUIESCE_ERROR_CODE;

/// Audit tool name of every auto-resume decision about an interrupted run.
pub(crate) const UPGRADE_RESUME_AUDIT: &str = "pipeline.run.upgrade_resume";

/// How long after its interruption a run still belongs to the current
/// upgrade. A worker yields within the quiesce bound and the clock resumes it
/// on the first tick after the generation settles, so an older interruption
/// is from an earlier upgrade; `orbit job resume` still continues it by hand.
const UPGRADE_RESUME_WINDOW: TimeDelta = TimeDelta::minutes(30);

/// Drain coordinators, and the ship wrapper whose window is its drain's.
const DRAIN_JOBS: [&str; 3] = [
    "workspace_auto_pipeline",
    "workspace_pull_pipeline",
    "workspace_ship_pipeline",
];

/// Step whose output stamps a drain's admission deadline (`drain_window`).
const DRAIN_WINDOW_STEP: &str = "open_window";

/// Newest runs of a job scanned for one that supersedes the candidate.
const SUPERSEDE_SCAN_LIMIT: usize = 50;

const CLOCK_ACTOR: &str = "clock";

/// Why the clock leaves an upgrade-interrupted run alone.
struct Skip {
    reason: &'static str,
    detail: String,
}

impl Skip {
    fn new(reason: &'static str, detail: impl Into<String>) -> Option<Self> {
        Some(Self {
            reason,
            detail: detail.into(),
        })
    }
}

impl OrbitRuntime {
    /// Resume each run the current upgrade interrupted, once; audit and skip
    /// the rest. Returns the ids of the resumed runs.
    pub(crate) fn auto_resume_upgrade_interrupted_runs(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<String>, OrbitError> {
        if self.is_write_free() {
            return Ok(Vec::new());
        }
        let query = JobRunQuery {
            state: Some(JobRunState::Interrupted),
            include_steps: true,
            ..JobRunQuery::default()
        };
        let interrupted = self.stores().jobs().list_job_runs_filtered(&query)?;
        let mut resumed = Vec::new();

        for run in interrupted {
            if !upgrade_interrupted(&run)
                || !self
                    .stores()
                    .jobs()
                    .job_run_retries(&run.run_id, 1)?
                    .is_empty()
                || self.upgrade_resume_decided(&run.run_id)?
            {
                continue;
            }
            // A live worker is a passing condition, not a decision; checked first
            // because generic resume refuses it, which would read as a skip.
            if run_owner_liveness(&run) == RunOwnerLiveness::Alive {
                tracing::debug!(
                    target: "orbit.core.sweep",
                    run_id = %run.run_id,
                    "skipping upgrade-interrupted run whose worker process is still alive",
                );
                continue;
            }
            if let Some(skip) = self.upgrade_resume_skip(&run, now)? {
                tracing::info!(
                    target: "orbit.core.sweep",
                    run_id = %run.run_id,
                    reason = skip.reason,
                    detail = %skip.detail,
                    "clock tick skipped upgrade-interrupted run",
                );
                self.audit_upgrade_resume(
                    &run,
                    AuditEventStatus::Success,
                    json!({
                        "decision": "skipped",
                        "reason": skip.reason,
                        "detail": skip.detail,
                    }),
                );
                continue;
            }

            match self.submit_resume_run(&run.run_id, Some(CLOCK_ACTOR), None) {
                Ok(invoke) => {
                    tracing::info!(
                        target: "orbit.core.sweep",
                        source_run_id = %run.run_id,
                        resumed_run_id = %invoke.run_id,
                        "clock tick resumed upgrade-interrupted run",
                    );
                    self.audit_upgrade_resume(
                        &run,
                        AuditEventStatus::Success,
                        json!({
                            "decision": "resumed",
                            "reason": "interrupted_by_current_upgrade",
                            "resumed_run_id": invoke.run_id,
                        }),
                    );
                    resumed.push(invoke.run_id);
                }
                // Not a decision: the next tick inside the window retries.
                Err(error) => {
                    tracing::warn!(
                        target: "orbit.core.sweep",
                        source_run_id = %run.run_id,
                        error = %error,
                        "failed to resume upgrade-interrupted run",
                    );
                    self.audit_upgrade_resume(
                        &run,
                        AuditEventStatus::Failure,
                        json!({
                            "decision": "resume_failed",
                            "error": error.to_string(),
                        }),
                    );
                }
            }
        }

        Ok(resumed)
    }

    /// Whether an earlier tick already resumed or skipped `run_id`.
    fn upgrade_resume_decided(&self, run_id: &str) -> Result<bool, OrbitError> {
        let decisions = self
            .stores()
            .audit_events()
            .list_audit_events(&AuditEventFilter {
                tool_name: Some(UPGRADE_RESUME_AUDIT.to_string()),
                status: Some(AuditEventStatus::Success),
                job_run_id: Some(run_id.to_string()),
                limit: 1,
                ..AuditEventFilter::default()
            })?;
        Ok(!decisions.is_empty())
    }

    fn upgrade_resume_skip(
        &self,
        run: &JobRun,
        now: DateTime<Utc>,
    ) -> Result<Option<Skip>, OrbitError> {
        match run.finished_at {
            Some(at) if now.signed_duration_since(at) <= UPGRADE_RESUME_WINDOW => {}
            at => {
                return Ok(Skip::new(
                    "interrupted_before_current_upgrade",
                    format!(
                        "interrupted at {}; only a run interrupted within the last {} minutes \
                         belongs to the upgrade that just settled",
                        at.map_or_else(|| "an unrecorded time".to_string(), |at| at.to_rfc3339()),
                        UPGRADE_RESUME_WINDOW.num_minutes(),
                    ),
                ));
            }
        }
        if self.is_claimed_execution(&run.run_id)? {
            return Ok(Skip::new(
                "claimed_execution",
                "a claimed execution is re-admitted by the owner's claim recovery",
            ));
        }

        if let Some(detail) =
            crate::application::review::upgrade_resume_admission_mismatch(self, run)
        {
            return Ok(Skip::new("review_admission_changed", detail));
        }

        let state = self.read_run_state(&run.run_id)?;
        if DRAIN_JOBS.contains(&run.job_id.as_str()) {
            if self.upgrade_resume_drain_stopped(run, state.as_ref())? {
                return Ok(Skip::new(
                    "drain_admissions_stopped",
                    "an operator stopped this drain's admissions",
                ));
            }
            let deadline = self.drain_window_deadline(run, state.as_ref())?;
            if deadline <= now {
                return Ok(Skip::new(
                    "drain_window_elapsed",
                    format!("the drain window closed at {}", deadline.to_rfc3339()),
                ));
            }
            if let Some(newer) = self.superseding_run(run, None)? {
                return Ok(Skip::new(
                    "superseded",
                    format!("a newer drain run '{newer}' superseded it"),
                ));
            }
        } else if let Some(routine) = state
            .as_ref()
            .and_then(|state| state.trigger.as_ref())
            .and_then(|trigger| trigger.routine.as_deref())
            && let Some(newer) = self.superseding_run(run, Some(routine))?
        {
            return Ok(Skip::new(
                "superseded",
                format!("a newer run '{newer}' of routine '{routine}' superseded it"),
            ));
        }

        if let Err(error) = self.plan_job_run_resume(&run.run_id) {
            return Ok(Skip::new("not_resumable", error.to_string()));
        }
        Ok(None)
    }

    /// A ship wrapper delegates its admission window and stop control to its
    /// blocking `workspace_auto_pipeline` child.
    fn upgrade_resume_drain_stopped(
        &self,
        run: &JobRun,
        state: Option<&PipelineState>,
    ) -> Result<bool, OrbitError> {
        if state.is_some_and(|state| state.admissions_stopped() || state.drain_cancelling()) {
            return Ok(true);
        }
        if run.job_id != "workspace_ship_pipeline" {
            return Ok(false);
        }
        for dispatch in state
            .iter()
            .flat_map(|state| &state.child_dispatches)
            .filter(|dispatch| dispatch.job_name == "workspace_auto_pipeline")
        {
            if self
                .read_run_state(&dispatch.child_run_id)?
                .is_some_and(|state| state.admissions_stopped() || state.drain_cancelling())
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// A claimed follower leaf or a run bound to an execution claim.
    fn is_claimed_execution(&self, run_id: &str) -> Result<bool, OrbitError> {
        if self.stores().jobs().local_pull_for_run(run_id)?.is_some() {
            return Ok(true);
        }
        Ok(self.resolve_execution_claims().is_ok_and(|claims| {
            claims.iter().any(|claim| {
                claim
                    .bound_run
                    .as_ref()
                    .is_some_and(|bound| bound.run_id == run_id)
            })
        }))
    }

    /// The deadline the drain stamped when it opened its window. The ship
    /// wrapper holds no window of its own, so its window is the one its drain
    /// child stamped. A run that never stamped one gets a window opened at
    /// its start, so replaying `open_window` cannot stretch it.
    fn drain_window_deadline(
        &self,
        run: &JobRun,
        state: Option<&PipelineState>,
    ) -> Result<DateTime<Utc>, OrbitError> {
        if let Some(deadline) = state.and_then(stamped_deadline) {
            return Ok(deadline);
        }
        for dispatch in state.iter().flat_map(|state| &state.child_dispatches) {
            if let Some(deadline) = self
                .read_run_state(&dispatch.child_run_id)?
                .as_ref()
                .and_then(stamped_deadline)
            {
                return Ok(deadline);
            }
        }
        let opened = run.started_at.unwrap_or(run.created_at);
        let seconds = run
            .input
            .as_ref()
            .and_then(|input| input.get("for_seconds"))
            .and_then(|value| match value {
                Value::Number(number) => number.as_f64(),
                Value::String(text) => text.trim().parse().ok(),
                _ => None,
            })
            .filter(|seconds: &f64| seconds.is_finite() && *seconds > 0.0)
            .unwrap_or(0.0);
        Ok(
            TimeDelta::try_milliseconds((seconds * 1000.0).round() as i64)
                .and_then(|window| opened.checked_add_signed(window))
                .unwrap_or(opened),
        )
    }

    /// A run of the same job created after `run`, fired by `routine` when one
    /// is named.
    fn superseding_run(
        &self,
        run: &JobRun,
        routine: Option<&str>,
    ) -> Result<Option<String>, OrbitError> {
        let newer = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            job_id: Some(run.job_id.clone()),
            created_since: Some(run.created_at),
            limit: Some(SUPERSEDE_SCAN_LIMIT),
            include_steps: false,
            ..JobRunQuery::default()
        })?;
        for candidate in newer {
            if candidate.run_id == run.run_id || candidate.created_at <= run.created_at {
                continue;
            }
            let same_routine = match routine {
                None => true,
                Some(routine) => self
                    .read_run_state(&candidate.run_id)?
                    .and_then(|state| state.trigger)
                    .and_then(|trigger| trigger.routine)
                    .is_some_and(|name| name == routine),
            };
            if same_routine {
                return Ok(Some(candidate.run_id));
            }
        }
        Ok(None)
    }

    fn audit_upgrade_resume(&self, run: &JobRun, status: AuditEventStatus, decision: Value) {
        let mut arguments = json!({
            "run_id": run.run_id,
            "job_id": run.job_id,
            "interrupted_at": run.finished_at.map(|at| at.to_rfc3339()),
        });
        if let (Some(arguments), Value::Object(decision)) = (arguments.as_object_mut(), decision) {
            arguments.extend(decision);
        }
        super::log_best_effort(
            "record upgrade resume audit",
            &run.run_id,
            self.record_pipeline_audit(
                UPGRADE_RESUME_AUDIT,
                Some(&run.run_id),
                Some(CLOCK_ACTOR),
                status,
                arguments,
                None,
            ),
        );
    }
}

/// Whether an upgrade quiesced the run or refused its worker's admission.
fn upgrade_interrupted(run: &JobRun) -> bool {
    run.steps.iter().any(|step| {
        step.error_code.as_deref() == Some(UPGRADE_QUIESCE_ERROR_CODE)
            || step.error_message.as_deref().is_some_and(|message| {
                message.contains(UPGRADE_QUIESCE_ERROR_CODE)
                    || message.contains("upgrade admission refused")
            })
    })
}

fn stamped_deadline(state: &PipelineState) -> Option<DateTime<Utc>> {
    let window = state.pipeline.get(DRAIN_WINDOW_STEP)?;
    let deadline = window
        .get("deadline")
        .or_else(|| window.get("output")?.get("deadline"))?
        .as_str()?;
    DateTime::parse_from_rfc3339(deadline.trim())
        .ok()
        .map(|deadline| deadline.with_timezone(&Utc))
}
