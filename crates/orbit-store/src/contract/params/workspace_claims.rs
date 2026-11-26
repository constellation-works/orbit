use serde::{Deserialize, Serialize};

use super::ExpiredTaskReservation;

/// The current holder of a workspace claim, as reported to a contender.
///
/// `machine_id` and `session_id` are diagnostics only. The claim is keyed on
/// [`WorkspaceClaimAcquireResult::claim_token`], never on session identity: MCP
/// session identity is minted per connection and cleared when client-supplied,
/// so a reconnecting client would orphan the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceClaimHolder {
    pub claim_id: String,
    pub workspace_id: Option<String>,
    pub actor: String,
    pub created_at: String,
    pub expires_at: String,
    pub machine_id: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WorkspaceClaimAcquireParams {
    pub workspace_orbit_dir: String,
    pub workspace_id: Option<String>,
    pub actor: String,
    pub ttl_seconds: u32,
    pub machine_id: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceClaimAcquireResult {
    pub acquired: bool,
    /// The minted bearer token, returned exactly once to the acquiring holder.
    /// Never populated on the contention path, so a refused contender cannot
    /// learn the incumbent's token from its own refusal.
    pub claim_token: Option<String>,
    pub claim: Option<WorkspaceClaimHolder>,
    /// The incumbent when acquisition was refused.
    pub conflict: Option<WorkspaceClaimHolder>,
    pub expired_claims: Vec<ExpiredTaskReservation>,
}

#[derive(Debug, Clone)]
pub struct WorkspaceClaimReleaseParams {
    pub workspace_orbit_dir: String,
    pub workspace_id: Option<String>,
    /// The holder's token. Required unless `force` is set.
    pub claim_token: Option<String>,
    /// Release the claim without its token — the audited escape hatch for a
    /// holder that has gone away.
    pub force: bool,
    pub released_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceClaimReleaseResult {
    pub released: bool,
    pub forced: bool,
    pub released_at: Option<String>,
    /// The claim that was released, or — when a token release was refused — the
    /// incumbent it did not match.
    pub claim: Option<WorkspaceClaimHolder>,
    pub expired_claims: Vec<ExpiredTaskReservation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceClaimStatusResult {
    pub claim: Option<WorkspaceClaimHolder>,
    pub expired_claims: Vec<ExpiredTaskReservation>,
}

#[derive(Debug, Clone)]
pub struct WorkspaceClaimCheckParams {
    pub workspace_orbit_dir: String,
    pub workspace_id: Option<String>,
    /// The token the caller presented, if any.
    pub claim_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceClaimCheckResult {
    /// The active claim after lazy expiry, or `None` when the workspace is
    /// unclaimed. An unclaimed workspace gates nothing.
    pub claim: Option<WorkspaceClaimHolder>,
    /// Whether the presented token matches the active claim. Always `false`
    /// when `claim` is `None`: there is no token to match, and the caller must
    /// read the absent claim rather than a truthy match.
    pub token_matches: bool,
    pub expired_claims: Vec<ExpiredTaskReservation>,
}
