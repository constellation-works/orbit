//! Task-pilot preparation: discover and partition the tasks a run assesses.

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::task::{Task, TaskComplexity, TaskStatus};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

use crate::OrbitRuntime;
use crate::application::job::delivery::{PrForgeCheck, tags_route_through_pr_pipeline};
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
/// A task the workspace's PR pipeline could not deliver: no Git remote of the
/// checkout names a network host. Preparing it would only stage work that
/// fails at `pr_open`.
const PR_FORGE_REMOTE_MISSING: &str = "pr_forge_remote_missing";

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
    let run_id = pilot_run_id(input);
    let mut active_preparations =
        crate::application::automation::preparation::active_task_pilot_preparations(runtime)
            .map_err(|error| action_failed(action, error.to_string()))?;
    // A retried prepare is not held by its own run.
    if let Some(run_id) = run_id {
        active_preparations.retain(|_, holders| {
            holders.remove(run_id);
            !holders.is_empty()
        });
    }
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

    // Shared with delivery admission: the workspace's own ship mode decides
    // whether a task would take the PR route.
    let ship_mode = runtime.automatic_delivery_ship_mode();
    let forge = PrForgeCheck::default();
    let forge_refused = |tags: &[String]| {
        tags_route_through_pr_pipeline(tags, ship_mode) && forge.require(runtime).is_err()
    };

    // Apply proves that a newer owner superseded preparation from the history
    // past each task's `history_len`, so that boundary must not follow the
    // snapshot apply compares against. Explicit selections name their tasks
    // up front, so their boundaries are read before the selection read; a
    // task absent here had no history yet. The selection read reports a
    // malformed or missing id.
    let mut explicit_history_lens = BTreeMap::new();
    for task_id in &explicit_task_ids {
        if orbit_types::task::validate_orb_task_id(task_id).is_err() {
            continue;
        }
        match runtime.get_task_history(task_id) {
            Ok(history) => {
                explicit_history_lens.insert(task_id.clone(), history.len());
            }
            Err(OrbitError::NotFound { .. }) => {}
            Err(error) => return Err(action_failed(action, error.to_string())),
        }
    }

    let (mode, task_ids, task_snapshots, mut excluded) = if explicit_mode {
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
                } else if forge_refused(&task.tags) {
                    excluded.record(&task.id, PR_FORGE_REMOTE_MISSING, &active_preparations);
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
            })
            .or_else(|| forge_refused(&envelope.tags).then_some(PR_FORGE_REMOTE_MISSING));
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
    //
    // Automatic selection learns its tasks only from the envelope read, so its
    // boundary is read here and the snapshot is retaken from the hydration
    // that follows it. A task another writer moved out of automatic selection
    // in between, such as by a workflow admission, settles superseded now
    // rather than reaching apply with that change hidden behind its boundary.
    #[cfg(test)]
    hydration_test_hook::run(runtime);
    let lane = ImplementationLane::resolve(runtime);
    let mut retained = Vec::with_capacity(task_ids.len());
    let mut superseded_during_preparation = Vec::new();
    for (task_id, mut snapshot) in task_ids.into_iter().zip(task_snapshots) {
        let history_len = if explicit_mode {
            explicit_history_lens.get(&task_id).copied().unwrap_or(0)
        } else {
            runtime
                .get_task_history(&task_id)
                .map_err(|error| action_failed(action, error.to_string()))?
                .len()
        };
        let task = runtime
            .get_task(&task_id)
            .map_err(|error| action_failed(action, error.to_string()))?;
        if !explicit_mode {
            if let Some(outcome) = left_automatic_selection(&snapshot, &task) {
                superseded_during_preparation.push(outcome);
                continue;
            }
            snapshot = task_snapshot(
                &task.id,
                &task.title,
                task.status,
                task.complexity,
                &task.tags,
                &task.context_files,
            );
        }
        snapshot["history_len"] = json!(history_len);
        snapshot[VALIDATION_TOOL_WARNINGS] = json!(lane.validation_warnings(&task));
        let creation = runtime
            .context_creation_state(&task)
            .map_err(|error| action_failed(action, error.to_string()))?;
        snapshot[CONTEXT_CREATION_SELECTORS] = json!(creation.selectors());
        snapshot[CONTEXT_CREATION_IDENTITY] = json!(creation.identity());

        if let Some(source) = &source {
            let fingerprints = crate::application::automation::preparation::fingerprints(
                runtime,
                &task,
                &source.source_revision,
                &policy,
            )
            .map_err(|error| action_failed(action, error.to_string()))?;
            // Each task is checked against the batch member that claimed it.
            // A member edited since the claim is set aside alone, as apply
            // supersedes per member [ORB-14476]: it is claimed afresh while
            // its siblings are piloted.
            if let Some(claim) = &claim {
                let member = claim
                    .members()
                    .iter()
                    .find(|member| member.task_ids.contains(&task_id))
                    .ok_or_else(|| {
                        action_failed(action, "state-trigger task membership changed")
                    })?;
                if member.fingerprint != fingerprints.material {
                    superseded_during_preparation.push(json!({
                        "task_id": task_id, "outcome": "superseded",
                        "reason": "material_changed", "status": task.status,
                        "detail": "task material changed after its state-trigger claim",
                    }));
                    continue;
                }
            }
            snapshot["material_fingerprint"] = json!(fingerprints.material);
            snapshot["status_neutral_fingerprint"] = json!(fingerprints.status_neutral);
            snapshot["material_components"] = json!(fingerprints.components);
        }
        retained.push((task_id, snapshot));
    }

    // Selection read the holders before any work. This re-check and this
    // run's reservation are one critical section of the commit boundary, so
    // of two runs preparing one task at once exactly one keeps it.
    let held = crate::application::automation::pilot_reservation::reserve(
        runtime,
        run_id,
        &retained
            .iter()
            .map(|(task_id, _)| task_id.clone())
            .collect::<Vec<_>>(),
    )
    .map_err(|error| action_failed(action, format!("reserve pilot tasks: {error}")))?;
    let held_reason = if explicit_mode {
        "already_preparing"
    } else {
        "active_pilot_prepared"
    };
    let (task_ids, task_snapshots): (Vec<_>, Vec<_>) = retained
        .into_iter()
        .filter(|(task_id, _)| {
            let free = !held.contains_key(task_id);
            if !free {
                excluded.record(task_id, held_reason, &held);
            }
            free
        })
        .unzip();

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
        "superseded_during_preparation": superseded_during_preparation,
        "source_age": source.as_ref().map(|source| source.age(&workspace_root)),
        "mode": mode,
        "workspace_path": workspace_root,
        // The only machine a `required_machine` finding may name.
        "owner_machine": runtime.automation_execution_location(),
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
        // The one refusal behind every `pr_forge_remote_missing` exclusion,
        // naming the remotes and both ways out.
        "pr_forge_refusal": excluded
            .by_reason
            .contains_key(PR_FORGE_REMOTE_MISSING)
            .then(|| forge.require(runtime).err().map(|error| error.to_string()))
            .flatten(),
    }))
}

/// The pilot run this preparation belongs to. The dispatcher names it
/// `run_id`, or `job_run_id` when the input already carried a `run_id` token.
fn pilot_run_id(input: &Value) -> Option<&str> {
    ["job_run_id", "run_id"]
        .into_iter()
        .find_map(|key| input.get(key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|run_id| !run_id.is_empty())
}

/// The superseded outcome for an automatically selected task that another
/// writer moved out of automatic selection after its envelope was read, or
/// `None` while hydration still selects it.
fn left_automatic_selection(selected: &Value, task: &Task) -> Option<Value> {
    automatic_exclusion_reason(
        task.status,
        &task.context_files,
        task.complexity,
        &task.tags,
    )?;
    let (reason, detail) = if matches!(
        task.status,
        TaskStatus::Done | TaskStatus::Rejected | TaskStatus::Archived
    ) {
        ("task_terminal", "task became terminal after selection")
    } else if selected["status"] != json!(task.status) {
        (
            "status_changed",
            "a durable task transition superseded preparation",
        )
    } else {
        ("task_edited", "task fields changed after selection")
    };
    Some(json!({
        "task_id": task.id, "outcome": "superseded", "reason": reason,
        "status": task.status, "detail": detail,
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

/// Runs once between the selection read and hydration, so a test can land a
/// concurrent write in that window.
#[cfg(test)]
pub(super) mod hydration_test_hook {
    use std::cell::RefCell;

    use crate::OrbitRuntime;

    type Hook = Box<dyn FnOnce(&OrbitRuntime)>;

    thread_local! {
        static HOOK: RefCell<Option<Hook>> = RefCell::new(None);
    }

    pub(in super::super) fn install(hook: impl FnOnce(&OrbitRuntime) + 'static) {
        HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    }

    pub(super) fn run(runtime: &OrbitRuntime) {
        if let Some(hook) = HOOK.with(|slot| slot.borrow_mut().take()) {
            hook(runtime);
        }
    }
}
