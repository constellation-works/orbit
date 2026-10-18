use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_types::task::{
    EpicHierarchyNode, Task, TaskReferenceIndex, TaskStatus, inherited_only_epic_roots,
    task_dependencies_ready_with_index,
};
use orbit_types::workflow::{REVIEW_RUN_OPTION_SOURCE, ReviewAdmission, ReviewLayer, ShipMode};

use super::super::pass_error::PassError;
use super::super::surface_reservation::{
    MAX_SURFACE_RESERVATIONS, reserves_surface, reserving_detail, withhold_reserved_surfaces,
};
use super::super::unknown_footprint::footprint_waits;
use super::exclusions::{
    clears_complexity_gate, host_os_exclusion, inherited_only_epic_root_exclusion,
    pilot_finding_exclusion, pilot_preparation_exclusion,
};
use super::listing::active_pilot_preparations;
use super::ordering::task_root_id;
use super::population::admission_population;
use super::{
    BacklogSnapshot, BacklogTaskConflict, BacklogTaskExclusion, BacklogTaskExclusionReason,
    sort_tasks_for_automatic_dispatch,
};
use crate::OrbitRuntime;
use crate::application::job::crew_pools::CapturedCrewPools;
use crate::runtime::engine::crew::CrewAllowlist;
use crate::runtime::task::locks::{
    active_task_lock_holders, lock_holder_index, task_lock_overlaps,
};

/// The ship mode an unattended drain delivers in.
///
/// [`OrbitRuntime::automatic_delivery_ship_mode`]: the workspace binding's
/// mode, local when the workspace is unregistered. Readiness and doctor call
/// the same method.
pub(crate) fn workspace_ship_mode(runtime: &OrbitRuntime) -> ShipMode {
    runtime.automatic_delivery_ship_mode()
}

/// The drain's snapshot: admitted work is delivered in the workspace's ship
/// mode. `review` is the admitting run's captured review admission, when
/// there is one.
pub(crate) fn backlog_snapshot(
    runtime: &OrbitRuntime,
    action: &str,
    allowlist: Option<&CrewAllowlist>,
    pools: &CapturedCrewPools,
    review: Option<&ReviewAdmission>,
) -> Result<BacklogSnapshot, PassError> {
    backlog_snapshot_in_mode(
        runtime,
        action,
        allowlist,
        pools,
        workspace_ship_mode(runtime),
        review,
    )
}

/// `mode` is the ship mode the admitted work will be delivered in; a task's
/// `delivery:<job>` selection is checked against it.
pub(super) fn backlog_snapshot_in_mode(
    runtime: &OrbitRuntime,
    action: &str,
    allowlist: Option<&CrewAllowlist>,
    pools: &CapturedCrewPools,
    mode: ShipMode,
    review: Option<&ReviewAdmission>,
) -> Result<BacklogSnapshot, PassError> {
    // The population is materialized once, by moving the selection into the
    // lookup. Everything below borrows from it: the backlog is a vector of
    // references and the snapshot hands back IDs.
    let task_lookup = admission_population(runtime.stores().tasks())
        .map_err(|error| PassError::store(action, "list tasks", error))?;
    // One status projection per snapshot. It is the registry-global index,
    // not a re-read of `task_lookup`'s statuses (see `BacklogSnapshot`).
    let status_by_id = runtime
        .dependency_status_index(
            task_lookup
                .values()
                .filter(|task| task.status == TaskStatus::Backlog),
        )
        .map_err(|error| PassError::store(action, "load global task status projection", error))?;
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
    // [ORB-14624] A frozen batch about to pass its deadline would lose its
    // retry budget waiting in the queue. Ordering only: a store that cannot
    // be read leaves the ordinary order, never a failed pass.
    let expiring_batches = expiring_backlog_batches(runtime, &backlog);
    sort_tasks_for_automatic_dispatch(&mut backlog, &expiring_batches);
    let mut excluded = Vec::new();
    let executor_host_os = runtime
        .executor_host_os_limits()
        .map_err(|error| PassError::store(action, "read executor host operating systems", error))?;
    let pilot_preparations = active_pilot_preparations(runtime, action)?;
    backlog.retain(|task| {
        if !pilot_preparations.contains_key(&task.id) {
            return true;
        }
        excluded.push(pilot_preparation_exclusion(&task.id));
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
    let mut kept = Vec::with_capacity(backlog.len());
    for task in backlog {
        if let Some(exclusion) = pilot_finding_exclusion(runtime, action, task)? {
            excluded.push(exclusion);
        } else {
            kept.push(task);
        }
    }
    backlog = kept;
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
    // A selection the gate would refuse is withheld here instead: the task is
    // still `backlog` after a refused gate, so the drain would otherwise
    // dispatch it again, and fail again, on every pass. A task with no
    // `delivery:<job>` tag resolves to the default without a catalog read; the
    // checkout's remotes are read once, only if a task takes the PR route.
    let forge = crate::application::job::delivery::PrForgeCheck::default();
    let mut routed_locally = std::collections::BTreeSet::new();
    backlog.retain(|task| {
        let (reason, error) = match runtime.resolve_admitted_delivery_route(
            std::slice::from_ref(*task),
            mode,
            &forge,
        ) {
            Ok(route) => {
                if route.delivers_locally() {
                    routed_locally.insert(task.id.clone());
                }
                return true;
            }
            Err(error @ orbit_common::OrbitError::PrForgeRemoteMissing { .. }) => {
                (BacklogTaskExclusionReason::PrForgeRemoteMissing, error)
            }
            Err(error) => (BacklogTaskExclusionReason::DeliveryJobUnavailable, error),
        };
        excluded.push(BacklogTaskExclusion {
            id: task.id.clone(),
            reason,
            conflicts: Vec::new(),
            crew: None,
            detail: Some(error.to_string()),
        });
        false
    });
    // A task this host's OS cannot run is left for a host that can. Read every
    // pass, so a retagged backlog task is honoured at its next admission.
    backlog.retain(|task| {
        let Some(exclusion) = host_os_exclusion(runtime, task, &executor_host_os, pools, None)
        else {
            return true;
        };
        excluded.push(exclusion);
        false
    });
    // [ORB-14258] A task held for a red base waits until the command passes
    // on a new base tip; dispatching it would only fail the same command
    // again. Only the clock tick's recorded verdict is read here, never a
    // validation run [ORB-14739]. A hold that cannot be read is not one: the
    // delivery's own validation decides.
    backlog.retain(|task| match runtime.standing_baseline_hold(task) {
        Ok(None) => true,
        Ok(Some(why)) => {
            excluded.push(BacklogTaskExclusion {
                id: task.id.clone(),
                reason: BacklogTaskExclusionReason::BaselineRedHold,
                conflicts: Vec::new(),
                crew: None,
                detail: Some(why),
            });
            false
        }
        Err(error) => {
            tracing::warn!(task_id = %task.id, "could not read baseline red hold: {error}");
            true
        }
    });
    // [ORB-14266] A task whose provider failure hold excludes every crew it
    // could run as waits out the backoff; one that leaves another crew is
    // admitted and drawn onto it. An unreadable hold is not one.
    backlog.retain(
        |task| match runtime.provider_backoff_deferral(task, pools) {
            Ok(None) => true,
            Ok(Some(why)) => {
                excluded.push(BacklogTaskExclusion {
                    id: task.id.clone(),
                    reason: BacklogTaskExclusionReason::ProviderBackoff,
                    conflicts: Vec::new(),
                    crew: None,
                    detail: Some(why),
                });
                false
            }
            Err(error) => {
                tracing::warn!(task_id = %task.id, "could not read provider failure hold: {error}");
                true
            }
        },
    );
    // [ORB-14697] A task every crew of which is at its provider's usage
    // limit on this host waits for the reading's reset; one with an
    // unlimited crew left is admitted and drawn onto it.
    backlog.retain(|task| match runtime.provider_limit_deferral(task, pools) {
        Ok(None) => true,
        Ok(Some(why)) => {
            excluded.push(BacklogTaskExclusion {
                id: task.id.clone(),
                reason: BacklogTaskExclusionReason::ProviderLimit,
                conflicts: Vec::new(),
                crew: None,
                detail: Some(why),
            });
            false
        }
        Err(error) => {
            tracing::warn!(task_id = %task.id, "could not read provider limits: {error}");
            true
        }
    });
    // [ORB-14168] A task that cleared the per-task gates would still fail
    // closed at local-route admission while `review.before_pr` or
    // `review.before_landing` [ORB-14849] is on. Hold it here so the drain
    // does not spawn that delivery. Tasks already excluded above keep the
    // more specific reason. PR delivery skips this, and turning the switch
    // off (workspace overriding global included) clears it. A PR drain holds
    // only the tasks a `delivery:task_local_pipeline` tag routes locally.
    // [ORB-16572] A layer the admitting run added with `--review` holds them
    // too: its children inherit it whatever the policy says.
    let policy = runtime.operation_policy();
    let run_option_layer = review
        .filter(|admission| admission.timing_source == REVIEW_RUN_OPTION_SOURCE)
        .and_then(|admission| ReviewLayer::from_timing(admission.timing));
    let local_route_review = if let Some(layer) = run_option_layer {
        Some((
            match layer {
                ReviewLayer::BeforePr => BacklogTaskExclusionReason::LocalRouteBeforePr,
                ReviewLayer::BeforeLanding => BacklogTaskExclusionReason::LocalRouteBeforeLanding,
            },
            crate::application::review::local_route_run_option_refusal(layer),
        ))
    } else if policy.review_before_pr.value {
        Some((
            BacklogTaskExclusionReason::LocalRouteBeforePr,
            crate::application::review::local_route_before_pr_conflict(
                policy.review_before_pr.source.label(),
            ),
        ))
    } else if policy.review_before_landing.value {
        Some((
            BacklogTaskExclusionReason::LocalRouteBeforeLanding,
            crate::application::review::local_route_before_landing_conflict(
                policy.review_before_landing.source.label(),
            ),
        ))
    } else {
        None
    };
    if let Some((reason, detail)) = local_route_review
        && (mode == ShipMode::Local || !routed_locally.is_empty())
    {
        backlog.retain(|task| {
            if mode == ShipMode::Pr && !routed_locally.contains(&task.id) {
                return true;
            }
            excluded.push(BacklogTaskExclusion {
                id: task.id.clone(),
                reason,
                conflicts: Vec::new(),
                crew: None,
                detail: Some(detail.clone()),
            });
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
            // [ORB-14310] Lock-blocked tasks that reserve their surface this
            // pass, in dispatch order: the backlog is still sorted here.
            let mut reserving = Vec::new();
            for task in backlog {
                let root_id = task_root_id(task, &task_lookup);
                if let Some(trigger_conflicts) = root_trigger.get(&root_id) {
                    let direct = direct_conflicts.contains_key(&task.id);
                    let reserves = direct
                        && reserving.len() < MAX_SURFACE_RESERVATIONS
                        && reserves_surface(task);
                    if reserves {
                        reserving.push(task);
                    }
                    excluded.push(BacklogTaskExclusion {
                        id: task.id.clone(),
                        reason: if direct {
                            BacklogTaskExclusionReason::ContextLockConflict
                        } else {
                            BacklogTaskExclusionReason::GroupMemberConflict
                        },
                        conflicts: direct_conflicts
                            .get(&task.id)
                            .cloned()
                            .unwrap_or_else(|| trigger_conflicts.clone()),
                        crew: None,
                        detail: reserves.then(reserving_detail),
                    });
                } else {
                    kept.push(task);
                }
            }
            // A multi-lock task would otherwise lose every race: each lock it
            // waits on is taken by lower-ranked work the moment it frees.
            backlog = withhold_reserved_surfaces(
                kept,
                &reserving,
                &expiring_batches,
                workspace_root,
                &mut excluded,
            );
        }
    }
    excluded.sort_by(|a, b| a.id.cmp(&b.id));
    let footprint_waits = footprint_waits(runtime, action, backlog.iter().copied(), Utc::now())?;
    let admissible_leaves = backlog.into_iter().map(|task| task.id.clone()).collect();
    Ok(BacklogSnapshot {
        task_lookup,
        status_by_id,
        reference_index,
        admissible_leaves,
        excluded,
        lock_holders,
        expiring_batches,
        footprint_waits,
    })
}

/// The backlog tasks whose frozen batch nears its deadline [ORB-14624].
fn expiring_backlog_batches(
    runtime: &OrbitRuntime,
    backlog: &[&Task],
) -> BTreeMap<String, DateTime<Utc>> {
    match crate::application::automation::expiring_frozen_batch_tasks(runtime, Utc::now()) {
        Ok(mut expiring) => {
            expiring.retain(|task_id, _| backlog.iter().any(|task| task.id == *task_id));
            expiring
        }
        Err(error) => {
            tracing::warn!(
                "could not read frozen batch deadlines; backlog keeps its ordinary order: {error}"
            );
            BTreeMap::new()
        }
    }
}
