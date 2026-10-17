//! Freshness of enabled cron routines against their own schedule.
//!
//! A routine is stale when the time since its newest successful fire — or,
//! with no success yet, since this host first observed it — is strictly
//! longer than three times the cron gap around now. Disabled, paused, and
//! non-cron routines are not judged. A routine this host has never observed
//! and that has never succeeded is not stale.

use chrono::{DateTime, Duration, Local, Utc};
use orbit_common::OrbitError;
use orbit_store::Store;

use super::due::{parse_cron, schedule_interval};
use super::status::checkout_routine_statuses;
use crate::OrbitRuntime;

/// How many schedule intervals may pass after the freshness anchor before
/// doctor warns. Exactly this multiple is still fresh.
const STALE_INTERVAL_MULTIPLE: i32 = 3;

/// One enabled cron routine whose last success is older than
/// three times its current schedule gap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleRoutine {
    /// Routine name, as `orbit routine list` prints it.
    pub name: String,
    /// Time since the freshness anchor.
    pub age: Duration,
    /// Cron gap around the judgment instant.
    pub interval: Duration,
}

/// Enabled, unpaused cron routines on this checkout that are past 3× interval.
pub fn stale_cron_routines(
    runtime: &OrbitRuntime,
    now: DateTime<Utc>,
) -> Result<Vec<StaleRoutine>, OrbitError> {
    let report = checkout_routine_statuses(runtime)?;
    let database = orbit_config::resolved_audit_db_path(&orbit_config::ConfigRoots::global_only(
        runtime.global_root(),
    ))?;
    let store = if database.exists() {
        Some(Store::open_read_only(&database)?)
    } else {
        None
    };
    let now_local = now.with_timezone(&Local);
    let mut stale = Vec::new();
    for status in &report.statuses {
        let definition = &status.routine.definition;
        if !definition.enabled || status.paused_at.is_some() {
            continue;
        }
        let trigger = &definition.trigger;
        if trigger.cron.is_empty() || trigger.state.is_some() || trigger.deliveries_landed.is_some()
        {
            continue;
        }
        let cron = parse_cron(&trigger.cron)?;
        let interval = schedule_interval(&cron, &now_local)?;
        let threshold = interval
            .checked_mul(STALE_INTERVAL_MULTIPLE)
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "routine '{}' interval is too large to judge freshness",
                    definition.name
                ))
            })?;
        let anchor = freshness_anchor(
            &store,
            &definition.name,
            status.first_observed_at.as_deref(),
        )?;
        let Some(anchor) = anchor else {
            continue;
        };
        let age = now.signed_duration_since(anchor);
        if age > threshold {
            stale.push(StaleRoutine {
                name: definition.name.clone(),
                age,
                interval,
            });
        }
    }
    stale.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(stale)
}

fn freshness_anchor(
    store: &Option<Store>,
    name: &str,
    first_observed_at: Option<&str>,
) -> Result<Option<DateTime<Utc>>, OrbitError> {
    let Some(store) = store else {
        return Ok(None);
    };
    if let Some(fire) = store.routine_latest_succeeded_fire(name)? {
        return Ok(Some(parse_timestamp(&fire.updated_at)?));
    }
    first_observed_at.map(parse_timestamp).transpose()
}

fn parse_timestamp(raw: &str) -> Result<DateTime<Utc>, OrbitError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| OrbitError::Store(format!("invalid stored timestamp '{raw}': {error}")))
}
