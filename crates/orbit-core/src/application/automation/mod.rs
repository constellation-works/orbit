//! Core composition of source facts, existing lifecycle actions and evidence.

use crate::OrbitRuntime;
use chrono::{DateTime, Utc};
use orbit_automation::delivery::{self, ActionOutcome, DeliveryHost};
use orbit_automation::{AutomationError, automation_error_to_orbit};
use orbit_common::OrbitError;
use orbit_types::workflow::automation::*;
use orbit_types::workflow::{AutoTaskDefinition, AutoTaskSchedule, RoutineDefinition};

mod direct;
pub(crate) mod incidents;
mod inspect;
pub(crate) mod members;
mod ownership;
pub(crate) mod preparation;
mod provider;
pub(crate) mod source;
mod task;
#[cfg(test)]
mod tests;

pub(crate) use direct::record_direct_landing_intent;
pub use inspect::{inspect_auto_task, inspect_routine};

pub const COVERAGE_ARTIFACT: &str = "automation-coverage.json";

/// Identity is machine/workspace-qualified in the authoritative host database.
pub fn consumer_key(runtime: &OrbitRuntime, kind: &str, name: &str) -> Result<String, OrbitError> {
    let machine = runtime.automation_machine_identity().ok_or_else(|| {
        OrbitError::InvalidInput(
            "delivery automation requires a registered machine identity".into(),
        )
    })?;
    Ok(format!(
        "{machine}/{}/{kind}/{name}",
        runtime.workspace_id()?
    ))
}

pub fn evaluate_auto_task(
    runtime: &OrbitRuntime,
    definition: &AutoTaskDefinition,
    dry_run: bool,
    now: DateTime<Utc>,
) -> Result<AutomationDiagnostic, OrbitError> {
    let AutoTaskSchedule::Deliveries {
        deliveries_landed: declared,
    } = &definition.schedule
    else {
        return Err(OrbitError::InvalidInput("not a delivery definition".into()));
    };

    let ownership = ownership::resolve(runtime, declared.owner_machine.as_deref());
    let trigger = ownership::with_resolved_owner(declared, &ownership);
    let epoch = ownership::auto_task_epoch(definition, &trigger)?;

    evaluate(
        runtime,
        Action::Task(definition),
        ownership,
        delivery::Evaluation {
            consumer: &consumer_key(runtime, "auto-task", &definition.name)?,
            epoch: &epoch,
            trigger: &trigger,
            enabled: definition.enabled,
            dry_run,
            now,
        },
    )
}

pub fn evaluate_routine(
    runtime: &OrbitRuntime,
    definition: &RoutineDefinition,
    dry_run: bool,
    now: DateTime<Utc>,
) -> Result<AutomationDiagnostic, OrbitError> {
    if definition.trigger.state.is_some() {
        return members::evaluate(runtime, definition, dry_run, now);
    }

    let declared = definition
        .trigger
        .deliveries_landed
        .as_ref()
        .ok_or_else(|| OrbitError::InvalidInput("not a delivery routine".into()))?;

    let ownership = ownership::resolve(runtime, declared.owner_machine.as_deref());
    let mut effective_trigger = ownership::with_resolved_owner(declared, &ownership);
    let epoch = ownership::routine_epoch(definition, &effective_trigger)?;

    // The routine's retry policy caps whatever the trigger asks for.
    effective_trigger.retries = effective_trigger.retries.min(definition.policy.retries.max);

    evaluate(
        runtime,
        Action::Job(definition),
        ownership,
        delivery::Evaluation {
            consumer: &consumer_key(runtime, "routine", &definition.name)?,
            epoch: &epoch,
            trigger: &effective_trigger,
            enabled: definition.enabled,
            dry_run,
            now,
        },
    )
}

fn evaluate(
    runtime: &OrbitRuntime,
    action: Action<'_>,
    ownership: DeliveryOwnership,
    mut request: delivery::Evaluation<'_>,
) -> Result<AutomationDiagnostic, OrbitError> {
    let configured_enabled = request.enabled;
    // Only the owner admits new work; any other host still reconciles what it
    // already admitted. Preview remains read-only and exposes the actual
    // consumer/baseline.
    request.enabled &= ownership.owned_here;

    let store = runtime.automation_store()?;
    let host = Host {
        runtime,
        action,
        source: source::Source::new(&runtime.paths().repo_root),
    };

    let mut diagnostic =
        delivery::evaluate(store.as_ref(), &host, request).map_err(automation_error_to_orbit)?;

    // An enabled definition this host cannot admit for reports why, so
    // `disabled` keeps meaning the operator disabled it. Preview reports the
    // same refusal rather than promising an admission that cannot happen.
    if configured_enabled
        && diagnostic.reason != delivery::DEFINITION_CHANGED
        && let Some(refusal) = ownership.refusal()
    {
        diagnostic.reason = refusal.into();
    }
    diagnostic.ownership = Some(ownership);

    Ok(diagnostic)
}

enum Action<'a> {
    Task(&'a AutoTaskDefinition),
    Job(&'a RoutineDefinition),
}

struct Host<'a> {
    runtime: &'a OrbitRuntime,
    action: Action<'a>,
    source: source::Source<'a>,
}

impl DeliveryHost for Host<'_> {
    fn admission_deferral(&self) -> Result<Option<String>, AutomationError> {
        if let Action::Task(definition) = self.action {
            return auto_task_admission_deferral(self.runtime, definition).map_err(Into::into);
        }

        Ok(None)
    }

    fn head(&self, branch: &str) -> Result<(String, SourceRevision), AutomationError> {
        self.source.head(branch)
    }

    fn observe(
        &self,
        branch: &str,
        state: &AutomationState,
    ) -> Result<SourcePage, AutomationError> {
        let mut page = self.source.observe(branch, state)?;
        direct::observe(
            &self.source,
            self.runtime.automation_store()?.as_ref(),
            state,
            &mut page,
        )?;
        // [ORB-11333] Accepted before-PR certificates become exclusions only
        // after the shared rule proves the landed trees; the evaluator then
        // applies them for review consumers alone.
        crate::application::review::exclusions(self.runtime, &self.source, state, &mut page)?;

        Ok(page)
    }

    fn admit(&self, attempt: &BatchAttempt) -> Result<String, AutomationError> {
        self.runtime.ensure_coordination_task_write_permitted()?;

        // The claim this host was handed must still be the one recorded.
        let state = self
            .runtime
            .automation_store()?
            .automation_state(&attempt.batch.consumer)?
            .ok_or_else(|| AutomationError::Deferred("claim_missing".into()))?;
        if state.active.as_ref() != Some(attempt) {
            return Err(AutomationError::Deferred("claim_superseded".into()));
        }

        self.source.retain_batch(&attempt.batch)?;

        match self.action {
            Action::Task(definition) => {
                task::mint(self.runtime, definition, attempt).map_err(Into::into)
            }
            Action::Job(definition) => self
                .runtime
                .submit_automation_pipeline_run(
                    definition.target.job_name(),
                    serde_json::json!({"automation":attempt}),
                    &attempt.action_key,
                )
                .map(|run| run.run_id)
                .map_err(Into::into),
        }
    }

    fn outcome(&self, attempt: &BatchAttempt) -> Result<ActionOutcome, AutomationError> {
        match self.action {
            Action::Task(_) => task::outcome(self.runtime, &self.source, attempt),
            Action::Job(_) => task::job_outcome(self.runtime, &self.source, attempt),
        }
    }
}

fn auto_task_admission_deferral(
    runtime: &OrbitRuntime,
    definition: &AutoTaskDefinition,
) -> Result<Option<String>, OrbitError> {
    if definition.dedupe == orbit_types::workflow::DedupePolicy::SkipIfOpen
        && orbit_automation::auto_tasks::scheduler::AutoTaskDispatch::has_open_instance(
            runtime, definition,
        )?
        .is_some()
    {
        return Ok(Some("open_instance".into()));
    }

    Ok(None)
}
