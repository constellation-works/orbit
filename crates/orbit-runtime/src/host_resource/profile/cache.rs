//! Profiles kept across admission passes until a delivery finishes.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{DeliveryRunsFreshness, ResourceUsageStoreBackend};

use super::{ProfileBounds, ProfileSet, build_profiles, template_task_ids};

/// Which of the given template task ids carry `no-diff-expected`.
pub(crate) type NoDiffTasks<'a> =
    &'a dyn Fn(&BTreeSet<String>) -> Result<BTreeSet<String>, OrbitError>;

struct Cached {
    freshness: DeliveryRunsFreshness,
    bounds: ProfileBounds,
    crew_models: BTreeMap<String, String>,
    profiles: Arc<ProfileSet>,
}

/// The host's profiles, rebuilt only when the store's freshness probe (the
/// newest finish and count of finished sampled runs), the bounds or the
/// configured crews' models change.
///
/// Only profiles live here. What a forecast reads about running deliveries,
/// reservations and the baseline is read fresh every pass, or two drains on
/// one host would fill the same gap.
#[derive(Default)]
pub(crate) struct ResourceProfileCache {
    slot: Mutex<Option<Cached>>,
}

impl ResourceProfileCache {
    /// The current profiles.
    pub(crate) fn profiles(
        &self,
        store: &dyn ResourceUsageStoreBackend,
        no_diff_tasks: NoDiffTasks<'_>,
        crew_models: BTreeMap<String, String>,
        bounds: ProfileBounds,
        now: DateTime<Utc>,
    ) -> Result<Arc<ProfileSet>, OrbitError> {
        let freshness = store.delivery_runs_freshness()?;
        // Held while building, so concurrent passes build once.
        let mut slot = self
            .slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(cached) = slot.as_ref()
            && cached.freshness == freshness
            && cached.bounds == bounds
            && cached.crew_models == crew_models
        {
            return Ok(Arc::clone(&cached.profiles));
        }
        let profiles = Arc::new(build_uncached(
            store,
            no_diff_tasks,
            crew_models.clone(),
            bounds,
            now,
        )?);
        *slot = Some(Cached {
            freshness,
            bounds,
            crew_models,
            profiles: Arc::clone(&profiles),
        });
        Ok(profiles)
    }
}

/// Profiles built straight from the store, bypassing and leaving the cache:
/// what a reader asking for another window gets.
pub(crate) fn build_uncached(
    store: &dyn ResourceUsageStoreBackend,
    no_diff_tasks: NoDiffTasks<'_>,
    crew_models: BTreeMap<String, String>,
    bounds: ProfileBounds,
    now: DateTime<Utc>,
) -> Result<ProfileSet, OrbitError> {
    // Every retained delivery, whatever its age: retention keeps the newest
    // of each key, so a break leaves the windows as they were.
    let runs = store.delivery_run_resources(DateTime::UNIX_EPOCH)?;
    let templates = template_task_ids(&runs);
    let tagged = if templates.is_empty() {
        BTreeSet::new()
    } else {
        no_diff_tasks(&templates)?
    };
    Ok(build_profiles(runs, &tagged, crew_models, bounds, now))
}
