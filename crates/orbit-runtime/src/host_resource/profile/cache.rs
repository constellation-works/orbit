//! Profiles kept across admission passes until a delivery finishes or a
//! template's `no-diff-expected` tag changes. A pass probes delivery
//! freshness only when the store's change counter moved.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{DeliveryRunsFreshness, DeliveryRunsLoad, ResourceUsageStoreBackend};

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
    /// The runs the build read, which the next build re-reads only where a
    /// run's minute rows or `job_runs` row moved. `None` after a failed
    /// build took it, so the next build reads every run.
    load: Option<DeliveryRunsLoad>,
}

/// The host's profiles, rebuilt only when the store's freshness probe (the
/// newest finish and count of finished sampled runs), the bounds, the
/// configured crews' models or a template's `no-diff-expected` tag change.
/// The probe scans every minute row, so a pass runs it only when the store's
/// change counter moved since the cached answer. A rebuild hands the store
/// the runs the last one read, so only runs whose minute rows or `job_runs`
/// row moved are read again.
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
        let previous = slot.as_mut().and_then(|cached| cached.load.take());
        let built = build_classified(
            store,
            no_diff_tasks,
            crew_models.clone(),
            bounds,
            now,
            previous,
        )?;
        tracing::debug!(
            load_ms = built.timing.load_ms,
            tags_ms = built.timing.tags_ms,
            build_ms = built.timing.build_ms,
            runs = built.timing.runs,
            "resource profiles rebuilt"
        );
        let profiles = Arc::new(built.profiles);
        *slot = Some(Cached {
            freshness,
            stamp,
            bounds,
            crew_models,
            templates: built.templates,
            tagged: built.tagged,
            profiles: Arc::clone(&profiles),
            load: Some(built.load),
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
    let runs = store.delivery_run_resources(DateTime::UNIX_EPOCH)?;
    let tagged = tagged_templates(&template_task_ids(&runs), no_diff_tasks)?;
    Ok(build_profiles(&runs, &tagged, crew_models, bounds, now))
}

/// A build, with the template classes it read.
struct Classified {
    profiles: ProfileSet,
    templates: BTreeSet<String>,
    tagged: BTreeSet<String>,
    load: DeliveryRunsLoad,
    timing: BuildTiming,
}

/// Where a build spent its time, for the rebuild's debug event.
struct BuildTiming {
    load_ms: f64,
    tags_ms: f64,
    build_ms: f64,
    runs: usize,
}

fn build_classified(
    store: &dyn ResourceUsageStoreBackend,
    no_diff_tasks: NoDiffTasks<'_>,
    crew_models: BTreeMap<String, String>,
    bounds: ProfileBounds,
    now: DateTime<Utc>,
    previous: Option<DeliveryRunsLoad>,
) -> Result<Classified, OrbitError> {
    // Every retained delivery, whatever its age: retention keeps the newest
    // of each key, so a break leaves the windows as they were.
    let started = Instant::now();
    let load = store.reload_delivery_runs(previous)?;
    let loaded = Instant::now();
    let templates = template_task_ids(&load.runs);
    let tagged = tagged_templates(&templates, no_diff_tasks)?;
    let classified = Instant::now();
    let profiles = build_profiles(&load.runs, &tagged, crew_models, bounds, now);
    let millis = |from: Instant, to: Instant| (to - from).as_secs_f64() * 1000.0;
    Ok(Classified {
        timing: BuildTiming {
            load_ms: millis(started, loaded),
            tags_ms: millis(loaded, classified),
            build_ms: millis(classified, Instant::now()),
            runs: load.runs.len(),
        },
        profiles,
        templates,
        tagged,
        load,
    })
}

/// The `templates` tagged `no-diff-expected`; no lookup without a template.
fn tagged_templates(
    templates: &BTreeSet<String>,
    no_diff_tasks: NoDiffTasks<'_>,
) -> Result<BTreeSet<String>, OrbitError> {
    if templates.is_empty() {
        Ok(BTreeSet::new())
    } else {
        no_diff_tasks(templates)
    }
}
