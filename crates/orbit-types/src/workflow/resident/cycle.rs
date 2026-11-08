//! The deterministic observer's durable live-cycle fence.

/// A live cycle published by the deterministic observer after taking its lease.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResidentLiveCycle {
    pub cycle: String,
    pub run_id: String,
    /// The cycle lease's expiry; an expired writer is never live.
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

/// History event recording the authority one cycle was granted.
pub const RESIDENT_CYCLE_GRANTED_EVENT: &str = "resident_cycle_granted";
/// History event recording that a cycle's conversation reference expired or
/// could not be resumed, so the cycle fell back to `continuity: rebuilt`.
pub const RESIDENT_CONTINUITY_REBUILT_EVENT: &str = "resident_continuity_rebuilt";
/// History event recording one mutating tool call a cycle made.
pub const RESIDENT_ACTION_EVENT: &str = "resident_action";
/// The job a resident cycle runs as.
pub const RESIDENT_CYCLE_JOB: &str = "resident_cycle_pipeline";
/// Checkpoints a cycle's brief carries, newest first.
pub const RESIDENT_BRIEF_CHECKPOINTS: usize = 5;

/// What the observer granted one cycle: the effective authority, narrowed to
/// observe-only while a maintenance notice is open in scope or the mandate's
/// deadline has passed. The tool boundary enforces this, never the
/// checkpoint's own report.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ResidentCycleGrant {
    pub cycle: String,
    pub run_id: String,
    /// Rights the cycle may use; empty when observe-only.
    pub rights: Vec<super::ResidentRight>,
    /// Workspaces the cycle's tools may name.
    pub workspaces: Vec<String>,
    pub observe_only: bool,
    /// Why the cycle is observe-only: `notice:<id>` or `deadline`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub narrowed_by: Option<String>,
    /// The dead cycle this one recovers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_of: Option<String>,
    /// The wake digest observe computed.
    pub digest: String,
    /// Why the cycle woke.
    #[serde(default)]
    pub reasons: Vec<String>,
}

/// Where one cycle action stands: `started` before the tool runs, then one
/// result row.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResidentActionPhase {
    Started,
    Ok,
    Failed,
    Refused,
    AlreadyDone,
}

impl ResidentActionPhase {
    /// The checkpoint `result` this phase reports, if it is a result.
    pub fn result(self) -> Option<&'static str> {
        match self {
            Self::Started => None,
            Self::Ok => Some("ok"),
            Self::Failed => Some("failed"),
            Self::Refused => Some("refused"),
            Self::AlreadyDone => Some("already_done"),
        }
    }
}

/// One audit row of a cycle's mutating tool call, keyed
/// `resident:<mandate>:<cycle>:<n>`. Core assigns the key; the recovery
/// cycle reads these rows to skip what the dead cycle already did.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ResidentActionRow {
    pub key: String,
    pub cycle: String,
    /// The tool's canonical name.
    pub tool: String,
    /// Digest of the call's canonical input: `sha256:v2:` over the input
    /// with object keys sorted recursively (array order preserved). Rows
    /// written before canonicalisation carry a bare `sha256:` legacy digest.
    pub input_digest: String,
    /// The task the call names, when it names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub phase: ResidentActionPhase,
    /// What the call produced (a run id, a task id), when known.
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}
