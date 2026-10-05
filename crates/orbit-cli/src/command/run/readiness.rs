//! `orbit run readiness` read-only auto-drain diagnostic.

use clap::Args;
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_types::workflow::ResourceThrottle;
use serde_json::Value;

use crate::command::{Block, CommandOut, Execute, Payload};

const DEFAULT_LIMIT: usize = 50;

#[derive(Args)]
#[command(
    about = "Explain why backlog tasks can or cannot start in auto-drain",
    override_usage = "orbit run readiness [<TASK_ID>...] [OPTIONS]",
    after_help = "Examples:\n  orbit run readiness\n  orbit run readiness TASK-123 TASK-124\n  orbit run readiness --concurrency 8 --json\n  orbit run readiness --allow-crew opus,sonnet\n\nThis is a read-only snapshot. It does not reserve work, reconcile stale runs,\nsubmit a run, or mutate tasks; an eligible task is not guaranteed to start.\n\n`--allow-crew` previews the same restriction `orbit run auto --allow-crew` would\napply: excluded tasks report `crew_not_allowed` with the crew they would run as,\nand the rest keep filling the free slots.\n\nWhile the host has a shutdown or reboot scheduled, every task reports\n`host_shutdown_scheduled` and the output names the scheduled time and mode.\n\nWhile sustained host resource pressure throttles admissions\n(`[workflow.resource_throttle]`), every task reports `resource_throttled` and\nthe output names the resource, its value, threshold and since-when.\nUnknown readings never throttle; `--json` lists them in\n`capacity.resource_telemetry_unknown`."
)]
pub struct ReadinessCommand {
    /// Optional task IDs to explain. Omit to inspect a bounded backlog snapshot.
    #[arg(value_name = "TASK_ID", num_args = 0..)]
    pub task_ids: Vec<String>,
    /// Leaf-run concurrency to evaluate. Defaults to auto-drain's default (5).
    #[arg(long, value_name = "N")]
    pub concurrency: Option<u32>,
    /// Maximum tasks to explain, including an explicit selection (1-500).
    #[arg(long, default_value_t = DEFAULT_LIMIT, value_name = "N")]
    pub limit: usize,
    /// Evaluate as if the drain were restricted to these configured crews.
    /// Repeatable and comma-separated; omitted, no crew restriction applies.
    #[arg(long = "allow-crew", value_name = "CREW", value_delimiter = ',')]
    pub allow_crew: Vec<String>,
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

impl Execute for ReadinessCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        validate_task_ids(&self.task_ids)?;
        let payload = runtime.workspace_auto_readiness(
            &self.task_ids,
            self.concurrency,
            self.limit,
            &self.allow_crew,
        )?;
        readiness_payload(payload)
    }
}

// pub(super) widened for sibling-layout tests in run/tests/readiness.rs
fn validate_task_ids(task_ids: &[String]) -> Result<(), OrbitError> {
    let mut seen = std::collections::BTreeSet::new();
    for task_id in task_ids {
        if task_id.trim().is_empty() {
            return Err(OrbitError::InvalidInput(
                "task id in readiness selection must not be empty".to_string(),
            ));
        }
        if !seen.insert(task_id) {
            return Err(OrbitError::InvalidInput(format!(
                "duplicate task id '{task_id}' in readiness selection"
            )));
        }
    }
    Ok(())
}

pub(crate) fn readiness_payload(payload: Value) -> CommandOut {
    let lines = readiness_lines(&payload);
    Ok(Payload::blocks(payload, vec![Block::text(lines.join("\n"))]).into())
}

// pub(super) widened for sibling-layout tests in run/tests/readiness.rs
fn readiness_lines(payload: &Value) -> Vec<String> {
    let capacity = &payload["capacity"];
    let mut lines = vec![format!(
        "Snapshot only — eligible does not guarantee a task will start. Active leaf runs: {}/{}; free slots: {}.",
        capacity["active_leaf_runs"], capacity["max_active_leaf_runs"], capacity["free_slots"],
    )];
    if let Some(run_id) = capacity["drain_run_id"].as_str() {
        lines.push(format!("Running drain: {run_id}."));
    }
    if let Some(run_id) = capacity["pull_drain_run_id"].as_str() {
        let stopped = if capacity["pull_drain_admissions_stopped"].as_bool() == Some(true) {
            " (admissions stopped)"
        } else {
            ""
        };
        lines.push(format!("Running pull drain: {run_id}{stopped}."));
    }
    if let Some(queued) = capacity["queued_drains"].as_array() {
        for drain in queued {
            let run_id = drain["run_id"].as_str().unwrap_or("-");
            let completion = drain["completion"].as_str().unwrap_or("review");
            lines.push(format!(
                "Queued drain: {run_id} (completion: {completion})."
            ));
        }
    }
    if let Some(hold) = host_shutdown_hold(&capacity["host_shutdown"]) {
        lines.push(hold);
    }
    // [ORB-13901] Named up front like a shutdown hold: it holds every task.
    if let Ok(throttle) =
        serde_json::from_value::<ResourceThrottle>(capacity["resource_throttle"].clone())
    {
        lines.push(throttle.hold_reason());
    }
    if let Some(phases) = occupancy_phases(&capacity["occupancy"]["phases"]) {
        lines.push(format!("Occupied slots: {phases}."));
    }
    if let Some(approvals) = approval_line(&payload["approvals"]) {
        lines.push(approvals);
    }
    if let Some(tasks) = payload["tasks"].as_array() {
        for task in tasks {
            let task_id = task["task_id"].as_str().unwrap_or("-");
            let reason = task["reason"].as_str().unwrap_or("unknown");
            let eligible = task["eligible"].as_bool().unwrap_or(false);
            let crew = task["crew"]
                .as_str()
                .map(|crew| format!(" crew={crew}"))
                .unwrap_or_default();
            let blocked_by = blocking_task_ids(&task["blocking_task_ids"])
                .map(|ids| format!(" blocked-by={ids}"))
                .unwrap_or_default();
            // A host-OS wait names the host. A local-route before-PR hold
            // names the remedy. Other long repair instructions stay in JSON.
            let host = matches!(reason, "host_os_mismatch" | "local_route_before_pr")
                .then(|| task["detail"].as_str())
                .flatten()
                .map(|detail| format!(": {detail}"))
                .unwrap_or_default();
            lines.push(format!(
                "{task_id}: {} ({reason}){crew}{blocked_by}{host}",
                if eligible { "eligible" } else { "waiting" }
            ));
        }
    }
    lines
}

/// [ORB-14117] An `--approve-proposed` drain's approvals so far, and why the
/// remaining proposed tasks are held.
fn approval_line(approvals: &Value) -> Option<String> {
    if approvals["enabled"].as_bool() != Some(true) {
        return None;
    }
    let held = approvals["held_by_reason"]
        .as_object()
        .map(|reasons| {
            reasons
                .iter()
                .map(|(reason, count)| format!("{reason}={count}"))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|reasons| !reasons.is_empty())
        .map(|reasons| format!(" ({reasons})"))
        .unwrap_or_default();
    Some(format!(
        "Proposed approval: {} approved by drain {}; {} awaiting pilot; {} held{held}.",
        approvals["approved_total"],
        approvals["drain_run_id"].as_str().unwrap_or("-"),
        approvals["awaiting_pilot"],
        approvals["held_total"],
    ))
}

/// [ORB-12968] A pending host shutdown holds every new admission, so it is
/// named up front with its mode and time rather than left to per-task reasons.
fn host_shutdown_hold(shutdown: &Value) -> Option<String> {
    let mode = shutdown["mode"].as_str()?;
    let at = shutdown["scheduled_at"]
        .as_str()
        .unwrap_or("an unknown time");
    Some(format!(
        "Admissions held: host {mode} scheduled for {at}. New runs start again once the \
         schedule is cancelled or the host has restarted; in-flight runs are not touched."
    ))
}

/// [ORB-11973] A saturated drain reads the same whether its slots are working
/// or queued on each other's locks, so name the phases beside the count.
/// Phases that are zero are omitted; an occupancy block with nothing in it
/// prints no line at all.
fn occupancy_phases(phases: &Value) -> Option<String> {
    let named = phases
        .as_object()?
        .iter()
        .filter_map(|(phase, count)| {
            let count = count.as_u64().filter(|count| *count > 0)?;
            Some(format!("{count} {}", phase.replace('_', "-")))
        })
        .collect::<Vec<_>>();
    (!named.is_empty()).then(|| named.join(", "))
}

fn blocking_task_ids(blocking: &Value) -> Option<String> {
    let ids = blocking
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    (!ids.is_empty()).then(|| ids.join(","))
}
