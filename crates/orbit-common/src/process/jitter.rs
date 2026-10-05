//! Tiny non-cryptographic PRNG for retry backoff jitter.
//!
//! Retry loops that sleep a deterministic exponential backoff synchronize
//! their wake-ups when many workers fail against the same dependency
//! (thundering herd). "Full jitter" (AWS architecture blog) replaces the
//! deterministic sleep with a uniform draw over `[0, bound]`, decorrelating
//! the retries while keeping the same cap growth.
//!
//! The generator is an xorshift64* seeded through SplitMix64. Operating-system
//! entropy supplies production seeds; this remains unsuitable for anything
//! security-sensitive.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Fallback seed used when the entropy sources collapse to zero
/// (xorshift64* has a fixed point at state 0).
const SEED_FALLBACK: u64 = 0x9e37_79b9_7f4a_7c15;
/// Separates fallback seeds if the operating system cannot provide entropy.
static FALLBACK_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Small xorshift64* PRNG for retry jitter. Not cryptographic.
#[derive(Debug, Clone)]
pub struct JitterRng {
    state: u64,
}

impl JitterRng {
    /// Construct from operating-system entropy so production retry streams
    /// are independent of application identifiers.
    pub fn from_entropy() -> Self {
        let seed = getrandom::u64().unwrap_or_else(|_| fallback_seed());
        Self::from_seed(seed)
    }

    /// Construct from an explicit seed (deterministic; intended for tests).
    pub fn from_seed(seed: u64) -> Self {
        // Run the raw seed through SplitMix64 so weak/adjacent seeds still
        // produce well-mixed initial states, then guard the zero fixed point.
        let mixed = splitmix64(seed);
        Self {
            state: if mixed == 0 { SEED_FALLBACK } else { mixed },
        }
    }

    /// Next pseudo-random `u64` (xorshift64* step).
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// Full-jitter sample: a value in `[0, bound]` (inclusive). Returns 0
    /// when `bound` is 0 so zero-backoff configs stay zero-backoff.
    pub fn full_jitter(&mut self, bound: u64) -> u64 {
        if bound == u64::MAX {
            return self.next_u64();
        }
        // Modulo bias is negligible for backoff purposes (bounds are tiny
        // relative to u64::MAX) and irrelevant to correctness.
        self.next_u64() % (bound + 1)
    }
}

fn fallback_seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    let sequence = FALLBACK_COUNTER.fetch_add(1, Ordering::Relaxed);

    nanos ^ u64::from(std::process::id()) ^ sequence
}

/// SplitMix64 finalizer — mixes a raw seed into a well-distributed state.
fn splitmix64(seed: u64) -> u64 {
    let mut z = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}
