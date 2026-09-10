//! What the drain's occupied leaf slots are actually doing [ORB-11973].
//!
//! A saturated drain used to report one number and a flat list of wrapper run
//! ids, which cannot distinguish the two situations an operator has to tell
//! apart: ten slots doing ten pieces of work, and ten slots where half are
//! queued behind each other's locks inside `task_gate_pipeline`. The occupancy
//! itself is identical; only the phase of each wrapper's descendant separates
//! them.
//!
//! Phase is read from durable run state and nothing else. A wrapper's
//! `child_dispatches` name its gate run, the gate's name the implementation
//! run, and the gate's `waiting_on_locks` is the record `reserve_locks` writes
//! when it parks. Where that evidence is missing the phase is reported as
//! `unknown` with the reason, because a guessed phase is worse than an
//! acknowledged gap in a diagnostic an operator uses to decide whether to
//! intervene.
//!
//! This module reads; it never writes. Occupancy stays wrapper-based, so a
//! wrapper with several descendants still consumes exactly one slot.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_common::fs::path::workspace_relative_paths_overlap;
use orbit_types::workflow::{ChildDispatch, PipelineState};
use serde_json::{Value, json};

use crate::OrbitRuntime;

/// The wrapper job that occupies a leaf slot.
const LEAF_JOB_NAME: &str = "task_auto_pipeline";

/// The wrapper's own child: waits for the bundle's lock window, reserves it,
/// then dispatches the implementation run.
const GATE_JOB_NAME: &str = "task_gate_pipeline";

/// How far the dispatch walk follows lineage. The wrapper -> gate ->
/// implementation chain is two hops; the bound is a loop guard for a
/// malformed or cyclic dispatch record, not a tuning knob.
const MAX_DISPATCH_DEPTH: usize = 2;

/// What a live leaf wrapper is doing, as far as durable state can say.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum LeafRunPhase {
    /// The gate has parked on `reserve_locks`: it holds a slot but no lock.
    LockWaiting,
    /// An implementation run is live under the gate.
    Implementing,
    /// The gate recorded an implementation dispatch that has since gone
    /// terminal, so the slot is doing post-implementation work — the base
    /// sync, reservation release, and gate teardown. This is the sync/other
    /// bucket: it is neither waiting for a lock nor implementing.
    PostImplementation,
    /// No durable evidence resolved a phase. Always accompanied by the reason.
    Unknown,
}

impl LeafRunPhase {
    fn as_str(self) -> &'static str {
        match self {
            LeafRunPhase::LockWaiting => "lock_waiting",
            LeafRunPhase::Implementing => "implementing",
            LeafRunPhase::PostImplementation => "post_implementation",
            LeafRunPhase::Unknown => "unknown",
        }
    }

    /// How advanced a phase is, for a wrapper carrying more than one gate. The
    /// drain dispatches one task per wrapper, so this is a tie-break for
    /// hand-submitted bundles rather than the normal path.
    fn rank(self) -> u8 {
        match self {
            LeafRunPhase::Implementing => 3,
            LeafRunPhase::PostImplementation => 2,
            LeafRunPhase::LockWaiting => 1,
            LeafRunPhase::Unknown => 0,
        }
    }
}

/// One occupied slot, its lineage, and the evidence behind its phase.
pub(super) struct LeafRunOccupancy {
    pub(super) run_id: String,
    pub(super) task_ids: Vec<String>,
    pub(super) phase: LeafRunPhase,
    /// Every descendant run the dispatch records name, deduplicated.
    pub(super) descendant_run_ids: Vec<String>,
    /// Selectors the gate is parked on, with the holders they resolve to.
    pub(super) lock_wait: Option<LockWait>,
    /// Why the phase is `unknown`, and only then.
    pub(super) unknown_reason: Option<&'static str>,
}

/// The gate's parked-lock record, resolved against current holders.
pub(super) struct LockWait {
    pub(super) gate_run_id: String,
    pub(super) selectors: Vec<String>,
    /// Selectors that map to a task currently holding a lock.
    pub(super) blockers: Vec<LockWaitBlocker>,
    /// Selectors no current holder explains. The gate recorded a conflict, so
    /// the wait is real; the holder has since moved on, or holds through a
    /// reservation rather than a task status. Reported rather than dropped so
    /// the missing evidence is visible instead of looking like no conflict.
    pub(super) unresolved_selectors: Vec<String>,
}

pub(super) struct LockWaitBlocker {
    pub(super) selector: String,
    pub(super) holder_task_ids: Vec<String>,
}

impl LeafRunOccupancy {
    fn to_json(&self) -> Value {
        let mut entry = json!({
            "run_id": self.run_id,
            "task_ids": self.task_ids,
            "phase": self.phase.as_str(),
            "descendant_run_ids": self.descendant_run_ids,
        });
        let Some(object) = entry.as_object_mut() else {
            return entry;
        };
        if let Some(reason) = self.unknown_reason {
            object.insert("unknown_reason".to_string(), json!(reason));
        }
        if let Some(lock_wait) = &self.lock_wait {
            object.insert(
                "lock_wait".to_string(),
                json!({
                    "gate_run_id": lock_wait.gate_run_id,
                    "selectors": lock_wait.selectors,
                    "blockers": lock_wait
                        .blockers
                        .iter()
                        .map(|blocker| json!({
                            "selector": blocker.selector,
                            "holder_task_ids": blocker.holder_task_ids,
                        }))
                        .collect::<Vec<_>>(),
                    "unresolved_selectors": lock_wait.unresolved_selectors,
                }),
            );
        }
        entry
    }
}

/// Project the occupied slots into the readiness payload's `occupancy` block.
///
/// `free_slots` is passed through rather than recomputed so the block cannot
/// disagree with the capacity figures beside it.
pub(super) fn occupancy_json(runs: &[LeafRunOccupancy], free_slots: usize) -> Value {
    let mut phases: BTreeMap<&'static str, usize> = BTreeMap::from([
        ("lock_waiting", 0),
        ("implementing", 0),
        ("post_implementation", 0),
        ("unknown", 0),
    ]);
    for run in runs {
        *phases.entry(run.phase.as_str()).or_default() += 1;
    }
    // A task carried by two wrappers is one task but two occupied slots; the
    // per-run entries keep the slots and this roll-up keeps the tasks.
    let task_ids: BTreeSet<&String> = runs.iter().flat_map(|run| run.task_ids.iter()).collect();

    json!({
        "active_leaf_runs": runs.len(),
        "free_slots": free_slots,
        "phases": phases,
        "task_ids": task_ids,
        "runs": runs.iter().map(LeafRunOccupancy::to_json).collect::<Vec<_>>(),
    })
}

/// Resolve the phase of every live leaf wrapper from durable run state.
///
/// `lock_holders` is the readiness snapshot's selector -> holder map, reused so
/// a blocker named here is the same task the exclusion reasons name.
pub(super) fn read_leaf_occupancy(
    runtime: &OrbitRuntime,
    leaf_runs: &[(String, Vec<String>)],
    lock_holders: &BTreeMap<String, Vec<String>>,
) -> Result<Vec<LeafRunOccupancy>, OrbitError> {
    let lineage = read_dispatch_lineage(
        runtime,
        &leaf_runs
            .iter()
            .map(|(run_id, _)| run_id.clone())
            .collect::<Vec<_>>(),
    )?;

    Ok(leaf_runs
        .iter()
        .map(|(run_id, task_ids)| {
            let resolved = resolve_phase(run_id, &lineage, lock_holders);
            LeafRunOccupancy {
                run_id: run_id.clone(),
                task_ids: task_ids.clone(),
                phase: resolved.phase,
                descendant_run_ids: descendants_of(run_id, &lineage),
                lock_wait: resolved.lock_wait,
                unknown_reason: resolved.unknown_reason,
            }
        })
        .collect())
}

/// Every run state reachable from the wrappers within [`MAX_DISPATCH_DEPTH`],
/// read one level at a time so each level costs one batched store call.
fn read_dispatch_lineage(
    runtime: &OrbitRuntime,
    wrapper_run_ids: &[String],
) -> Result<BTreeMap<String, PipelineState>, OrbitError> {
    let mut lineage: BTreeMap<String, PipelineState> = BTreeMap::new();
    let mut frontier = wrapper_run_ids.to_vec();

    for _ in 0..=MAX_DISPATCH_DEPTH {
        frontier.retain(|run_id| !lineage.contains_key(run_id));
        if frontier.is_empty() {
            break;
        }
        let states = runtime.read_run_states(&frontier)?;
        let mut next = BTreeSet::new();
        for (run_id, state) in states {
            let Some(state) = state else {
                continue;
            };
            for dispatch in &state.child_dispatches {
                next.insert(dispatch.child_run_id.clone());
            }
            lineage.insert(run_id, state);
        }
        frontier = next.into_iter().collect();
    }

    Ok(lineage)
}

struct ResolvedPhase {
    phase: LeafRunPhase,
    lock_wait: Option<LockWait>,
    unknown_reason: Option<&'static str>,
}

fn resolve_phase(
    wrapper_run_id: &str,
    lineage: &BTreeMap<String, PipelineState>,
    lock_holders: &BTreeMap<String, Vec<String>>,
) -> ResolvedPhase {
    let Some(wrapper) = lineage.get(wrapper_run_id) else {
        return unknown("no readable pipeline state for this leaf run");
    };
    let gates = wrapper
        .child_dispatches
        .iter()
        .filter(|dispatch| dispatch.job_name == GATE_JOB_NAME)
        .collect::<Vec<_>>();
    if gates.is_empty() {
        return unknown("leaf run has not recorded a gate dispatch yet");
    }

    // Take the most advanced gate: a wrapper carrying a bundle occupies its
    // slot until its last gate finishes, so the furthest one describes what the
    // slot is doing.
    gates
        .into_iter()
        .map(|gate| resolve_gate_phase(gate, lineage, lock_holders))
        .max_by_key(|resolved| resolved.phase.rank())
        .unwrap_or_else(|| unknown("leaf run has not recorded a gate dispatch yet"))
}

fn resolve_gate_phase(
    gate: &ChildDispatch,
    lineage: &BTreeMap<String, PipelineState>,
    lock_holders: &BTreeMap<String, Vec<String>>,
) -> ResolvedPhase {
    let Some(state) = lineage.get(&gate.child_run_id) else {
        return unknown("no readable pipeline state for this gate run");
    };

    let implementation = state
        .child_dispatches
        .iter()
        .filter(|dispatch| is_implementation_job(&dispatch.job_name))
        .collect::<Vec<_>>();
    if implementation
        .iter()
        .any(|dispatch| dispatch.phase.is_open())
    {
        return resolved(LeafRunPhase::Implementing, None);
    }
    if !implementation.is_empty() {
        return resolved(LeafRunPhase::PostImplementation, None);
    }

    let selectors = state.waiting_on_locks.clone().unwrap_or_default();
    if selectors.is_empty() {
        return unknown("gate run recorded neither a lock wait nor a dispatch");
    }
    resolved(
        LeafRunPhase::LockWaiting,
        Some(resolve_lock_wait(
            gate.child_run_id.clone(),
            selectors,
            lock_holders,
        )),
    )
}

/// The gate's child is `task_{mode}_pipeline`, so the implementation job name
/// is not a closed set — anything the gate dispatches that is not another gate
/// or another wrapper is the implementation it was waiting to start.
fn is_implementation_job(job_name: &str) -> bool {
    job_name != GATE_JOB_NAME
        && job_name != LEAF_JOB_NAME
        && job_name.starts_with("task_")
        && job_name.ends_with("_pipeline")
}

fn resolve_lock_wait(
    gate_run_id: String,
    selectors: Vec<String>,
    lock_holders: &BTreeMap<String, Vec<String>>,
) -> LockWait {
    let mut blockers = Vec::new();
    let mut unresolved_selectors = Vec::new();

    for selector in &selectors {
        let holder_task_ids = lock_holders
            .iter()
            .filter(|(held_selector, _)| workspace_relative_paths_overlap(selector, held_selector))
            .flat_map(|(_, holder_task_ids)| holder_task_ids.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if holder_task_ids.is_empty() {
            unresolved_selectors.push(selector.clone());
        } else {
            blockers.push(LockWaitBlocker {
                selector: selector.clone(),
                holder_task_ids,
            });
        }
    }

    LockWait {
        gate_run_id,
        selectors,
        blockers,
        unresolved_selectors,
    }
}

fn descendants_of(wrapper_run_id: &str, lineage: &BTreeMap<String, PipelineState>) -> Vec<String> {
    let mut descendants = BTreeSet::new();
    let mut frontier = vec![wrapper_run_id.to_string()];

    for _ in 0..=MAX_DISPATCH_DEPTH {
        let mut next = Vec::new();
        for run_id in &frontier {
            let Some(state) = lineage.get(run_id) else {
                continue;
            };
            for dispatch in &state.child_dispatches {
                if descendants.insert(dispatch.child_run_id.clone()) {
                    next.push(dispatch.child_run_id.clone());
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }

    descendants.into_iter().collect()
}

fn resolved(phase: LeafRunPhase, lock_wait: Option<LockWait>) -> ResolvedPhase {
    ResolvedPhase {
        phase,
        lock_wait,
        unknown_reason: None,
    }
}

fn unknown(reason: &'static str) -> ResolvedPhase {
    ResolvedPhase {
        phase: LeafRunPhase::Unknown,
        lock_wait: None,
        unknown_reason: Some(reason),
    }
}
