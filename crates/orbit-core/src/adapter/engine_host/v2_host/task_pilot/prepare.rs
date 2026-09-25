//! Task-pilot preparation: discover and partition the tasks a run assesses.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_engine::DispatchError;
use orbit_store::contracts::JobRunQuery;
use orbit_types::task::{TaskComplexity, TaskEnvelopeV2, TaskStatus};
use orbit_types::workflow::JobRunState;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::TaskListFilter;

use super::input::{action_failed, bounded_usize, requested_workspace_root, string_array};
use super::source::{SourceSnapshot, resolve_source_snapshot};
use super::validation_tools::ImplementationLane;
use super::{VALIDATION_TOOL_WARNINGS, requested_base_branch};

const DEFAULT_MAX_PARTITION_SIZE: usize = 5;
const HARD_MAX_PARTITION_SIZE: usize = 5;
const DEFAULT_MAX_TASKS: usize = 50;
const HARD_MAX_TASKS: usize = 500;
const NO_DIFF_TAGS: [&str; 2] = ["no-diff-needed", "no-diff-expected"];
const TASK_PILOT_JOB_ID: &str = "task_pilot_pipeline";
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
    let claim = crate::application::automation::members::claim(runtime, input, &[])
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
    let active_preparations = active_task_pilot_preparations(runtime, action, &workspace_root)?;

    let (mode, task_ids, mut task_snapshots, excluded) = if explicit_mode {
        let all_tasks = runtime
            .list_tasks()
            .map_err(|error| action_failed(action, format!("list workspace tasks: {error}")))?;
        let by_id = all_tasks
            .iter()
            .map(|task| (task.id.as_str(), task))
            .collect::<BTreeMap<_, _>>();
        let mut seen = BTreeSet::new();
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
                }).and_then(|task| {
                    if let Some(run_ids) = active_preparations.get(task_id) {
                        Err(action_failed(
                            action,
                            format!(
                                "task {task_id} is already prepared by active task-pilot run(s) {}; inspect or resume that durable run instead of starting duplicate pilot work",
                                run_ids.iter().cloned().collect::<Vec<_>>().join(", ")
                            ),
                        ))
                    } else {
                        Ok(task)
                    }
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
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
        (
            "explicit",
            task_ids,
            task_snapshots,
            ExcludedEvidence::default(),
        )
    } else {
        // Envelope-only metadata (no description/plan/comments/history/artifacts)
        // for every task in the workspace, so routine discovery no longer pays
        // for full-bundle hydration of terminal history it will only exclude.
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
            match reason {
                Some(reason) => excluded.record(envelope, reason, &active_preparations),
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
    let eligibility =
        crate::application::automation::preparation::claim_eligibility(runtime, claim.as_ref())
            .map_err(|error| action_failed(action, error.to_string()))?;
    for (task_id, snapshot) in task_ids.iter().zip(task_snapshots.iter_mut()) {
        let task = runtime
            .get_task(task_id)
            .map_err(|error| action_failed(action, error.to_string()))?;
        snapshot[VALIDATION_TOOL_WARNINGS] = json!(lane.validation_warnings(&task));

        let Some(source) = &source else {
            continue;
        };
        let (fingerprint, status_neutral_fingerprint) =
            crate::application::automation::preparation::fingerprints(
                runtime,
                &task,
                &source.source_revision,
                &eligibility,
            )
            .map_err(|error| action_failed(action, error.to_string()))?;
        // Each task is checked against the batch member that claimed it.
        if claim.as_ref().is_some_and(|claim| {
            claim
                .members()
                .iter()
                .find(|member| member.task_ids.contains(task_id))
                .is_none_or(|member| member.fingerprint != fingerprint)
        }) {
            return Err(action_failed(action, "state-trigger task meaning changed"));
        }
        snapshot["material_fingerprint"] = json!(fingerprint);
        snapshot["status_neutral_fingerprint"] = json!(status_neutral_fingerprint);
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

    Ok(json!({
        "state_automation": claim,
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
        envelope: &TaskEnvelopeV2,
        reason: &'static str,
        active_preparations: &BTreeMap<String, BTreeSet<String>>,
    ) {
        self.total += 1;
        *self.by_reason.entry(reason).or_default() += 1;
        if self.sample.len() >= MAX_EXCLUDED_SAMPLE {
            return;
        }
        let mut entry = json!({ "task_id": envelope.id, "reason": reason });
        if reason == "active_pilot_prepared"
            && let Some(run_ids) = active_preparations.get(&envelope.id)
            && let Value::Object(fields) = &mut entry
        {
            fields.insert("prepared_by_run_ids".to_string(), json!(run_ids));
        }
        self.sample.push(entry);
    }
}

fn active_task_pilot_preparations(
    runtime: &OrbitRuntime,
    action: &str,
    workspace_root: &Path,
) -> Result<BTreeMap<String, BTreeSet<String>>, DispatchError> {
    let mut prepared_by_task = BTreeMap::<String, BTreeSet<String>>::new();
    for state in [
        JobRunState::Pending,
        JobRunState::Running,
        JobRunState::Retrying,
    ] {
        let runs = runtime
            .stores()
            .jobs()
            .list_job_runs_filtered(&JobRunQuery {
                job_id: Some(TASK_PILOT_JOB_ID.to_string()),
                state: Some(state),
                terminal_only: false,
                created_since: None,
                limit: None,
                ..Default::default()
            })
            .map_err(|error| {
                action_failed(action, format!("list active task-pilot runs: {error}"))
            })?;
        for run in runs {
            let run = runtime.show_job_run(&run.run_id).map_err(|error| {
                action_failed(
                    action,
                    format!("reconcile active task-pilot run {}: {error}", run.run_id),
                )
            })?;
            if run.state.is_terminal() {
                continue;
            }
            let Some(state) = runtime.read_run_state(&run.run_id).map_err(|error| {
                action_failed(
                    action,
                    format!("read active task-pilot run {} state: {error}", run.run_id),
                )
            })?
            else {
                continue;
            };
            let Some(task_ids) = state.step_outputs.iter().find_map(|(step_index, output)| {
                (state.step_states.get(step_index) == Some(&JobRunState::Success))
                    .then(|| prepared_task_ids(output, workspace_root))
                    .flatten()
            }) else {
                continue;
            };
            for task_id in task_ids {
                prepared_by_task
                    .entry(task_id)
                    .or_default()
                    .insert(run.run_id.clone());
            }
        }
    }
    Ok(prepared_by_task)
}

fn prepared_task_ids(output: &Value, workspace_root: &Path) -> Option<Vec<String>> {
    let object = output.as_object()?;
    let prepared_workspace = object.get("workspace_path")?.as_str()?;
    if Path::new(prepared_workspace) != workspace_root
        || !object.get("partitions")?.is_array()
        || !object.get("tasks")?.is_array()
        || !matches!(object.get("mode")?.as_str(), Some("automatic" | "explicit"))
    {
        return None;
    }
    object
        .get("task_ids")?
        .as_array()?
        .iter()
        .map(|task_id| task_id.as_str().map(ToOwned::to_owned))
        .collect()
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
