//! The forecast's memory at one minute: a high quantile of the deliveries'
//! summed memory, not the sum of their 90th percentiles.
//!
//! Each profiled delivery `i` alive with chance `s_i` contributes its
//! 90th-percentile memory `p_i · s_i`, its mean `μ_i · s_i` and the variance
//! of "alive with chance `s_i`, then drawn from that minute's deliveries",
//! `s_i · σ_i² + s_i · (1 − s_i) · μ_i²`. The deliveries' memory is
//! treated as independent, so their sum is about normal:
//!
//! ```text
//! M(t) = flat + clamp(Σ μ_i·s_i + z · sqrt(Σ var_i), max p_i·s_i, Σ p_i·s_i)
//! ```
//!
//! The clamp keeps one delivery at exactly its 90th percentile and never
//! forecasts more than the sum of percentiles did. `flat` is what is counted
//! as measured: the baseline and deliveries held at their current use.

use crate::runtime::host_resource::profile::ProfileMinute;

/// The normal quantile of the summed memory: its 95th percentile, above the
/// 90th each delivery is read at. Chosen by replaying recorded forecasts
/// against the host's measured minutes: it under-forecast no more often at
/// +5m than the sum of percentiles did.
pub(crate) const MEMORY_SUM_Z: f64 = 1.645;

/// What the forecast holds of memory at one minute.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct MemoryTerms {
    flat: f64,
    p90_sum: f64,
    p90_max: f64,
    mean_sum: f64,
    variance_sum: f64,
}

impl MemoryTerms {
    /// Count `bytes` as they are, without spread.
    pub(crate) fn add_flat(&mut self, bytes: f64) {
        self.flat += bytes;
    }

    /// Count a delivery described by `minute`, alive with chance `alive`.
    pub(crate) fn add_profiled(&mut self, minute: &ProfileMinute, alive: f64) {
        let p90 = minute.memory_bytes as f64 * alive;
        let mean = minute.memory_mean_bytes;
        self.p90_sum += p90;
        self.p90_max = self.p90_max.max(p90);
        self.mean_sum += mean * alive;
        self.variance_sum +=
            alive * minute.memory_stddev_bytes.powi(2) + alive * (1.0 - alive) * mean.powi(2);
    }

    /// These terms with a delivery described by `minute` added.
    pub(crate) fn with_profiled(mut self, minute: &ProfileMinute, alive: f64) -> Self {
        self.add_profiled(minute, alive);
        self
    }

    /// The forecast, `M(t)`, in whole bytes.
    pub(crate) fn bytes(&self) -> f64 {
        let quantile = self.mean_sum + MEMORY_SUM_Z * self.variance_sum.max(0.0).sqrt();
        (self.flat + quantile.max(self.p90_max).min(self.p90_sum)).round()
    }
}
