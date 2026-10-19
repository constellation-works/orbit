//! Task-isolated validation, idempotent recovery, and atomic application for task pilots.
//!
//! The step input is read once into the typed records of [`input`]. The
//! prepared envelope, the promotion authority and the partition layout are
//! all checked before any task is written; each partition then settles on its
//! own ([`partition`], one task at a time in [`task`]), and [`report`] rolls
//! the decisions up into the step output.

pub(super) mod admission;
mod input;
mod partition;
mod report;
mod task;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_engine::DispatchError;
use orbit_types::workflow::automation::members::PreparationPolicy;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::sweep_filing::ci_failure::admission as ci_failure_admission;

use super::drain_promotion::DrainAuthority;
use super::requested_workspace_root;
use super::source::SourceSnapshot;
use admission::{PreparedTaskSnapshot, PromotionAuthority};
use input::{ApplyInput, AuthorityInput, ExcludedTask, PreparedPartition, Record};
use partition::{Partition, settle_partition};
use report::Report;

/// A task outcome that needs no further apply work: applied, already applied,
/// or superseded. Partition decisions and the run report share this test.
fn settled(outcome: &Value) -> bool {
    matches!(
        outcome["outcome"].as_str(),
        Some("applied" | "already_applied" | "superseded")
    )
}

/// Error text of an unsettled task, falling back to its detail.
fn detail(outcome: &Value) -> Option<&str> {
    outcome
        .get("error")
        .and_then(Value::as_str)
        .or_else(|| outcome.get("detail").and_then(Value::as_str))
}

/// What every task of one apply is checked and written against.
struct ApplyContext<'a> {
    runtime: &'a OrbitRuntime,
    action: &'a str,
    workspace_root: &'a Path,
    /// The prepared payload verbatim, for the receipt and write boundaries.
    prepared: &'a Value,
    source: Option<&'a SourceSnapshot>,
    policy: &'a PreparationPolicy,
    authority: &'a PromotionAuthority<'a>,
    /// Tasks whose material the branch changed since preparation.
    source_superseded: &'a BTreeMap<String, String>,
    /// The pilot job run applying these results, recorded on each receipt so
    /// the assessment can be attributed to the run's crew and model.
    run_id: Option<&'a str>,
}

/// What the applied tasks of every partition contribute to the report.
#[derive(Default)]
struct Applied {
    task_results: Vec<Value>,
    ci_sweep_admission: Vec<Value>,
    drain_approval: Vec<Value>,
    resulting_fingerprints: BTreeMap<String, String>,
}

pub(crate) fn apply(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let workspace_root = requested_workspace_root(runtime, action, input)?;
    let mut envelope = ApplyInput::parse(action, input)?.envelope(action, &workspace_root)?;
    let claim = crate::application::automation::members::claim(runtime, &envelope.prepared)
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
    // The same consumer policy prepare fingerprinted under; a run without a
    // claim evaluates the default eligibility and configured freshness
    // [ORB-12745, ORB-13638].
    let policy = crate::application::automation::preparation::claim_policy(runtime, claim.as_ref())
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
    let source = SourceSnapshot::from_prepared(&envelope.prepared, action)?;
    if let Some(source) = &source {
        source.ensure_commit(action, &workspace_root)?;
    }
    let prepared_before = input::snapshots(action, &envelope.tasks, source.as_ref())?;
    // Tasks whose material the branch changed since preparation settle
    // superseded, whatever their pilot returned, so their members are claimed
    // afresh at the head; disjoint siblings still apply [ORB-14476].
    let source_superseded = match &claim {
        Some(claim) => crate::application::automation::members::stale_tasks(
            runtime,
            claim,
            &policy,
            &envelope.prepared,
            &prepared_before.keys().cloned().collect::<Vec<_>>(),
            &BTreeMap::new(),
        )
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?,
        None => BTreeMap::new(),
    };
    let authority = authority(
        runtime,
        action,
        &envelope.authority,
        &PreparedSelection {
            mode: &envelope.mode,
            prepared: &envelope.prepared,
            excluded: &envelope.excluded,
            tasks: &prepared_before,
        },
        &mut envelope.carried_task_outcomes,
    )?;
    let partitions = partitions(action, &envelope.partitions, &prepared_before)?;

    let context = ApplyContext {
        runtime,
        action,
        workspace_root: &workspace_root,
        prepared: &envelope.prepared,
        source: source.as_ref(),
        policy: &policy,
        authority: &authority,
        source_superseded: &source_superseded,
        run_id: envelope.authority.run_id.as_deref(),
    };
    let mut applied = Applied::default();
    let mut partition_decisions = partitions
        .iter()
        .enumerate()
        .map(|(position, partition)| {
            settle_partition(
                &context,
                &mut applied,
                &prepared_before,
                position,
                partition,
                envelope.results.get(position),
            )
        })
        .collect::<Vec<_>>();
    for position in partitions.len()..envelope.results.len() {
        partition_decisions.push(json!({
            "partition_index": Value::Null,
            "task_ids": [],
            "outcome": "failed",
            "error": format!("unexpected extra partition result at position {position}"),
            "unresolved_count": 1,
            "applied_task_ids": [],
            "task_outcomes": [],
        }));
    }

    Ok(Report {
        workspace_root: &workspace_root,
        mode: &envelope.mode,
        echo: &envelope.echo,
        tasks: &envelope.tasks,
        source: source.as_ref(),
        claim: claim.as_ref(),
        carried_task_outcomes: &envelope.carried_task_outcomes,
        prior_applied_count: envelope.prior_applied_count,
        partition_count: partitions.len(),
        received_partition_count: envelope.results.len(),
    }
    .render(partition_decisions, applied))
}

/// The prepared selection an authority is checked against.
struct PreparedSelection<'a> {
    mode: &'a str,
    prepared: &'a Value,
    excluded: &'a [ExcludedTask],
    tasks: &'a BTreeMap<String, PreparedTaskSnapshot>,
}

/// Who, if anyone, authorized this apply to promote. A CI sweep whose one
/// filed task another pilot holds, and each task of an explicit selection
/// another pilot holds, settle superseded among the carried outcomes.
fn authority<'a>(
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
        let task_id = filing.fields.task_id.non_empty("task_id", action)?;
        ci_failure_admission::piloted_elsewhere(action, task_id, selection.prepared)?
    } else {
        None
    };
    if let Some(outcome) = &piloted_elsewhere {
        carried_task_outcomes.push(outcome.clone());
    } else if ci_sweep_filing.is_none() && selection.mode == "explicit" {
        carried_task_outcomes.extend(held_elsewhere(selection.excluded));
    }
    let promotion_authorized = input.promotion_authorized(action)?;
    if ci_sweep_filing.is_some()
        && selection.tasks.len() + usize::from(piloted_elsewhere.is_some()) != 1
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
        (Some(filing), None) => PromotionAuthority::CiSweep(&filing.raw, promotion_authorized),
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
        let filed_task_id = filing.fields.task_id.non_empty("task_id", action)?;
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
fn partitions(
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
