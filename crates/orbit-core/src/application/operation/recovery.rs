//! Aggregate recovery budgets and the privileged-action rechecks for admitted
//! work [ORB-11332].
//!
//! One ledger per task spans engine step-recovery hooks, resumed runs, and
//! terminal-run triage: an episode is reserved *before* a recovery worker is
//! dispatched, its wall time is settled afterwards, and nesting or requeueing
//! never resets the count. Runs without a captured admission keep the
//! pre-existing unbounded behavior. Completion of admitted work is rechecked
//! against the grant at the guarded `review -> done` transition, so a hard
//! revocation stops it even after the window expired.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::{RuntimeHost, StepRecoveryAdmission, TaskAutomationUpdate};
use orbit_store::contracts::{RecoveryBudget, RecoveryReserveRequest};
use orbit_types::task::TaskStatus;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{
    JobRun, JobRunState, OperationAdmission, RecoveryEpisodeKind, RecoveryLedger,
    RecoveryReservation,
};
use serde_json::{Value, json};

use super::{COMPLETION_AUDIT, RECOVERY_AUDIT, captured_policy};
use crate::OrbitRuntime;

/// History event recorded when a task's aggregate allowance is spent.
pub(crate) const RECOVERY_EXHAUSTED_EVENT: &str = "recovery_budget_exhausted";

/// The reservation triage made for one candidate under a grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TriageReservation {
    pub episode: Option<u32>,
    /// Set when the allowance is spent; the candidate is an escalation.
    pub exhausted: Option<&'static str>,
}

/// The tasks a run carries, from its persisted input.
fn run_task_ids(run: &JobRun) -> Vec<String> {
    let Some(input) = run.input.as_ref() else {
        return Vec::new();
    };
    let mut ids = input
        .get("task_ids")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for key in ["task_id", "epic_task_id"] {
        if let Some(id) = input.get(key).and_then(Value::as_str) {
            ids.push(id.to_string());
        }
    }
    ids.sort();
    ids.dedup();
    ids
}

fn budget(admission: &OperationAdmission) -> RecoveryBudget {
    RecoveryBudget {
        episodes: admission.limits.recovery_episodes_per_task,
        seconds: u64::from(admission.limits.recovery_minutes_per_task) * 60,
    }
}

impl OrbitRuntime {
    /// The run and its captured admission, or `None` for unbound runs.
    fn admitted_run(
        &self,
        run_id: &str,
    ) -> Result<Option<(JobRun, OperationAdmission)>, OrbitError> {
        let Some(run) = self.get_job_run_backend(run_id)? else {
            return Ok(None);
        };
        let admission = run
            .input
            .as_ref()
            .map(OperationAdmission::from_run_input)
            .transpose()
            .map_err(OrbitError::InvalidInput)?
            .flatten();
        Ok(admission.map(|admission| (run, admission)))
    }

    /// Reserve a step-recovery episode for every task the run carries.
    /// Unbound runs are always allowed; bound runs are refused once any of
    /// their tasks has spent its allowance, or once the grant is revoked.
    pub(crate) fn authorize_step_recovery(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<StepRecoveryAdmission, OrbitError> {
        let Some((run, admission)) = self.admitted_run(run_id)? else {
            return Ok(StepRecoveryAdmission::Allowed);
        };
        let decision = self.reserve_recovery(
            &run,
            &admission,
            RecoveryEpisodeKind::StepRecovery,
            Some(step_id),
        )?;
        let (outcome, detail) = match &decision {
            StepRecoveryAdmission::Allowed => ("allowed", Value::Null),
            StepRecoveryAdmission::Reserved { episode } => ("reserved", json!(episode)),
            StepRecoveryAdmission::Denied { reason } => ("denied", json!(reason)),
        };
        self.record_pipeline_audit(
            RECOVERY_AUDIT,
            Some(run_id),
            Some("system"),
            AuditEventStatus::Success,
            json!({
                "grant_id": admission.grant_id,
                "kind": "step_recovery",
                "step_id": step_id,
                "task_ids": run_task_ids(&run),
                "outcome": outcome,
                "detail": detail,
                "recorded_at": Utc::now().to_rfc3339(),
            }),
            None,
        )?;
        Ok(decision)
    }

    /// Record the wall time a step-recovery episode consumed.
    pub(crate) fn settle_step_recovery(
        &self,
        run_id: &str,
        step_id: &str,
        elapsed_seconds: u64,
    ) -> Result<(), OrbitError> {
        let Some((run, _)) = self.admitted_run(run_id)? else {
            return Ok(());
        };
        let workspace_id = self.workspace_id()?;
        let store = self.operation_store()?;
        for task_id in run_task_ids(&run) {
            let Some(ledger) = store.operation_recovery_ledger(&workspace_id, &task_id)? else {
                continue;
            };
            if let Some(episode) = open_episode(&ledger, run_id, Some(step_id)) {
                store.operation_recovery_settle(
                    &workspace_id,
                    &task_id,
                    episode,
                    elapsed_seconds,
                    Utc::now(),
                )?;
            }
        }
        Ok(())
    }

    /// Authenticate the active run, tasks, and inherited worktree checkpoint
    /// immediately before executor-owned recovery writes Git metadata.
    pub(crate) fn validate_step_recovery_mutation(
        &self,
        run_id: &str,
        step_id: &str,
        task_ids: &[String],
        workspace_path: &std::path::Path,
    ) -> Result<(), OrbitError> {
        let run = self.get_job_run_backend(run_id)?.ok_or_else(|| {
            OrbitError::Execution(format!(
                "step recovery '{step_id}' has no durable run '{run_id}'"
            ))
        })?;
        if run.state != JobRunState::Running {
            return Err(OrbitError::Execution(format!(
                "step recovery '{step_id}' refuses Git mutation for run '{run_id}' in state '{}'",
                run.state
            )));
        }

        let mut expected_task_ids = run_task_ids(&run);
        let mut observed_task_ids = task_ids.to_vec();
        expected_task_ids.sort();
        expected_task_ids.dedup();
        observed_task_ids.sort();
        observed_task_ids.dedup();
        if expected_task_ids != observed_task_ids {
            return Err(OrbitError::Execution(format!(
                "step recovery '{step_id}' task lineage does not match run '{run_id}'"
            )));
        }
        let mut task_owner = None;
        for task_id in &observed_task_ids {
            let task = self.get_task(task_id)?;
            let owner = task.job_run_id.as_deref().ok_or_else(|| {
                OrbitError::Execution(format!(
                    "step recovery '{step_id}' task '{task_id}' has no active run owner"
                ))
            })?;
            if !matches!(task.status, TaskStatus::InProgress | TaskStatus::Review) {
                return Err(OrbitError::Execution(format!(
                    "step recovery '{step_id}' task '{task_id}' is no longer owned by active run '{run_id}'"
                )));
            }
            match task_owner.as_deref() {
                None => task_owner = Some(owner.to_string()),
                Some(current) if current == owner => {}
                Some(_) => {
                    return Err(OrbitError::Execution(format!(
                        "step recovery '{step_id}' tasks do not share one worktree owner"
                    )));
                }
            }
        }

        let state = self.read_run_state(run_id)?.ok_or_else(|| {
            OrbitError::Execution(format!(
                "step recovery '{step_id}' run '{run_id}' has no durable pipeline state"
            ))
        })?;
        let worktree = state.pipeline.get("worktree").ok_or_else(|| {
            OrbitError::Execution(format!(
                "step recovery '{step_id}' run '{run_id}' has no worktree checkpoint"
            ))
        })?;
        let checkpoint_path = worktree
            .get("workspace_path")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                OrbitError::Execution(format!(
                    "step recovery '{step_id}' run '{run_id}' has no checkpointed workspace path"
                ))
            })?;
        let checkpoint_path = std::fs::canonicalize(checkpoint_path).map_err(|error| {
            OrbitError::Execution(format!(
                "step recovery '{step_id}' cannot resolve checkpointed workspace '{checkpoint_path}': {error}"
            ))
        })?;
        let requested_path = workspace_path.canonicalize().map_err(|error| {
            OrbitError::Execution(format!(
                "step recovery '{step_id}' cannot resolve assigned workspace '{}': {error}",
                workspace_path.display()
            ))
        })?;
        if checkpoint_path != requested_path {
            return Err(OrbitError::Execution(format!(
                "step recovery '{step_id}' assigned workspace '{}' does not match run '{run_id}' checkpoint '{}'",
                requested_path.display(),
                checkpoint_path.display()
            )));
        }

        let checkpoint_owner = worktree
            .get("job_run_id")
            .or_else(|| worktree.get("batch_id"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                OrbitError::Execution(format!(
                    "step recovery '{step_id}' run '{run_id}' has no worktree checkpoint owner"
                ))
            })?;
        if task_owner.as_deref() != Some(checkpoint_owner) {
            return Err(OrbitError::Execution(format!(
                "step recovery '{step_id}' task owner does not match worktree owner '{checkpoint_owner}'"
            )));
        }
        if !self.run_descends_from(run, checkpoint_owner)? {
            return Err(OrbitError::Execution(format!(
                "step recovery '{step_id}' worktree owner '{checkpoint_owner}' is outside run '{run_id}' retry lineage"
            )));
        }
        Ok(())
    }

    fn run_descends_from(
        &self,
        mut run: JobRun,
        expected_ancestor: &str,
    ) -> Result<bool, OrbitError> {
        let mut visited = std::collections::BTreeSet::new();
        loop {
            if run.run_id == expected_ancestor {
                return Ok(true);
            }
            if !visited.insert(run.run_id.clone()) {
                return Ok(false);
            }
            let Some(parent_id) = run.retry_source_run_id.as_deref() else {
                return Ok(false);
            };
            let Some(parent) = self.get_job_run_backend(parent_id)? else {
                return Ok(false);
            };
            run = parent;
        }
    }

    fn reserve_recovery(
        &self,
        run: &JobRun,
        admission: &OperationAdmission,
        kind: RecoveryEpisodeKind,
        step_id: Option<&str>,
    ) -> Result<StepRecoveryAdmission, OrbitError> {
        let grant = self.operation_grant(&admission.grant_id)?;
        if !grant.privileged_actions_allowed() {
            return Ok(StepRecoveryAdmission::Denied {
                reason: "grant_revoked".to_string(),
            });
        }
        if let Err(error) = captured_policy(&grant) {
            return Ok(StepRecoveryAdmission::Denied {
                reason: error.to_string(),
            });
        }
        let workspace_id = self.workspace_id()?;
        let store = self.operation_store()?;
        let budget = budget(admission);
        let mut reserved = None;
        for task_id in run_task_ids(run) {
            // A retried reservation for the same run/step reuses its episode.
            if let Some(ledger) = store.operation_recovery_ledger(&workspace_id, &task_id)?
                && let Some(episode) = open_episode(&ledger, &run.run_id, step_id)
            {
                reserved.get_or_insert(episode);
                continue;
            }
            let (outcome, _) = store.operation_recovery_reserve(
                &workspace_id,
                &RecoveryReserveRequest {
                    task_id: &task_id,
                    run_id: &run.run_id,
                    step_id,
                    kind,
                    budget,
                    now: Utc::now(),
                },
            )?;
            match outcome {
                RecoveryReservation::Reserved { episode, .. } => {
                    reserved.get_or_insert(episode);
                }
                RecoveryReservation::Exhausted {
                    reason,
                    episodes_consumed,
                    consumed_seconds,
                } => {
                    self.mark_recovery_exhausted(
                        &task_id,
                        &run.run_id,
                        reason,
                        episodes_consumed,
                        consumed_seconds,
                        &budget,
                    );
                    return Ok(StepRecoveryAdmission::Denied {
                        reason: reason.to_string(),
                    });
                }
            }
        }
        Ok(match reserved {
            Some(episode) => StepRecoveryAdmission::Reserved { episode },
            None => StepRecoveryAdmission::Allowed,
        })
    }

    /// Durable escalation: the task keeps its status, gains a history event
    /// naming the spent allowance, and is never re-marked for the same lineage.
    fn mark_recovery_exhausted(
        &self,
        task_id: &str,
        run_id: &str,
        reason: &str,
        episodes_consumed: u32,
        consumed_seconds: u64,
        budget: &RecoveryBudget,
    ) {
        let already_marked = self
            .get_task_history(task_id)
            .map(|history| {
                history
                    .iter()
                    .any(|entry| entry.event == RECOVERY_EXHAUSTED_EVENT)
            })
            .unwrap_or(false);
        if already_marked {
            return;
        }
        let note = format!(
            "operation-mode recovery allowance spent ({reason}): {episodes_consumed}/{} episodes, \
             {consumed_seconds}/{} seconds across step recovery and triage; latest run {run_id}. \
             Escalated for a decision; automation will not retry this lineage.",
            budget.episodes, budget.seconds
        );
        if let Err(error) = self.apply_task_automation_update(
            task_id,
            TaskAutomationUpdate {
                status_event: Some(RECOVERY_EXHAUSTED_EVENT.to_string()),
                status_note: Some(note),
                ..TaskAutomationUpdate::default()
            },
        ) {
            tracing::warn!(
                task_id,
                run_id,
                "recovery exhaustion escalation write failed: {error}"
            );
        }
    }

    /// Recheck the grant before the guarded `review -> done` transition.
    /// Unbound runs keep their captured `--complete` authority unchanged.
    pub(crate) fn authorize_task_completion(
        &self,
        run_id: &str,
        task_ids: &[String],
    ) -> Result<(), OrbitError> {
        let Some((_, admission)) = self.admitted_run(run_id)? else {
            return Ok(());
        };
        let decision = self.completion_decision(&admission, task_ids);
        self.record_pipeline_audit(
            COMPLETION_AUDIT,
            Some(run_id),
            Some("system"),
            if decision.is_ok() {
                AuditEventStatus::Success
            } else {
                AuditEventStatus::Failure
            },
            json!({
                "grant_id": admission.grant_id,
                "task_ids": task_ids,
                "outcome": if decision.is_ok() { "allowed" } else { "refused" },
                "recorded_at": Utc::now().to_rfc3339(),
            }),
            decision.as_ref().err().map(ToString::to_string),
        )?;
        decision
    }

    fn completion_decision(
        &self,
        admission: &OperationAdmission,
        task_ids: &[String],
    ) -> Result<(), OrbitError> {
        let grant = self.operation_grant(&admission.grant_id)?;
        captured_policy(&grant)?;
        if admission.completion != "done" {
            return Err(OrbitError::CapabilityDenied(format!(
                "completion refused: this run was admitted under operation grant '{}' with \
                 completion '{}'",
                grant.id, admission.completion
            )));
        }
        if !grant.completion_allowed() {
            return Err(OrbitError::CapabilityDenied(format!(
                "completion refused: operation grant '{}' {}",
                grant.id,
                if grant.privileged_actions_allowed() {
                    "carries no complete right"
                } else {
                    "was revoked"
                }
            )));
        }
        if let Some(outside) = task_ids.iter().find(|task_id| !grant.covers(task_id)) {
            return Err(OrbitError::CapabilityDenied(format!(
                "completion refused: task {outside} is outside operation grant '{}'",
                grant.id
            )));
        }
        Ok(())
    }
}

/// The still-open episode this run/step reserved, if any.
fn open_episode(ledger: &RecoveryLedger, run_id: &str, step_id: Option<&str>) -> Option<u32> {
    ledger
        .episodes
        .iter()
        .find(|episode| {
            episode.run_id == run_id
                && episode.step_id.as_deref() == step_id
                && episode.elapsed_seconds.is_none()
        })
        .map(|episode| episode.index)
}

/// Reserve (or reuse) a triage episode for a blocked task whose failed run
/// was admitted under a grant. `None` for unbound runs.
pub(crate) fn triage_recovery_reservation(
    runtime: &OrbitRuntime,
    task_id: &str,
    run: &JobRun,
) -> Result<Option<TriageReservation>, OrbitError> {
    let Some(admission) = run
        .input
        .as_ref()
        .map(OperationAdmission::from_run_input)
        .transpose()
        .map_err(OrbitError::InvalidInput)?
        .flatten()
    else {
        return Ok(None);
    };
    let bound_run = JobRun {
        input: Some(json!({ "task_ids": [task_id] })),
        ..run.clone()
    };
    Ok(Some(
        match runtime.reserve_recovery(&bound_run, &admission, RecoveryEpisodeKind::Triage, None)? {
            StepRecoveryAdmission::Reserved { episode } => TriageReservation {
                episode: Some(episode),
                exhausted: None,
            },
            StepRecoveryAdmission::Allowed => TriageReservation {
                episode: None,
                exhausted: None,
            },
            StepRecoveryAdmission::Denied { reason } => TriageReservation {
                episode: None,
                exhausted: Some(if reason == "recovery_minutes_exhausted" {
                    "recovery_minutes_exhausted"
                } else if reason == "grant_revoked" {
                    "grant_revoked"
                } else {
                    "recovery_episodes_exhausted"
                }),
            },
        },
    ))
}

/// Settle a triage episode once dispositions are applied.
pub(crate) fn settle_triage_episode(
    runtime: &OrbitRuntime,
    task_id: &str,
    run_id: &str,
) -> Result<(), OrbitError> {
    let workspace_id = runtime.workspace_id()?;
    let store = runtime.operation_store()?;
    let Some(ledger) = store.operation_recovery_ledger(&workspace_id, task_id)? else {
        return Ok(());
    };
    let now = Utc::now();
    let Some(episode) = ledger.episodes.iter().find(|episode| {
        episode.run_id == run_id && episode.step_id.is_none() && episode.elapsed_seconds.is_none()
    }) else {
        return Ok(());
    };
    let elapsed_seconds = (now - episode.reserved_at)
        .num_seconds()
        .try_into()
        .unwrap_or(0);
    store.operation_recovery_settle(&workspace_id, task_id, episode.index, elapsed_seconds, now)?;
    Ok(())
}
