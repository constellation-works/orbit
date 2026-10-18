use std::collections::{BTreeMap, BTreeSet};

use orbit_engine::DispatchError;
use orbit_types::task::{EpicHierarchyNode, has_epic_tag, inherited_only_epic_roots};
use orbit_types::workflow::ShipMode;
use serde_json::Value;

use super::super::pass_error::PassError;
use super::super::unknown_footprint::{FootprintGuard, FootprintWave, whole_tree_holder};
use super::exclusions::{
    browser_kit_exclusion, clears_complexity_gate, footprint_exclusion, host_os_exclusion,
    inherited_only_epic_root_exclusion, pilot_finding_exclusion, pilot_preparation_exclusion,
};
use super::snapshot::backlog_snapshot_in_mode;
use super::{
    BacklogTaskConflict, BacklogTaskExclusion, BacklogTaskExclusionReason, workspace_ship_mode,
};
use crate::OrbitRuntime;
use crate::application::workspace_auto::read_live_leaf_runs;
use crate::runtime::engine::crew::CrewAllowlist;
use crate::runtime::task::locks::lock_context_files_for_task;

/// The tasks a live claim is currently executing [ORB-12500].
fn live_claim_task_ids(
    runtime: &OrbitRuntime,
    action: &str,
) -> Result<std::collections::BTreeSet<String>, DispatchError> {
    Ok(runtime
        .inspect_execution_claims()
        .map_err(|error| DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: format!("read execution claims: {error}"),
        })?
        .into_iter()
        .map(|inspection| inspection.claim)
        .filter(|claim| claim.phase.protects_footprint())
        .map(|claim| claim.task_id)
        .collect())
}

pub(crate) fn list_backlog_tasks(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    runtime.reclaim_worktrees_on_admission();
    runtime
        .record_backlog_pilot_operator_handoffs()
        .map_err(|error| DispatchError::DeterministicActionFailed {
            action: action.into(),
            message: format!("record pilot operator handoffs: {error}"),
        })?;
    let max_tasks = input
        .get("max_tasks")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .min(500) as usize;
    let explicit_task_ids: Vec<String> = input
        .get("task_ids")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let (mut tasks, excluded_entries) = if explicit_task_ids.is_empty() {
        // [ORB-12635] Discovery routes a crew-less task through its complexity
        // pool exactly as admission does. The run's frozen policy is reached
        // through the injected `run_id`, so this crew filter and
        // `install_auto_crew_admission` cannot disagree about which tasks an
        // allowlist permits; a direct call without a run reads the workspace's
        // current pools rather than falling back to the default crew chain.
        let pools = runtime.auto_crew_pools_for_input(input).map_err(|error| {
            DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: error.to_string(),
            }
        })?;
        let mode = match input.get("mode").and_then(Value::as_str) {
            Some(mode) => ShipMode::parse(mode.trim()).map_err(|error| {
                DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: error.to_string(),
                }
            })?,
            None => workspace_ship_mode(runtime),
        };
        let review = crate::application::review::input_run_review_admission(runtime, input)
            .map_err(|error| DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: error.to_string(),
            })?;
        let mut snapshot = backlog_snapshot_in_mode(
            runtime,
            action,
            allowlist_from_input(runtime, action, input)?.as_ref(),
            &pools,
            mode,
            review.as_ref(),
        )?;
        // [ORB-15191] A ship that fans out to more than one task holds work
        // with no footprint to the drain's rule: it waits for its pilot, or
        // goes alone. Other live leaves carrying tasks are the work in flight;
        // this run carries none yet.
        let live_tasks = read_live_leaf_runs(runtime)
            .map_err(|error| DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: format!("list live leaf runs: {error}"),
            })?
            .into_iter()
            .flat_map(|run| run.task_ids)
            .collect::<BTreeSet<_>>();
        snapshot
            .include_tasks(runtime, &live_tasks)
            .map_err(|error| DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: format!("read live leaf tasks: {error}"),
            })?;
        let mut wave = FootprintWave::new(FootprintGuard {
            enabled: max_tasks > 1,
            leaves_in_flight: !live_tasks.is_empty(),
            whole_tree_holder: whole_tree_holder(&live_tasks, &snapshot.task_lookup),
            waits: &snapshot.footprint_waits,
        });
        let mut shipped = Vec::new();
        for task_id in &snapshot.admissible_leaves {
            if shipped.len() >= max_tasks {
                break;
            }
            let Some(task) = snapshot.task_lookup.get(task_id) else {
                continue;
            };
            let edits = !lock_context_files_for_task(
                task,
                runtime.paths().repo_root.as_path(),
                runtime.unlocked_context_paths(),
            )
            .is_empty();
            match wave.awaiting(task).or_else(|| wave.check(task, edits)) {
                Some(deferred) => snapshot.excluded.push(footprint_exclusion(deferred)),
                None => {
                    wave.selected(task, edits);
                    shipped.push(task_id.clone());
                }
            }
        }
        snapshot.excluded.sort_by(|a, b| a.id.cmp(&b.id));
        // Nothing reads the lookup after this, so the shipped tasks move out
        // of it rather than being cloned; the rest is dropped with it.
        let tasks = shipped
            .iter()
            .filter_map(|task_id| snapshot.task_lookup.remove(task_id))
            .collect();
        (tasks, snapshot.excluded)
    } else {
        let mut tasks = Vec::new();
        let mut excluded = Vec::new();
        // [ORB-12500] An override picks which task ships; it does not let a
        // task a live claim is already executing be dispatched beside itself.
        // Discovery reaches the same rule through the claim footprints
        // `backlog_snapshot` merges into its holders.
        let claimed_tasks = live_claim_task_ids(runtime, action)?;
        let pilot_preparations = active_pilot_preparations(runtime, action)?;
        // The hierarchy the inherited-only diagnostic reads, materialized only
        // if an override actually names such a root: the explicit path is
        // deliberately a per-id load, and one selected ship should not pay for
        // a whole-workspace listing.
        // Built on first need and reused: the hierarchy read and walk cover
        // the whole workspace, so doing them per epic candidate is quadratic.
        let mut epic_roots: Option<BTreeMap<String, Vec<String>>> = None;
        let executor_host_os = runtime.executor_host_os_limits().map_err(|error| {
            DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: format!("read executor host operating systems: {error}"),
            }
        })?;
        let pools = runtime.auto_crew_pools_for_input(input).map_err(|error| {
            DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: error.to_string(),
            }
        })?;
        let kit_verified = runtime.browser_kit_verified();
        for task_id in &explicit_task_ids {
            let task = runtime.get_task(task_id).map_err(|err| {
                DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: format!("load task {task_id}: {err}"),
                }
            })?;
            if pilot_preparations.contains_key(&task.id) {
                excluded.push(pilot_preparation_exclusion(&task.id));
                continue;
            }
            let admitted_crew = runtime
                .admitted_task_crew_for_input(input, &task.id)
                .map_err(|error| DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: format!("read admitted task crew: {error}"),
                })?;
            if let Some(exclusion) = host_os_exclusion(
                runtime,
                &task,
                &executor_host_os,
                &pools,
                admitted_crew.as_deref(),
            ) {
                excluded.push(exclusion);
                continue;
            }
            if let Some(exclusion) = browser_kit_exclusion(&task, kit_verified) {
                excluded.push(exclusion);
                continue;
            }
            if claimed_tasks.contains(&task.id) {
                excluded.push(BacklogTaskExclusion {
                    conflicts: task
                        .context_files
                        .iter()
                        .map(|requested_file| BacklogTaskConflict {
                            requested_file: requested_file.clone(),
                            locking_task_id: task.id.clone(),
                        })
                        .collect(),
                    id: task.id,
                    reason: BacklogTaskExclusionReason::ContextLockConflict,
                    crew: None,
                    detail: Some(
                        "A live execution claim holds this task. It settles or is deliberately \
                         recovered; it is never dispatched a second time."
                            .to_string(),
                    ),
                });
                continue;
            }
            // An override picks which task ships, not whether it may reserve
            // nothing: an inherited-only root is withheld here exactly as the
            // drain withholds it, and for the same reason. The cheap test is a
            // superset of the rule — every inherited-only root is tagged and
            // undeclared — so it decides only who pays for the hierarchy read,
            // never who is withheld.
            if has_epic_tag(&task.tags) && task.context_files.is_empty() {
                if epic_roots.is_none() {
                    let workspace_tasks = runtime.list_task_metadata().map_err(|err| {
                        DispatchError::DeterministicActionFailed {
                            action: action.to_string(),
                            message: format!("list tasks: {err}"),
                        }
                    })?;
                    let roots = inherited_only_epic_roots(
                        workspace_tasks.iter().map(EpicHierarchyNode::from),
                    )
                    .into_iter()
                    .map(|(root, descendants)| {
                        let descendants = descendants.into_iter().map(str::to_string).collect();
                        (root.to_string(), descendants)
                    })
                    .collect();
                    epic_roots = Some(roots);
                }
                if let Some(descendants) = epic_roots
                    .as_ref()
                    .and_then(|roots| roots.get(task.id.as_str()))
                {
                    let descendants: Vec<&str> = descendants.iter().map(String::as_str).collect();
                    excluded.push(inherited_only_epic_root_exclusion(&task.id, &descendants));
                    continue;
                }
            }
            if let Some(exclusion) = pilot_finding_exclusion(runtime, action, &task)? {
                excluded.push(exclusion);
                continue;
            }
            if !clears_complexity_gate(&task) {
                excluded.push(BacklogTaskExclusion {
                    id: task.id,
                    reason: BacklogTaskExclusionReason::UnassessedComplexity,
                    conflicts: Vec::new(),
                    crew: None,
                    detail: None,
                });
                continue;
            }
            tasks.push(task);
        }
        (tasks, excluded)
    };
    tasks.truncate(max_tasks);
    let ids: Vec<String> = tasks.iter().map(|t| t.id.clone()).collect();
    let bundles: Vec<Vec<String>> = ids.iter().map(|task_id| vec![task_id.clone()]).collect();
    let task_objs: Vec<Value> = tasks
        .iter()
        .map(|t| {
            serde_json::json!({
                "id": t.id,
                "title": t.title,
                "type": t.task_type.to_string(),
                "priority": t.priority.to_string(),
                "context_files": t.context_files,
                "parent_id": t.parent_id(),
            })
        })
        .collect();
    let mut payload = serde_json::Map::new();
    payload.insert("task_count".to_string(), Value::from(task_objs.len()));
    payload.insert("task_ids".to_string(), serde_json::json!(ids));
    payload.insert("tasks".to_string(), serde_json::json!(task_objs));
    payload.insert("bundles".to_string(), serde_json::json!(bundles));
    // Keep this Rust serialization contract in sync with
    // crates/orbit-core/assets/activities/list_backlog_tasks.yaml.
    payload.insert(
        "excluded".to_string(),
        serde_json::to_value(excluded_entries).map_err(|err| {
            DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: format!("serialize excluded backlog tasks: {err}"),
            }
        })?,
    );
    Ok(Value::Object(payload))
}

/// The run-scoped crew allowlist carried on a deterministic action's input
/// [ORB-11242]. Absent or empty means unrestricted.
pub(crate) fn allowlist_from_input(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Option<CrewAllowlist>, DispatchError> {
    runtime.crew_allowlist_from_input(input).map_err(|error| {
        DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: format!("resolve run crew allowlist: {error}"),
        }
    })
}

pub(super) fn active_pilot_preparations(
    runtime: &OrbitRuntime,
    action: &str,
) -> Result<BTreeMap<String, std::collections::BTreeSet<String>>, PassError> {
    crate::application::automation::preparation::active_task_pilot_preparations(runtime)
        .map_err(|error| PassError::store(action, "read active pilot preparations", error))
}
