//! The role registry: each role's unit of work, its terms and weights, and
//! the function that builds its rows from the scan. A role whose track has
//! not landed registers `rows: None` and is answered `not_computed` with its
//! terms, so adding it later changes only its own file.

mod implementers;
pub(super) mod pilot_warnings;
pub(super) mod pilots;
mod reviewers;

use super::score::{RowInput, Term};
use super::{AgentFacts, AgentRole, ExecutionScope, WindowBounds};

/// Builds a role's rows for the window, requested minimum support and
/// execution scope. Only implementers read the scope: the other roles score
/// review, authorship and assessment, not how a task was executed.
pub(super) type Rows =
    for<'a> fn(&'a AgentFacts, WindowBounds, usize, ExecutionScope) -> Vec<RowInput<'a>>;

/// Role-level fields a role adds to every response header for the window.
pub(super) type Header =
    fn(&AgentFacts, WindowBounds) -> serde_json::Map<String, serde_json::Value>;

/// One role's scorecard definition.
pub(super) struct RoleSpec {
    /// The label of the role's unit of work.
    pub(super) unit: &'static str,
    pub(super) terms: &'static [Term],
    /// Terms rated per 100 of their own unit rather than the role's, with
    /// that unit's label.
    pub(super) term_units: &'static [(&'static str, &'static str)],
    /// `None` while the role's terms are not computed yet.
    pub(super) rows: Option<Rows>,
    /// `None` for a role that adds nothing to the header.
    pub(super) header: Option<Header>,
}

/// The implementer `(crew, model, tier, type)` cell matrix.
/// One row per implemented task, for the offline type-discovery report.
pub(super) fn implementer_per_task_rows(
    facts: &AgentFacts,
    window: WindowBounds,
    types: &[orbit_config::TaskTypeDefinition],
    execution: ExecutionScope,
) -> Vec<serde_json::Value> {
    implementers::per_task_rows(facts, window, types, execution)
}

pub(super) fn implementer_by_type_cells(
    facts: &AgentFacts,
    window: WindowBounds,
    types: &[orbit_config::TaskTypeDefinition],
    execution: ExecutionScope,
) -> Vec<serde_json::Value> {
    implementers::by_type_cells(facts, window, types, execution)
}

/// The implementer regression rate by landed change size, per crew
/// reference, optionally narrowed to one tier.
pub(super) fn implementer_size_curves(
    facts: &AgentFacts,
    window: WindowBounds,
    complexity: Option<&str>,
    execution: ExecutionScope,
) -> serde_json::Map<String, serde_json::Value> {
    implementers::size_curves(facts, window, complexity, execution)
}

/// Each implementer row's tasks with the scorecard's per-task counts and
/// expectations, for the model ranking.
pub(super) use implementers::{LedgerRow, LedgerTask, implementer_ledger};

/// The tiers [`implementer_size_curves`] can narrow to.
pub(super) const SIZE_CURVE_TIERS: &[&str] = implementers::SIZE_CURVE_TIERS;

pub(super) fn spec(role: AgentRole) -> &'static RoleSpec {
    match role {
        AgentRole::Implementers => &implementers::ROLE,
        AgentRole::Reviewers => &reviewers::ROLE,
        AgentRole::Pilots => &pilots::ROLE,
    }
}
