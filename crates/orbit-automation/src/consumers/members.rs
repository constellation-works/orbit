//! State consumers: host facts for the shared state-trigger evaluator.

use super::{consumer_key, preparation, preparation::InstructionSnapshot};
use crate::host::{AutomationHost, RunOwnerLiveness};
use crate::source::Source;
use crate::{
    AutomationError, automation_error_to_orbit,
    delivery::definition_epoch,
    members::{self, MemberAdmission, MemberEvaluation, MemberHost, MemberOutcome, MemberPage},
};
use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::TaskListFilter;
use orbit_types::{
    task::TaskStatus,
    workflow::{
        JobRunState, RoutineDefinition,
        automation::{members::*, *},
    },
};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::BTreeMap;

pub(crate) fn evaluate<H: AutomationHost>(
    host: &H,
    definition: &RoutineDefinition,
    dry_run: bool,
    now: DateTime<Utc>,
) -> Result<AutomationDiagnostic, OrbitError> {
    let trigger = definition
        .trigger
        .state
        .as_ref()
        .ok_or_else(|| OrbitError::InvalidInput("not a state routine".into()))?;

    let consumer = consumer_key(host, "routine", &definition.name)?;

    // Timing edits apply to pending work; the active attempt retains its budget.
    let epoch = definition_epoch(&(
        &trigger.kind,
        &trigger.owner_machine,
        &trigger.branch,
        &definition.target,
    ))
    .map_err(automation_error_to_orbit)?;

    let owned = Some(trigger.owner_machine.as_str()) == host.machine_identity();

    let mut effective = trigger.clone();
    effective.retries = effective.retries.min(definition.policy.retries.max);

    // [ORB-11332] Operation mode supplies constraints to this evaluation; it
    // never owns a cadence of its own. Empty constraints are the pre-existing
    // behavior.
    let constraints = host.member_constraints(&effective)?;

    members::evaluate(
        host.automation_store()?.as_ref(),
        &StateConsumer::new(host, &effective),
        MemberEvaluation {
            consumer: &consumer,
            epoch: &epoch,
            trigger: &effective,
            enabled: definition.enabled && owned,
            dry_run,
            now,
            constraints,
        },
    )
    .map_err(automation_error_to_orbit)
}

/// One state-trigger evaluation's view of the host: the facts the shared
/// evaluator asks for, plus the per-pass caches that keep repeated incident and
/// instruction reads bounded.
pub struct StateConsumer<'a, H> {
    host: &'a H,
    trigger: &'a StateTrigger,
    incidents: RefCell<super::incidents::IncidentSession>,
    instructions: RefCell<BTreeMap<String, InstructionSnapshot>>,
}

impl<'a, H: AutomationHost> StateConsumer<'a, H> {
    /// Bind one evaluation to `host` and `trigger`, with empty caches.
    pub fn new(host: &'a H, trigger: &'a StateTrigger) -> Self {
        Self {
            host,
            trigger,
            incidents: RefCell::new(super::incidents::IncidentSession::new()),
            instructions: RefCell::new(BTreeMap::new()),
        }
    }

    fn fingerprint(
        &self,
        task: &orbit_types::task::Task,
        revision: &str,
    ) -> Result<String, AutomationError> {
        let instructions = {
            let mut cached = self.instructions.borrow_mut();
            match cached.get(revision) {
                Some(snapshot) => snapshot.clone(),
                None => {
                    let snapshot = preparation::instructions(self.host, revision)?;
                    cached.insert(revision.to_string(), snapshot.clone());
                    snapshot
                }
            }
        };

        preparation::fingerprint_with_instructions(self.host, task, revision, &instructions)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn incident_work_stats(&self) -> super::incidents::IncidentWorkStats {
        self.incidents.borrow().stats()
    }
}

impl<H: AutomationHost> MemberHost for StateConsumer<'_, H> {
    fn head(&self, branch: &str) -> Result<(String, SourceRevision), AutomationError> {
        Source::new(self.host.repo_root()).head(branch)
    }

    fn observe(
        &self,
        after: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<MemberPage, AutomationError> {
        let scan_before = after
            .map(serde_json::from_str)
            .transpose()
            .map_err(|e| AutomationError::Evidence(format!("invalid task continuation: {e}")))?;

        let tasks = self.host.task_candidates(
            &TaskListFilter {
                scan_before,
                statuses: Some(match self.trigger.kind {
                    StateTriggerKind::PreparationEligible => {
                        vec![TaskStatus::Proposed, TaskStatus::Backlog]
                    }
                    StateTriggerKind::ExecutionFailed => vec![TaskStatus::Blocked],
                }),
                ..Default::default()
            },
            50,
        )?;

        let next = if tasks.total > tasks.items.len() {
            tasks
                .items
                .last()
                .map(|last| serde_json::to_string(&(last.created_at, &last.id)))
                .transpose()
                .map_err(|e| AutomationError::Evidence(e.to_string()))?
        } else {
            None
        };

        let (_, source) = self.head(&self.trigger.branch)?;

        let mut candidates = Vec::new();
        let mut incident_inventory = None;
        let mut withheld = BTreeMap::new();

        for envelope in tasks.items {
            let task = self.host.get_task(&envelope.id)?;

            match self.trigger.kind {
                StateTriggerKind::PreparationEligible => {
                    if !crate::members::preparation::eligible(&task) {
                        withheld.insert(task.id, "task_ineligible".into());
                        continue;
                    }
                    let fingerprint = match self.fingerprint(&task, &source.commit) {
                        Ok(fingerprint) => fingerprint,
                        Err(error) => {
                            withheld.insert(task.id, error.to_string());
                            continue;
                        }
                    };
                    candidates.push(StateMember {
                        key: task.id.clone(),
                        task_ids: vec![task.id.clone()],
                        fingerprint,
                        source: source.clone(),
                        evidence: json!({"task_id":task.id}),
                        first_seen: now,
                        changed_at: now,
                    });
                }
                StateTriggerKind::ExecutionFailed => {
                    match super::incidents::observe(self.host, &task) {
                        Ok((key, evidence)) => {
                            if candidates
                                .iter()
                                .any(|other: &StateMember| other.key == key)
                            {
                                continue;
                            }

                            // The full cohort is hydrated at most once per page
                            // and reused across later admissions in this consumer
                            // after a freshness check.
                            if incident_inventory.is_none() {
                                incident_inventory =
                                    Some(self.incidents.borrow_mut().inventory(self.host)?);
                            }

                            let task_ids = incident_inventory
                                .as_ref()
                                .and_then(|inventory| inventory.get(&key))
                                .cloned()
                                .unwrap_or_default();

                            if task_ids.len() > 50 {
                                withheld.insert(task.id, "incident_member_budget".into());
                                continue;
                            }

                            if task_ids.is_empty() {
                                withheld.insert(task.id, "incident_recovery_pending".into());
                                continue;
                            }

                            candidates.push(StateMember {
                                fingerprint: key.clone(),
                                key,
                                task_ids,
                                source: source.clone(),
                                evidence,
                                first_seen: now,
                                changed_at: now,
                            });
                        }
                        Err(error) => {
                            withheld.insert(task.id, error.to_string());
                        }
                    }
                }
            }
        }

        Ok(MemberPage {
            candidates,
            withheld,
            next,
        })
    }

    fn admission(&self, member: &StateMember) -> Result<MemberAdmission, AutomationError> {
        if self.trigger.kind == StateTriggerKind::ExecutionFailed
            && self
                .incidents
                .borrow_mut()
                .inventory(self.host)?
                .get(&member.key)
                != Some(&member.task_ids)
        {
            return Ok(MemberAdmission::Retire(
                "incident_membership_or_recovery_changed".into(),
            ));
        }

        // Re-derive the material now: a member whose input moved may not be admitted.
        for id in &member.task_ids {
            let task = self.host.get_task(id)?;
            let current = match self.trigger.kind {
                StateTriggerKind::PreparationEligible => {
                    if !crate::members::preparation::eligible(&task) {
                        return Ok(MemberAdmission::Retire("task_ineligible".into()));
                    }
                    let (_, source) = self.head(&self.trigger.branch)?;
                    self.fingerprint(&task, &source.commit)?
                }
                StateTriggerKind::ExecutionFailed => {
                    match super::incidents::observe(self.host, &task) {
                        Ok((key, _)) => key,
                        Err(error) => {
                            return Ok(MemberAdmission::Retire(error.to_string()));
                        }
                    }
                }
            };

            if current != member.fingerprint {
                return Ok(MemberAdmission::Retire("material_changed".into()));
            }
        }

        Ok(MemberAdmission::Admit)
    }

    fn lookup(&self, attempt: &MemberAttempt) -> Result<Option<String>, AutomationError> {
        self.host
            .automation_job_for_key(&attempt.action_key)
            .map_err(Into::into)
    }

    fn admit(&self, attempt: &MemberAttempt) -> Result<String, AutomationError> {
        self.host.ensure_coordination_task_write_permitted()?;

        let state = self
            .host
            .automation_store()?
            .automation_state(&attempt.consumer)?
            .ok_or_else(|| AutomationError::Deferred("claim_missing".into()))?;
        if state
            .members
            .as_ref()
            .and_then(|members| members.active.as_ref())
            != Some(attempt)
        {
            return Err(AutomationError::Deferred("claim_superseded".into()));
        }

        // Pin the source the attempt froze so the run can still reach it later.
        Source::new(self.host.repo_root()).git(&[
            "update-ref",
            &format!("refs/orbit/automation/{}", attempt.id),
            &attempt.member.source.commit,
        ])?;

        let origin = if self.trigger.kind == StateTriggerKind::ExecutionFailed {
            "triage"
        } else {
            "preparation"
        };

        self.host
            .submit_automation_run(
                self.trigger.job_name(),
                json!({
                    "state_automation": attempt,
                    "task_ids": attempt.member.task_ids,
                    "source_revision": attempt.member.source.commit,
                    "base_branch": self.trigger.branch,
                    "max_tasks": attempt.member.task_ids.len(),
                    "promotion_authorized": false,
                    "automation_origin": origin,
                }),
                &attempt.action_key,
            )
            .map_err(Into::into)
    }

    fn outcome(&self, attempt: &MemberAttempt) -> Result<MemberOutcome, AutomationError> {
        let Some(id) = attempt.action_id.as_deref() else {
            return Ok(MemberOutcome::Pending);
        };

        let run = self.host.show_job_run(id)?;
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

        if let Some(state) = self.host.read_run_state(id)? {
            // Only the exact canonical deterministic apply step can provide this
            // record. Agent prose and unrelated output keys are never searched.
            let index = 2;
            if state.step_states.get(&index) == Some(&JobRunState::Success)
                && let Some(result) = state
                    .step_outputs
                    .get(&index)
                    .and_then(|v| v.get("member_evidence"))
            {
                let mut evidence: MemberEvidence = serde_json::from_value(result.clone())
                    .map_err(|e| AutomationError::Evidence(e.to_string()))?;
                evidence.action_id = id.into();
                return Ok(MemberOutcome::Applied(evidence));
            }
        }

        if run.state.is_terminal()
            && self.host.run_owner_liveness(&run) == RunOwnerLiveness::Stopped
        {
            return Ok(MemberOutcome::Failed(
                "stopped_without_member_evidence".into(),
            ));
        }

        Ok(MemberOutcome::Pending)
    }
}

/// Recheck the server-issued claim at the deterministic prepare/apply boundary.
pub fn claim<H: AutomationHost>(
    host: &H,
    value: &Value,
) -> Result<Option<MemberAttempt>, OrbitError> {
    let Some(value) = value
        .get("state_automation")
        .filter(|claim| !claim.is_null())
    else {
        return Ok(None);
    };

    let submitted: MemberAttempt = serde_json::from_value(value.clone())
        .map_err(|e| OrbitError::InvalidInput(e.to_string()))?;

    let state = host
        .automation_store()?
        .automation_state(&submitted.consumer)?
        .ok_or_else(|| OrbitError::InvalidInput("state claim missing".into()))?;

    // Preparation material is derived from the branch head, so a moved head
    // invalidates the claim; incident material is not tied to the head.
    if submitted.kind == StateTriggerKind::PreparationEligible
        && Source::new(host.repo_root())
            .head(&state.branch)
            .map_err(automation_error_to_orbit)?
            .1
            != submitted.member.source
    {
        return Err(OrbitError::InvalidInput(
            "state-trigger source changed".into(),
        ));
    }

    let active = state
        .members
        .and_then(|members| members.active)
        .ok_or_else(|| OrbitError::InvalidInput("state claim missing".into()))?;

    if active.kind != submitted.kind
        || active.id != submitted.id
        || active.member != submitted.member
        || active.action_key != submitted.action_key
        || active.attempt != submitted.attempt
        || active.exhausted
        || Utc::now() >= active.deadline
    {
        return Err(OrbitError::InvalidInput(
            "state claim stale or expired".into(),
        ));
    }

    Ok(Some(active))
}
