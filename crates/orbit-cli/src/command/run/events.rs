use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_core::runtime::run_audit::RunAuditEvent;
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute, Payload};

use super::format::format_timestamp;
use super::steps::{RunRead, resolve_run, resolve_step_filter};

#[derive(Args)]
#[command(
    after_help = "JSON shape: {\"run_id\":\"...\",\"job_id\":\"...\",\"events\":[<raw-v2-audit-event-with-step_id>]}\nExamples:\n  orbit run events\n  orbit run events jrun-20260426-0631\n  orbit run events jrun-20260426-0631 -s implement_one --type cli.invocation.finished --json"
)]
pub struct RunEventsArgs {
    /// Run ID to inspect. Defaults to the most recently scheduled run globally.
    pub run_id: Option<String>,

    /// Filter to an activity step.id from the v2 job YAML, or its zero-based audit step index
    #[arg(short = 's', long = "step")]
    pub step_id: Option<String>,

    /// Filter to an exact v2 audit event_type such as step.started
    #[arg(long = "type")]
    pub event_type: Option<String>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Report stored run records as-is: skip stale-run reconciliation, which
    /// finalizes an orphaned pending or running run as interrupted and
    /// releases its task reservations
    #[arg(long)]
    pub no_reconcile: bool,
}

impl Execute for RunEventsArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        run_events_payload(
            runtime,
            self.run_id.as_deref(),
            self.step_id.as_deref(),
            self.event_type.as_deref(),
            RunRead::from_no_reconcile(self.no_reconcile),
        )
    }
}

fn run_events_payload(
    runtime: &OrbitRuntime,
    run_id: Option<&str>,
    step_id: Option<&str>,
    event_type: Option<&str>,
    read: RunRead,
) -> CommandOut {
    let run = resolve_run(runtime, run_id, read)?;
    let audit_steps = runtime.collect_run_audit_steps(&run.run_id)?;
    let step_filter = resolve_step_filter(&run, &audit_steps, step_id)?;
    let events = filter_run_audit_events(
        runtime.collect_run_audit_events(&run.run_id)?,
        step_filter.as_deref(),
        event_type,
    );

    let doc = json!({
        "run_id": run.run_id,
        "job_id": run.job_id,
        "events": events.iter().map(RunAuditEvent::json_with_step_id).collect::<Vec<_>>(),
    });

    use crate::output::table::{Column, Table};
    // `orbit run trace <run_id>` prints each event's untruncated payload.
    let mut table = Table::new(vec![
        Column::new("TS").fixed(),
        Column::new("STEP").fixed(),
        Column::new("EVENT_TYPE").fixed(),
        Column::new("SUMMARY"),
    ])
    .empty_message("no audit events recorded");
    for event in &events {
        use comfy_table::Cell;
        table.add_row(vec![
            Cell::new(format_timestamp(event.timestamp)),
            Cell::new(event.step_id.as_deref().unwrap_or("-")),
            Cell::new(event.event_type.as_deref().unwrap_or("-")),
            Cell::new(summarize_audit_event(event)),
        ]);
    }
    Ok(Payload::detail_table(doc, table).into())
}

pub(crate) fn filter_run_audit_events(
    events: Vec<RunAuditEvent>,
    step_filter: Option<&str>,
    event_type: Option<&str>,
) -> Vec<RunAuditEvent> {
    events
        .into_iter()
        .filter(|event| {
            step_filter.is_none_or(|filter| event.step_id.as_deref() == Some(filter))
                && event_type.is_none_or(|filter| event.event_type.as_deref() == Some(filter))
        })
        .collect()
}

pub(crate) fn summarize_audit_event(event: &RunAuditEvent) -> String {
    let raw = &event.raw;
    match event.event_type.as_deref() {
        Some("run.started") => field_summary(raw, "job_name"),
        Some("run.finished") => field_summary(raw, "outcome"),
        Some("step.started") => field_summary(raw, "step_id"),
        Some("step.finished") => join_present(&[
            ("step", raw_str(raw, "step_id")),
            ("outcome", raw_str(raw, "outcome")),
        ]),
        Some("step.skipped") => join_present(&[
            ("step", raw_str(raw, "step_id")),
            ("reason", raw_str(raw, "reason")),
        ]),
        Some("step.denied") => join_present(&[
            ("step", raw_str(raw, "step_id")),
            ("reason", raw_str(raw, "reason")),
        ]),
        Some("activity.started") => join_present(&[
            ("activity", raw_str(raw, "activity_name")),
            ("type", raw_str(raw, "activity_type")),
        ]),
        Some("activity.finished") => join_present(&[
            ("activity", raw_str(raw, "activity_name")),
            ("outcome", raw_str(raw, "outcome")),
        ]),
        Some("cli.invocation.started") => join_present(&[
            ("provider", raw_str(raw, "provider")),
            ("model", raw_str(raw, "model")),
        ]),
        Some("cli.invocation.finished") => join_present(&[
            ("provider", raw_str(raw, "provider")),
            ("exit", raw_i64(raw, "exit_code")),
            ("duration_ms", raw_u64(raw, "duration_ms")),
            ("timed_out", raw_bool(raw, "timed_out")),
        ]),
        Some("tool.denied") => join_present(&[
            ("tool", raw_str(raw, "tool_name")),
            ("reason", raw_str(raw, "reason")),
        ]),
        Some("fs.call.request" | "fs.call.result" | "fs.call.denied") => join_present(&[
            ("op", raw_str(raw, "op")),
            ("path", raw_str(raw, "path")),
            ("allowed", raw_bool(raw, "allowed")),
        ]),
        Some("fanout.dispatched") => join_present(&[
            ("step", raw_str(raw, "step_id")),
            ("workers", raw_u64(raw, "worker_count")),
        ]),
        Some("worker.state") => join_present(&[
            ("step", raw_str(raw, "step_id")),
            ("worker", raw_u64(raw, "worker_index")),
            ("state", raw_str(raw, "state")),
        ]),
        Some("fanin.joined") => join_present(&[
            ("step", raw_str(raw, "step_id")),
            ("collected", raw_u64(raw, "collected")),
            ("failed", raw_u64(raw, "failed")),
        ]),
        _ => event.body_kind.clone().unwrap_or_else(|| "-".to_string()),
    }
}

fn field_summary(raw: &Value, field: &str) -> String {
    raw_str(raw, field).unwrap_or_else(|| "-".to_string())
}

fn raw_str(raw: &Value, field: &str) -> Option<String> {
    raw.get(field).and_then(Value::as_str).map(str::to_string)
}

fn raw_i64(raw: &Value, field: &str) -> Option<String> {
    raw.get(field)
        .and_then(Value::as_i64)
        .map(|value| value.to_string())
}

fn raw_u64(raw: &Value, field: &str) -> Option<String> {
    raw.get(field)
        .and_then(Value::as_u64)
        .map(|value| value.to_string())
}

fn raw_bool(raw: &Value, field: &str) -> Option<String> {
    raw.get(field)
        .and_then(Value::as_bool)
        .map(|value| value.to_string())
}

fn join_present(fields: &[(&str, Option<String>)]) -> String {
    let summary = fields
        .iter()
        .filter_map(|(label, value)| value.as_ref().map(|value| format!("{label}={value}")))
        .collect::<Vec<_>>()
        .join(" ");
    if summary.is_empty() {
        "-".to_string()
    } else {
        summary
    }
}
