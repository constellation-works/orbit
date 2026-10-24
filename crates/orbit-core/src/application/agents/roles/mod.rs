//! The role registry: each role's unit of work, its terms and weights, and
//! the function that builds its rows from the scan. A role whose track has
//! not landed registers `rows: None` and is answered `not_computed` with its
//! terms, so adding it later changes only its own file.

mod implementers;
mod orchestrators;
pub(super) mod pilots;
mod reviewers;

use chrono::{DateTime, Utc};

use super::score::{RowInput, Term};
use super::{AgentFacts, AgentRole};

/// Builds a role's rows from the scan for the window starting at `since`.
pub(super) type Rows = for<'a> fn(&'a AgentFacts, DateTime<Utc>) -> Vec<RowInput<'a>>;

/// One role's scorecard definition.
pub(super) struct RoleSpec {
    /// The label of the role's unit of work.
    pub(super) unit: &'static str,
    pub(super) terms: &'static [Term],
    /// `None` while the role's terms are not computed yet.
    pub(super) rows: Option<Rows>,
}

/// The implementer `(crew, model, tier, type)` cell matrix.
pub(super) fn implementer_by_type_cells(
    facts: &AgentFacts,
    since: DateTime<Utc>,
    types: &[orbit_config::CrewPerformanceType],
) -> Vec<serde_json::Value> {
    implementers::by_type_cells(facts, since, types)
}

pub(super) fn spec(role: AgentRole) -> &'static RoleSpec {
    match role {
        AgentRole::Orchestrators => &orchestrators::ROLE,
        AgentRole::Implementers => &implementers::ROLE,
        AgentRole::Reviewers => &reviewers::ROLE,
        AgentRole::Pilots => &pilots::ROLE,
    }
}
