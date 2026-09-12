//! Triage deterministic actions [ORB-10129]: the bounded, non-agent half of
//! the `task_triage_pipeline` default workflow.
//!
//! `list_triage_candidates` materializes the set of blocked tasks that are
//! attributable to a terminally-failed job run (the coupling stamped by
//! `worktree_setup` and flipped to `blocked` by
//! `runtime::task::block_on_run_failure`). `apply_triage_dispositions`
//! applies the triage agent's per-task verdicts under hard deterministic
//! bounds: only listed candidates may be touched, only `environmental`
//! classifications may re-backlog, and a durable per-task re-backlog budget
//! (counted from `triage_rebacklogged` history events) stops the
//! blocked → backlog → blocked ping-pong. Agent dispositions are advisory;
//! lifecycle transitions remain bounded here or require human action.

use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use orbit_engine::DispatchError;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_store::friction_store::FrictionAddParams;
use orbit_types::task::{Task, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::JobRun;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::runtime::{failed_run_error_context, is_workflow_failure_state};

/// History event recorded when triage returns a blocked task to the backlog.
pub(crate) const TRIAGE_REBACKLOGGED_EVENT: &str = "triage_rebacklogged";
/// History event recorded when triage attaches a diagnosis but leaves the
/// task blocked for a human decision.
pub(crate) const TRIAGE_DIAGNOSIS_EVENT: &str = "triage_diagnosis";
/// History event recorded when triage exhausts the re-backlog budget and
/// permanently stops touching the task.
pub(crate) const TRIAGE_GAVE_UP_EVENT: &str = "triage_gave_up";

/// Default cap on triage-initiated re-backlogs per task. Kept in sync with
/// the `max_rebacklogs` default in
/// `crates/orbit-core/assets/jobs/task_triage_pipeline.yaml`.
const DEFAULT_MAX_REBACKLOGS: u64 = 2;
/// Ceiling on the configurable budget — triage is a second chance, not a
/// retry loop.
const MAX_REBACKLOGS_CEILING: u64 = 10;
const DEFAULT_MAX_TASKS: u64 = 20;
const MAX_TASKS_CEILING: u64 = 100;
/// Cap on agent-supplied prose folded into task notes.
const MAX_AGENT_TEXT_CHARS: usize = 600;

fn action_failed(action: &str, message: impl Into<String>) -> DispatchError {
    DispatchError::DeterministicActionFailed {
        action: action.to_string(),
        message: message.into(),
    }
}

fn rebacklog_budget(input: &Value) -> u64 {
    input
        .get("max_rebacklogs")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_MAX_REBACKLOGS)
        .min(MAX_REBACKLOGS_CEILING)
}

fn triage_rebacklog_count(history: &[TaskHistoryEntry]) -> u64 {
    history
        .iter()
        .filter(|entry| entry.event == TRIAGE_REBACKLOGGED_EVENT)
        .count() as u64
}

fn triage_already_gave_up(history: &[TaskHistoryEntry]) -> bool {
    history
        .iter()
        .any(|entry| entry.event == TRIAGE_GAVE_UP_EVENT)
}

fn triage_already_diagnosed_run(history: &[TaskHistoryEntry], run_id: &str) -> bool {
    let note_prefix = format!("triage: failed run {run_id} ");
    history.iter().any(|entry| {
        entry.event == TRIAGE_DIAGNOSIS_EVENT
            && entry
                .note
                .as_deref()
                .is_some_and(|note| note.starts_with(&note_prefix))
    })
}

/// Bound agent-supplied prose before it lands in a durable task note.
fn clamp_agent_text(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.chars().count() <= MAX_AGENT_TEXT_CHARS {
        return trimmed.to_string();
    }
    let clipped: String = trimmed.chars().take(MAX_AGENT_TEXT_CHARS).collect();
    format!("{clipped}…")
}

/// Mark a task as beyond triage: a durable `triage_gave_up` history event
/// (idempotent — recorded at most once) plus a friction so the systemic
/// cause surfaces to a human. The task's status is left untouched
/// (`blocked`). Best-effort: a failed write is logged, never fatal, so one
/// task cannot wedge a whole triage pass.
fn mark_triage_gave_up(
    runtime: &OrbitRuntime,
    task: &Task,
    run_id: &str,
    rebacklog_count: u64,
    max_rebacklogs: u64,
) {
    let note = format!(
        "triage gave up after {rebacklog_count}/{max_rebacklogs} re-backlog attempts: \
         run {run_id} failed again; leaving blocked for a human decision."
    );
    if let Err(error) = runtime.apply_task_automation_update(
        &task.id,
        TaskAutomationUpdate {
            status_event: Some(TRIAGE_GAVE_UP_EVENT.to_string()),
            status_note: Some(note),
            ..TaskAutomationUpdate::default()
        },
    ) {
        tracing::warn!(
            task_id = %task.id,
            run_id,
            "triage failed to record gave-up event: {error}"
        );
        return;
    }
    if let Err(error) = crate::runtime::friction::store_for(runtime).and_then(|frictions| {
        frictions.add(FrictionAddParams {
            model: "system".to_string(),
            title: Some(format!(
                "Triage exhausted its re-backlog budget for task {}",
                task.id
            )),
            body: format!(
                "Triage exhausted its re-backlog budget ({max_rebacklogs}) for task {} — \
                 its workflow runs keep failing (latest: {run_id}). The task stays \
                 blocked until a human decides; the repeated failure likely has a \
                 systemic cause worth fixing.",
                task.id
            ),
            tags: vec!["lifecycle".to_string()],
            during_task: Some(task.id.to_string()),
            created_at: Utc::now(),
        })
    }) {
        tracing::warn!(
            task_id = %task.id,
            run_id,
            "triage failed to file gave-up friction: {error}"
        );
    }
}

fn candidate_json(task: &Task, run: &JobRun, rebacklog_count: u64, max_rebacklogs: u64) -> Value {
    let (error_code, error_message) = failed_run_error_context(run);
    json!({
        "task_id": task.id,
        "title": task.title,
        "task_revision": task.updated_at,
        "run_id": run.run_id,
        "job_id": run.job_id,
        "run_state": run.state.to_string(),
        "error_code": error_code,
        "error_message": error_message,
        "rebacklog_count": rebacklog_count,
        "max_rebacklogs": max_rebacklogs,
    })
}

/// Materialize the blocked tasks eligible for triage: `status: blocked`,
/// coupled to a job run (`job_run_id`), and that run terminalized as a
/// workflow failure (`failed` / `timeout` / `cancelled`). Everything else is
/// out of bounds by construction: a task a human blocked by hand has no
/// coupled run (or a non-failed one) and is never listed, so the agent step
/// physically cannot see it. Tasks whose re-backlog budget is exhausted are
/// diverted to the gave-up path here (durable note + friction, once) and
/// reported under `exhausted` instead of `candidates`. An implicit scan also
/// suppresses a task already diagnosed for its currently-coupled failed run;
/// a new run id is fresh evidence, and explicit `task_ids` bypass suppression.
///
/// Reuses `reconcile_stale_job_runs` up front so dead-owner runs are settled
/// (`interrupted`, which keeps their tasks resumable and out of triage)
/// before run states are read.
pub(super) fn list_triage_candidates(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let claim = crate::application::automation::members::claim(runtime, input)
        .map_err(|error| action_failed(action, error.to_string()))?;
    let max_rebacklogs = rebacklog_budget(input);
    let max_tasks = input
        .get("max_tasks")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_MAX_TASKS)
        .clamp(1, MAX_TASKS_CEILING) as usize;
    let explicit_task_ids: BTreeSet<String> = input
        .get("task_ids")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(|id| id.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default();

    // Settle stale runs first (dead-owner `running` → `interrupted`) so the
    // candidate scan below reads reconciled run states. Best-effort: triage
    // must still run when the orphan scan hiccups.
    let reconciled_stale_runs = runtime
        .reconcile_stale_job_runs(None)
        .unwrap_or_else(|error| {
            tracing::warn!("triage stale-run reconcile failed; continuing: {error}");
            0
        });

    let mut blocked: Vec<Task> = runtime
        .stores()
        .tasks()
        .list_tasks()
        .map_err(|error| action_failed(action, format!("list tasks: {error}")))?
        .into_iter()
        .filter(|task| task.status == TaskStatus::Blocked)
        .filter(|task| explicit_task_ids.is_empty() || explicit_task_ids.contains(task.id.as_str()))
        .collect();
    blocked.sort_by(|a, b| a.id.cmp(&b.id));

    let mut candidates = Vec::new();
    let mut exhausted = Vec::new();
    for task in &blocked {
        // Human intent wins: no coupled run means a human parked this task.
        let Some(run_id) = task.job_run_id.as_deref() else {
            continue;
        };
        let Some(run) = runtime
            .get_job_run_backend(run_id)
            .map_err(|error| action_failed(action, format!("load run {run_id}: {error}")))?
        else {
            continue;
        };
        // Only runs that terminalized as workflow failures are triageable.
        // A succeeded/running/interrupted run means the block did not come
        // from the failure coupling — leave it alone.
        if !is_workflow_failure_state(run.state) {
            continue;
        }
        if claim.is_some() {
            match crate::application::automation::incidents::observe(runtime, task) {
                Ok((key, _)) if claim.as_ref().is_some_and(|claim| claim.member.key == key) => {}
                _ => continue,
            }
        }
        let history = runtime
            .get_task_history(&task.id)
            .map_err(|error| action_failed(action, format!("history for {}: {error}", task.id)))?;
        if explicit_task_ids.is_empty() && triage_already_diagnosed_run(&history, run_id) {
            continue;
        }
        let rebacklog_count = triage_rebacklog_count(&history);
        if rebacklog_count >= max_rebacklogs {
            if !triage_already_gave_up(&history) {
                mark_triage_gave_up(runtime, task, run_id, rebacklog_count, max_rebacklogs);
            }
            exhausted.push(json!({
                "task_id": task.id,
                "run_id": run_id,
                "rebacklog_count": rebacklog_count,
            }));
            continue;
        }
        // [ORB-11332] A failed run admitted under an operation-mode grant
        // draws its diagnosis from the task's aggregate recovery allowance,
        // reserved here before the triage agent is dispatched. A spent
        // allowance is an escalation, not a candidate.
        let reservation =
            crate::application::operation::triage_recovery_reservation(runtime, &task.id, &run)
                .map_err(|error| action_failed(action, error.to_string()))?;
        if let Some(reason) = reservation
            .as_ref()
            .and_then(|reservation| reservation.exhausted)
        {
            exhausted.push(json!({
                "task_id": task.id,
                "run_id": run_id,
                "rebacklog_count": rebacklog_count,
                "reason": reason,
            }));
            continue;
        }
        if candidates.len() < max_tasks {
            let mut candidate = candidate_json(task, &run, rebacklog_count, max_rebacklogs);
            if let Some(episode) = reservation.and_then(|reservation| reservation.episode)
                && let Some(object) = candidate.as_object_mut()
            {
                object.insert("recovery_episode".to_string(), json!(episode));
            }
            candidates.push(candidate);
        }
    }

    let task_ids: Vec<Value> = candidates
        .iter()
        .filter_map(|candidate| candidate.get("task_id").cloned())
        .collect();
    // Keep this Rust serialization contract in sync with
    // crates/orbit-core/assets/activities/list_triage_candidates.yaml.
    Ok(json!({
        "state_automation": claim,
        "candidates": candidates,
        "candidate_count": task_ids.len(),
        "task_ids": task_ids,
        "exhausted": exhausted,
        "reconciled_stale_runs": reconciled_stale_runs,
    }))
}

const CLASSIFICATIONS: &[&str] = &["environmental", "task_defect", "code_defect", "unknown"];

struct CandidateSnapshot {
    run_id: String,
    revision: Option<String>,
    incident: Option<String>,
}

struct DispositionOutcome {
    action: &'static str,
    reason: Option<String>,
}

impl DispositionOutcome {
    fn applied(action: &'static str) -> Self {
        Self {
            action,
            reason: None,
        }
    }

    fn skipped(reason: impl Into<String>) -> Self {
        Self {
            action: "skipped",
            reason: Some(reason.into()),
        }
    }
}

// ADR-0216: the agent's dispositions are advisory data — this action is the
// only lifecycle writer, and its bounds (candidates-only, environmental-only
// re-backlog, durable budget) must not be weakened.
/// Apply the triage agent's dispositions under deterministic bounds:
///
/// - only task ids present in `candidates` (the deterministic listing this
///   run produced) can be touched — the agent cannot smuggle in other tasks;
/// - the task must still be `blocked` and still coupled to the same run the
///   listing saw, which makes the write idempotent and safe under overlap
///   (a concurrent transition, human or otherwise, turns ours into a skip);
/// - re-backlog is honored only for the `environmental` classification and
///   only while the durable re-backlog budget has room — on exhaustion the
///   task takes the gave-up path instead;
/// - every other classification records a `triage_diagnosis` history note
///   and leaves the task blocked.
pub(super) fn apply_triage_dispositions(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let claim = crate::application::automation::members::claim(runtime, input)
        .map_err(|error| action_failed(action, error.to_string()))?;
    let dispositions = input
        .get("dispositions")
        .and_then(Value::as_array)
        .ok_or_else(|| action_failed(action, "missing `dispositions` array"))?;
    let candidates = input
        .get("candidates")
        .and_then(Value::as_array)
        .ok_or_else(|| action_failed(action, "missing `candidates` array"))?;
    let max_rebacklogs = rebacklog_budget(input);

    let candidate_run_by_task: BTreeMap<String, CandidateSnapshot> = candidates
        .iter()
        .filter_map(|candidate| {
            Some((
                candidate.get("task_id")?.as_str()?.to_string(),
                CandidateSnapshot {
                    run_id: candidate.get("run_id")?.as_str()?.to_string(),
                    revision: candidate
                        .get("task_revision")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    incident: claim.as_ref().map(|claim| claim.member.key.clone()),
                },
            ))
        })
        .collect();

    if let Some(claim) = &claim {
        let expected = claim
            .member
            .task_ids
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let actual = dispositions
            .iter()
            .filter_map(|v| v.get("task_id").and_then(Value::as_str))
            .collect::<BTreeSet<_>>();
        if actual != expected || dispositions.len() != expected.len() {
            return Err(action_failed(
                action,
                "state triage requires exactly one disposition per captured task",
            ));
        }
    }

    let mut results = Vec::new();
    let mut counts: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut seen_task_ids = BTreeSet::new();
    for disposition in dispositions {
        let Some(task_id) = disposition.get("task_id").and_then(Value::as_str) else {
            record(&mut results, &mut counts, "<missing task_id>", {
                DispositionOutcome::skipped("disposition has no `task_id`")
            });
            continue;
        };
        let outcome = apply_one_disposition(
            runtime,
            task_id,
            disposition,
            &candidate_run_by_task,
            &mut seen_task_ids,
            max_rebacklogs,
        );
        // [ORB-11332] The diagnosis and its disposition are one episode;
        // settle its wall time now so a requeue cannot start a fresh count.
        if let Some(snapshot) = candidate_run_by_task.get(task_id)
            && let Err(error) = crate::application::operation::settle_triage_episode(
                runtime,
                task_id,
                &snapshot.run_id,
            )
        {
            tracing::warn!(
                task_id,
                "triage recovery episode settlement failed: {error}"
            );
        }
        record(&mut results, &mut counts, task_id, outcome);
    }

    let member_evidence = claim
        .filter(|_| results.iter().all(|r| r["action"] != "skipped") && !results.is_empty())
        .map(
            |claim| orbit_types::workflow::automation::members::MemberEvidence {
                action_id: claim.action_id.unwrap_or_default(),
                attempt_id: claim.id,
                member_key: claim.member.key,
                input_fingerprint: claim.member.fingerprint.clone(),
                resulting_fingerprint: claim.member.fingerprint,
                ready: false,
                result: json!(results),
            },
        );
    Ok(json!({
        "member_evidence": member_evidence,
        "results": results,
        "rebacklogged_count": counts.get("rebacklogged").copied().unwrap_or(0),
        "diagnosed_count": counts.get("diagnosed").copied().unwrap_or(0),
        "gave_up_count": counts.get("gave_up").copied().unwrap_or(0),
        "skipped_count": counts.get("skipped").copied().unwrap_or(0),
    }))
}

fn record(
    results: &mut Vec<Value>,
    counts: &mut BTreeMap<&'static str, u64>,
    task_id: &str,
    outcome: DispositionOutcome,
) {
    *counts.entry(outcome.action).or_default() += 1;
    results.push(json!({
        "task_id": task_id,
        "action": outcome.action,
        "reason": outcome.reason,
    }));
}

fn apply_one_disposition(
    runtime: &OrbitRuntime,
    task_id: &str,
    disposition: &Value,
    candidate_run_by_task: &BTreeMap<String, CandidateSnapshot>,
    seen_task_ids: &mut BTreeSet<String>,
    max_rebacklogs: u64,
) -> DispositionOutcome {
    let mut outcome = None;
    let mut apply = || {
        outcome = Some(apply_one_disposition_locked(
            runtime,
            task_id,
            disposition,
            candidate_run_by_task,
            seen_task_ids,
            max_rebacklogs,
        ));
        Ok(())
    };
    match runtime
        .stores()
        .tasks()
        .with_task_write_lock(task_id, &mut apply)
    {
        Ok(()) => outcome.unwrap_or_else(|| DispositionOutcome::skipped("task write did not run")),
        Err(error) => DispositionOutcome::skipped(error.to_string()),
    }
}

fn apply_one_disposition_locked(
    runtime: &OrbitRuntime,
    task_id: &str,
    disposition: &Value,
    candidate_run_by_task: &BTreeMap<String, CandidateSnapshot>,
    seen_task_ids: &mut BTreeSet<String>,
    max_rebacklogs: u64,
) -> DispositionOutcome {
    if !seen_task_ids.insert(task_id.to_string()) {
        return DispositionOutcome::skipped("duplicate disposition for this task");
    }
    let Some(candidate) = candidate_run_by_task.get(task_id) else {
        // Structural bound: the agent may only dispose of tasks the
        // deterministic listing produced.
        return DispositionOutcome::skipped("task is not a triage candidate of this run");
    };
    let expected_run_id = &candidate.run_id;
    let expected_revision = &candidate.revision;
    let classification = disposition
        .get("classification")
        .and_then(Value::as_str)
        .filter(|value| CLASSIFICATIONS.contains(value))
        .unwrap_or("unknown");
    let requested_rebacklog =
        disposition.get("disposition").and_then(Value::as_str) == Some("rebacklog");
    let diagnosis = disposition
        .get("diagnosis")
        .and_then(Value::as_str)
        .map(clamp_agent_text)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "no diagnosis provided".to_string());
    let mitigation = disposition
        .get("mitigation")
        .and_then(Value::as_str)
        .map(clamp_agent_text)
        .filter(|value| !value.is_empty());

    let task = match runtime.get_task(task_id) {
        Ok(task) => task,
        Err(error) => {
            return DispositionOutcome::skipped(format!("load task failed: {error}"));
        }
    };
    // Idempotency / overlap guard: if anything (a concurrent triage run, a
    // ship sweep admission, a human) moved the task since the listing, our
    // write becomes a skip instead of a double transition.
    if task.status != TaskStatus::Blocked {
        return DispositionOutcome::skipped(format!(
            "task is no longer blocked (status: {})",
            task.status
        ));
    }
    if task.job_run_id.as_deref() != Some(expected_run_id.as_str()) {
        return DispositionOutcome::skipped("task run coupling changed since listing");
    }

    if expected_revision.as_ref().is_some_and(|revision| {
        &task
            .updated_at
            .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)
            != revision
    }) {
        return DispositionOutcome::skipped("task changed since candidate capture");
    }
    match crate::application::automation::incidents::failure_coupled(
        runtime,
        &task,
        expected_run_id,
    ) {
        Ok(true) => {}
        _ => return DispositionOutcome::skipped("failure coupling or human intent changed"),
    }
    if runtime
        .get_job_run_backend(expected_run_id)
        .ok()
        .flatten()
        .is_none_or(|run| {
            !matches!(
                run.state,
                orbit_types::workflow::JobRunState::Failed
                    | orbit_types::workflow::JobRunState::Timeout
            ) || run.job_id == "task_triage_pipeline"
        })
    {
        return DispositionOutcome::skipped("run is no longer an eligible execution failure");
    }

    if let Some(expected) = &candidate.incident {
        match crate::application::automation::incidents::members(runtime) {
            Ok(inventory) if inventory.contains_key(expected) => {}
            _ => return DispositionOutcome::skipped("incident recovery or membership changed"),
        }
        match crate::application::automation::incidents::observe(runtime, &task) {
            Ok((key, _)) if &key == expected => {}
            _ => return DispositionOutcome::skipped("incident recovery or eligibility changed"),
        }
    }

    if requested_rebacklog && classification == "environmental" {
        let history = match runtime.get_task_history(task_id) {
            Ok(history) => history,
            Err(error) => {
                return DispositionOutcome::skipped(format!("load task history failed: {error}"));
            }
        };
        let rebacklog_count = triage_rebacklog_count(&history);
        if rebacklog_count >= max_rebacklogs {
            if !triage_already_gave_up(&history) {
                mark_triage_gave_up(
                    runtime,
                    &task,
                    expected_run_id,
                    rebacklog_count,
                    max_rebacklogs,
                );
            }
            return DispositionOutcome::applied("gave_up");
        }
        let mitigation_note = mitigation
            .map(|value| format!(" mitigation: {value}."))
            .unwrap_or_default();
        let note = format!(
            "triage: failed run {expected_run_id} classified environmental — {diagnosis}.\
             {mitigation_note} returned to backlog (re-backlog {}/{max_rebacklogs}).",
            rebacklog_count + 1
        );
        if let Err(error) = runtime.apply_task_automation_update(
            task_id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Backlog),
                status_event: Some(TRIAGE_REBACKLOGGED_EVENT.to_string()),
                status_note: Some(note),
                ..TaskAutomationUpdate::default()
            },
        ) {
            tracing::warn!(task_id, "triage re-backlog write failed: {error}");
            return DispositionOutcome::skipped(format!("re-backlog write failed: {error}"));
        }
        return DispositionOutcome::applied("rebacklogged");
    }

    let denial_note = if requested_rebacklog {
        " re-backlog denied: only `environmental` failures may return to backlog."
    } else {
        ""
    };
    let note = format!(
        "triage: failed run {expected_run_id} classified {classification} — {diagnosis}.\
         {denial_note} task stays blocked for a human decision."
    );
    if let Err(error) = runtime.apply_task_automation_update(
        task_id,
        TaskAutomationUpdate {
            status_event: Some(TRIAGE_DIAGNOSIS_EVENT.to_string()),
            status_note: Some(note),
            ..TaskAutomationUpdate::default()
        },
    ) {
        tracing::warn!(task_id, "triage diagnosis write failed: {error}");
        return DispositionOutcome::skipped(format!("diagnosis write failed: {error}"));
    }
    DispositionOutcome::applied("diagnosed")
}
