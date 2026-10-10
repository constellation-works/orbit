//! Typed root causes for cascaded parent failures, and folding a run listing
//! into one row per incident [ORB-15202].
//!
//! A gate or auto parent fails in `pipeline_success_guard` whenever a child
//! does. The guard records the leaf it echoes on the parent's pipeline state
//! (`root_cause`); a parent of a cascaded parent inherits the same leaf, so
//! every run in one chain names the run that failed on its own.

use std::collections::HashMap;

use orbit_common::OrbitError;
use orbit_types::workflow::{
    BASELINE_RED_ERROR_CODE, JobRun, JobRunState, JobTargetType, PipelineState,
    ProviderFailureClass, RunRootCause, is_baseline_red_failure,
};

use crate::OrbitRuntime;

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

/// The typed failure code a diagnostic leads with: its recorded code, a red
/// base or provider class, else the first bracketed `[snake_case]` marker or
/// leading `snake_case:` token in its text.
fn failure_code(error_code: Option<&str>, message: Option<&str>) -> Option<String> {
    if let Some(code) = error_code.filter(|code| !code.trim().is_empty()) {
        return Some(code.to_string());
    }
    let message = message?;
    if is_baseline_red_failure(None, Some(message)) {
        return Some(BASELINE_RED_ERROR_CODE.to_string());
    }
    if let Some(class) = ProviderFailureClass::of(None, Some(message)) {
        return Some(class.as_str().to_string());
    }
    let snake = |word: &str| {
        word.contains('_')
            && word
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    let bracketed = message.split('[').skip(1).find_map(|rest| {
        let (marker, _) = rest.split_once(']')?;
        snake(marker).then(|| marker.to_string())
    });
    bracketed.or_else(|| {
        message
            .split_whitespace()
            .filter_map(|word| word.strip_suffix(':'))
            .find(|word| snake(word))
            .map(str::to_string)
    })
}

impl OrbitRuntime {
    /// The leaf failure a child run's outcome stands for: the root cause it
    /// recorded when it only echoed its own child, else the run itself.
    /// `None` when the run does not exist.
    pub(crate) fn run_root_cause(&self, run_id: &str) -> Result<Option<RunRootCause>, OrbitError> {
        let Some(run) = self.get_job_run_backend(run_id)? else {
            return Ok(None);
        };
        let state = self.read_run_state(run_id)?;
        if let Some(cause) = state.as_ref().and_then(|state| state.root_cause.clone()) {
            return Ok(Some(cause));
        }
        let error_step = run_error_step(&run);
        let (error_code, message) = error_step
            .map(|step| (step.error_code.as_deref(), step.error_message.as_deref()))
            .unwrap_or((None, None));
        // The failure handoff names the step that failed; otherwise the audit
        // trail does, and a stored per-activity step is the last resort. A
        // job-level diagnostic step names the job, not a step.
        let audit_step = || {
            self.collect_run_audit_steps(run_id)
                .unwrap_or_default()
                .into_iter()
                .rev()
                .find(|step| {
                    step.error_message
                        .as_deref()
                        .is_some_and(|message| !message.is_empty())
                        || matches!(
                            step.state.as_deref(),
                            Some("error" | "failed" | "timeout" | "interrupted")
                        )
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
            code: failure_code(error_code, message),
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
