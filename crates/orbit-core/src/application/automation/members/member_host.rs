//! The shared member scheduler's view of this workspace.

use super::super::{pins, preparation};
use crate::OrbitRuntime;
use chrono::{DateTime, Utc};
use orbit_automation::{
    AutomationError, DeferredReason,
    members::{MemberAdmission, MemberHost, MemberOutcome, MemberPage},
};
use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::contracts::TaskListFilter;
use orbit_types::{
    task::{Task, TaskHistoryEntry, TaskStatus, TaskType},
    workflow::{
        ActivityV2Spec, JobRun, JobRunState, JobRunTrigger, JobV2, JobV2StepBody, PipelineState,
        TargetStep,
        automation::{members::*, *},
    },
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

use super::{Host, SUPERSEDED_BY_SOURCE, stale_tasks};
use crate::application::task::blocked_recovery::block_triaged_at;
use crate::application::task::escalation_triage_steps::{PilotWarningCandidacy, triaged_at};
use crate::application::task::{
    TriageCandidacy, VERIFIABLE_STATUSES, finding_fingerprint, needs_verification,
};

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
        let scan_before = after
            .map(serde_json::from_str)
            .transpose()
            .map_err(|e| AutomationError::Evidence(format!("invalid task continuation: {e}")))?;

        let tasks = self.runtime.task_candidates(
            &TaskListFilter {
                scan_before,
                statuses: Some(self.observed_statuses()),
                task_type: (self.trigger.kind == StateTriggerKind::FindingUnverified)
                    .then_some(TaskType::Bug),
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

        let active_preparations = match self.trigger.kind {
            StateTriggerKind::PreparationEligible => {
                preparation::active_task_pilot_preparations(self.runtime)?
            }
            StateTriggerKind::FindingUnverified => {
                preparation::active_finding_verifications(self.runtime)?
            }
            StateTriggerKind::PilotWarningsHeld | StateTriggerKind::BlockEscalated => {
                preparation::active_escalation_triages(self.runtime)?
            }
            StateTriggerKind::ExecutionFailed => BTreeMap::new(),
        };
        // One scan for the page. Admission of a member retained from an
        // earlier page reads this cache instead of scanning again.
        let provider_hold = (self.trigger.kind == StateTriggerKind::PreparationEligible)
            .then(|| self.pilot_provider_withhold(now))
            .flatten();

        for envelope in tasks.items {
            let task = self.runtime.get_task(&envelope.id)?;

            match self.trigger.kind {
                StateTriggerKind::PreparationEligible => {
                    if !orbit_automation::members::preparation::eligible(&task, self.eligibility())
                    {
                        withheld.insert(task.id, "task_ineligible".into());
                        continue;
                    }
                    if let Some(run_ids) = active_preparations.get(&task.id) {
                        withheld.insert(task.id, already_preparing(run_ids));
                        continue;
                    }
                    if let Some(reason) = &provider_hold {
                        withheld.insert(task.id, reason.clone());
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
                        crew: bundle_crew(task.crew.as_deref()),
                    });
                }
                StateTriggerKind::FindingUnverified => {
                    // Only a finding still owed a verdict is a member; a
                    // confirmed one, or another bug, is not withheld either.
                    let history = self.runtime.get_task_history(&task.id)?;
                    if !needs_verification(&task, &history) {
                        continue;
                    }
                    if let Some(run_ids) = active_preparations.get(&task.id) {
                        withheld.insert(task.id, already_verifying(run_ids));
                        continue;
                    }
                    candidates.push(StateMember {
                        key: task.id.clone(),
                        task_ids: vec![task.id.clone()],
                        fingerprint: finding_fingerprint(&task),
                        source: source.clone(),
                        evidence: json!({"task_id":task.id}),
                        first_seen: now,
                        changed_at: now,
                        crew: bundle_crew(task.crew.as_deref()),
                    });
                }
                StateTriggerKind::PilotWarningsHeld => {
                    // Only an assessment that still holds on warnings and
                    // awaits triage is a member, keyed by its receipt so
                    // each assessment is read once.
                    let PilotWarningCandidacy::Qualifies(held) =
                        self.runtime.pilot_warning_candidacy_of(&task)?
                    else {
                        continue;
                    };
                    if let Some(run_ids) = active_preparations.get(&task.id) {
                        withheld.insert(task.id, already_triaging(run_ids));
                        continue;
                    }
                    candidates.push(StateMember {
                        key: task.id.clone(),
                        task_ids: vec![task.id.clone()],
                        evidence: json!({"task_id":task.id, "operation_id":held.operation_id}),
                        fingerprint: held.operation_id,
                        source: source.clone(),
                        first_seen: now,
                        changed_at: now,
                        crew: bundle_crew(task.crew.as_deref()),
                    });
                }
                StateTriggerKind::BlockEscalated => {
                    // Only an episode the shared triage rule qualifies is a
                    // member, keyed by the task and fingerprinted by the
                    // episode, so each episode is read once and a fresh
                    // block is a new member.
                    let TriageCandidacy::Qualifies(episode) =
                        self.runtime.block_triage_candidacy_of(&task, now)?
                    else {
                        continue;
                    };
                    if let Some(run_ids) = active_preparations.get(&task.id) {
                        withheld.insert(task.id, already_triaging(run_ids));
                        continue;
                    }
                    candidates.push(StateMember {
                        key: task.id.clone(),
                        task_ids: vec![task.id.clone()],
                        evidence: json!({
                            "task_id": task.id,
                            "episode": episode.key(),
                            "basis": episode.basis,
                        }),
                        fingerprint: episode.key(),
                        source: source.clone(),
                        first_seen: now,
                        changed_at: now,
                        crew: bundle_crew(task.crew.as_deref()),
                    });
                }
                StateTriggerKind::ExecutionFailed => {
                    match super::super::incidents::observe(self.runtime, &task) {
                        Ok((key, evidence)) => {
                            if candidates
                                .iter()
                                .any(|other: &StateMember| other.key == key)
                            {
                                continue;
                            }

                            // The full cohort is hydrated at most once per page
                            // and reused across later admissions in this Host
                            // after a freshness check.
                            if incident_inventory.is_none() {
                                incident_inventory =
                                    Some(self.incidents.borrow_mut().inventory(self.runtime)?);
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

                            // An incident's task_ids span whatever cohort
                            // diagnose grouped, not one task's stored crew,
                            // so the member's own bundle must agree before
                            // it can carry a crew identity at all [ORB-12796].
                            let crew = match incident_crew(self.runtime, &task_ids) {
                                Ok(Some(crew)) => crew,
                                Ok(None) => {
                                    withheld.insert(task.id, "incident_mixed_crew".into());
                                    continue;
                                }
                                Err(error) => {
                                    withheld.insert(task.id, error.to_string());
                                    continue;
                                }
                            };

                            candidates.push(StateMember {
                                fingerprint: key.clone(),
                                key,
                                task_ids,
                                source: source.clone(),
                                evidence,
                                first_seen: now,
                                changed_at: now,
                                crew,
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
                .inventory(self.runtime)?
                .get(&member.key)
                != Some(&member.task_ids)
        {
            return Ok(MemberAdmission::Retire(
                "incident_membership_or_recovery_changed".into(),
            ));
        }

        // Re-derive the material now: a member whose input moved may not be admitted.
        // Branch head is invariant for this call; resolve it once rather than
        // per task_id; observation and admission share the cached source.
        if self.trigger.kind == StateTriggerKind::PreparationEligible {
            let (_, source) = self.head(&self.trigger.branch)?;
            // Retained pending members may be off the current observation
            // page, or another pilot may have prepared them since observation.
            let active_preparations = preparation::active_task_pilot_preparations(self.runtime)?;
            for id in &member.task_ids {
                let task = match self.runtime.get_task(id) {
                    Ok(task) => task,
                    Err(OrbitError::NotFound {
                        kind: NotFoundKind::Task,
                        ..
                    }) => return Ok(MemberAdmission::Retire("task_deleted".into())),
                    Err(error) => return Err(error.into()),
                };
                if !orbit_automation::members::preparation::eligible(&task, self.eligibility()) {
                    return Ok(MemberAdmission::Retire("task_ineligible".into()));
                }
                if let Some(run_ids) = active_preparations.get(id) {
                    return Ok(MemberAdmission::Withhold(already_preparing(run_ids)));
                }
                if self.fingerprint(&task, &source.commit)? != member.fingerprint {
                    return Ok(MemberAdmission::Retire("material_changed".into()));
                }
                // Pending members can be off the observation page when an
                // explicit pilot settles. Recheck accepted evidence here too,
                // after the active-holder guard, before creating any run.
                if self
                    .runtime
                    .fresh_task_pilot_assessment(id, &member.fingerprint)?
                    .is_some()
                {
                    return Ok(MemberAdmission::Retire("assessment_fresh".into()));
                }
            }
            // A member retained from before the hold started is still pending.
            // Withhold it here, after the active-pilot guard, so the routine
            // does not submit a run the provider would fail.
            if let Some(reason) = self.cached_pilot_provider_withhold() {
                return Ok(MemberAdmission::Withhold(reason));
            }
            return Ok(MemberAdmission::Admit);
        }

        if self.trigger.kind == StateTriggerKind::FindingUnverified {
            let active = preparation::active_finding_verifications(self.runtime)?;
            for id in &member.task_ids {
                let task = match self.runtime.get_task(id) {
                    Ok(task) => task,
                    Err(OrbitError::NotFound {
                        kind: NotFoundKind::Task,
                        ..
                    }) => return Ok(MemberAdmission::Retire("task_deleted".into())),
                    Err(error) => return Err(error.into()),
                };
                // Deletion can also happen between reading the task and its history.
                let history = match self.runtime.get_task_history(id) {
                    Ok(history) => history,
                    Err(OrbitError::NotFound {
                        kind: NotFoundKind::Task,
                        ..
                    }) => return Ok(MemberAdmission::Retire("task_deleted".into())),
                    Err(error) => return Err(error.into()),
                };
                if !needs_verification(&task, &history) {
                    return Ok(MemberAdmission::Retire(
                        "finding_verified_or_ineligible".into(),
                    ));
                }
                if let Some(run_ids) = active.get(id) {
                    return Ok(MemberAdmission::Withhold(already_verifying(run_ids)));
                }
                if finding_fingerprint(&task) != member.fingerprint {
                    return Ok(MemberAdmission::Retire("material_changed".into()));
                }
            }
            return Ok(MemberAdmission::Admit);
        }

        if self.is_escalation_triage() {
            let active = preparation::active_escalation_triages(self.runtime)?;
            for id in &member.task_ids {
                let escalation = match self
                    .runtime
                    .get_task(id)
                    .and_then(|task| self.escalation_of(&task, Utc::now()))
                {
                    Ok(escalation) => escalation,
                    Err(OrbitError::NotFound {
                        kind: NotFoundKind::Task,
                        ..
                    }) => return Ok(MemberAdmission::Retire("task_deleted".into())),
                    Err(error) => return Err(error.into()),
                };
                match escalation {
                    Escalation::Qualifies(fingerprint) if fingerprint == member.fingerprint => {}
                    // A re-pilot wrote another assessment, or the task was
                    // blocked again; that is its own member.
                    Escalation::Qualifies(_) => {
                        return Ok(MemberAdmission::Retire("material_changed".into()));
                    }
                    Escalation::Gone(reason) => return Ok(MemberAdmission::Retire(reason)),
                }
                if let Some(run_ids) = active.get(id) {
                    return Ok(MemberAdmission::Withhold(already_triaging(run_ids)));
                }
            }
            return Ok(MemberAdmission::Admit);
        }

        for id in &member.task_ids {
            let task = self.runtime.get_task(id)?;
            let current = match super::super::incidents::observe(self.runtime, &task) {
                Ok((key, _)) => key,
                Err(error) => {
                    return Ok(MemberAdmission::Retire(error.to_string()));
                }
            };

            if current != member.fingerprint {
                return Ok(MemberAdmission::Retire("material_changed".into()));
            }
        }

        Ok(MemberAdmission::Admit)
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
            | StateTriggerKind::BlockEscalated => None,
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

impl Host<'_> {
    /// The task-pilot withhold for `now`, cached so this evaluation scans the
    /// host's recent runs once. A different clock replaces the cache.
    fn pilot_provider_withhold(&self, now: DateTime<Utc>) -> Option<String> {
        if let Some(cached) = self.pilot_hold.borrow().as_ref()
            && cached.now == now
        {
            return cached.withhold.clone();
        }
        let withhold = self.runtime.task_pilot_provider_withhold(now);
        *self.pilot_hold.borrow_mut() = Some(super::PilotHoldCache {
            now,
            withhold: withhold.clone(),
        });
        withhold
    }

    /// Admission reads the observation cache. A reconcile that admits before
    /// observation computes the hold at the current time.
    fn cached_pilot_provider_withhold(&self) -> Option<String> {
        if let Some(cached) = self.pilot_hold.borrow().as_ref() {
            return cached.withhold.clone();
        }
        self.pilot_provider_withhold(Utc::now())
    }

    /// The task statuses this consumer's members are observed in.
    fn observed_statuses(&self) -> Vec<TaskStatus> {
        match self.trigger.kind {
            StateTriggerKind::PreparationEligible => self.eligibility().statuses.clone(),
            StateTriggerKind::ExecutionFailed => vec![TaskStatus::Blocked],
            StateTriggerKind::FindingUnverified => VERIFIABLE_STATUSES.to_vec(),
            StateTriggerKind::PilotWarningsHeld => vec![TaskStatus::Proposed],
            StateTriggerKind::BlockEscalated => vec![TaskStatus::Blocked],
        }
    }

    /// Whether this consumer feeds `escalation_triage_pipeline`.
    fn is_escalation_triage(&self) -> bool {
        self.escalation_subject().is_some()
    }

    /// The escalation triage subject this consumer's members are, as the
    /// pipeline's `subject` input names it.
    fn escalation_subject(&self) -> Option<&'static str> {
        match self.trigger.kind {
            StateTriggerKind::PilotWarningsHeld => Some("pilot_warning"),
            StateTriggerKind::BlockEscalated => Some("block"),
            StateTriggerKind::PreparationEligible
            | StateTriggerKind::ExecutionFailed
            | StateTriggerKind::FindingUnverified => None,
        }
    }

    /// Whether `task` is still an escalation triage member at `now`, with
    /// the fingerprint it would carry: the held assessment's receipt, or the
    /// block episode.
    fn escalation_of(&self, task: &Task, now: DateTime<Utc>) -> Result<Escalation, OrbitError> {
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
    fn escalation_triaged_at(
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
    fn escalation_outcome(
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
}

/// A temporary hold diagnostic names every durable run the operator can inspect.
fn already_verifying(run_ids: &BTreeSet<String>) -> String {
    format!(
        "already_verifying: {}",
        run_ids.iter().cloned().collect::<Vec<_>>().join(", ")
    )
}

/// Whether a task is still an escalation triage member, and as what.
enum Escalation {
    /// It qualifies, with the fingerprint its member carries.
    Qualifies(String),
    /// It no longer qualifies, for this retirement reason.
    Gone(String),
}

/// A temporary hold diagnostic names every durable run the operator can inspect.
fn already_triaging(run_ids: &BTreeSet<String>) -> String {
    format!(
        "already_triaging: {}",
        run_ids.iter().cloned().collect::<Vec<_>>().join(", ")
    )
}

/// A temporary hold diagnostic names every durable run the operator can inspect.
fn already_preparing(run_ids: &BTreeSet<String>) -> String {
    format!(
        "already_preparing: {}",
        run_ids.iter().cloned().collect::<Vec<_>>().join(", ")
    )
}

/// The stored `task.crew` shared by every id in an incident's `task_ids`,
/// or `None` when they disagree. Unlike preparation's single-task read, an
/// execution-failed member's ids come from incident grouping and can
/// themselves carry mixed crews [ORB-12796].
fn incident_crew(
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

/// One deterministic step of the definition a run executed: its id and its
/// global step index, the key of its checkpoint.
#[derive(Clone, Copy)]
struct PilotStep {
    id: &'static str,
    index: u32,
}

/// The deterministic steps of a member job a member settles from: the
/// preparation, the partition apply and the follow-up apply — the pilot's
/// targeted repair, or finding verification's escalation. Each is found by
/// step id and action in the definition the run executed, never by position,
/// so a step inserted before them cannot hand another step's output to
/// settlement [ORB-15197].
struct PilotSteps {
    prepare: Option<PilotStep>,
    apply: Option<PilotStep>,
    repair_apply: Option<PilotStep>,
    /// The initial apply's count of tasks the follow-up apply settles.
    follow_up_count: &'static str,
}

impl PilotSteps {
    fn of(
        runtime: &OrbitRuntime,
        run: &JobRun,
        kind: StateTriggerKind,
    ) -> Result<Self, AutomationError> {
        let (prepare, apply, follow_up, follow_up_count) = match kind {
            StateTriggerKind::FindingUnverified => (
                "prepare_finding_verification",
                "apply_finding_verification",
                "apply_escalations",
                "escalation_count",
            ),
            StateTriggerKind::PreparationEligible | StateTriggerKind::ExecutionFailed => (
                "prepare_task_pilot",
                "apply_task_pilot_results",
                "apply_repairs",
                "repair_count",
            ),
            // Settled from triage markers, never from an apply checkpoint.
            StateTriggerKind::PilotWarningsHeld | StateTriggerKind::BlockEscalated => {
                return Ok(Self {
                    prepare: None,
                    apply: None,
                    repair_apply: None,
                    follow_up_count: "repair_count",
                });
            }
        };
        // The run's pinned snapshot when it has one, otherwise the catalog
        // asset. A job no catalog ships any more (the retired triage job)
        // offers no step, so its runs settle as stopped without evidence.
        let mut job = match runtime.resolve_run_definition(run) {
            Ok((_, job)) => job,
            Err(OrbitError::NotFound {
                kind: NotFoundKind::Job,
                ..
            }) => {
                return Ok(Self {
                    prepare: None,
                    apply: None,
                    repair_apply: None,
                    follow_up_count,
                });
            }
            Err(error) => return Err(error.into()),
        };
        // Resolve `activity:` refs exactly as execution does, so only a step
        // that ran the deterministic action qualifies.
        let catalog = runtime
            .v2_activity_catalog()
            .map_err(|e| AutomationError::Evidence(format!("activity catalog: {e}")))?;
        orbit_engine::resolve_job_catalog_refs_for_execution(&mut job, &catalog)
            .map_err(|e| AutomationError::Evidence(format!("run definition: {e}")))?;
        Ok(Self {
            prepare: deterministic_step(&job, "prepare", prepare),
            apply: deterministic_step(&job, "apply", apply),
            repair_apply: deterministic_step(&job, follow_up, apply),
            follow_up_count,
        })
    }
}

/// The top-level step `id` of `job`, when it runs the deterministic `action`.
fn deterministic_step(job: &JobV2, id: &'static str, action: &str) -> Option<PilotStep> {
    let index = job.steps.iter().position(|step| step.id == id)?;
    let JobV2StepBody::Target(TargetStep {
        spec: ActivityV2Spec::Deterministic(spec),
        ..
    }) = &job.steps[index].body
    else {
        return None;
    };
    (spec.action == action).then_some(PilotStep {
        id,
        index: u32::try_from(index).ok()?,
    })
}

/// The output `step` checkpointed, once it succeeded. The pipeline entry
/// under the step's id is written by the same checkpoint; a mismatch means
/// the index names another step in what was recorded, so nothing is read.
fn checkpoint(state: Option<&PipelineState>, step: Option<PilotStep>) -> Option<&Value> {
    let (state, step) = (state?, step?);
    if state.step_states.get(&step.index) != Some(&JobRunState::Success) {
        return None;
    }
    let output = state.step_output(step.index)?;
    state
        .pipeline
        .get(step.id)
        .is_none_or(|entry| entry == output)
        .then_some(output)
}

/// Whether `output` is an apply record: every apply output counts its
/// follow-ups and carries the member evidence slot. Anything else (a skipped
/// step's null, another step's output) settles no member [ORB-15197].
fn is_apply_output(output: &Value) -> bool {
    output.get("repair_count").is_some_and(Value::is_u64) || output.get("member_evidence").is_some()
}

/// The `member_evidence` an apply step recorded: one entry per claim member it
/// applied. A run checkpointed before batching carried a single object.
fn member_evidence(output: &Value) -> Result<Vec<MemberEvidence>, AutomationError> {
    match output.get("member_evidence") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(entries)) => entries
            .iter()
            .map(|entry| serde_json::from_value(entry.clone()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AutomationError::Evidence(e.to_string())),
        Some(entry) => serde_json::from_value(entry.clone())
            .map(|evidence| vec![evidence])
            .map_err(|e| AutomationError::Evidence(e.to_string())),
    }
}
