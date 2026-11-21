#[cfg(any(test, feature = "test-support"))]
use super::clock::refusal_clock;
use super::types::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
/// The counting window used while GitHub's reset time is unknown.
const FALLBACK_WINDOW_SECONDS: u64 = 3600;
/// The longest refresh diagnostic the record keeps.
const MAX_ERROR_CHARS: usize = 240;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct QuotaRecord {
    #[serde(default)]
    pub(super) refresh_claimed_at: Option<u64>,
    #[serde(default)]
    pub(super) refresh_generation: u64,
    #[serde(default)]
    pub(super) refresh_in_flight: bool,
    #[serde(default)]
    pub(super) refreshed_at: Option<u64>,
    #[serde(default)]
    pub(super) refresh_error: Option<String>,
    #[serde(default)]
    pub(super) resources: BTreeMap<String, ResourceRecord>,
}

/// Lock-free release marker staged when a clipped probe cannot acquire the
/// record lock before its deadline expires.
#[derive(Debug, Clone, Copy, Serialize)]
pub(super) struct ClippedClaimRelease {
    pub(super) generation: u64,
    /// Always written: a claim timestamp, or an explicit `null` for a first
    /// claim. An absent key is ambiguous and must not decode as `null`.
    pub(super) previous_claim: Option<u64>,
}

impl<'de> Deserialize<'de> for ClippedClaimRelease {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct ClippedClaimReleaseVisitor;

        impl<'de> serde::de::Visitor<'de> for ClippedClaimReleaseVisitor {
            type Value = ClippedClaimRelease;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a clipped claim release object")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut generation = None;
                let mut previous_claim = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "generation" => {
                            if generation.is_some() {
                                return Err(<A::Error as serde::de::Error>::duplicate_field(
                                    "generation",
                                ));
                            }
                            generation = Some(map.next_value()?);
                        }
                        "previous_claim" => {
                            if previous_claim.is_some() {
                                return Err(<A::Error as serde::de::Error>::duplicate_field(
                                    "previous_claim",
                                ));
                            }
                            previous_claim = Some(map.next_value()?);
                        }
                        _ => {
                            let _: serde::de::IgnoredAny = map.next_value()?;
                        }
                    }
                }

                Ok(ClippedClaimRelease {
                    generation: generation.ok_or_else(|| {
                        <A::Error as serde::de::Error>::missing_field("generation")
                    })?,
                    previous_claim: previous_claim.ok_or_else(|| {
                        <A::Error as serde::de::Error>::missing_field("previous_claim")
                    })?,
                })
            }
        }

        deserializer.deserialize_map(ClippedClaimReleaseVisitor)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct ResourceRecord {
    #[serde(default)]
    pub(super) limit: Option<u64>,
    #[serde(default)]
    pub(super) remaining: Option<u64>,
    #[serde(default)]
    pub(super) reset_epoch_seconds: Option<u64>,
    #[serde(default)]
    pub(super) observed_at: Option<u64>,
    /// No read of the resource leaves the host before this instant.
    #[serde(default)]
    pub(super) held_until: Option<u64>,
    #[serde(default)]
    pub(super) window_ends_at: Option<u64>,
    #[serde(default)]
    pub(super) calls: CallCounts,
    #[serde(default)]
    pub(super) refused: CallCounts,
    /// Reads admitted while an external rate-limit probe was in flight and not
    /// yet committed by [`apply_answer`].
    #[serde(default)]
    pub(super) in_flight_debits: u64,
}

impl ResourceRecord {
    /// The observation, while it still describes the current window.
    pub(super) fn current(&self, now: u64) -> Option<(u64, u64, u64)> {
        match (self.limit, self.remaining, self.reset_epoch_seconds) {
            (Some(limit), Some(remaining), Some(reset)) if reset > now => {
                Some((limit, remaining, reset))
            }
            _ => None,
        }
    }

    pub(super) fn active_hold(&self, now: u64) -> Option<u64> {
        self.held_until.filter(|until| *until > now)
    }

    /// Start a new counting window once the current one ended.
    pub(super) fn roll_window(&mut self, now: u64) {
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

    pub(super) fn refusal(
        &self,
        policy: &GithubQuotaPolicy,
        caller: GithubCaller,
        resource: &str,
        now: u64,
    ) -> Option<QuotaRefusal> {
        #[cfg(any(test, feature = "test-support"))]
        refusal_clock::at_refusal_evaluation();
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

pub(super) fn bounded_error(reason: &str) -> String {
    reason.chars().take(MAX_ERROR_CHARS).collect()
}

pub(super) fn release_clipped_claim(record: &mut QuotaRecord, previous_claim: Option<u64>) {
    // Give the interval back and keep what the record knew. Debits taken
    // during the read were already applied to the known remaining count.
    record.refresh_in_flight = false;
    record.refresh_claimed_at = previous_claim;
    for entry in record.resources.values_mut() {
        entry.in_flight_debits = 0;
    }
}

pub(super) fn apply_answer(
    record: &mut QuotaRecord,
    answer: RateLimitAnswer,
    now: u64,
    claim_generation: u64,
    previous_claim: Option<u64>,
) {
    if !record.refresh_in_flight || record.refresh_generation != claim_generation {
        return;
    }
    if matches!(answer, RateLimitAnswer::Clipped(_)) {
        release_clipped_claim(record, previous_claim);
        return;
    }
    record.refresh_in_flight = false;
    record.refreshed_at = Some(now);
    let observed = match answer {
        RateLimitAnswer::Known(observed) => {
            record.refresh_error = None;
            observed
        }
        RateLimitAnswer::Unknown(reason) | RateLimitAnswer::Clipped(reason) => {
            record.refresh_error = Some(bounded_error(&reason));
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
