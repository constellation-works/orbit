//! Admission wire fixtures shared by orbit-core's unit tests (`lib.rs`) and the
//! `runtime` integration binary, which cannot import the `cfg(test)` library
//! helpers. Each constructor deserializes only the required wire fields, so a
//! new optional field with a serde default needs no edit here or at a call site.
//! Callers set scenario values with struct update syntax.

use orbit_store::contracts::{
    AdmissionRequest, AdmissionReviewContract, AdmissionRunContext, AdmissionShipContract,
    DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
};
use orbit_types::workflow::{REVIEW_CONTRACT_VERSION, ReviewBudget};
use serde_json::json;

/// Supply required wire fields and let serde default unrelated capabilities.
/// Callers keep their review settings explicit with struct update syntax.
pub(crate) fn admission_request(
    request_id: String,
    run_context: AdmissionRunContext,
    ship: AdmissionShipContract,
) -> AdmissionRequest {
    serde_json::from_value(json!({
        "request_id": request_id,
        "caller_version": "fixture",
        "caller_schema": DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
        "run_context": run_context,
        "ship": ship,
    }))
    .expect("admission request fixture matches the wire contract")
}

/// A pull-request ship contract onto `main` with no review timing captured.
/// Scenarios set `before_pr`, `before_landing` and `review` explicitly.
// Only the runtime integration binary builds ship contracts this way.
#[allow(dead_code)]
pub(crate) fn admission_ship_contract() -> AdmissionShipContract {
    serde_json::from_value(json!({
        "mode": "pr",
        "base_branch": "main",
        "landing_branch": "main",
        "completion": "review",
    }))
    .expect("admission ship fixture matches the wire contract")
}

/// A review contract at the current evidence version and default budget.
/// Scenarios set the crew and the captured checks explicitly.
// Only the runtime integration binary builds review contracts this way.
#[allow(dead_code)]
pub(crate) fn admission_review_contract() -> AdmissionReviewContract {
    serde_json::from_value(json!({
        "contract_version": REVIEW_CONTRACT_VERSION,
        "budget": ReviewBudget::default(),
    }))
    .expect("admission review fixture matches the wire contract")
}
