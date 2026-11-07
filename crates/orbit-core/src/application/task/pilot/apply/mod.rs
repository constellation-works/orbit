//! Task-isolated validation, idempotent recovery, and atomic application for task pilots.
//!
//! The step input is read once into the typed records of [`input`]. The
//! prepared envelope, the promotion authority and the partition layout are
//! all checked before any task is written; each partition then settles on its
//! own ([`partition`], one task at a time in [`task`]), and [`report`] rolls
//! the decisions up into the step output.

pub(super) mod admission;
mod assess;
mod batch;
mod footprint;
mod input;
mod invalid_finding;
mod partition;
mod report;
mod scope;
mod selection;
mod task;
mod unresolved;
mod write;

use std::collections::BTreeMap;
use std::path::Path;

use orbit_types::workflow::ActivityCrewDraw;
use orbit_types::workflow::automation::members::PreparationPolicy;
use serde_json::Value;

use crate::OrbitRuntime;

use super::source::SourceSnapshot;
use admission::PromotionAuthority;
pub(crate) use batch::apply;

/// A task outcome that needs no further apply work: applied, already applied,
/// or superseded. Partition decisions and the run report share this test.
fn settled(outcome: &Value) -> bool {
    matches!(
        outcome["outcome"].as_str(),
        Some("applied" | "already_applied" | "superseded" | ESCALATED)
    )
}

/// The outcome of a task whose valid assessment a lower-effort `@auto`
/// pilot produced and that the repair round re-pilots at a higher effort
/// ([`super::auto_effort`]). Nothing is written for it here; the repair
/// apply settles it.
pub(super) const ESCALATED: &str = "escalated";

/// Error text of an unsettled task, falling back to its detail.
fn detail(outcome: &Value) -> Option<&str> {
    outcome
        .get("error")
        .and_then(Value::as_str)
        .or_else(|| outcome.get("detail").and_then(Value::as_str))
}

/// What every task of one apply is checked and written against.
struct ApplyContext<'a> {
    runtime: &'a OrbitRuntime,
    action: &'a str,
    workspace_root: &'a Path,
    /// The prepared payload verbatim, for the receipt and write boundaries.
    prepared: &'a Value,
    source: Option<&'a SourceSnapshot>,
    /// The pinned revision's tree id, recomputed host-side: the value a
    /// result's `observed_tree` must match. `None` for a non-Git workspace,
    /// which skips the inspection proof.
    source_tree: Option<&'a str>,
    policy: &'a PreparationPolicy,
    authority: &'a PromotionAuthority<'a>,
    /// Tasks whose material the branch changed since preparation.
    source_superseded: &'a BTreeMap<String, String>,
    /// The pilot job run applying these results, recorded on each receipt so
    /// the assessment can be attributed to the run's crew and model.
    run_id: Option<&'a str>,
    /// That run's crew, recorded in an explore brief's provenance.
    pilot_crew: Option<&'a str>,
    /// The round this apply settles: the first pilots, or the repair round.
    round: &'a str,
    /// Each partition's resolved crew when the run drew a task-pilot
    /// `<crew>@auto` member, by partition index.
    partition_draws: &'a BTreeMap<u64, ActivityCrewDraw>,
}

impl ApplyContext<'_> {
    /// The crew partition `index`'s pilot ran as: its own resolved draw, else
    /// the run's.
    fn partition_crew(&self, index: u64) -> Option<&str> {
        self.partition_draws
            .get(&index)
            .map(|draw| draw.crew.as_str())
            .or(self.pilot_crew)
    }
}

/// What the applied tasks of every partition contribute to the report.
#[derive(Default)]
struct Applied {
    task_results: Vec<Value>,
    ci_sweep_admission: Vec<Value>,
    drain_approval: Vec<Value>,
    resulting_fingerprints: BTreeMap<String, String>,
}
