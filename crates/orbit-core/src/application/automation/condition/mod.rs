//! Condition triggers on the workspace owner
//! (`docs/design/condition-triggers/1_scope.md`): one pass of the firing
//! protocol per definition per clock tick, `when:` guards beside the typed
//! kinds, the enable dry run, `clear-stall`, and the read projections that
//! explain, lists and the dashboard show. Task and workspace scopes only.

pub(crate) mod facts;
mod host;
mod status;

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use orbit_automation::automation_error_to_orbit;
use orbit_automation::condition::{
    CheckedCondition, ConditionCarrier, ConditionDiagnostic, ConditionEvaluation,
    ConditionEvaluator, GuardVerdict, Truth, check_condition, check_guard, condition_epoch,
    evaluate_condition,
};
use orbit_common::OrbitError;
use orbit_types::workflow::automation::{
    ConditionAudit, ConditionOnEnable, ConditionState, ConditionTrigger,
};
use orbit_types::workflow::{AutoTaskDefinition, AutoTaskSchedule, RoutineDefinition};
use serde::Serialize;

pub(crate) use host::{Target, render_placeholders};
pub(crate) use status::condition_status;
pub use status::{ConditionChainLink, ConditionFireView, ConditionListing, ConditionStatus};

use super::consumer_key;
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
    super::ownership::resolve(runtime, None).refusal()
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

/// What enabling a condition definition would do now (§9): the subjects
/// true now, baselined by default or fired under `on_enable: fire_existing`.
#[derive(Debug, Clone, Serialize)]
pub struct ConditionEnablePreview {
    /// `routine` or `auto-task`.
    pub kind: &'static str,
    pub name: String,
    pub scope: &'static str,
    /// `baseline` or `fire_existing`.
    pub on_enable: &'static str,
    pub max_matches: u32,
    /// Subjects true now, at most 20.
    pub matches: Vec<String>,
    pub matched: usize,
    pub unknown: usize,
    /// True when the matches exceed `max_matches`: enabling is refused.
    pub refused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    /// Subjects enabling would fire (`fire_existing`) or record as already
    /// fired without an action (`baseline`).
    pub would_fire: Vec<String>,
    pub would_baseline: Vec<String>,
    /// Board values that could not be read.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unavailable: Vec<String>,
}

/// Preview an enable, an epoch change or `fire_existing` without writing.
pub(crate) fn enable_preview(
    runtime: &OrbitRuntime,
    definition: &ConditionDefinition<'_>,
    now: DateTime<Utc>,
) -> Result<ConditionEnablePreview, OrbitError> {
    let checked = definition.checked()?;
    // A host without a registered identity never fires the definition, but
    // its enable is still previewed against an empty history.
    let (consumer, state) = match condition_consumer(runtime, definition.carrier, definition.name) {
        Ok(consumer) => {
            let state = runtime.automation_store()?.condition_state(&consumer)?;
            (consumer, state)
        }
        Err(_) => (
            format!(
                "unregistered/{}/{}",
                definition.carrier.kind(),
                definition.name
            ),
            None,
        ),
    };
    let state = state.unwrap_or_else(|| ConditionState::new(&consumer, ""));
    let host = host::Host {
        runtime,
        checked: &checked,
        condition: definition.condition,
        target: definition.target,
        consumer: &consumer,
    };
    // Enable sees every selected subject, not one lap.
    let mut full = state.clone();
    full.lap_after = None;
    let mut matches = Vec::new();
    // Tracked subjects are observed in every lap, so count each key once.
    let mut unknown = BTreeSet::new();
    let mut unavailable = Vec::new();
    loop {
        let (observation, missing) = host.observe_view(&full, now)?;
        unavailable.extend(missing);
        for subject in &observation.subjects {
            match subject.when {
                Truth::True if !matches.contains(&subject.key) => matches.push(subject.key.clone()),
                Truth::Unknown(_) => {
                    unknown.insert(subject.key.clone());
                }
                _ => {}
            }
        }
        match observation.lap_after {
            Some(after) if !observation.lap_complete => full.lap_after = Some(after),
            _ => break,
        }
    }
    unavailable.sort();
    unavailable.dedup();
    let fire_existing = definition.condition.on_enable == ConditionOnEnable::FireExisting;
    let refused = matches.len() > definition.condition.max_matches as usize;
    Ok(ConditionEnablePreview {
        kind: definition.carrier.kind(),
        name: definition.name.to_string(),
        scope: definition.condition.scope.as_str(),
        on_enable: if fire_existing {
            "fire_existing"
        } else {
            "baseline"
        },
        max_matches: definition.condition.max_matches,
        matched: matches.len(),
        unknown: unknown.len(),
        refused,
        refusal: refused.then(|| {
            format!(
                "too_many_matches: {} subjects match now and max_matches is {}; nothing was \
                 enabled",
                matches.len(),
                definition.condition.max_matches
            )
        }),
        would_fire: if fire_existing && !refused {
            matches.iter().take(20).cloned().collect()
        } else {
            Vec::new()
        },
        would_baseline: if fire_existing || refused {
            Vec::new()
        } else {
            matches.iter().take(20).cloned().collect()
        },
        matches: matches.into_iter().take(20).collect(),
        unavailable,
    })
}

/// Record that a definition was disabled, so its next enable baselines even
/// if no clock pass saw it disabled. Best effort: a pass that sees the
/// disable records the same.
pub(crate) fn record_condition_disabled(
    runtime: &OrbitRuntime,
    carrier: ConditionCarrier,
    name: &str,
) {
    let result = (|| -> Result<(), OrbitError> {
        let consumer = condition_consumer(runtime, carrier, name)?;
        let store = runtime.automation_store()?;
        let Some(state) = store.condition_state(&consumer)? else {
            return Ok(());
        };
        if !state.enabled {
            return Ok(());
        }
        let mut next = state.clone();
        next.enabled = false;
        next.generation += 1;
        store.condition_commit(&state, &next)?;
        Ok(())
    })();
    if let Err(error) = result {
        tracing::warn!(%name, %error, "condition disable not recorded; the next pass records it");
    }
}

/// Forget a deleted definition's condition state (`self` with it).
pub(crate) fn forget_condition_state(
    runtime: &OrbitRuntime,
    carrier: ConditionCarrier,
    name: &str,
) {
    let result = condition_consumer(runtime, carrier, name)
        .and_then(|consumer| runtime.automation_store()?.condition_delete(&consumer));
    if let Err(error) = result {
        tracing::warn!(%name, %error, "condition state not forgotten");
    }
}

/// The result of clearing a stall.
#[derive(Debug, Clone, Serialize)]
pub struct ConditionStallCleared {
    pub consumer: String,
    pub cleared: orbit_types::workflow::automation::ConditionStall,
    pub audit: ConditionAudit,
}

impl OrbitRuntime {
    /// The enable dry run of a condition auto-task (§9): what enabling it now
    /// would baseline or fire, and whether `max_matches` refuses it. Writes
    /// nothing.
    pub fn auto_task_enable_preview(
        &self,
        name: &str,
    ) -> Result<ConditionEnablePreview, OrbitError> {
        let definition = self
            .auto_task_show(name)?
            .ok_or_else(|| OrbitError::InvalidInput(format!("no such auto-task '{name}'")))?;
        let condition = ConditionDefinition::of_auto_task(&definition).ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "auto-task '{name}' has no condition; --dry-run previews condition definitions \
                 only"
            ))
        })?;
        enable_preview(self, &condition, Utc::now())
    }

    /// Clear a condition's stall (§8, §9): the operator's acknowledgement
    /// that a storm, loop or oscillation was looked at. Audited, fenced, and
    /// refused off the owner. The next pass evaluates normally; a cause that
    /// persists stalls it again.
    pub fn clear_condition_stall(
        &self,
        selector: &str,
        reason: &str,
    ) -> Result<ConditionStallCleared, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        if let Some(refusal) = condition_refusal(self) {
            return Err(OrbitError::InvalidInput(format!(
                "clear-stall runs on the workspace owner; this checkout is {refusal}"
            )));
        }
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(OrbitError::InvalidInput(
                "clear-stall needs a reason for the audit record".into(),
            ));
        }
        let found = self.condition_definition_for(selector)?;
        let consumer = condition_consumer(self, found.carrier, &found.name)?;
        let store = self.automation_store()?;
        let state = store.condition_state(&consumer)?.ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "{} '{}' has no condition state yet; nothing is stalled",
                found.carrier.kind(),
                found.name
            ))
        })?;
        let Some(stall) = state.stall.clone() else {
            return Err(OrbitError::InvalidInput(format!(
                "{} '{}' is not stalled",
                found.carrier.kind(),
                found.name
            )));
        };
        let audit = ConditionAudit {
            consumer: consumer.clone(),
            action: "clear_stall".into(),
            actor: self.actor().resolve_write_label(None, None)?,
            at: Utc::now(),
            reason: reason.to_string(),
            stall: Some(stall.clone()),
        };
        let mut next = state.clone();
        next.stall = None;
        next.generation += 1;
        if !store.condition_commit_audited(&state, &next, &audit)? {
            return Err(OrbitError::InvalidInput(
                "the condition moved while clearing; refresh and retry".into(),
            ));
        }
        Ok(ConditionStallCleared {
            consumer,
            cleared: stall,
            audit,
        })
    }
}
