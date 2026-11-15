//! Deterministic tickets for crew pool draws in tests.
//!
//! A draw's outcome is random, and a weighting can only make it likely: usage
//! pressure caps a member at ×8 and floors it at ×1/8, so the losing member of
//! a pressure-decided two-member draw keeps about one ticket in 65. Pinning
//! where the ticket lands lets a test assert the outcome the weights decide.

use std::cell::Cell;

thread_local! {
    static DRAW_QUANTILE: Cell<Option<f64>> = const { Cell::new(None) };
}

/// Run `operation` with every crew pool draw on this thread taking the ticket
/// at `quantile` of its pool's total weight (`0.0` the first ticket, just
/// under `1.0` the last), instead of a random one. The previous setting is
/// restored on return or unwind; other threads keep drawing at random.
///
/// # Panics
///
/// When `quantile` is outside `[0.0, 1.0)`.
pub fn with_crew_draw_quantile<T>(quantile: f64, operation: impl FnOnce() -> T) -> T {
    assert!(
        (0.0..1.0).contains(&quantile),
        "crew draw quantile {quantile} is outside [0, 1)"
    );
    struct Restore(Option<f64>);
    impl Drop for Restore {
        fn drop(&mut self) {
            DRAW_QUANTILE.set(self.0);
        }
    }
    let _restore = Restore(DRAW_QUANTILE.replace(Some(quantile)));
    operation()
}

/// The pinned ticket in `[0, bound)`, when a test pinned the draw on this
/// thread.
pub(super) fn pinned_crew_ticket(bound: u64) -> Option<u64> {
    let quantile = DRAW_QUANTILE.get()?;
    // `bound` counts whole tickets well inside f64's exact integer range, and
    // `quantile < 1.0` keeps the product below it.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let ticket = (quantile * bound as f64) as u64;
    Some(ticket.min(bound - 1))
}
