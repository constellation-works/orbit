use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use orbit_automation::members::{MemberAdmission, MemberHost, MemberOutcome, MemberPage};
use orbit_automation::{AutomationError, DeferredReason};
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::{Task, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::JobRunTrigger;
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::automation::members::{
    MemberAssessment, MemberAttempt, MemberBatchEvidence, MemberEvidence, StateMember,
    StateTriggerKind, bundle_crew,
};
use serde_json::json;

use super::super::super::pins;
use super::super::Host;
use crate::OrbitRuntime;
use crate::application::task::blocked_recovery::block_triaged_at;
use crate::application::task::escalation_triage_steps::{PilotWarningCandidacy, triaged_at};
use crate::application::task::{TriageCandidacy, VERIFIABLE_STATUSES};

impl MemberHost for Host<'_> {
    fn head(&self, branch: &str) -> Result<(String, SourceRevision), AutomationError> {
        if let Some(head) = self.head.borrow().as_ref() {
            return Ok(head.clone());
        }
        // Pilot preparation and finding verification best-effort fetch;
        // incident consumers retain their local-only source contract.
        let head = if self.trigger.kind != StateTriggerKind::ExecutionFailed {
            self.source().preparation_head(branch)?
        } else {
            self.source().local_head(branch)?
        };
        *self.head.borrow_mut() = Some(head.clone());
        Ok(head)
    }

    fn observe(
        &self,
        after: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<MemberPage, AutomationError> {
        self.observe_page(after, now)
    }

    fn admission(&self, member: &StateMember) -> Result<MemberAdmission, AutomationError> {
        self.admission_for(member)
    }

    fn observable(&self, keys: &BTreeSet<String>) -> Result<BTreeSet<String>, AutomationError> {
        // Hidden task reads say nothing about membership; retire nothing.
        if !self.runtime.coordination_task_reads_visible() {
            return Ok(keys.clone());
        }

        // Task-keyed entries stay while their task holds a status `observe`
        // queries; an incident key stays while the current inventory has it.
        let statuses = self.observed_statuses();
        let indexed = self.runtime.task_status_index_for(keys)?;
        let incidents = match self.trigger.kind {
            StateTriggerKind::ExecutionFailed => {
                Some(self.incidents.borrow_mut().inventory(self.runtime)?)
            }
            StateTriggerKind::PreparationEligible
            | StateTriggerKind::FindingUnverified
            | StateTriggerKind::PilotWarningsHeld
            | StateTriggerKind::BlockEscalated
            | StateTriggerKind::DecisionRequested => None,
        };

        Ok(keys
            .iter()
            .filter(|key| {
                indexed
                    .get(*key)
                    .is_some_and(|status| statuses.contains(status))
                    || incidents
                        .as_ref()
                        .is_some_and(|inventory| inventory.contains_key(*key))
            })
            .cloned()
            .collect())
    }

    fn lookup(&self, attempt: &MemberAttempt) -> Result<Option<String>, AutomationError> {
        self.runtime
            .stores()
            .jobs()
            .automation_job_for_key(&attempt.action_key)
            .map_err(Into::into)
    }

    fn admit(&self, attempt: &MemberAttempt) -> Result<String, AutomationError> {
        self.runtime.ensure_coordination_task_write_permitted()?;

        let state = self
            .runtime
            .automation_store()?
            .automation_state(&attempt.consumer)?
            .ok_or_else(|| AutomationError::Deferred(DeferredReason::ClaimMissing))?;
        if state
            .members
            .as_ref()
            .and_then(|members| members.active.as_ref())
            != Some(attempt)
        {
            return Err(AutomationError::Deferred(DeferredReason::ClaimSuperseded));
        }

        // Pin the source the attempt froze so the run can still reach it later.
        pins::pin(&self.source(), &self.owner()?, attempt)?;

        let origin = match self.trigger.kind {
            StateTriggerKind::PreparationEligible => "preparation",
            StateTriggerKind::ExecutionFailed => "execution_failure",
            StateTriggerKind::FindingUnverified => "finding_verification",
            StateTriggerKind::PilotWarningsHeld | StateTriggerKind::BlockEscalated => {
                "escalation_triage"
            }
            StateTriggerKind::DecisionRequested => "decision_request",
        };

        // Every batch member travels as an explicit task id. Finding
        // verification grouped the attempt by stored crew [ORB-12761].
        // Task-pilot preparation may carry several crews; prepare partitions
        // those ids by live `task.crew` and then by `max_partition_size`
        // [ORB-12746, ORB-16839].
        let task_ids = attempt.task_ids();
        let mut input = json!({
            "state_automation": attempt,
            "task_ids": task_ids,
            "source_revision": attempt.member.source.commit,
            "base_branch": self.trigger.branch,
            "max_tasks": task_ids.len(),
            "promotion_authorized": false,
            "automation_origin": origin,
        });
        // One escalation job reads both subjects; the trigger names which.
        if let Some(subject) = self.escalation_subject() {
            input["subject"] = json!(subject);
        }
        self.runtime
            .submit_automation_pipeline_run(
                self.trigger.job_name(),
                input,
                &attempt.action_key,
                JobRunTrigger::state_routine(self.routine, &attempt.consumer),
            )
            .map(|run| run.run_id)
            .map_err(Into::into)
    }

    fn accepted_assessment(&self, member: &StateMember) -> Result<Option<bool>, AutomationError> {
        if self.trigger.kind != StateTriggerKind::PreparationEligible {
            return Ok(None);
        }
        let [task_id] = member.task_ids.as_slice() else {
            return Ok(None);
        };
        self.runtime
            .fresh_task_pilot_assessment(task_id, &member.fingerprint)
            .map_err(Into::into)
    }

    /// An assessment accepted under `material_v1` hashed every task field and
    /// the head it pinned, so it can only be checked by recomputing that hash
    /// at the pinned revision [ORB-13638]. When it still matches, nothing
    /// that contract covered has changed — in particular none of the default
    /// material fields — and the task keeps its assessment instead of joining
    /// a re-pilot wave on upgrade. Any doubt answers `false`; a failure to
    /// recompute is logged, so a silent re-pilot wave has a cause on record.
    fn carries_forward(&self, member: &StateMember, assessment: &MemberAssessment) -> bool {
        if self.trigger.kind != StateTriggerKind::PreparationEligible {
            return false;
        }
        match self.legacy_fingerprint(member, assessment) {
            Ok(legacy) => legacy.as_ref() == Some(&assessment.resulting_fingerprint),
            Err(error) => {
                tracing::warn!(
                    routine = self.routine,
                    member = member.key,
                    receipt = assessment.receipt_id,
                    error = %error,
                    "could not recompute the material_v1 fingerprint at the assessment's pinned \
                     revision; the member is assessed again"
                );
                false
            }
        }
    }

    fn release(&self, attempt: &MemberAttempt) {
        if attempt.kind == StateTriggerKind::PreparationEligible {
            match self.has_legacy_assessment(attempt) {
                Ok(true) => {
                    tracing::debug!(
                        routine = self.routine,
                        attempt = attempt.id,
                        "keeping the retired attempt's pin for its material_v1 assessment"
                    );
                    return;
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(
                        routine = self.routine,
                        attempt = attempt.id,
                        error = %error,
                        "could not determine whether the retired attempt's pin backs a material_v1 assessment; keeping it for doctor cleanup"
                    );
                    return;
                }
            }
        }
        self.pinned.borrow_mut().remove(&attempt.id);
        if let Err(error) = self
            .owner()
            .and_then(|owner| pins::release(&self.source(), &owner, attempt))
        {
            tracing::warn!(
                routine = self.routine,
                attempt = attempt.id,
                error = %error,
                "could not release the retired attempt's source pin; \
                 `orbit doctor --fix-automation-pins` reclaims it once its owner is proved \
                 and nothing names it"
            );
        }
    }

    fn outcome(&self, attempt: &MemberAttempt) -> Result<MemberOutcome, AutomationError> {
        self.outcome_for(attempt)
    }
}

impl Host<'_> {
    /// The task-pilot withhold for `now`, cached so this evaluation scans the
    /// host's recent runs once. A different clock replaces the cache.
    pub(super) fn pilot_provider_withhold(&self, now: DateTime<Utc>) -> Option<String> {
        if let Some(cached) = self.pilot_hold.borrow().as_ref()
            && cached.now == now
        {
            return cached.withhold.clone();
        }
        let withhold = self.runtime.task_pilot_provider_withhold(now);
        *self.pilot_hold.borrow_mut() = Some(super::super::PilotHoldCache {
            now,
            withhold: withhold.clone(),
        });
        withhold
    }

    /// Admission reads the observation cache. A reconcile that admits before
    /// observation computes the hold at the current time.
    pub(super) fn cached_pilot_provider_withhold(&self) -> Option<String> {
        if let Some(cached) = self.pilot_hold.borrow().as_ref() {
            return cached.withhold.clone();
        }
        self.pilot_provider_withhold(Utc::now())
    }

    /// The task statuses this consumer's members are observed in.
    pub(super) fn observed_statuses(&self) -> Vec<TaskStatus> {
        match self.trigger.kind {
            StateTriggerKind::PreparationEligible => self.eligibility().statuses.clone(),
            StateTriggerKind::ExecutionFailed => vec![TaskStatus::Blocked],
            StateTriggerKind::FindingUnverified => VERIFIABLE_STATUSES.to_vec(),
            StateTriggerKind::PilotWarningsHeld => vec![TaskStatus::Proposed],
            StateTriggerKind::BlockEscalated => vec![TaskStatus::Blocked],
            // An open request can hold a task at any gate: blocked
            // (block triage), backlog (pilot admission), proposed or
            // review (approvals).
            StateTriggerKind::DecisionRequested => vec![
                TaskStatus::Blocked,
                TaskStatus::Backlog,
                TaskStatus::Proposed,
                TaskStatus::Review,
            ],
        }
    }

    /// Whether this consumer feeds `escalation_triage_pipeline`.
    pub(super) fn is_escalation_triage(&self) -> bool {
        self.escalation_subject().is_some()
    }

    /// The escalation triage subject this consumer's members are, as the
    /// pipeline's `subject` input names it.
    pub(super) fn escalation_subject(&self) -> Option<&'static str> {
        match self.trigger.kind {
            StateTriggerKind::PilotWarningsHeld => Some("pilot_warning"),
            StateTriggerKind::BlockEscalated => Some("block"),
            StateTriggerKind::PreparationEligible
            | StateTriggerKind::ExecutionFailed
            | StateTriggerKind::FindingUnverified
            | StateTriggerKind::DecisionRequested => None,
        }
    }

    /// Whether `task` is still an escalation triage member at `now`, with
    /// the fingerprint it would carry: the held assessment's receipt, or the
    /// block episode.
    pub(super) fn escalation_of(
        &self,
        task: &Task,
        now: DateTime<Utc>,
    ) -> Result<Escalation, OrbitError> {
        Ok(match self.trigger.kind {
            StateTriggerKind::BlockEscalated => {
                match self.runtime.block_triage_candidacy_of(task, now)? {
                    TriageCandidacy::Qualifies(episode) => Escalation::Qualifies(episode.key()),
                    other => Escalation::Gone(format!("block_{}", other.as_str())),
                }
            }
            _ => match self.runtime.pilot_warning_candidacy_of(task)? {
                PilotWarningCandidacy::Qualifies(held) => Escalation::Qualifies(held.operation_id),
                other => Escalation::Gone(format!("pilot_warning_{}", other.as_str())),
            },
        })
    }

    /// When the triager's marker for the member `fingerprint` was recorded.
    pub(super) fn escalation_triaged_at(
        &self,
        history: &[TaskHistoryEntry],
        fingerprint: &str,
    ) -> Option<DateTime<Utc>> {
        match self.trigger.kind {
            StateTriggerKind::BlockEscalated => block_triaged_at(history, fingerprint),
            _ => triaged_at(history, fingerprint),
        }
    }

    /// Settle an escalation triage attempt from the markers its triagers
    /// recorded. A member whose assessment or block episode carries its
    /// `pilot_warning_triaged` or `blocked_task_triaged` marker is applied;
    /// once the run stopped, one that no longer qualifies (an operator
    /// decided or touched it, the task moved on, was re-piloted or blocked
    /// again, or was deleted) is released, and one still qualifying without
    /// a marker failed.
    pub(super) fn escalation_outcome(
        &self,
        attempt: &MemberAttempt,
        action_id: &str,
        stopped: bool,
    ) -> Result<MemberOutcome, AutomationError> {
        let mut applied = Vec::new();
        let mut failed = BTreeMap::new();
        let mut superseded = BTreeMap::new();
        for member in attempt.members() {
            let [task_id] = member.task_ids.as_slice() else {
                failed.insert(member.key.clone(), "malformed_member".into());
                continue;
            };
            let task = match self.runtime.get_task(task_id) {
                Ok(task) => task,
                Err(OrbitError::NotFound {
                    kind: NotFoundKind::Task,
                    ..
                }) => {
                    superseded.insert(member.key.clone(), "task_deleted".into());
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let history = self.runtime.get_task_history(task_id)?;
            if let Some(at) = self.escalation_triaged_at(&history, &member.fingerprint) {
                applied.push(MemberEvidence {
                    action_id: action_id.into(),
                    attempt_id: attempt.id.clone(),
                    member_key: member.key.clone(),
                    input_fingerprint: member.fingerprint.clone(),
                    resulting_fingerprint: member.fingerprint.clone(),
                    ready: true,
                    result: json!({
                        "task_id": task_id,
                        "subject": self.escalation_subject(),
                        // The assessment's receipt or the block episode.
                        "operation_id": member.fingerprint,
                        "triaged_at": at,
                    }),
                });
                continue;
            }
            if !stopped {
                return Ok(MemberOutcome::Pending);
            }
            match self.escalation_of(&task, Utc::now())? {
                Escalation::Qualifies(fingerprint) if fingerprint == member.fingerprint => {
                    failed.insert(member.key.clone(), "no_triage_marker".into());
                }
                Escalation::Qualifies(_) => {
                    superseded.insert(member.key.clone(), "material_changed".into());
                }
                Escalation::Gone(reason) => {
                    superseded.insert(member.key.clone(), reason);
                }
            }
        }
        Ok(MemberOutcome::Settled(MemberBatchEvidence {
            action_id: action_id.into(),
            attempt_id: attempt.id.clone(),
            applied,
            failed,
            superseded,
        }))
    }

    /// The effective decision request `member` was raised for, by its
    /// fingerprint (the request id), or `None` when that request no longer
    /// reads on the task at all.
    pub(super) fn decision_request_of(
        &self,
        member: &StateMember,
    ) -> Result<Option<orbit_types::task::DecisionRequest>, AutomationError> {
        let [task_id] = member.task_ids.as_slice() else {
            return Ok(None);
        };
        Ok(self
            .runtime
            .get_task_decision_requests(task_id)?
            .into_iter()
            .find(|request| request.id == member.fingerprint))
    }

    /// Settle a decision-request attempt from the request's effective
    /// state. A request that left `open` (answered, superseded or expired)
    /// releases its member as superseded, whether or not the run finished;
    /// a run that stopped while its request is still open has done the
    /// notification it was fired for, so its member is applied at the
    /// request's fingerprint and the same request never fires again.
    pub(super) fn decision_outcome(
        &self,
        attempt: &MemberAttempt,
        action_id: &str,
        stopped: bool,
    ) -> Result<MemberOutcome, AutomationError> {
        use orbit_types::task::DecisionRequestState;
        let mut applied = Vec::new();
        let mut failed = BTreeMap::new();
        let mut superseded = BTreeMap::new();
        for member in attempt.members() {
            let [task_id] = member.task_ids.as_slice() else {
                failed.insert(member.key.clone(), "malformed_member".into());
                continue;
            };
            match self.runtime.get_task(task_id) {
                Ok(_) => {}
                Err(OrbitError::NotFound {
                    kind: NotFoundKind::Task,
                    ..
                }) => {
                    superseded.insert(member.key.clone(), "task_deleted".into());
                    continue;
                }
                Err(error) => return Err(error.into()),
            }
            let request = match self.decision_request_of(member)? {
                Some(request) => request,
                None => {
                    superseded.insert(member.key.clone(), "request_superseded".into());
                    continue;
                }
            };
            match request.state {
                DecisionRequestState::Answered => {
                    superseded.insert(member.key.clone(), "request_answered".into());
                }
                DecisionRequestState::Superseded => {
                    superseded.insert(member.key.clone(), "request_superseded".into());
                }
                DecisionRequestState::Expired => {
                    superseded.insert(member.key.clone(), "request_expired".into());
                }
                DecisionRequestState::Open => {
                    if !stopped {
                        return Ok(MemberOutcome::Pending);
                    }
                    applied.push(MemberEvidence {
                        action_id: action_id.into(),
                        attempt_id: attempt.id.clone(),
                        member_key: member.key.clone(),
                        input_fingerprint: member.fingerprint.clone(),
                        resulting_fingerprint: member.fingerprint.clone(),
                        ready: true,
                        result: json!({
                            "task_id": task_id,
                            "request_id": request.id,
                            "gate": request.gate.as_str(),
                            "binding": request.binding.key(),
                        }),
                    });
                }
            }
        }
        Ok(MemberOutcome::Settled(MemberBatchEvidence {
            action_id: action_id.into(),
            attempt_id: attempt.id.clone(),
            applied,
            failed,
            superseded,
        }))
    }
}

/// A temporary hold diagnostic names every durable run the operator can inspect.
pub(super) fn already_verifying(run_ids: &BTreeSet<String>) -> String {
    format!(
        "already_verifying: {}",
        run_ids.iter().cloned().collect::<Vec<_>>().join(", ")
    )
}

/// Whether a task is still an escalation triage member, and as what.
pub(super) enum Escalation {
    /// It qualifies, with the fingerprint its member carries.
    Qualifies(String),
    /// It no longer qualifies, for this retirement reason.
    Gone(String),
}

/// A temporary hold diagnostic names every durable run the operator can inspect.
pub(super) fn already_triaging(run_ids: &BTreeSet<String>) -> String {
    format!(
        "already_triaging: {}",
        run_ids.iter().cloned().collect::<Vec<_>>().join(", ")
    )
}

/// A temporary hold diagnostic names every durable run the operator can inspect.
pub(super) fn already_preparing(run_ids: &BTreeSet<String>) -> String {
    format!(
        "already_preparing: {}",
        run_ids.iter().cloned().collect::<Vec<_>>().join(", ")
    )
}

/// The stored `task.crew` shared by every id in an incident's `task_ids`,
/// or `None` when they disagree. Unlike preparation's single-task read, an
/// execution-failed member's ids come from incident grouping and can
/// themselves carry mixed crews [ORB-12796].
pub(super) fn incident_crew(
    runtime: &OrbitRuntime,
    task_ids: &[String],
) -> Result<Option<Option<String>>, AutomationError> {
    let mut agreed: Option<Option<String>> = None;
    for id in task_ids {
        let task = runtime.get_task(id)?;
        let crew = bundle_crew(task.crew.as_deref());
        match &agreed {
            None => agreed = Some(crew),
            Some(existing) if existing == &crew => {}
            Some(_) => return Ok(None),
        }
    }
    Ok(Some(agreed.flatten()))
}
