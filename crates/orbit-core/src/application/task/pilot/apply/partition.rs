//! One prepared partition settled against its pilot result.
//!
//! Each partition is its own validation boundary. A malformed or stale
//! partition mutates none of its tasks, but it cannot discard an unrelated
//! partition whose agent result and prepared snapshot are still current.

use std::collections::{BTreeMap, BTreeSet};

use orbit_engine::DispatchError;
use serde_json::{Value, json};

use super::super::PILOT_NORMALIZATIONS;
use super::super::apply_admission::PreparedTaskSnapshot;
use super::super::persist::{failed_partition, task_outcome};
use super::input::{PartitionResult, Record};
use super::task::settle_task;
use super::{Applied, ApplyContext, detail, settled};

/// A prepared partition whose layout the envelope checks accepted.
pub(super) struct Partition {
    pub(super) index: u64,
    pub(super) task_ids: Vec<String>,
}

/// The partition's decision: each task settled against its assessment, or
/// every task failed when the result itself is unusable.
pub(super) fn settle_partition(
    context: &ApplyContext<'_>,
    applied: &mut Applied,
    prepared_before: &BTreeMap<String, PreparedTaskSnapshot>,
    position: usize,
    partition: &Partition,
    result: Option<&Record<PartitionResult>>,
) -> Value {
    let (assessments, normalizations) =
        match assessments(context.action, position, partition, result) {
            Ok(checked) => checked,
            Err(error) => {
                return failed_partition(
                    partition.index,
                    &partition.task_ids,
                    error,
                    context.source_superseded,
                );
            }
        };
    let mut outcomes = Vec::with_capacity(partition.task_ids.len());
    let mut applied_task_ids = Vec::new();
    for task_id in &partition.task_ids {
        let Some(assessment) = assessments.get(task_id) else {
            outcomes.push(task_outcome(
                task_id,
                "invalid",
                Some(format!(
                    "partition {} omitted task {task_id}",
                    partition.index
                )),
            ));
            continue;
        };
        match settle_task(
            context,
            applied,
            task_id,
            assessment,
            &prepared_before[task_id],
        ) {
            Ok(outcome) => {
                applied_task_ids.push(task_id.clone());
                outcomes.push(outcome);
            }
            Err(outcome) => outcomes.push(outcome),
        }
    }
    decision(partition, outcomes, applied_task_ids, normalizations)
}

/// The result's assessments by task id, with the host's notes on the
/// result's formatting, or why the result is unusable.
fn assessments<'a>(
    action: &str,
    position: usize,
    partition: &Partition,
    result: Option<&'a Record<PartitionResult>>,
) -> Result<(BTreeMap<String, &'a Value>, Vec<Value>), String> {
    let index = partition.index;
    let Some(result) = result else {
        return Err(format!("partition result {position} is missing"));
    };
    if !result.object {
        return Err(format!(
            "partition {index} failed or returned no structured result"
        ));
    }
    let result = &result.fields;
    // The host owns the partition index: results are matched by position,
    // so a missing or wrong echo is a formatting slip, not a failure. The
    // task_ids check below still rejects a result for another partition.
    let normalizations = match result.partition_index.get() {
        Some(reported) if *reported == index => Vec::new(),
        Some(reported) => vec![json!({
            "field": "partition_index",
            "note": format!(
                "result {position} reported index {reported}; the host's index {index} applies"
            ),
        })],
        None => vec![json!({
            "field": "partition_index",
            "note": format!(
                "result {position} omitted partition_index; the host's index {index} applies"
            ),
        })],
    };
    let result_ids = result
        .task_ids
        .strings("task_ids", action)
        .map_err(|error| error.to_string())?;
    if result_ids != partition.task_ids {
        return Err(format!(
            "partition {index} task_ids do not match the prepared partition"
        ));
    }
    let Some(assessments) = result.tasks.get() else {
        return Err(format!("partition {index}.tasks must be an array"));
    };
    if assessments.len() != partition.task_ids.len() {
        return Err(format!(
            "partition {index} returned {} task assessments for {} tasks",
            assessments.len(),
            partition.task_ids.len()
        ));
    }
    let by_id = assessments
        .iter()
        .map(|assessment| {
            let task_id = assessment.fields.task_id.non_empty("task_id", action)?;
            Ok((task_id.to_string(), &assessment.raw))
        })
        .collect::<Result<BTreeMap<_, _>, DispatchError>>()
        .map_err(|error| error.to_string())?;
    if by_id.len() != assessments.len() {
        return Err(format!(
            "partition {index} contains duplicate task assessments"
        ));
    }
    let assessed = by_id.keys().collect::<BTreeSet<_>>();
    if assessed != partition.task_ids.iter().collect::<BTreeSet<_>>() {
        return Err(format!(
            "partition {index} task assessment identities do not match the prepared partition"
        ));
    }
    Ok((by_id, normalizations))
}

fn decision(
    partition: &Partition,
    outcomes: Vec<Value>,
    applied_task_ids: Vec<String>,
    normalizations: Vec<Value>,
) -> Value {
    let unresolved = outcomes.iter().filter(|outcome| !settled(outcome)).count();
    let outcome = if unresolved == 0 {
        if outcomes.iter().any(|task| task["outcome"] == "superseded") {
            "superseded"
        } else {
            "applied"
        }
    } else if applied_task_ids.is_empty()
        && outcomes.iter().all(|outcome| outcome["outcome"] == "stale")
    {
        "skipped_stale"
    } else if applied_task_ids.is_empty() {
        "failed"
    } else {
        "partial"
    };
    let error = outcomes.iter().find_map(|task| {
        (!settled(task))
            .then(|| detail(task).map(ToOwned::to_owned))
            .flatten()
    });
    let stale_tasks = outcomes
        .iter()
        .filter(|task| task["outcome"] == "stale")
        .cloned()
        .collect::<Vec<_>>();
    let mut decision = json!({
        "partition_index": partition.index,
        "task_ids": partition.task_ids,
        "outcome": outcome,
        "applied_task_ids": applied_task_ids,
        "task_outcomes": outcomes,
        "unresolved_count": unresolved,
        "error": error,
        "stale_tasks": stale_tasks,
    });
    if !normalizations.is_empty() {
        decision[PILOT_NORMALIZATIONS] = json!(normalizations);
    }
    decision
}
