use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;

use super::super::loader::LoadedRoutine;
use super::contract::RoutineSweepReport;

pub(super) fn duration_from_minutes(minutes: u64, field: &str) -> Result<Duration, OrbitError> {
    let minutes = i64::try_from(minutes).map_err(|_| {
        OrbitError::InvalidInput(format!("routine {field} is too large for a duration"))
    })?;

    Duration::try_minutes(minutes).ok_or_else(|| {
        OrbitError::InvalidInput(format!("routine {field} is too large for a duration"))
    })
}

pub(super) fn skipped(routine: &LoadedRoutine, reason: &str) -> RoutineSweepReport {
    RoutineSweepReport {
        routine: routine.definition.name.clone(),
        source: routine.source_workspace.clone(),
        origin: routine.origin.as_str(),
        action: "skipped",
        reason: Some(reason.to_string()),
        slot: None,
        run_id: None,
        batch: Vec::new(),
    }
}

pub(super) fn failure_report(routine: &LoadedRoutine, reason: String) -> RoutineSweepReport {
    RoutineSweepReport {
        routine: routine.definition.name.clone(),
        source: routine.source_workspace.clone(),
        origin: routine.origin.as_str(),
        action: "error",
        reason: Some(reason),
        slot: None,
        run_id: None,
        batch: Vec::new(),
    }
}

pub(super) fn action(routine: &LoadedRoutine, action: &'static str) -> RoutineSweepReport {
    RoutineSweepReport {
        routine: routine.definition.name.clone(),
        source: routine.source_workspace.clone(),
        origin: routine.origin.as_str(),
        action,
        reason: None,
        slot: None,
        run_id: None,
        batch: Vec::new(),
    }
}

pub(super) fn parse_rfc3339(raw: &str) -> Result<DateTime<Utc>, OrbitError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| OrbitError::Store(format!("invalid stored timestamp '{raw}': {error}")))
}
