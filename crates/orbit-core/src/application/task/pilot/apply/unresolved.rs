//! Hold stale repilots that finish apply without certifying unchanged meaning.

use std::collections::BTreeMap;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::ProviderFailureClass;
use serde_json::Value;

use crate::application::contracts::INSPECTION_CREW_UNAVAILABLE;
use crate::application::task::{
    SYSTEM_ACTOR_LABEL, TaskJobPort, TaskRecordUpdateParams, TaskRecords,
};

use super::super::persist::{superseded_with_current, with_task_locks};
use super::admission::{PreparedTaskSnapshot, PromotionAuthority};
use super::input::{PartitionResult, Record};
use super::{ApplyContext, settled};

/// Record both proposed and backlog failures here: standalone scheduled runs
/// have no drain to record unresolved children after they finish. A later
/// successful repair applies a newer receipt and releases this hold.
pub(super) fn record_failed_repilots(
    context: &ApplyContext<'_>,
    snapshots: &BTreeMap<String, PreparedTaskSnapshot>,
    decisions: &[Value],
    results: &[Record<PartitionResult>],
) -> Result<(), DispatchError> {
    let Some(run_id) = context.run_id else {
        return Ok(());
    };
    let runtime = context.runtime;
    let record = || -> Result<(), OrbitError> {
        let run = runtime.stored_job_run(run_id)?;
        let independent_failure = run.is_some_and(|run| {
            run.steps.iter().any(|step| {
                ProviderFailureClass::of(step.error_code.as_deref(), step.error_message.as_deref())
                    .is_some()
                    || step
                        .error_message
                        .as_deref()
                        .is_some_and(|message| message.contains(INSPECTION_CREW_UNAVAILABLE))
            })
        });
        for (position, decision) in decisions.iter().enumerate() {
            // An outage with no assessment says nothing about the task. A
            // concrete task result still owns its refusal even when another
            // partition of this run encountered an independent failure.
            let has_assessments = results
                .get(position)
                .and_then(|result| result.fields.tasks.get())
                .is_some_and(|tasks| !tasks.is_empty());
            if independent_failure && !has_assessments {
                continue;
            }
            for outcome in decision["task_outcomes"].as_array().into_iter().flatten() {
                if settled(outcome) {
                    continue;
                }
                let Some(task_id) = outcome["task_id"].as_str() else {
                    continue;
                };
                let Some(snapshot) = snapshots.get(task_id) else {
                    continue;
                };
                if context.source_superseded.contains_key(task_id) {
                    continue;
                }
                with_task_locks(runtime, task_id, &[task_id.to_string()], 0, &mut || {
                    let current = runtime.get_task(task_id)?;
                    if !matches!(current.status, TaskStatus::Proposed | TaskStatus::Backlog) {
                        return Ok(());
                    }
                    let history = runtime.get_task_history(task_id)?;
                    if superseded_with_current(
                        runtime,
                        &current,
                        &history,
                        snapshot,
                        context.policy,
                        matches!(context.authority, PromotionAuthority::CiSweep(..)),
                    )?
                    .is_some()
                    {
                        return Ok(());
                    }
                    // Never overwrite a newer apply, or duplicate a replay of
                    // this run's unresolved settlement.
                    if history
                        .iter()
                        .skip(snapshot.history_len.unwrap_or(0))
                        .any(|entry| {
                            entry.event == "task_pilot_applied"
                                || super::super::unresolved_pilot_run_id(entry)
                                    .is_some_and(|recorded| recorded == run_id)
                        })
                    {
                        return Ok(());
                    }
                    let comments = runtime.get_task_comments(task_id)?;
                    let Some((_, receipt)) = super::super::pilot_receipts(&comments).next() else {
                        return Ok(());
                    };
                    let Ok(audit) = serde_json::from_str::<Value>(receipt.body) else {
                        return Ok(());
                    };
                    if !super::super::freshness::pilot_requirements_stale(
                        &current,
                        &audit,
                        receipt.operation_id,
                        &|| Ok(history.clone()),
                    )? {
                        return Ok(());
                    }
                    runtime.stores().task_records().update(
                        task_id,
                        TaskRecordUpdateParams {
                            actor: SYSTEM_ACTOR_LABEL.to_string(),
                            append_history: vec![super::super::unresolved_pilot_entry(
                                run_id,
                                Utc::now(),
                            )],
                            expected_status: Some(vec![TaskStatus::Proposed, TaskStatus::Backlog]),
                            ..Default::default()
                        },
                    )?;
                    Ok(())
                })?;
            }
        }
        Ok(())
    };
    record().map_err(|error| {
        DispatchError::action_failed(
            context.action,
            format!("record unresolved task-pilot results: {error}"),
        )
    })
}
