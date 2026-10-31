//! The six path rankings of the codebase view, computed from scan facts for
//! one window, minimum support and rollup, and the paths several of them
//! share.
//!
//! Every ranking is a rate over distinct tasks (or requesting identities for
//! contention, reservations for coupling) with its numerator and denominator,
//! so a popular file does not lead merely because many tasks touch it. Rows whose denominator is below
//! the minimum support are omitted and counted in `below_support`.
//!
//! The Regressions ranking counts a regressed task against the paths its
//! `regression_from` links record as `regression.sites`, not against its
//! whole footprint; links without sites are reported as `unsited`.
//!
//! Each ranking's rows come from one builder that also keeps what stands
//! behind every numerator, so the per-path detail reads the counts the
//! rankings rank.

use super::facts::{
    ACTOR_KINDS, Anchor, Block, BlockCause, CodebaseFacts, INFRA_CAUSES, ReservationFacts,
    TaskFacts,
};
use super::lock_waits::{WaitEpisode, wait_episodes, wait_summary};
use super::regression::{RegressionFilter, RegressionLink};
use chrono::{DateTime, Utc};
use orbit_common::fs::selector::Selector;
use orbit_types::task::TaskStatus;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

mod blocked;
mod coupling;
mod footprint;
mod locks;
mod regressions;
mod rejected;
mod rollup;

pub(in crate::application::codebase) use blocked::{blocked_rows, rank_blocked};
pub(in crate::application::codebase) use coupling::coupling_pairs;
pub(in crate::application::codebase) use footprint::footprint_rows;
pub(in crate::application::codebase) use locks::{lock_rows, lock_tally};
pub(in crate::application::codebase) use regressions::{
    footprint_regression_rows, regressed_fields, regression_rows, site_entries,
};
pub(in crate::application::codebase) use rejected::rejected_rows;
pub(in crate::application::codebase) use rollup::{
    LARGE_FOOTPRINT_CAP, Params, ROW_LIMIT, Rollup, Tally, all_sections, merge, rank, rate_fields,
};
