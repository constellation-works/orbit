//! Narrow Core adapter for the shared state scheduling domain.

use super::{consumer_key, preparation, preparation::InstructionSnapshot, source::Source};
use crate::OrbitRuntime;
use chrono::{DateTime, Utc};
use orbit_automation::{
    AutomationError, automation_error_to_orbit,
    delivery::definition_epoch,
    members::{self, MemberAdmission, MemberEvaluation, MemberHost, MemberOutcome, MemberPage},
};
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

#[cfg(test)]
thread_local! {
    static HEAD_INVOCATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_head_invocations() {
    HEAD_INVOCATIONS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn head_invocations() -> usize {
    HEAD_INVOCATIONS.with(std::cell::Cell::get)
}

pub(crate) fn evaluate(
    runtime: &OrbitRuntime,
    definition: &RoutineDefinition,
    dry_run: bool,
    now: DateTime<Utc>,
) -> Result<AutomationDiagnostic, OrbitError> {
    let trigger = definition
        .trigger
        .state
        .as_ref()
        .ok_or_else(|| OrbitError::InvalidInput("not a state routine".into()))?;

    let consumer = consumer_key(runtime, "routine", &definition.name)?;

    // Timing edits apply to pending work; the active attempt retains its budget.
    let epoch = definition_epoch(&(
        &trigger.kind,
        &trigger.owner_machine,
        &trigger.branch,
        &definition.target,
    ))
    .map_err(automation_error_to_orbit)?;

    let owned = Some(trigger.owner_machine.as_str()) == runtime.automation_machine_identity();

    let mut effective = trigger.clone();
    effective.retries = effective.retries.min(definition.policy.retries.max);

    members::evaluate(
        runtime.automation_store()?.as_ref(),
        &Host::new(runtime, &effective),
        MemberEvaluation {
            consumer: &consumer,
            epoch: &epoch,
            trigger: &effective,
            enabled: definition.enabled && owned,
            dry_run,
            now,
            constraints: Default::default(),
        },
    )
    .map_err(automation_error_to_orbit)
}

pub(crate) struct Host<'a> {
    runtime: &'a OrbitRuntime,
    trigger: &'a StateTrigger,
    incidents: RefCell<super::incidents::IncidentSession>,
    instructions: RefCell<BTreeMap<String, InstructionSnapshot>>,
}

impl<'a> Host<'a> {
    pub(crate) fn new(runtime: &'a OrbitRuntime, trigger: &'a StateTrigger) -> Self {
        Self {
            runtime,
            trigger,
            incidents: RefCell::new(super::incidents::IncidentSession::new()),
            instructions: RefCell::new(BTreeMap::new()),
        }
    }

    /// The one predicate this consumer observes, admits and fingerprints
    /// with [ORB-12745].
    fn eligibility(&self) -> &PreparationEligibility {
        &self.trigger.eligibility
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
                    let snapshot = preparation::instructions(self.runtime, revision)?;
                    cached.insert(revision.to_string(), snapshot.clone());
                    snapshot
                }
            }
        };

        preparation::fingerprint_with_instructions(
            self.runtime,
            task,
            revision,
            &instructions,
            self.eligibility(),
        )
    }

    #[cfg(test)]
    pub(crate) fn incident_work_stats(&self) -> super::incidents::IncidentWorkStats {
        self.incidents.borrow().stats()
    }
}

impl MemberHost for Host<'_> {
    fn head(&self, branch: &str) -> Result<(String, SourceRevision), AutomationError> {
        #[cfg(test)]
        HEAD_INVOCATIONS.with(|count| count.set(count.get() + 1));
        Source::new(&self.runtime.paths().repo_root).head(branch)
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

        for envelope in tasks.items {
            let task = self.runtime.get_task(&envelope.id)?;

            match self.trigger.kind {
                StateTriggerKind::PreparationEligible => {
                    if !orbit_automation::members::preparation::eligible(&task, self.eligibility())
                    {
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
                        crew: bundle_crew(task.crew.as_deref()),
                    });
                }
                StateTriggerKind::ExecutionFailed => {
                    match super::incidents::observe(self.runtime, &task) {
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

                            candidates.push(StateMember {
                                fingerprint: key.clone(),
                                key,
                                task_ids,
                                source: source.clone(),
                                evidence,
                                first_seen: now,
                                changed_at: now,
                                crew: None,
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
        // per task_id (each Source::head is several git spawns).
        if self.trigger.kind == StateTriggerKind::PreparationEligible {
            let (_, source) = self.head(&self.trigger.branch)?;
            for id in &member.task_ids {
                let task = self.runtime.get_task(id)?;
                if !orbit_automation::members::preparation::eligible(&task, self.eligibility()) {
                    return Ok(MemberAdmission::Retire("task_ineligible".into()));
                }
                if self.fingerprint(&task, &source.commit)? != member.fingerprint {
                    return Ok(MemberAdmission::Retire("material_changed".into()));
                }
            }
            return Ok(MemberAdmission::Admit);
        }

        for id in &member.task_ids {
            let task = self.runtime.get_task(id)?;
            let current = match super::incidents::observe(self.runtime, &task) {
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
        Source::new(&self.runtime.paths().repo_root).git(&[
            "update-ref",
            &format!("refs/orbit/automation/{}", attempt.id),
            &attempt.member.source.commit,
        ])?;

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
            )
            .map(|run| run.run_id)
            .map_err(Into::into)
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
            return Ok(if stopped {
                MemberOutcome::Failed("stopped_without_member_evidence".into())
            } else {
                MemberOutcome::Pending
            });
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
        for member in attempt.members() {
            if applied.iter().any(|entry| entry.member_key == member.key) {
                continue;
            }
            let reason = latest_outcomes
                .get("task_outcomes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|outcome| {
                    outcome
                        .get("task_id")
                        .and_then(Value::as_str)
                        .is_some_and(|task_id| member.task_ids.iter().any(|id| id == task_id))
                })
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
        }))
    }
}

/// Step indices of the two deterministic apply steps in
/// `task_pilot_pipeline`: the partition apply and the targeted repair apply.
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

/// Recheck the server-issued claim at the deterministic prepare/apply boundary.
pub(crate) fn claim(
    runtime: &OrbitRuntime,
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

    let state = runtime
        .automation_store()?
        .automation_state(&submitted.consumer)?
        .ok_or_else(|| OrbitError::InvalidInput("state claim missing".into()))?;

    // Preparation material is derived from the branch head, so a moved head
    // invalidates the claim; incident material is not tied to the head.
    if submitted.kind == StateTriggerKind::PreparationEligible
        && Source::new(&runtime.paths().repo_root)
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
        || active.members() != submitted.members()
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
