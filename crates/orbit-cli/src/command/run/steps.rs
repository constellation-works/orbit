use orbit_core::application::job::JobRunListParams;
use orbit_core::runtime::run_audit::RunAuditStep;
use orbit_core::{JobRun, JobRunStep, JobTargetType, NotFoundKind, OrbitError, OrbitRuntime};
use orbit_types::workflow::PipelineState;
use serde_json::{Value, json};

use crate::command::{CommandOut, Payload};
use crate::output::color::Domain;

use super::format::{
    format_admissions_stop_line, format_child_dispatch_lines, format_duration, format_run_role,
    format_timestamp, format_waiting_line, format_worker_limit_line, summarize_error_message,
};

pub(crate) fn resolve_run(
    runtime: &OrbitRuntime,
    run_id: Option<&str>,
) -> Result<JobRun, OrbitError> {
    if let Some(run_id) = run_id {
        return runtime
            .show_job_run(run_id)
            .map_err(|_| OrbitError::not_found(NotFoundKind::JobRun, run_id.to_string()));
    }

    runtime
        .list_job_runs(JobRunListParams {
            limit: Some(1),
            ..Default::default()
        })?
        .into_iter()
        .next()
        .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, "latest".to_string()))
}

pub(crate) fn resolve_run_step(
    runtime: &OrbitRuntime,
    run: &JobRun,
    step_id: &str,
) -> Result<RunStepRecord, OrbitError> {
    if let Some(audit_step) = runtime
        .collect_run_audit_steps(&run.run_id)?
        .into_iter()
        .find(|step| step.step_id == step_id)
    {
        return Ok(RunStepRecord::from_audit_step(audit_step));
    }

    find_stored_run_step(run, step_id)
        .map(RunStepRecord::from_job_step)
        .ok_or_else(|| step_not_found(&run.run_id, step_id))
}

fn find_stored_run_step<'a>(run: &'a JobRun, step_id: &str) -> Option<&'a JobRunStep> {
    run.steps
        .iter()
        .find(|step| step.target_id == step_id || step.step_index.to_string() == step_id)
}

fn step_not_found(run_id: &str, step_id: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "step '{step_id}' does not match any step in run '{run_id}'"
    ))
}

pub(crate) fn filtered_steps<'a>(
    run: &'a JobRun,
    step_id: Option<&str>,
) -> Result<Vec<&'a JobRunStep>, OrbitError> {
    match step_id {
        Some(step_id) => Ok(vec![
            find_stored_run_step(run, step_id)
                .ok_or_else(|| step_not_found(&run.run_id, step_id))?,
        ]),
        None => Ok(run.steps.iter().collect()),
    }
}

pub(crate) fn resolve_step_filter(
    run: &JobRun,
    audit_steps: &[RunAuditStep],
    step_id: Option<&str>,
) -> Result<Option<String>, OrbitError> {
    let Some(step_id) = step_id else {
        return Ok(None);
    };

    if let Some(step) = audit_steps.iter().find(|step| step.step_id == step_id) {
        return Ok(Some(step.step_id.clone()));
    }
    if let Ok(index) = step_id.parse::<u32>()
        && let Some(step) = audit_steps.iter().find(|step| step.step_index == index)
    {
        return Ok(Some(step.step_id.clone()));
    }
    if find_stored_run_step(run, step_id).is_some() {
        return Ok(Some(step_id.to_string()));
    }

    Err(step_not_found(&run.run_id, step_id))
}

#[derive(Clone, Debug)]
pub(crate) struct RunStepRecord {
    pub(crate) step_index: u32,
    pub(crate) target_type: String,
    pub(crate) target_id: String,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
    finished_at: Option<chrono::DateTime<chrono::Utc>>,
    duration_ms: Option<u64>,
    exit_code: Option<i32>,
    agent_response_json: Option<Value>,
    state: String,
    error_code: Option<String>,
    error_message: Option<String>,
}

impl RunStepRecord {
    pub(crate) fn from_job_step(step: &JobRunStep) -> Self {
        Self {
            step_index: step.step_index,
            target_type: step.target_type.to_string(),
            target_id: step.target_id.clone(),
            started_at: step.started_at,
            finished_at: step.finished_at,
            duration_ms: step.duration_ms,
            exit_code: step.exit_code,
            agent_response_json: step.agent_response_json.clone(),
            state: step.state.to_string(),
            error_code: step.error_code.clone(),
            error_message: step.error_message.clone(),
        }
    }

    fn from_audit_step(step: RunAuditStep) -> Self {
        let duration_ms = match (step.started_at, step.finished_at) {
            (Some(started), Some(finished)) => Some(
                finished
                    .signed_duration_since(started)
                    .num_milliseconds()
                    .max(0) as u64,
            ),
            _ => None,
        };
        Self {
            step_index: step.step_index,
            target_type: JobTargetType::Activity.to_string(),
            target_id: step.step_id,
            started_at: step.started_at,
            finished_at: step.finished_at,
            duration_ms,
            exit_code: None,
            agent_response_json: None,
            state: step.state.unwrap_or_else(|| "running".to_string()),
            error_code: None,
            error_message: step.error_message,
        }
    }
}

pub(crate) fn run_header_text(run: &JobRun) -> String {
    run_header_text_with_state(run, None)
}

pub(crate) fn run_header_text_with_state(run: &JobRun, state: Option<&PipelineState>) -> String {
    use crate::output::color::{Domain, bold, dimmed, text};
    let mut lines = vec![
        format!(
            "{} {} ({})",
            bold("Run ID:"),
            run.run_id,
            format_run_role(&run.run_id)
        ),
        format!("{} {}", bold("Job ID:"), run.job_id),
        format!(
            "{} {}",
            bold("State:"),
            text(&run.state.to_string(), Domain::JobState)
        ),
        format!(
            "{} {}",
            bold("Started:"),
            dimmed(&format_timestamp(run.started_at))
        ),
        format!(
            "{} {}",
            bold("Finished:"),
            dimmed(&format_timestamp(run.finished_at))
        ),
        format!("{} {}", bold("Duration:"), format_duration(run.duration_ms)),
    ];
    if let Some(requested_crew) = run
        .input
        .as_ref()
        .and_then(|input| input.get("crew"))
        .and_then(Value::as_str)
    {
        lines.push(format!("{} {}", bold("Requested Crew:"), requested_crew));
    }
    if run.resolved_crew.is_some() || run.crew_model.is_some() {
        lines.push(format!(
            "{} {} ({})",
            bold("Resolved Run Crew:"),
            run.resolved_crew.as_deref().unwrap_or("-"),
            run.crew_model.as_deref().unwrap_or("model unavailable"),
        ));
    }
    if let Some(line) = format_waiting_line(run.state, state) {
        lines.push(line);
    }
    if let Some(line) = format_worker_limit_line(state) {
        lines.push(line);
    }
    if let Some(line) = format_admissions_stop_line(state) {
        lines.push(line);
    }
    lines.extend(format_child_dispatch_lines(state));
    lines.join("\n")
}

/// Compact actual provider/model evidence for ordinary human run inspection.
/// The JSON projection keeps the complete invocation list, including retries.
pub(crate) fn activity_provenance_lines(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(|activity| {
            let id = activity
                .get("activity_id")
                .and_then(Value::as_str)
                .unwrap_or("-");
            let status = activity
                .get("actual_status")
                .and_then(Value::as_str)
                .unwrap_or("unavailable");
            let values = activity
                .get("invocations")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|invocation| {
                    let provider = invocation
                        .get("provider")
                        .and_then(Value::as_str)
                        .unwrap_or("-");
                    let model = invocation
                        .get("model")
                        .and_then(Value::as_str)
                        .unwrap_or("model unavailable");
                    format!("{provider}/{model}")
                })
                .collect::<Vec<_>>();
            format!(
                "Activity {} actual={} {}",
                id,
                status,
                if values.is_empty() {
                    "".to_string()
                } else {
                    values.join(", ")
                }
            )
            .trim_end()
            .to_string()
        })
        .collect()
}

/// Where the steps a run view renders came from.
///
/// A v2 pipeline run keeps its step history in the audit trail rather than in
/// the job-run record, so `JobRun::steps` is empty for exactly the runs whose
/// header reports `step_outputs=N` and whose `orbit run events` lists every
/// step. `orbit run show` used to answer "no steps recorded" for those; it now
/// reconstructs them from the same trail and names the source [ORB-12113].
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepSource {
    /// The job-run record carried its own steps.
    Record,
    /// Reconstructed from the run's v2 audit trail.
    Audit,
}

impl StepSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Record => "record",
            Self::Audit => "audit",
        }
    }
}

/// The steps a run view should render, and where they came from.
pub(crate) struct RunDisplaySteps {
    pub(crate) records: Vec<RunStepRecord>,
    pub(crate) source: StepSource,
}

/// Choose the steps a run view renders: the record's own when it has them,
/// and the ones reconstructed from its audit trail otherwise.
///
/// `audit_steps` come from the caller's existing audit scan, so recovering
/// them costs no extra read.
pub(crate) fn run_display_steps(run: &JobRun, audit_steps: Vec<RunAuditStep>) -> RunDisplaySteps {
    if !run.steps.is_empty() {
        return RunDisplaySteps {
            records: run.steps.iter().map(RunStepRecord::from_job_step).collect(),
            source: StepSource::Record,
        };
    }

    RunDisplaySteps {
        records: audit_steps
            .into_iter()
            .map(RunStepRecord::from_audit_step)
            .collect(),
        source: StepSource::Audit,
    }
}

pub(crate) fn step_summary_table(steps: &[RunStepRecord]) -> crate::output::table::Table {
    use crate::output::table::{Column, Table};
    // `orbit run show <run_id> -s <step>` prints one step's untruncated record.
    let mut table = Table::new(vec![
        Column::new("#").number(),
        Column::new("TARGET"),
        Column::new("STATE").fixed(),
        Column::new("DURATION (ms)").number(),
        Column::new("ERROR CODE").fixed(),
        Column::new("ERROR MESSAGE"),
    ])
    .empty_message("no steps recorded in the run record or its audit trail");
    for step in steps {
        use comfy_table::Cell;
        table.add_row(vec![
            Cell::new(step.step_index),
            Cell::new(&step.target_id),
            crate::output::color::cell(&step.state, Domain::JobState),
            Cell::new(
                step.duration_ms
                    .map(|ms| ms.to_string())
                    .unwrap_or_else(|| "-".to_string()),
            ),
            Cell::new(step.error_code.as_deref().unwrap_or("-")),
            Cell::new(summarize_error_message(step.error_message.as_deref())),
        ]);
    }
    table
}

pub(crate) fn step_record_payload(
    run: &JobRun,
    step: &RunStepRecord,
    step_output: Option<Value>,
) -> CommandOut {
    let doc = json!({
        "run_id": run.run_id,
        "job_id": run.job_id,
        "step": run_step_record_to_json(step),
        "step_output": step_output,
    });

    use crate::output::color::{Domain, bold, dimmed, text};
    let mut lines = vec![
        format!(
            "{} {} ({})",
            bold("Run ID:"),
            run.run_id,
            format_run_role(&run.run_id)
        ),
        format!("{} {}", bold("Job ID:"), run.job_id),
        format!("{} {}", bold("Target ID:"), step.target_id),
        format!("{} {}", bold("Target Type:"), step.target_type),
        format!("{} {}", bold("State:"), text(&step.state, Domain::JobState)),
        format!(
            "{} {}",
            bold("Started:"),
            dimmed(&format_timestamp(step.started_at))
        ),
        format!(
            "{} {}",
            bold("Finished:"),
            dimmed(&format_timestamp(step.finished_at))
        ),
        format!(
            "{} {}",
            bold("Duration:"),
            format_duration(step.duration_ms)
        ),
        format!(
            "{} {}",
            bold("Exit Code:"),
            step.exit_code
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string())
        ),
        format!(
            "{} {}",
            bold("Error Code:"),
            step.error_code.as_deref().unwrap_or("-")
        ),
        format!(
            "{} {}",
            bold("Error Message:"),
            step.error_message.as_deref().unwrap_or("-")
        ),
    ];
    if let Some(output) = doc.get("step_output").filter(|value| !value.is_null()) {
        lines.push(bold("Step Output:").to_string());
        lines.push(
            serde_json::to_string_pretty(output)
                .map_err(|err| OrbitError::Store(err.to_string()))?,
        );
    }
    Ok(Payload::detail(doc.clone(), lines.join("\n")).into())
}

pub(crate) fn run_step_record_to_json(step: &RunStepRecord) -> Value {
    json!({
        "step_index": step.step_index,
        "target_id": step.target_id,
        "target_type": step.target_type,
        "state": step.state,
        "started_at": step.started_at.map(|t| t.to_rfc3339()),
        "finished_at": step.finished_at.map(|t| t.to_rfc3339()),
        "duration_ms": step.duration_ms,
        "exit_code": step.exit_code,
        "agent_response_json": step.agent_response_json,
        "error_code": step.error_code,
        "error_message": step.error_message,
    })
}

pub(crate) fn legacy_step_to_json(step: &JobRunStep) -> Value {
    json!({
        "step_index": step.step_index,
        "target_id": step.target_id,
        "target_type": step.target_type.to_string(),
        "state": step.state.to_string(),
        "started_at": step.started_at.map(|t| t.to_rfc3339()),
        "finished_at": step.finished_at.map(|t| t.to_rfc3339()),
        "duration_ms": step.duration_ms,
        "exit_code": step.exit_code,
        "error_code": step.error_code,
        "error_message": step.error_message,
    })
}
