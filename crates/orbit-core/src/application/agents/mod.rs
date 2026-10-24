//! Agent scorecards: one scorecard per agent role (orchestrators,
//! implementers, reviewers, pilots), ranked by crew and model, where every
//! problem counts against exactly one role or is Not scored.
//!
//! [`OrbitRuntime::agent_scan`](crate::OrbitRuntime::agent_scan) reads the
//! workspace once ([`AgentFacts`]):
//! tasks with activity in the last 90 days, the implementation job runs that
//! name them, the review gate's settlements, the verdicts on review-filed
//! findings and the typed regressions, and gives each problem its one owner
//! ([`Charge`]). [`agent_scorecard`],
//! [`agent_detail`] and [`agent_attribution`] only slice those facts by
//! role, window and minimum support, so a caller that keeps the facts (the
//! dashboard) answers another selection without rescanning.
//!
//! Every role shares one score: each term's count per 100 units of the
//! role's work (or of the term's own denominator, as the pilots' warning
//! terms are rated per 100 settled assessments), weighted and summed, then,
//! above the fleet average, pulled down toward it with K = 30. Cost is never
//! a term.

use orbit_common::OrbitError;

mod api;
mod attribution;
mod candidates;
mod facts;
mod performance;
mod query;
mod records;
mod roles;
mod score;
mod shadow;

pub use attribution::{Charge, Credit, Owner};
pub use facts::{AgentFacts, Problem};

/// The model of a row whose runs, or for orchestrators whose tasks, recorded
/// none.
pub(crate) const MODEL_UNKNOWN: &str = "model unknown";
/// The crew of a row whose attempt no job run records.
pub(crate) const CREW_UNKNOWN: &str = "crew unknown";

const DEFAULT_WINDOW: &str = "30d";
const DEFAULT_MIN_SUPPORT: usize = 10;
/// The selectable windows: requested name, the window served and its hours.
/// `all` is served as 90 days, the scan's reach.
const WINDOWS: &[(&str, &str, i64)] = &[
    ("1h", "1h", 1),
    ("24h", "24h", 24),
    ("7d", "7d", 7 * 24),
    ("30d", "30d", 30 * 24),
    ("90d", "90d", facts::SCAN_DAYS * 24),
    ("all", "90d", facts::SCAN_DAYS * 24),
];

/// An agent role, each with its own scorecard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AgentRole {
    Orchestrators,
    Implementers,
    Reviewers,
    Pilots,
}

impl AgentRole {
    pub const ALL: [AgentRole; 4] = [
        AgentRole::Orchestrators,
        AgentRole::Implementers,
        AgentRole::Reviewers,
        AgentRole::Pilots,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AgentRole::Orchestrators => "orchestrators",
            AgentRole::Implementers => "implementers",
            AgentRole::Reviewers => "reviewers",
            AgentRole::Pilots => "pilots",
        }
    }

    /// A role by its wire name.
    pub fn parse(name: &str) -> Result<Self, OrbitError> {
        Self::ALL
            .into_iter()
            .find(|role| role.as_str() == name.trim())
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "role must be one of orchestrators, implementers, reviewers or pilots; got '{name}'"
                ))
            })
    }
}

/// A validated selection: the window and the units a row needs before it is
/// ranked.
#[derive(Debug, Clone, Copy)]
pub struct AgentQuery {
    requested: &'static str,
    window: &'static str,
    hours: i64,
    min_support: usize,
}

pub use api::{agent_attribution, agent_by_type_scorecard, agent_detail, agent_scorecard};
