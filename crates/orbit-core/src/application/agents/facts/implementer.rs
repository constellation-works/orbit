//! Which attempts launched no implementer, proven from their run's own
//! records. Such an attempt (a run that only resumed a candidate, recovered
//! or handed off a failure) is not an implementer unit: it counts on no
//! implementer row and charges none.
//!
//! The absence must be provable on this machine, so an attempt is marked
//! only when all of these hold:
//!
//! - a local implementation run matched it (a pull from another machine,
//!   whose run this machine does not hold, is never marked);
//! - that run's audit trail holds both its `run.started` and `run.finished`
//!   events, so the trail is complete (retention prunes the oldest events
//!   first) and the run can launch nothing more;
//! - no `activity.started` event in that trail names an implementing
//!   activity, and the trail's activity events were read in full;
//! - no invocation and no launch record of the run is an implementing one,
//!   and no invocation of the run lacks its activity.
//!
//! An attempt whose records are missing or cut short stays a unit, with
//! whatever effort its records give. The landed attempt is never marked:
//! its credit stays where landing put it.

use std::collections::{BTreeSet, HashMap, HashSet};

use orbit_runtime::cost::IMPLEMENT_INVOCATION_IDS;
use serde_json::Value;

use super::invocations::AgentInvocationFacts;
use super::launches::AgentLaunchFacts;
use super::model::TaskFacts;
use crate::OrbitRuntime;

/// The most `activity.started` events read per run. A run that reaches it
/// may have more, so its trail cannot prove an absence.
const ACTIVITY_EVENTS_PER_RUN: usize = 1024;

/// Mark each attempt whose local run provably launched no implementer. A
/// failed read marks nothing: every attempt stays a unit.
pub(in crate::application::agents) fn mark_implementer_less_attempts(
    runtime: &OrbitRuntime,
    tasks: &mut [TaskFacts],
    invocations: &AgentInvocationFacts,
    launches: &AgentLaunchFacts,
) {
    let candidates: BTreeSet<String> = tasks
        .iter()
        .flat_map(|task| {
            task.attempts
                .iter()
                .enumerate()
                .filter(|(index, attempt)| attempt.local && task.final_attempt != Some(*index))
                .filter_map(|(_, attempt)| attempt.runner.run_id.clone())
        })
        .filter(|run_id| !telemetry_names_an_implementer(invocations, launches, run_id))
        .collect();
    if candidates.is_empty() {
        return;
    }
    let proven = match runs_without_implementing_activity(runtime, &candidates) {
        Ok(proven) => proven,
        Err(error) => {
            tracing::warn!(%error, "agent scorecards: run audit trails unreadable; no attempt dropped");
            return;
        }
    };
    for task in tasks {
        let landed = task.final_attempt;
        for (index, attempt) in task.attempts.iter_mut().enumerate() {
            if attempt.local
                && landed != Some(index)
                && attempt
                    .runner
                    .run_id
                    .as_deref()
                    .is_some_and(|run_id| proven.contains(run_id))
            {
                attempt.implementer = false;
            }
        }
    }
}

fn is_implementing(activity_id: &str) -> bool {
    IMPLEMENT_INVOCATION_IDS.contains(&activity_id)
}

/// Whether the run's invocations or launch records show, or may show, an
/// implementer: an implementing record, or an invocation with no activity.
fn telemetry_names_an_implementer(
    invocations: &AgentInvocationFacts,
    launches: &AgentLaunchFacts,
    run_id: &str,
) -> bool {
    invocations
        .invocations_for_run(run_id)
        .any(|fact| fact.activity_id.as_deref().is_none_or(is_implementing))
        || launches
            .launches_for_run(run_id)
            .any(|launch| is_implementing(&launch.activity_id))
}

/// The runs among `run_ids` whose complete audit trail started no
/// implementing activity.
fn runs_without_implementing_activity(
    runtime: &OrbitRuntime,
    run_ids: &BTreeSet<String>,
) -> Result<HashSet<String>, orbit_common::OrbitError> {
    let workspace_id = runtime.workspace_id()?;
    let store = runtime.v2_audit_store()?;
    let run_ids: Vec<String> = run_ids.iter().cloned().collect();
    let read = |kind: &str, limit: usize| {
        store.list_v2_audit_events_for_runs_partitioned(
            &workspace_id,
            &run_ids,
            None,
            Some(kind),
            limit,
        )
    };
    let started: HashSet<String> = read("run_started", 1)?
        .into_iter()
        .map(|row| row.run_id)
        .collect();
    let finished: HashSet<String> = read("run_finished", 1)?
        .into_iter()
        .map(|row| row.run_id)
        .collect();
    let mut activities: HashMap<String, (usize, bool)> = HashMap::new();
    for row in read("activity_started", ACTIVITY_EVENTS_PER_RUN)? {
        let implementing = serde_json::from_str::<Value>(&row.payload_json)
            .ok()
            .and_then(|payload| {
                payload
                    .get("activity_name")
                    .and_then(Value::as_str)
                    .map(is_implementing)
            })
            // An activity event whose name cannot be read proves nothing.
            .unwrap_or(true);
        let entry = activities.entry(row.run_id).or_default();
        entry.0 += 1;
        entry.1 |= implementing;
    }
    Ok(run_ids
        .into_iter()
        .filter(|run_id| started.contains(run_id) && finished.contains(run_id))
        .filter(|run_id| {
            activities.get(run_id).is_none_or(|(count, implementing)| {
                *count < ACTIVITY_EVENTS_PER_RUN && !implementing
            })
        })
        .collect())
}
