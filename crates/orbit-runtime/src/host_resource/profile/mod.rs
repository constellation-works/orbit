//! Resource profiles: what one kind of delivery costs, minute by minute since
//! the drain admitted it, learned from the `run_resource_minutes` that
//! delivery workers record.
//!
//! - **Key.** `(host, workspace, work class, complexity)`; an
//!   `auto-task:<template>` key drops complexity, because a recurring template
//!   does the same job every time. The host is the store: each host keeps one
//!   database, shared by its workspaces.
//! - **Delivery.** The runs of one task that share an admission time: a local
//!   `task_auto_pipeline` → `task_gate_pipeline` → `task_pr_pipeline` chain,
//!   or one claimed leaf. Its series is the per-minute sum over those runs,
//!   counted from admission. A delivery with a resumed or retried run, or a
//!   run still live, is left out.
//! - **Crew model.** Each delivery carries the crew model its runs named.
//!   A key has one profile per crew model, over that model's deliveries,
//!   and a union over every crew model's. A lookup that knows the crew model
//!   reads its crew profile first and falls back to the union while that is
//!   sparse, so a mixed crew pool keeps each model's history and a newly
//!   finished delivery of one model never hides the others'.
//! - **Window.** The last `profile_window` completed deliveries of the key,
//!   or of the key and crew model, whatever their age, so a month without
//!   deliveries leaves every profile as it was. The Orbit version a delivery
//!   ran under is recorded but never narrows a window: a release does not
//!   change what a delivery costs. Retention keeps the newest
//!   [`RESOURCE_HISTORY_DELIVERIES_PER_KEY`] deliveries of each key, and of
//!   each key and crew model, past its age cutoff.
//! - **Per minute.** `alive` is the fraction still running, `cpu` the mean
//!   cores and `mem` the 90th-percentile memory over the deliveries alive
//!   then, each with its spread; memory also keeps its mean, from which the
//!   forecast sums several deliveries' memory. The horizon is the first
//!   minute `alive` falls below 0.1, capped at `horizon_cap_minutes`.
//! - **Delivery peaks.** The nearest-rank p50 and p90 of each delivery's own
//!   highest minute, for cores and for memory. Averaging minute by minute
//!   blurs peaks that fall at different times, so these quantiles can be
//!   several times the series peak. A sparse profile still reports them.
//!
//! Profiles change when a delivery finishes or a template's `no-diff-expected`
//! tag is edited, so `ResourceProfileCache` keeps them across admission passes
//! and rebuilds when the freshness probe, the bounds, the crew-model map or
//! those tags change. A tag edit finishes no run, so each pass re-reads the
//! tags of the cached templates. The probe scans every retained minute row,
//! so a pass skips it while the store's change counter, which triggers bump
//! on every write that can move the probe, holds since the cached answer.
//! Live load is never cached here.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::AUTO_TASK_TAG_PREFIX;
use serde::Serialize;
use serde_json::value::RawValue;

mod build;
mod cache;
mod lookup;
#[cfg(test)]
mod tests;

pub(crate) use build::{ProfileBounds, build_profiles, template_task_ids};
pub(crate) use cache::{ResourceProfileCache, build_uncached};
pub use lookup::{ProfileLookup, ProfileMatch, ProfileSource};

/// Deliveries of each workspace, work class and complexity whose run minutes
/// retention keeps whatever their age, or `profile_window` when larger.
pub const RESOURCE_HISTORY_DELIVERIES_PER_KEY: usize = 200;

/// Cores above which a no-diff delivery's minute looks like build work.
pub const BUILD_LIKE_CORES: f64 = 1.0;
/// Consecutive minutes above [`BUILD_LIKE_CORES`] that flag a no-diff delivery.
pub const BUILD_LIKE_MINUTES: usize = 2;

/// What a delivery falls back to when its own key is too sparse: whether it
/// is expected to make no diff. The fallback chain never borrows a `change`
/// profile for `no-diff` work, or the reverse, before its last step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BaseClass {
    NoDiff,
    Change,
}

impl BaseClass {
    /// The work class this base names.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoDiff => "no-diff",
            Self::Change => "change",
        }
    }
}

/// One profile's key. The host is implicit: profiles come from one host's
/// store.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ProfileKey {
    pub workspace_id: String,
    /// `auto-task:<template>`, `no-diff` or `change`.
    pub work_class: String,
    /// Always `None` for a template, and for a class-wide profile; `None`
    /// otherwise means the deliveries had no complexity.
    pub complexity: Option<String>,
}

impl ProfileKey {
    /// The template of an `auto-task:<template>` key.
    pub fn template(&self) -> Option<&str> {
        self.work_class.strip_prefix(AUTO_TASK_TAG_PREFIX)
    }
}

/// The profile at one minute since admission.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProfileMinute {
    pub minute: u32,
    /// Fraction of the window's deliveries still running.
    pub alive: f64,
    /// Mean cores over the deliveries alive at this minute.
    pub cpu_cores: f64,
    /// Standard deviation of those cores.
    pub cpu_stddev: f64,
    /// 90th-percentile memory over the deliveries alive at this minute.
    pub memory_bytes: u64,
    /// Mean memory over those deliveries.
    pub memory_mean_bytes: f64,
    /// Standard deviation of that memory.
    pub memory_stddev_bytes: f64,
}

/// One crew model's share of a profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfileCrew {
    /// `None` for deliveries whose runs named no crew model, or whose minutes
    /// predate crew recording.
    pub crew_model: Option<String>,
    /// The deliveries of this crew model in the profile's window.
    pub deliveries: usize,
    /// The deliveries in this crew model's own profile of the key, which
    /// prices its work while it has at least `min_profile_deliveries`;
    /// `None` without one: no crew model, or a class-wide profile.
    pub profile_deliveries: Option<usize>,
}

/// One delivery in a profile's window.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProfileDelivery {
    pub task_id: String,
    pub admitted_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    /// Minutes from admission to the last run's end.
    pub minutes: u32,
    pub run_ids: Vec<String>,
    /// The crew model its runs named; `None` when none did.
    pub crew_model: Option<String>,
    /// The Orbit version that sampled it, for reference only; `None` before
    /// it was recorded.
    pub orbit_version: Option<String>,
}

/// A delivery behind a no-diff profile's build flag.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BuildLikeDelivery {
    pub task_id: String,
    pub admitted_at: DateTime<Utc>,
    pub run_ids: Vec<String>,
    /// The first minute of the sustained run above [`BUILD_LIKE_CORES`].
    pub from_minute: u32,
    pub peak_cpu_cores: f64,
}

/// The highest value of a profile series before its horizon.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ProfilePeak<T> {
    pub minute: u32,
    pub value: T,
}

/// Nearest-rank p50 and p90 of one measurement over a profile's deliveries.
///
/// Rank is `ceil(n · p)`, the same rule as a minute's memory percentile: the
/// value at index `ceil(n · p) - 1` of the sorted peaks. One delivery reports
/// that delivery's peak for both; two report the lower as p50 and the higher
/// as p90.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ProfileQuantile<T> {
    pub p50: T,
    pub p90: T,
}

/// One key's resource profile.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResourceProfile {
    pub key: ProfileKey,
    pub base_class: BaseClass,
    /// The samples the profile used: deliveries in its window.
    pub delivery_count: usize,
    /// The crew model whose deliveries this profile holds; `None` for a
    /// union over every crew model: a key's own profile or a class-wide one.
    pub crew_model: Option<String>,
    /// The crew models behind the profile, by model: how many of the
    /// window's deliveries each ran, and how many its own profile of the key
    /// holds. A union's crew count is this list's length.
    pub crews: Vec<ProfileCrew>,
    /// Fewer deliveries than `min_profile_deliveries`: a lookup falls back
    /// past this key.
    pub sparse: bool,
    pub horizon_minutes: u32,
    pub peak_cpu_cores: ProfilePeak<f64>,
    pub peak_memory_bytes: ProfilePeak<u64>,
    /// Nearest-rank p50 and p90 of each delivery's highest single-minute
    /// core count, over its whole series. These stay high when deliveries
    /// peak in different minutes; [`Self::peak_cpu_cores`] is the peak of
    /// the per-minute mean and can be much lower.
    pub delivery_peak_cpu: ProfileQuantile<f64>,
    /// Nearest-rank p50 and p90 of each delivery's highest single-minute
    /// memory, in bytes.
    pub delivery_peak_memory_bytes: ProfileQuantile<u64>,
    /// Expected core-minutes before the horizon, `Σ cpu(m)·alive(m)`. The
    /// fallback chain's "heaviest" profile has the most.
    pub expected_core_minutes: f64,
    /// How far the window's deliveries disagree before the horizon: the summed
    /// per-minute standard deviation over the summed mean, for CPU and memory.
    /// Zero when every delivery used the same; near or above 1 when a minute's
    /// mean says little about any one delivery.
    pub cpu_spread: f64,
    pub memory_spread: f64,
    /// No-diff deliveries that did build-like work; empty when the profile is
    /// not no-diff or its template builds by design.
    pub build_like: Vec<BuildLikeDelivery>,
    /// The window, newest first.
    pub deliveries: Vec<ProfileDelivery>,
    /// Every minute until the window's longest delivery ended.
    pub series: Vec<ProfileMinute>,
}

/// Every profile on the host, built at one moment.
#[derive(Debug, Clone)]
pub struct ProfileSet {
    pub built_at: DateTime<Utc>,
    /// One profile per key, in key order: the union over every crew model.
    pub profiles: Vec<Arc<ResourceProfile>>,
    /// One profile per key and recorded crew model, in key then model order:
    /// what a lookup that knows its crew model reads before the union.
    pub crew_profiles: Vec<Arc<ResourceProfile>>,
    /// One profile per workspace and `no-diff` or `change` class, over every
    /// complexity, including template deliveries of that base class: the
    /// first fallback step, or the base-class step for templates.
    pub class_profiles: Vec<Arc<ResourceProfile>>,
    /// The configured crews' models by crew name, so admission can look a
    /// task's assigned crew up by the model its profiles are keyed on.
    pub crew_models: BTreeMap<String, String>,
    workspace_json: WorkspaceProfilesJson,
}

impl ProfileSet {
    /// One workspace's union profiles, in key order, as a JSON array of the
    /// objects `orbit run profiles --json` emits. A set never changes once
    /// built, so each workspace's array is encoded on its first read and
    /// served again while the cache keeps the set.
    pub fn workspace_profiles_json(&self, workspace_id: &str) -> Result<Arc<RawValue>, OrbitError> {
        let mut encoded = self
            .workspace_json
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(json) = encoded.get(workspace_id) {
            return Ok(Arc::clone(json));
        }
        let profiles: Vec<&ResourceProfile> = self
            .profiles
            .iter()
            .filter(|profile| profile.key.workspace_id == workspace_id)
            .map(Arc::as_ref)
            .collect();
        let json: Arc<RawValue> = serde_json::value::to_raw_value(&profiles)
            .map_err(|error| OrbitError::Execution(format!("encode resource profiles: {error}")))?
            .into();
        encoded.insert(workspace_id.to_string(), Arc::clone(&json));
        Ok(json)
    }
}

/// [`ProfileSet::workspace_profiles_json`]'s arrays by workspace.
#[derive(Debug, Default)]
struct WorkspaceProfilesJson(Mutex<BTreeMap<String, Arc<RawValue>>>);

impl Clone for WorkspaceProfilesJson {
    /// A clone starts empty: its public fields may be edited after cloning.
    fn clone(&self) -> Self {
        Self::default()
    }
}
