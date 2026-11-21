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
        Instant::now() + OFFSET.get()
    }
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
