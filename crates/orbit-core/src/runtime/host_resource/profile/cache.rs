//! Profiles kept across admission passes until a delivery finishes.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{DeliveryRunsFreshness, ResourceUsageStoreBackend};

use super::{PROFILE_HISTORY_DAYS, ProfileBounds, ProfileSet, build_profiles, template_task_ids};

/// Which of the given template task ids carry `no-diff-expected`.
pub(crate) type NoDiffTasks<'a> =
    &'a dyn Fn(&BTreeSet<String>) -> Result<BTreeSet<String>, OrbitError>;

struct Cached {
    freshness: DeliveryRunsFreshness,
    bounds: ProfileBounds,
    profiles: Arc<ProfileSet>,
}

/// The host's profiles, rebuilt only when the store's freshness probe (the
/// newest finish and count of finished sampled runs) or the bounds change.
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
        {
            return Ok(Arc::clone(&cached.profiles));
        }
        let runs = store.delivery_run_resources(now - Duration::days(PROFILE_HISTORY_DAYS))?;
        let templates = template_task_ids(&runs);
        let tagged = if templates.is_empty() {
            BTreeSet::new()
        } else {
            no_diff_tasks(&templates)?
        };
        let profiles = Arc::new(build_profiles(runs, &tagged, bounds, now));
        *slot = Some(Cached {
            freshness,
            bounds,
            profiles: Arc::clone(&profiles),
        });
        Ok(profiles)
    }
}
