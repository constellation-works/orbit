//! Backlog forecast for Analytics › Forecast [ORB-18385]: how long the
//! dispatchable backlog takes to drain, and what it costs in USD and
//! normalized tokens, as p50 and p90 from resampled delivery history, plus
//! the calibration of earlier stored forecasts.
//!
//! The method is documented in `docs/design/backlog-forecast/1_scope.md`. In short:
//!
//! - A delivering attempt draws one completed delivery: a task that reached
//!   `done` inside the lookback, with every managed invocation linked to it
//!   since its last attempt that did not deliver (recoveries, auto-fixes
//!   and reworks included). Its hours are the union of those invocations'
//!   run intervals, bridging gaps shorter than [`usage::GAP_BRIDGE_HOURS`]; its
//!   tokens and USD are their normalized token and provider cost sums.
//! - Each attempt first draws an admission episode from status history,
//!   delivered or not, and a blocked one draws its return from a
//!   competing-risk return curve [ORB-18688] (`outcomes.rs`). Blocked tasks
//!   join the cohort waiting for that return.
//! - Each task draws from its complexity × crew-pool × review-mode cell.
//!   Thin cells drop crew, then complexity, preserving mode at five samples;
//!   only then use the mode-pooled cell, complexity and workspace history.
//! - The drain is simulated on the drain's leaf-run ceiling
//!   ([`crate::OrbitRuntime::drain_concurrency_ceiling`]), with no new arrivals
//!   unless `arrivals=on`. Each iteration admits work as the drain would
//!   [ORB-18686]: in its dispatch order, once in-forecast dependencies have
//!   finished, never beside overlapping locks, with unknown footprints run
//!   alone and surface reservations honoured (`schedule.rs`, through the
//!   admission module's forecast adapter). A task whose dependency the
//!   forecast cannot schedule leaves the full-backlog totals unknown and is
//!   reported with a schedulable subset (`dependencies.rs`).
//! - Where the drain enforces gap fit, each simulated pass also fits
//!   candidates against the host's CPU and memory, from this host's profiles,
//!   size and baseline [ORB-18689] (`resource_fit.rs`). The ceiling stays the
//!   upper bound; gap fit only holds work below it.
//!
//! - The lookback is counted in tasks as well as days: it reaches back past
//!   its days to the newest [`options::MIN_HISTORY_TASKS`] completions, so a month
//!   without deliveries never leaves the forecast without history.
//!   `history_tasks` instead selects the newest N completions, whatever
//!   their age.
//! - Each attempt's hours, tokens and USD are then shifted by a
//!   per-metric, per-complexity correction learned from scored residuals
//!   of the current model version (`correction.rs`). A cohort with too few
//!   residuals is left unchanged. Earlier versions keep their own scoring
//!   rules.
//!
//! The canonical forecast (default lookback, the live ceiling, no arrivals)
//! is stored at most once per workspace per [`persistence::STORE_INTERVAL_MINUTES`], and
//! the owner clock records it even without reads. A stored row carries the
//! whole response as a snapshot, which a canonical read serves while it is
//! fresh, calibration included; every live request scores the newest stored
//! forecasts plus the mature snapshots of each model version from 9 on.

mod dependencies;
mod model;
mod outcomes;
mod resource_fit;
mod review_modes;
mod rework;
mod schedule;
mod scoring;
mod simulation;

mod assembly;
mod calculate;
mod cohorts;
mod correction;
mod history;
mod options;
mod persistence;
mod projection;
mod runtime;
mod usage;

use assembly::arrival_source;
pub use options::BacklogForecastHistory;
use usage::{ForecastRun, forecast_usage};
