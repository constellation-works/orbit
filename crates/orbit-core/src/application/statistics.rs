//! Shared statistics for the analytics modules: a seeded SplitMix64
//! generator, the 95% Wilson score interval, and the median and
//! linearly interpolated percentile of a sample.
//!
//! A leaf: it imports no other application module, so any of them may use it.
//! Seeds stay with their owners; this module only turns a seed into a fixed
//! draw sequence, so the same seed always gives the same numbers.

/// The 97.5th percentile of the standard normal distribution.
const Z_95: f64 = 1.959_963_984_540_054;

/// SplitMix64: small, fast and fully determined by its seed.
pub(crate) struct SplitMix64(u64);

impl SplitMix64 {
    pub(crate) fn seeded(seed: u64) -> Self {
        Self(seed)
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// An index below `len`, which must be positive.
    pub(crate) fn index(&mut self, len: usize) -> usize {
        // `len` fits in u64 and the remainder is below it, so both casts hold.
        (self.next_u64() % len as u64) as usize
    }

    /// Uniform in (0, 1].
    pub(crate) fn unit(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 1.0) / (1u64 << 53) as f64
    }
}

/// The 95% Wilson score interval, low then high, of `successes` in `n`
/// trials; `None` when `n` is zero. The bounds are exactly 0 and 1 at the
/// edges, where rounding would otherwise leave residue.
pub(crate) fn wilson_95(successes: usize, n: usize) -> Option<[f64; 2]> {
    if n == 0 {
        return None;
    }
    let trials = n as f64;
    let p = successes as f64 / trials;
    let z2 = Z_95 * Z_95;
    let centre = (p + z2 / (2.0 * trials)) / (1.0 + z2 / trials);
    let half =
        Z_95 * (p * (1.0 - p) / trials + z2 / (4.0 * trials * trials)).sqrt() / (1.0 + z2 / trials);
    let low = if successes == 0 {
        0.0
    } else {
        (centre - half).max(0.0)
    };
    let high = if successes == n {
        1.0
    } else {
        (centre + half).min(1.0)
    };
    Some([low, high])
}

/// The median of `values`, or `None` when empty.
///
/// Sorts `values` in place with `f64::total_cmp`. The result is the mean of
/// the two central samples, which is the middle sample when the count is odd.
pub(crate) fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let n = values.len();
    Some(if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    })
}

/// The `pct` (0–100) percentile of `values` by linear interpolation, or
/// `None` when empty. Sorts `values` in place with `f64::total_cmp`.
pub(crate) fn percentile(values: &mut [f64], pct: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let rank = pct / 100.0 * (values.len() - 1) as f64;
    let (low, high) = (rank.floor() as usize, rank.ceil() as usize);
    Some(values[low] + (values[high] - values[low]) * (rank - low as f64))
}
