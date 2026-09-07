//! The trusted-host execution contract [ORB-11354].
//!
//! Ordinary managed activities run their provider subprocess inside the
//! executor's sandbox. Exactly one activity does not: the operator-only
//! exploration invocation, which exists precisely to reach the host the way
//! the operator's own shell would. This module owns the three facts that mode
//! is made of, in the leaf crate both the engine and the application layer can
//! read:
//!
//! 1. [`TRUSTED_HOST_ACTIVITY`] — the one activity name allowed to declare it.
//! 2. [`TRUSTED_HOST_ADMISSION_KEY`] — the reserved run-input key that carries
//!    an operator's admission to the worker.
//! 3. [`TrustedHostAdmission`] — what that admission records.
//!
//! # This is a mode, not a privilege
//!
//! Trusted host execution removes Orbit's *filesystem sandbox* from one
//! provider subprocess. It does not hand the child any Orbit capability: the
//! child still runs with managed-run provenance, so it resolves as an agent at
//! every capability chokepoint and cannot perform a governed operation. That is
//! deliberate, and it is also not an isolation boundary — the subprocess runs
//! as the same OS user as Orbit and can read and write anything that user can.
//! The honest summary is in
//! `crates/orbit-core/assets/skills/orbit/references/tool-surface.md`.
//!
//! # Why the flag alone is not enough
//!
//! Activity assets live in a workspace directory an operator can edit, so a
//! YAML key can never be the admission by itself. The engine requires the flag
//! *and* an admission stamped into the run input by the canonical governed
//! submission, and the flag is legal only on [`TRUSTED_HOST_ACTIVITY`]. A
//! definition that declares the flag without an admission fails closed rather
//! than degrading to a sandboxed run, so a broken admission path is loud.

use crate::tool::{CallerIdentityProof, RemoteAgentInvokeMode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The one activity name permitted to declare `trustedHostExecution: true`.
pub const TRUSTED_HOST_ACTIVITY: &str = "agent_invoke";

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
    /// Destination-resolved remote caller identity, when this admission came
    /// through SSH MCP rather than a local operator surface. The separate
    /// proof and mode fields state whether that identity was authenticated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_machine_id: Option<String>,
    /// Strength of the remote identity proof. It is retained independently of
    /// the operation's trust mode so cooperative/self-asserted and key-bound
    /// admissions cannot be confused in the durable run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_identity: Option<CallerIdentityProof>,
    /// Destination-selected remote invocation trust mode. Absent for local
    /// operator admissions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_invoke_mode: Option<RemoteAgentInvokeMode>,
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
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "activity `{activity}` declares `trustedHostExecution: true`, which only the built-in \
     `{TRUSTED_HOST_ACTIVITY}` activity may declare; an unsandboxed provider subprocess is \
     admitted per invocation by an operator, never by an asset"
)]
pub struct TrustedHostActivityError {
    /// Name of the offending activity asset.
    pub activity: String,
}

/// Reject `trustedHostExecution` on any activity but [`TRUSTED_HOST_ACTIVITY`].
///
/// Called from asset load, so a hand-written or edited asset is refused before
/// it can reach a dispatcher.
pub fn validate_trusted_host_activity(
    activity: &str,
    declares_trusted_host: bool,
) -> Result<(), TrustedHostActivityError> {
    if declares_trusted_host && activity != TRUSTED_HOST_ACTIVITY {
        return Err(TrustedHostActivityError {
            activity: activity.to_string(),
        });
    }
    Ok(())
}
