//! Verify promotion authority and the prepared selection's partitions.
use super::super::drain_promotion::DrainAuthority;
use super::admission::{PreparedTaskSnapshot, PromotionAuthority};
use super::input::{AuthorityInput, ExcludedTask, PreparedPartition, Record};
use super::partition::Partition;
use crate::OrbitRuntime;
use crate::application::task::TaskSweepFilingPort;
use orbit_engine::DispatchError;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The prepared selection an authority is checked against.
pub(super) struct PreparedSelection<'a> {
    pub(super) mode: &'a str,
    pub(super) prepared: &'a Value,
    pub(super) excluded: &'a [ExcludedTask],
    pub(super) tasks: &'a BTreeMap<String, PreparedTaskSnapshot>,
}

/// Who, if anyone, authorized this apply to promote. A CI sweep whose one
/// filed task another pilot holds, and each task of an explicit selection
/// another pilot holds, settle superseded among the carried outcomes.
pub(super) fn authority<'a>(
    runtime: &OrbitRuntime,
    action: &str,
    input: &'a AuthorityInput,
    selection: &PreparedSelection<'_>,
    carried_task_outcomes: &mut Vec<Value>,
) -> Result<PromotionAuthority<'a>, DispatchError> {
    let ci_sweep_filing = input.ci_sweep_filing.as_ref();
    let piloted_elsewhere = if let Some(filing) = ci_sweep_filing
        && selection.tasks.is_empty()
    {
        let task_id = filing.required_task_id(action)?;
        runtime.ci_sweep_piloted_elsewhere(action, task_id, selection.prepared)?
    } else {
        None
    };
    if let Some(outcome) = &piloted_elsewhere {
        carried_task_outcomes.push(outcome.clone());
    } else if ci_sweep_filing.is_none() && selection.mode == "explicit" {
        carried_task_outcomes.extend(held_elsewhere(selection.excluded));
    }
    let promotion_authorized = input.promotion_authorized(action)?;
    // Reservation acquisition refused before any assessment or reservation
    // write. Its carried outcome releases this attempt for fresh admission;
    // a CI-sweep child must not turn that wait into a missing-task failure.
    let reservation_deferred = if let Some(filing) = ci_sweep_filing {
        let task_id = filing.required_task_id(action)?;
        selection.tasks.is_empty()
            && carried_task_outcomes.iter().any(|outcome| {
                outcome["task_id"] == task_id
                    && outcome["outcome"] == "superseded"
                    && outcome["reason"] == "pilot_reservation_lock_busy"
            })
    } else {
        false
    };
    if ci_sweep_filing.is_some()
        && selection.tasks.len() + usize::from(piloted_elsewhere.is_some() || reservation_deferred)
            != 1
    {
        return Err(DispatchError::action_failed(
            action,
            "CI-sweep admission requires exactly one prepared task",
        ));
    }
    let authority = match (ci_sweep_filing, &input.drain_promotion) {
        (Some(_), Some(_)) => {
            return Err(DispatchError::action_failed(
                action,
                "ci_sweep_filing and drain_promotion are separate authorities; supply one",
            ));
        }
        (Some(filing), None) => PromotionAuthority::CiSweep(filing, promotion_authorized),
        (None, Some(_)) if !promotion_authorized => {
            return Err(DispatchError::action_failed(
                action,
                "drain_promotion requires promotion_authorized",
            ));
        }
        (None, Some(record)) => PromotionAuthority::Drain(DrainAuthority::verify(
            runtime,
            action,
            record,
            input.run_id.as_deref(),
        )?),
        (None, None) => PromotionAuthority::None,
    };
    if let (Some(filing), Some(prepared_task_id)) = (ci_sweep_filing, selection.tasks.keys().next())
    {
        let filed_task_id = filing.required_task_id(action)?;
        if filed_task_id != prepared_task_id {
            return Err(DispatchError::action_failed(
                action,
                format!("CI-sweep filing names task {filed_task_id}, expected {prepared_task_id}"),
            ));
        }
    }
    Ok(authority)
}

/// The prepared partitions, refused unless together they name every prepared
/// task exactly once.
pub(super) fn partitions(
    action: &str,
    prepared: &[Record<PreparedPartition>],
    tasks: &BTreeMap<String, PreparedTaskSnapshot>,
) -> Result<Vec<Partition>, DispatchError> {
    let mut seen_task_ids = BTreeSet::new();
    let mut partitions = Vec::with_capacity(prepared.len());
    for (position, partition) in prepared.iter().enumerate() {
        let index = partition
            .fields
            .partition_index
            .get()
            .copied()
            .ok_or_else(|| {
                DispatchError::action_failed(
                    action,
                    format!("prepared.partitions[{position}].partition_index is invalid"),
                )
            })?;
        let task_ids = partition.fields.task_ids.strings("task_ids", action)?;
        for task_id in &task_ids {
            if !seen_task_ids.insert(task_id.clone()) {
                return Err(DispatchError::action_failed(
                    action,
                    format!("task {task_id} appears in more than one prepared partition"),
                ));
            }
            if !tasks.contains_key(task_id) {
                return Err(DispatchError::action_failed(
                    action,
                    format!("task {task_id} was not in prepared.tasks"),
                ));
            }
        }
        partitions.push(Partition { index, task_ids });
    }
    if seen_task_ids.len() != tasks.len() {
        return Err(DispatchError::action_failed(
            action,
            "prepared partitions did not cover every prepared task",
        ));
    }
    Ok(partitions)
}

/// An explicit selection names tasks its caller wants assessed. Each one
/// another live pilot holds settles superseded, naming the holder that
/// applies it, so the caller's run neither fails nor leaves a claimed member
/// without an outcome.
fn held_elsewhere(excluded: &[ExcludedTask]) -> Vec<Value> {
    excluded
        .iter()
        .filter(|entry| {
            entry
                .reason
                .get()
                .is_some_and(|reason| reason == "already_preparing")
        })
        .filter_map(|entry| {
            let run_ids = entry.prepared_by_run_ids.get()?;
            Some(json!({
                "task_id": entry.task_id.get()?,
                "outcome": "superseded",
                "reason": "piloted_elsewhere",
                "run_id": run_ids.first()?,
                "run_ids": run_ids,
                "detail": "another active task-pilot run already prepared this task",
            }))
        })
        .collect()
}
