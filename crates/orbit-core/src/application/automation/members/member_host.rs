//! The shared member scheduler's view of this workspace.

use super::super::{pins, preparation};
use crate::OrbitRuntime;
use chrono::{DateTime, Utc};
use orbit_automation::{
    AutomationError,
    members::{MemberAdmission, MemberHost, MemberOutcome, MemberPage},
};
use orbit_store::contracts::TaskListFilter;
use orbit_types::{
    task::TaskStatus,
    workflow::{
        JobRunState, JobRunTrigger,
        automation::{members::*, *},
    },
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

use super::{Host, SUPERSEDED_BY_SOURCE, stale_tasks};

impl MemberHost for Host<'_> {
    fn head(&self, branch: &str) -> Result<(String, SourceRevision), AutomationError> {
        if let Some(head) = self.head.borrow().as_ref() {
            return Ok(head.clone());
        }
        // Only pilot preparation best-effort fetches; incident consumers
        // retain their local-only source contract.
        let head = if self.trigger.kind == StateTriggerKind::PreparationEligible {
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
                statuses: Some(match self.trigger.kind {
                    StateTriggerKind::PreparationEligible => self.eligibility().statuses.clone(),
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

        let active_preparations = match self.trigger.kind {
            StateTriggerKind::PreparationEligible => {
                preparation::active_task_pilot_preparations(self.runtime)?
            }
            StateTriggerKind::ExecutionFailed => BTreeMap::new(),
        };

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
                let task = self.runtime.get_task(id)?;
                if !orbit_automation::members::preparation::eligible(&task, self.eligibility()) {
                    return Ok(MemberAdmission::Retire("task_ineligible".into()));
                }
                if let Some(run_ids) = active_preparations.get(id) {
                    return Ok(MemberAdmission::Withhold(already_preparing(run_ids)));
                }
                if self.fingerprint(&task, &source.commit)? != member.fingerprint {
                    return Ok(MemberAdmission::Retire("material_changed".into()));
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
        let statuses = match self.trigger.kind {
            StateTriggerKind::PreparationEligible => self.eligibility().statuses.clone(),
            StateTriggerKind::ExecutionFailed => vec![TaskStatus::Blocked],
        };
        let indexed = self.runtime.task_status_index_for(keys)?;
        let incidents = match self.trigger.kind {
            StateTriggerKind::ExecutionFailed => {
                Some(self.incidents.borrow_mut().inventory(self.runtime)?)
            }
            StateTriggerKind::PreparationEligible => None,
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
        pins::pin(&self.source(), &self.owner()?, attempt)?;

        let origin = if self.trigger.kind == StateTriggerKind::ExecutionFailed {
            "execution_failure"
        } else {
            "preparation"
        };

        // Every batch member travels as an explicit task id; evaluate already
        // grouped the attempt by stored crew, and prepare partitions those
        // ids by `max_partition_size`, so one run fans out over a
        // crew-homogeneous batch [ORB-12746, ORB-12761].
        let task_ids = attempt.task_ids();
        self.runtime
            .submit_automation_pipeline_run(
                self.trigger.job_name(),
                json!({
                    "state_automation": attempt,
                    "task_ids": task_ids,
                    "source_revision": attempt.member.source.commit,
                    "base_branch": self.trigger.branch,
                    "max_tasks": task_ids.len(),
                    "promotion_authorized": false,
                    "automation_origin": origin,
                }),
                &attempt.action_key,
                JobRunTrigger::state_routine(self.routine, &attempt.consumer),
            )
            .map(|run| run.run_id)
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

        // Only the exact canonical deterministic apply steps can provide this
        // record. Agent prose and unrelated output keys are never searched.
        let state = self.runtime.read_run_state(id)?;
        let apply_output = |index: u32| {
            state
                .as_ref()
                .filter(|state| state.step_states.get(&index) == Some(&JobRunState::Success))
                .and_then(|state| state.step_outputs.get(&index))
        };
        let Some(initial) = apply_output(APPLY_STEP) else {
            if !stopped {
                return Ok(MemberOutcome::Pending);
            }
            // A retry would replay the frozen source; once the branch moved
            // under any member's material, no member was applied and every
            // one is better claimed afresh at the head [ORB-14476].
            let prepared = state
                .as_ref()
                .filter(|state| state.step_states.get(&PREPARE_STEP) == Some(&JobRunState::Success))
                .and_then(|state| state.step_outputs.get(&PREPARE_STEP))
                .unwrap_or(&Value::Null);
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
            .get("repair_count")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            != 0;
        let repair = apply_output(REPAIR_APPLY_STEP);
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

/// Step indices of the deterministic steps in `task_pilot_pipeline`: the
/// preparation, the partition apply and the targeted repair apply.
const PREPARE_STEP: u32 = 0;
const APPLY_STEP: u32 = 2;
const REPAIR_APPLY_STEP: u32 = 4;

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
