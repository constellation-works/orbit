//! The host-shared GitHub quota record and its admission decision.
//!
//! One JSON file per GitHub account under the store directory holds, per
//! resource (`core`, `graphql`), what GitHub's rate-limit endpoint last
//! reported, a local hold after an authenticated primary rate-limit answer,
//! and the calls each caller class made in the current window. Every change
//! is a read-modify-write under an exclusive file lock beside the record and
//! lands through a staging file and an atomic rename, so concurrent writers in
//! any number of processes never leave a torn record.
//!
//! The store never turns its own failure into a refusal: an unreadable or
//! unwritable record admits the read.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use orbit_common::ProviderReadThrottle;
use serde::{Deserialize, Serialize};

/// GitHub's REST bucket.
pub const RESOURCE_CORE: &str = "core";
/// GitHub's GraphQL bucket, counted apart from `core`.
pub const RESOURCE_GRAPHQL: &str = "graphql";
const TRACKED_RESOURCES: [&str; 2] = [RESOURCE_CORE, RESOURCE_GRAPHQL];

/// Default share of a resource's limit kept back from agent reads.
pub const DEFAULT_AGENT_RESERVE_PERCENT: u8 = 20;
/// Default share of a resource's limit kept back from background reads.
pub const DEFAULT_BACKGROUND_RESERVE_PERCENT: u8 = 10;
/// Default shortest interval between two rate-limit endpoint reads on a host.
pub const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// The counting window used while GitHub's reset time is unknown.
const FALLBACK_WINDOW_SECONDS: u64 = 3600;
/// The longest refresh diagnostic the record keeps.
const MAX_ERROR_CHARS: usize = 240;

/// Who is spending the host's GitHub quota.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GithubCaller {
    /// Landing, the completion wait and the merge. Runs the quota to zero.
    Delivery,
    /// The CI and security sweeps, attention, drain passes and task PR forge reads.
    Background,
    /// Every `github.*` tool call an agent makes.
    Agent,
}

impl GithubCaller {
    /// The caller class as the status surface and refusals name it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Delivery => "delivery",
            Self::Background => "background",
            Self::Agent => "agent",
        }
    }
}

/// The reserves and refresh interval the guard applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GithubQuotaPolicy {
    /// Share of the limit agent reads may not spend; 0 disables their refusal.
    pub agent_reserve_percent: u8,
    /// Share of the limit background reads may not spend; 0 disables their refusal.
    pub background_reserve_percent: u8,
    /// Shortest interval between two rate-limit endpoint reads on the host.
    pub refresh_interval: Duration,
}

impl Default for GithubQuotaPolicy {
    fn default() -> Self {
        Self {
            agent_reserve_percent: DEFAULT_AGENT_RESERVE_PERCENT,
            background_reserve_percent: DEFAULT_BACKGROUND_RESERVE_PERCENT,
            refresh_interval: DEFAULT_REFRESH_INTERVAL,
        }
    }
}

impl GithubQuotaPolicy {
    /// The number of calls `caller` must leave in a bucket of `limit`.
    /// Delivery keeps none; a reserve of 0 keeps none.
    pub fn reserve_calls(&self, caller: GithubCaller, limit: u64) -> u64 {
        let percent = match caller {
            GithubCaller::Delivery => 0,
            GithubCaller::Background => self.background_reserve_percent,
            GithubCaller::Agent => self.agent_reserve_percent,
        };
        (limit.saturating_mul(u64::from(percent.min(100)))).div_ceil(100)
    }

    fn refresh_seconds(&self) -> u64 {
        self.refresh_interval.as_secs().max(1)
    }
}

/// One resource as GitHub's rate-limit endpoint reported it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservedQuota {
    pub limit: u64,
    pub remaining: u64,
    pub reset_epoch_seconds: u64,
}

/// What a read of GitHub's rate-limit endpoint answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RateLimitAnswer {
    /// An authenticated answer, by resource name.
    Known(BTreeMap<String, ObservedQuota>),
    /// The endpoint could not be read, answered for an unauthenticated
    /// caller, or answered something unreadable. Reads stay allowed and the
    /// quota is reported unknown, never as zero remaining.
    Unknown(String),
}

/// Calls per caller class.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallCounts {
    #[serde(default)]
    pub delivery: u64,
    #[serde(default)]
    pub background: u64,
    #[serde(default)]
    pub agent: u64,
}

impl CallCounts {
    fn bump(&mut self, caller: GithubCaller) {
        let slot = match caller {
            GithubCaller::Delivery => &mut self.delivery,
            GithubCaller::Background => &mut self.background,
            GithubCaller::Agent => &mut self.agent,
        };
        *slot = slot.saturating_add(1);
    }
}

/// Why a read was refused locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaRefusalReason {
    /// An authenticated primary rate-limit answer, or an empty bucket, holds
    /// every read of the resource until its reset.
    Exhausted,
    /// The remaining count is at or below the caller's reserve.
    Reserve,
}

/// A read the guard refused without calling GitHub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaRefusal {
    pub caller: GithubCaller,
    pub resource: String,
    pub reason: QuotaRefusalReason,
    /// When reads of the resource may resume, in Unix seconds.
    pub resume_epoch_seconds: u64,
    pub remaining: Option<u64>,
    pub limit: Option<u64>,
    /// The caller's reserve in calls (0 for an exhausted hold).
    pub reserve: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct QuotaRecord {
    #[serde(default)]
    refresh_claimed_at: Option<u64>,
    #[serde(default)]
    refresh_generation: u64,
    #[serde(default)]
    refresh_in_flight: bool,
    #[serde(default)]
    refreshed_at: Option<u64>,
    #[serde(default)]
    refresh_error: Option<String>,
    #[serde(default)]
    resources: BTreeMap<String, ResourceRecord>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ResourceRecord {
    #[serde(default)]
    limit: Option<u64>,
    #[serde(default)]
    remaining: Option<u64>,
    #[serde(default)]
    reset_epoch_seconds: Option<u64>,
    #[serde(default)]
    observed_at: Option<u64>,
    /// No read of the resource leaves the host before this instant.
    #[serde(default)]
    held_until: Option<u64>,
    #[serde(default)]
    window_ends_at: Option<u64>,
    #[serde(default)]
    calls: CallCounts,
    #[serde(default)]
    refused: CallCounts,
    /// Reads admitted while an external rate-limit probe was in flight and not
    /// yet committed by [`apply_answer`].
    #[serde(default)]
    in_flight_debits: u64,
}

impl ResourceRecord {
    /// The observation, while it still describes the current window.
    fn current(&self, now: u64) -> Option<(u64, u64, u64)> {
        match (self.limit, self.remaining, self.reset_epoch_seconds) {
            (Some(limit), Some(remaining), Some(reset)) if reset > now => {
                Some((limit, remaining, reset))
            }
            _ => None,
        }
    }

    fn active_hold(&self, now: u64) -> Option<u64> {
        self.held_until.filter(|until| *until > now)
    }

    /// Start a new counting window once the current one ended.
    fn roll_window(&mut self, now: u64) {
        if self.window_ends_at.is_none_or(|end| now >= end) {
            self.calls = CallCounts::default();
            self.refused = CallCounts::default();
            self.window_ends_at = Some(
                self.reset_epoch_seconds
                    .filter(|reset| *reset > now)
                    .unwrap_or(now + FALLBACK_WINDOW_SECONDS),
            );
        }
    }

    fn refusal(
        &self,
        policy: &GithubQuotaPolicy,
        caller: GithubCaller,
        resource: &str,
        now: u64,
    ) -> Option<QuotaRefusal> {
        if let Some(until) = self.active_hold(now) {
            return Some(QuotaRefusal {
                caller,
                resource: resource.to_string(),
                reason: QuotaRefusalReason::Exhausted,
                resume_epoch_seconds: until,
                remaining: self.remaining,
                limit: self.limit,
                reserve: 0,
            });
        }
        let (limit, remaining, reset) = self.current(now)?;
        let reserve = policy.reserve_calls(caller, limit);
        (reserve > 0 && remaining <= reserve).then(|| QuotaRefusal {
            caller,
            resource: resource.to_string(),
            reason: QuotaRefusalReason::Reserve,
            resume_epoch_seconds: reset,
            remaining: Some(remaining),
            limit: Some(limit),
            reserve,
        })
    }
}

/// The operator's view of one account's quota record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GithubQuotaStatus {
    /// An opaque key for the GitHub host and credential; never the credential.
    pub account: String,
    pub refreshed_at: Option<String>,
    /// Why the last rate-limit read left the quota unknown.
    pub refresh_error: Option<String>,
    pub agent_reserve_percent: u8,
    pub background_reserve_percent: u8,
    pub resources: Vec<GithubResourceStatus>,
}

/// One resource of [`GithubQuotaStatus`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GithubResourceStatus {
    pub resource: String,
    /// `known`, or `unknown` when no current authenticated answer exists.
    pub state: &'static str,
    pub limit: Option<u64>,
    pub remaining: Option<u64>,
    pub reset_at: Option<String>,
    pub observed_at: Option<String>,
    /// Every read is held until this instant after a rate-limit answer.
    pub held_until: Option<String>,
    pub agent_reads_refused: bool,
    pub background_reads_refused: bool,
    pub delivery_reads_refused: bool,
    /// Calls each caller class made in the current window.
    pub calls: CallCounts,
    /// Reads refused locally in the current window.
    pub refused: CallCounts,
}

/// The directory of host-shared quota records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubQuotaStore {
    directory: PathBuf,
}

/// Directory under the Orbit data root.
const STORE_DIRECTORY: [&str; 2] = ["state", "github-quota"];

impl GithubQuotaStore {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    /// The store under the Orbit data root: `ORBIT_ROOT` when set, else the
    /// machine-global `~/.orbit`.
    pub fn in_data_root() -> Option<Self> {
        let root = match std::env::var("ORBIT_ROOT") {
            Ok(root) if !root.trim().is_empty() => PathBuf::from(root),
            _ => orbit_common::fs::path::global_orbit_dir().ok()?,
        };
        Some(Self::new(
            STORE_DIRECTORY
                .iter()
                .fold(root, |path, part| path.join(part)),
        ))
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    fn record_path(&self, account: &str) -> PathBuf {
        self.directory.join(format!("{account}.json"))
    }

    fn load(path: &Path) -> QuotaRecord {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Read-modify-write `account`'s record under its exclusive lock.
    fn update<T>(
        &self,
        account: &str,
        change: impl FnOnce(&mut QuotaRecord) -> T,
    ) -> io::Result<T> {
        let path = self.record_path(account);
        orbit_common::fs::io::create_private_dir_all(&self.directory)?;
        orbit_common::fs::io::with_exclusive_file_lock(&path, "github quota record", || {
            let mut record = Self::load(&path);
            let output = change(&mut record);
            let text = serde_json::to_string(&record).map_err(io::Error::other)?;
            orbit_common::fs::io::atomic_write_text_volatile(&path, &text)?;
            Ok(output)
        })
    }

    /// Admit or refuse one read of `resource` by `caller`, first refreshing
    /// the record through `refresh` when no process on the host has read the
    /// rate-limit endpoint within the policy's interval. An admitted read is
    /// counted against its class and the recorded remaining count.
    pub fn admit(
        &self,
        policy: &GithubQuotaPolicy,
        account: &str,
        caller: GithubCaller,
        resource: &str,
        now: u64,
        refresh: &dyn Fn() -> RateLimitAnswer,
    ) -> Result<(), QuotaRefusal> {
        self.refresh_if_due(policy, account, now, refresh);
        let decided = self.update(account, |record| {
            let refresh_in_flight = record.refresh_in_flight;
            let entry = record.resources.entry(resource.to_string()).or_default();
            entry.roll_window(now);
            if let Some(refusal) = entry.refusal(policy, caller, resource, now) {
                entry.refused.bump(caller);
                return Err(refusal);
            }
            entry.calls.bump(caller);
            if refresh_in_flight {
                entry.in_flight_debits = entry.in_flight_debits.saturating_add(1);
            }
            if entry.current(now).is_some() {
                entry.remaining = entry.remaining.map(|left| left.saturating_sub(1));
            }
            Ok(())
        });
        match decided {
            Ok(decision) => decision,
            Err(error) => {
                // A process that may read but not write the record (a
                // sandboxed lane) still honours it, uncounted. One that cannot
                // read it admits the read.
                tracing::warn!(target: "orbit_tools::github_quota", %error,
                    "GitHub quota record not writable; deciding from a read-only copy");
                let record = Self::load(&self.record_path(account));
                match record.resources.get(resource) {
                    Some(entry) => match entry.refusal(policy, caller, resource, now) {
                        Some(refusal) => Err(refusal),
                        None => Ok(()),
                    },
                    None => Ok(()),
                }
            }
        }
    }

    /// Read the rate-limit endpoint through `refresh` when no process on the
    /// host has within the interval. The claim is taken under the record's
    /// lock before the read, so concurrent callers refresh at most once, and a
    /// caller whose sampled `now` is older than the active claim never moves
    /// it backward.
    pub fn refresh_if_due(
        &self,
        policy: &GithubQuotaPolicy,
        account: &str,
        now: u64,
        refresh: &dyn Fn() -> RateLimitAnswer,
    ) {
        let interval = policy.refresh_seconds();
        let claim = self
            .update(account, |record| {
                // `now` was sampled before the lock, so a caller that waited
                // behind a later claimant can hold an older time: that is
                // inside the claim's interval, not due. Only a claim further
                // ahead than one interval (a clock stepped back) is stale.
                let due = record.refresh_claimed_at.is_none_or(|at| {
                    if now < at {
                        at - now > interval
                    } else {
                        now - at >= interval
                    }
                });
                if due {
                    record.refresh_claimed_at = Some(now);
                    record.refresh_generation = record.refresh_generation.wrapping_add(1);
                    record.refresh_in_flight = true;
                    for entry in record.resources.values_mut() {
                        entry.in_flight_debits = 0;
                    }
                    Some(record.refresh_generation)
                } else {
                    None
                }
            })
            .ok()
            .flatten();
        let Some(claim_generation) = claim else {
            return;
        };
        let answer = refresh();
        let _ = self.update(account, |record| {
            apply_answer(record, answer, now, claim_generation)
        });
    }

    /// Record an authenticated primary rate-limit answer for `resource`:
    /// every read of it on the host is held until the reset GitHub named,
    /// else its `Retry-After`, else the recorded reset, else one refresh
    /// interval.
    pub fn record_throttle(
        &self,
        policy: &GithubQuotaPolicy,
        account: &str,
        resource: &str,
        throttle: &ProviderReadThrottle,
        now: u64,
    ) {
        let _ = self.update(account, |record| {
            let entry = record.resources.entry(resource.to_string()).or_default();
            let until = throttle
                .reset_epoch_seconds
                .filter(|reset| *reset > now)
                .or_else(|| throttle.retry_after_seconds.map(|wait| now + wait))
                .or_else(|| entry.reset_epoch_seconds.filter(|reset| *reset > now))
                .unwrap_or(now + policy.refresh_seconds());
            entry.held_until = Some(entry.held_until.map_or(until, |held| held.max(until)));
            if entry.limit.is_some() {
                entry.remaining = Some(0);
            }
        });
    }

    /// The operator's view of `account`'s record at `now`.
    pub fn status(&self, policy: &GithubQuotaPolicy, account: &str, now: u64) -> GithubQuotaStatus {
        let record = Self::load(&self.record_path(account));
        let resources = TRACKED_RESOURCES
            .iter()
            .map(|name| {
                let mut entry = record.resources.get(*name).cloned().unwrap_or_default();
                if entry.window_ends_at.is_some_and(|end| now >= end) {
                    entry.calls = CallCounts::default();
                    entry.refused = CallCounts::default();
                }
                let current = entry.current(now);
                let refused = |caller| entry.refusal(policy, caller, name, now).is_some();
                GithubResourceStatus {
                    resource: (*name).to_string(),
                    state: if current.is_some() {
                        "known"
                    } else {
                        "unknown"
                    },
                    limit: current.map(|(limit, _, _)| limit),
                    remaining: current.map(|(_, remaining, _)| remaining),
                    reset_at: current.and_then(|(_, _, reset)| rfc3339(reset)),
                    observed_at: entry.observed_at.and_then(rfc3339),
                    held_until: entry.active_hold(now).and_then(rfc3339),
                    agent_reads_refused: refused(GithubCaller::Agent),
                    background_reads_refused: refused(GithubCaller::Background),
                    delivery_reads_refused: refused(GithubCaller::Delivery),
                    calls: entry.calls,
                    refused: entry.refused,
                }
            })
            .collect();
        GithubQuotaStatus {
            account: account.to_string(),
            refreshed_at: record.refreshed_at.and_then(rfc3339),
            refresh_error: record.refresh_error,
            agent_reserve_percent: policy.agent_reserve_percent,
            background_reserve_percent: policy.background_reserve_percent,
            resources,
        }
    }
}

fn apply_answer(
    record: &mut QuotaRecord,
    answer: RateLimitAnswer,
    now: u64,
    claim_generation: u64,
) {
    if !record.refresh_in_flight || record.refresh_generation != claim_generation {
        return;
    }
    record.refresh_in_flight = false;
    record.refreshed_at = Some(now);
    let observed = match answer {
        RateLimitAnswer::Known(observed) => {
            record.refresh_error = None;
            observed
        }
        RateLimitAnswer::Unknown(reason) => {
            record.refresh_error = Some(reason.chars().take(MAX_ERROR_CHARS).collect());
            BTreeMap::new()
        }
    };
    for name in TRACKED_RESOURCES {
        let entry = record.resources.entry(name.to_string()).or_default();
        let debits = std::mem::take(&mut entry.in_flight_debits);
        match observed.get(name) {
            Some(quota) => {
                if entry.window_ends_at.is_some_and(|end| now >= end) {
                    entry.calls = CallCounts::default();
                    entry.refused = CallCounts::default();
                }
                if quota.reset_epoch_seconds > now {
                    entry.window_ends_at = Some(quota.reset_epoch_seconds);
                }
                entry.limit = Some(quota.limit);
                let remaining = if entry
                    .active_hold(now)
                    .is_some_and(|until| quota.reset_epoch_seconds <= until)
                {
                    0
                } else {
                    quota.remaining.saturating_sub(debits)
                };
                entry.remaining = Some(remaining);
                entry.reset_epoch_seconds = Some(quota.reset_epoch_seconds);
                entry.observed_at = Some(now);
                if quota.remaining == 0 && quota.reset_epoch_seconds > now {
                    entry.held_until =
                        Some(entry.held_until.map_or(quota.reset_epoch_seconds, |held| {
                            held.max(quota.reset_epoch_seconds)
                        }));
                }
            }
            None => {
                entry.limit = None;
                entry.remaining = None;
                entry.reset_epoch_seconds = None;
                entry.observed_at = None;
            }
        }
    }
}

/// `epoch` as an RFC 3339 UTC instant.
pub(crate) fn rfc3339(epoch: u64) -> Option<String> {
    let seconds = i64::try_from(epoch).ok()?;
    chrono::DateTime::from_timestamp(seconds, 0)
        .map(|instant| instant.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}
