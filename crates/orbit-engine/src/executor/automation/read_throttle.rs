//! When a GitHub read the provider rate-limited may repeat.
//!
//! An adapter types an authenticated throttling answer
//! ([`ProviderReadThrottle`]). Auto-fix completion, the owner's landing and
//! the CI-failure sweep all wait it out the same way: until the later of the
//! provider's `Retry-After` and its rate-limit reset, never sooner than
//! [`MIN_PROVIDER_WAIT_MS`] from now, or, when the answer named neither, a
//! conservative backoff that doubles per attempt.

use orbit_common::ProviderReadThrottle;

/// The first wait when the provider named no reset, doubled per attempt.
const UNKNOWN_RESET_BACKOFF_MS: u64 = 60_000;
const MAX_UNKNOWN_RESET_BACKOFF_MS: u64 = 300_000;
/// The least wait a provider-named reset earns, so a reset already past by
/// this host's clock cannot make the next read immediate.
const MIN_PROVIDER_WAIT_MS: u64 = 1_000;

/// The earliest time a throttled read may repeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::executor::automation) struct ReadNotBefore {
    /// Milliseconds since the Unix epoch.
    pub(in crate::executor::automation) at_ms: u64,
    /// Whether the provider named it (`Retry-After` or a reset), rather than
    /// the unknown-reset backoff.
    pub(in crate::executor::automation) provider_named: bool,
}

/// When the read `throttle` refused may repeat, for the `attempt`th wait
/// (1-based) observed at `now_ms`.
pub(in crate::executor::automation) fn read_not_before(
    throttle: &ProviderReadThrottle,
    now_ms: u64,
    attempt: u64,
) -> ReadNotBefore {
    let provider = [
        throttle
            .retry_after_seconds
            .map(|seconds| now_ms.saturating_add(seconds.saturating_mul(1000))),
        throttle
            .reset_epoch_seconds
            .map(|seconds| seconds.saturating_mul(1000)),
    ]
    .into_iter()
    .flatten()
    .max();
    match provider {
        Some(at) => ReadNotBefore {
            at_ms: at.max(now_ms.saturating_add(MIN_PROVIDER_WAIT_MS)),
            provider_named: true,
        },
        None => {
            let doublings = u32::try_from(attempt.saturating_sub(1).min(16)).unwrap_or(16);
            ReadNotBefore {
                at_ms: now_ms.saturating_add(
                    UNKNOWN_RESET_BACKOFF_MS
                        .saturating_mul(1_u64 << doublings)
                        .min(MAX_UNKNOWN_RESET_BACKOFF_MS),
                ),
                provider_named: false,
            }
        }
    }
}

/// `at_ms` as an RFC 3339 UTC time, or the raw milliseconds when it is out
/// of range.
pub(in crate::executor::automation) fn rfc3339_ms(at_ms: u64) -> String {
    i64::try_from(at_ms)
        .ok()
        .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
        .map_or_else(
            || format!("{at_ms} ms"),
            |at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        )
}
