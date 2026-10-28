use orbit_store::contracts::{AdmissionReceipt, ExecutionClaimPhase};
use serde::Serialize;

/// What `orbit.task.pull` answers: the immutable receipt and, separately, the
/// claim's phase right now. A replayed receipt is historical evidence; the
/// current phase is what says whether the attempt may still execute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskPullResponse {
    pub receipt: AdmissionReceipt,
    pub claim_state: Option<ExecutionClaimPhase>,
}
