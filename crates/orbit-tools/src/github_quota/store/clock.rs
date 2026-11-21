use std::time::{Duration, Instant};
/// The monotonic instant admission and the request-budget checks compare with a
/// request's deadline.
pub(in crate::github_quota) fn monotonic_now() -> Instant {
    #[cfg(any(test, feature = "test-support"))]
    return refusal_clock::now();
    #[cfg(not(any(test, feature = "test-support")))]
    Instant::now()
}

/// Whether less than one whole millisecond of the budget ending at `deadline`
/// remains: the point where a caller's floored remaining budget reads zero and
/// it returns its timeout instead of spawning.
pub(in crate::github_quota) fn budget_spent(deadline: Instant) -> bool {
    deadline.saturating_duration_since(monotonic_now()) < Duration::from_millis(1)
}

/// A deadline clock a fixture can advance while a record's refusal is being
/// evaluated, the instant between the record lock and the deadline check.
#[cfg(any(test, feature = "test-support"))]
pub(super) mod refusal_clock {
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    thread_local! {
        static ADVANCE_AT_REFUSAL: Cell<Option<Duration>> = const { Cell::new(None) };
        static ADVANCE_AFTER_DECISION: Cell<Option<Duration>> = const { Cell::new(None) };
        static OFFSET: Cell<Duration> = const { Cell::new(Duration::ZERO) };
        static FIXED: Cell<Option<Instant>> = const { Cell::new(None) };
    }

    pub(super) fn arm(by: Duration) {
        OFFSET.set(Duration::ZERO);
        ADVANCE_AT_REFUSAL.set(Some(by));
    }

    pub(super) fn arm_after_decision(by: Duration) {
        OFFSET.set(Duration::ZERO);
        ADVANCE_AFTER_DECISION.set(Some(by));
    }

    pub(in crate::github_quota::store) fn at_refusal_evaluation() {
        if let Some(by) = ADVANCE_AT_REFUSAL.take() {
            OFFSET.set(by);
        }
    }

    pub(in crate::github_quota::store) fn after_refusal_decision() {
        if let Some(by) = ADVANCE_AFTER_DECISION.take() {
            OFFSET.set(by);
        }
    }

    pub(super) fn now() -> Instant {
        FIXED.get().unwrap_or_else(|| Instant::now() + OFFSET.get())
    }

    pub(super) fn with_fixed<T>(body: impl FnOnce() -> T) -> T {
        struct Reset(Option<Instant>);
        impl Drop for Reset {
            fn drop(&mut self) {
                FIXED.set(self.0);
            }
        }
        let _reset = Reset(FIXED.replace(Some(Instant::now())));
        body()
    }

    /// Advance this thread's clock to at least `deadline`, never back.
    pub(super) fn reach(deadline: Instant) {
        if FIXED.get().is_some() {
            FIXED.set(Some(deadline));
            return;
        }
        let behind = deadline.saturating_duration_since(Instant::now());
        if behind > OFFSET.get() {
            OFFSET.set(behind);
        }
    }
}

/// Advance this thread's admission deadline clock to `deadline`, where a
/// probe the deadline cut short leaves it in production. Test fixtures only:
/// the probe-timeout seam in `request.rs` calls it when it ends a probe early.
#[cfg(any(test, feature = "test-support"))]
pub(in crate::github_quota) fn reach_deadline(deadline: Instant) {
    refusal_clock::reach(deadline);
}

/// Make this thread's next refusal evaluation advance the admission deadline
/// clock by `by` for the rest of the thread, so a request deadline crosses
/// during that evaluation. Test fixtures only.
#[cfg(any(test, feature = "test-support"))]
pub fn advance_deadline_clock_at_next_refusal(by: Duration) {
    refusal_clock::arm(by);
}

/// Make this thread's next refusal, once its decision is published and before
/// its caller delivers it, advance the admission deadline clock by `by` for the
/// rest of the thread, so a request deadline crosses after the quota decision.
/// Test fixtures only.
#[cfg(any(test, feature = "test-support"))]
pub fn advance_deadline_clock_after_next_refusal_decision(by: Duration) {
    refusal_clock::arm_after_decision(by);
}

/// Freeze the admission clock during a deterministic probe fixture.
#[cfg(any(test, feature = "test-support"))]
pub(in crate::github_quota) fn with_fixed_clock<T>(body: impl FnOnce() -> T) -> T {
    refusal_clock::with_fixed(body)
}
