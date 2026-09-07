//! Independent review policy composed over the existing PR pipeline
//! [ORB-11333].
//!
//! Three concerns stay separate here, as they do for operation mode:
//!
//! - **Admission** captures the effective review policy once, in the run's
//!   immutable input, so a later preference edit cannot weaken a gate that
//!   is already active. Children inherit their parent's snapshot.
//! - **The gate** runs as two deterministic Core actions around a fresh
//!   reviewer invocation: `review_gate_admit` reserves a reviewer start,
//!   pins the candidate and hands the reviewer an immutable manifest;
//!   `review_gate_settle` turns the reviewer's report plus the repository
//!   state into an honest verdict, commits reviewer repairs under the
//!   reviewer's identity, and issues the certificate.
//! - **Coverage** is decided by `orbit-automation`'s shared rules over facts
//!   Core gathers: managed landings are classified after completion, and
//!   delivery observation feeds proven exclusions to the shared evaluator.
//!
//! Store persists ledgers, certificates and landings; Engine owns the Git
//! mechanics. Nothing here approves a task, merges, or reads a verdict from
//! a tag or a timestamp.

use orbit_common::OrbitError;

mod admission;
mod coverage;
mod gate;
mod landing;
mod projection;

#[cfg(test)]
mod tests;

pub(crate) use admission::install_review_admission;
pub(crate) use coverage::exclusions;
pub(crate) use gate::{review_gate_admit, review_gate_settle};
pub(crate) use landing::record_review_landing;
pub use projection::task_review_projection;

/// Audit command name shared by every gate decision.
pub(crate) const REVIEW_AUDIT: &str = "review.gate";

/// The jobs that carry a review admission: the delivery family, so a leaf
/// PR pipeline can inherit the policy its coordinator captured.
pub(crate) const REVIEW_ADMITTED_JOBS: &[&str] = &[
    "workspace_auto_pipeline",
    "task_auto_pipeline",
    "task_gate_pipeline",
    "task_pr_pipeline",
    "task_local_pipeline",
    "epic_pipeline",
];

/// The job that delivers locally and therefore cannot honour `before-pr`
/// as a final route. A parent-authorized [`EPIC_JOB`] child may still use
/// it to assemble onto the epic branch; the epic's own gate remains the
/// before-pr checkpoint.
pub(crate) const LOCAL_ROUTE_JOB: &str = "task_local_pipeline";

/// The job that assembles descendants locally and then reviews the combined
/// PR-bound candidate. Its child `task_local_pipeline` submissions are
/// intermediate landing, not local-only final delivery.
pub(crate) const EPIC_JOB: &str = "epic_pipeline";

/// One candidate lineage: the task set delivered together against a base.
pub(crate) fn lineage_key(workspace_id: &str, task_ids: &[String], base: &str) -> String {
    let mut ids = task_ids.to_vec();
    ids.sort();
    ids.dedup();
    format!("{workspace_id}/{}/{base}", ids.join("+"))
}

/// Translate a shared-rule failure into the Core error vocabulary.
pub(crate) fn automation_error(error: orbit_automation::AutomationError) -> OrbitError {
    orbit_automation::automation_error_to_orbit(error)
}
