use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::fs::path::workspace_relative_paths_overlap;
use orbit_engine::DispatchError;
use orbit_types::task::{Task, TaskPriority, TaskStatus, TaskType, task_dependencies_ready};
use serde::Serialize;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::runtime::engine::crew::CrewAllowlist;
use crate::runtime::task::locks::lock_context_files_for_task;

const MAX_TASK_PARENT_CHAIN_DEPTH: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct BacklogTaskExclusion {
    pub(super) id: String,
    pub(super) reason: BacklogTaskExclusionReason,
    pub(super) conflicts: Vec<BacklogTaskConflict>,
    /// The crew the task would have dispatched as, on a
    /// [`BacklogTaskExclusionReason::CrewNotAllowed`] exclusion. Naming the
    /// *effective* crew — not the raw `task.crew`, which is often unset and
    /// inherited — is what makes the exclusion actionable [ORB-11242].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) crew: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum BacklogTaskExclusionReason {
    ContextLockConflict,
    /// The run window permits a set of crews and this task's effective crew is
    /// not one of them [ORB-11242]. The task is left in `backlog` exactly as
    /// it is — never silently re-crewed — and the remaining eligible work
    /// keeps filling the drain's slots.
    CrewNotAllowed,
    EpicChild,
    EpicRoot,
    GroupMemberConflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EpicFamilyMembership {
    Child,
    Root,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(super) struct BacklogTaskConflict {
    pub(super) requested_file: String,
    pub(super) locking_task_id: String,
}

/// The task population and leaf eligibility result shared by automatic
/// dispatch and its read-only diagnostic.  Keeping the lock/epic filter here
/// prevents the diagnostic from becoming a second scheduler.
pub(super) struct BacklogSnapshot {
    pub(super) task_lookup: BTreeMap<String, Task>,
    pub(super) status_by_id: BTreeMap<String, TaskStatus>,
    pub(super) admissible_leaves: Vec<Task>,
    pub(super) excluded: Vec<BacklogTaskExclusion>,
}

fn active_task_lock_holders(
    tasks: &BTreeMap<String, Task>,
    workspace_root: &Path,
) -> BTreeMap<String, Vec<String>> {
    let mut holders: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for task in tasks.values() {
        if matches!(task.status, TaskStatus::InProgress | TaskStatus::Review) {
            for file in lock_context_files_for_task(task, tasks, workspace_root) {
                holders.entry(file).or_default().push(task.id.clone());
            }
        }
    }
    for locking_task_ids in holders.values_mut() {
        locking_task_ids.sort();
        locking_task_ids.dedup();
    }
    holders
}

fn task_overlap_conflicts(
    task: &Task,
    task_lookup: &BTreeMap<String, Task>,
    holders: &BTreeMap<String, Vec<String>>,
    workspace_root: &Path,
) -> Vec<BacklogTaskConflict> {
    let mut conflicts = Vec::new();
    for requested_file in lock_context_files_for_task(task, task_lookup, workspace_root) {
        for (held_file, locking_task_ids) in holders {
            if workspace_relative_paths_overlap(&requested_file, held_file) {
                for locking_task_id in locking_task_ids {
                    conflicts.push(BacklogTaskConflict {
                        requested_file: requested_file.clone(),
                        locking_task_id: locking_task_id.clone(),
                    });
                }
            }
        }
    }
    conflicts.sort();
    conflicts.dedup();
    conflicts
}

pub(super) fn list_backlog_tasks(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
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
        // The lookup must stay whole: an epic's lock set is the union over its
        // descendants (`lock_context_files_for_task`), which is a downward walk
        // that a status-filtered map would silently shorten — narrowing it
        // would shrink lock coverage, not just the scan. Reducing the scan
        // itself needs a persisted parent -> children index; until then the
        // population is loaded once and the whole set is walked.
        //
        // What it must not do is materialize that population twice. Consume the
        // list into the lookup, then clone only the backlog tasks that survive
        // the filter — tens of clones on a large workspace instead of one per
        // task, and one copy held rather than two.
        let snapshot = backlog_snapshot(
            runtime,
            action,
            allowlist_from_input(runtime, action, input)?.as_ref(),
        )?;
        (snapshot.admissible_leaves, Some(snapshot.excluded))
    } else {
        let tasks = explicit_task_ids
            .iter()
            .map(|task_id| {
                runtime
                    .get_task(task_id)
                    .map_err(|err| DispatchError::DeterministicActionFailed {
                        action: action.to_string(),
                        message: format!("load task {task_id}: {err}"),
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        (tasks, None)
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
    if let Some(excluded) = excluded_entries {
        payload.insert(
            "excluded".to_string(),
            serde_json::to_value(excluded).map_err(|err| {
                DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: format!("serialize excluded backlog tasks: {err}"),
                }
            })?,
        );
    }
    Ok(Value::Object(payload))
}

/// The run-scoped crew allowlist carried on a deterministic action's input
/// [ORB-11242]. Absent or empty means unrestricted.
pub(super) fn allowlist_from_input(
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

pub(super) fn backlog_snapshot(
    runtime: &OrbitRuntime,
    action: &str,
    allowlist: Option<&CrewAllowlist>,
) -> Result<BacklogSnapshot, DispatchError> {
    let task_lookup: BTreeMap<String, Task> = runtime
        .stores()
        .tasks()
        .list_tasks()
        .map_err(|err| DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: format!("list tasks: {err}"),
        })?
        .into_iter()
        .map(|task| (task.id.clone(), task))
        .collect();
    let status_by_id =
        runtime
            .task_status_index()
            .map_err(|err| DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: format!("load global task status projection: {err}"),
            })?;
    let workspace_root = runtime.paths().repo_root.as_path();
    let lock_holders = active_task_lock_holders(&task_lookup, workspace_root);
    // `task_lookup` iterates in task-ID order rather than the store's
    // created-at order; `sort_tasks_for_automatic_dispatch` is a total order
    // ending in the task ID, so the dispatch sequence is unchanged.
    let mut backlog: Vec<Task> = task_lookup
        .values()
        .filter(|task| {
            task.status == TaskStatus::Backlog && task_dependencies_ready(task, &status_by_id)
        })
        .cloned()
        .collect();
    sort_tasks_for_automatic_dispatch(&mut backlog);
    let mut excluded = Vec::new();
    // [ORB-11242] The crew filter runs first so an excluded task reports the
    // reason an operator can act on — reassign it, or run a drain that permits
    // its crew — rather than a downstream epic/lock reason that would not
    // explain why a permitted-looking task never started. Everything that
    // survives keeps its ordinary priority/age order, so the remaining crews
    // go on filling the drain's slots at the usual rate.
    if let Some(allowlist) = allowlist {
        backlog.retain(|task| {
            match runtime.effective_task_crew(task) {
                Ok(crew) if allowlist.permits(&crew) => true,
                // An unresolvable crew fails closed under an explicit
                // restriction: the drain cannot show it is permitted, and
                // guessing would spend a budget the operator scoped.
                resolution => {
                    excluded.push(BacklogTaskExclusion {
                        id: task.id.clone(),
                        reason: BacklogTaskExclusionReason::CrewNotAllowed,
                        conflicts: Vec::new(),
                        crew: Some(match resolution {
                            Ok(crew) => crew.name,
                            Err(error) => format!("<unresolved: {error}>"),
                        }),
                    });
                    false
                }
            }
        });
    }
    backlog.retain(|task| {
        let Some(membership) = epic_family_membership(task, &task_lookup) else {
            return true;
        };
        excluded.push(BacklogTaskExclusion {
            id: task.id.clone(),
            reason: match membership {
                EpicFamilyMembership::Root => BacklogTaskExclusionReason::EpicRoot,
                EpicFamilyMembership::Child => BacklogTaskExclusionReason::EpicChild,
            },
            conflicts: Vec::new(),
            crew: None,
        });
        false
    });
    if !lock_holders.is_empty() {
        let direct_conflicts: BTreeMap<String, Vec<BacklogTaskConflict>> = backlog
            .iter()
            .filter_map(|task| {
                let conflicts =
                    task_overlap_conflicts(task, &task_lookup, &lock_holders, workspace_root);
                (!conflicts.is_empty()).then(|| (task.id.clone(), conflicts))
            })
            .collect();
        let mut root_trigger: BTreeMap<String, Vec<BacklogTaskConflict>> = BTreeMap::new();
        for task in &backlog {
            if let Some(conflicts) = direct_conflicts.get(&task.id) {
                let root_id = task_root_id(task, &task_lookup);
                root_trigger
                    .entry(root_id)
                    .or_insert_with(|| conflicts.clone());
            }
        }
        if !root_trigger.is_empty() {
            let mut kept = Vec::new();
            for task in backlog {
                let root_id = task_root_id(&task, &task_lookup);
                if let Some(trigger_conflicts) = root_trigger.get(&root_id) {
                    excluded.push(BacklogTaskExclusion {
                        id: task.id.clone(),
                        reason: if direct_conflicts.contains_key(&task.id) {
                            BacklogTaskExclusionReason::ContextLockConflict
                        } else {
                            BacklogTaskExclusionReason::GroupMemberConflict
                        },
                        conflicts: direct_conflicts
                            .get(&task.id)
                            .cloned()
                            .unwrap_or_else(|| trigger_conflicts.clone()),
                        crew: None,
                    });
                } else {
                    kept.push(task);
                }
            }
            backlog = kept;
        }
    }
    excluded.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(BacklogSnapshot {
        task_lookup,
        status_by_id,
        admissible_leaves: backlog,
        excluded,
    })
}

pub(super) fn sort_tasks_for_automatic_dispatch(tasks: &mut [Task]) {
    let dispatch_band = |task: &Task| {
        if task.priority == TaskPriority::Critical {
            0
        } else if task.task_type == TaskType::Bug
            || task
                .tags
                .iter()
                .any(|tag| matches!(tag.as_str(), "code-review" | "security-review"))
        {
            1
        } else {
            2
        }
    };
    let priority_rank = |priority: TaskPriority| match priority {
        TaskPriority::Critical => 0,
        TaskPriority::High => 1,
        TaskPriority::Medium => 2,
        TaskPriority::Low => 3,
    };
    tasks.sort_by(|left, right| {
        dispatch_band(left)
            .cmp(&dispatch_band(right))
            .then(priority_rank(left.priority).cmp(&priority_rank(right.priority)))
            .then(left.created_at.cmp(&right.created_at))
            .then(left.id.cmp(&right.id))
    });
}

pub(super) fn epic_family_membership(
    task: &Task,
    task_lookup: &BTreeMap<String, Task>,
) -> Option<EpicFamilyMembership> {
    if task.tags.iter().any(|tag| tag == "epic") {
        return Some(EpicFamilyMembership::Root);
    }

    let mut visited = vec![task.id.clone()];
    let mut next_parent_id = task.parent_id().map(ToOwned::to_owned);
    for _ in 0..MAX_TASK_PARENT_CHAIN_DEPTH {
        let parent_id = next_parent_id?;
        if visited.iter().any(|task_id| task_id == &parent_id) {
            return None;
        }
        let parent = task_lookup.get(&parent_id)?;
        if parent.tags.iter().any(|tag| tag == "epic") {
            return Some(EpicFamilyMembership::Child);
        }
        visited.push(parent.id.clone());
        next_parent_id = parent.parent_id().map(ToOwned::to_owned);
    }
    None
}

fn task_root_id(task: &Task, task_lookup: &BTreeMap<String, Task>) -> String {
    let mut path = vec![task.id.clone()];
    let mut root_id = task.id.clone();
    let mut next_parent_id = task.parent_id().map(ToOwned::to_owned);

    for _ in 0..MAX_TASK_PARENT_CHAIN_DEPTH {
        let Some(parent_id) = next_parent_id else {
            return root_id;
        };

        if let Some(cycle_start) = path.iter().position(|task_id| task_id == &parent_id) {
            return path[cycle_start..].iter().min().cloned().unwrap_or(root_id);
        }

        let Some(parent) = task_lookup.get(&parent_id) else {
            return root_id;
        };

        root_id = parent.id.clone();
        path.push(parent.id.clone());
        next_parent_id = parent.parent_id().map(ToOwned::to_owned);
    }

    root_id
}
