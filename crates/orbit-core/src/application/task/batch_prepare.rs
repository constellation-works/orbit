//! The batch-selection skeleton shared by the escalation-triage and
//! finding-verification prepare steps: bounds, claim check, explicit and
//! automatic candidate selection, crew partitioning and the output
//! envelope. Each step keeps only its own filter, qualification and crew
//! draw, supplied here as hooks.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::task::Task;
use orbit_types::workflow::automation::members::MemberAttempt;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::TaskListFilter;
use crate::application::task::pilot::input::{bounded_usize, string_array};

const DEFAULT_MAX_PARTITION_SIZE: usize = 5;
const HARD_MAX_PARTITION_SIZE: usize = 5;
const DEFAULT_MAX_TASKS: usize = 50;
const HARD_MAX_TASKS: usize = 500;

/// One candidate's fate from a caller's qualification hook.
pub(crate) enum Qualification<T> {
    Selected(T),
    Rejected(&'static str),
}

/// Tasks live runs hold, by task ID, with the holding run IDs.
pub(crate) type HeldRuns = BTreeMap<String, BTreeSet<String>>;

/// Caller-supplied pieces of [`prepare_batch`] that are not hooks:
/// the claim and the source/workspace it resolved, the automatic filter,
/// reasons silently dropped in automatic mode, and extra output keys
/// inserted before `workspace_path` / before `task_count`.
pub(crate) struct BatchSpec {
    pub claim: Option<MemberAttempt>,
    pub source_revision: Option<String>,
    pub source: Value,
    pub workspace_path: PathBuf,
    pub automatic_filter: TaskListFilter,
    pub silent_reasons: &'static [&'static str],
    pub before_workspace: Vec<(&'static str, Value)>,
    pub before_task_count: Vec<(&'static str, Value)>,
}

/// Run the shared batch-selection skeleton. The hooks carry the steps'
/// behavioural differences: `held` is fetched where the copies fetched it,
/// `keep_automatic` filters automatic candidates after their task read,
/// `qualify` decides one candidate, `draw` returns its row and crew key,
/// `skipped_detail` renders a superseded detail, and `partition_extra`
/// optionally adds one partition key from the rows and its task IDs.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_batch<T>(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
    run_id: Option<&str>,
    spec: BatchSpec,
    held: impl FnOnce(&OrbitRuntime) -> Result<HeldRuns, OrbitError>,
    keep_automatic: impl Fn(&Task) -> bool,
    mut qualify: impl FnMut(&Task, &HeldRuns, bool) -> Result<Qualification<T>, OrbitError>,
    mut draw: impl FnMut(&Task, T) -> Result<(Value, String), OrbitError>,
    skipped_detail: impl Fn(&str, Option<&BTreeSet<String>>) -> String,
    partition_extra: impl Fn(&[Value], &[String]) -> Option<(&'static str, Value)>,
) -> Result<Value, DispatchError> {
    let failed = |error: OrbitError| DispatchError::action_failed(action, error.to_string());
    if let Some(claim) = &spec.claim
        && (spec.source_revision.as_deref() != Some(claim.member.source.commit.as_str())
            || input.get("task_ids") != Some(&json!(claim.task_ids())))
    {
        return Err(DispatchError::action_failed(
            action,
            "state-trigger source or task membership changed",
        ));
    }
    let bound = |field, default, max| bounded_usize(action, input, field, default, max);
    let max_partition_size = bound(
        "max_partition_size",
        DEFAULT_MAX_PARTITION_SIZE,
        HARD_MAX_PARTITION_SIZE,
    )?;
    let max_tasks = bound("max_tasks", DEFAULT_MAX_TASKS, HARD_MAX_TASKS)?;
    let explicit_task_ids = string_array(input, "task_ids", action)?;
    let explicit = !explicit_task_ids.is_empty();
    if explicit_task_ids.len() > max_tasks {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "explicit selection names {} tasks, exceeding max_tasks {max_tasks}",
                explicit_task_ids.len()
            ),
        ));
    }

    // A retried prepare is not held by its own earlier checkpoint.
    let mut held = held(runtime).map_err(failed)?;
    if let Some(run_id) = run_id {
        held.retain(|_, holders| {
            holders.remove(run_id);
            !holders.is_empty()
        });
    }

    let candidates = if explicit {
        let mut seen = BTreeSet::new();
        let mut tasks = Vec::with_capacity(explicit_task_ids.len());
        for task_id in &explicit_task_ids {
            if !seen.insert(task_id.as_str()) {
                return Err(DispatchError::action_failed(
                    action,
                    format!("explicit task_ids contains duplicate {task_id}"),
                ));
            }
            tasks.push(runtime.get_task(task_id).map_err(|error| {
                if matches!(error, OrbitError::NotFound { .. }) {
                    DispatchError::action_failed(
                        action,
                        format!("task {task_id} does not exist in the selected workspace"),
                    )
                } else {
                    failed(error)
                }
            })?);
        }
        tasks
    } else {
        let mut envelopes = runtime
            .task_candidates(&spec.automatic_filter, usize::MAX)
            .map_err(failed)?
            .items;
        // Oldest first, so a capped pass makes progress through them.
        envelopes.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then(left.id.cmp(&right.id))
        });
        envelopes
            .iter()
            .map(|envelope| runtime.get_task(&envelope.id))
            .collect::<Result<Vec<_>, _>>()
            .map_err(failed)?
            .into_iter()
            .filter(|task| keep_automatic(task))
            .collect()
    };

    // An explicitly named task that no longer qualifies settles superseded
    // with its reason, so a claimed member is released rather than failed.
    let mut superseded = Vec::new();
    let mut excluded = BTreeMap::<&'static str, usize>::new();
    let mut selected = Vec::new();
    for task in candidates {
        match qualify(&task, &held, explicit).map_err(failed)? {
            Qualification::Selected(_) if selected.len() >= max_tasks => {
                *excluded.entry("deferred_by_max_tasks").or_default() += 1;
            }
            Qualification::Selected(payload) => selected.push((task, payload)),
            Qualification::Rejected(reason) if explicit => superseded.push(json!({
                "task_id": task.id, "outcome": "superseded", "reason": reason,
                "status": task.status,
                "detail": skipped_detail(reason, held.get(&task.id)),
            })),
            Qualification::Rejected(reason) if spec.silent_reasons.contains(&reason) => {}
            Qualification::Rejected(reason) => *excluded.entry(reason).or_default() += 1,
        }
    }

    // Every task's crew is drawn before any is dispatched, so its crew is
    // part of the prepared checkpoint the run records.
    let mut tasks = Vec::with_capacity(selected.len());
    let mut by_crew = BTreeMap::<String, Vec<String>>::new();
    for (task, payload) in selected {
        let (row, crew) = draw(&task, payload).map_err(failed)?;
        by_crew.entry(crew).or_default().push(task.id.clone());
        tasks.push(row);
    }
    let partitions = by_crew
        .iter()
        .flat_map(|(crew, ids)| {
            ids.chunks(max_partition_size)
                .map(move |chunk| (crew.clone(), chunk.to_vec()))
        })
        .enumerate()
        .map(|(partition_index, (crew, task_ids))| {
            let mut partition = json!({
                "partition_index": partition_index, "task_ids": task_ids, "crew": crew
            });
            if let Some((key, value)) = partition_extra(&tasks, &task_ids) {
                partition[key] = value;
            }
            partition
        })
        .collect::<Vec<_>>();
    let task_ids = tasks
        .iter()
        .map(|task| task["task_id"].clone())
        .collect::<Vec<_>>();

    // `shift_insert` places the callers' extra keys: before_workspace at
    // index 2 (after `mode`), before_task_count at the `task_count` slot,
    // which follows the six keys ahead of it plus any before_workspace keys.
    let mut output = json!({
        "state_automation": spec.claim,
        "mode": if explicit { "explicit" } else { "automatic" },
        "workspace_path": spec.workspace_path,
        "owner_machine": runtime.automation_execution_location(),
        // Results of this run must carry the observed-revision proof.
        crate::application::task::pilot::source::INSPECTION_PROOF_KEY: true,
        "source": spec.source,
        "task_count": task_ids.len(),
        "task_ids": task_ids,
        "tasks": tasks,
        "partition_size": max_partition_size,
        "partition_count": partitions.len(),
        "partitions": partitions,
        "excluded_by_reason": excluded,
        "superseded_during_preparation": superseded,
    });
    if let Some(map) = output.as_object_mut() {
        let extra = spec.before_workspace.len();
        for (index, (key, value)) in spec.before_workspace.into_iter().enumerate() {
            map.shift_insert(2 + index, key.into(), value);
        }
        for (index, (key, value)) in spec.before_task_count.into_iter().enumerate() {
            map.shift_insert(6 + extra + index, key.into(), value);
        }
    }
    Ok(output)
}

/// The `skipped_detail` arm both steps share: `{verb} by run {holders}`.
pub(crate) fn held_by_run_detail(verb: &str, holders: Option<&BTreeSet<String>>) -> String {
    let runs = holders
        .into_iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    format!("{verb} by run {runs}")
}
