//! Compatibility types for stored trusted-host admissions.
//!
//! Host invocation has been removed. Old run inputs still deserialize, and
//! ordinary submissions and replay retain their reserved-key protections.
//! No activity may declare trusted-host execution and no stored admission
//! authorizes a new process.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Reserved run-input key carrying an operator's trusted-host admission.
///
/// Reserved means reserved: every ordinary submission path refuses input that
/// contains it, so the key can only ever have been written by the canonical
/// submission.
pub const TRUSTED_HOST_ADMISSION_KEY: &str = "trusted_host_admission";

/// One operator's admission of one trusted-host invocation.
///
/// Recorded rather than reduced to a boolean because the durable run record is
/// the only place that can later answer "who authorized an unsandboxed process,
/// against which checkout, from where".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedHostAdmission {
    /// Attribution label of the operator who authorized the invocation.
    pub authorized_by: String,
    /// How the authorization chokepoint resolved that operator
    /// (`interactive-terminal`, `operator-override`, `session`).
    pub authorizer_provenance: String,
    /// Caller label an SSH-originated MCP session forwarded, when this
    /// admission arrived over SSH rather than from a local operator surface.
    ///
    /// Attribution only: an SSH login to this machine is ownership of it, so
    /// the label names who reached in without being an authenticated
    /// principal [ORB-12564].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_machine_id: Option<String>,
    /// RFC 3339 timestamp of the admission.
    pub authorized_at: String,
    /// Canonical workspace checkout the invocation was admitted against.
    pub workspace_path: String,
    /// Canonical working directory the provider subprocess starts in.
    pub cwd: String,
}

impl TrustedHostAdmission {
    /// Read an admission out of a run input, if one is present and well-formed.
    ///
    /// A malformed value reads as absent so the engine fails closed on it the
    /// same way it fails closed on a missing one.
    pub fn from_run_input(input: &Value) -> Option<Self> {
        serde_json::from_value(input.get(TRUSTED_HOST_ADMISSION_KEY)?.clone()).ok()
    }
}

/// Whether `input` carries the reserved admission key at all, well-formed or not.
///
/// Submission guards ask this rather than [`TrustedHostAdmission::from_run_input`]:
/// a caller who supplies a *malformed* admission is still forging one, and must
/// be refused rather than quietly stripped.
pub fn run_input_declares_trusted_host(input: &Value) -> bool {
    input
        .as_object()
        .is_some_and(|object| object.contains_key(TRUSTED_HOST_ADMISSION_KEY))
}

/// Remove the reserved admission key from a run input, returning whether one
/// was present.
///
/// Used by replay, which re-runs a historical input under no new admission.
pub fn strip_trusted_host_admission(input: &mut Value) -> bool {
    input
        .as_object_mut()
        .is_some_and(|object| object.remove(TRUSTED_HOST_ADMISSION_KEY).is_some())
}

/// Refusal to load an asset that claims trusted-host execution it may not have.
///
/// The refusal names the offending activity. An activity's `metadata.name`
/// identifies a workspace asset file, not secret material, and the operator
/// reading this error is the person who has to find and edit that file — a
/// refusal that will not say which asset it refused is not actionable.
/// Redaction in this workspace covers credentials and user-identifying paths
/// (`orbit_common::security::redaction`), and the sibling
/// `AssetLoadError::ToolAllowlist` names the same value on the same grounds.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("activity `{activity}` declares removed `trustedHostExecution: true`; use a task workflow")]
pub struct TrustedHostActivityError {
    /// Name of the offending activity asset.
    pub activity: String,
}

/// Reject the removed `trustedHostExecution` mode on every activity.
///
/// Called from asset load, so a hand-written or edited asset is refused before
/// it can reach a dispatcher.
pub fn validate_trusted_host_activity(
    activity: &str,
    declares_trusted_host: bool,
) -> Result<(), TrustedHostActivityError> {
    if declares_trusted_host {
        return Err(TrustedHostActivityError {
            activity: activity.to_string(),
        });
    }
    Ok(())
}
