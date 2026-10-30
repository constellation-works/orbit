//! Profiles kept across admission passes until a delivery finishes or a
//! template's `no-diff-expected` tag changes. A pass probes delivery
//! freshness only when the store's change counter moved.

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
    /// The store's change counter, read before `freshness` was probed: while
    /// it holds, so does the probe's answer.
    stamp: Option<i64>,
    bounds: ProfileBounds,
    crew_models: BTreeMap<String, String>,
    /// The template task ids the build classified, and the subset tagged
    /// `no-diff-expected` then. A tag edit finishes no run, so a hit re-reads
    /// these tags rather than trusting the freshness probe.
    templates: BTreeSet<String>,
    tagged: BTreeSet<String>,
    profiles: Arc<ProfileSet>,
}

/// The host's profiles, rebuilt only when the store's freshness probe (the
/// newest finish and count of finished sampled runs), the bounds, the
/// configured crews' models or a template's `no-diff-expected` tag change.
/// The probe scans every minute row, so a pass runs it only when the store's
/// change counter moved since the cached answer.
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
        // Read before any probe: a change after this read moves the counter
        // past the one kept, so a later pass probes again.
        let stamp = store.delivery_runs_stamp()?;
        // Held while building, so concurrent passes build once.
        let mut slot = self
            .slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut probed = None;
        if let Some(cached) = slot.as_mut()
            && cached.bounds == bounds
            && cached.crew_models == crew_models
            && tags_unchanged(&cached.templates, &cached.tagged, no_diff_tasks)?
        {
            if stamp.is_some() && stamp == cached.stamp {
                return Ok(Arc::clone(&cached.profiles));
            }
            let freshness = store.delivery_runs_freshness()?;
            if freshness == cached.freshness {
                cached.stamp = stamp;
                return Ok(Arc::clone(&cached.profiles));
            }
            probed = Some(freshness);
        }
        let freshness = match probed {
            Some(freshness) => freshness,
            None => store.delivery_runs_freshness()?,
        };
        let built = build_classified(store, no_diff_tasks, crew_models.clone(), bounds, now)?;
        let profiles = Arc::new(built.profiles);
        *slot = Some(Cached {
            freshness,
            stamp,
            bounds,
            crew_models,
            templates: built.templates,
            tagged: built.tagged,
            profiles: Arc::clone(&profiles),
        });
        Ok(profiles)
    }
}

/// Whether the templates still carry the `no-diff-expected` tags the cache
/// classified them with. One tag lookup, skipped when there is no template.
fn tags_unchanged(
    templates: &BTreeSet<String>,
    tagged: &BTreeSet<String>,
    no_diff_tasks: NoDiffTasks<'_>,
) -> Result<bool, OrbitError> {
    if templates.is_empty() {
        return Ok(true);
    }
    Ok(no_diff_tasks(templates)? == *tagged)
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
    Ok(build_classified(store, no_diff_tasks, crew_models, bounds, now)?.profiles)
}

/// A build, with the template classes it read.
struct Classified {
    profiles: ProfileSet,
    templates: BTreeSet<String>,
    tagged: BTreeSet<String>,
}

fn build_classified(
    store: &dyn ResourceUsageStoreBackend,
    no_diff_tasks: NoDiffTasks<'_>,
    crew_models: BTreeMap<String, String>,
    bounds: ProfileBounds,
    now: DateTime<Utc>,
) -> Result<Classified, OrbitError> {
    // Every retained delivery, whatever its age: retention keeps the newest
    // of each key, so a break leaves the windows as they were.
    let runs = store.delivery_run_resources(DateTime::UNIX_EPOCH)?;
    let templates = template_task_ids(&runs);
    let tagged = if templates.is_empty() {
        BTreeSet::new()
    } else {
        no_diff_tasks(&templates)?
    };
    Ok(Classified {
        profiles: build_profiles(runs, &tagged, crew_models, bounds, now),
        templates,
        tagged,
    })
}
