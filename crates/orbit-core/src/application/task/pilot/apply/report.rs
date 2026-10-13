//! The apply step's output: the partition decisions rolled up into the run's
//! status, the repair input for invalid assessments, and one evidence record
//! per claim member this apply settled.

use std::path::Path;

use orbit_types::workflow::automation::members::{MemberAttempt, MemberEvidence};
use serde_json::{Value, json};

use super::super::member_ready;
use super::super::source::SourceSnapshot;
use super::Applied;
use super::input::{PreparedEcho, PreparedTask, Record};

pub(super) struct Report<'a> {
    pub(super) workspace_root: &'a Path,
    pub(super) mode: &'a str,
    pub(super) echo: &'a PreparedEcho,
    pub(super) tasks: &'a [Record<PreparedTask>],
    pub(super) source: Option<&'a SourceSnapshot>,
    pub(super) claim: Option<&'a MemberAttempt>,
    pub(super) carried_task_outcomes: &'a [Value],
    pub(super) prior_applied_count: u64,
    pub(super) partition_count: usize,
    pub(super) received_partition_count: usize,
}

fn settled(task: &Value) -> bool {
    matches!(
        task["outcome"].as_str(),
        Some("applied" | "already_applied" | "superseded")
    )
}

fn detail(task: &Value) -> Option<&str> {
    task.get("error")
        .and_then(Value::as_str)
        .or_else(|| task.get("detail").and_then(Value::as_str))
}

impl Report<'_> {
    pub(super) fn render(self, partition_decisions: Vec<Value>, applied: Applied) -> Value {
        let failed_partitions = partition_decisions
            .iter()
            .filter(|decision| matches!(decision["outcome"].as_str(), Some("failed" | "partial")))
            .cloned()
            .collect::<Vec<_>>();
        let own_task_outcomes = partition_decisions
            .iter()
            .filter_map(|decision| decision["task_outcomes"].as_array())
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        let mut task_outcomes = self.carried_task_outcomes.to_vec();
        task_outcomes.extend(own_task_outcomes.iter().cloned());
        let skipped_stale_partitions = partition_decisions
            .iter()
            .filter(|decision| decision["outcome"] == "skipped_stale")
            .cloned()
            .collect::<Vec<_>>();
        let applied_tasks = self.prior_applied_count as usize
            + partition_decisions
                .iter()
                .filter_map(|decision| decision["applied_task_ids"].as_array())
                .map(Vec::len)
                .sum::<usize>();
        let unresolved_tasks = self
            .carried_task_outcomes
            .iter()
            .filter(|task| task["outcome"] != "superseded")
            .count() as u64
            + partition_decisions
                .iter()
                .filter_map(|decision| decision.get("unresolved_count").and_then(Value::as_u64))
                .sum::<u64>();
        let succeeded = failed_partitions.is_empty()
            && skipped_stale_partitions.is_empty()
            && self
                .carried_task_outcomes
                .iter()
                .all(|task| task["outcome"] == "superseded");
        let status = if succeeded { "succeeded" } else { "failed" };
        let superseded_count = task_outcomes
            .iter()
            .filter(|task| task["outcome"] == "superseded")
            .count();
        let error = (!succeeded).then(|| {
            format!(
                "task-pilot apply unresolved: {}; {applied_tasks} applied, {unresolved_tasks} unresolved",
                self.first_unresolved(&partition_decisions)
            )
        });
        let repair = self.repair(&own_task_outcomes);
        let non_repairable_outcomes = task_outcomes
            .iter()
            .filter(|outcome| {
                !matches!(
                    outcome["outcome"].as_str(),
                    Some("invalid" | "applied" | "already_applied")
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        let member_evidence = self.claim.map(|claim| member_evidence(claim, &applied));
        json!({
            "member_evidence": member_evidence,
            "status": status,
            "outcome": if succeeded && superseded_count > 0 { "superseded" } else { status },
            "superseded_count": superseded_count,
            "error": error,
            "mode": self.mode,
            "workspace_path": self.workspace_root,
            "source": self.echo.source,
            "source_age": self.echo.source_age,
            "discovery": {
                "task_ids": self.echo.task_ids,
                "excluded": self.echo.excluded,
            },
            "partition_decisions": partition_decisions,
            "partition_count": self.partition_count,
            "received_partition_count": self.received_partition_count,
            "failed_partitions": failed_partitions,
            "skipped_stale_partitions": skipped_stale_partitions,
            "task_outcomes": task_outcomes,
            "applied_count": applied_tasks,
            "unresolved_count": unresolved_tasks,
            "repair_count": repair.task_ids.len(),
            "repair_partitions": repair.partitions,
            "repair_prepared": repair.prepared,
            "non_repairable_outcomes": non_repairable_outcomes,
            "tasks": applied.task_results,
            "ci_sweep_admission": applied.ci_sweep_admission,
            "drain_approval": applied.drain_approval,
        })
    }

    /// The first task left unresolved, by partition and then among the
    /// carried outcomes.
    fn first_unresolved(&self, partition_decisions: &[Value]) -> String {
        partition_decisions
            .iter()
            .find_map(|partition| {
                let partition_index = partition["partition_index"].as_u64()?;
                let task = partition["task_outcomes"]
                    .as_array()?
                    .iter()
                    .find(|task| !settled(task))?;
                let classification = task
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| task["outcome"].as_str().unwrap_or("unresolved"));
                Some(format!(
                    "partition {partition_index}, task {}: {classification}: {}",
                    task["task_id"].as_str().unwrap_or("<unknown>"),
                    detail(task).unwrap_or("unresolved task"),
                ))
            })
            .or_else(|| {
                self.carried_task_outcomes
                    .iter()
                    .find(|task| task["outcome"] != "superseded")
                    .map(|task| {
                        format!(
                            "carried task {}: {}",
                            task["task_id"].as_str().unwrap_or("<unknown>"),
                            detail(task).unwrap_or("unresolved task")
                        )
                    })
            })
            .unwrap_or_else(|| "partition envelope validation failed".to_string())
    }

    /// One single-task partition per invalid assessment, and the prepared
    /// input a repair apply settles them with.
    fn repair(&self, own_task_outcomes: &[Value]) -> Repair {
        let task_ids = own_task_outcomes
            .iter()
            .filter(|outcome| outcome["outcome"] == "invalid")
            .filter_map(|outcome| outcome["task_id"].as_str().map(ToOwned::to_owned))
            .collect::<Vec<_>>();
        let partitions = task_ids
            .iter()
            .enumerate()
            .map(|(partition_index, task_id)| {
                let errors = own_task_outcomes
                    .iter()
                    .filter(|outcome| outcome["task_id"].as_str() == Some(task_id))
                    .filter_map(|outcome| outcome["error"].as_str())
                    .collect::<Vec<_>>();
                json!({
                    "partition_index": partition_index,
                    "task_ids": [task_id],
                    "validation_errors": errors,
                    "source_revision": self.source.map(|source| source.source_revision.as_str()),
                })
            })
            .collect::<Vec<_>>();
        let tasks = self
            .tasks
            .iter()
            .filter(|task| {
                task.fields
                    .task_id
                    .get()
                    .is_some_and(|id| task_ids.contains(id))
            })
            .map(|task| task.raw.clone())
            .collect::<Vec<_>>();
        // The repair apply keeps the claim so a repaired member still
        // certifies its own evidence under the consumer's predicate
        // [ORB-12746].
        let prepared = json!({
            "state_automation": self.echo.state_automation,
            "mode": self.mode,
            "workspace_path": self.workspace_root,
            "source": self.echo.source,
            "task_count": task_ids.len(),
            "task_ids": task_ids,
            "tasks": tasks,
            "partition_size": 1,
            "partition_count": partitions.len(),
            "partitions": partitions,
            "excluded": [],
        });
        Repair {
            task_ids,
            partitions,
            prepared,
        }
    }
}

struct Repair {
    task_ids: Vec<String>,
    partitions: Vec<Value>,
    prepared: Value,
}

/// One evidence record per claim member this apply settled, independent of
/// its siblings: a failed partition never withholds an applied member's
/// receipt [ORB-12746].
fn member_evidence(claim: &MemberAttempt, applied: &Applied) -> Vec<MemberEvidence> {
    claim
        .members()
        .iter()
        .filter_map(|member| {
            let id = member.task_ids.first()?;
            let resulting = applied.resulting_fingerprints.get(id)?;
            let assessment = applied
                .task_results
                .iter()
                .find(|result| result["task_id"].as_str() == Some(id))?;
            Some(MemberEvidence {
                action_id: claim.action_id.clone().unwrap_or_default(),
                attempt_id: claim.id.clone(),
                member_key: member.key.clone(),
                input_fingerprint: member.fingerprint.clone(),
                resulting_fingerprint: resulting.clone(),
                ready: member_ready(assessment),
                result: assessment.clone(),
            })
        })
        .collect()
}
