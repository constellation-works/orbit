//! Task-pilot preparation: discover and partition the tasks a run assesses.

use orbit_engine::DispatchError;
use orbit_types::task::{TaskComplexity, TaskStatus};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

use crate::OrbitRuntime;
use crate::application::task::TaskListFilter;

use super::input::{action_failed, bounded_usize, requested_workspace_root, string_array};
use super::persist::{no_target_assessment_marker, source_superseded};
use super::source::{SourceSnapshot, resolve_source_snapshot};
use super::validation_tools::ImplementationLane;
use super::{
    CONTEXT_CREATION_IDENTITY, CONTEXT_CREATION_SELECTORS, VALIDATION_TOOL_WARNINGS,
    requested_base_branch,
};

const DEFAULT_MAX_PARTITION_SIZE: usize = 5;
const HARD_MAX_PARTITION_SIZE: usize = 5;
const DEFAULT_MAX_TASKS: usize = 50;
const HARD_MAX_TASKS: usize = 500;
const NO_DIFF_TAGS: [&str; 2] = ["no-diff-needed", "no-diff-expected"];
/// Cap on individually itemized `excluded` entries in routine discovery output.
/// Automatic-mode exclusions still carry full counts by reason; this bounds
/// only the per-task sample so evidence size stops scaling with terminal
/// workspace history [ORB-11244].
const MAX_EXCLUDED_SAMPLE: usize = 20;

pub(in super::super) fn prepare(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let workspace_root = requested_workspace_root(runtime, action, input)?;
    let claim = crate::application::automation::members::claim(runtime, input)
        .map_err(|error| action_failed(action, error.to_string()))?;
    let source = resolve_source_snapshot(runtime, action, input, &workspace_root)?;
    if let Some(claim) = &claim
        && (source.as_ref().map(|s| &s.source_revision) != Some(&claim.member.source.commit)
            || input.get("task_ids") != Some(&json!(claim.task_ids())))
    {
        return Err(action_failed(
            action,
            "state-trigger source or task membership changed",
        ));
    }
    let max_partition_size = bounded_usize(
        action,
        input,
        "max_partition_size",
        DEFAULT_MAX_PARTITION_SIZE,
        HARD_MAX_PARTITION_SIZE,
    )?;
    let max_tasks = bounded_usize(
        action,
        input,
        "max_tasks",
        DEFAULT_MAX_TASKS,
        HARD_MAX_TASKS,
    )?;
    let explicit_task_ids = string_array(input, "task_ids", action)?;
    let explicit_mode = !explicit_task_ids.is_empty();
    let active_preparations =
        crate::application::automation::preparation::active_task_pilot_preparations(runtime)
            .map_err(|error| action_failed(action, error.to_string()))?;
    let policy = crate::application::automation::preparation::claim_policy(runtime, claim.as_ref())
        .map_err(|error| action_failed(action, error.to_string()))?;
    // A claimed task the branch already changed under is not piloted against
    // the frozen source: it settles superseded, and its member is claimed
    // afresh at the head [ORB-14476]. Apply carries these outcomes.
    let superseded = match &claim {
        Some(claim) => crate::application::automation::members::stale_tasks(
            runtime,
            claim,
            &policy,
            &Value::Null,
            &claim.task_ids(),
            &BTreeMap::new(),
        )
        .map_err(|error| action_failed(action, error.to_string()))?,
        None => BTreeMap::new(),
    };

    let (mode, task_ids, mut task_snapshots, excluded) = if explicit_mode {
        let all_tasks = runtime
            .list_tasks()
            .map_err(|error| action_failed(action, format!("list workspace tasks: {error}")))?;
        let by_id = all_tasks
            .iter()
            .map(|task| (task.id.as_str(), task))
            .collect::<BTreeMap<_, _>>();
        let mut seen = BTreeSet::new();
        let mut excluded = ExcludedEvidence::default();
        let selected = explicit_task_ids
            .iter()
            .map(|task_id| {
                if !seen.insert(task_id.as_str()) {
                    return Err(action_failed(
                        action,
                        format!("explicit task_ids contains duplicate {task_id}"),
                    ));
                }
                by_id.get(task_id.as_str()).copied().ok_or_else(|| {
                    action_failed(
                        action,
                        format!("task {task_id} does not exist in the selected workspace"),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let selected = selected
            .into_iter()
            .filter(|task| !superseded.contains_key(&task.id))
            .filter(|task| {
                if active_preparations.contains_key(&task.id) {
                    excluded.record(&task.id, "already_preparing", &active_preparations);
                    false
                } else {
                    true
                }
            })
            .collect::<Vec<_>>();
        let task_ids = selected
            .iter()
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        let task_snapshots = selected
            .iter()
            .map(|task| {
                task_snapshot(
                    &task.id,
                    &task.title,
                    task.status,
                    task.complexity,
                    &task.tags,
                    &task.context_files,
                )
            })
            .collect::<Vec<_>>();
        ("explicit", task_ids, task_snapshots, excluded)
    } else {
        // Envelopes exclude terminal and already scoped work before any task
        // hydration. Remaining candidates may need their last applied pilot
        // marker checked against current material.
        let candidates = runtime
            .task_candidates(&TaskListFilter::default(), usize::MAX)
            .map_err(|error| {
                action_failed(action, format!("list workspace task envelopes: {error}"))
            })?;
        let mut envelopes = candidates.items;
        envelopes.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then(left.id.cmp(&right.id))
        });

        let mut selected = Vec::new();
        let mut excluded = ExcludedEvidence::default();
        for envelope in &envelopes {
            let reason = automatic_exclusion_reason(
                envelope.status,
                &envelope.context_files,
                envelope.complexity,
                &envelope.tags,
            )
            .or_else(|| {
                active_preparations
                    .contains_key(&envelope.id)
                    .then_some("active_pilot_prepared")
            });
            let reason = if reason.is_none()
                && envelope.context_files.is_empty()
                && fresh_no_target_assessment(
                    runtime,
                    action,
                    &envelope.id,
                    source.as_ref(),
                    &policy,
                )? {
                Some("no_target_assessment_fresh")
            } else {
                reason
            };
            match reason {
                Some(reason) => excluded.record(&envelope.id, reason, &active_preparations),
                None => selected.push(envelope),
            }
        }

        let task_ids = selected
            .iter()
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        let task_snapshots = selected
            .iter()
            .map(|task| {
                task_snapshot(
                    &task.id,
                    &task.title,
                    task.status,
                    task.complexity,
                    &task.tags,
                    &task.context_files,
                )
            })
            .collect::<Vec<_>>();
        ("automatic", task_ids, task_snapshots, excluded)
    };

    if task_ids.len() > max_tasks {
        return Err(action_failed(
            action,
            format!(
                "{mode} task-pilot selection contains {} tasks, exceeding max_tasks {max_tasks}",
                task_ids.len()
            ),
        ));
    }

    // One hydration per selected task feeds both the state-automation
    // fingerprint and the validation-tool feasibility check. Discovery above
    // deliberately works from envelopes, which carry no acceptance criteria,
    // and the selection is already bounded by `max_tasks` at this point.
    let lane = ImplementationLane::resolve(runtime);
    for (task_id, snapshot) in task_ids.iter().zip(task_snapshots.iter_mut()) {
        snapshot["history_len"] = json!(
            runtime
                .get_task_history(task_id)
                .map_err(|error| action_failed(action, error.to_string()))?
                .len()
        );
        let task = runtime
            .get_task(task_id)
            .map_err(|error| action_failed(action, error.to_string()))?;
        snapshot[VALIDATION_TOOL_WARNINGS] = json!(lane.validation_warnings(&task));
        let creation = runtime
            .context_creation_state(&task)
            .map_err(|error| action_failed(action, error.to_string()))?;
        snapshot[CONTEXT_CREATION_SELECTORS] = json!(creation.selectors());
        snapshot[CONTEXT_CREATION_IDENTITY] = json!(creation.identity());

        let Some(source) = &source else {
            continue;
        };
        let fingerprints = crate::application::automation::preparation::fingerprints(
            runtime,
            &task,
            &source.source_revision,
            &policy,
        )
        .map_err(|error| action_failed(action, error.to_string()))?;
        // Each task is checked against the batch member that claimed it.
        if claim.as_ref().is_some_and(|claim| {
            claim
                .members()
                .iter()
                .find(|member| member.task_ids.contains(task_id))
                .is_none_or(|member| member.fingerprint != fingerprints.material)
        }) {
            return Err(action_failed(action, "state-trigger task meaning changed"));
        }
        snapshot["material_fingerprint"] = json!(fingerprints.material);
        snapshot["status_neutral_fingerprint"] = json!(fingerprints.status_neutral);
        snapshot["material_components"] = json!(fingerprints.components);
    }

    // Size partitions only. Crew homogeneity is the state-consumer batching
    // step [ORB-12761]: a mixed-crew attempt is rejected at dispatch before
    // this action runs.
    let partitions = task_ids
        .chunks(max_partition_size)
        .enumerate()
        .map(|(partition_index, ids)| {
            json!({
                "partition_index": partition_index,
                "task_ids": ids,
            })
        })
        .collect::<Vec<_>>();

    let superseded_by_source = superseded
        .iter()
        .map(|(task_id, detail)| source_superseded(task_id, detail))
        .collect::<Vec<_>>();

    Ok(json!({
        "state_automation": claim,
        "superseded_by_source": superseded_by_source,
        "source_age": source.as_ref().map(|source| source.age(&workspace_root)),
        "mode": mode,
        "workspace_path": workspace_root,
        "source": source.as_ref().map(SourceSnapshot::to_json).unwrap_or_else(|| {
            json!({
                "base_branch": requested_base_branch(runtime, input),
                "source_ref": Value::Null,
                "source_revision": Value::Null,
                "fast_forwarded": false,
            })
        }),
        "task_count": task_ids.len(),
        "task_ids": task_ids,
        "tasks": task_snapshots,
        "partition_size": max_partition_size,
        "partition_count": partitions.len(),
        "partitions": partitions,
        "excluded": excluded.sample,
        "excluded_total": excluded.total,
        "excluded_by_reason": excluded.by_reason,
        "excluded_sample_truncated": excluded.total > excluded.sample.len(),
        "excluded_omitted_count": excluded.total.saturating_sub(excluded.sample.len()),
    }))
}

fn fresh_no_target_assessment(
    runtime: &OrbitRuntime,
    action: &str,
    task_id: &str,
    source: Option<&SourceSnapshot>,
    policy: &orbit_types::workflow::automation::members::PreparationPolicy,
) -> Result<bool, DispatchError> {
    let history = runtime
        .get_task_history(task_id)
        .map_err(|error| action_failed(action, format!("read task {task_id} history: {error}")))?;
    let marker = history
        .iter()
        .rev()
        .find(|entry| entry.event == "task_pilot_applied")
        .and_then(|entry| entry.note.as_deref())
        .and_then(no_target_assessment_marker);
    let Some((assessed_status, assessed_fingerprint)) = marker else {
        return Ok(false);
    };
    let task = runtime
        .get_task(task_id)
        .map_err(|error| action_failed(action, format!("read task {task_id}: {error}")))?;
    if task.status != assessed_status {
        return Ok(false);
    }
    let current = crate::application::automation::preparation::pilot_fingerprint(
        runtime,
        &task,
        source.map(|source| source.source_revision.as_str()),
        policy,
    )
    .map_err(|error| action_failed(action, format!("fingerprint task {task_id}: {error}")))?;
    Ok(current == assessed_fingerprint)
}

/// Bounded evidence for routine (non-explicit) discovery exclusions: every
/// excluded task is counted by reason, but only a capped sample is itemized
/// so response size stops scaling with terminal workspace history.
#[derive(Default)]
struct ExcludedEvidence {
    sample: Vec<Value>,
    total: usize,
    by_reason: BTreeMap<&'static str, usize>,
}

impl ExcludedEvidence {
    fn record(
        &mut self,
        task_id: &str,
        reason: &'static str,
        active_preparations: &BTreeMap<String, BTreeSet<String>>,
    ) {
        self.total += 1;
        *self.by_reason.entry(reason).or_default() += 1;
        // Explicit selections report every held task; only automatic discovery
        // samples exclusions from potentially unbounded workspace history.
        if reason != "already_preparing" && self.sample.len() >= MAX_EXCLUDED_SAMPLE {
            return;
        }
        let mut entry = json!({ "task_id": task_id, "reason": reason });
        if matches!(reason, "active_pilot_prepared" | "already_preparing")
            && let Some(run_ids) = active_preparations.get(task_id)
            && let Value::Object(fields) = &mut entry
        {
            fields.insert("prepared_by_run_ids".to_string(), json!(run_ids));
        }
        self.sample.push(entry);
    }
}

fn automatic_exclusion_reason(
    status: TaskStatus,
    context_files: &[String],
    complexity: Option<TaskComplexity>,
    tags: &[String],
) -> Option<&'static str> {
    if !matches!(status, TaskStatus::Proposed | TaskStatus::Backlog) {
        return Some("status_not_eligible");
    }
    if !context_files.is_empty() && complexity.is_some_and(TaskComplexity::is_assessed) {
        return Some("context_files_not_empty");
    }
    // No-diff work produces its durable result outside the repository, so
    // there are no modification selectors to assess. Automatically minted
    // no-diff tasks are no exception: admission already exempts
    // `no-diff-expected` from the assessed-complexity gate [ORB-12118], and a
    // `verified_no_diff` assessment leaves `context_files` empty, so admitting
    // them here re-piloted the same task on every routine tick.
    if tags.iter().any(|tag| NO_DIFF_TAGS.contains(&tag.as_str())) {
        return Some("no_diff_task");
    }
    None
}

fn task_snapshot(
    id: &str,
    title: &str,
    status: TaskStatus,
    complexity: Option<TaskComplexity>,
    tags: &[String],
    context_files: &[String],
) -> Value {
    json!({
        "task_id": id,
        "title": title,
        "status": status,
        "complexity": complexity,
        "tags": tags,
        "context_files_before": context_files,
    })
}
