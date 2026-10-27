//! Typed root causes for cascaded parent failures, and folding a run listing
//! into one row per incident [ORB-15202].
//!
//! A gate or auto parent fails in `pipeline_success_guard` whenever a child
//! does. The guard records the leaf it echoes on the parent's pipeline state
//! (`root_cause`); a parent of a cascaded parent inherits the same leaf, so
//! every run in one chain names the run that failed on its own.
//!
//! [`OrbitRuntime::run_failure_context`] is the separate observation query a
//! run view uses to name the failed descendants behind a run, by walking its
//! child dispatches live. It reads records and never stamps a root cause.

use std::collections::{HashMap, HashSet};

use orbit_common::OrbitError;
use orbit_types::workflow::{JobRun, JobRunState, JobTargetType, PipelineState, RunRootCause};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::runtime::audit::run::RunAuditStep;

use super::projection::{job_run_task_ids, run_error_step};

/// One incident in a run listing: the row that stands for it and the
/// cascaded parents folded into it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunIncident {
    /// Index of the representative run in the listing: the leaf itself when
    /// it is listed, else the first run that echoes it.
    pub index: usize,
    /// The incident's leaf run, which the representative may only echo.
    pub leaf_run_id: String,
    /// Every other listed run whose recorded root cause is this leaf, in
    /// listing order.
    pub cascaded_run_ids: Vec<String>,
}

/// Fold a run listing into one entry per incident, in listing order of each
/// incident's first row. A run with no recorded root cause is its own leaf;
/// a run whose root cause names a leaf joins that leaf's incident, whether
/// or not the leaf itself is listed.
pub fn fold_run_incidents(runs: &[JobRun], states: &[Option<&PipelineState>]) -> Vec<RunIncident> {
    let leaf_of = |index: usize| {
        states
            .get(index)
            .copied()
            .flatten()
            .and_then(|state| state.root_cause.as_ref())
            .map_or(runs[index].run_id.as_str(), |cause| {
                cause.leaf_run_id.as_str()
            })
    };
    let mut incidents: Vec<RunIncident> = Vec::new();
    let mut by_leaf: HashMap<&str, usize> = HashMap::new();
    for (index, run) in runs.iter().enumerate() {
        let leaf = leaf_of(index);
        let Some(&slot) = by_leaf.get(leaf) else {
            by_leaf.insert(leaf, incidents.len());
            incidents.push(RunIncident {
                index,
                leaf_run_id: leaf.to_string(),
                cascaded_run_ids: Vec::new(),
            });
            continue;
        };
        let incident = &mut incidents[slot];
        if run.run_id == leaf {
            // The leaf itself represents its incident once it is listed.
            let echo = runs[incident.index].run_id.clone();
            incident.cascaded_run_ids.insert(0, echo);
            incident.index = index;
        } else {
            incident.cascaded_run_ids.push(run.run_id.clone());
        }
    }
    incidents
}

/// Use the recorded code or typed terminal outcome. Diagnostic prose and
/// captured command output cannot supply a classification.
fn failure_code(error_code: Option<&str>, state: JobRunState) -> Option<String> {
    if let Some(code) = error_code.filter(|code| !code.trim().is_empty()) {
        return Some(code.to_string());
    }
    (state.is_terminal() && state != JobRunState::Success).then(|| state.to_string())
}

impl OrbitRuntime {
    /// The leaf failure a child run's outcome stands for: the root cause it
    /// recorded when it only echoed its own child, else the run itself.
    /// Resolves the failed step from its failure checkpoint, audit trail, or
    /// stored activity steps, excluding synthetic job-level diagnostics.
    /// Observes stored records without reconciliation; `None` when the run
    /// does not exist.
    pub fn run_root_cause(&self, run_id: &str) -> Result<Option<RunRootCause>, OrbitError> {
        let Some(run) = self.get_job_run_backend(run_id)? else {
            return Ok(None);
        };
        let state = self.read_run_state(run_id)?;
        if let Some(cause) = state.as_ref().and_then(|state| state.root_cause.clone()) {
            return Ok(Some(cause));
        }
        let error_step = run_error_step(&run);
        let error_code = error_step.and_then(|step| step.error_code.as_deref());
        // The failure handoff names the step that failed; otherwise the audit
        // trail does, and a stored per-activity step is the last resort. A
        // job-level diagnostic step names the job, not a step.
        let audit_step = || {
            self.collect_run_audit_steps(run_id)
                .unwrap_or_default()
                .into_iter()
                .rev()
                .find(|step| {
                    step.state.as_deref() != Some("skipped")
                        && (step
                            .error_message
                            .as_deref()
                            .is_some_and(|message| !message.is_empty())
                            || matches!(
                                step.state.as_deref(),
                                Some("error" | "failed" | "timeout" | "interrupted")
                            ))
                })
                .map(|step| step.step_id)
        };
        let step = state
            .as_ref()
            .and_then(|state| state.failure_activity_checkpoint.as_ref())
            .map(|checkpoint| checkpoint.failed_step_id.clone())
            .or_else(|| {
                (run.state != JobRunState::Success)
                    .then(audit_step)
                    .flatten()
            })
            .or_else(|| {
                error_step
                    .filter(|step| step.target_type != JobTargetType::Job)
                    .map(|step| step.target_id.clone())
            });
        Ok(Some(RunRootCause {
            leaf_run_id: run.run_id.clone(),
            task_id: job_run_task_ids(&run).into_iter().next(),
            step,
            code: failure_code(error_code, run.state),
        }))
    }

    /// Why a settled run ended as it did, from its own records: the error its
    /// explaining step recorded, else the leaf its recorded root cause echoes.
    /// `None` when the run is unknown, still active, succeeded, or recorded
    /// neither. Reads the run row and its state, never the audit trail, so a
    /// parent can afford it once per settled child [ORB-15435].
    pub(crate) fn run_outcome_reason(&self, run_id: &str) -> Result<Option<String>, OrbitError> {
        let Some(run) = self.get_job_run_backend(run_id)? else {
            return Ok(None);
        };
        if !run.state.is_terminal() {
            return Ok(None);
        }
        let step_error = run_error_step(&run).and_then(|step| {
            match (step.error_code.as_deref(), step.error_message.as_deref()) {
                (Some(code), Some(message)) => Some(format!("{code}: {message}")),
                (Some(text), None) | (None, Some(text)) => Some(text.to_string()),
                (None, None) => None,
            }
        });
        if step_error.is_some() {
            return Ok(step_error);
        }
        let known = |value: Option<String>| value.unwrap_or_else(|| "-".to_string());
        Ok(self
            .read_run_state(run_id)?
            .and_then(|state| state.root_cause)
            .map(|cause| {
                format!(
                    "echoes leaf run {} (task {}, step {}, code {})",
                    cause.leaf_run_id,
                    known(cause.task_id),
                    known(cause.step),
                    known(cause.code),
                )
            }))
    }

    /// Record on a parent run the leaf failure its own failure echoes. Best
    /// effort: the parent's failure stands whether or not the record lands.
    pub(crate) fn record_run_root_cause(&self, run_id: &str, cause: &RunRootCause) {
        let result = self.read_run_state(run_id).and_then(|state| {
            let Some(mut state) = state else {
                return Ok(());
            };
            state.root_cause = Some(cause.clone());
            self.write_run_state(run_id, &state)
        });
        crate::application::job::log_best_effort("record run root cause", run_id, result);
    }
}

/// How far [`RunFailureContextProfile::DeepestObserved`] follows child
/// dispatches. Matches the cancel cascade.
const DEEPEST_OBSERVED_MAX_DEPTH: usize = 8;

/// How a run-failure query reads each descendant run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunFailureRead {
    /// Reconcile a stale descendant and settle orphaned pull claims, as a
    /// default `orbit run show` read does.
    Reconcile,
    /// Report stored records as-is.
    Observe,
}

/// Which causes [`OrbitRuntime::run_failure_context`] reports, how it reads
/// descendants, and which read failures it survives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunFailureContextProfile {
    /// `orbit run show`: every failed terminal leaf in depth-first
    /// child-dispatch order, through successful branches and without a depth
    /// ceiling. The run itself is its own cause when no descendant failed. A
    /// missing child is skipped and an unreadable child checkpoint is warned
    /// about; every other read failure fails the query.
    AllFailedLeaves(RunFailureRead),
    /// Dashboard run detail: only the deepest failed descendant of a failed,
    /// timed-out, interrupted or cancelled run, at most
    /// eight dispatches down, skipping successful
    /// and skipped branches; the earlier dispatch wins a tie. The run is
    /// never its own cause. Reads are observed, and every supplementary read
    /// failure is warned about and its evidence left out.
    DeepestObserved,
}

impl RunFailureContextProfile {
    fn read(self) -> RunFailureRead {
        match self {
            Self::AllFailedLeaves(read) => read,
            Self::DeepestObserved => RunFailureRead::Observe,
        }
    }
}

/// One failed run a run-failure query names, with the step and message that
/// explain it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunFailureCause {
    /// The failed run.
    pub run_id: String,
    /// Its terminal state.
    pub state: JobRunState,
    /// The step the profile's precedence chose, if any.
    pub step: Option<String>,
    /// That step's error message, if any.
    pub message: Option<String>,
}

impl RunFailureCause {
    /// The `orbit run show` root-cause item: `run_id`, `step`, `message`.
    pub fn to_leaf_json(&self) -> Value {
        json!({
            "run_id": self.run_id,
            "step": self.step,
            "message": self.message,
        })
    }

    /// The dashboard's `failure_root`: `run_id`, `state`, `step`, `message`.
    pub fn to_failure_root_json(&self) -> Value {
        json!({
            "run_id": self.run_id,
            "state": self.state.to_string(),
            "step": self.step,
            "message": self.message,
        })
    }
}

/// The causes a run-failure query found, in the profile's order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunFailureContext {
    /// At most one for [`RunFailureContextProfile::DeepestObserved`].
    pub causes: Vec<RunFailureCause>,
}

/// Evidence a run-failure query could not read and went on without.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunFailureEvidence {
    /// A dispatched child's run row; the child was skipped.
    ChildRun,
    /// A run's pipeline checkpoint; its child dispatches were not followed.
    Checkpoint,
    /// A run's audit steps; its cause was chosen from stored steps.
    AuditSteps,
}

/// One read a run-failure query survived, for the caller to report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunFailureContextWarning {
    /// What could not be read.
    pub evidence: RunFailureEvidence,
    /// The run it belongs to.
    pub run_id: String,
    /// The read error.
    pub error: String,
}

impl RunFailureContextWarning {
    fn new(evidence: RunFailureEvidence, run_id: &str, error: &OrbitError) -> Self {
        Self {
            evidence,
            run_id: run_id.to_string(),
            error: error.to_string(),
        }
    }
}

/// A failed, timed-out, interrupted or cancelled run: one a failure query may
/// name as a cause.
fn is_chain_failure(state: JobRunState) -> bool {
    matches!(
        state,
        JobRunState::Failed
            | JobRunState::Timeout
            | JobRunState::Interrupted
            | JobRunState::Cancelled
    )
}

struct FailureWalk<'w> {
    profile: RunFailureContextProfile,
    visited: HashSet<String>,
    /// [`RunFailureContextProfile::AllFailedLeaves`] causes, in walk order.
    causes: Vec<RunFailureCause>,
    /// [`RunFailureContextProfile::DeepestObserved`]'s deepest leaf so far.
    deepest: Option<(usize, JobRun)>,
    warn: &'w mut dyn FnMut(&RunFailureContextWarning),
}

impl FailureWalk<'_> {
    fn warn(&mut self, evidence: RunFailureEvidence, run_id: &str, error: &OrbitError) {
        (self.warn)(&RunFailureContextWarning::new(evidence, run_id, error));
    }
}

impl OrbitRuntime {
    /// The failed descendants behind `run`, chosen and summarized by
    /// `profile`. `state` is `run`'s own checkpoint, when readable. Each read
    /// failure the profile survives is passed to `warn` as it happens, so a
    /// later failure does not hide it.
    pub fn run_failure_context(
        &self,
        run: &JobRun,
        state: Option<&PipelineState>,
        profile: RunFailureContextProfile,
        warn: &mut dyn FnMut(&RunFailureContextWarning),
    ) -> Result<RunFailureContext, OrbitError> {
        if profile == RunFailureContextProfile::DeepestObserved && !is_chain_failure(run.state) {
            return Ok(RunFailureContext::default());
        }
        let mut walk = FailureWalk {
            profile,
            visited: HashSet::new(),
            causes: Vec::new(),
            deepest: None,
            warn,
        };
        self.walk_run_failure(run, state, 0, &mut walk)?;
        let mut causes = std::mem::take(&mut walk.causes);
        if let Some((_, leaf)) = walk.deepest.take() {
            causes.push(self.run_failure_cause(&leaf, profile, walk.warn)?);
        }
        Ok(RunFailureContext { causes })
    }

    /// `run`'s own failure step and message under `profile`'s precedence,
    /// whatever its state. [`RunFailureContextProfile::AllFailedLeaves`]
    /// fails on an unreadable audit trail; `DeepestObserved` passes it to
    /// `warn` and uses the stored steps.
    pub fn run_failure_cause(
        &self,
        run: &JobRun,
        profile: RunFailureContextProfile,
        warn: &mut dyn FnMut(&RunFailureContextWarning),
    ) -> Result<RunFailureCause, OrbitError> {
        let (step, message) = match profile {
            RunFailureContextProfile::AllFailedLeaves(_) => {
                all_leaves_summary(&self.collect_run_audit_steps(&run.run_id)?, run)
            }
            RunFailureContextProfile::DeepestObserved => {
                let audit = self
                    .collect_run_audit_steps(&run.run_id)
                    .unwrap_or_else(|error| {
                        warn(&RunFailureContextWarning::new(
                            RunFailureEvidence::AuditSteps,
                            &run.run_id,
                            &error,
                        ));
                        Vec::new()
                    });
                deepest_observed_audit_summary(&audit)
                    .unwrap_or_else(|| deepest_observed_stored_summary(run))
            }
        };
        Ok(RunFailureCause {
            run_id: run.run_id.clone(),
            state: run.state,
            step,
            message,
        })
    }

    fn read_failure_descendant(
        &self,
        run_id: &str,
        read: RunFailureRead,
    ) -> Result<JobRun, OrbitError> {
        match read {
            RunFailureRead::Reconcile => {
                let run = self.show_job_run(run_id)?;
                self.settle_orphaned_pulls();
                Ok(run)
            }
            // Observed, not reconciled: naming a cause must not finalize a
            // child.
            RunFailureRead::Observe => self.show_job_run_observed(run_id),
        }
    }

    /// Walk `run`'s child dispatches depth first; whether its subtree holds a
    /// cause. Depth 0 is the run being viewed.
    fn walk_run_failure(
        &self,
        run: &JobRun,
        state: Option<&PipelineState>,
        depth: usize,
        walk: &mut FailureWalk<'_>,
    ) -> Result<bool, OrbitError> {
        let deepest_observed = walk.profile == RunFailureContextProfile::DeepestObserved;
        if deepest_observed && depth > DEEPEST_OBSERVED_MAX_DEPTH {
            return Ok(false);
        }
        if !walk.visited.insert(run.run_id.clone()) {
            return Ok(false);
        }
        // A successful or skipped descendant is not a cause, and following it
        // would walk a drain's finished leaves to find an unrelated failure.
        if deepest_observed
            && depth > 0
            && matches!(run.state, JobRunState::Success | JobRunState::Skipped)
        {
            return Ok(false);
        }
        let mut found = false;
        for dispatch in state.into_iter().flat_map(|state| &state.child_dispatches) {
            let child =
                match self.read_failure_descendant(&dispatch.child_run_id, walk.profile.read()) {
                    Ok(child) => child,
                    Err(error) if deepest_observed => {
                        walk.warn(RunFailureEvidence::ChildRun, &dispatch.child_run_id, &error);
                        continue;
                    }
                    Err(OrbitError::NotFound { .. }) => continue,
                    Err(error) => return Err(error),
                };
            let child_state = self.read_run_state(&child.run_id).unwrap_or_else(|error| {
                walk.warn(RunFailureEvidence::Checkpoint, &child.run_id, &error);
                None
            });
            found |= self.walk_run_failure(&child, child_state.as_ref(), depth + 1, walk)?;
        }
        // Only a failed leaf is a cause; a wrapper's own error stays in its
        // run header and steps.
        let may_be_cause = depth > 0 || !deepest_observed;
        if found || !may_be_cause || !is_chain_failure(run.state) {
            return Ok(found);
        }
        if deepest_observed {
            // The earlier dispatch keeps a tie.
            if walk.deepest.as_ref().is_none_or(|(best, _)| depth > *best) {
                walk.deepest = Some((depth, run.clone()));
            }
        } else {
            let cause = self.run_failure_cause(run, walk.profile, walk.warn)?;
            walk.causes.push(cause);
        }
        Ok(true)
    }
}

/// [`RunFailureContextProfile::AllFailedLeaves`] precedence: a non-skipped
/// audit error, a non-skipped stored error, a failed audit step, a failed
/// stored step.
///
/// V2 runs can store a synthetic job-level step as well as the actual YAML
/// steps in their audit trail. Preferring an audit error names the step that
/// failed, even when the stored step only wraps that failure.
fn all_leaves_summary(audit: &[RunAuditStep], run: &JobRun) -> (Option<String>, Option<String>) {
    let audit_error = audit.iter().rev().find(|step| {
        step.state.as_deref() != Some("skipped")
            && step
                .error_message
                .as_deref()
                .is_some_and(|message| !message.is_empty())
    });
    let stored_error = run.steps.iter().rev().find(|step| {
        step.state != JobRunState::Skipped
            && step
                .error_message
                .as_deref()
                .is_some_and(|message| !message.is_empty())
    });
    let audit_failed = audit.iter().rev().find(|step| {
        matches!(
            step.state.as_deref(),
            Some("error" | "failed" | "timeout" | "interrupted")
        )
    });
    let stored_failed = run.steps.iter().rev().find(|step| {
        matches!(
            step.state,
            JobRunState::Failed | JobRunState::Timeout | JobRunState::Interrupted
        )
    });
    if let Some(step) = audit_error {
        (Some(step.step_id.clone()), step.error_message.clone())
    } else if let Some(step) = stored_error {
        (Some(step.target_id.clone()), step.error_message.clone())
    } else if let Some(step) = audit_failed {
        (Some(step.step_id.clone()), step.error_message.clone())
    } else if let Some(step) = stored_failed {
        (Some(step.target_id.clone()), step.error_message.clone())
    } else {
        (None, None)
    }
}

/// [`RunFailureContextProfile::DeepestObserved`] audit precedence: the last
/// step with a nonblank error, else the last failed, timed-out, interrupted
/// or cancelled step. `None` when neither names a step or message.
fn deepest_observed_audit_summary(
    steps: &[RunAuditStep],
) -> Option<(Option<String>, Option<String>)> {
    let chosen = steps
        .iter()
        .rev()
        .find(|step| nonblank(step.error_message.as_deref()))
        .or_else(|| {
            steps.iter().rev().find(|step| {
                matches!(
                    step.state.as_deref(),
                    Some("error" | "failed" | "timeout" | "interrupted" | "cancelled")
                ) && nonblank(Some(step.step_id.as_str()))
            })
        })?;
    let summary = (
        trimmed(Some(chosen.step_id.as_str())),
        trimmed(chosen.error_message.as_deref()),
    );
    (summary.0.is_some() || summary.1.is_some()).then_some(summary)
}

/// [`RunFailureContextProfile::DeepestObserved`] stored-step precedence: the
/// last non-skipped step with a nonblank error, else the last failed step,
/// else the last non-skipped step.
fn deepest_observed_stored_summary(run: &JobRun) -> (Option<String>, Option<String>) {
    let Some(chosen) = run
        .steps
        .iter()
        .rev()
        .find(|step| step.state != JobRunState::Skipped && nonblank(step.error_message.as_deref()))
        .or_else(|| {
            run.steps
                .iter()
                .rev()
                .find(|step| is_chain_failure(step.state))
        })
        .or_else(|| {
            run.steps
                .iter()
                .rev()
                .find(|step| step.state != JobRunState::Skipped)
        })
    else {
        return (None, None);
    };
    (
        trimmed(Some(chosen.target_id.as_str())),
        trimmed(chosen.error_message.as_deref()),
    )
}

fn nonblank(value: Option<&str>) -> bool {
    value.is_some_and(|text| !text.trim().is_empty())
}

fn trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}
