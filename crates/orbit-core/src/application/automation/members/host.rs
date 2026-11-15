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
    evaluate_with_cache(runtime, definition, dry_run, now, None)
}

pub(crate) fn evaluate_with_cache(
    runtime: &OrbitRuntime,
    definition: &RoutineDefinition,
    dry_run: bool,
    now: DateTime<Utc>,
    cache: Option<&super::super::source_cache::SourceCache>,
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

    let mut host = Host::new(runtime, &definition.name, &effective, dry_run);
    host.inspection_cache = cache;
    host.guard = definition.trigger.when.clone();
    members::evaluate(
        runtime.automation_store()?.as_ref(),
        &host,
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
        dry_run: bool,
    ) -> Self {
        Self {
            runtime,
            routine,
            trigger,
            dry_run,
            policy: preparation::resolve_policy(runtime, Some(trigger)),
            incidents: RefCell::new(super::super::incidents::IncidentSession::new()),
            instructions: RefCell::new(BTreeMap::new()),
            owner: RefCell::new(None),
            head: RefCell::new(None),
            inspection_cache: None,
            pilot_hold: RefCell::new(None),
            resident_now: std::cell::Cell::new(None),
            guard: None,
            guard_verdict: RefCell::new(None),
        }
    }

    pub(super) fn source(&self) -> Source<'_> {
        let root = &self.runtime.paths().repo_root;
        if self.dry_run {
            match self.inspection_cache {
                Some(cache) => Source::read_only_with_cache(root, cache),
                None => Source::read_only(root),
            }
        } else {
            Source::new(root)
        }
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
}
