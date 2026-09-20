use std::borrow::Borrow;
use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::fs::overlap_index::OverlapIndex;
use orbit_engine::DispatchError;
use orbit_types::task::{
    EpicHierarchyNode, NO_DIFF_EXPECTED_TAG, Task, TaskComplexity, TaskReferenceIndex, TaskStatus,
    has_epic_tag, inherited_only_epic_roots, task_dependencies_ready_with_index,
};
use serde::Serialize;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::job::crew_pools::CapturedCrewPools;
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
    /// What an operator has to change for this task to become admissible, on
    /// an exclusion the drain cannot resolve by itself. A lock conflict clears
    /// when the holder finishes and needs no instruction; an
    /// [`BacklogTaskExclusionReason::InheritedOnlyEpicRoot`] never clears until
    /// the task is edited, so it carries one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) detail: Option<String>,
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
    GroupMemberConflict,
    /// An `epic`-tagged root that declared no `context_files` of its own while
    /// its descendants did. Admission no longer inherits their surface, so the
    /// root would take a slot while reserving nothing — the population is
    /// [`orbit_types::task::inherited_only_epic_roots`]. Only an edit to the
    /// task clears this, so the exclusion carries a `detail`.
    InheritedOnlyEpicRoot,
    /// Automated work must be prepared before an implementation lane can
    /// consume it; urgency does not substitute for a complexity assessment.
    /// Work tagged [`NO_DIFF_EXPECTED_TAG`] is exempt — see
    /// [`clears_complexity_gate`].
    UnassessedComplexity,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(super) struct BacklogTaskConflict {
    pub(super) requested_file: String,
    pub(super) locking_task_id: String,
}

/// The task population and leaf eligibility result shared by automatic
/// dispatch and its read-only diagnostic.  Keeping the lock filter here
/// prevents the diagnostic from becoming a second scheduler.
pub(super) struct BacklogSnapshot {
    /// Every task in the workspace, whole: parent-chain walks and the
    /// group-conflict roll-up read tasks at any status, which a status-filtered
    /// map would silently shorten. This is the one materialized copy; the other
    /// fields refer into it by ID rather than holding clones.
    pub(super) task_lookup: BTreeMap<String, Task>,
    /// The registry-global status projection. Deliberately not derived from
    /// `task_lookup`: task lists are workspace-scoped, dependency readiness
    /// is not, and a dependency on a task in another workspace resolves only
    /// here.
    pub(super) status_by_id: BTreeMap<String, TaskStatus>,
    pub(super) reference_index: TaskReferenceIndex,
    /// Admissible leaf task IDs in dispatch order; the tasks are in
    /// `task_lookup`.
    pub(super) admissible_leaves: Vec<String>,
    pub(super) excluded: Vec<BacklogTaskExclusion>,
    /// Selector -> the `in-progress` / `review` tasks holding it. Carried on
    /// the snapshot rather than recomputed by each consumer so admission
    /// selection, exclusion reasons, and the lock-wait diagnostic all name the
    /// same holder for the same selector [ORB-11973].
    pub(super) lock_holders: BTreeMap<String, Vec<String>>,
}

/// Expand each `in-progress` / `review` surface exactly once. Expansion is
/// the expensive half — every selector is checked against the filesystem —
/// so the map is built here and every consumer (exclusion, admission, the
/// lock-wait diagnostic) reads it rather than expanding again.
fn active_task_lock_holders(
    tasks: &BTreeMap<String, Task>,
    workspace_root: &Path,
) -> BTreeMap<String, Vec<String>> {
    let mut holders: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for task in tasks.values() {
        if matches!(task.status, TaskStatus::InProgress | TaskStatus::Review) {
            for file in lock_context_files_for_task(task, workspace_root) {
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

/// The holders keyed by anchor, so one backlog task's overlap check is a
/// prefix lookup per requested selector rather than a pass over every held
/// one.
fn lock_holder_index(lock_holders: &BTreeMap<String, Vec<String>>) -> OverlapIndex<&[String]> {
    let mut index = OverlapIndex::new();
    for (selector, locking_task_ids) in lock_holders {
        index.insert(selector, locking_task_ids.as_slice());
    }
    index
}

fn task_overlap_conflicts(
    task: &Task,
    holders: &OverlapIndex<&[String]>,
    workspace_root: &Path,
) -> Vec<BacklogTaskConflict> {
    let mut conflicts = Vec::new();
    for requested_file in lock_context_files_for_task(task, workspace_root) {
        for (_, locking_task_ids) in holders.overlapping(&requested_file) {
            for locking_task_id in locking_task_ids.iter() {
                conflicts.push(BacklogTaskConflict {
                    requested_file: requested_file.clone(),
                    locking_task_id: locking_task_id.clone(),
                });
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
        let pools = if input.get("auto_crew_pools").is_some()
            || action == "classify_workspace_auto_tasks"
        {
            runtime.auto_crew_pools_for_input(input).map_err(|error| {
                DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: error.to_string(),
                }
            })?
        } else {
            CapturedCrewPools::new()
        };
        let mut snapshot = backlog_snapshot(
            runtime,
            action,
            allowlist_from_input(runtime, action, input)?.as_ref(),
            &pools,
        )?;
        // Nothing reads the lookup after this, so the admissible tasks move
        // out of it rather than being cloned; the rest is dropped with it.
        let tasks = snapshot
            .admissible_leaves
            .iter()
            .take(max_tasks)
            .filter_map(|task_id| snapshot.task_lookup.remove(task_id))
            .collect();
        (tasks, snapshot.excluded)
    } else {
        let mut tasks = Vec::new();
        let mut excluded = Vec::new();
        // The hierarchy the inherited-only diagnostic reads, materialized only
        // if an override actually names such a root: the explicit path is
        // deliberately a per-id load, and one selected ship should not pay for
        // a whole-workspace listing.
        let mut workspace_tasks: Option<Vec<Task>> = None;
        for task_id in &explicit_task_ids {
            let task = runtime.get_task(task_id).map_err(|err| {
                DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: format!("load task {task_id}: {err}"),
                }
            })?;
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
            // An override picks which task ships, not whether it may reserve
            // nothing: an inherited-only root is withheld here exactly as the
            // drain withholds it, and for the same reason. The cheap test is a
            // superset of the rule — every inherited-only root is tagged and
            // undeclared — so it decides only who pays for the hierarchy read,
            // never who is withheld.
            if has_epic_tag(&task.tags) && task.context_files.is_empty() {
                if workspace_tasks.is_none() {
                    workspace_tasks = Some(runtime.list_tasks().map_err(|err| {
                        DispatchError::DeterministicActionFailed {
                            action: action.to_string(),
                            message: format!("list tasks: {err}"),
                        }
                    })?);
                }
                let inherited_only_roots = inherited_only_epic_roots(
                    workspace_tasks
                        .as_deref()
                        .unwrap_or_default()
                        .iter()
                        .map(EpicHierarchyNode::from),
                );
                if let Some(descendants) = inherited_only_roots.get(task.id.as_str()) {
                    excluded.push(inherited_only_epic_root_exclusion(&task.id, descendants));
                    continue;
                }
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
    pools: &CapturedCrewPools,
) -> Result<BacklogSnapshot, DispatchError> {
    // The population is materialized once, by moving the listing into the
    // lookup. Everything below borrows from it: the backlog is a vector of
    // references and the snapshot hands back IDs.
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
    // One status projection per snapshot. It is the registry-global index,
    // not a re-read of `task_lookup`'s statuses (see `BacklogSnapshot`).
    let status_by_id =
        runtime
            .task_status_index()
            .map_err(|err| DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: format!("load global task status projection: {err}"),
            })?;
    let reference_index = TaskReferenceIndex::from_status_index(&status_by_id);
    let workspace_root = runtime.paths().repo_root.as_path();
    let lock_holders = active_task_lock_holders(&task_lookup, workspace_root);
    // `task_lookup` iterates in task-ID order rather than the store's
    // created-at order; `sort_tasks_for_automatic_dispatch` is a total order
    // ending in the task ID, so the dispatch sequence is unchanged.
    let mut backlog: Vec<&Task> = task_lookup
        .values()
        .filter(|task| {
            task.status == TaskStatus::Backlog
                && task_dependencies_ready_with_index(task, &status_by_id, &reference_index)
        })
        .collect();
    sort_tasks_for_automatic_dispatch(&mut backlog);
    let mut excluded = Vec::new();
    backlog.retain(|task| {
        if clears_complexity_gate(task) {
            return true;
        }
        excluded.push(BacklogTaskExclusion {
            id: task.id.clone(),
            reason: BacklogTaskExclusionReason::UnassessedComplexity,
            conflicts: Vec::new(),
            crew: None,
            detail: None,
        });
        false
    });
    // A root that declared no context of its own, while its descendants did,
    // inherits nothing now that epic execution is retired: it would take a slot
    // holding no reservation and race the very children that union covered.
    // Withheld here rather than at the conflict filter, which has no footprint
    // to weigh, and with a repair instruction because no later drain clears
    // it.
    {
        let inherited_only_roots =
            inherited_only_epic_roots(task_lookup.values().map(EpicHierarchyNode::from));
        backlog.retain(|task| {
            let Some(descendants) = inherited_only_roots.get(task.id.as_str()) else {
                return true;
            };
            excluded.push(inherited_only_epic_root_exclusion(&task.id, descendants));
            false
        });
    }
    // Once the assessment gate has held back unprepared work, the crew filter
    // runs before scheduling exclusions so a task reports the reason an
    // operator can act on — reassign it, or run a drain that permits its crew
    // — rather than a downstream lock reason. Everything that survives keeps
    // its ordinary priority/age order.
    if let Some(allowlist) = allowlist {
        backlog.retain(|task| {
            match runtime.auto_task_crew_candidates(task, pools, None) {
                // A parked pool member (weight 0) is never drawn, so it cannot
                // make a task eligible under this restriction either.
                Ok((crews, _))
                    if crews.iter().any(|candidate| {
                        candidate.weight > 0 && allowlist.permits(&candidate.crew)
                    }) =>
                {
                    true
                }
                // An unresolvable crew fails closed under an explicit
                // restriction: the drain cannot show it is permitted, and
                // guessing would spend a budget the operator scoped.
                resolution => {
                    excluded.push(BacklogTaskExclusion {
                        id: task.id.clone(),
                        reason: BacklogTaskExclusionReason::CrewNotAllowed,
                        conflicts: Vec::new(),
                        crew: Some(match resolution {
                            Ok((crews, _)) => crews
                                .into_iter()
                                .map(|candidate| candidate.crew.name)
                                .collect::<Vec<_>>()
                                .join(", "),
                            Err(error) => format!("<unresolved: {error}>"),
                        }),
                        detail: None,
                    });
                    false
                }
            }
        });
    }
    if !lock_holders.is_empty() {
        let holder_index = lock_holder_index(&lock_holders);
        let direct_conflicts: BTreeMap<String, Vec<BacklogTaskConflict>> = backlog
            .iter()
            .filter_map(|task| {
                let conflicts = task_overlap_conflicts(task, &holder_index, workspace_root);
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
                let root_id = task_root_id(task, &task_lookup);
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
                        detail: None,
                    });
                } else {
                    kept.push(task);
                }
            }
            backlog = kept;
        }
    }
    excluded.sort_by(|a, b| a.id.cmp(&b.id));
    let admissible_leaves = backlog.into_iter().map(|task| task.id.clone()).collect();
    Ok(BacklogSnapshot {
        task_lookup,
        status_by_id,
        reference_index,
        admissible_leaves,
        excluded,
        lock_holders,
    })
}

/// The exclusion for an inherited-only `epic` root.
///
/// Shared by automatic backlog selection and the explicit ship override so both
/// withhold the same population and say the same thing about it. The detail
/// names the descendants that do declare context: those are what an operator
/// writes the root's own `context_files` from, and they are why this root is
/// withheld at all.
fn inherited_only_epic_root_exclusion(
    task_id: &str,
    descendants_with_context: &[&str],
) -> BacklogTaskExclusion {
    BacklogTaskExclusion {
        id: task_id.to_string(),
        reason: BacklogTaskExclusionReason::InheritedOnlyEpicRoot,
        conflicts: Vec::new(),
        crew: None,
        detail: Some(format!(
            "this task carries the `epic` size tag and declares no `context_files` of its own, \
             while its descendants do. Admission no longer inherits their surface, so it would \
             run holding no reservation against the work it would touch. Declare its own surface \
             with `orbit task update --context`, or retire the root. Descendants that declare \
             context: {}.",
            descendants_with_context.join(", ")
        )),
    }
}

/// The one complexity-admission rule, shared by automatic backlog selection,
/// explicit ship selection, and the readiness diagnostic that reports their
/// exclusions.
///
/// An implementation lane needs a complexity assessment to size the work it is
/// about to do. Work tagged exactly `no-diff-expected` produces its durable
/// result outside the repository, so requiring task-pilot preparation only to
/// clear this gate withholds operational work for a judgement it does not
/// consume [ORB-12118]. The exemption is the tag alone: automated mint
/// provenance does not grant it, and nothing here rewrites the task's stored
/// complexity — an exempt task keeps `unassessed` and resolves its crew from
/// the configured crew or the workspace default.
fn clears_complexity_gate(task: &Task) -> bool {
    task.complexity.is_some_and(TaskComplexity::is_assessed)
        || task.tags.iter().any(|tag| tag == NO_DIFF_EXPECTED_TAG)
}

/// Sort owned or borrowed tasks into automatic dispatch order: critical
/// first, then corrective work, then priority, age, and the task ID as the
/// total tie-breaker.
pub(super) fn sort_tasks_for_automatic_dispatch<T: Borrow<Task>>(tasks: &mut [T]) {
    tasks.sort_by(|left, right| {
        orbit_types::task::automatic_dispatch_cmp(left.borrow(), right.borrow())
    });
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
