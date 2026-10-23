//! How well the recorded admission forecasts matched the host's measured
//! load, and how far shadow decisions differed from what admission did.
//!
//! - **Pairs.** A point forecast at `forecast_at` for `+n` minutes pairs with
//!   the `host_resource_minutes` row of the minute starting at
//!   `forecast_at`, truncated to the minute, plus `n`. A point whose minute
//!   has not fully ended at report time is pending, even with a partial
//!   measurement. Ended minutes without a row are missing: `unsampled` when
//!   no delivery worker overlapped that minute, `partial` when one did but
//!   none of them saw the minute whole, and `gap` when one was running for
//!   the whole minute.
//!   Pending and missing both count as unmeasured; only ended, measured
//!   minutes contribute error pairs.
//! - **Error** is forecast minus measured, so a negative error is an
//!   under-forecast: the host carried more than admission expected.
//! - **Attribution** includes only a complete, resolvable covered cohort.
//!   Forecasts containing a pending pull reservation have no attributed pair
//!   until its synthetic request identity can be resolved to the claimed task.
//! - **Shadow decisions.** Per drain, over its `shadow` passes: the tasks
//!   admission admitted that gap fit would have held, and the tasks gap fit
//!   would have admitted that admission did not. Each task is counted once
//!   per drain, however many passes it appeared in. Pull drains separately
//!   count request forecasts, requests sent, and shadow requests enforce
//!   would have held because no work fit, even without a returned claim.
//! - **Enforce decisions.** Per drain, over its local `enforce` passes: the
//!   distinct tasks the pass admitted, and the distinct tasks gap fit held
//!   (`fit_skipped`). A task held on one pass and admitted on another counts
//!   in both. Pull drains separately count enforce requests held because no
//!   work fit. Shadow columns stay on shadow passes only.
//! - **Limiters.** Per drain, what limited its admission passes, from the
//!   cumulative pass counters on its run state: each outcome's share of its
//!   counted passes, which sum to one, and each waiting reason's share of
//!   them, which overlap. The selection picks the drains — those with a
//!   selected forecast and, without `mode`, this workspace's drains live at
//!   or after `since` — but the counters cover each drain's whole lineage.

mod attribution;
mod report;
mod shadow;
mod types;

pub(crate) use report::forecast_accuracy;
pub use types::{
    AccuracySelection, DrainLimiters, DrainShadowDecisions, ErrorSummary, ForecastAccuracy,
    ForecastPair, OffsetAccuracy, WorstMiss,
};
