//! The model ranking (model-ranking design §2–§4, §6): one row per
//! implementer model, its crews and effort levels pooled, scored on six
//! measures against the implementer scorecard's same-mix expectations.
//!
//! Each measure's residual per unit is divided by a frozen noise scale,
//! the weighted mean over the measures that count for the row is shrunk
//! toward the other eligible models (K = 30), and a within-tier bootstrap
//! gives the interval, the paired probabilities and the tie bands, each band
//! ordered by score, with cost and model name breaking exact score ties.
//! Measures whose events Orbit does not record yet report
//! as unmeasured and leave every weight denominator.
//!
//! The ranked window is the 30 days ending at the settled cutoff,
//! `until − settle_lag`; `live` ends it at `until` and marks it
//! provisional. Scales come from the newest season `orbit agents rank
//! calibrate` stored; before the first one they are estimated from the
//! ranked window itself and the payload says so.

mod board;
mod compute;
mod execution;
mod measures;
mod query;
mod render;
pub(super) mod sample;
mod season;

pub use self::compute::{agent_rank, agent_rank_calibration};
pub use self::query::RankQuery;
pub use self::season::RankSeasons;
