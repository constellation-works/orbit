//! The steps a run view displays, and where they came from.
//!
//! `orbit run show`, MCP `orbit.workflow.run.show`/`list` and the dashboard's
//! run detail all render a run's steps. They share this one rule for which
//! source answers and how an audit-derived step reads once its run has ended.

use chrono::{DateTime, Utc};
use orbit_common::security::redaction::redact_all;
use orbit_types::workflow::{JobRun, JobRunState, JobRunStep, JobTargetType, PipelineState};
use serde_json::{Value, json};

use crate::runtime::audit::run::RunAuditStep;

/// Where the steps a run view renders came from.
///
/// A v2 pipeline run keeps its step history in the audit trail rather than in
/// the job-run record, so `JobRun::steps` is empty for exactly the runs whose
/// `orbit run events` lists every step [ORB-12113].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunStepSource {
    /// The job-run record carried its own steps.
    Record,
    /// Reconstructed from the run's v2 audit trail.
    Audit,
}

impl RunStepSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Record => "record",
            Self::Audit => "audit",
        }
    }
}

/// One displayed step, from either source.
#[derive(Clone, Debug)]
pub struct RunDisplayStep {
    pub step_index: u32,
    pub target_type: String,
    pub target_id: String,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<u64>,
    pub lock_wait_ms: Option<u64>,
    pub exit_code: Option<i32>,
    pub agent_response_json: Option<Value>,
    pub state: String,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    /// How an audit-derived step ended; record steps carry none.
    pub outcome: Option<String>,
}

impl RunDisplayStep {
    pub fn from_job_step(step: &JobRunStep) -> Self {
        Self {
            step_index: step.step_index,
            target_type: step.target_type.to_string(),
            target_id: step.target_id.clone(),
            started_at: step.started_at,
            finished_at: step.finished_at,
            duration_ms: step.duration_ms,
            lock_wait_ms: step.lock_wait_ms,
            exit_code: step.exit_code,
            agent_response_json: step.agent_response_json.clone(),
            state: step.state.to_string(),
            error_code: step.error_code.clone(),
            error_message: step.error_message.clone(),
            outcome: None,
        }
    }

    /// Project one audit-derived step, reconciled against its run's state.
    ///
    /// [ORB-10971] Steps are rebuilt from `step_started` / `step_finished`
    /// audit events, so a run killed mid-step leaves a `step_started` with no
    /// partner. A step cannot outlive its run: once the run is terminal, an
    /// unfinished step takes the run's terminal state and the `interrupted`
    /// outcome rather than implying work is still in flight.
    pub fn from_audit(step: RunAuditStep, run_state: JobRunState) -> Self {
        let duration_ms = match (step.started_at, step.finished_at) {
            (Some(started), Some(finished)) => Some(
                finished
                    .signed_duration_since(started)
                    .num_milliseconds()
                    .max(0) as u64,
            ),
            _ => None,
        };
        let interrupted = step.state.is_none() && run_state.is_terminal();
        let state = match step.state {
            Some(state) => state,
            None if interrupted => run_state.to_string(),
            None => "running".to_string(),
        };
        let outcome = step
            .outcome
            .or_else(|| interrupted.then(|| "interrupted".to_string()));
        Self {
            step_index: step.step_index,
            target_type: JobTargetType::Activity.to_string(),
            target_id: step.step_id,
            started_at: step.started_at,
            finished_at: step.finished_at,
            duration_ms,
            lock_wait_ms: step.lock_wait_ms,
            exit_code: None,
            agent_response_json: None,
            state,
            error_code: None,
            error_message: step.error_message,
            outcome,
        }
    }

    /// The output this step recorded in the run's checkpoint state.
    ///
    /// Read by step id. The audit-derived `step_index` counts steps in
    /// first-started order, while checkpoints are keyed by YAML position, and
    /// a `when:`-skipped or resume-skipped step (which never starts) shifts
    /// the two apart; indexing `step_outputs` with it returns another step's
    /// output. Only the job-level summary step of a run that wrote no
    /// per-step checkpoint is read by index: it is recorded at step 0.
    pub fn output<'a>(&self, state: &'a PipelineState) -> Option<&'a Value> {
        state.pipeline_step_output(&self.target_id).or_else(|| {
            (self.target_type == JobTargetType::Job.to_string())
                .then(|| state.step_output(self.step_index))
                .flatten()
        })
    }

    pub fn to_json(&self) -> Value {
        json!({
            "step_index": self.step_index,
            "target_id": self.target_id,
            "target_type": self.target_type,
            "state": self.state,
            "started_at": self.started_at.map(|value| value.to_rfc3339()),
            "finished_at": self.finished_at.map(|value| value.to_rfc3339()),
            "duration_ms": self.duration_ms,
            "lock_wait_ms": self.lock_wait_ms,
            "exit_code": self.exit_code,
            "agent_response_json": self.agent_response_json,
            "error_code": self.error_code,
            "error_message": self.error_message,
            "outcome": self.outcome,
        })
    }

    /// A step's recorded launches, including retries, beside its normal output.
    pub fn to_json_with_agent_execution(
        &self,
        state: Option<&PipelineState>,
        evidence: &[super::ActivityInvocationEvidence],
    ) -> Value {
        let mut value = self.to_json_with_command_output(state);
        value["agent_executions"] = json!(evidence.iter().filter(|record| record.activity_id == self.target_id).map(|record| json!({
            "provider": record.provider, "model": record.model,
            "crew": record.execution.as_ref().and_then(|identity| identity.crew.as_deref()),
            "reasoning_effort": record.execution.as_ref().map(|identity| identity.reasoning_effort.map(|effort| effort.to_string())).unwrap_or(Some("unknown".to_string())),
        })).collect::<Vec<_>>());
        value
    }

    /// A command's bounded result, excluding argv, environment and raw streams.
    /// Other checkpoint values (including provider responses) stay private to
    /// their existing projections. Older command checkpoints derive a tail
    /// from their captured streams; missing command results return none.
    pub(crate) fn command_output_summary(&self, state: Option<&PipelineState>) -> Option<Value> {
        let output = self.output(state?)?;
        let captured = match output.get("log_tail").and_then(Value::as_str) {
            Some(tail) => tail.to_string(),
            None if output.get("argv").is_some_and(Value::is_array)
                && output.get("sandbox").is_some_and(Value::is_string) =>
            {
                format!(
                    "{}{}",
                    output.get("stdout").and_then(Value::as_str).unwrap_or(""),
                    output.get("stderr").and_then(Value::as_str).unwrap_or(""),
                )
            }
            None => return None,
        };
        let text = redact_all(&captured);
        let mut start = text.len().saturating_sub(4096);
        while !text.is_char_boundary(start) {
            start += 1;
        }
        Some(json!({
            "success":output.get("success").and_then(Value::as_bool),
            "exit_code":output.get("exit_code").and_then(Value::as_i64),
            "duration_ms":output.get("duration_ms").and_then(Value::as_u64),
            "timed_out":output.get("timed_out").and_then(Value::as_bool),
            "log_tail":&text[start..],
            "log_tail_truncated":start > 0 || output.get("log_tail_truncated").and_then(Value::as_bool).unwrap_or(false),
        }))
    }

    /// Render step metadata together with its bounded command result.
    pub fn to_json_with_command_output(&self, state: Option<&PipelineState>) -> Value {
        let mut value = self.to_json();
        let output = self.command_output_summary(state);
        if self.exit_code.is_none()
            && let Some(output) = output.as_ref()
        {
            value["exit_code"] = output["exit_code"].clone();
        }
        if let Some(output) = output {
            value["output"] = output;
        }
        value
    }
}

/// The steps a run view should render, and where they came from.
pub struct RunDisplaySteps {
    pub records: Vec<RunDisplayStep>,
    pub source: RunStepSource,
}

impl RunDisplaySteps {
    pub fn to_json(&self) -> Value {
        Value::Array(self.records.iter().map(RunDisplayStep::to_json).collect())
    }
}

/// Choose the steps a run view renders: the record's own when it has them,
/// and the ones reconstructed from its audit trail otherwise.
///
/// A record holding only run-level steps (target type `job`, as the
/// interruption reconciler writes) says how the run ended, not what it did, so
/// the audit trail answers there too. The run-level step's error stays in the
/// record's own `steps` and in the run's `error_code`/`error_message`.
///
/// Preferring the record changes nothing for a live run: a pipeline worker
/// persists its audit-derived steps only once the run ends, so a running run
/// reads its trail.
///
/// `audit_steps` is called only when the record does not answer, so a caller
/// listing many runs reads no trail it does not show.
pub fn run_display_steps(
    run: &JobRun,
    audit_steps: impl FnOnce() -> Vec<RunAuditStep>,
) -> RunDisplaySteps {
    // Vacuously true for an empty record.
    let run_level_only = run
        .steps
        .iter()
        .all(|step| step.target_type == JobTargetType::Job);
    let audit = if run_level_only {
        audit_steps()
    } else {
        Vec::new()
    };
    if !run.steps.is_empty() && audit.is_empty() {
        return RunDisplaySteps {
            records: run
                .steps
                .iter()
                .map(RunDisplayStep::from_job_step)
                .collect(),
            source: RunStepSource::Record,
        };
    }
    RunDisplaySteps {
        records: audit
            .into_iter()
            .map(|step| RunDisplayStep::from_audit(step, run.state))
            .collect(),
        source: RunStepSource::Audit,
    }
}
