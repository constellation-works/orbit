use std::collections::BTreeMap;

use orbit_automation::AutomationError;
use orbit_automation::members::MemberOutcome;
use orbit_types::workflow::automation::members::{MemberAttempt, MemberBatchEvidence};
use serde_json::Value;

use super::super::{Host, stale_tasks};
use super::pilot_steps::{PilotStep, PilotSteps, checkpoint, is_apply_output, member_evidence};
use crate::application::contracts::SUPERSEDED_BY_SOURCE;

impl Host<'_> {
    pub(super) fn outcome_for(
        &self,
        attempt: &MemberAttempt,
    ) -> Result<MemberOutcome, AutomationError> {
        let Some(id) = attempt.action_id.as_deref() else {
            return Ok(MemberOutcome::Pending);
        };

        let run = self.runtime.show_job_run(id)?;
        let expected =
            serde_json::to_value(attempt).map_err(|e| AutomationError::Evidence(e.to_string()))?;

        if run
            .input
            .as_ref()
            .and_then(|input| input.get("state_automation"))
            .and_then(|automation| automation.get("action_key"))
            != expected.get("action_key")
        {
            return Err(AutomationError::Evidence("job_input_mismatch".into()));
        }

        let stopped = run.state.is_terminal()
            && crate::application::job::run_owner_liveness(&run)
                == crate::application::job::RunOwnerLiveness::Stopped;

        // The triager records each decision itself; its durable marker, not
        // the agent's final message, settles the member.
        if self.is_escalation_triage() {
            return self.escalation_outcome(attempt, id, stopped);
        }

        if self.trigger.kind
            == orbit_types::workflow::automation::members::StateTriggerKind::DecisionRequested
        {
            return self.decision_outcome(attempt, id, stopped, run.state);
        }

        // Only the exact canonical deterministic apply steps can provide this
        // record. Agent prose and unrelated output keys are never searched.
        let steps = PilotSteps::of(self.runtime, &run, self.trigger.kind)?;
        let state = self.runtime.read_run_state(id)?;
        let checkpoint = |step: Option<PilotStep>| checkpoint(state.as_ref(), step);
        let Some(initial) = checkpoint(steps.apply).filter(|output| is_apply_output(output)) else {
            if !stopped {
                return Ok(MemberOutcome::Pending);
            }
            // A retry would replay the frozen source; once the branch moved
            // under any member's material, no member was applied and every
            // one is better claimed afresh at the head [ORB-14476].
            let prepared = checkpoint(steps.prepare).unwrap_or(&Value::Null);
            let stale = stale_tasks(
                self.runtime,
                attempt,
                &self.policy,
                prepared,
                &attempt.task_ids(),
                &BTreeMap::new(),
            )
            .unwrap_or_else(|error| {
                tracing::warn!(
                    routine = self.routine,
                    attempt = attempt.id,
                    error = %error,
                    "could not compare the stopped attempt's source with the branch head; \
                     it retries as an ordinary failure"
                );
                BTreeMap::new()
            });
            if stale.is_empty() {
                return Ok(MemberOutcome::Failed(
                    "stopped_without_member_evidence".into(),
                ));
            }
            let superseded = attempt
                .members()
                .iter()
                .map(|member| {
                    let reason = member
                        .task_ids
                        .iter()
                        .find_map(|id| stale.get(id))
                        .map_or_else(
                            || format!("{SUPERSEDED_BY_SOURCE}: a sibling's source moved"),
                            |detail| format!("{SUPERSEDED_BY_SOURCE}: {detail}"),
                        );
                    (member.key.clone(), reason)
                })
                .collect();
            return Ok(MemberOutcome::Settled(MemberBatchEvidence {
                action_id: id.into(),
                attempt_id: attempt.id.clone(),
                applied: Vec::new(),
                failed: BTreeMap::new(),
                superseded,
            }));
        };

        // A member whose partition needed repair settles with the repair apply,
        // or as failed once the run stopped without reaching it [ORB-12746].
        let repairs_requested = initial
            .get(steps.follow_up_count)
            .and_then(Value::as_u64)
            .unwrap_or(0)
            != 0;
        let repair = checkpoint(steps.repair_apply).filter(|output| is_apply_output(output));
        if repairs_requested && repair.is_none() && !stopped {
            return Ok(MemberOutcome::Pending);
        }

        let mut applied = Vec::new();
        for output in [Some(initial), repair].into_iter().flatten() {
            let mut evidence = member_evidence(output)?;
            for entry in &mut evidence {
                entry.action_id = id.into();
            }
            applied.extend(evidence);
        }

        let latest_outcomes = repair.unwrap_or(initial);
        let mut failed = BTreeMap::new();
        let mut superseded = BTreeMap::new();
        for member in attempt.members() {
            if applied.iter().any(|entry| entry.member_key == member.key) {
                continue;
            }
            let outcome = latest_outcomes
                .get("task_outcomes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|outcome| {
                    outcome
                        .get("task_id")
                        .and_then(Value::as_str)
                        .is_some_and(|task_id| member.task_ids.iter().any(|id| id == task_id))
                });
            // The deterministic apply also supersedes durable task edits and
            // ownership changes. Release those members for fresh observation,
            // just as for a source move, without recording a failed pilot.
            if let Some(outcome) = outcome.filter(|outcome| outcome["outcome"] == "superseded") {
                let reason = outcome
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("superseded");
                let detail = outcome
                    .get("detail")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                superseded.insert(member.key.clone(), format!("{reason}: {detail}"));
                continue;
            }
            let reason = outcome
                .and_then(|outcome| {
                    let classification = outcome
                        .get("reason")
                        .and_then(Value::as_str)
                        .or_else(|| outcome.get("outcome").and_then(Value::as_str))?;
                    let detail = outcome
                        .get("error")
                        .or_else(|| outcome.get("detail"))
                        .and_then(Value::as_str)
                        .map(|detail| format!(": {detail}"))
                        .unwrap_or_default();
                    Some(format!("{classification}{detail}"))
                })
                .unwrap_or_else(|| {
                    if repairs_requested && repair.is_none() {
                        "stopped_before_repair_apply".into()
                    } else {
                        "no_member_evidence".into()
                    }
                });
            failed.insert(member.key.clone(), reason);
        }

        Ok(MemberOutcome::Settled(MemberBatchEvidence {
            action_id: id.into(),
            attempt_id: attempt.id.clone(),
            applied,
            failed,
            superseded,
        }))
    }
}
