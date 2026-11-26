use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_core::runtime::audit::run::RunAuditEvent;
use orbit_core::runtime::audit::run_events::RunEventsSelection;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute, Payload};

use super::format::format_timestamp;
use super::steps::{RunRead, resolve_run, resolve_step_filter};

#[derive(Args)]
#[command(
    after_help = "JSON shape: installed orbit-orchestrate skill `references/run-debugging-audit-output.md`.\n\nExamples:\n  orbit run events\n  orbit run events jrun-20260426-0631\n  orbit run events jrun-20260426-0631 -s implement_one --type cli.invocation.finished --json"
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

    /// Report stored run records as-is: skip stale-run reconciliation, which
    /// finalizes an orphaned pending or running run as interrupted, releases
    /// its task reservations, and settles with their owner the pull claims an
    /// ended drain or leaf left
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
    let events = runtime.run_events_trail(
        &run,
        RunEventsSelection {
            step_id: step_filter.as_deref(),
            event_type,
        },
    )?;

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

/// The plain-text summary of one audit event: the typed body decoded once
/// from the event type and raw payload, then rendered.
pub(crate) fn summarize_audit_event(event: &RunAuditEvent) -> String {
    RunEventSummaryBody::decode(event.event_type.as_deref(), &event.raw).render()
}

/// Decodes one payload field leniently: a missing, null or wrong-typed value
/// is absent, and the event's other fields are kept.
fn lenient<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    Ok(Value::deserialize(deserializer)
        .ok()
        .and_then(|value| T::deserialize(value).ok()))
}

#[derive(Default, Deserialize)]
struct RunStartedFields {
    #[serde(default, deserialize_with = "lenient")]
    job_name: Option<String>,
}

#[derive(Default, Deserialize)]
struct RunFinishedFields {
    #[serde(default, deserialize_with = "lenient")]
    outcome: Option<String>,
}

#[derive(Default, Deserialize)]
struct RunCancelledFields {
    #[serde(default, deserialize_with = "lenient")]
    actor: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    reason: Option<String>,
}

#[derive(Default, Deserialize)]
struct StepStartedFields {
    #[serde(default, deserialize_with = "lenient")]
    step_id: Option<String>,
}

#[derive(Default, Deserialize)]
struct StepFinishedFields {
    #[serde(default, deserialize_with = "lenient")]
    step_id: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    outcome: Option<String>,
}

/// Shared by `step.skipped` and `step.denied`, which render identically.
#[derive(Default, Deserialize)]
struct StepReasonFields {
    #[serde(default, deserialize_with = "lenient")]
    step_id: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    reason: Option<String>,
}

#[derive(Default, Deserialize)]
struct ActivityStartedFields {
    #[serde(default, deserialize_with = "lenient")]
    activity_name: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    activity_type: Option<String>,
}

#[derive(Default, Deserialize)]
struct ActivityFinishedFields {
    #[serde(default, deserialize_with = "lenient")]
    activity_name: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    outcome: Option<String>,
}

#[derive(Default, Deserialize)]
struct CliInvocationStartedFields {
    #[serde(default, deserialize_with = "lenient")]
    provider: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    model: Option<String>,
}

#[derive(Default, Deserialize)]
struct CliInvocationFinishedFields {
    #[serde(default, deserialize_with = "lenient")]
    provider: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    exit_code: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    duration_ms: Option<u64>,
    #[serde(default, deserialize_with = "lenient")]
    timed_out: Option<bool>,
}

#[derive(Default, Deserialize)]
struct ToolDeniedFields {
    #[serde(default, deserialize_with = "lenient")]
    tool_name: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    reason: Option<String>,
}

/// Shared by `fs.call.request`, `fs.call.result` and `fs.call.denied`.
#[derive(Default, Deserialize)]
struct FsCallFields {
    #[serde(default, deserialize_with = "lenient")]
    op: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    path: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    allowed: Option<bool>,
}

#[derive(Default, Deserialize)]
struct FanoutDispatchedFields {
    #[serde(default, deserialize_with = "lenient")]
    step_id: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    worker_count: Option<u64>,
}

#[derive(Default, Deserialize)]
struct WorkerStateFields {
    #[serde(default, deserialize_with = "lenient")]
    step_id: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    worker_index: Option<u64>,
    #[serde(default, deserialize_with = "lenient")]
    state: Option<String>,
}

#[derive(Default, Deserialize)]
struct FaninJoinedFields {
    #[serde(default, deserialize_with = "lenient")]
    step_id: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    collected: Option<u64>,
    #[serde(default, deserialize_with = "lenient")]
    failed: Option<u64>,
}

#[derive(Default, Deserialize)]
struct UnknownEventFields {
    #[serde(default, deserialize_with = "lenient")]
    body_kind: Option<String>,
}

/// The fields the plain audit renderers show, selected by the event type.
/// Every field is optional and independent: a missing or wrong-typed field is
/// absent without dropping its siblings. The raw payload stays on the event
/// for JSON and detail output.
enum RunEventSummaryBody {
    RunStarted(RunStartedFields),
    RunFinished(RunFinishedFields),
    RunCancelled(RunCancelledFields),
    StepStarted(StepStartedFields),
    StepFinished(StepFinishedFields),
    StepSkipped(StepReasonFields),
    StepDenied(StepReasonFields),
    ActivityStarted(ActivityStartedFields),
    ActivityFinished(ActivityFinishedFields),
    CliInvocationStarted(CliInvocationStartedFields),
    CliInvocationFinished(CliInvocationFinishedFields),
    ToolDenied(ToolDeniedFields),
    FsCall(FsCallFields),
    FanoutDispatched(FanoutDispatchedFields),
    WorkerState(WorkerStateFields),
    FaninJoined(FaninJoinedFields),
    Unknown(UnknownEventFields),
}

impl RunEventSummaryBody {
    fn decode(event_type: Option<&str>, raw: &Value) -> Self {
        match event_type {
            Some("run.started") => Self::RunStarted(decode_fields(raw)),
            Some("run.finished") => Self::RunFinished(decode_fields(raw)),
            Some("run.cancelled") => Self::RunCancelled(decode_fields(raw)),
            Some("step.started") => Self::StepStarted(decode_fields(raw)),
            Some("step.finished") => Self::StepFinished(decode_fields(raw)),
            Some("step.skipped") => Self::StepSkipped(decode_fields(raw)),
            Some("step.denied") => Self::StepDenied(decode_fields(raw)),
            Some("activity.started") => Self::ActivityStarted(decode_fields(raw)),
            Some("activity.finished") => Self::ActivityFinished(decode_fields(raw)),
            Some("cli.invocation.started") => Self::CliInvocationStarted(decode_fields(raw)),
            Some("cli.invocation.finished") => Self::CliInvocationFinished(decode_fields(raw)),
            Some("tool.denied") => Self::ToolDenied(decode_fields(raw)),
            Some("fs.call.request" | "fs.call.result" | "fs.call.denied") => {
                Self::FsCall(decode_fields(raw))
            }
            Some("fanout.dispatched") => Self::FanoutDispatched(decode_fields(raw)),
            Some("worker.state") => Self::WorkerState(decode_fields(raw)),
            Some("fanin.joined") => Self::FaninJoined(decode_fields(raw)),
            _ => Self::Unknown(decode_fields(raw)),
        }
    }

    fn render(&self) -> String {
        match self {
            Self::RunStarted(f) => join_present(&[("", shown(&f.job_name))]),
            Self::RunFinished(f) => join_present(&[("", shown(&f.outcome))]),
            Self::RunCancelled(f) => {
                join_present(&[("actor", shown(&f.actor)), ("reason", shown(&f.reason))])
            }
            Self::StepStarted(f) => join_present(&[("", shown(&f.step_id))]),
            Self::StepFinished(f) => {
                join_present(&[("step", shown(&f.step_id)), ("outcome", shown(&f.outcome))])
            }
            Self::StepSkipped(f) | Self::StepDenied(f) => {
                join_present(&[("step", shown(&f.step_id)), ("reason", shown(&f.reason))])
            }
            Self::ActivityStarted(f) => join_present(&[
                ("activity", shown(&f.activity_name)),
                ("type", shown(&f.activity_type)),
            ]),
            Self::ActivityFinished(f) => join_present(&[
                ("activity", shown(&f.activity_name)),
                ("outcome", shown(&f.outcome)),
            ]),
            Self::CliInvocationStarted(f) => {
                join_present(&[("provider", shown(&f.provider)), ("model", shown(&f.model))])
            }
            Self::CliInvocationFinished(f) => join_present(&[
                ("provider", shown(&f.provider)),
                ("exit", shown(&f.exit_code)),
                ("duration_ms", shown(&f.duration_ms)),
                ("timed_out", shown(&f.timed_out)),
            ]),
            Self::ToolDenied(f) => {
                join_present(&[("tool", shown(&f.tool_name)), ("reason", shown(&f.reason))])
            }
            Self::FsCall(f) => join_present(&[
                ("op", shown(&f.op)),
                ("path", shown(&f.path)),
                ("allowed", shown(&f.allowed)),
            ]),
            Self::FanoutDispatched(f) => join_present(&[
                ("step", shown(&f.step_id)),
                ("workers", shown(&f.worker_count)),
            ]),
            Self::WorkerState(f) => join_present(&[
                ("step", shown(&f.step_id)),
                ("worker", shown(&f.worker_index)),
                ("state", shown(&f.state)),
            ]),
            Self::FaninJoined(f) => join_present(&[
                ("step", shown(&f.step_id)),
                ("collected", shown(&f.collected)),
                ("failed", shown(&f.failed)),
            ]),
            Self::Unknown(f) => join_present(&[("", shown(&f.body_kind))]),
        }
    }
}

/// Decodes the typed fields from an object payload; a payload that is not an
/// object has none of them.
fn decode_fields<T: DeserializeOwned + Default>(raw: &Value) -> T {
    if raw.is_object() {
        T::deserialize(raw).unwrap_or_default()
    } else {
        T::default()
    }
}

fn shown<T: ToString>(value: &Option<T>) -> Option<String> {
    value.as_ref().map(ToString::to_string)
}

/// Joins the present fields as `label=value`; an empty label prints the bare
/// value. Nothing present is a dash.
fn join_present(fields: &[(&str, Option<String>)]) -> String {
    let summary = fields
        .iter()
        .filter_map(|(label, value)| {
            value.as_ref().map(|value| {
                if label.is_empty() {
                    value.clone()
                } else {
                    format!("{label}={value}")
                }
            })
        })
        .collect::<Vec<_>>()
        .join(" ");
    if summary.is_empty() {
        "-".to_string()
    } else {
        summary
    }
}
