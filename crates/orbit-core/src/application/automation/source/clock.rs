//! Monotonic time used by source budgets, separate from delivery timestamps.

use std::time::{Duration, Instant};

/// Frozen elapsed time for isolated fixtures, inherited by their CLI children.
/// Only dev-dependency builds with `test-support` recognize this variable.
#[cfg(feature = "test-support")]
pub const SOURCE_ELAPSED_ENV: &str = "ORBIT_TEST_SOURCE_ELAPSED_MS";

pub(in crate::application::automation) struct BudgetClock {
    started: Instant,
    #[cfg(feature = "test-support")]
    frozen: Option<Duration>,
}

impl BudgetClock {
    pub(in crate::application::automation) fn start() -> Self {
        Self {
            started: Instant::now(),
            #[cfg(feature = "test-support")]
            frozen: std::env::var(SOURCE_ELAPSED_ENV)
                .ok()
                .and_then(|value| value.parse().ok())
                .map(Duration::from_millis),
        }
    }

    /// A command gets its own timer with the source's clock policy.
    pub(super) fn restart(&self) -> Self {
        Self {
            started: Instant::now(),
            #[cfg(feature = "test-support")]
            frozen: self.frozen,
        }
    }

    pub(in crate::application::automation) fn elapsed(&self) -> Duration {
        #[cfg(feature = "test-support")]
        if let Some(elapsed) = self.frozen {
            return elapsed;
        }
        self.started.elapsed()
    }
}
