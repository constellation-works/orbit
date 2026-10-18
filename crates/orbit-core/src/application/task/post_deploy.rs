//! Acceptance criteria a delivery owes after deploy.
//!
//! An implementer may hand off a criterion the task pilot typed post-deploy
//! as owed, with the host, measurement command and target. The owner admits
//! the claim only against that finding, records it in task history, and the
//! delivery settles in `review` rather than `done`. Measurement afterwards is
//! the operator's: a `post-deploy: met <N>` or `post-deploy: failed <N>`
//! comment with evidence records each result. Nothing re-measures on its own,
//! and a failed criterion is repaired through an ordinary new task.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{Task, TaskHistoryEntry};
use orbit_types::workflow::{
    OwedCriterion, POST_DEPLOY_OWED_EVENT, POST_DEPLOY_RECORDED_EVENT, PostDeployOwed,
    PostDeployRecord as OwedRecord, PostDeployResult, post_deploy_header,
};
use serde_json::{Value, json};

use super::TaskRecordUpdateParams;
use super::helpers::SYSTEM_ACTOR_LABEL;
use crate::OrbitRuntime;

/// The `owed_criteria` section of a task record: each criterion the current
/// delivery owes after deploy, with its state (`owed`, `met` or `failed`).
/// `None` when the task has never owed one.
pub(crate) fn owed_criteria_json(history: &[TaskHistoryEntry]) -> Option<Value> {
    let record = OwedRecord::read(history)?;
    Some(Value::Array(
        record
            .owed
            .criteria
            .iter()
            .zip(&record.recorded)
            .map(|(owed, recorded)| {
                let mut entry = json!({
                    "criterion": owed.criterion,
                    "host": owed.host,
                    "command": owed.command,
                    "target": owed.target,
                    "state": recorded
                        .as_ref()
                        .map_or("owed", |recorded| recorded.result.outcome.as_str()),
                    "run_id": record.owed.run_id,
                });
                if !owed.candidate_evidence.is_empty() {
                    entry["candidate_evidence"] = json!(owed.candidate_evidence);
                }
                if let Some(recorded) = recorded {
                    entry["evidence"] = json!(recorded.result.evidence);
                    entry["recorded_by"] = json!(recorded.by);
                    entry["recorded_at"] = json!(recorded.at);
                }
                entry
            })
            .collect(),
    ))
}

impl OrbitRuntime {
    /// Refuse an owed claim unless the task's current pilot finding typed
    /// its criterion post-deploy for the same host. This is the only way a
    /// criterion goes unmet at handoff; every other one still fails.
    pub(crate) fn check_owed_criteria(
        &self,
        task: &Task,
        owed: &[OwedCriterion],
    ) -> Result<(), OrbitError> {
        if owed.is_empty() {
            return Ok(());
        }
        // A handoff reaches here without the implement step's shape check.
        OwedCriterion::check_all(owed).map_err(OrbitError::PolicyDenied)?;
        let comments = self.get_task_comments(&task.id)?;
        let typed = self.pilot_post_deploy(task, &comments)?;
        for claim in owed {
            let Some(requirement) = typed
                .iter()
                .find(|requirement| requirement.criterion == claim.criterion)
            else {
                return Err(OrbitError::PolicyDenied(format!(
                    "criterion {} of task {} is not one its task-pilot assessment typed \
                     post-deploy, so it cannot be owed after deploy",
                    claim.criterion, task.id
                )));
            };
            if !claim
                .host
                .trim()
                .eq_ignore_ascii_case(requirement.host.trim())
            {
                return Err(OrbitError::PolicyDenied(format!(
                    "criterion {} of task {} is owed on {}, but its task-pilot assessment typed \
                     it for {}",
                    claim.criterion, task.id, claim.host, requirement.host
                )));
            }
        }
        Ok(())
    }

    /// Admit an implementer's owed claim on `task_id`. An owner run records
    /// it as the task's owed record; a claimed leaf only checks it, because
    /// the owner checks it again and records it when it accepts the handoff.
    pub(crate) fn admit_owed_criteria(
        &self,
        task_id: &str,
        run_id: &str,
        owed: &[OwedCriterion],
    ) -> Result<(), OrbitError> {
        let task = self.get_task(task_id)?;
        self.check_owed_criteria(&task, owed)?;
        if owed.is_empty() || self.worker_invocation().is_some() {
            return Ok(());
        }
        let note = serde_json::to_string(&PostDeployOwed {
            run_id: run_id.to_string(),
            criteria: owed.to_vec(),
        })
        .map_err(|error| OrbitError::Execution(format!("encode owed criteria: {error}")))?;
        self.with_mutation(|| {
            let updated = self.stores().task_records().update(
                task_id,
                TaskRecordUpdateParams {
                    actor: SYSTEM_ACTOR_LABEL.to_string(),
                    append_history: vec![TaskHistoryEntry {
                        at: Utc::now(),
                        by: SYSTEM_ACTOR_LABEL.to_string(),
                        event: POST_DEPLOY_OWED_EVENT.to_string(),
                        note: Some(note.clone()),
                        from_status: None,
                        to_status: None,
                        operation_id: None,
                        decision: None,
                    }],
                    ..Default::default()
                },
            )?;
            Ok((
                updated,
                OrbitEvent::TaskUpdated {
                    id: task_id.to_string(),
                },
            ))
        })?;
        Ok(())
    }

    /// The criteria of the task's current owed record that no operator has
    /// recorded met; with `run_id`, only when that run made the record.
    pub(crate) fn outstanding_owed_criteria(
        &self,
        task_id: &str,
        run_id: Option<&str>,
    ) -> Result<Vec<usize>, OrbitError> {
        let history = self.get_task_history(task_id)?;
        Ok(OwedRecord::read(&history)
            .filter(|record| run_id.is_none_or(|run_id| record.owed.run_id == run_id))
            .map(|record| record.outstanding())
            .unwrap_or_default())
    }

    /// The history entry an operator's `post-deploy: met|failed <N>` comment
    /// records, or `None` when the comment is not one. Only the trusted human
    /// update path calls this; a worker's comment of the same shape records
    /// nothing.
    pub(in crate::application::task) fn post_deploy_resolution(
        &self,
        task: &Task,
        comment: Option<&str>,
        actor: &str,
    ) -> Result<Option<TaskHistoryEntry>, OrbitError> {
        let Some(message) = comment else {
            return Ok(None);
        };
        let (header, evidence) = message.split_once('\n').unwrap_or((message, ""));
        let Some(stated) = post_deploy_header(header) else {
            return Ok(None);
        };
        let (outcome, criterion) = stated.map_err(OrbitError::InvalidInput)?;
        let evidence = evidence.trim();
        if evidence.is_empty() {
            return Err(OrbitError::InvalidInput(
                "a post-deploy record requires the measurement's evidence after its first line"
                    .into(),
            ));
        }
        let history = self.get_task_history(&task.id)?;
        let owes = OwedRecord::read(&history).is_some_and(|record| {
            record
                .owed
                .criteria
                .iter()
                .any(|owed| owed.criterion == criterion)
        });
        if !owes {
            return Err(OrbitError::InvalidInput(format!(
                "task {} owes no criterion {criterion} after deploy",
                task.id
            )));
        }
        let note = serde_json::to_string(&PostDeployResult {
            criterion,
            outcome,
            evidence: evidence.to_string(),
        })
        .map_err(|error| OrbitError::Execution(format!("encode post-deploy result: {error}")))?;
        Ok(Some(TaskHistoryEntry {
            at: Utc::now(),
            by: actor.into(),
            event: POST_DEPLOY_RECORDED_EVENT.into(),
            note: Some(note),
            from_status: None,
            to_status: None,
            operation_id: None,
            decision: None,
        }))
    }
}
