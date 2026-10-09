//! The state consumer's entry point and its per-evaluation caches.

use super::super::{
    consumer_key, pins, preparation, preparation::InstructionSnapshot, source::Source,
};
use crate::OrbitRuntime;
use chrono::{DateTime, Utc};
use orbit_automation::{
    AutomationError, automation_error_to_orbit,
    delivery::definition_epoch,
    members::{self, MemberEvaluation},
};
use orbit_common::OrbitError;
use orbit_types::workflow::{
    RoutineDefinition,
    automation::{members::*, *},
};
use std::cell::RefCell;
use std::collections::BTreeMap;

use super::Host;

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
            incidents: RefCell::new(super::super::incidents::IncidentSession::new()),
            instructions: RefCell::new(BTreeMap::new()),
            pinned: RefCell::new(BTreeMap::new()),
            owner: RefCell::new(None),
            head: RefCell::new(None),
        }
    }

    pub(super) fn source(&self) -> Source<'_> {
        Source::new(&self.runtime.paths().repo_root)
    }

    pub(super) fn owner(&self) -> Result<pins::Owner, AutomationError> {
        if let Some(owner) = self.owner.borrow().as_ref() {
            return Ok(owner.clone());
        }
        let owner = pins::Owner::of(self.runtime, &self.source())?;
        *self.owner.borrow_mut() = Some(owner.clone());
        Ok(owner)
    }

    pub(super) fn eligibility(&self) -> &PreparationEligibility {
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

    pub(super) fn fingerprint(
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
    pub(super) fn legacy_fingerprint(
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
    pub(super) fn has_legacy_assessment(
        &self,
        attempt: &MemberAttempt,
    ) -> Result<bool, AutomationError> {
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
