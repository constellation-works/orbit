use orbit_common::OrbitError;
use serde_json::Value;

use super::pr::{pr_create, pr_list, pr_merge, pr_merge_capabilities, pr_status, pr_view};
use super::push_retry::{push, push_candidate_ref};
use super::{
    CANDIDATE_REF_PUSH, PR_CREATE, PR_LIST, PR_MERGE, PR_MERGE_CAPABILITIES, PR_STATUS, PR_VIEW,
    PUSH,
};

/// Execute the VCS operations owned by deterministic shipment automation.
///
/// This boundary is deliberately separate from `ToolRegistry`: the operation
/// labels are engine-private, are never advertised to agents, and do not pass
/// through public tool authorization or activity allowlists.
pub(crate) fn run(operation: &str, input: &Value) -> Result<Value, OrbitError> {
    match operation {
        PUSH => push(input),
        CANDIDATE_REF_PUSH => push_candidate_ref(input),
        PR_LIST => pr_list(input),
        PR_CREATE => pr_create(input),
        PR_VIEW => pr_view(input),
        PR_MERGE => pr_merge(input),
        PR_MERGE_CAPABILITIES => pr_merge_capabilities(input),
        PR_STATUS => pr_status(input),
        other => Err(OrbitError::InvalidInput(format!(
            "unknown private automation VCS operation '{other}'"
        ))),
    }
}
