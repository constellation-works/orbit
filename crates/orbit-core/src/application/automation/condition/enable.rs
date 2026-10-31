use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use orbit_automation::condition::{ConditionCarrier, Truth};
use orbit_common::OrbitError;
use orbit_types::workflow::automation::{ConditionAudit, ConditionOnEnable, ConditionState};
use serde::Serialize;

use super::evaluate::{ConditionDefinition, condition_consumer};
use super::host;
use crate::OrbitRuntime;

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
