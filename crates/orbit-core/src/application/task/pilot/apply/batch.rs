//! Apply a prepared batch, settling each partition independently.
use super::super::requested_workspace_root;
use super::super::source::SourceSnapshot;
use super::input::{self, ApplyInput};
use super::partition::settle_partition;
use super::report::Report;
use super::{Applied, ApplyContext, selection, unresolved};
use crate::OrbitRuntime;
use crate::application::task::{TaskAutomationPort, TaskJobPort};
use orbit_engine::DispatchError;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) fn apply(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let workspace_root = requested_workspace_root(runtime, action, input)?;
    let mut envelope = ApplyInput::parse(action, input)?.envelope(action, &workspace_root)?;
    let claim = runtime
        .state_claim(&envelope.prepared)
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
    // The same consumer policy prepare fingerprinted under; a run without a
    // claim evaluates the default eligibility and configured freshness
    // [ORB-12745, ORB-13638].
    let policy = runtime
        .claim_preparation_policy(claim.as_ref())
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
    let source = SourceSnapshot::from_prepared(&envelope.prepared, action)?;
    let source_tree = match &source {
        Some(source) => {
            source.ensure_commit(action, &workspace_root)?;
            Some(source.tree_id(action, &workspace_root)?)
        }
        None => None,
    };
    let prepared_before = input::snapshots(action, &envelope.tasks, source.as_ref())?;
    // Tasks whose material the branch changed since preparation settle
    // superseded, whatever their pilot returned, so their members are claimed
    // afresh at the head; disjoint siblings still apply [ORB-14476].
    let source_superseded = match &claim {
        Some(claim) => runtime
            .stale_claimed_tasks(
                claim,
                &policy,
                &envelope.prepared,
                &prepared_before.keys().cloned().collect::<Vec<_>>(),
                &BTreeMap::new(),
            )
            .map_err(|error| DispatchError::action_failed(action, error.to_string()))?,
        None => BTreeMap::new(),
    };
    let authority = selection::authority(
        runtime,
        action,
        &envelope.authority,
        &selection::PreparedSelection {
            mode: &envelope.mode,
            prepared: &envelope.prepared,
            excluded: &envelope.excluded,
            tasks: &prepared_before,
        },
        &mut envelope.carried_task_outcomes,
    )?;
    let partitions = selection::partitions(action, &envelope.partitions, &prepared_before)?;

    // Best effort: a brief's provenance names the crew when the run says.
    let pilot_crew = envelope
        .authority
        .run_id
        .as_deref()
        .and_then(|run_id| runtime.stored_job_run(run_id).ok().flatten())
        .and_then(|run| run.resolved_crew);
    let round = envelope
        .prepared
        .get(super::super::auto_effort::PILOT_ROUND)
        .and_then(Value::as_str)
        .filter(|round| *round == super::super::auto_effort::REPAIR_ROUND)
        .unwrap_or(super::super::auto_effort::FIRST_ROUND);
    let partition_draws = envelope
        .authority
        .run_id
        .as_deref()
        .and_then(|run_id| runtime.stored_run_state(run_id).ok().flatten())
        .map(|state| {
            partitions
                .iter()
                .filter_map(|partition| {
                    let key = super::super::auto_effort::partition_draw_key(round, partition.index);
                    let draw = state.activity_crew_draws.get(&key)?;
                    Some((partition.index, draw.clone()))
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let context = ApplyContext {
        runtime,
        action,
        workspace_root: &workspace_root,
        prepared: &envelope.prepared,
        source: source.as_ref(),
        source_tree: source_tree.as_deref(),
        policy: &policy,
        authority: &authority,
        source_superseded: &source_superseded,
        run_id: envelope.authority.run_id.as_deref(),
        pilot_crew: pilot_crew.as_deref(),
        round,
        partition_draws: &partition_draws,
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
    unresolved::record_failed_repilots(
        &context,
        &prepared_before,
        &partition_decisions,
        &envelope.results,
    )?;
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
