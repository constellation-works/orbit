//! Deterministic support actions for `finding_verification_pipeline`.
//!
//! `prepare_finding_verification` pins one landing-branch revision, selects
//! the review-filed bug tasks to verify, draws each one's verifier crew from
//! the complexity pool of its tier, and partitions the tasks by crew.
//! `apply_finding_verification` validates every verifier verdict against that
//! revision, sends a REJECTED verdict from the low tier or any low-confidence
//! verdict to the next tier's pool once, and records the deciding verdict
//! through the sibling `finding_verification` module. The verifier agents are
//! read-only; only apply writes, and only the verdict. The engine dispatcher
//! (`adapter::engine_host::actions::dispatch`) only routes the actions here.
//! The design is
//! `docs/design/finding-verification/2_design.md`.

mod apply;
mod prepare;

pub(crate) use apply::apply;
pub(crate) use prepare::prepare;

use serde_json::Value;

/// The run a deterministic action executes for: the reservation owner the
/// dispatcher passes, else the run id the engine put in its input.
pub(crate) fn run_id<'a>(owner: Option<&'a str>, input: &'a Value) -> Option<&'a str> {
    owner
        .or_else(|| {
            ["job_run_id", "run_id"]
                .into_iter()
                .find_map(|key| input.get(key).and_then(Value::as_str))
        })
        .map(str::trim)
        .filter(|run_id| !run_id.is_empty())
}
