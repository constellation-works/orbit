//! `orbit run readiness` read-only auto-drain diagnostic.

use chrono::Utc;
use clap::Args;
use orbit_core::application::task::ProviderLimitReading;
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_types::workflow::ResourceThrottle;
use serde_json::Value;

use super::format::{WaitingTaskFormatter, waiting_reason};
use crate::command::{Block, CommandOut, Execute, Payload};

const DEFAULT_LIMIT: usize = 50;

#[derive(Args)]
#[command(
    about = "Explain why backlog tasks can or cannot start in auto-drain",
    override_usage = "orbit run readiness [<TASK_ID>...] [OPTIONS]",
    after_help = "This is a read-only snapshot: it does not reserve work, reconcile stale runs,\nsubmit a run, or mutate tasks. Eligible tasks are not guaranteed to start.\nAn explicit --concurrency overrides the live drain's ceiling; --allow-crew\npreviews a crew restriction without changing the drain.\n\nExamples:\n  orbit run readiness\n  orbit run readiness TASK-123 TASK-124\n  orbit run readiness --concurrency 8 --allow-crew sol --json"
)]
pub struct ReadinessCommand {
    /// Optional task IDs to explain. Omit to inspect a bounded backlog snapshot.
    #[arg(value_name = "TASK_ID", num_args = 0..)]
    pub task_ids: Vec<String>,
    /// Leaf-run concurrency to evaluate. Omit for the live local drain's
    /// ceiling, else a live pull drain's, else 5.
    #[arg(long, value_name = "N")]
    pub concurrency: Option<u32>,
    /// Maximum tasks to explain, including an explicit selection (1-500).
    #[arg(long, default_value_t = DEFAULT_LIMIT, value_name = "N")]
    pub limit: usize,
    /// Evaluate as if the drain were restricted to these configured crews.
    /// Repeatable and comma-separated; omitted, the live drain's own crew
    /// restriction applies (none when no drain is live or it has none).
    #[arg(long = "allow-crew", value_name = "CREW", value_delimiter = ',')]
    pub allow_crew: Vec<String>,
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
        readiness_payload(payload, &WaitingTaskFormatter::new(runtime))
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

fn readiness_payload(payload: Value, formatter: &WaitingTaskFormatter) -> CommandOut {
    let lines = readiness_lines(&payload, formatter);
    Ok(Payload::blocks(payload, vec![Block::text(lines.join("\n"))]).into())
}

// pub(super) widened for sibling-layout tests in run/tests/readiness.rs
fn readiness_lines(payload: &Value, formatter: &WaitingTaskFormatter) -> Vec<String> {
    let capacity = &payload["capacity"];
    let mut lines = vec![format!(
        "Snapshot only — eligible does not guarantee a task will start. Active leaf runs: {}/{}; free slots: {}.",
        capacity["active_leaf_runs"], capacity["max_active_leaf_runs"], capacity["free_slots"],
    )];
    if let Some(warning) = payload["validation_env_warning"].as_str() {
        lines.push(format!("Validation environment warning: {warning}"));
    }
    for warning in capacity["build_budget_warnings"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if let Some(message) = warning["message"].as_str() {
            lines.push(format!(
                "Build-budget warning ({}): {message}",
                warning["run_id"].as_str().unwrap_or("drain")
            ));
        }
    }
    if let Some(error) = capacity["build_budget_error"].as_str() {
        lines.push(format!("Build-budget error: {error}"));
    }
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
        // [ORB-16337] Name the ceiling only when this pull drain supplies it.
        // A local drain, an explicit `--concurrency`, or the idle default
        // keep the lines they already print.
        if let Some(source) = capacity["limit_source"]
            .as_str()
            .filter(|source| source.starts_with("pull_"))
        {
            lines.push(format!("Leaf ceiling: {run_id} ({source})."));
        }
        // [ORB-15637] Name why it requests nothing, or an open breaker reads
        // like an idle owner.
        if let Some(refusal) = capacity["pull_drain_refusal"].as_str() {
            lines.push(format!("Pull drain requests held: {refusal}"));
        }
    }
    for drain in capacity["queued_drains"].as_array().into_iter().flatten() {
        lines.extend(queued_drain_lines(drain, capacity));
    }
    if let Some(hold) = host_shutdown_line(
        &capacity["host_shutdown"],
        capacity["host_shutdown_holding"].as_bool(),
    ) {
        lines.push(hold);
    }
    if let Some(source) = capacity["resource_cpu_source"].as_str() {
        lines.push(format!("Resource throttle CPU source: {source}."));
    }
    // [ORB-13901] Named up front like a shutdown hold: it holds every task.
    if let Ok(throttle) =
        serde_json::from_value::<ResourceThrottle>(capacity["resource_throttle"].clone())
    {
        lines.push(throttle.hold_reason());
    }
    let forecast = &capacity["admission_forecast"];
    let last_forecast = &capacity["last_admission_forecast"];
    // Both views carry the drain that recorded them; a finished drain's
    // forecast is labelled as history.
    let origin = super::admission_forecast::ForecastOrigin {
        run_id: forecast["drain_run_id"]
            .as_str()
            .or_else(|| last_forecast["drain_run_id"].as_str()),
        ended: forecast["drain_live"]
            .as_bool()
            .or_else(|| last_forecast["drain_live"].as_bool())
            == Some(false),
        none_live: capacity["drain_run_id"].is_null() && capacity["pull_drain_run_id"].is_null(),
    };
    lines.extend(super::admission_forecast::admission_lines(
        forecast,
        last_forecast,
        &origin,
        |id| {
            forecast["task_titles"][id]
                .as_str()
                .or_else(|| last_forecast["task_titles"][id].as_str())
                .map(str::to_string)
        },
    ));
    lines.extend(provider_limit_lines(&payload["provider_limits"]));
    lines.push(crew_performance_line(&payload["crew_performance"]));
    let now = Utc::now();
    if let Ok(providers) = serde_json::from_value::<
        Vec<orbit_core::application::task::ProviderUsageStatus>,
    >(payload["provider_pressures"].clone())
    {
        lines.extend(providers.iter().map(|provider| {
            format!(
                "Provider pressure: {} {}",
                provider.label(),
                provider.usage.describe(now)
            )
        }));
    }
    if let Ok(crews) = serde_json::from_value::<Vec<orbit_core::application::task::CrewUsagePressure>>(
        payload["crew_pressures"].clone(),
    ) {
        lines.extend(
            crews
                .iter()
                .map(|crew| format!("Crew pressure: {} {}", crew.crew, crew.usage.describe(now))),
        );
    }
    if let Some(budget) = cpu_light_budget_line(&capacity["cpu_light_budget"]) {
        lines.push(budget);
    }
    if let Some(phases) = occupancy_phases(&capacity["occupancy"]["phases"]) {
        lines.push(format!("Occupied slots: {phases}."));
    }
    lines.extend(approval_lines(&payload["approvals"]));
    lines.extend(out_of_run_auto_fix_lines(&payload["out_of_run_auto_fix"]));
    if let Some(tasks) = payload["tasks"].as_array() {
        for task in tasks {
            let task_id = task["task_id"].as_str().unwrap_or("-");
            let reason = waiting_reason(task["reason"].as_str().unwrap_or("unknown"));
            let eligible = task["eligible"].as_bool().unwrap_or(false);
            let crew = task["crew"]
                .as_str()
                .map(|crew| format!(" crew={crew}"))
                .unwrap_or_default();
            let blocked_by = formatter.blockers(task);
            // [ORB-15514] A blocker a follower's execution claim holds clears
            // when that claim settles, on the machine named here.
            let claimed = blocking_claims(&task["blocking_claims"])
                .map(|claims| format!(" claimed-on={claims}"))
                .unwrap_or_default();
            // A host-OS wait names the host, a browser-kit wait the install
            // command, and a native-OS requirement the tag to add. A local-route review hold names the remedy, and
            // a provider limit its window, reset and skipped crews. A task
            // with no footprint names its wait and fix [ORB-15191], and an
            // unverified review finding the verification job to run. Other
            // long repair instructions stay in JSON.
            let host = matches!(
                reason,
                "host_os_mismatch"
                    | "browser_kit_missing"
                    | "native_os_required"
                    | "unverified_review_finding"
                    | "local_route_before_pr"
                    | "local_route_before_landing"
                    | "provider_limit"
                    | "awaiting_footprint"
                    | "awaiting_exclusive_slot"
            )
            .then(|| task["detail"].as_str())
            .flatten()
            .map(|detail| format!(": {detail}"))
            .unwrap_or_default();
            // A host-OS wait also names the registered hosts of that OS and
            // whether each is pulling, and one hidden behind an unmet
            // dependency is named beside it.
            let host = match (task["host_os_wait"].as_str(), task["host_fleet"].as_str()) {
                (Some(wait), Some(fleet)) => format!("{host}; then {wait}; {fleet}"),
                (Some(wait), None) => format!("{host}; then {wait}"),
                (None, Some(fleet)) => format!("{host}; {fleet}"),
                (None, None) => host,
            };
            // [ORB-14624] Which tasks a CPU-only throttle still admits, and
            // which jump the queue before their frozen batch expires.
            let light = if task["cpu_light"].as_bool() == Some(true) {
                " cpu-light"
            } else {
                ""
            };
            let deadline = task["frozen_batch_deadline"]
                .as_str()
                .map(|deadline| format!(" frozen-batch-deadline={deadline}"))
                .unwrap_or_default();
            // A task resuming a preserved publication waits on no context
            // lock: its next run adopts the existing pull request first.
            let published = match &task["published_delivery"] {
                Value::Object(delivery) => match delivery["pull_request"].as_str() {
                    Some(number) => format!(" resuming published delivery (PR #{number})"),
                    None => " resuming published delivery".to_string(),
                },
                _ => String::new(),
            };
            let observation = &task["admission_forecast"];
            let profile = observation["profile_source"]
                .as_str()
                .map(|source| format!(" profile-source={source} (last admission pass)"))
                .unwrap_or_default();
            let forecast_wait = if observation["decision"] == "skipped" {
                let shadow = if observation["mode"] == "shadow" {
                    "shadow would wait"
                } else {
                    "forecast wait"
                };
                format!(
                    "; {shadow}: {}",
                    super::admission_forecast::skip_detail(observation)
                )
            } else {
                String::new()
            };
            lines.push(format!(
                "{task_id}: {} ({reason}){published}{light}{deadline}{crew}{blocked_by}{claimed}{host}{profile}{forecast_wait}",
                if eligible { "eligible" } else { "waiting" }
            ));
        }
    }
    if let (Some(tasks), Some(total)) = (payload["tasks"].as_array(), payload["total"].as_u64())
        && (tasks.len() as u64) < total
    {
        lines.push(format!(
            "showing {} of {total} backlog tasks; use --limit N or name task ids",
            tasks.len()
        ));
    }
    lines
}

/// Live repairs and the latest drain pass's skipped repair candidates.
fn out_of_run_auto_fix_lines(repairs: &Value) -> Vec<String> {
    repairs
        .as_array()
        .into_iter()
        .flatten()
        .map(|repair| {
            let tasks = repair["task_ids"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            if repair["state"] == "skipped" {
                let ended = if repair["drain_live"].as_bool() == Some(false) {
                    "ended drain"
                } else {
                    "drain"
                };
                return format!(
                    "Out-of-run auto-fix skipped: {tasks} PR #{} ({}): {} — {} (latest pass of {ended} {}, {}).",
                    repair["pr_number"].as_str().unwrap_or("-"),
                    repair["blocker"].as_str().unwrap_or("blocker unknown"),
                    repair["reason"].as_str().unwrap_or("unknown"),
                    repair["detail"].as_str().unwrap_or("-"),
                    repair["drain_run_id"].as_str().unwrap_or("-"),
                    repair["observed_at"].as_str().unwrap_or("time unknown"),
                );
            }
            format!(
                "Out-of-run auto-fix: {tasks} PR #{} ({}) in run {} ({}), resumed from {}.",
                repair["pr_number"].as_str().unwrap_or("-"),
                repair["blocker"].as_str().unwrap_or("-"),
                repair["run_id"].as_str().unwrap_or("-"),
                repair["state"].as_str().unwrap_or("-"),
                repair["source_run_id"].as_str().unwrap_or("-"),
            )
        })
        .collect()
}

/// [ORB-14117] An `--approve-proposed` drain's approvals so far, and why the
/// remaining proposed tasks are held. A task task-pilot verified as already
/// fixed gets its own line with the pilot's evidence.
fn approval_lines(approvals: &Value) -> Vec<String> {
    if approvals["enabled"].as_bool() != Some(true) {
        return Vec::new();
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
    let closed = match approvals["closed_total"].as_u64() {
        Some(total) if total > 0 => format!("; {total} closed as already fixed"),
        _ => String::new(),
    };
    let mut lines = vec![format!(
        "Proposed approval: {} approved by drain {}{closed}; {} awaiting pilot; {} held{held}.",
        approvals["approved_total"],
        approvals["drain_run_id"].as_str().unwrap_or("-"),
        approvals["awaiting_pilot"],
        approvals["held_total"],
    )];
    for task in approvals["held"].as_array().into_iter().flatten() {
        if task["reason"] == "pilot_verified_no_diff" {
            lines.push(format!(
                "  Task {}: pilot_verified_no_diff ({})",
                task["task_id"].as_str().unwrap_or("-"),
                task["detail"].as_str().unwrap_or("no evidence recorded"),
            ));
        }
        if task["reason"] == "warnings" {
            for finding in task["findings"].as_array().into_iter().flatten() {
                let Some(text) = finding["text"].as_str().filter(|text| !text.is_empty()) else {
                    continue;
                };
                lines.push(format!(
                    "  Task {}: {}: {text}",
                    task["task_id"].as_str().unwrap_or("-"),
                    finding["kind"].as_str().unwrap_or("warning"),
                ));
            }
        }
    }
    lines
}

/// [ORB-14698] One line per provider usage reading that keeps crews out of
/// admission: its window, use against the threshold, reset and the crews
/// skipped until then.
fn provider_limit_lines(readings: &Value) -> Vec<String> {
    let now = Utc::now();
    serde_json::from_value::<Vec<ProviderLimitReading>>(readings.clone())
        .unwrap_or_default()
        .into_iter()
        .filter(|reading| reading.gated)
        .map(|reading| format!("Provider limits: {}", reading.skipped_line(now)))
        .collect()
}

/// The crew-performance snapshot draws read: when it was computed, its
/// window and type list, or the reason a draw ignores it.
fn crew_performance_line(crew_performance: &Value) -> String {
    let mode = crew_performance["mode"].as_str().unwrap_or("off");
    let snapshot = &crew_performance["snapshot"];
    if snapshot.is_null() {
        let reason = crew_performance["unusable_reason"]
            .as_str()
            .unwrap_or("missing");
        return format!("Crew performance snapshot: {reason} (mode {mode}).");
    }
    let types = snapshot["types"]
        .as_array()
        .map(|types| {
            types
                .iter()
                .filter_map(|entry| entry["name"].as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|types| !types.is_empty())
        .unwrap_or_else(|| "other only".to_string());
    let detail = format!(
        "computed {} (window {}d, types: {types}; {} cells)",
        snapshot["computed_at"].as_str().unwrap_or("-"),
        snapshot["window_days"],
        snapshot["cells"],
    );
    match crew_performance["unusable_reason"].as_str() {
        Some(reason) => {
            format!("Crew performance snapshot: unusable ({reason}); {detail} (mode {mode}).")
        }
        None => format!("Crew performance snapshot: {detail} (mode {mode})."),
    }
}

/// [ORB-14624] While CPU alone throttles, the reserved light slots are the
/// only ones open, so say how many are left.
fn cpu_light_budget_line(budget: &Value) -> Option<String> {
    if budget["applies"].as_bool() != Some(true) {
        return None;
    }
    Some(format!(
        "CPU-light budget: {} of {} reserved slots in use, {} left; only cpu-light \
         auto-tasks start while CPU alone is held.",
        budget["active"], budget["reserved"], budget["remaining"],
    ))
}

/// A drain waiting for the live window's slot: what its window will admit,
/// when it starts, the `execution.env.pass` names its submission lacked, and
/// the Claude crews in its window that will start with no worker credential.
fn queued_drain_lines(drain: &Value, capacity: &Value) -> Vec<String> {
    let names = |value: &Value| {
        value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let run_id = drain["run_id"].as_str().unwrap_or("-");
    let completion = drain["completion"].as_str().unwrap_or("review");
    let ceiling = &drain["max_active_leaf_runs"];
    let crews = Some(names(&drain["allowed_crews"]))
        .filter(|crews| !crews.is_empty())
        .unwrap_or_else(|| "all".to_string());
    let window = match drain["for_seconds"].as_u64() {
        Some(seconds) if seconds > 0 => {
            super::format::format_duration(Some(seconds.saturating_mul(1000)))
        }
        _ => "single pass".to_string(),
    };
    let starts = match (
        capacity["drain_run_id"].as_str(),
        capacity["ends_at"].as_str(),
    ) {
        (Some(live), Some(ends_at)) => format!("starts when {live} ends (window closes {ends_at})"),
        (Some(live), None) => format!("starts when {live} ends"),
        _ => "waiting for its slot".to_string(),
    };
    let mut lines = vec![format!(
        "Queued drain: {run_id} (completion: {completion}; ceiling: {ceiling}; crews: {crews}; window: {window}); {starts}."
    )];
    let unset = names(&drain["env_pass_unset"]);
    if !unset.is_empty() {
        lines.push(format!("  Unset env: {unset}."));
    }
    let uncredentialed = names(&drain["uncredentialed_crews"]);
    if !uncredentialed.is_empty() {
        lines.push(format!(
            "  Starts without a Claude worker credential for crews: {uncredentialed}."
        ));
    }
    lines
}

/// [ORB-12968] A pending host shutdown is named up front with its mode and
/// time either way. Admissions are held only inside the horizon; a missing
/// `host_shutdown_holding` flag is treated as holding.
fn host_shutdown_line(shutdown: &Value, holding: Option<bool>) -> Option<String> {
    let mode = shutdown["mode"].as_str()?;
    let at = shutdown["scheduled_at"]
        .as_str()
        .unwrap_or("an unknown time");
    if holding == Some(false) {
        return Some(format!(
            "Host {mode} scheduled for {at}. Admissions continue until \
             machine.host_shutdown_hold_minutes of that time; in-flight runs are not touched."
        ));
    }
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

fn blocking_claims(claims: &Value) -> Option<String> {
    let named = claims
        .as_array()?
        .iter()
        .filter_map(|claim| {
            let task_id = claim["task_id"].as_str()?;
            let machine = claim["machine_name"]
                .as_str()
                .or_else(|| claim["machine_id"].as_str())?;
            Some(format!("{task_id}@{machine}"))
        })
        .collect::<Vec<_>>();
    (!named.is_empty()).then(|| named.join(","))
}
