use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::task::{
    Task, TaskReferenceIndex, TaskStatus, task_dependencies_ready_with_index,
    unmet_task_dependencies_with_index,
};
use orbit_types::workflow::{DrainAdmissionsStop, DrainWorkerLimit, OperationAdmission};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::job::crew_pools::CapturedCrewPools;
use crate::application::operation::{admission_state, live_admission, promote_within_grant};

use crate::runtime::engine::crew::CrewAllowlist;

use super::auto_admission::{AdmissionHolders, select_admissions};
use super::backlog_exclusion::{
    BacklogTaskExclusionReason, EpicFamilyMembership, allowlist_from_input, backlog_snapshot,
    epic_family_membership, sort_tasks_for_automatic_dispatch,
};
use super::leaf_occupancy::{occupancy_json, read_leaf_occupancy};

/// The job that supervises one epic root. `classify_workspace_auto_tasks`
/// reads its live runs to decide whether another root may start.
const EPIC_JOB_NAME: &str = "epic_pipeline";

/// The job that ships loose leaves. Its live runs are read for two things at
/// once: how many slots are occupied, and which backlog tasks are already
/// spoken for. The second matters because a leaf handed to a detached child
/// stays `backlog` until that child moves it to `in-progress` — the child's
/// own run input is the only record of the claim in between, and without it
/// the next iteration would hand the same task to a second child.
const LEAF_JOB_NAME: &str = "task_auto_pipeline";

/// The drain job itself. Readiness reads its live run to report the ceiling a
/// running drain is actually admitting under [ORB-11253].
const DRAIN_JOB_NAME: &str = "workspace_auto_pipeline";

/// Default ceiling on concurrently live leaf runs. Matches the `max_workers`
/// the fan-out used while the drain waited on its leaves, so steady-state
/// parallelism is unchanged; what changed is that a slot reopens the moment
/// its own child finishes rather than when the slowest child in the batch does.
const DEFAULT_MAX_ACTIVE_LEAF_RUNS: u64 = 5;

/// Wait before re-listing when the backlog has admissible work but every slot
/// is occupied. This is the latency a freed slot sits idle, so it is much
/// shorter than the idle wait.
const DEFAULT_POLL_SLEEP_SECONDS: u64 = 30;

/// Wait before re-listing when nothing is admissible at all. Long, because the
/// only things that can change are a task arriving or the detached epic
/// finishing.
const DEFAULT_IDLE_SLEEP_SECONDS: u64 = 60;

const MAX_READINESS_LIMIT: usize = 500;

/// Default and ceiling for the backlog prefix one iteration examines, matching
/// `list_backlog_tasks`'s `max_tasks` bound.
const DEFAULT_CANDIDATE_POOL: u64 = 50;
const MAX_CANDIDATE_POOL: u64 = 500;

/// Longest drain window a caller may request, in seconds (24h). The window is
/// the caller's, not a safety property, but an unbounded deadline would let a
/// typo hold `workspace_auto_pipeline`'s single active-run slot indefinitely.
const MAX_DRAIN_WINDOW_SECONDS: f64 = 86_400.0;

/// The admissible work for one drain iteration [ORB-10819].
///
/// This answers "what may start right now", not "what is the one action for
/// this tick". Loose leaves and an epic root are independent answers: a
/// conflict-free chore ships in the same iteration that an epic is running,
/// because an `in-progress` epic root already reserves the union of its
/// descendants' `context_files` (ORB-10816) and `backlog_snapshot` drops
/// exactly the leaves that overlap it. That reservation is why the former
/// `hold` decision is gone — a blanket freeze excluded conflict-free work the
/// lock surface had no reason to exclude.
///
/// Leaves are offered up to the number of *free* slots rather than in one
/// batch, because the drain no longer waits on them. The whole backlog is
/// re-listed every iteration and the free slots are topped up from it, so a
/// task that entered `backlog` a minute ago starts as soon as any one child
/// finishes — not after the slowest member of the batch that was running when
/// it arrived. One task per dispatch: a multi-task child bought nothing but a
/// coarser refill unit, and a one-task child is crew-homogeneous by
/// construction.
///
/// [ORB-11973] Which leaves fill those slots is `select_admissions`' answer,
/// not a prefix of the queue: the wave is pairwise conflict-free, so a cluster
/// of overlapping candidates costs one slot instead of all of them. Readiness
/// reads the same snapshot and calls the same routine, which is what keeps the
/// diagnostic and the drain from disagreeing about who starts.
pub(super) fn classify_workspace_auto_tasks(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let submitted_max_active_leaf_runs = templated_u64(
        action,
        input,
        "max_active_leaf_runs",
        DEFAULT_MAX_ACTIVE_LEAF_RUNS,
    )?;
    // [ORB-11253] The submitted ceiling is a snapshot; the run's own control is
    // the live one. Reading it here, per iteration, is what makes an adjustment
    // take effect on the next admission without replacing the coordinator.
    let worker_limit = live_worker_limit(runtime, input);
    let max_active_leaf_runs = worker_limit
        .as_ref()
        .map_or(submitted_max_active_leaf_runs, |limit| {
            u64::from(limit.max_active_leaf_runs)
        });
    let poll_sleep_seconds = templated_u64(
        action,
        input,
        "poll_sleep_seconds",
        DEFAULT_POLL_SLEEP_SECONDS,
    )?;
    let idle_sleep_seconds = templated_u64(
        action,
        input,
        "idle_sleep_seconds",
        DEFAULT_IDLE_SLEEP_SECONDS,
    )?;

    let live_leaves = live_leaf_runs(runtime, action)?;
    let claimed: BTreeSet<String> = live_leaves
        .iter()
        .flat_map(|run| run.task_ids.iter().cloned())
        .collect();
    let admissions_stop = live_admissions_stop(runtime, input);
    let admissions_stopped = admissions_stop.is_some();

    // [ORB-11332] A grant-bound drain rechecks its grant every iteration:
    // stop, expiry, and revocation close the window here, the finite scope
    // bounds what may be offered, the captured ceiling bounds how much, and
    // fresh positively assessed proposed work inside the scope is promoted
    // before the backlog is listed. The Store transaction rechecks all of it
    // again at each child insert; this is the classifier's snapshot.
    let operation = grant_bound_admission(runtime, action, input)?;
    let operation_open = operation.as_ref().is_none_or(|operation| operation.open);
    let max_active_leaf_runs = operation
        .as_ref()
        .map_or(max_active_leaf_runs, |operation| {
            max_active_leaf_runs.min(u64::from(operation.leaf_ceiling))
        });
    let free_slots = if admissions_stopped || !operation_open {
        0
    } else {
        usize::try_from(max_active_leaf_runs)
            .unwrap_or(usize::MAX)
            .saturating_sub(live_leaves.len())
    };

    let pools = runtime
        .auto_crew_pools_for_input(input)
        .map_err(|error| action_failed(action, error.to_string()))?;
    let allowlist = allowlist_from_input(runtime, action, input)?;
    // The same snapshot readiness reads, so the two cannot disagree about the
    // eligible population before they even reach the selection rule.
    let snapshot = backlog_snapshot(runtime, action, allowlist.as_ref(), &pools)?;
    // Priority/age order is the snapshot's, and everything below preserves it:
    // the slots are scarce, so they go to the front of the queue rather than to
    // whichever tasks happen to sort last.
    let pending: Vec<String> = snapshot
        .admissible_leaves
        .iter()
        .map(|task| task.id.clone())
        .filter(|task_id| !claimed.contains(task_id))
        .filter(|task_id| {
            operation
                .as_ref()
                .is_none_or(|operation| operation.scope.contains(task_id))
        })
        .collect();

    // [ORB-11973] The wave used to be `pending[..free_slots]`, which could hand
    // every slot to one cluster of overlapping tasks and leave independent work
    // queued behind a contention it created itself. Select a mutually
    // compatible set instead, walking past a blocked candidate to the next
    // compatible one rather than stopping at it.
    let max_tasks = candidate_pool_limit(action, input)?;
    let examined = &pending[..pending.len().min(max_tasks)];
    let holders = AdmissionHolders::new(
        &snapshot.lock_holders,
        &claimed,
        &snapshot.task_lookup,
        runtime.paths().repo_root.as_path(),
    );
    let selection = select_admissions(
        examined,
        &snapshot.task_lookup,
        runtime.paths().repo_root.as_path(),
        &holders,
        free_slots,
    );
    // `max_tasks` bounds how much of the backlog one iteration expands lock
    // footprints for. It only hides work when the wave ran out of *examined*
    // candidates rather than out of slots, so report exactly that case instead
    // of leaving a short wave looking like an empty backlog.
    let candidate_pool_truncated =
        pending.len() > examined.len() && selection.selected.len() < free_slots;
    let admitted = &selection.selected;
    let loose_task_dispatches: Vec<Value> = admitted
        .iter()
        .map(|task_id| json!({ "task_ids": [task_id] }))
        .collect();

    let active_epic = active_epic_run(runtime, action)?;
    // One epic at a time. `epic_pipeline` declares `max_active_runs: 1`,
    // so offering a second root would not run it — it would queue a
    // `pending` run behind the live one, and the drain loop would mint a
    // fresh one every iteration. Keying on the run rather than on the
    // root's status also closes the window between a detached submit and
    // the child's `worktree_setup` moving that root to `in-progress`.
    // [ORB-11283] A stopped drain offers no new epic either.
    let epic_task_id = if admissions_stopped || !operation_open || active_epic.is_some() {
        None
    } else {
        next_admissible_epic_root(runtime, action, allowlist.as_ref(), &pools)?.filter(|root| {
            operation
                .as_ref()
                .is_none_or(|operation| operation.scope.contains(root))
        })
    };

    let has_leaves = !loose_task_dispatches.is_empty();
    let has_epic = epic_task_id.is_some();
    // Idle means "this iteration started nothing", which is not the same as
    // "there is nothing to do": a saturated drain with a full backlog behind
    // it is idle in this sense and waits the short poll, while a genuinely
    // empty workspace waits the long one.
    let idle = !has_leaves && !has_epic;
    let sleep_seconds = if pending.is_empty() {
        idle_sleep_seconds
    } else {
        poll_sleep_seconds
    };

    Ok(json!({
        "loose_task_ids": admitted,
        "loose_task_dispatches": loose_task_dispatches,
        "has_leaves": has_leaves,
        "epic_task_id": epic_task_id,
        "has_epic": has_epic,
        "idle": idle,
        "sleep_seconds": sleep_seconds,
        "pending_backlog": pending.len(),
        // [ORB-11973] Why a wave is shorter than its free slots. A deferral
        // names the tasks and selectors it would have collided with, so a drain
        // that looks under-filled can be read as contention rather than as an
        // empty backlog.
        "deferred_conflicts": selection.deferred_json(),
        "candidate_pool_size": examined.len(),
        "candidate_pool_truncated": candidate_pool_truncated,
        "active_leaf_runs": live_leaves.len(),
        "free_slots": free_slots,
        "max_active_leaf_runs": max_active_leaf_runs,
        "submitted_max_active_leaf_runs": submitted_max_active_leaf_runs,
        "worker_limit_source": if worker_limit.is_some() { "run_control" } else { "run_input" },
        "worker_limit": worker_limit,
        "admissions_stopped": admissions_stopped,
        "admissions_stop": admissions_stop,
        "active_epic_run_id": active_epic.as_ref().map(|epic| epic.run_id.clone()),
        "active_epic_task_id": active_epic.and_then(|epic| epic.task_id),
        "operation": operation.map(|operation| operation.report),
    }))
}

/// The grant-bound facts one classifier iteration works under.
struct GrantBoundAdmission {
    /// Whether the grant still admits new work.
    open: bool,
    /// The finite task set the drain may offer.
    scope: BTreeSet<String>,
    /// The captured leaf ceiling.
    leaf_ceiling: u32,
    /// The durable explanation carried in the step output.
    report: Value,
}

/// Recheck the coordinator's grant and promote eligible in-scope work.
/// `None` for a drain that was not admitted under a grant.
fn grant_bound_admission(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Option<GrantBoundAdmission>, DispatchError> {
    let Some(admission) = live_admission(runtime, input) else {
        return Ok(None);
    };
    let (grant, state) = admission_state(runtime, &admission)
        .map_err(|error| action_failed(action, format!("recheck operation grant: {error}")))?;
    let open = state.admits();
    let promotions = if open {
        promote_within_grant(runtime, &grant)
            .map_err(|error| action_failed(action, format!("operation promotion: {error}")))?
    } else {
        Vec::new()
    };
    Ok(Some(GrantBoundAdmission {
        open,
        scope: grant.task_ids.iter().cloned().collect(),
        leaf_ceiling: admission.limits.leaf_ceiling,
        report: json!({
            "grant_id": grant.id,
            "grant_revision": grant.revision,
            "admission": state.reason(),
            "expires_at": grant.expires_at.to_rfc3339(),
            "completion": admission.completion,
            "scope": grant.task_ids,
            "leaf_ceiling": admission.limits.leaf_ceiling,
            "promotions": promotions,
        }),
    }))
}

/// Explain the same snapshot that auto-drain uses without performing its
/// stale-run reconciliation. This is deliberately an observation API: it
/// neither reserves work nor creates a pipeline run, and its answer can go
/// stale immediately after the stores are read.
pub fn explain_workspace_auto_readiness(
    runtime: &OrbitRuntime,
    task_ids: &[String],
    max_active_leaf_runs: Option<u32>,
    limit: usize,
    allowed_crews: &[String],
) -> Result<Value, OrbitError> {
    if !(1..=MAX_READINESS_LIMIT).contains(&limit) {
        return Err(OrbitError::InvalidInput(format!(
            "readiness limit must be between 1 and {MAX_READINESS_LIMIT}"
        )));
    }
    if max_active_leaf_runs == Some(0) {
        return Err(OrbitError::InvalidInput(
            "concurrency must be at least 1".to_string(),
        ));
    }
    // [ORB-11253] Without an explicit `--concurrency`, report what the live
    // drain is admitting under — including an operator adjustment — rather than
    // the static default, so readiness and the drain cannot disagree about the
    // ceiling that decides `capacity_saturated`.
    let active_drain = active_drain(runtime)?;
    let (max_active_leaf_runs, limit_source) = match (max_active_leaf_runs, &active_drain) {
        (Some(requested), _) => (u64::from(requested), "requested"),
        (None, Some(drain)) => (
            u64::from(drain.effective_max_active_leaf_runs()),
            if drain.limit.is_some() {
                "run_control"
            } else {
                "run_input"
            },
        ),
        (None, None) => (DEFAULT_MAX_ACTIVE_LEAF_RUNS, "default"),
    };

    // Validated here, before any snapshot work, so a typo reads the same way
    // it would on `orbit run auto --allow-crew`.
    let pool_input = active_drain
        .as_ref()
        .map_or_else(|| json!({}), |drain| json!({"run_id": drain.run_id}));
    let pools = runtime.auto_crew_pools_for_input(&pool_input)?;
    let allowlist = runtime.crew_allowlist(allowed_crews)?;
    let snapshot = backlog_snapshot(
        runtime,
        "explain_workspace_auto_readiness",
        allowlist.as_ref(),
        &pools,
    )
    .map_err(|error| OrbitError::Execution(format!("read readiness snapshot: {error}")))?;
    let live_leaves = read_live_leaf_runs(runtime)?;
    let active_epic = read_active_epic_run(runtime)?;
    let claimed_by_task =
        live_leaves
            .iter()
            .fold(BTreeMap::<String, Vec<String>>::new(), |mut claims, run| {
                for task_id in &run.task_ids {
                    claims
                        .entry(task_id.clone())
                        .or_default()
                        .push(run.run_id.clone());
                }
                claims
            });
    let admissions_stopped = active_drain
        .as_ref()
        .is_some_and(|drain| drain.admissions_stopped());
    // [ORB-11332] A grant-bound drain admits only its finite scope, and only
    // while the grant admits at all; report that the same way the drain sees it.
    let operation = active_drain
        .as_ref()
        .and_then(|drain| drain.operation.as_ref())
        .map(|admission| {
            admission_state(runtime, admission).map(|(grant, state)| ReadinessGrant {
                grant,
                state,
                leaf_ceiling: admission.limits.leaf_ceiling,
            })
        })
        .transpose()?;
    let grant_closed = operation
        .as_ref()
        .is_some_and(|operation| !operation.state.admits());
    let max_active_leaf_runs = operation
        .as_ref()
        .map_or(max_active_leaf_runs, |operation| {
            max_active_leaf_runs.min(u64::from(operation.leaf_ceiling))
        });
    let free_slots = if admissions_stopped || grant_closed {
        0
    } else {
        usize::try_from(max_active_leaf_runs)
            .unwrap_or(usize::MAX)
            .saturating_sub(live_leaves.len())
    };
    let pending = snapshot
        .admissible_leaves
        .iter()
        .filter(|task| !claimed_by_task.contains_key(&task.id))
        .filter(|task| {
            operation
                .as_ref()
                .is_none_or(|operation| operation.grant.covers(&task.id))
        })
        .map(|task| task.id.clone())
        .collect::<Vec<_>>();
    // [ORB-11973] Use the classifier's identical ordered prefix and admission
    // routine, so readiness explains the wave the drain would actually admit
    // rather than a second guess at it.
    let candidate_pool_size = usize::try_from(DEFAULT_CANDIDATE_POOL).unwrap_or(usize::MAX);
    let examined = &pending[..pending.len().min(candidate_pool_size)];
    let workspace_root = runtime.paths().repo_root.as_path();
    let claimed = claimed_by_task.keys().cloned().collect::<BTreeSet<_>>();
    let holders = AdmissionHolders::new(
        &snapshot.lock_holders,
        &claimed,
        &snapshot.task_lookup,
        workspace_root,
    );
    let selection = select_admissions(
        examined,
        &snapshot.task_lookup,
        workspace_root,
        &holders,
        free_slots,
    );
    let candidate_pool_truncated =
        pending.len() > examined.len() && selection.selected.len() < free_slots;
    let admitted = selection.selected.iter().cloned().collect::<BTreeSet<_>>();
    let occupancy = read_leaf_occupancy(
        runtime,
        &live_leaves
            .iter()
            .map(|run| (run.run_id.clone(), run.task_ids.clone()))
            .collect::<Vec<_>>(),
        &snapshot.lock_holders,
    )?;
    let excluded_by_id = snapshot
        .excluded
        .iter()
        .map(|excluded| (excluded.id.as_str(), excluded))
        .collect::<BTreeMap<_, _>>();
    let next_epic = if active_epic.is_none() && !admissions_stopped {
        next_admissible_epic_root(
            runtime,
            "explain_workspace_auto_readiness",
            allowlist.as_ref(),
            &pools,
        )
        .map_err(|error| OrbitError::Execution(format!("read epic readiness: {error}")))?
    } else {
        None
    };

    let selected_ids = if task_ids.is_empty() {
        let mut ids = snapshot
            .task_lookup
            .values()
            .filter(|task| task.status == TaskStatus::Backlog)
            .cloned()
            .collect::<Vec<_>>();
        sort_tasks_for_automatic_dispatch(&mut ids);
        ids.into_iter().take(limit).map(|task| task.id).collect()
    } else {
        let mut ids = task_ids.to_vec();
        ids.sort();
        ids.dedup();
        if let Some(missing) = ids
            .iter()
            .find(|id| !snapshot.task_lookup.contains_key(*id))
        {
            return Err(OrbitError::InvalidInput(format!(
                "task `{missing}` was not found in this workspace"
            )));
        }
        if ids.len() > limit {
            return Err(OrbitError::InvalidInput(format!(
                "readiness selection contains {} tasks; limit is {limit}",
                ids.len()
            )));
        }
        ids
    };

    let reference_index = TaskReferenceIndex::from_status_index(&snapshot.status_by_id);
    let tasks = selected_ids
        .iter()
        .filter_map(|id| snapshot.task_lookup.get(id))
        .map(|task| {
            let mut entry = json!({
                "task_id": task.id,
                "status": task.status.to_string(),
                "eligible": false,
                "reason": "not_backlog",
            });
            let Some(object) = entry.as_object_mut() else {
                unreachable!("readiness task entry is an object");
            };
            if task.status != TaskStatus::Backlog {
                return Value::Object(object.clone());
            }
            let unmet = unmet_task_dependencies_with_index(
                task,
                &snapshot.status_by_id,
                &reference_index,
            );
            if !unmet.is_empty() {
                object.insert("reason".to_string(), Value::String("unmet_dependency".to_string()));
                object.insert(
                    "dependencies".to_string(),
                    json!(unmet
                        .into_iter()
                        .map(|dependency| json!({ "task_id": dependency.id, "status": dependency.status }))
                        .collect::<Vec<_>>()),
                );
                return Value::Object(object.clone());
            }
            if let Some(excluded) = excluded_by_id.get(task.id.as_str()) {
                match excluded.reason {
                    BacklogTaskExclusionReason::UnassessedComplexity => {
                        object.insert(
                            "reason".to_string(),
                            Value::String("task_pilot_preparation_required".to_string()),
                        );
                    }
                    BacklogTaskExclusionReason::CrewNotAllowed => {
                        object.insert("reason".to_string(), Value::String("crew_not_allowed".to_string()));
                        object.insert("crew".to_string(), json!(excluded.crew));
                        object.insert("allowed_crews".to_string(), json!(allowlist.as_ref().map(CrewAllowlist::names)));
                    }
                    BacklogTaskExclusionReason::EpicChild => {
                        object.insert("reason".to_string(), Value::String("epic_managed".to_string()));
                    }
                    BacklogTaskExclusionReason::EpicRoot => {
                        if admissions_stopped {
                            object.insert(
                                "reason".to_string(),
                                Value::String("admissions_stopped".to_string()),
                            );
                        } else if active_epic.is_some() {
                            object.insert("reason".to_string(), Value::String("epic_run_active".to_string()));
                            object.insert("epic_run_id".to_string(), json!(active_epic.as_ref().map(|run| &run.run_id)));
                        } else if next_epic.as_deref() == Some(task.id.as_str()) {
                            object.insert("eligible".to_string(), Value::Bool(true));
                            object.insert("reason".to_string(), Value::String("ready_as_epic".to_string()));
                        } else {
                            object.insert("reason".to_string(), Value::String("queued_behind_epic".to_string()));
                            object.insert("next_epic_task_id".to_string(), json!(next_epic));
                        }
                    }
                    BacklogTaskExclusionReason::ContextLockConflict
                    | BacklogTaskExclusionReason::GroupMemberConflict => {
                        object.insert(
                            "reason".to_string(),
                            Value::String(match excluded.reason {
                                BacklogTaskExclusionReason::ContextLockConflict => "context_lock_conflict",
                                BacklogTaskExclusionReason::GroupMemberConflict => "group_member_conflict",
                                _ => unreachable!("lock exclusions are handled above"),
                            }.to_string()),
                        );
                        object.insert(
                            "conflicts".to_string(),
                            json!(excluded.conflicts.iter().map(|conflict| json!({
                                "requested_file": conflict.requested_file,
                                "locking_task_id": conflict.locking_task_id,
                            })).collect::<Vec<_>>()),
                        );
                    }
                }
                return Value::Object(object.clone());
            }
            if let Some(run_ids) = claimed_by_task.get(&task.id) {
                object.insert("reason".to_string(), Value::String("claimed_by_live_child".to_string()));
                object.insert("run_ids".to_string(), json!(run_ids));
            } else if admissions_stopped {
                object.insert(
                    "reason".to_string(),
                    Value::String("admissions_stopped".to_string()),
                );
            } else if let Some(closed) = operation.as_ref().filter(|operation| !operation.state.admits()) {
                object.insert("reason".to_string(), Value::String(closed.state.reason().to_string()));
                object.insert("grant_id".to_string(), json!(closed.grant.id));
            } else if let Some(outside) = operation.as_ref().filter(|operation| !operation.grant.covers(&task.id)) {
                object.insert("reason".to_string(), Value::String("outside_grant_scope".to_string()));
                object.insert("grant_id".to_string(), json!(outside.grant.id));
            } else if admitted.contains(&task.id) {
                object.insert("eligible".to_string(), Value::Bool(true));
                object.insert("reason".to_string(), Value::String("ready".to_string()));
            } else if !examined.contains(&task.id) {
                object.insert(
                    "reason".to_string(),
                    Value::String("outside_candidate_pool".to_string()),
                );
            } else if let Some(deferred) = selection.deferred_for(&task.id) {
                // [ORB-11973] A slot was free and this task did not take it,
                // which is a different problem from having no slot at all.
                object.insert("reason".to_string(), Value::String("conflict_deferred".to_string()));
                object.insert("blocking_task_ids".to_string(), json!(deferred.blocking_task_ids()));
                object.insert("conflicts".to_string(), deferred.to_json()["conflicts"].clone());
            } else {
                object.insert("reason".to_string(), Value::String("capacity_saturated".to_string()));
                object.insert("active_run_ids".to_string(), json!(live_leaves.iter().map(|run| &run.run_id).collect::<Vec<_>>()));
            }
            Value::Object(object.clone())
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "snapshot": {
            "read_only": true,
            "limitations": "Snapshot only: eligibility can change immediately and does not guarantee a task will start. No stale-run reconciliation, reservation, task mutation, or run submission was performed.",
        },
        "capacity": {
            "max_active_leaf_runs": max_active_leaf_runs,
            "active_leaf_runs": live_leaves.len(),
            "free_slots": free_slots,
            // [ORB-11973] The same occupancy, broken down by what each slot is
            // doing. A drain with every slot parked in `task_gate_pipeline` is
            // indistinguishable from a busy one by the counts above alone.
            "occupancy": occupancy_json(&occupancy, free_slots),
            "deferred_conflicts": selection.deferred_json(),
            "candidate_pool_size": examined.len(),
            "candidate_pool_truncated": candidate_pool_truncated,
            "limit_source": limit_source,
            "drain_run_id": active_drain.as_ref().map(|drain| &drain.run_id),
            "worker_limit": active_drain.as_ref().and_then(|drain| drain.limit.clone()),
            "admissions_stopped": admissions_stopped,
            "admissions_stop": active_drain
                .as_ref()
                .and_then(|drain| drain.stop.clone()),
            "operation": operation.as_ref().map(|operation| json!({
                "grant_id": operation.grant.id,
                "admission": operation.state.reason(),
                "scope": operation.grant.task_ids,
                "expires_at": operation.grant.expires_at.to_rfc3339(),
                "leaf_ceiling": operation.leaf_ceiling,
            })),
        },
        "tasks": tasks,
    }))
}

impl OrbitRuntime {
    /// Read-only projection of the current auto-drain admission snapshot.
    pub fn workspace_auto_readiness(
        &self,
        task_ids: &[String],
        max_active_leaf_runs: Option<u32>,
        limit: usize,
        allowed_crews: &[String],
    ) -> Result<Value, OrbitError> {
        explain_workspace_auto_readiness(self, task_ids, max_active_leaf_runs, limit, allowed_crews)
    }
}

/// The grant a live grant-bound drain is admitting under, as readiness sees it.
struct ReadinessGrant {
    grant: orbit_types::workflow::OperationGrant,
    state: orbit_types::workflow::GrantAdmission,
    /// The ceiling captured at the drain's admission.
    leaf_ceiling: u32,
}

/// The live worker ceiling an operator has set on *this* drain [ORB-11253].
///
/// The engine injects the executing run's id into every activity input, which
/// is the only handle the admission path has on the coordinator whose control
/// it must read. Absent or unreadable state degrades to the submitted ceiling:
/// a drain that cannot read its own control must keep admitting at the value it
/// was started with rather than stalling.
fn live_worker_limit(runtime: &OrbitRuntime, input: &Value) -> Option<DrainWorkerLimit> {
    let run_id = input
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    read_drain_worker_limit(runtime, run_id)
}

fn live_admissions_stop(runtime: &OrbitRuntime, input: &Value) -> Option<DrainAdmissionsStop> {
    let run_id = input
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    runtime
        .read_run_state(run_id)
        .ok()
        .flatten()
        .and_then(|state| state.drain_admissions_stop)
}

/// The live drain run, with both the ceiling it was submitted with and the one
/// an operator has since set.
struct ActiveDrain {
    run_id: String,
    submitted: u32,
    limit: Option<DrainWorkerLimit>,
    stop: Option<DrainAdmissionsStop>,
    /// The operation-mode snapshot the drain was admitted under [ORB-11332].
    operation: Option<OperationAdmission>,
}

impl ActiveDrain {
    fn admissions_stopped(&self) -> bool {
        self.stop.is_some()
    }

    fn effective_max_active_leaf_runs(&self) -> u32 {
        self.limit
            .as_ref()
            .map_or(self.submitted, |limit| limit.max_active_leaf_runs)
    }
}

/// The workspace's live drain, if one is running. `workspace_auto_pipeline`
/// declares `max_active_runs: 1`, so there is at most one to report.
fn active_drain(runtime: &OrbitRuntime) -> Result<Option<ActiveDrain>, OrbitError> {
    let Some(run) = runtime
        .stores()
        .jobs()
        .list_pending_or_running_job_runs(DRAIN_JOB_NAME)?
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    let submitted = run
        .input
        .as_ref()
        .and_then(|input| input.get("max_active_leaf_runs"))
        .and_then(json_u32)
        .unwrap_or(DEFAULT_MAX_ACTIVE_LEAF_RUNS as u32);
    let state = runtime
        .stores()
        .jobs()
        .read_run_state(&run.run_id)
        .ok()
        .flatten();
    let limit = state
        .as_ref()
        .and_then(|state| state.drain_worker_limit.clone());
    let stop = state.and_then(|state| state.drain_admissions_stop);
    let operation = run
        .input
        .as_ref()
        .map(OperationAdmission::from_run_input)
        .transpose()
        .map_err(OrbitError::InvalidInput)?
        .flatten();
    Ok(Some(ActiveDrain {
        run_id: run.run_id,
        submitted,
        limit,
        stop,
        operation,
    }))
}

fn read_drain_worker_limit(runtime: &OrbitRuntime, run_id: &str) -> Option<DrainWorkerLimit> {
    runtime
        .stores()
        .jobs()
        .read_run_state(run_id)
        .ok()
        .flatten()
        .and_then(|state| state.drain_worker_limit)
}

/// A durable run-input ceiling. The generic job surface persists every
/// `--input key=value` as a JSON string, so `"7"` must parse the same as `7`
/// ([ORB-11273]). Matches `job_input_u32` on the worker-limit write path.
fn json_u32(value: &Value) -> Option<u32> {
    match value {
        Value::Number(number) => number.as_u64().and_then(|value| u32::try_from(value).ok()),
        Value::String(text) => text.trim().parse::<u32>().ok(),
        _ => None,
    }
}

/// A numeric loop input, tolerating the string a template renders. A step's
/// `default_input` value goes through the template engine, so `5` arrives as
/// `"5"`; an input the caller omitted renders as an empty string rather than
/// JSON `null`, which is the default case.
fn templated_u64(
    action: &str,
    input: &Value,
    name: &str,
    default: u64,
) -> Result<u64, DispatchError> {
    let Some(raw) = input.get(name) else {
        return Ok(default);
    };
    match raw {
        Value::Null => Ok(default),
        Value::Number(number) => number
            .as_u64()
            .ok_or_else(|| action_failed(action, format!("`{name}` must be a whole number"))),
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                return Ok(default);
            }
            text.parse::<u64>().map_err(|err| {
                action_failed(action, format!("`{name}` '{text}' is not a number: {err}"))
            })
        }
        other => Err(action_failed(
            action,
            format!("`{name}` must be a number, got {other}"),
        )),
    }
}

/// How many backlog candidates one iteration may expand lock footprints for.
///
/// Mirrors `list_backlog_tasks`'s `max_tasks` bound — same default, same
/// ceiling — because the two describe the same scan. It goes through
/// [`templated_u64`] rather than `as_u64` because the drain job templates the
/// value, which renders `50` as `"50"`.
fn candidate_pool_limit(action: &str, input: &Value) -> Result<usize, DispatchError> {
    let requested = templated_u64(action, input, "max_tasks", DEFAULT_CANDIDATE_POOL)?;
    Ok(usize::try_from(requested.clamp(1, MAX_CANDIDATE_POOL)).unwrap_or(usize::MAX))
}

/// A live `task_auto_pipeline` run and the tasks it is carrying.
struct LiveLeafRun {
    run_id: String,
    task_ids: Vec<String>,
}

fn live_leaf_runs(runtime: &OrbitRuntime, action: &str) -> Result<Vec<LiveLeafRun>, DispatchError> {
    // Reconcile first, for the same reason the epic gate does: one orphaned
    // `running` row — a worker killed by a reboot or an OOM — would occupy a
    // slot forever. Unlike the epic gate, that failure degrades rather than
    // stops: the drain keeps shipping at a quietly lower parallelism, which is
    // exactly the kind of thing nobody notices.
    runtime
        .reconcile_stale_job_runs(Some(LEAF_JOB_NAME))
        .map_err(|err| {
            action_failed(
                action,
                format!("reconcile stale {LEAF_JOB_NAME} runs: {err}"),
            )
        })?;
    read_live_leaf_runs(runtime)
        .map_err(|error| action_failed(action, format!("list live {LEAF_JOB_NAME} runs: {error}")))
}

fn read_live_leaf_runs(runtime: &OrbitRuntime) -> Result<Vec<LiveLeafRun>, OrbitError> {
    let runs = runtime
        .stores()
        .jobs()
        .list_pending_or_running_job_runs(LEAF_JOB_NAME)?;
    Ok(runs
        .into_iter()
        .map(|run| LiveLeafRun {
            run_id: run.run_id,
            task_ids: run
                .input
                .as_ref()
                .and_then(|input| input.get("task_ids"))
                .and_then(Value::as_array)
                .map(|ids| {
                    ids.iter()
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(ToOwned::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect())
}

/// A live `epic_pipeline` run, if one is already supervising a root.
struct ActiveEpicRun {
    run_id: String,
    task_id: Option<String>,
}

fn active_epic_run(
    runtime: &OrbitRuntime,
    action: &str,
) -> Result<Option<ActiveEpicRun>, DispatchError> {
    // Reconcile first, exactly as the submit path does before it counts
    // active runs. Without this, one orphaned `running` row — a worker killed
    // by a reboot or an OOM — would read as a live epic forever and silently
    // stop every epic dispatch in the workspace. That failure would be
    // invisible: the drain keeps succeeding, it just never starts an epic.
    runtime
        .reconcile_stale_job_runs(Some(EPIC_JOB_NAME))
        .map_err(|err| DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: format!("reconcile stale {EPIC_JOB_NAME} runs: {err}"),
        })?;
    read_active_epic_run(runtime).map_err(|error| DispatchError::DeterministicActionFailed {
        action: action.to_string(),
        message: format!("list live {EPIC_JOB_NAME} runs: {error}"),
    })
}

fn read_active_epic_run(runtime: &OrbitRuntime) -> Result<Option<ActiveEpicRun>, OrbitError> {
    let runs = runtime
        .stores()
        .jobs()
        .list_pending_or_running_job_runs(EPIC_JOB_NAME)?;
    Ok(runs.into_iter().next().map(|run| ActiveEpicRun {
        task_id: run
            .input
            .as_ref()
            .and_then(|input| input.get("epic_task_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        run_id: run.run_id,
    }))
}

/// The highest-priority `backlog` epic root whose dependencies are satisfied
/// and whose effective crew this run's window permits [ORB-11242].
fn next_admissible_epic_root(
    runtime: &OrbitRuntime,
    action: &str,
    allowlist: Option<&CrewAllowlist>,
    pools: &CapturedCrewPools,
) -> Result<Option<String>, DispatchError> {
    let all_tasks = runtime.stores().tasks().list_tasks().map_err(|err| {
        DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: format!("list tasks: {err}"),
        }
    })?;
    let task_lookup: BTreeMap<String, Task> = all_tasks
        .iter()
        .cloned()
        .map(|task| (task.id.clone(), task))
        .collect();
    let status_by_id =
        runtime
            .task_status_index()
            .map_err(|err| DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: format!("load global task status projection: {err}"),
            })?;
    let reference_index = TaskReferenceIndex::from_status_index(&status_by_id);

    let mut backlog_epics = all_tasks
        .into_iter()
        .filter(|task| {
            task.status == TaskStatus::Backlog
                && epic_family_membership(task, &task_lookup) == Some(EpicFamilyMembership::Root)
                && task_dependencies_ready_with_index(task, &status_by_id, &reference_index)
                && allowlist.is_none_or(|allowlist| {
                    runtime
                        .auto_task_crew_eligibility(task, pools, allowlist)
                        .is_ok()
                })
        })
        .collect::<Vec<_>>();
    sort_tasks_for_automatic_dispatch(&mut backlog_epics);
    Ok(backlog_epics.into_iter().next().map(|epic| epic.id))
}

/// Open or re-read a drain window [ORB-10819].
///
/// Two call shapes, one action. Called with `for_seconds` and no `deadline` it
/// *stamps*: the deadline is `now + for_seconds`, returned as RFC3339. Called
/// with that `deadline` echoed back it *answers*: whether the window has since
/// expired. The stamp therefore lives in the stamping step's own pipeline
/// output, which the run state already persists — no new durable artifact, and
/// re-reading the window is a pure function of a value the run carries.
///
/// A zero or absent window is expired on the first answer. `break_when` is
/// evaluated after a loop body runs, so that yields exactly one iteration:
/// the one-tick behavior every pre-window caller of `orbit run auto` has.
///
/// The deadline gates *starting* work. Nothing here cancels anything, which is
/// what makes "the window does not affect tasks already in progress" true by
/// construction: an in-flight child is held by `invoke_and_wait`, not by the
/// window.
pub(super) fn drain_window(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let now = Utc::now();
    let deadline = match optional_deadline(action, input)? {
        Some(deadline) => deadline,
        None => {
            let for_seconds = window_seconds(action, input)?;
            now.checked_add_signed(seconds_to_delta(action, for_seconds)?)
                .ok_or_else(|| {
                    action_failed(
                        action,
                        format!("`for_seconds` {for_seconds} overflows the drain deadline"),
                    )
                })?
        }
    };

    let remaining_seconds = (deadline - now).num_milliseconds() as f64 / 1000.0;
    let window_expired = remaining_seconds <= 0.0;
    let admissions_stopped = live_admissions_stop(runtime, input).is_some();
    // [ORB-11332] A grant-bound drain also closes when its grant stops
    // admitting: ordinary expiry and stop end new admissions, revocation does
    // too; admitted children are untouched either way.
    let grant_closed = match live_admission(runtime, input) {
        Some(admission) => {
            let (_, state) = admission_state(runtime, &admission).map_err(|error| {
                action_failed(action, format!("recheck operation grant: {error}"))
            })?;
            (!state.admits()).then_some(state.reason())
        }
        None => None,
    };
    let closed = admissions_stopped || grant_closed.is_some();
    let expired = window_expired || closed;
    Ok(json!({
        "deadline": deadline.to_rfc3339_opts(SecondsFormat::Secs, true),
        "expired": expired,
        "remaining_seconds": if closed { 0.0 } else { remaining_seconds.max(0.0) },
        "expired_reason": if admissions_stopped {
            "admissions_stopped"
        } else if let Some(reason) = grant_closed {
            reason
        } else if window_expired {
            "window"
        } else {
            "open"
        },
    }))
}

fn optional_deadline(action: &str, input: &Value) -> Result<Option<DateTime<Utc>>, DispatchError> {
    let Some(raw) = input
        .get("deadline")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    DateTime::parse_from_rfc3339(raw)
        .map(|parsed| Some(parsed.with_timezone(&Utc)))
        .map_err(|err| action_failed(action, format!("`deadline` '{raw}' is not RFC3339: {err}")))
}

/// Read `for_seconds`, tolerating the string a template renders when the
/// caller supplied no window at all (`"{{ input.for_seconds }}"` over an
/// absent key resolves to an empty string, not to JSON `null`).
fn window_seconds(action: &str, input: &Value) -> Result<f64, DispatchError> {
    let Some(raw) = input.get("for_seconds") else {
        return Ok(0.0);
    };
    let seconds = match raw {
        Value::Null => 0.0,
        Value::Number(number) => number
            .as_f64()
            .ok_or_else(|| action_failed(action, "`for_seconds` is not a finite number".into()))?,
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                0.0
            } else {
                text.parse::<f64>().map_err(|err| {
                    action_failed(
                        action,
                        format!("`for_seconds` '{text}' is not a number: {err}"),
                    )
                })?
            }
        }
        other => {
            return Err(action_failed(
                action,
                format!("`for_seconds` must be a number, got {other}"),
            ));
        }
    };
    if !seconds.is_finite() || !(0.0..=MAX_DRAIN_WINDOW_SECONDS).contains(&seconds) {
        return Err(action_failed(
            action,
            format!("`for_seconds` must be between 0 and {MAX_DRAIN_WINDOW_SECONDS}"),
        ));
    }
    Ok(seconds)
}

fn seconds_to_delta(action: &str, seconds: f64) -> Result<TimeDelta, DispatchError> {
    TimeDelta::try_milliseconds((seconds * 1000.0).round() as i64)
        .ok_or_else(|| action_failed(action, format!("`for_seconds` {seconds} is out of range")))
}

fn action_failed(action: &str, message: String) -> DispatchError {
    DispatchError::DeterministicActionFailed {
        action: action.to_string(),
        message,
    }
}

pub(super) fn list_epic_descendants(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let epic_task_id = input
        .get("epic_task_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: "missing `epic_task_id`".to_string(),
        })?;
    let all_tasks = runtime.stores().tasks().list_tasks().map_err(|error| {
        DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: format!("list tasks: {error}"),
        }
    })?;
    let task_lookup = all_tasks
        .iter()
        .cloned()
        .map(|task| (task.id.clone(), task))
        .collect::<BTreeMap<_, _>>();
    let epic =
        task_lookup
            .get(epic_task_id)
            .ok_or_else(|| DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: format!("epic task `{epic_task_id}` was not found"),
            })?;
    if !epic.tags.iter().any(|tag| tag == "epic") {
        return Err(DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: format!("task `{epic_task_id}` is not tagged `epic`"),
        });
    }

    let mut remaining = all_tasks
        .into_iter()
        .filter(|task| {
            is_descendant_of(task, epic_task_id, &task_lookup)
                && !matches!(
                    task.status,
                    TaskStatus::Done | TaskStatus::Rejected | TaskStatus::Archived
                )
        })
        .map(|task| (task.id.clone(), task))
        .collect::<BTreeMap<_, _>>();
    let mut ordered = Vec::with_capacity(remaining.len());

    while !remaining.is_empty() {
        let remaining_ids = remaining.keys().cloned().collect::<BTreeSet<_>>();
        let mut ready = remaining
            .values()
            .filter(|task| {
                task.dependencies()
                    .iter()
                    .all(|dependency_id| !remaining_ids.contains(dependency_id))
            })
            .cloned()
            .collect::<Vec<_>>();
        if ready.is_empty() {
            return Err(DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: format!(
                    "epic `{epic_task_id}` has a dependency cycle among unfinished descendants: {}",
                    remaining_ids.into_iter().collect::<Vec<_>>().join(", ")
                ),
            });
        }
        sort_tasks_for_automatic_dispatch(&mut ready);
        for task in ready {
            remaining.remove(&task.id);
            ordered.push(task.id);
        }
    }

    let empty = ordered.is_empty();
    let fail_if_nonempty = input
        .get("fail_if_nonempty")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if fail_if_nonempty && !empty {
        return Err(DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: format!(
                "epic descendants remain after drain: tasks=[{}]",
                ordered.join(", ")
            ),
        });
    }

    Ok(json!({
        "epic_task_id": epic_task_id,
        "task_count": ordered.len(),
        "task_ids": ordered,
        "empty": empty,
    }))
}

fn is_descendant_of(task: &Task, ancestor_id: &str, task_lookup: &BTreeMap<String, Task>) -> bool {
    let mut visited = BTreeSet::from([task.id.clone()]);
    let mut next_parent_id = task.parent_id();
    for _ in 0..32 {
        let Some(parent_id) = next_parent_id else {
            return false;
        };
        if parent_id == ancestor_id {
            return true;
        }
        if !visited.insert(parent_id.to_string()) {
            return false;
        }
        let Some(parent) = task_lookup.get(parent_id) else {
            return false;
        };
        next_parent_id = parent.parent_id();
    }
    false
}
