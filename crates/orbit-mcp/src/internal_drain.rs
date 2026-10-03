//! Deterministic owner/follower RPC, enabled only by the server launch path.

/// Internal transport revision, independent of public tool discovery.
pub const INTERNAL_DRAIN_PROTOCOL: u64 = 1;
pub(crate) const CALL_METHOD: &str = "orbit/internal/drain/call";
pub(crate) const PREFLIGHT_METHOD: &str = "orbit/internal/drain/preflight";

/// Resolve only the five protocol operations, including their retired wire names.
/// This does not grant access: public requests use it to refuse both spellings.
pub fn internal_drain_name(name: &str) -> Option<&'static str> {
    match name {
        "orbit.drain.probe" | "orbit_drain_probe" => Some("orbit.drain.probe"),
        "orbit.drain.receipt.lookup" | "orbit_drain_receipt_lookup" => {
            Some("orbit.drain.receipt.lookup")
        }
        "orbit.drain.claim.bind" | "orbit_drain_claim_bind" => Some("orbit.drain.claim.bind"),
        "orbit.drain.claim.settle" | "orbit_drain_claim_settle" => Some("orbit.drain.claim.settle"),
        "orbit.task.pull" | "orbit_task_pull" => Some("orbit.task.pull"),
        _ => None,
    }
}

pub(crate) fn refusal() -> orbit_common::OrbitError {
    orbit_common::OrbitError::PolicyDenied(
        "distributed drain requires the internal runtime route".into(),
    )
}
