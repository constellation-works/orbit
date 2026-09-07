//! Operation mode: scoped automation authority composed over existing
//! pipelines [ORB-11332].
//!
//! Three things are deliberately kept apart here:
//!
//! - **Preferences** (`[operation]` in `config.toml`) are resolved by
//!   `orbit-config` with per-field provenance. They describe desired
//!   defaults and authorize nothing.
//! - **Authority** is a durable [`OperationGrant`]: exact workspace, finite
//!   task set, separate prepare/promote/complete rights, absolute expiry,
//!   captured limits, and the versioned effective policy. Store persists it;
//!   this module decides when to consult it and records every decision.
//! - **Enforcement** happens at the boundaries that already own the action:
//!   the drain classifier and window, the Store child-admission transaction,
//!   the engine's recovery hook, and the guarded `review -> done` transition.
//!   Nothing here is a second scheduler; scheduling stays with
//!   `orbit-automation`, which only receives resolved constraints.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_config::OperationPolicy;
use orbit_types::workflow::OperationGrant;

use crate::OrbitRuntime;

mod admission;
mod constraints;
mod explain;
mod grant;
mod promotion;
mod recovery;

#[cfg(test)]
mod tests;

pub use admission::{OperationDrainRequest, OperationDrainResult};
pub(crate) use admission::{
    admission_state, child_admission_authority, inherit_child_admission, live_admission,
    reserved_operation_key_error,
};
pub(crate) use constraints::member_constraints;
pub use grant::{
    EnableOperationGrantRequest, OperationGrantControlRequest, OperationGrantControlResult,
};
pub(crate) use promotion::promote_within_grant;
pub(crate) use recovery::{settle_triage_episode, triage_recovery_reservation};

/// Governed operation ids. Placement of the derived MCP tools is decided in
/// `orbit-common`'s authorization registry; these strings are the claim
/// operation names the workspace-claim check records.
pub(crate) const ENABLE_OPERATION: &str = "orbit.operation.enable";
pub(crate) const STOP_OPERATION: &str = "orbit.operation.stop";
pub(crate) const REVOKE_OPERATION: &str = "orbit.operation.revoke";

/// Audit command names shared by every grant decision.
pub(crate) const GRANT_AUDIT: &str = "operation.grant";
pub(crate) const PROMOTION_AUDIT: &str = "operation.promotion";
pub(crate) const RECOVERY_AUDIT: &str = "operation.recovery";
pub(crate) const COMPLETION_AUDIT: &str = "operation.completion";

/// Reason vocabulary for a workspace with no usable grant.
pub(crate) const NO_GRANT_REASON: &str = "scoped_authorization_required";

impl OrbitRuntime {
    /// The workspace's active, unexpired grant, if any.
    pub fn active_operation_grant(&self) -> Result<Option<OperationGrant>, OrbitError> {
        let workspace_id = self.workspace_id()?;
        self.operation_store()?
            .operation_active_grant(&workspace_id, Utc::now())
    }

    /// One grant by id, in this workspace.
    pub fn operation_grant(&self, grant_id: &str) -> Result<OperationGrant, OrbitError> {
        let workspace_id = self.workspace_id()?;
        self.operation_store()?
            .operation_grant(&workspace_id, grant_id)?
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "operation grant '{grant_id}' was not found in this workspace"
                ))
            })
    }

    /// Recent grants for this workspace, newest first.
    pub fn list_operation_grants(&self, limit: usize) -> Result<Vec<OperationGrant>, OrbitError> {
        let workspace_id = self.workspace_id()?;
        self.operation_store()?
            .operation_grants(&workspace_id, limit.max(1))
    }
}

/// The policy a grant captured at enablement. A grant whose snapshot cannot
/// be read is not reinterpreted from current configuration: privileged
/// actions fail closed on it.
pub(crate) fn captured_policy(grant: &OperationGrant) -> Result<OperationPolicy, OrbitError> {
    if grant.policy_version != orbit_config::OPERATION_POLICY_VERSION {
        return Err(OrbitError::InvalidInput(format!(
            "operation grant '{}' captured policy version {} but this binary understands {}; \
             stop or revoke the grant and enable a replacement",
            grant.id,
            grant.policy_version,
            orbit_config::OPERATION_POLICY_VERSION
        )));
    }
    serde_json::from_value(grant.policy.clone()).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "operation grant '{}' carries an unreadable policy snapshot: {error}",
            grant.id
        ))
    })
}
