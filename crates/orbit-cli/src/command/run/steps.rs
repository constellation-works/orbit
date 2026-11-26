pub(crate) use orbit_core::app::run::RunRead;
pub(crate) use orbit_core::application::job::{
    RunDisplayStep as RunStepRecord, RunStepSource as StepSource,
};
use orbit_core::{JobRun, OrbitError};
use orbit_types::workflow::PipelineState;
use serde_json::{Value, json};

use crate::command::{CommandOut, Payload};
use crate::output::color::Domain;

use super::format::{
    LockHolders, format_admissions_stop_line, format_child_dispatch_lines,
    format_crew_selection_line, format_duration, format_run_role, format_timestamp,
    format_waiting_line_with_holders, format_window_extension_line, format_worker_limit_line,
    summarize_error_message,
};

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
    if let Some(state) = state {
        for warning in state.pull_drain.pool_crew_exclusion_warnings() {
            lines.push(format!("{} {warning}", bold("Warning:")));
        }
    }
    lines.extend(format_child_dispatch_lines(state));
    lines.join("\n")
}

/// Compact actual provider/model evidence for ordinary human run inspection.
/// The JSON projection keeps the complete invocation list, including retries.
pub(crate) fn activity_provenance_lines(
    value: &Value,
    agent_activity_ids: &std::collections::HashSet<String>,
) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|activity| {
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
            if values.is_empty() && !(status == "unavailable" && agent_activity_ids.contains(id)) {
                return None;
            }
            let evidence = if values.is_empty() {
                "evidence unavailable".to_string()
            } else {
                values.join(", ")
            };
            Some(format!("Agent provider/model ({id}): {evidence}"))
        })
        .collect()
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
