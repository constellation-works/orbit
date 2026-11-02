//! The resident cycle's deterministic steps: `resident_observe` takes the
//! cycle lease, recovers a dead predecessor, records the cycle's grant and
//! builds the brief; `resident_finalize` closes the cycle, synthesizing an
//! `incomplete` checkpoint when the agent wrote none.

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{TaskComment, TaskHistoryEntry};
use orbit_types::workflow::{
    RESIDENT_BRIEF_CHECKPOINTS, RESIDENT_CYCLE_GRANTED_EVENT, RESIDENT_CYCLE_JOB,
    ResidentCheckpoint, ResidentCheckpointAction, ResidentCheckpointAuthority,
    ResidentCheckpointWake, ResidentComment, ResidentCycleGrant, ResidentLiveCycle,
    ResidentNextWake,
};
use serde_json::{Value, json};

use super::due::{checkpoints, cycle_grant, started_cycles};
use super::guard::action_rows;
use super::{ResidentMandateState, cycle::live_cycle_from_history};
use crate::OrbitRuntime;
use crate::application::task::{TaskRecordUpdateParams, TaskRecords};

/// How long a cycle's lease lasts: the agent step's budget plus margin.
pub const RESIDENT_CYCLE_LEASE_MINUTES: i64 = 40;

/// The cycle id a run takes.
pub fn resident_cycle_id(run_id: &str) -> String {
    format!("rcy-{run_id}")
}

fn skipped(task_id: &str, reason: &str) -> Value {
    json!({"task_id": task_id, "cycle_count": 0, "skip_reason": reason})
}

impl OrbitRuntime {
    /// The mandate a resident cycle run serves: the single task id the
    /// scheduler put in its input. Only a `resident_cycle_pipeline` run has one.
    pub(crate) fn resident_cycle_mandate_of_run(&self, run_id: &str) -> Result<String, OrbitError> {
        let run = self.show_job_run_observed(run_id)?;
        if run.job_id != RESIDENT_CYCLE_JOB {
            return Err(OrbitError::PolicyDenied(format!(
                "run {run_id} is not a {RESIDENT_CYCLE_JOB} run"
            )));
        }
        let ids: Vec<String> = run
            .input
            .as_ref()
            .and_then(|input| input.get("task_ids"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        match ids.as_slice() {
            [id] => Ok(id.clone()),
            _ => Err(OrbitError::InvalidInput(
                "a resident cycle run names exactly one mandate in task_ids".into(),
            )),
        }
    }

    /// `resident_observe`: start this run's cycle for its mandate, or report
    /// why it does not (`cycle_count: 0`). A dead predecessor first gets its
    /// synthesized `incomplete` checkpoint, and this cycle recovers it.
    pub fn begin_resident_cycle(
        &self,
        run_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Value, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let task_id = self.resident_cycle_mandate_of_run(run_id)?;
        let task = self.get_task(&task_id)?;
        let history = self.get_task_history(&task_id)?;
        let view = self.resident_view(task.clone(), &history)?;
        if view.state().0 != ResidentMandateState::Active {
            return Ok(skipped(&task_id, "mandate_not_active"));
        }
        let Ok(mandate) = view.mandate.clone() else {
            return Ok(skipped(&task_id, "mandate_invalid"));
        };
        let effective = view.effective();
        let cycle = resident_cycle_id(run_id);
        let live = live_cycle_from_history(&history)?;
        // A resumed run re-enters the cycle it already holds.
        if let Some(live) = &live
            && live.run_id == run_id
            && let Some(grant) = cycle_grant(&history, &live.cycle)
        {
            return self.resident_brief(&task_id, &mandate, &grant, &history);
        }
        let due = self.resident_due(&task, mandate.clone(), &history, now)?;
        if let Some(reason) = &due.withheld {
            return Ok(skipped(&task_id, reason));
        }
        if due.reasons.is_empty() {
            return Ok(skipped(&task_id, "not_due"));
        }
        let recovery_of = match &due.dead_cycle {
            Some(dead) => {
                self.synthesize_resident_checkpoint(
                    &task_id,
                    dead,
                    "the cycle died before it wrote a checkpoint (process killed or lease expired)",
                    None,
                )?;
                Some(dead.cycle.clone())
            }
            None => None,
        };
        self.set_resident_live_cycle(
            &task_id,
            live.as_ref().map(|live| live.cycle.as_str()),
            Some(&ResidentLiveCycle {
                cycle: cycle.clone(),
                run_id: run_id.to_string(),
                expires_at: now + Duration::minutes(RESIDENT_CYCLE_LEASE_MINUTES),
            }),
        )?;
        let deadline_passed = mandate.deadline.is_some_and(|deadline| deadline <= now);
        let narrowed_by = if let Some(notice) = due.maintenance.first() {
            Some(format!("notice:{notice}"))
        } else if deadline_passed {
            Some("deadline".to_string())
        } else {
            None
        };
        let grant = ResidentCycleGrant {
            cycle,
            run_id: run_id.to_string(),
            rights: if narrowed_by.is_some() {
                Vec::new()
            } else {
                effective.rights.clone()
            },
            workspaces: effective.workspaces.clone(),
            observe_only: narrowed_by.is_some(),
            narrowed_by,
            recovery_of,
            digest: due.digest.clone(),
            reasons: due.reasons.clone(),
        };
        self.append_resident_history(
            &task_id,
            RESIDENT_CYCLE_GRANTED_EVENT,
            serde_json::to_string(&grant)
                .map_err(|error| OrbitError::Execution(format!("serialize grant: {error}")))?,
        )?;
        let history = self.get_task_history(&task_id)?;
        let mut brief = self.resident_brief(&task_id, &mandate, &grant, &history)?;
        brief["notices"] = json!(
            due.notices
                .iter()
                .map(|notice| json!({
                    "id": notice.id,
                    "kind": notice.kind,
                    "status": notice.status,
                    "workspace": notice.workspace.name,
                    "untrusted_title": notice.title,
                    "untrusted_body": notice.body,
                }))
                .collect::<Vec<_>>()
        );
        Ok(brief)
    }

    /// The brief a cycle's agent reads, in the checkpoint contract's order.
    fn resident_brief(
        &self,
        task_id: &str,
        mandate: &orbit_types::workflow::ResidentMandate,
        grant: &ResidentCycleGrant,
        history: &[TaskHistoryEntry],
    ) -> Result<Value, OrbitError> {
        let comments = self.get_task_comments(task_id)?;
        let all = checkpoints(task_id, &comments);
        let since = started_cycles(history)
            .iter()
            .rev()
            .find(|started| started.live.cycle != grant.cycle)
            .map(|started| started.at);
        let recent: Vec<&ResidentCheckpoint> =
            all.iter().rev().take(RESIDENT_BRIEF_CHECKPOINTS).collect();
        let messages: Vec<Value> = comments
            .iter()
            .filter(|comment| since.is_none_or(|since| comment.at >= since))
            .filter(|comment| {
                !matches!(
                    ResidentComment::parse(&comment.message),
                    Ok(ResidentComment::Checkpoint(_) | ResidentComment::Request(_))
                )
            })
            .map(|comment| json!({"at": comment.at, "by": comment.by, "message": comment.message}))
            .collect();
        let recovered = grant
            .recovery_of
            .as_deref()
            .map(|dead| action_rows(history, dead))
            .unwrap_or_default();
        Ok(json!({
            "task_id": task_id,
            "cycle_count": 1,
            "cycle": grant.cycle,
            "crew": mandate.crew,
            "action_key_prefix": format!("resident:{task_id}:{}:", grant.cycle),
            "mandate": mandate,
            "authority": grant,
            "checkpoints": recent,
            "open_requests": self
                .get_task_decision_requests(task_id)?
                .into_iter()
                .filter(|request| request.state == orbit_types::task::DecisionRequestState::Open)
                .map(|request| crate::application::task::decision_request_json(&request))
                .collect::<Vec<_>>(),
            "comments_since_last_cycle": messages,
            "wake": {"reasons": grant.reasons, "digest": grant.digest},
            "notices": [],
            "recovery_of": grant.recovery_of,
            "recovered_actions": recovered,
        }))
    }

    /// `resident_finalize`: close this run's cycle. A cycle that wrote no
    /// checkpoint gets a synthesized `incomplete` one first. A run that holds
    /// no live cycle (it skipped, or a newer cycle replaced it) changes nothing.
    pub fn finish_resident_cycle(&self, run_id: &str) -> Result<Value, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let task_id = self.resident_cycle_mandate_of_run(run_id)?;
        let history = self.get_task_history(&task_id)?;
        let Some(live) = live_cycle_from_history(&history)?.filter(|live| live.run_id == run_id)
        else {
            return Ok(json!({"task_id": task_id, "finalized": false}));
        };
        let synthesized = self.synthesize_resident_checkpoint(
            &task_id,
            &live,
            "the cycle's agent ended without a valid checkpoint",
            None,
        )?;
        self.set_resident_live_cycle(&task_id, Some(&live.cycle), None)?;
        Ok(json!({
            "task_id": task_id,
            "finalized": true,
            "cycle": live.cycle,
            "synthesized": synthesized,
        }))
    }

    /// Write the `incomplete` checkpoint for `cycle` unless it already has
    /// one. Trusted: the cycle is no longer live, so this bypasses the
    /// live-cycle admission ordinary checkpoint writes need. Returns whether
    /// it wrote one.
    pub(crate) fn synthesize_resident_checkpoint(
        &self,
        task_id: &str,
        cycle: &ResidentLiveCycle,
        error: &str,
        now: Option<DateTime<Utc>>,
    ) -> Result<bool, OrbitError> {
        let now = now.unwrap_or_else(Utc::now);
        let mut wrote = false;
        self.stores()
            .tasks()
            .with_task_write_lock(task_id, &mut || {
                let comments = self.get_task_comments(task_id)?;
                if checkpoints(task_id, &comments)
                    .iter()
                    .any(|checkpoint| checkpoint.cycle == cycle.cycle)
                {
                    return Ok(());
                }
                let history = self.get_task_history(task_id)?;
                let grant = cycle_grant(&history, &cycle.cycle);
                let mandate = super::resident_mandate_from_artifact(self, task_id)?
                    .map_err(OrbitError::InvalidInput)?;
                let actions = action_rows(&history, &cycle.cycle)
                    .into_iter()
                    .take(50)
                    .map(|row| ResidentCheckpointAction {
                        kind: row.tool,
                        subject: row.subject.unwrap_or_else(|| task_id.to_string()),
                        // A call that started and never reported is in doubt:
                        // recorded failed, and refused if retried.
                        result: row.phase.result().unwrap_or("failed").to_string(),
                        reference: row.reference,
                        key: row.key,
                    })
                    .collect();
                let digest = grant.as_ref().map_or_else(
                    || format!("sha256:{}", "0".repeat(64)),
                    |grant| grant.digest.clone(),
                );
                let record = ResidentCheckpoint {
                    schema: 1,
                    mandate: task_id.to_string(),
                    cycle: cycle.cycle.clone(),
                    run_id: cycle.run_id.clone(),
                    crew: mandate.crew.clone(),
                    recovery_of: grant.as_ref().and_then(|grant| grant.recovery_of.clone()),
                    wake: ResidentCheckpointWake {
                        reasons: grant
                            .as_ref()
                            .map(|grant| grant.reasons.clone())
                            .filter(|reasons| !reasons.is_empty())
                            .unwrap_or_else(|| vec!["unknown".into()]),
                        digest,
                    },
                    authority: ResidentCheckpointAuthority {
                        rights: grant.as_ref().map(|g| g.rights.clone()).unwrap_or_default(),
                        observe_only: grant.as_ref().is_none_or(|g| g.observe_only),
                        narrowed_by: grant.as_ref().and_then(|g| g.narrowed_by.clone()),
                    },
                    actions,
                    decisions: Vec::new(),
                    requests: Vec::new(),
                    costs: Vec::new(),
                    notes: String::new(),
                    next_wake: ResidentNextWake {
                        reason: "recovery".into(),
                        not_before: now,
                    },
                    continuity: "off".into(),
                    outcome: "incomplete".into(),
                    probe: mandate.probe,
                    error: Some(error.to_string()),
                    conversation_ref: None,
                };
                let message = serde_json::to_string(&ResidentComment::Checkpoint(Box::new(record)))
                    .map_err(|error| {
                        OrbitError::Execution(format!("serialize checkpoint: {error}"))
                    })?;
                ResidentComment::parse(&message)
                    .map_err(|error| OrbitError::Execution(error.to_string()))?;
                self.stores().task_records().update(
                    task_id,
                    TaskRecordUpdateParams {
                        actor: "system".into(),
                        append_comments: vec![TaskComment {
                            at: now,
                            by: "system".into(),
                            message,
                            operation_id: None,
                        }],
                        ..Default::default()
                    },
                )?;
                wrote = true;
                Ok(())
            })?;
        Ok(wrote)
    }

    /// Append one system history entry to the assignment.
    pub(crate) fn append_resident_history(
        &self,
        task_id: &str,
        event: &str,
        note: String,
    ) -> Result<(), OrbitError> {
        self.stores().task_records().update(
            task_id,
            TaskRecordUpdateParams {
                actor: "system".into(),
                append_history: vec![TaskHistoryEntry {
                    at: Utc::now(),
                    by: "system".into(),
                    event: event.into(),
                    note: Some(note),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                }],
                ..Default::default()
            },
        )?;
        Ok(())
    }
}
