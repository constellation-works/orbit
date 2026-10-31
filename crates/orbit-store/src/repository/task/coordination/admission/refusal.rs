//! Pre-admission refusal verdict, shared by admission and the read-only preflight probe.

use crate::contracts::*;

/// The ordered pre-admission ladder, shared by admission and the read-only
/// preflight probe.
///
/// Selector resolution, session capability, and trusted invocation context are
/// decided by the calling surface before this function is reached; identity
/// here is already trusted. Order is the spec's: input shape, then
/// version/schema, then ship mode, then before-PR review. Evaluating it returns a
/// verdict and writes nothing, so a probe and an admission cannot disagree
/// about what would be refused.
pub fn admission_refusal(
    identity: &AdmissionIdentity,
    request: &AdmissionRequest,
    owner_version: &str,
) -> Option<AdmissionRefusal> {
    if request
        .caller_fingerprint
        .as_deref()
        .is_some_and(|caller| caller != crate::contracts::distributed_drain_protocol_fingerprint())
    {
        return Some(AdmissionRefusal::ProtocolSkew);
    }
    if [
        &identity.location().machine_id,
        &request.request_id,
        &request.caller_version,
        &request.run_context.run_id,
        &request.run_context.job_name,
    ]
    .iter()
    .any(|v| v.trim().is_empty())
        || request.crews_malformed()
    {
        return Some(AdmissionRefusal::InvalidInput);
    }
    if request.caller_schema == 0
        || request.ship.base_branch.trim().is_empty()
        || request.ship.landing_branch.trim().is_empty()
        || !matches!(request.ship.completion.as_str(), "review" | "done")
        || (request.ship.completion == "done"
            && request
                .ship
                .authorization_reference
                .as_deref()
                .is_none_or(|r| r.trim().is_empty()))
        || !request.ship.review_contract_consistent()
    {
        return Some(AdmissionRefusal::InvalidInput);
    }
    if request.caller_schema != DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA {
        return Some(AdmissionRefusal::ProtocolMismatch);
    }
    if request.caller_version != owner_version {
        return Some(AdmissionRefusal::VersionMismatch);
    }
    if !matches!(request.ship.mode.as_str(), "pr" | "local")
        || (identity.is_remote() && request.ship.mode == "local")
    {
        return Some(AdmissionRefusal::ShipModeUnsupported);
    }
    // An owner that captured a `review.timing` of `before_pr` or `before_landing`
    // admits only a leaf that runs the gate its ship contract carries, so
    // nothing is delivered unreviewed [ORB-13908] [ORB-14849]. The claim's captured contract decides: the executor's own
    // switch never gates a claimed leaf. Only the PR route runs the gate.
    // After-landing review is owner-side and never refuses.
    if (request.ship.before_pr || request.ship.before_landing)
        && !(request.review_gate && request.ship.mode == "pr")
    {
        return Some(AdmissionRefusal::BeforePrUnsupported);
    }
    // An active selective policy is refused to an executor that cannot
    // evaluate it on the PR route: dropping it would deliver unreviewed.
    if request.ship.selective_policy().is_some()
        && !(request.selective_review && request.review_gate && request.ship.mode == "pr")
    {
        return Some(AdmissionRefusal::SelectiveUnsupported);
    }
    None
}
