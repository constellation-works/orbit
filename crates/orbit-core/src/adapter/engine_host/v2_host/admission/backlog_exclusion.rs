use std::borrow::Borrow;
use std::collections::BTreeMap;

use orbit_engine::DispatchError;
use orbit_types::task::{
    EpicHierarchyNode, NO_DIFF_EXPECTED_TAG, Task, TaskComplexity, TaskOsRequirement,
    TaskReferenceIndex, TaskStatus, has_epic_tag, inherited_only_epic_roots,
    task_dependencies_ready_with_index,
};
use orbit_types::workflow::ShipMode;
use serde::Serialize;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::job::crew_pools::CapturedCrewPools;
use crate::application::task::list_task_metadata_in;
use crate::runtime::engine::crew::CrewAllowlist;
use crate::runtime::task::locks::{
    TaskLockOverlap, active_task_lock_holders, lock_holder_index, task_lock_overlaps,
};

const MAX_TASK_PARENT_CHAIN_DEPTH: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(in crate::adapter::engine_host::v2_host) struct BacklogTaskExclusion {
    pub(in crate::adapter::engine_host::v2_host) id: String,
    pub(in crate::adapter::engine_host::v2_host) reason: BacklogTaskExclusionReason,
    pub(in crate::adapter::engine_host::v2_host) conflicts: Vec<BacklogTaskConflict>,
    /// The crew the task would have dispatched as, on a
    /// [`BacklogTaskExclusionReason::CrewNotAllowed`] exclusion. Naming the
    /// *effective* crew — not the raw `task.crew`, which is often unset and
    /// inherited — is what makes the exclusion actionable [ORB-11242].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::adapter::engine_host::v2_host) crew: Option<String>,
    /// What an operator has to change for this task to become admissible, on
    /// an exclusion the drain cannot resolve by itself. A lock conflict clears
    /// when the holder finishes and needs no instruction; an
    /// [`BacklogTaskExclusionReason::InheritedOnlyEpicRoot`] never clears until
    /// the task is edited, so it carries one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::adapter::engine_host::v2_host) detail: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::adapter::engine_host::v2_host) enum BacklogTaskExclusionReason {
    ContextLockConflict,
    /// The task selects a delivery job with a `delivery:<job>` tag that cannot
    /// deliver it in the drain's ship mode — its plugin is disabled or
    /// uninstalled, or the job does not declare that mode. The task stays in
    /// `backlog` rather than failing a gate run on every drain pass; `detail`
    /// carries the refusal, naming the plugin when one is installed.
    DeliveryJobUnavailable,
    /// The run window permits a set of crews and this task's effective crew is
    /// not one of them [ORB-11242]. The task is left in `backlog` exactly as
    /// it is — never silently re-crewed — and the remaining eligible work
    /// keeps filling the drain's slots.
    CrewNotAllowed,
    /// The task's `os:` tags name no OS this host runs. It stays in `backlog`
    /// for a host that does — a follower pulling from this owner, say — and
    /// `detail` names the wait (`waits for a macos host (os:macos)`).
    HostOsMismatch,
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
    /// Effective `review.before_pr` is on and this delivery is the local-only
    /// route. Pipeline admission refuses that combination; the task stays in
    /// `backlog` until the switch is turned off or delivery uses the PR route.
    /// `detail` names the deciding config layer and the remedy [ORB-14168].
    LocalRouteBeforePr,
}

/// The overlap `orbit task eligible` reports, so a conflict means the same
/// thing to a drain and to an operator choosing work.
pub(in crate::adapter::engine_host::v2_host) type BacklogTaskConflict = TaskLockOverlap;

/// The task population and leaf eligibility result shared by automatic
/// dispatch and its read-only diagnostic.  Keeping the lock filter here
/// prevents the diagnostic from becoming a second scheduler.
pub(in crate::adapter::engine_host::v2_host) struct BacklogSnapshot {
    /// Every task in the workspace, at any status (parent-chain walks and the
    /// group-conflict roll-up read tasks at any status, which a status-filtered
    /// map would silently shorten), as envelope metadata: the body documents
    /// (`description`, `acceptance_criteria`, `plan`, `execution_summary`) are
    /// empty, and nothing in admission, exclusion or readiness reads them.
    /// This is the one materialized copy; the other fields refer into it by ID
    /// rather than holding clones.
    pub(in crate::adapter::engine_host::v2_host) task_lookup: BTreeMap<String, Task>,
    /// The registry-global status projection. Deliberately not derived from
    /// `task_lookup`: task lists are workspace-scoped, dependency readiness
    /// is not, and a dependency on a task in another workspace resolves only
    /// here. Backlog tasks' archived dependencies that reached `done` first
    /// project as `done` (`OrbitRuntime::dependency_status_index`).
    pub(in crate::adapter::engine_host::v2_host) status_by_id: BTreeMap<String, TaskStatus>,
    pub(in crate::adapter::engine_host::v2_host) reference_index: TaskReferenceIndex,
    /// Admissible leaf task IDs in dispatch order; the tasks are in
    /// `task_lookup`.
    pub(in crate::adapter::engine_host::v2_host) admissible_leaves: Vec<String>,
    pub(in crate::adapter::engine_host::v2_host) excluded: Vec<BacklogTaskExclusion>,
    /// Selector -> the `in-progress` / `review` tasks holding it. Carried on
    /// the snapshot rather than recomputed by each consumer so admission
    /// selection, exclusion reasons, and the lock-wait diagnostic all name the
    /// same holder for the same selector [ORB-11973].
    pub(in crate::adapter::engine_host::v2_host) lock_holders: BTreeMap<String, Vec<String>>,
}

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

pub(in crate::adapter::engine_host::v2_host) fn list_backlog_tasks(
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
        let mut snapshot = backlog_snapshot_in_mode(
            runtime,
            action,
            allowlist_from_input(runtime, action, input)?.as_ref(),
            &pools,
            mode,
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
        // [ORB-12500] An override picks which task ships; it does not let a
        // task a live claim is already executing be dispatched beside itself.
        // Discovery reaches the same rule through the claim footprints
        // `backlog_snapshot` merges into its holders.
        let claimed_tasks = live_claim_task_ids(runtime, action)?;
        // The hierarchy the inherited-only diagnostic reads, materialized only
        // if an override actually names such a root: the explicit path is
        // deliberately a per-id load, and one selected ship should not pay for
        // a whole-workspace listing.
        // Built on first need and reused: the hierarchy read and walk cover
        // the whole workspace, so doing them per epic candidate is quadratic.
        let mut epic_roots: Option<BTreeMap<String, Vec<String>>> = None;
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
            if let Some(exclusion) = host_os_exclusion(runtime, &task) {
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
pub(in crate::adapter::engine_host::v2_host) fn allowlist_from_input(
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

/// The ship mode an unattended drain delivers in.
///
/// [`OrbitRuntime::automatic_delivery_ship_mode`]: the workspace binding's
/// mode, local when the workspace is unregistered. Readiness and doctor call
/// the same method.
pub(in crate::adapter::engine_host::v2_host) fn workspace_ship_mode(
    runtime: &OrbitRuntime,
) -> ShipMode {
    runtime.automatic_delivery_ship_mode()
}

/// The drain's snapshot: admitted work is delivered in the workspace's ship
/// mode.
pub(in crate::adapter::engine_host::v2_host) fn backlog_snapshot(
    runtime: &OrbitRuntime,
    action: &str,
    allowlist: Option<&CrewAllowlist>,
    pools: &CapturedCrewPools,
) -> Result<BacklogSnapshot, DispatchError> {
    backlog_snapshot_in_mode(
        runtime,
        action,
        allowlist,
        pools,
        workspace_ship_mode(runtime),
    )
}

/// `mode` is the ship mode the admitted work will be delivered in; a task's
/// `delivery:<job>` selection is checked against it.
fn backlog_snapshot_in_mode(
    runtime: &OrbitRuntime,
    action: &str,
    allowlist: Option<&CrewAllowlist>,
    pools: &CapturedCrewPools,
    mode: ShipMode,
) -> Result<BacklogSnapshot, DispatchError> {
    // The population is materialized once, by moving the listing into the
    // lookup. Everything below borrows from it: the backlog is a vector of
    // references and the snapshot hands back IDs.
    let task_lookup: BTreeMap<String, Task> = list_task_metadata_in(runtime.stores().tasks())
        .map_err(|err| DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: format!("list tasks: {err}"),
        })?
        .into_iter()
        .map(|task| (task.id.clone(), task))
        .collect();
    // One status projection per snapshot. It is the registry-global index,
    // not a re-read of `task_lookup`'s statuses (see `BacklogSnapshot`).
    let status_by_id = runtime
        .dependency_status_index(
            task_lookup
                .values()
                .filter(|task| task.status == TaskStatus::Backlog),
        )
        .map_err(|err| DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: format!("load global task status projection: {err}"),
        })?;
    let reference_index = TaskReferenceIndex::from_status_index(&status_by_id);
    let workspace_root = runtime.paths().repo_root.as_path();
    // [ORB-12500] Claims need no separate holder entry here. A claimed task is
    // `in-progress` or `review`, so its surface is already in this map, and
    // the claim journal refuses every ordinary mutation of a claimed task —
    // including narrowing its `context_files` — so the recomputed lock cannot
    // drift from the footprint the claim froze. Consulting the claim ledger on
    // this path would only add a way for the drain's hot loop to fail on a
    // journal awaiting repair. The paths that can reach a claimed task
    // directly, rather than through the backlog, consult it themselves.
    // [ORB-14247] `no-diff-expected` is omitted from this map. The claim
    // journal still fences that task's own writes; it just does not exclude
    // overlapping backlog work.
    let lock_holders = active_task_lock_holders(task_lookup.values(), workspace_root);
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
    // A selection the gate would refuse is withheld here instead: the task is
    // still `backlog` after a refused gate, so the drain would otherwise
    // dispatch it again, and fail again, on every pass. A task with no
    // `delivery:<job>` tag resolves to the default without a catalog read.
    backlog.retain(|task| {
        let Err(error) = runtime.resolve_delivery_route(std::slice::from_ref(*task), mode) else {
            return true;
        };
        excluded.push(BacklogTaskExclusion {
            id: task.id.clone(),
            reason: BacklogTaskExclusionReason::DeliveryJobUnavailable,
            conflicts: Vec::new(),
            crew: None,
            detail: Some(error.to_string()),
        });
        false
    });
    // A task this host's OS cannot run is left for a host that can. Read every
    // pass, so a retagged backlog task is honoured at its next admission.
    backlog.retain(|task| {
        let Some(exclusion) = host_os_exclusion(runtime, task) else {
            return true;
        };
        excluded.push(exclusion);
        false
    });
    // [ORB-14168] A task that cleared the per-task gates would still fail
    // closed at local-route admission while `review.before_pr` is on. Hold it
    // here so the drain does not spawn that delivery. Tasks already excluded
    // above keep the more specific reason. PR delivery skips this, and turning
    // the switch off (workspace overriding global included) clears it.
    if mode == ShipMode::Local && runtime.operation_policy().review_before_pr.value {
        let detail = crate::application::review::local_route_before_pr_conflict(
            runtime.operation_policy().review_before_pr.source.label(),
        );
        for task in backlog.drain(..) {
            excluded.push(BacklogTaskExclusion {
                id: task.id.clone(),
                reason: BacklogTaskExclusionReason::LocalRouteBeforePr,
                conflicts: Vec::new(),
                crew: None,
                detail: Some(detail.clone()),
            });
        }
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
                let conflicts = task_lock_overlaps(task, &holder_index, workspace_root);
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

/// The exclusion for a task whose `os:` tags this host does not satisfy.
///
/// Shared by automatic selection and the explicit ship override, so a drain,
/// ship discovery and a gate carrying a named task withhold the same tasks and
/// name the same wait.
fn host_os_exclusion(runtime: &OrbitRuntime, task: &Task) -> Option<BacklogTaskExclusion> {
    let reason = TaskOsRequirement::from_tags(&task.tags).unsatisfied_reason(runtime.host_os())?;
    Some(BacklogTaskExclusion {
        id: task.id.clone(),
        reason: BacklogTaskExclusionReason::HostOsMismatch,
        conflicts: Vec::new(),
        crew: None,
        detail: Some(reason),
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
pub(in crate::adapter::engine_host::v2_host) fn sort_tasks_for_automatic_dispatch<
    T: Borrow<Task>,
>(
    tasks: &mut [T],
) {
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
