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
            started: super::budget_now(),
            #[cfg(feature = "test-support")]
            frozen: std::env::var(SOURCE_ELAPSED_ENV)
                .ok()
                .and_then(|value| value.parse().ok())
                .map(Duration::from_millis),
        }
    }

    /// The elapsed reading does not advance. `SOURCE_ELAPSED_ENV` freezes it
    /// for a whole process, including commands that outlive a wall-clock budget.
    #[cfg(feature = "test-support")]
    pub(super) fn is_frozen(&self) -> bool {
        self.frozen.is_some()
    }

    /// A command gets its own timer with the source's clock policy.
    ///
    /// Only the test-support path restarts a clock: a frozen elapsed is copied
    /// as-is, so a fixture that has already spent the command budget stays spent.
    #[cfg(feature = "test-support")]
    pub(super) fn restart(&self) -> Self {
        Self {
            started: super::budget_now(),
            #[cfg(feature = "test-support")]
            frozen: self.frozen,
        }
    }

    pub(in crate::application::automation) fn elapsed(&self) -> Duration {
        #[cfg(feature = "test-support")]
        if let Some(elapsed) = self.frozen {
            return elapsed;
        }
        super::budget_now().saturating_duration_since(self.started)
    }
}
