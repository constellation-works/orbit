#![allow(missing_docs)]

//! Step backoff arithmetic for `step.rs`.

use super::*;

#[test]
fn compute_backoff_ms_respects_initial_max_and_zero_attempt_boundary() {
    // Invariant: `compute_backoff_ms` is monotonic with attempt index (linear
    // strategy) and never exceeds the cap. Pure unit test — no host required.
    let retry = RetrySpec {
        max_attempts: 5,
        initial_backoff_ms: 100,
        backoff_cap_ms: 250,
        backoff_strategy: BackoffStrategy::Linear,
    };
    // Linear: shifted = initial * (attempt_index + 1)
    assert_eq!(compute_backoff_ms(&retry, 0), 100); // 100 * 1
    assert_eq!(compute_backoff_ms(&retry, 1), 200); // 100 * 2
    assert_eq!(compute_backoff_ms(&retry, 2), 250); // 300 capped to 250
    assert_eq!(compute_backoff_ms(&retry, 5), 250); // capped

    let exp = RetrySpec {
        max_attempts: 5,
        initial_backoff_ms: 50,
        backoff_cap_ms: 1000,
        backoff_strategy: BackoffStrategy::Exponential,
    };
    assert_eq!(compute_backoff_ms(&exp, 0), 50); // 50 << 0
    assert_eq!(compute_backoff_ms(&exp, 1), 100); // 50 << 1
    assert_eq!(compute_backoff_ms(&exp, 4), 800); // 50 << 4
    assert_eq!(compute_backoff_ms(&exp, 10), 1000); // capped
}
