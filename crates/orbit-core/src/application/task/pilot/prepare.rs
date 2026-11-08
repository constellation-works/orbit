//! Task-pilot preparation: discover and partition the tasks a run assesses.

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::task::{
    ACCEPTANCE_CRITERIA_DOCUMENT, DESCRIPTION_DOCUMENT, TASK_FIELDS_UPDATED_EVENT, Task,
    TaskComplexity, TaskStatus, task_fields_updated_documents,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

use crate::OrbitRuntime;
use crate::application::contracts::PilotTaskReservation;
use crate::application::task::TaskListFilter;
use crate::application::task::{TaskAutomationPort, TaskJobPort};

use super::input::{bounded_usize, requested_workspace_root, string_array};
use super::persist::{self, source_superseded};
use super::source::{SourceSnapshot, resolve_source_snapshot};
use super::validation_tools::ImplementationLane;
use super::{
    CONTEXT_CREATION_IDENTITY, CONTEXT_CREATION_SELECTORS, OPERATOR_TOOL_CANDIDATES,
    VALIDATION_TOOL_WARNINGS, requested_base_branch,
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
/// A task assigned to a human crew: operator work the pilot never assesses,
/// warns on or holds.
const HUMAN_CREW_EXCLUSION: &str = "human_crew";

pub(crate) fn prepare(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let workspace_root = requested_workspace_root(runtime, action, input)?;
    let claim = runtime
        .state_claim(input)
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
    let source = resolve_source_snapshot(runtime, action, input, &workspace_root)?;
    if let Some(claim) = &claim
        && (source.as_ref().map(|s| &s.source_revision) != Some(&claim.member.source.commit)
            || input.get("task_ids") != Some(&json!(claim.task_ids())))
    {
        return Err(DispatchError::action_failed(
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
    if explicit_task_ids.len() > HARD_MAX_TASKS {
        return Err(DispatchError::action_failed(
            action,
            format!("explicit task-pilot selection exceeds the limit of {HARD_MAX_TASKS} tasks"),
        ));
    }
    let explicit_mode = !explicit_task_ids.is_empty();
    let recheck_request = recheck_request(action, input)?;
    let run_id = pilot_run_id(input);
    let mut active_preparations = runtime
        .pilot_preparation_holders()
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
    // A retried prepare is not held by its own run.
    if let Some(run_id) = run_id {
        active_preparations.retain(|_, holders| {
            holders.remove(run_id);
            !holders.is_empty()
        });
    }
    let policy = runtime
        .claim_preparation_policy(claim.as_ref())
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
    // A claimed task the branch already changed under is not piloted against
    // the frozen source: it settles superseded, and its member is claimed
    // afresh at the head [ORB-14476]. Apply carries these outcomes.
    let superseded = match &claim {
        Some(claim) => runtime
            .stale_claimed_tasks(
                claim,
                &policy,
                &Value::Null,
                &claim.task_ids(),
                &BTreeMap::new(),
            )
            .map_err(|error| DispatchError::action_failed(action, error.to_string()))?,
        None => BTreeMap::new(),
    };

    // Shared with delivery admission: the workspace's own ship mode decides
    // whether a task would take the PR route.
    let ship_mode = runtime.automatic_delivery_ship_mode();
    let forge = <OrbitRuntime as TaskJobPort>::PrForge::default();
    let forge_refused = |tags: &[String]| runtime.pr_route_lacks_forge(&forge, tags, ship_mode);

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
            Err(error) => return Err(DispatchError::action_failed(action, error.to_string())),
        }
    }

    // Automatic discovery only: tasks reselected because their meaning
    // changed after their applied assessment (see `stale_repilot_candidate`).
    let mut stale_repilot_ids = BTreeSet::new();
    let (mode, task_ids, task_snapshots, mut excluded) = if explicit_mode {
        let all_tasks = runtime.list_tasks().map_err(|error| {
            DispatchError::action_failed(action, format!("list workspace tasks: {error}"))
        })?;
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
                    return Err(DispatchError::action_failed(
                        action,
                        format!("explicit task_ids contains duplicate {task_id}"),
                    ));
                }
                by_id.get(task_id.as_str()).copied().ok_or_else(|| {
                    DispatchError::action_failed(
                        action,
                        format!("task {task_id} does not exist in the selected workspace"),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        // Check every named task before exclusions or reservations. An invalid
        // sibling must not spend agent budget, even when another pilot holds
        // it or a routine's frozen source would supersede it. Apply still owns
        // status changes after valid preparation and their audited outcomes.
        let unsupported = selected
            .iter()
            .filter(|task| !super::pilot_assessable_status(task.status))
            .map(|task| format!("{} (status {})", task.id, task.status))
            .collect::<Vec<_>>();
        if !unsupported.is_empty() {
            return Err(DispatchError::action_failed(
                action,
                format!(
                    "explicit task-pilot selection refuses {}; only proposed and backlog tasks may be assessed",
                    unsupported.join(", ")
                ),
            ));
        }
        // A child pilot is named like an explicit request, but its host
        // trigger says otherwise. An assessment an explicit or routine pilot
        // already certified is reused, not assessed again. Operator explicit
        // runs are not children.
        let child = super::drain_promotion::dispatched_child_pilot(runtime, run_id)
            .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
        let mut kept = Vec::new();
        for task in selected
            .into_iter()
            .filter(|task| !superseded.contains_key(&task.id))
        {
            let reason = if runtime.is_human_crew(task.crew.as_deref()) {
                // Operator work: no pilot assessment, warning or hold.
                Some(HUMAN_CREW_EXCLUSION)
            } else if active_preparations.contains_key(&task.id) {
                Some("already_preparing")
            } else if child
                && let Some(reason) =
                    super::drain_promotion::reused_assessment_exclusion(runtime, task)
                        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?
            {
                Some(reason)
            } else if forge_refused(&task.tags) {
                Some(PR_FORGE_REMOTE_MISSING)
            } else {
                None
            };
            match reason {
                Some(reason) => excluded.record(&task.id, reason, &active_preparations),
                None => kept.push(task),
            }
        }
        let selected = kept;
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
                DispatchError::action_failed(
                    action,
                    format!("list workspace task envelopes: {error}"),
                )
            })?;
        let mut envelopes = candidates.items;
        envelopes.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then(left.id.cmp(&right.id))
        });

        let claimed_tasks = super::drain_promotion::live_execution_claim_tasks(runtime)
            .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
        let mut selected = Vec::new();
        let mut excluded = ExcludedEvidence::default();
        for envelope in &envelopes {
            let reason = runtime
                .is_human_crew(envelope.crew.as_deref())
                .then_some(HUMAN_CREW_EXCLUSION)
                .or_else(|| {
                    automatic_exclusion_reason(
                        envelope.status,
                        &envelope.context_files,
                        envelope.complexity,
                        &envelope.tags,
                    )
                })
                .or_else(|| {
                    active_preparations
                        .contains_key(&envelope.id)
                        .then_some("active_pilot_prepared")
                })
                .or_else(|| forge_refused(&envelope.tags).then_some(PR_FORGE_REMOTE_MISSING));
            // A task whose meaning changed after its applied assessment is
            // discovered again for a fresh pilot, unless a live execution
            // claim holds it or a repilot already failed. Its assessed
            // selectors and complexity are the stale assessment's output, so
            // they do not exclude it either. Waiving that reason must not
            // waive exclusions hidden behind it in the short-circuit chain
            // above: recompute the independent gates in chain order.
            let stale_repilot = matches!(reason, None | Some("context_files_not_empty"))
                && stale_repilot_candidate(runtime, action, &envelope.id, &claimed_tasks)?;
            let reason = if stale_repilot && reason == Some("context_files_not_empty") {
                no_diff_exclusion_reason(&envelope.tags)
                    .or_else(|| {
                        active_preparations
                            .contains_key(&envelope.id)
                            .then_some("active_pilot_prepared")
                    })
                    .or_else(|| forge_refused(&envelope.tags).then_some(PR_FORGE_REMOTE_MISSING))
            } else {
                reason
            };
            let reason = if reason.is_none()
                && !stale_repilot
                && already_piloted(runtime, action, &envelope.id)?
            {
                Some("assessment_already_applied")
            } else {
                reason
            };
            match reason {
                Some(reason) => excluded.record(&envelope.id, reason, &active_preparations),
                None => {
                    if stale_repilot {
                        stale_repilot_ids.insert(envelope.id.clone());
                    }
                    selected.push(envelope);
                }
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
        return Err(DispatchError::action_failed(
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
                .map_err(|error| DispatchError::action_failed(action, error.to_string()))?
                .len()
        };
        let task = runtime
            .get_task(&task_id)
            .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
        if !explicit_mode {
            if let Some(outcome) = left_automatic_selection(
                &snapshot,
                &task,
                stale_repilot_ids.contains(&task_id),
                forge_refused(&task.tags),
            ) {
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
        let operator_tools = lane.operator_requirements(&task);
        if !operator_tools.is_empty() {
            snapshot[OPERATOR_TOOL_CANDIDATES] = json!(operator_tools);
        }
        let creation = runtime
            .context_creation_state(&task)
            .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
        snapshot[CONTEXT_CREATION_SELECTORS] = json!(creation.selectors());
        snapshot[CONTEXT_CREATION_IDENTITY] = json!(creation.identity());
        // Only an explicit request re-pilots an assessed task, so only it
        // may re-check a held assessment instead of piloting in full.
        if explicit_mode {
            super::recheck::plan(
                runtime,
                &workspace_root,
                &task,
                source
                    .as_ref()
                    .map(|source| source.source_revision.as_str()),
                &recheck_request,
            )
            .map_err(|error| DispatchError::action_failed(action, error.to_string()))?
            .record(&mut snapshot);
        }

        if let Some(source) = &source {
            let fingerprints = runtime
                .task_pilot_fingerprints(&task, &source.source_revision, &policy)
                .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
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
                        DispatchError::action_failed(
                            action,
                            "state-trigger task membership changed",
                        )
                    })?;
                if member.fingerprint != fingerprints.material {
                    superseded_during_preparation.push(super::superseded_outcome(
                        &task_id,
                        "material_changed",
                        task.status,
                        "task material changed after its state-trigger claim",
                    ));
                    continue;
                }
            }
            snapshot["material_fingerprint"] = json!(fingerprints.material);
            snapshot["status_neutral_fingerprint"] = json!(fingerprints.status_neutral);
            snapshot["material_components"] = json!(fingerprints.components);
        }
        retained.push((
            task_id,
            snapshot,
            (
                crate::runtime::engine::crew::normalized_task_crew(task.crew.as_deref()),
                runtime.pilot_effort_key(task.complexity),
                task.complexity,
            ),
        ));
    }

    // Selection read the holders before any work. This re-check and this
    // run's reservation are one critical section of the commit boundary, so
    // of two runs preparing one task at once exactly one keeps it.
    let reservation = runtime
        .reserve_pilot_tasks(
            run_id,
            &retained
                .iter()
                .map(|(task_id, _, _)| task_id.clone())
                .collect::<Vec<_>>(),
        )
        .map_err(|error| {
            DispatchError::action_failed(action, format!("reserve pilot tasks: {error}"))
        })?;
    let (held, reservation_deferred) = match reservation {
        PilotTaskReservation::Reserved(held) => (held, None),
        PilotTaskReservation::Deferred { reason } => {
            for (task_id, snapshot, _) in retained.drain(..) {
                excluded.record(&task_id, "pilot_reservation_lock_busy", &BTreeMap::new());
                // Supersede this preparation attempt, not the task material.
                // Apply carries these outcomes so state-triggered members are
                // observed afresh instead of recorded as failed assessments.
                superseded_during_preparation.push(json!({
                    "task_id": task_id,
                    "outcome": "superseded",
                    "reason": "pilot_reservation_lock_busy",
                    "status": snapshot["status"],
                    "detail": reason,
                }));
            }
            (
                BTreeMap::new(),
                Some(json!({"code": "lock_busy", "reason": reason})),
            )
        }
    };
    let held_reason = if explicit_mode {
        "already_preparing"
    } else {
        "active_pilot_prepared"
    };
    let mut task_ids = Vec::with_capacity(retained.len());
    let mut task_snapshots = Vec::with_capacity(retained.len());
    let mut task_crews = Vec::with_capacity(retained.len());
    for (task_id, snapshot, crew) in retained {
        if held.contains_key(&task_id) {
            excluded.record(&task_id, held_reason, &held);
            continue;
        }
        task_ids.push(task_id);
        task_snapshots.push(snapshot);
        task_crews.push(crew);
    }

    // One crew per partition, then the size cap. A preparation pass may carry
    // siblings whose stored crews differ [ORB-16839]; each pilot worker still
    // sees a single crew. Same-crew selection order is unchanged, so a
    // homogeneous batch partitions exactly as a size-only split did. When the
    // task-pilot pool has `<crew>@auto` members, tasks also split by the
    // effort their filed complexity resolves to, so each partition runs at a
    // single effort; the partition carries the signal it resolves from.
    let mut crews_seen: Vec<(Option<String>, Option<String>)> = Vec::new();
    let mut groups: Vec<(Vec<String>, Option<serde_json::Value>)> = Vec::new();
    for (task_id, (crew, effort_key, complexity)) in task_ids.iter().zip(task_crews) {
        let key = (crew, effort_key);
        if let Some(index) = crews_seen.iter().position(|existing| existing == &key) {
            groups[index].0.push(task_id.clone());
        } else {
            let signal = super::auto_effort::partition_signal(key.1.as_deref(), complexity);
            crews_seen.push(key);
            groups.push((vec![task_id.clone()], signal));
        }
    }
    let partitions = groups
        .iter()
        .flat_map(|(ids, signal)| {
            ids.chunks(max_partition_size)
                .map(move |chunk| (chunk, signal.as_ref()))
        })
        .enumerate()
        .map(|(partition_index, (ids, signal))| {
            let mut partition = json!({
                "partition_index": partition_index,
                "task_ids": ids,
            });
            if let Some(signal) = signal {
                partition[super::auto_effort::AUTO_EFFORT_SIGNAL] = signal.clone();
            }
            let operator_tools = ids
                .iter()
                .filter_map(|id| {
                    task_snapshots
                        .iter()
                        .find(|snapshot| snapshot["task_id"].as_str() == Some(id.as_str()))
                        .and_then(|snapshot| snapshot.get(OPERATOR_TOOL_CANDIDATES))
                        .map(|candidates| (id.clone(), candidates.clone()))
                })
                .collect::<serde_json::Map<_, _>>();
            if !operator_tools.is_empty() {
                partition[OPERATOR_TOOL_CANDIDATES] = Value::Object(operator_tools);
            }
            if let Some(rechecks) = super::recheck::partition_rechecks(ids.iter(), &task_snapshots)
            {
                partition[super::recheck::RECHECKS] = Value::Object(rechecks);
            }
            partition
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
        // Callable tools of the activity that will implement the task, plus
        // both artifact-attachment paths. Null when that activity cannot be read.
        "implementation_lane": lane.prompt_input(),
        // Results of this run must carry the observed-revision proof.
        crate::application::task::pilot::source::INSPECTION_PROOF_KEY: true,
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
        "reservation_deferred": reservation_deferred,
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

/// Upper bound on `recheck_max_age_hours`: one year.
const HARD_MAX_RECHECK_AGE_HOURS: u64 = 24 * 365;

/// The run's re-check request: `full_pilot` forces a full assessment, and
/// `recheck_max_age_hours` bounds a prior assessment's age (0 disables
/// re-checks).
fn recheck_request(
    action: &str,
    input: &Value,
) -> Result<super::recheck::RecheckRequest, DispatchError> {
    let full = match input.get("full_pilot") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(full)) => *full,
        Some(_) => {
            return Err(DispatchError::action_failed(
                action,
                "`full_pilot` must be a boolean",
            ));
        }
    };
    let hours = match input.get("recheck_max_age_hours") {
        None | Some(Value::Null) => super::recheck::DEFAULT_RECHECK_MAX_AGE_HOURS,
        Some(value) => value
            .as_u64()
            .filter(|hours| *hours <= HARD_MAX_RECHECK_AGE_HOURS)
            .ok_or_else(|| {
                DispatchError::action_failed(
                    action,
                    format!(
                        "`recheck_max_age_hours` must be between 0 and {HARD_MAX_RECHECK_AGE_HOURS}"
                    ),
                )
            })?,
    };
    Ok(super::recheck::RecheckRequest {
        full,
        max_age: chrono::Duration::hours(hours as i64),
        now: chrono::Utc::now(),
    })
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
fn left_automatic_selection(
    selected: &Value,
    task: &Task,
    stale_repilot: bool,
    pr_route_lacks_forge: bool,
) -> Option<Value> {
    let exclusion = automatic_exclusion_reason(
        task.status,
        &task.context_files,
        task.complexity,
        &task.tags,
    )?;
    // A stale-repilot selection keeps its assessed selectors and complexity:
    // they are the stale assessment's output, not a move out of selection.
    // Independent exclusions hidden behind that reason (no-diff, missing
    // forge) still mark a move out of selection; active preparations are
    // re-checked at reservation below.
    if stale_repilot
        && exclusion == "context_files_not_empty"
        && no_diff_exclusion_reason(&task.tags).is_none()
        && !pr_route_lacks_forge
    {
        return None;
    }
    let (reason, detail) = if task.status.is_terminal() {
        ("task_terminal", "task became terminal after selection")
    } else if selected["status"] != json!(task.status) {
        (
            "status_changed",
            "a durable task transition superseded preparation",
        )
    } else {
        ("task_edited", "task fields changed after selection")
    };
    Some(super::superseded_outcome(
        &task.id,
        reason,
        task.status,
        detail,
    ))
}

/// Whether `task_id`'s latest applied assessment no longer certifies its
/// current meaning (description or acceptance criteria changed after it),
/// no live execution claim holds the task, and no failed repilot holds its
/// current requirements:
/// automatic discovery reselects such a task for a fresh pilot instead of
/// leaving it held for an operator repilot. A failed repilot is the newest
/// `task_pilot_unresolved` record after the latest apply; it keeps the task
/// held until a later description/criteria edit, a newer assessment or an
/// operator action, matching the drain's unresolved hold.
fn stale_repilot_candidate(
    runtime: &OrbitRuntime,
    action: &str,
    task_id: &str,
    claimed_tasks: &BTreeSet<String>,
) -> Result<bool, DispatchError> {
    if claimed_tasks.contains(task_id) {
        return Ok(false);
    }
    let task = runtime.get_task(task_id).map_err(|error| {
        DispatchError::action_failed(action, format!("read task {task_id}: {error}"))
    })?;
    if !matches!(task.status, TaskStatus::Proposed | TaskStatus::Backlog) {
        return Ok(false);
    }
    let history = runtime.get_task_history(task_id).map_err(|error| {
        DispatchError::action_failed(action, format!("read task {task_id} history: {error}"))
    })?;
    let latest_applied = history
        .iter()
        .rev()
        .find(|entry| entry.event == "task_pilot_applied")
        .map(|entry| entry.at);
    // Only a later requirements edit releases a failed repilot. Envelope,
    // plan and execution-summary updates must not restart automatic retries.
    let latest_requirements_edit = history
        .iter()
        .rev()
        .find(|entry| {
            entry.event == TASK_FIELDS_UPDATED_EVENT
                && task_fields_updated_documents(entry.note.as_deref().unwrap_or_default())
                    .iter()
                    .any(|document| {
                        matches!(
                            *document,
                            DESCRIPTION_DOCUMENT | ACCEPTANCE_CRITERIA_DOCUMENT
                        )
                    })
        })
        .map(|entry| entry.at);
    if history.iter().any(|entry| {
        entry.event == "task_pilot_unresolved"
            && latest_applied.is_none_or(|applied| entry.at > applied)
            && latest_requirements_edit.is_none_or(|edited| entry.at >= edited)
    }) {
        return Ok(false);
    }
    let comments = runtime.get_task_comments(task_id).map_err(|error| {
        DispatchError::action_failed(action, format!("read task {task_id} comments: {error}"))
    })?;
    let Some((_, receipt)) = super::pilot_receipts(&comments).next() else {
        return Ok(false);
    };
    if !super::is_operation_id(receipt.operation_id) {
        return Ok(false);
    }
    let Ok(audit) = serde_json::from_str::<Value>(receipt.body) else {
        return Ok(false);
    };
    super::freshness::pilot_requirements_stale(&task, &audit, receipt.operation_id, &|| {
        runtime.get_task_history(task_id)
    })
    .map_err(|error| DispatchError::action_failed(action, error.to_string()))
}

fn already_piloted(
    runtime: &OrbitRuntime,
    action: &str,
    task_id: &str,
) -> Result<bool, DispatchError> {
    if runtime
        .task_pilot_scheduling_assessment(task_id)
        .map_err(|error| {
            DispatchError::action_failed(action, format!("read task {task_id} assessment: {error}"))
        })?
        .is_some()
    {
        return Ok(true);
    }
    let history = runtime.get_task_history(task_id).map_err(|error| {
        DispatchError::action_failed(action, format!("read task {task_id} history: {error}"))
    })?;
    let Some(applied) = history
        .iter()
        .rev()
        .find(|entry| entry.event == "task_pilot_applied")
    else {
        return Ok(false);
    };
    let comments = runtime.get_task_comments(task_id).map_err(|error| {
        DispatchError::action_failed(action, format!("read task {task_id} comments: {error}"))
    })?;
    let no_target_assessment = persist::no_target_pilot_assessment(applied, &comments);
    Ok(no_target_assessment.is_some())
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
    no_diff_exclusion_reason(tags)
}

/// The no-diff exclusion alone: an independent gate that the assessed
/// footprint reason above can hide, checked separately wherever that
/// reason is waived for a stale repilot.
fn no_diff_exclusion_reason(tags: &[String]) -> Option<&'static str> {
    tags.iter()
        .any(|tag| NO_DIFF_TAGS.contains(&tag.as_str()))
        .then_some("no_diff_task")
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
