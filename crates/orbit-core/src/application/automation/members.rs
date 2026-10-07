//! Narrow Core adapter for the shared state scheduling domain.

use super::{consumer_key, pins, preparation, preparation::InstructionSnapshot, source::Source};
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
        JobRunState, JobRunTrigger, RoutineDefinition,
        automation::{members::*, *},
    },
};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

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
        &Host::new(runtime, &definition.name, &effective),
        MemberEvaluation {
            consumer: &consumer,
            epoch: &epoch,
            trigger: &effective,
            enabled: definition.enabled && owned,
            dry_run,
            now,
        },
    )
    .map_err(automation_error_to_orbit)
}

pub(crate) struct Host<'a> {
    runtime: &'a OrbitRuntime,
    /// The state routine this consumer serves; admitted runs name it as their
    /// trigger [ORB-13016].
    routine: &'a str,
    trigger: &'a StateTrigger,
    /// The one policy this consumer observes, admits and fingerprints with
    /// [ORB-12745, ORB-13638].
    policy: PreparationPolicy,
    incidents: RefCell<super::incidents::IncidentSession>,
    instructions: RefCell<BTreeMap<String, InstructionSnapshot>>,
    /// Attempt id → the commit its source pin names, resolved once per
    /// attempt to carry pre-upgrade assessments forward.
    pinned: RefCell<BTreeMap<String, Option<String>>>,
    /// The pin namespace owner, resolved once [ORB-14164].
    owner: RefCell<Option<pins::Owner>>,
}

impl<'a> Host<'a> {
    pub(crate) fn new(
        runtime: &'a OrbitRuntime,
        routine: &'a str,
        trigger: &'a StateTrigger,
    ) -> Self {
        Self {
            runtime,
            routine,
            trigger,
            policy: preparation::resolve_policy(runtime, Some(trigger)),
            incidents: RefCell::new(super::incidents::IncidentSession::new()),
            instructions: RefCell::new(BTreeMap::new()),
            pinned: RefCell::new(BTreeMap::new()),
            owner: RefCell::new(None),
        }
    }

    fn source(&self) -> Source<'_> {
        Source::new(&self.runtime.paths().repo_root)
    }

    fn owner(&self) -> Result<pins::Owner, AutomationError> {
        if let Some(owner) = self.owner.borrow().as_ref() {
            return Ok(owner.clone());
        }
        let owner = pins::Owner::of(self.runtime, &self.source())?;
        *self.owner.borrow_mut() = Some(owner.clone());
        Ok(owner)
    }

    fn eligibility(&self) -> &PreparationEligibility {
        &self.policy.eligibility
    }

    /// One instruction snapshot per revision serves every task on a page.
    fn instructions(&self, revision: &str) -> Result<InstructionSnapshot, AutomationError> {
        if let Some(snapshot) = self.instructions.borrow().get(revision) {
            return Ok(snapshot.clone());
        }
        let snapshot = preparation::instructions(self.runtime, revision)?;
        self.instructions
            .borrow_mut()
            .insert(revision.to_string(), snapshot.clone());
        Ok(snapshot)
    }

    fn fingerprint(
        &self,
        task: &orbit_types::task::Task,
        revision: &str,
    ) -> Result<String, AutomationError> {
        preparation::fingerprint_with_instructions(
            self.runtime,
            task,
            revision,
            &|revision| self.instructions(revision),
            &self.policy,
        )
    }

    /// The commit an attempt pinned when it was admitted, read by its exact
    /// ref rather than by listing every pin the repository holds [ORB-14164].
    fn pinned_revision(&self, attempt_id: &str) -> Result<Option<String>, AutomationError> {
        if let Some(pinned) = self.pinned.borrow().get(attempt_id) {
            return Ok(pinned.clone());
        }
        let pinned = pins::pinned(&self.source(), &self.owner()?, attempt_id)?;
        self.pinned
            .borrow_mut()
            .insert(attempt_id.to_string(), pinned.clone());
        Ok(pinned)
    }

    /// The `material_v1` hash the member's task would certify at the revision
    /// `assessment`'s attempt pinned.
    fn legacy_fingerprint(
        &self,
        member: &StateMember,
        assessment: &MemberAssessment,
    ) -> Result<Option<String>, AutomationError> {
        let [task_id] = member.task_ids.as_slice() else {
            return Ok(None);
        };
        let Some(revision) = self.pinned_revision(&assessment.receipt_id)? else {
            return Ok(None);
        };
        let task = self.runtime.get_task(task_id)?;
        let instructions = self.instructions(&revision)?;
        preparation::legacy_fingerprint(
            self.runtime,
            &task,
            &revision,
            &instructions,
            self.eligibility(),
        )
        .map(Some)
    }

    /// Whether a just-settled pre-upgrade assessment still needs its source
    /// revision to be carried forward. Current `material_v2` receipts do not
    /// match this legacy digest and can release their attempt pin immediately.
    fn has_legacy_assessment(&self, attempt: &MemberAttempt) -> Result<bool, AutomationError> {
        let Some(state) = self
            .runtime
            .automation_store()?
            .automation_state(&attempt.consumer)?
        else {
            return Ok(false);
        };
        let Some(members) = state.members else {
            return Ok(false);
        };

        for member in attempt.members() {
            let Some(assessment) = members
                .assessed
                .get(&member.key)
                .filter(|assessment| assessment.receipt_id == attempt.id)
            else {
                continue;
            };
            if self
                .legacy_fingerprint(member, assessment)?
                .is_some_and(|fingerprint| fingerprint == assessment.resulting_fingerprint)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl MemberHost for Host<'_> {
    fn head(&self, branch: &str) -> Result<(String, SourceRevision), AutomationError> {
        // Pilots prepare against the worktree branch. A fetch failure must
        // not fail preparation freshness, and this path does not observe
        // deliveries.
        self.source().local_head(branch)
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
        // per task_id (each local head is several git spawns, and it does not fetch).
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
            if let Some(outcome) = outcome.filter(|outcome| {
                outcome.get("reason").and_then(Value::as_str) == Some(SUPERSEDED_BY_SOURCE)
            }) {
                let detail = outcome
                    .get("detail")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                superseded.insert(
                    member.key.clone(),
                    format!("{SUPERSEDED_BY_SOURCE}: {detail}"),
                );
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

/// The task outcome reason apply and prepare record for a task the branch
/// made stale under its claim; the member settles superseded, not failed.
pub(crate) const SUPERSEDED_BY_SOURCE: &str = "superseded_by_source";

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

/// Recheck the server-issued claim at the deterministic prepare/apply boundary.
///
/// This proves only that the claim is still the consumer's live attempt. Whether
/// the branch moved under a member's material since the claim froze its source
/// is [`stale_tasks`]' per-task answer, which supersedes those members instead
/// of failing the run [ORB-14476].
pub(crate) fn claim(
    runtime: &OrbitRuntime,
    value: &Value,
) -> Result<Option<MemberAttempt>, OrbitError> {
    let Some(claim) = value
        .get("state_automation")
        .filter(|claim| !claim.is_null())
    else {
        return Ok(None);
    };

    let submitted: MemberAttempt = serde_json::from_value(claim.clone())
        .map_err(|e| OrbitError::InvalidInput(e.to_string()))?;

    let active = runtime
        .automation_store()?
        .automation_state(&submitted.consumer)?
        .and_then(|state| state.members)
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

/// Which of `task_ids` the branch made stale since `claim` froze its source,
/// each with the reason; empty while every one is still fresh.
///
/// A drain lands on the integration branch every few minutes, and an operator
/// pull or deploy moves it many commits at once, so its head routinely moves
/// while a pilot runs. A task stays fresh when the head only advanced through
/// commits disjoint from its own prepared material [ORB-12981], judged by the
/// consumer's freshness policy, the one the scheduler fingerprints under
/// [ORB-14476]:
///
/// - `source_sensitivity = any`: every move makes every task stale.
/// - otherwise a task is stale when a changed path lies under one of its
///   context selectors (current, prepared, or `material` the caller is about
///   to write for it), or when a changed `AGENTS.md` / `CLAUDE.md` governs one
///   of those selectors: it sits in the selector's directory or an ancestor.
///   A selector anchored outside the repository is one no commit changes; one
///   with no filesystem anchor at all (`module:`, `command:`) cannot be
///   compared, so its task is stale. When `instructions` is a material field,
///   any instruction change makes every task stale, as it changes every
///   fingerprint.
///
/// A rewritten branch, or a diff that cannot be read, makes every task stale.
/// Incident claims are not tied to the head.
pub(crate) fn stale_tasks(
    runtime: &OrbitRuntime,
    claim: &MemberAttempt,
    policy: &PreparationPolicy,
    prepared: &Value,
    task_ids: &[String],
    material: &BTreeMap<String, Vec<String>>,
) -> Result<BTreeMap<String, String>, OrbitError> {
    if claim.kind != StateTriggerKind::PreparationEligible || task_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let branch = runtime
        .automation_store()?
        .automation_state(&claim.consumer)?
        .ok_or_else(|| OrbitError::InvalidInput("state claim missing".into()))?
        .branch;
    let root = &runtime.paths().repo_root;
    let source = Source::new(root);
    let (_, head) = source
        .local_head(&branch)
        .map_err(automation_error_to_orbit)?;
    let prepared_source = &claim.member.source;
    if head == *prepared_source {
        return Ok(BTreeMap::new());
    }

    let stale = |detail: &str| {
        format!(
            "state-trigger source changed from {} to {}: {detail}",
            prepared_source.commit, head.commit
        )
    };
    let every = |detail: String| {
        Ok(task_ids
            .iter()
            .map(|id| (id.clone(), stale(&detail)))
            .collect())
    };

    if policy.freshness.source_sensitivity == SourceSensitivity::Any {
        return every("source_sensitivity is any".into());
    }
    if source
        .git(&[
            "merge-base",
            "--is-ancestor",
            &prepared_source.commit,
            &head.commit,
        ])
        .is_err()
    {
        return every("the branch no longer descends from the prepared source".into());
    }

    // `--no-renames` reports both sides of a rename; `--relative` keeps paths
    // in the workspace frame the selectors and instruction scan use.
    let changed = match source.git(&[
        "diff",
        "--name-only",
        "--no-renames",
        "--relative",
        "-z",
        &prepared_source.commit,
        &head.commit,
    ]) {
        Ok(changed) => changed,
        Err(error) => return every(format!("changed paths unavailable: {error}")),
    };
    let changed = changed
        .split('\0')
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    let instructions = changed
        .iter()
        .copied()
        .filter(|path| matches!(path.rsplit('/').next(), Some("AGENTS.md" | "CLAUDE.md")))
        .collect::<Vec<_>>();
    if policy.freshness.includes(MaterialField::Instructions)
        && let Some(path) = instructions.first()
    {
        return every(format!("repository instructions `{path}` changed"));
    }

    let prepared_selectors = prepared
        .get("tasks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|task| {
            let id = task.get("task_id")?.as_str()?;
            let selectors = task.get("context_files_before")?.as_array()?;
            Some((id, selectors.iter().filter_map(Value::as_str).collect()))
        })
        .collect::<BTreeMap<&str, Vec<&str>>>();

    let mut stale_tasks = BTreeMap::new();
    for id in task_ids {
        let mut selectors = material.get(id).cloned().unwrap_or_default();
        match runtime.get_task(id) {
            Ok(task) => selectors.extend(task.context_files),
            // The write boundary reports a deleted task stale on its own.
            Err(OrbitError::NotFound { .. }) => {}
            Err(error) => return Err(error),
        }
        if let Some(prepared) = prepared_selectors.get(id.as_str()) {
            selectors.extend(prepared.iter().map(|selector| (*selector).to_owned()));
        }
        selectors.sort();
        selectors.dedup();
        if let Some(detail) = selector_change(root, &selectors, &changed, &instructions) {
            stale_tasks.insert(id.clone(), stale(&detail));
        }
    }

    if stale_tasks.len() < task_ids.len() {
        tracing::info!(
            consumer = %claim.consumer,
            attempt = %claim.id,
            from = %prepared_source.commit,
            to = %head.commit,
            changed = changed.len(),
            stale = stale_tasks.len(),
            "preparation revalidated across a head move disjoint from its material"
        );
    }
    Ok(stale_tasks)
}

/// Why `changed` reaches one of `selectors`, if it does: a path under a
/// selector, or an instruction file on a selector's instruction path.
fn selector_change(
    root: &std::path::Path,
    selectors: &[String],
    changed: &[&str],
    instructions: &[&str],
) -> Option<String> {
    let under = |path: &str, anchor: &str| {
        anchor.is_empty()
            || path == anchor
            || path
                .strip_prefix(anchor)
                .is_some_and(|rest| rest.starts_with('/'))
    };
    for selector in selectors {
        let Some(anchor) = repository_anchor(root, selector) else {
            return Some(format!(
                "context selector `{selector}` has no repository path to compare"
            ));
        };
        let Some(anchor) = anchor else {
            continue;
        };
        if let Some(path) = changed.iter().find(|path| under(path, &anchor)) {
            return Some(format!(
                "`{path}` changed under prepared context selector `{selector}`"
            ));
        }
        if let Some(path) = instructions.iter().find(|path| {
            let directory = path.rsplit_once('/').map_or("", |(directory, _)| directory);
            under(&anchor, directory)
        }) {
            return Some(format!(
                "repository instructions `{path}` govern prepared context selector `{selector}`"
            ));
        }
    }
    None
}

/// The workspace-relative path a context selector anchors (`""` for the
/// root), `Some(None)` for an anchor outside the repository, which no commit
/// can change, and `None` when the selector has no filesystem anchor at all
/// (`module:`, `command:`, unparseable input).
fn repository_anchor(root: &std::path::Path, selector: &str) -> Option<Option<String>> {
    let anchor = orbit_common::fs::selector::anchor_path(selector).ok()?;
    let relative = if anchor.is_absolute() {
        let canonical = root.canonicalize().ok();
        match anchor
            .strip_prefix(root)
            .ok()
            .or_else(|| anchor.strip_prefix(canonical.as_deref()?).ok())
        {
            Some(relative) => relative.to_path_buf(),
            None => return Some(None),
        }
    } else {
        anchor
    };
    if relative.starts_with("..") {
        return Some(None);
    }
    let relative = relative.to_string_lossy();
    Some(Some(if relative == "." {
        String::new()
    } else {
        relative.into_owned()
    }))
}
