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
//! - **Window.** The last `profile_window` completed deliveries of the key,
//!   admitted within [`PROFILE_HISTORY_DAYS`].
//! - **Per minute.** `alive` is the fraction still running, `cpu` the mean
//!   cores and `mem` the 90th-percentile memory over the deliveries alive
//!   then, each with its spread. The horizon is the first minute `alive`
//!   falls below 0.1, capped at `horizon_cap_minutes`.
//!
//! Profiles change only when a delivery finishes, so `ResourceProfileCache`
//! keeps them across admission passes and rebuilds when a freshness probe
//! changes. Live load is never cached here.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use orbit_types::workflow::AUTO_TASK_TAG_PREFIX;
use serde::Serialize;

mod build;
mod cache;
mod lookup;

pub(crate) use build::{ProfileBounds, build_profiles, template_task_ids};
pub(crate) use cache::ResourceProfileCache;
pub use lookup::{ProfileLookup, ProfileMatch, ProfileSource};

/// Profiles read deliveries admitted within this many days, the retention of
/// `run_resource_minutes`.
pub const PROFILE_HISTORY_DAYS: i64 = 30;

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
    /// Standard deviation of that memory.
    pub memory_stddev_bytes: f64,
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

/// One key's resource profile.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResourceProfile {
    pub key: ProfileKey,
    pub base_class: BaseClass,
    pub delivery_count: usize,
    /// Fewer deliveries than `min_profile_deliveries`: a lookup falls back
    /// past this key.
    pub sparse: bool,
    pub horizon_minutes: u32,
    pub peak_cpu_cores: ProfilePeak<f64>,
    pub peak_memory_bytes: ProfilePeak<u64>,
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
    /// One profile per key, in key order.
    pub profiles: Vec<Arc<ResourceProfile>>,
    /// One profile per workspace and `no-diff` or `change` class, over every
    /// complexity: the first fallback step.
    pub class_profiles: Vec<Arc<ResourceProfile>>,
}
