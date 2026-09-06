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
pub(crate) mod preparation;
mod provider;
pub(crate) use direct::record_direct_landing_intent;
mod source;
pub use inspect::{inspect_auto_task, inspect_routine};
mod task;
#[cfg(test)]
mod tests;

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
        deliveries_landed: trigger,
    } = &definition.schedule
    else {
        return Err(OrbitError::InvalidInput("not a delivery definition".into()));
    };
    let epoch = delivery::definition_epoch(&(
        &definition.schedule,
        &definition.template,
        definition.dedupe,
    ))
    .map_err(automation_error_to_orbit)?;
    evaluate(
        runtime,
        Action::Task(definition),
        delivery::Evaluation {
            consumer: &consumer_key(runtime, "auto-task", &definition.name)?,
            epoch: &epoch,
            trigger,
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
    let trigger = definition
        .trigger
        .deliveries_landed
        .as_ref()
        .ok_or_else(|| OrbitError::InvalidInput("not a delivery routine".into()))?;
    let mut effective_trigger = trigger.clone();
    effective_trigger.retries = effective_trigger.retries.min(definition.policy.retries.max);
    let epoch =
        delivery::definition_epoch(&(&definition.trigger, &definition.target, &definition.policy))
            .map_err(automation_error_to_orbit)?;
    evaluate(
        runtime,
        Action::Job(definition),
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
    mut request: delivery::Evaluation<'_>,
) -> Result<AutomationDiagnostic, OrbitError> {
    let owner = request.trigger.owner_machine.as_deref();
    let owned_here =
        owner.is_some_and(|owner| Some(owner) == runtime.automation_machine_identity());
    let owned_elsewhere =
        owner.is_some_and(|owner| Some(owner) != runtime.automation_machine_identity());
    // Disable admission on another owner, but reconcile previously admitted work.
    // Preview remains read-only and exposes the actual consumer/baseline.
    request.enabled &= owned_here;
    let dry_run = request.dry_run;
    let store = runtime.automation_store()?;
    let host = Host {
        runtime,
        action,
        source: source::Source::new(&runtime.paths().repo_root),
    };
    let mut diagnostic =
        delivery::evaluate(store.as_ref(), &host, request).map_err(automation_error_to_orbit)?;
    if owned_elsewhere && !dry_run {
        diagnostic.reason = "owned_elsewhere".into();
    }
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
        Ok(page)
    }
    fn admit(&self, attempt: &BatchAttempt) -> Result<String, AutomationError> {
        self.runtime.ensure_coordination_task_write_permitted()?;
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
                .map(|r| r.run_id)
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
    {
        return Ok(Some("open_instance".into()));
    }
    Ok(None)
}
