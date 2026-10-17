//! The memory term of the forecast: a high quantile of the summed memory of
//! the deliveries it counts, not the sum of their high quantiles.
//!
//! Each profile minute contributes its center, the lower of its mean and its
//! 90th percentile, and its margin, the 90th percentile above that center,
//! both weighted by the chance the delivery is still running. The minute's
//! memory is
//!
//! ```text
//! B + Σ center_i + sqrt(Σ margin_i²)
//! ```
//!
//! The margins of independent deliveries add in quadrature, as standard
//! deviations do, so the sum of `N` similar deliveries gains about `√N`
//! margins rather than `N`. With one delivery the term is its 90th
//! percentile, and it is never below the largest one's.

use crate::runtime::host_resource::profile::ProfileMinute;

/// The summed memory at one forecast minute, in bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct MemorySum {
    /// The baseline, held-flat use and every delivery's center.
    center: f64,
    /// The sum of every delivery's squared margin.
    margin_squares: f64,
}

impl MemorySum {
    /// Measured or held-flat use, which has no margin.
    pub(crate) fn add_flat(&mut self, bytes: f64) {
        self.center += bytes;
    }

    /// `minute`'s memory scaled by `weight`, the chance it is still running.
    pub(crate) fn add_minute(&mut self, minute: &ProfileMinute, weight: f64) {
        let (center, margin) = parts(minute, weight);
        self.center += center;
        self.margin_squares += margin * margin;
    }

    /// One of several alternatives of which exactly one will run: the
    /// largest center and the largest margin among them, so the result is
    /// never below any one alternative's.
    pub(crate) fn add_alternative(&mut self, minute: &ProfileMinute, weight: f64) {
        let (center, margin) = parts(minute, weight);
        self.center = self.center.max(center);
        self.margin_squares = self.margin_squares.max(margin * margin);
    }

    /// Count `other` as independent of what this already holds.
    pub(crate) fn add(&mut self, other: Self) {
        self.center += other.center;
        self.margin_squares += other.margin_squares;
    }

    /// The forecast bytes.
    pub(crate) fn bytes(&self) -> f64 {
        self.center + self.margin_squares.sqrt()
    }
}

/// `minute`'s weighted center and margin.
fn parts(minute: &ProfileMinute, weight: f64) -> (f64, f64) {
    let high = minute.memory_bytes as f64;
    let center = minute.memory_mean_bytes.min(high);
    (center * weight, (high - center) * weight)
}
