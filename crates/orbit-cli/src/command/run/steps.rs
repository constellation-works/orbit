use orbit_core::application::job::JobRunListParams;
pub(crate) use orbit_core::application::job::{
    RunDisplayStep as RunStepRecord, RunDisplaySteps, RunStepSource as StepSource,
};
use orbit_core::runtime::audit::run::RunAuditStep;
use orbit_core::{JobRun, JobRunStep, NotFoundKind, OrbitError, OrbitRuntime};
use orbit_types::workflow::{JobTargetType, PipelineState};
use serde_json::{Value, json};

use crate::command::{CommandOut, Payload};
use crate::output::color::Domain;

use super::format::{
    LockHolders, format_admissions_stop_line, format_child_dispatch_lines,
    format_crew_selection_line, format_duration, format_run_role, format_timestamp,
    format_waiting_line_with_holders, format_window_extension_line, format_worker_limit_line,
    summarize_error_message,
};

/// Whether a run read may reconcile stale runs before reporting them.
///
/// `Reconcile` is the operator default: an orphaned `pending`/`running` run
/// is finalized as `interrupted`, releasing its task reservations, so the
/// view shows what actually happened. It then settles with their owner the
/// pull claims an ended drain or leaf left that nothing else will carry
/// [ORB-15783]. `Observe` reports stored records
/// untouched, for readers that promise not to mutate run state [ORB-12941].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunRead {
    Reconcile,
    Observe,
}

impl RunRead {
    pub(crate) fn from_no_reconcile(no_reconcile: bool) -> Self {
        if no_reconcile {
            Self::Observe
        } else {
            Self::Reconcile
        }
    }

    pub(crate) fn show(self, runtime: &OrbitRuntime, run_id: &str) -> Result<JobRun, OrbitError> {
        match self {
            Self::Reconcile => {
                let run = runtime.show_job_run(run_id)?;
                runtime.settle_orphaned_pulls();
                Ok(run)
            }
            Self::Observe => runtime.show_job_run_observed(run_id),
        }
    }

    pub(crate) fn list(
        self,
        runtime: &OrbitRuntime,
        params: JobRunListParams,
    ) -> Result<Vec<JobRun>, OrbitError> {
        match self {
            Self::Reconcile => {
                let runs = runtime.list_job_runs(params)?;
                runtime.settle_orphaned_pulls();
                Ok(runs)
            }
            Self::Observe => runtime.list_job_runs_observed(params),
        }
    }
}

pub(crate) fn resolve_run(
    runtime: &OrbitRuntime,
    run_id: Option<&str>,
    read: RunRead,
) -> Result<JobRun, OrbitError> {
    if let Some(run_id) = run_id {
        return read.show(runtime, run_id);
    }

    read.list(
        runtime,
        JobRunListParams {
            limit: Some(1),
            ..Default::default()
        },
    )?
    .into_iter()
    .next()
    .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, "latest".to_string()))
}

pub(crate) fn resolve_run_step(
    runtime: &OrbitRuntime,
    run: &JobRun,
    step_id: &str,
) -> Result<RunStepRecord, OrbitError> {
    let audit_steps = runtime.collect_run_audit_steps(&run.run_id)?;
    if let Some(audit_step) = audit_steps.iter().find(|step| step.step_id == step_id) {
        return Ok(RunStepRecord::from_audit(audit_step.clone(), run.state));
    }

    let audit_index = step_id
        .parse::<u32>()
        .ok()
        .and_then(|index| audit_steps.iter().find(|step| step.step_index == index));
    let audit_is_displayed = !audit_steps.is_empty()
        && run
            .steps
            .iter()
            .all(|step| step.target_type == JobTargetType::Job);
    if audit_is_displayed && let Some(audit_step) = audit_index {
        return Ok(RunStepRecord::from_audit(audit_step.clone(), run.state));
    }

    if let Some(stored_step) = find_stored_run_step(run, step_id) {
        return Ok(RunStepRecord::from_job_step(stored_step));
    }
    if let Some(audit_step) = audit_index {
        return Ok(RunStepRecord::from_audit(audit_step.clone(), run.state));
    }

    Err(step_not_found(&run.run_id, step_id))
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

pub(crate) fn run_header_text_with_lock_holders(
    run: &JobRun,
    state: Option<&PipelineState>,
    holders: &LockHolders,
) -> String {
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
    if let Some(line) = format_crew_selection_line(run.input.as_ref()) {
        lines.push(line);
    }
    if run.resolved_crew.is_some() || run.crew_model.is_some() {
        lines.push(format!(
            "{} {} ({})",
            bold("Resolved Run Crew:"),
            run.resolved_crew.as_deref().unwrap_or("-"),
            run.crew_model.as_deref().unwrap_or("model unavailable"),
        ));
    }
    // The pool each activity drew its crew from, and the rule that chose it.
    for (key, draw) in state
        .into_iter()
        .flat_map(|state| &state.activity_crew_draws)
    {
        lines.push(format!(
            "{} {} for {key} via {}",
            bold("Activity Crew:"),
            draw.crew,
            draw.source
        ));
    }
    if let Some(line) = format_waiting_line_with_holders(run.state, state, holders) {
        lines.push(line);
    }
    if let Some(line) = format_worker_limit_line(state) {
        lines.push(line);
    }
    if let Some(line) = format_window_extension_line(state) {
        lines.push(line);
    }
    if let Some(line) = format_admissions_stop_line(state) {
        lines.push(line);
    }
    // [ORB-14777] Variables `execution.env.pass` names that the submitting
    // process did not hold, so agents of this run never received them.
    if let Some(unset) = state
        .map(|state| &state.env_pass_unset)
        .filter(|names| !names.is_empty())
    {
        lines.push(format!("{} {}", bold("Unset env:"), unset.join(", ")));
    }
    // Crews whose `env` sources the submitting process did not hold: drains
    // excluded them and their activities were refused.
    if let Some(unset) = state
        .map(|state| &state.crew_env_unset)
        .filter(|crews| !crews.is_empty())
    {
        lines.push(format!(
            "{} {}",
            bold("Unset crew env:"),
            unset
                .iter()
                .map(orbit_types::workflow::CrewEnvUnset::describe)
                .collect::<Vec<_>>()
                .join("; ")
        ));
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

/// The steps a run view renders, chosen by the rule every run surface shares.
///
/// `audit_steps` come from the caller's existing audit scan, so recovering
/// them costs no extra read.
pub(crate) fn run_display_steps(run: &JobRun, audit_steps: Vec<RunAuditStep>) -> RunDisplaySteps {
    orbit_core::application::job::run_display_steps(run, || audit_steps)
}

pub(crate) fn step_summary_table(steps: &[RunStepRecord]) -> crate::output::table::Table {
    use crate::output::table::{Column, Table};
    // `orbit run show <run_id> -s <step>` prints one step's untruncated record.
    let mut columns = vec![
        Column::new("#").number(),
        Column::new("TARGET"),
        Column::new("STATE").fixed(),
        Column::new("DURATION").number(),
        Column::new("ERROR CODE").fixed(),
        Column::new("ERROR MESSAGE"),
    ];
    let show_lock_wait = steps
        .iter()
        .any(|step| step.lock_wait_ms.unwrap_or_default() > 0);
    if show_lock_wait {
        columns.insert(4, Column::new("LOCK WAIT (ms)").number());
    }
    let mut table =
        Table::new(columns).empty_message("no steps recorded in the run record or its audit trail");
    for step in steps {
        use comfy_table::Cell;
        let mut cells = vec![
            // One-based, as the dashboard numbers them; `step_index` stays
            // zero-based in `--json`.
            Cell::new(step.step_index + 1),
            Cell::new(&step.target_id),
            crate::output::color::cell(&step.state, Domain::JobState),
            Cell::new(format_duration(step.duration_ms)),
            Cell::new(step.error_code.as_deref().unwrap_or("-")),
            Cell::new(summarize_error_message(step.error_message.as_deref())),
        ];
        if show_lock_wait {
            cells.insert(
                4,
                Cell::new(
                    step.lock_wait_ms
                        .filter(|ms| *ms > 0)
                        .map(|ms| ms.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                ),
            );
        }
        table.add_row(cells);
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
    if let Some(wait) = step.lock_wait_ms.filter(|ms| *ms > 0) {
        lines.push(format!("{} {wait}ms", bold("Lock wait:")));
    }
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
    step.to_json()
}
