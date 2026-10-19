//! Extend the window of a live drain [ORB-15383].
//!
//! A drain's deadline is stamped once, by its `open_window` step, and a run
//! cannot be resubmitted with a longer one: the drain jobs admit one active
//! run, so a repeat `orbit run auto` queues behind the live drain until its
//! whole post-window tail has settled. This control is the supported way to
//! keep a drain admitting: one durable, audited extension on the run's own
//! pipeline state that every deadline reader prefers when it is later than
//! the stamp. The run id, its children, its completion authorization and its
//! ceiling are unchanged; `--concurrency` retunes the ceiling through the
//! worker-limit control in the same request.
//!
//! An extension never shortens a window, never reopens admissions an operator
//! stopped, and never turns a single-pass drain (submitted without a window)
//! into a timed one. A pull drain keeps iterating after its window closes,
//! until its leaves settle, so extending it in that tail resumes its requests:
//! each pass reads the window from run state, not from the answer its job
//! forwarded. The pass that would end a pull drain records the close in the
//! transaction that rereads its deadline [ORB-15417], so an extension accepted
//! first keeps it admitting and one after the close is refused. An owner drain
//! leaves its loop on the first expired answer, so once its window has closed
//! it is refused instead of accepted and never read.

use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{JobRun, JobRunState, PipelineState, PullWindowClose, RunStateUpdate};
use serde_json::{Value, json};

use super::worker_limit::{
    DrainWorkerLimitChange, DrainWorkerLimitRequest, validate_drain_worker_ceiling,
};
use crate::OrbitRuntime;
use crate::application::distributed::PULL_DRAIN_JOB;
use crate::application::workflow::{
    AUTO_WORKFLOW_ALIAS, MAX_DRAIN_WINDOW_SECONDS, find_workflow, validate_drain_window,
};

const EXTEND_REQUEST_AUDIT: &str = "pipeline.run.window_extend.requested";
const EXTEND_COMPLETION_AUDIT: &str = "pipeline.run.window_extend.completed";

/// The governed-operation id this control is authorized under, shared with
/// the drain's other live controls.
const EXTEND_OPERATION: &str = "orbit.workflow.auto";

/// The step whose output stamps a drain's deadline (`drain_window`).
const DRAIN_WINDOW_STEP: &str = "open_window";

/// One operator request to lengthen a live drain's window.
#[derive(Debug, Clone, Copy)]
pub struct DrainWindowExtensionRequest<'a> {
    pub run_id: &'a str,
    /// The window wanted from now: the new deadline is the later of the
    /// current one and now plus this many seconds.
    pub for_seconds: u64,
    /// Also retune the drain's worker ceiling, as `orbit run concurrency`.
    pub max_active_leaf_runs: Option<u32>,
    pub reason: Option<&'a str>,
    pub actor: &'a str,
    /// Surface that made the request, recorded on the audit trail.
    pub source: &'a str,
    pub claim_token: Option<&'a str>,
}

/// Result of lengthening a live drain window.
///
/// Serialized as the existing snake_case response tokens. Unknown response
/// tokens are rejected on deserialization; persisted audit records remain separate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DrainWindowExtensionOutcome {
    Extended,
    Unchanged,
}

impl DrainWindowExtensionOutcome {
    /// The stable response and audit token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Extended => "extended",
            Self::Unchanged => "unchanged",
        }
    }
}

impl std::fmt::Display for DrainWindowExtensionOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Outcome of an accepted window extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainWindowExtensionChange {
    pub run_id: String,
    pub job_id: String,
    /// `extended` when the deadline moved, `unchanged` when the requested
    /// window ends no later than the one already in force. Both are audited.
    pub outcome: DrainWindowExtensionOutcome,
    pub previous_deadline: DateTime<Utc>,
    pub deadline: DateTime<Utc>,
    /// Accepted-extension counter; zero while none has moved the deadline.
    pub revision: u32,
    /// The ceiling change, when the request carried one.
    pub worker_limit: Option<DrainWorkerLimitChange>,
}

impl OrbitRuntime {
    /// Lengthen the window of an auto or pull drain that is still running,
    /// and retune its ceiling when asked.
    ///
    /// The refusal checks and the write share one transaction, so a run that
    /// terminalizes, stops or is cancelled concurrently refuses the extension
    /// rather than recording one nothing will read. Every outcome is audited
    /// against the run with the old and new deadline.
    pub fn extend_drain_window(
        &self,
        request: DrainWindowExtensionRequest<'_>,
    ) -> Result<DrainWindowExtensionChange, OrbitError> {
        self.require_workspace_claim(EXTEND_OPERATION, request.claim_token)?;
        let request_id = audit_execution_id("window_extend");
        let outcome = self.apply_drain_window_extension(request, &request_id);
        match &outcome {
            Ok(change) => self.record_drain_control_completion(
                EXTEND_COMPLETION_AUDIT,
                Some(request.run_id),
                &request_id,
                change.outcome.as_str(),
                json!({
                    "job_id": change.job_id,
                    "for_seconds": request.for_seconds,
                    "previous_deadline": rfc3339(change.previous_deadline),
                    "deadline": rfc3339(change.deadline),
                    "revision": change.revision,
                }),
                None,
            )?,
            Err(error) => self.record_drain_control_completion(
                EXTEND_COMPLETION_AUDIT,
                Some(request.run_id),
                &request_id,
                "rejected",
                json!({ "for_seconds": request.for_seconds }),
                Some(error.to_string()),
            )?,
        }
        let mut change = outcome?;
        if let Some(max_active_leaf_runs) = request.max_active_leaf_runs {
            change.worker_limit = Some(self.set_drain_worker_limit(DrainWorkerLimitRequest {
                run_id: request.run_id,
                max_active_leaf_runs,
                expected_revision: None,
                reason: request.reason,
                actor: request.actor,
                source: request.source,
                claim_token: request.claim_token,
            })?);
        }
        Ok(change)
    }

    fn apply_drain_window_extension(
        &self,
        request: DrainWindowExtensionRequest<'_>,
        request_id: &str,
    ) -> Result<DrainWindowExtensionChange, OrbitError> {
        let run_id = request.run_id;
        let run = self
            .get_job_run_backend(run_id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, run_id.to_string()))?;
        let auto_job = find_workflow(AUTO_WORKFLOW_ALIAS)
            .map(|workflow| workflow.job_id)
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!("unknown workflow '{AUTO_WORKFLOW_ALIAS}'"))
            })?;
        if run.job_id != auto_job && run.job_id != PULL_DRAIN_JOB {
            return Err(OrbitError::InvalidInput(format!(
                "job run '{run_id}' is a `{}` run; only a `{auto_job}` or `{PULL_DRAIN_JOB}` drain has a window to extend",
                run.job_id
            )));
        }
        if request.for_seconds == 0 || validate_drain_window(Some(request.for_seconds)).is_err() {
            return Err(OrbitError::InvalidInput(format!(
                "the extension must be between 1 second and {MAX_DRAIN_WINDOW_SECONDS} seconds (24 hours)"
            )));
        }
        if let Some(requested_ceiling) = request.max_active_leaf_runs {
            validate_drain_worker_ceiling(requested_ceiling)?;
        }
        let window = TimeDelta::try_seconds(request.for_seconds as i64).ok_or_else(|| {
            OrbitError::InvalidInput(format!("window {}s is out of range", request.for_seconds))
        })?;
        if run.state.is_terminal() {
            return Err(terminal_run_error(run_id, run.state));
        }
        self.record_extension_request(&run, request_id, request)?;

        let pull_drain = run.job_id == PULL_DRAIN_JOB;
        let mut decided: Option<(bool, DateTime<Utc>, DateTime<Utc>, u32)> = None;
        let update = self.stores().jobs().update_run_state(
            run_id,
            &mut |run_state: JobRunState, state: &mut PipelineState| {
                if run_state.is_terminal() {
                    return Err(terminal_run_error(run_id, run_state));
                }
                refuse_closed_drain(run_id, &run, state)?;
                let stamped = stamped_drain_deadline(state).ok_or_else(|| {
                    OrbitError::JobValidation(format!(
                        "job run '{run_id}' has not opened its window yet; extend it once it has"
                    ))
                })?;
                let now = Utc::now();
                let current = state.effective_drain_deadline(stamped);
                // An owner drain leaves its loop on the first expired window
                // answer; an extension after that would be recorded and never
                // read. A pull drain keeps answering until its leaves settle.
                if !pull_drain && current <= now {
                    return Err(OrbitError::JobValidation(format!(
                        "job run '{run_id}' closed its window at {}; it admits nothing more, so start a new drain",
                        rfc3339(current)
                    )));
                }
                let requested = now.checked_add_signed(window).ok_or_else(|| {
                    OrbitError::InvalidInput("the extended deadline overflows".to_string())
                })?;
                let extended = state.extend_drain_window(
                    stamped,
                    requested,
                    request.actor.to_string(),
                    request.reason.map(str::to_string),
                );
                let revision = state
                    .pull_drain.drain_window_extension
                    .as_ref()
                    .map_or(0, |extension| extension.revision);
                decided = Some((
                    extended,
                    current,
                    state.effective_drain_deadline(stamped),
                    revision,
                ));
                Ok(())
            },
        )?;

        match update {
            RunStateUpdate::Updated => {}
            RunStateUpdate::NotFound => {
                return Err(OrbitError::not_found(
                    NotFoundKind::JobRun,
                    run_id.to_string(),
                ));
            }
            // A queued drain has no window yet. Its own `--for` decides it
            // once it starts.
            RunStateUpdate::NoState => {
                return Err(OrbitError::JobValidation(format!(
                    "job run '{run_id}' has not started; extend it once it is running"
                )));
            }
        }
        let (extended, previous_deadline, deadline, revision) = decided.ok_or_else(|| {
            OrbitError::Execution(format!("job run '{run_id}' recorded no extension decision"))
        })?;
        Ok(DrainWindowExtensionChange {
            run_id: run_id.to_string(),
            job_id: run.job_id,
            outcome: if extended {
                DrainWindowExtensionOutcome::Extended
            } else {
                DrainWindowExtensionOutcome::Unchanged
            },
            previous_deadline,
            deadline,
            revision,
            worker_limit: None,
        })
    }

    /// Close a timed pull drain's window for good, when its pass found the
    /// window expired and nothing left to settle [ORB-15417]: `false` when
    /// the window reads open again, because an extension was accepted after
    /// the pass read it. The reread and the close share one transaction with
    /// the extension's own check, so exactly one of them wins.
    /// `forwarded_expired` is the job's window answer, which governs a drain
    /// whose state holds no stamped window; no extension can reach that
    /// drain, so its close is not recorded.
    pub(crate) fn close_pull_window(
        &self,
        run_id: &str,
        forwarded_expired: bool,
    ) -> Result<bool, OrbitError> {
        let mut closed = false;
        let update = self.stores().jobs().update_run_state(
            run_id,
            &mut |_, state: &mut PipelineState| {
                closed = pull_window_closed(state, forwarded_expired, Utc::now());
                // Only a stamped window can be extended, so only its close
                // needs recording; an unstamped one keeps the forwarded answer.
                if closed
                    && state.pull_drain.pull_window_close.is_none()
                    && drain_deadline(state).is_some()
                {
                    state.pull_drain.pull_window_close = Some(PullWindowClose {
                        closed_at: Utc::now(),
                    });
                    state.updated_at = Utc::now();
                }
                Ok(())
            },
        )?;
        if update != RunStateUpdate::Updated {
            return Err(OrbitError::Store(format!(
                "pull drain {run_id} has no run state to record its window close in"
            )));
        }
        Ok(closed)
    }

    fn record_extension_request(
        &self,
        run: &JobRun,
        request_id: &str,
        request: DrainWindowExtensionRequest<'_>,
    ) -> Result<(), OrbitError> {
        self.record_pipeline_audit(
            EXTEND_REQUEST_AUDIT,
            Some(&run.run_id),
            Some(request.actor),
            AuditEventStatus::Success,
            json!({
                "request_id": request_id,
                "run_id": run.run_id,
                "job_id": run.job_id,
                "observed_state": run.state.to_string(),
                "for_seconds": request.for_seconds,
                "max_active_leaf_runs": request.max_active_leaf_runs,
                "reason": request.reason,
                "actor": request.actor,
                "source": request.source,
                "requested_at": Utc::now().to_rfc3339(),
            }),
            None,
        )
    }
}

/// The deadline in force for a drain: the one its `open_window` step stamped,
/// or the later one an operator extended it to. `None` before it stamped one.
pub(crate) fn drain_deadline(state: &PipelineState) -> Option<DateTime<Utc>> {
    stamped_drain_deadline(state).map(|stamped| state.effective_drain_deadline(stamped))
}

/// Whether a pull drain's window is closed at `now`, read from its own state:
/// closed for good once a pass recorded the close, otherwise when admissions
/// were stopped or the deadline in force has passed, as the drain's window
/// answer reads it. A state with no stamped window falls back to
/// `forwarded_expired`, the answer the job forwarded.
pub(crate) fn pull_window_closed(
    state: &PipelineState,
    forwarded_expired: bool,
    now: DateTime<Utc>,
) -> bool {
    if state.pull_drain.pull_window_close.is_some() {
        return true;
    }
    drain_deadline(state).map_or(forwarded_expired, |deadline| {
        state.admissions_stopped() || deadline <= now
    })
}

/// The deadline a drain's `open_window` step stamped, before any extension.
fn stamped_drain_deadline(state: &PipelineState) -> Option<DateTime<Utc>> {
    let window = state.pipeline.get(DRAIN_WINDOW_STEP)?;
    let deadline = window
        .get("deadline")
        .or_else(|| window.get("output")?.get("deadline"))?
        .as_str()?;
    DateTime::parse_from_rfc3339(deadline.trim())
        .ok()
        .map(|deadline| deadline.with_timezone(&Utc))
}

/// Refuse a drain an extension must not reopen or reshape: one whose
/// admissions an operator stopped or is cancelling, a pull drain whose last
/// pass closed its window, and one submitted without a window, whose single
/// admission pass is all it was authorized for.
fn refuse_closed_drain(
    run_id: &str,
    run: &JobRun,
    state: &PipelineState,
) -> Result<(), OrbitError> {
    if state.drain_cancelling() {
        return Err(OrbitError::JobValidation(format!(
            "job run '{run_id}' is being cancelled; an extension does not reopen it, so start a new drain"
        )));
    }
    if let Some(stop) = &state.pull_drain.drain_admissions_stop {
        return Err(OrbitError::JobValidation(format!(
            "job run '{run_id}' had its admissions stopped by {} at {}; an extension does not reopen them, so start a new drain",
            stop.actor,
            rfc3339(stop.stopped_at)
        )));
    }
    if let Some(close) = &state.pull_drain.pull_window_close {
        return Err(OrbitError::JobValidation(format!(
            "job run '{run_id}' closed its window at {}: its last pass found nothing left to settle and ended admissions, so start a new drain",
            rfc3339(close.closed_at)
        )));
    }
    if state.pull_drain.pull_single_pass.is_some()
        || submitted_window_seconds(&state.initial_input)
            .or_else(|| run.input.as_ref().and_then(submitted_window_seconds))
            .is_none_or(|seconds| seconds <= 0.0)
    {
        return Err(OrbitError::JobValidation(format!(
            "job run '{run_id}' runs in single-pass mode (submitted without a window); it has no window to extend, so start a drain with `--for`"
        )));
    }
    Ok(())
}

/// The `for_seconds` a drain was submitted with, as a number. Template
/// rendering may have stored it as a string; an empty string is no window.
fn submitted_window_seconds(input: &Value) -> Option<f64> {
    match input.get("for_seconds")? {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                Some(0.0)
            } else {
                text.parse().ok()
            }
        }
        Value::Null => Some(0.0),
        _ => None,
    }
}

fn rfc3339(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn terminal_run_error(run_id: &str, state: JobRunState) -> OrbitError {
    OrbitError::JobValidation(format!(
        "job run '{run_id}' is {state}; a terminal run admits no further work"
    ))
}
