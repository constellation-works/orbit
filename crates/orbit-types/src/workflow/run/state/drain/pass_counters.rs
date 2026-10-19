//! Admission pass outcomes, observations and cumulative counters.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// What limited one drain admission pass. Every counted pass has exactly one
/// outcome; the first that applies, in declaration order after `admitted`,
/// is the one recorded.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum DrainPassOutcome {
    /// It started at least one task, whatever else it held back.
    #[default]
    Admitted,
    /// It failed, or a local pass was deferred on a busy task-store lock.
    Failed,
    /// Admission was held outright: an operator stop or a host shutdown
    /// inside the hold horizon, and for a pull pass every other reason it
    /// sent no request although its window was open (breaker, refused
    /// settlement, owner probe refusal, no runnable crew, degraded drain).
    Held,
    /// Sustained host resource pressure held it.
    Throttle,
    /// A local pass had no admissible candidate: the backlog was empty or
    /// every task was excluded. A pull pass whose owner answered idle.
    NoCandidate,
    /// Every slot under the drain's live ceiling was occupied.
    CeilingFull,
    /// `enforce` gap fit kept every candidate out; for a pull pass, the
    /// forecast had room for no work (`forecast_no_headroom`).
    GapFitHeld,
    /// A local pass had candidates and free slots, but every candidate was
    /// deferred: a lock conflict, a claim, or a footprint wait.
    Deferred,
}

impl DrainPassOutcome {
    /// Every outcome, in precedence order.
    pub const ALL: [Self; 8] = [
        Self::Admitted,
        Self::Failed,
        Self::Held,
        Self::Throttle,
        Self::NoCandidate,
        Self::CeilingFull,
        Self::GapFitHeld,
        Self::Deferred,
    ];

    /// The snake-case name the counters are keyed by.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admitted => "admitted",
            Self::Failed => "failed",
            Self::Held => "held",
            Self::Throttle => "throttle",
            Self::NoCandidate => "no_candidate",
            Self::CeilingFull => "ceiling_full",
            Self::GapFitHeld => "gap_fit_held",
            Self::Deferred => "deferred",
        }
    }
}

/// One pass's outcome with what it names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DrainPassObservation {
    pub outcome: DrainPassOutcome,
    /// For [`DrainPassOutcome::Throttle`]: each pressured resource, as
    /// `cpu`, `memory`, `io` or `disk`.
    pub throttled: BTreeSet<String>,
    /// For [`DrainPassOutcome::Held`]: the hold's reason code.
    pub held_reason: Option<String>,
    /// Every reason that kept at least one backlog task waiting in this pass,
    /// whatever its outcome.
    pub waiting_reasons: BTreeSet<String>,
}

/// Cumulative counts of what limited a drain's admission passes.
///
/// `passes` is the denominator of every share: each counted pass adds one to
/// `passes` and one to exactly one entry of `outcomes`, so the outcome shares
/// sum to one. `throttle_by_resource` and `held_by_reason` split those two
/// outcomes. `waiting_by_reason` counts the passes in which at least one task
/// waited for a reason, whatever the pass's outcome, so its entries overlap.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct DrainPassCounters {
    /// When the first counted pass ran.
    pub since: DateTime<Utc>,
    /// When the latest counted pass ran.
    pub last_counted_at: DateTime<Utc>,
    pub passes: u64,
    /// Passes per outcome; an outcome no pass had is absent.
    #[serde(default)]
    pub outcomes: BTreeMap<DrainPassOutcome, u64>,
    /// Throttled passes per pressured resource; one pass can count several.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub throttle_by_resource: BTreeMap<String, u64>,
    /// Held passes per reason code; they sum to the `held` outcome.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub held_by_reason: BTreeMap<String, u64>,
    /// Passes in which a reason kept at least one backlog task waiting.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub waiting_by_reason: BTreeMap<String, u64>,
}

impl DrainPassCounters {
    /// Add one pass.
    pub fn count(&mut self, observation: &DrainPassObservation, at: DateTime<Utc>) {
        self.passes = self.passes.saturating_add(1);
        self.last_counted_at = at;
        bump(&mut self.outcomes, observation.outcome);
        if observation.outcome == DrainPassOutcome::Throttle {
            for resource in &observation.throttled {
                bump(&mut self.throttle_by_resource, resource.clone());
            }
        }
        if observation.outcome == DrainPassOutcome::Held {
            bump(
                &mut self.held_by_reason,
                observation
                    .held_reason
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
            );
        }
        for reason in &observation.waiting_reasons {
            bump(&mut self.waiting_by_reason, reason.clone());
        }
    }

    /// The passes that had `outcome`.
    #[must_use]
    pub fn outcome(&self, outcome: DrainPassOutcome) -> u64 {
        self.outcomes.get(&outcome).copied().unwrap_or(0)
    }

    /// `count` as a share of `passes`, `0..=1`; zero before any pass.
    #[must_use]
    pub fn share(&self, count: u64) -> f64 {
        if self.passes == 0 {
            0.0
        } else {
            count as f64 / self.passes as f64
        }
    }

    /// `12 passes since 2026-10-18 07:00Z: ceiling_full 7 (58%), admitted 3
    /// (25%), held 2 (17%); held by circuit_open 2; waiting:
    /// context_lock_conflict in 9 (75%)`. Outcomes are listed most frequent
    /// first.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut outcomes: Vec<(DrainPassOutcome, u64)> = DrainPassOutcome::ALL
            .into_iter()
            .map(|outcome| (outcome, self.outcome(outcome)))
            .filter(|(_, count)| *count > 0)
            .collect();
        outcomes.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        let outcomes = outcomes
            .into_iter()
            .map(|(outcome, count)| format!("{} {}", outcome.as_str(), self.counted(count)))
            .collect::<Vec<_>>()
            .join(", ");
        let mut text = format!(
            "{} passes since {}: {outcomes}",
            self.passes,
            self.since.format("%Y-%m-%d %H:%MZ")
        );
        let detail = self.describe_detail();
        if !detail.is_empty() {
            text.push_str("; ");
            text.push_str(&detail);
        }
        text
    }

    /// What splits the `held` and `throttle` outcomes, and the waiting
    /// reasons, as [`Self::describe`] ends; empty when there are none.
    #[must_use]
    pub fn describe_detail(&self) -> String {
        let listed = |map: &BTreeMap<String, u64>, with_share: bool| {
            map.iter()
                .map(|(key, count)| {
                    if with_share {
                        format!("{key} in {}", self.counted(*count))
                    } else {
                        format!("{key} {count}")
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        [
            ("held by", listed(&self.held_by_reason, false)),
            ("throttled by", listed(&self.throttle_by_resource, false)),
            ("waiting:", listed(&self.waiting_by_reason, true)),
        ]
        .into_iter()
        .filter(|(_, list)| !list.is_empty())
        .map(|(label, list)| format!("{label} {list}"))
        .collect::<Vec<_>>()
        .join("; ")
    }

    /// `7 (58%)`.
    fn counted(&self, count: u64) -> String {
        format!("{count} ({:.0}%)", self.share(count) * 100.0)
    }
}

fn bump<K: Ord>(map: &mut BTreeMap<K, u64>, key: K) {
    let entry = map.entry(key).or_insert(0);
    *entry = entry.saturating_add(1);
}
