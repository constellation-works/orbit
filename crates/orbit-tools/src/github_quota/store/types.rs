use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;
/// GitHub's REST bucket.
pub const RESOURCE_CORE: &str = "core";
/// GitHub's GraphQL bucket, counted apart from `core`.
pub const RESOURCE_GRAPHQL: &str = "graphql";
pub(super) const TRACKED_RESOURCES: [&str; 2] = [RESOURCE_CORE, RESOURCE_GRAPHQL];

/// Default share of a resource's limit kept back from agent reads.
pub const DEFAULT_AGENT_RESERVE_PERCENT: u8 = 20;
/// Default share of a resource's limit kept back from background reads.
pub const DEFAULT_BACKGROUND_RESERVE_PERCENT: u8 = 10;
/// Default shortest interval between two rate-limit endpoint reads on a host.
pub const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// Who is spending the host's GitHub quota.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GithubCaller {
    /// The private VCS operations behind PR open, completion, landing and merge,
    /// and every request the task PR forge makes. Runs the quota to zero.
    Delivery,
    /// The CI and security-alert sweeps, auto-task sources, and `github.*`
    /// calls a deterministic job step makes.
    Background,
    /// Every `github.*` tool call a deterministic job step does not make,
    /// attention's CI read included.
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

    pub(super) fn refresh_seconds(&self) -> u64 {
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
    /// The read was cut short by one caller's own deadline (a probe given less
    /// than the standard refresh timeout that ran out, or no time at all). It
    /// says nothing about the account, so it neither claims the interval nor
    /// replaces the record: the next caller reads the endpoint itself.
    Clipped(String),
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
    pub(super) fn bump(&mut self, caller: GithubCaller) {
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
