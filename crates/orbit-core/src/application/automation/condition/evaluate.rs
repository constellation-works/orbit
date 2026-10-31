use chrono::{DateTime, Utc};
use orbit_automation::automation_error_to_orbit;
use orbit_automation::condition::{
    CheckedCondition, ConditionCarrier, ConditionDiagnostic, ConditionEvaluation,
    ConditionEvaluator, GuardVerdict, Truth, check_condition, check_guard, condition_epoch,
    evaluate_condition,
};
use orbit_common::OrbitError;
use orbit_types::workflow::automation::ConditionTrigger;
use orbit_types::workflow::{AutoTaskDefinition, AutoTaskSchedule, RoutineDefinition};

use super::super::consumer_key;
use super::facts;
use super::host::{self, Target};
use crate::OrbitRuntime;

/// The clock cadence a hold's continuity limit is measured against.
fn cadence_seconds(runtime: &OrbitRuntime) -> u64 {
    crate::application::routines::clock::load_clock_settings(&runtime.global_root())
        .map(|settings| settings.cadence_seconds)
        .unwrap_or(60)
}

/// Why this checkout may not fire a workspace or task condition: a replica,
/// a workspace owned by another host, or an unresolved owner (§4).
pub(crate) fn condition_refusal(runtime: &OrbitRuntime) -> Option<&'static str> {
    if runtime.coordination_write_owner().is_some() {
        return Some("replica_checkout");
    }
    super::super::ownership::resolve(runtime, None).refusal()
}

/// The consumer key of a condition definition.
pub(crate) fn condition_consumer(
    runtime: &OrbitRuntime,
    carrier: ConditionCarrier,
    name: &str,
) -> Result<String, OrbitError> {
    consumer_key(runtime, carrier.kind(), name)
}

/// A definition carrying a condition, with what it fires.
pub(crate) struct ConditionDefinition<'a> {
    pub(crate) carrier: ConditionCarrier,
    pub(crate) name: &'a str,
    pub(crate) enabled: bool,
    pub(crate) condition: &'a ConditionTrigger,
    pub(crate) target: Target<'a>,
}

impl<'a> ConditionDefinition<'a> {
    pub(crate) fn of_routine(definition: &'a RoutineDefinition) -> Option<Self> {
        Some(Self {
            carrier: ConditionCarrier::Routine,
            name: &definition.name,
            enabled: definition.enabled,
            condition: definition.trigger.condition.as_ref()?,
            target: Target::Job(definition),
        })
    }

    pub(crate) fn of_auto_task(definition: &'a AutoTaskDefinition) -> Option<Self> {
        let AutoTaskSchedule::Condition { condition } = &definition.schedule else {
            return None;
        };
        Some(Self {
            carrier: ConditionCarrier::AutoTask,
            name: &definition.name,
            enabled: definition.enabled,
            condition,
            target: Target::Mint(definition),
        })
    }

    /// The epoch: the condition's meaning plus what it fires. A template or
    /// target edit is an epoch change and baselines again.
    pub(crate) fn epoch(&self) -> Result<String, OrbitError> {
        match self.target {
            Target::Mint(definition) => {
                condition_epoch(self.condition, &("mint", &definition.template))
            }
            Target::Job(definition) => {
                condition_epoch(self.condition, &("job", definition.target.job_name()))
            }
        }
        .map_err(automation_error_to_orbit)
    }

    pub(crate) fn checked(&self) -> Result<CheckedCondition, OrbitError> {
        check_condition(self.condition, self.carrier).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "{} '{}' does not load: {error}",
                self.carrier.kind(),
                self.name
            ))
        })
    }
}

/// One firing pass for a condition definition on this host.
pub(crate) fn evaluate_condition_definition(
    runtime: &OrbitRuntime,
    definition: &ConditionDefinition<'_>,
    dry_run: bool,
    now: DateTime<Utc>,
) -> Result<ConditionDiagnostic, OrbitError> {
    let checked = definition.checked()?;
    let consumer = condition_consumer(runtime, definition.carrier, definition.name)?;
    let epoch = definition.epoch()?;
    let host = host::Host {
        runtime,
        checked: &checked,
        condition: definition.condition,
        target: definition.target,
        consumer: &consumer,
    };
    evaluate_condition(
        runtime.automation_store()?.as_ref(),
        &host,
        ConditionEvaluation {
            consumer: &consumer,
            epoch: &epoch,
            trigger: definition.condition,
            enabled: definition.enabled,
            refusal: condition_refusal(runtime),
            dry_run,
            now,
            cadence_seconds: cadence_seconds(runtime),
        },
    )
    .map_err(automation_error_to_orbit)
}

pub(crate) fn evaluate_routine_condition(
    runtime: &OrbitRuntime,
    definition: &RoutineDefinition,
    dry_run: bool,
    now: DateTime<Utc>,
) -> Result<ConditionDiagnostic, OrbitError> {
    let found = ConditionDefinition::of_routine(definition)
        .ok_or_else(|| OrbitError::InvalidInput("not a condition routine".into()))?;
    evaluate_condition_definition(runtime, &found, dry_run, now)
}

pub(crate) fn evaluate_auto_task_condition(
    runtime: &OrbitRuntime,
    definition: &AutoTaskDefinition,
    dry_run: bool,
    now: DateTime<Utc>,
) -> Result<ConditionDiagnostic, OrbitError> {
    let found = ConditionDefinition::of_auto_task(definition)
        .ok_or_else(|| OrbitError::InvalidInput("not a condition auto-task".into()))?;
    evaluate_condition_definition(runtime, &found, dry_run, now)
}

/// Evaluate a `when:` guard over the workspace at a due moment. A guard that
/// does not check or cannot be read is unknown, which holds the fire.
pub(crate) fn evaluate_guard(
    runtime: &OrbitRuntime,
    source: &str,
    clause: &str,
    carrier: ConditionCarrier,
    now: DateTime<Utc>,
) -> GuardVerdict {
    let checked = match check_guard(source, clause, carrier) {
        Ok(checked) => checked,
        Err(error) => return GuardVerdict::unknown(error.to_string()),
    };
    let gathered = match facts::gather(runtime, &checked.reads, now, &facts::FactInputs::default())
    {
        Ok(gathered) => gathered,
        Err(error) => return GuardVerdict::unknown(error.to_string()),
    };
    let truth = ConditionEvaluator::new(&gathered.facts).truth(&checked.when, None);
    match (&truth, gathered.unavailable.first()) {
        (Truth::Unknown(reason), Some(unavailable)) => {
            GuardVerdict::unknown(format!("{reason} ({unavailable})"))
        }
        _ => GuardVerdict::from_truth(source, truth),
    }
}
