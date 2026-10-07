//! Independent review policy composed over the existing PR pipeline
//! [ORB-11333].
//!
//! Three concerns stay separate here, as they do for operation mode:
//!
//! - **Admission** captures the effective review policy once, in the run's
//!   immutable input, so a later preference edit cannot weaken a gate that
//!   is already active. Children inherit their parent's snapshot.
//! - **The gate** runs as two deterministic Core actions around a fresh
//!   reviewer invocation: `review_gate_admit` reserves the candidate's review,
//!   pins the candidate and hands the reviewer an immutable manifest;
//!   `review_gate_settle` turns the reviewer's report plus the repository
//!   state into an honest verdict, commits reviewer repairs under the
//!   reviewer's identity, and issues the certificate.
//! - **Coverage** is decided by `orbit-automation`'s shared rules over facts
//!   Core gathers: managed landings are classified after completion, and
//!   delivery observation feeds proven exclusions to the shared evaluator.
//!
//! Store persists ledgers, certificates and landings; Engine owns the Git
//! mechanics. The internal handoff seam records explicit operator completion
//! approval; it never merges or reads a verdict from a tag or timestamp.

use orbit_common::OrbitError;

mod admission;
mod coverage;
pub(crate) mod evidence;
mod fulfilment;
mod gate;
mod handoff;
mod landing;
mod projection;
pub(crate) mod reconciliation;
mod switches;

pub(crate) use admission::{
    install_review_admission, local_route_before_pr_conflict, run_review_admission,
    upgrade_resume_admission_mismatch,
};
pub(crate) use coverage::exclusions;
pub(crate) use fulfilment::fulfil_review_evidence;
pub use fulfilment::{
    EVIDENCE_FULFILMENT_AUDIT, EvidenceFulfilmentTick, REVIEW_EVIDENCE_FULFILMENT_JOB,
};
pub(crate) use gate::{
    record_reviewer_invocation, release_review_attempt, review_gate_admit, review_gate_settle,
};
/// The owner handoff console [ORB-12516]: what an authorized owner surface
/// reads and the typed refusals it renders. Adapters above Core cannot reach
/// `orbit-store`, so these are the only shapes they need.
pub use handoff::{
    DistributedClaimState, ExpectedCandidate, HANDOFF_CONSOLE_SCHEMA, HandoffConsoleRefusal,
};
pub(crate) use landing::record_review_landing;
pub use projection::task_review_projection;
pub use switches::{
    AfterLandingSwitch, BeforePrSwitch, ReviewSwitches, review_switches, review_switches_view,
};

/// Audit command name shared by every gate decision.
pub(crate) const REVIEW_AUDIT: &str = "review.gate";

/// The jobs whose submission captures a review admission: the delivery
/// family, so a leaf PR pipeline can inherit the `review.before_pr` its
/// coordinator captured, and the follower's pull drain, which declares its
/// captured value on every pull [ORB-13992].
///
/// A claimed leaf (`CLAIMED_LEAF_JOBS`) is never submitted: the pull store
/// creates it with the admission its claim's ship contract captured, so this
/// host's settings never decide its review, and a submission naming the
/// reserved key for one is refused like any other [ORB-13908].
pub(crate) const REVIEW_ADMITTED_JOBS: &[&str] = &[
    "workspace_auto_pipeline",
    "workspace_pull_pipeline",
    "task_auto_pipeline",
    "task_gate_pipeline",
    "task_pr_pipeline",
    "task_local_pipeline",
];

/// The job that delivers locally and therefore cannot honour `review.before_pr`
/// as a final route.
pub(crate) const LOCAL_ROUTE_JOB: &str = "task_local_pipeline";

/// One candidate lineage: the task set one delivery run lineage delivers
/// together against a base. `root_run_id` is the first run of the
/// lineage — a resumed run shares its source's budget, while a fresh
/// delivery run of the same tasks starts a new lineage with a full budget.
pub(crate) fn lineage_key(
    workspace_id: &str,
    task_ids: &[String],
    base: &str,
    root_run_id: &str,
) -> String {
    let mut ids = task_ids.to_vec();
    ids.sort();
    ids.dedup();
    format!("{workspace_id}/{}/{base}/{root_run_id}", ids.join("+"))
}

/// Translate a shared-rule failure into the Core error vocabulary.
pub(crate) fn automation_error(error: orbit_automation::AutomationError) -> OrbitError {
    orbit_automation::automation_error_to_orbit(error)
}

/// Record an operator decision resetting one explicitly selected review lineage.
/// The tool chokepoint supplies operator authorization; managed leaves are
/// refused here as well, including an accidentally elevated run.
pub(crate) fn reset_review(
    runtime: &crate::OrbitRuntime,
    input: &serde_json::Value,
) -> Result<serde_json::Value, OrbitError> {
    use orbit_common::protocol::tool_input::required_string;
    use orbit_store::contracts::ReviewResetRequest;
    if orbit_common::governance::authorization::agent_context_declared() {
        return Err(OrbitError::CapabilityDenied(
            "managed agents cannot reset review budgets".into(),
        ));
    }
    let id = required_string(input, &["id"], "id")?;
    let lineage = required_string(input, &["lineage_key"], "lineage_key")?;
    let reason = required_string(input, &["reason"], "reason")?;
    runtime.get_task(&id)?;
    runtime.ensure_coordination_task_write_permitted()?;
    let actor = runtime.actor().resolve_write_label(None, None)?;
    let adopt = input
        .get("adopt_configured_budget")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let ledger = runtime.review_store()?.review_reset(
        &runtime.workspace_id()?,
        &ReviewResetRequest {
            lineage_key: &lineage,
            task_id: &id,
            reason: &reason,
            actor: &actor,
            budget: adopt.then(|| runtime.operation_policy().review_budget()),
            now: chrono::Utc::now(),
        },
    )?;
    // The decision and its prior consumption are atomic in the ledger. The
    // normal tool-dispatch audit additionally records caller/session provenance.
    Ok(serde_json::json!({"id": id, "ledger": ledger, "reset": true}))
}
