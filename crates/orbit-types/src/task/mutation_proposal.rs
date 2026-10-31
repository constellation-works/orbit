//! Mutation proposals and their application receipts [granted-tools §4].
//!
//! A mutating operator-granted tool never runs live in the implementing
//! activity. The agent records the intended call as a proposal artifact; the
//! review gate shows it to the reviewer beside the diff, and only after that
//! gate approves does the pipeline apply it, once, recording a receipt.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Schema version of [`MutationProposal`] and [`MutationReceipt`].
pub const MUTATION_PROPOSAL_SCHEMA_VERSION: u32 = 1;

/// Directory of mutation proposal artifacts. Agent and operator writes here
/// are admitted only as a valid proposal for a tool the task grants.
pub const MUTATION_PROPOSAL_ARTIFACT_DIR: &str = "mutation-proposals";

/// Directory of application receipts, reserved for the pipeline's system
/// writer: a receipt an agent could write would suppress application
/// through the idempotency key.
pub const MUTATION_RECEIPT_ARTIFACT_DIR: &str = "mutation-receipts";

/// Whether a canonical artifact path is a mutation proposal. ASCII case
/// folding protects owners on case-insensitive filesystems.
pub fn is_mutation_proposal_artifact(path: &str) -> bool {
    in_artifact_dir(path, MUTATION_PROPOSAL_ARTIFACT_DIR)
}

/// Whether a canonical artifact path is a reserved application receipt.
pub fn is_mutation_receipt_artifact(path: &str) -> bool {
    in_artifact_dir(path, MUTATION_RECEIPT_ARTIFACT_DIR)
}

/// The receipt artifact for one idempotency key.
pub fn mutation_receipt_artifact_path(idempotency_key: &str) -> String {
    format!("{MUTATION_RECEIPT_ARTIFACT_DIR}/{idempotency_key}.json")
}

fn in_artifact_dir(path: &str, dir: &str) -> bool {
    path.split_once('/')
        .is_some_and(|(head, _)| head.eq_ignore_ascii_case(dir))
}

/// One proposed call of a mutating granted tool, as stored on the task.
///
/// The writer supplies `tool`, `input` and `rationale`; admission derives
/// `task_id` and `idempotency_key` from the task and the canonical
/// `tool` + `input`, so a reworded rationale is still the same change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MutationProposal {
    pub schema_version: u32,
    pub task_id: String,
    /// The exact canonical name of the granted tool.
    pub tool: String,
    /// The exact tool input the pipeline applies.
    pub input: Value,
    pub rationale: String,
    pub idempotency_key: String,
}

/// What a writer supplies for a proposal; derived fields are optional and,
/// when present, must equal what admission derives.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MutationProposalDraft {
    #[serde(default)]
    pub schema_version: Option<u32>,
    #[serde(default)]
    pub task_id: Option<String>,
    pub tool: String,
    pub input: Value,
    pub rationale: String,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

/// A proposal the review gate put in front of its reviewer, bound to the
/// exact artifact bytes it read. Application applies only this record's
/// input, and only while the task's artifact still has this digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewedMutationProposal {
    /// The proposal artifact's path on its task.
    pub artifact: String,
    /// SHA-256 of the proposal artifact's stored bytes.
    pub sha256: String,
    pub proposal: MutationProposal,
}

/// Where one proposal's application stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationReceiptState {
    /// Recorded before the call: a receipt left here means the outcome is
    /// unknown, and nothing re-applies it until an operator reconciles it.
    Pending,
    /// The call succeeded; the key never applies again.
    Applied,
    /// The call was refused or failed; a later approval may retry it.
    Failed,
}

impl MutationReceiptState {
    /// Stable label.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Applied => "applied",
            Self::Failed => "failed",
        }
    }
}

/// The pipeline's record of applying one reviewed proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationReceipt {
    pub schema_version: u32,
    pub task_id: String,
    pub idempotency_key: String,
    pub tool: String,
    /// The proposal artifact and digest the applied input was bound to.
    pub proposal_artifact: String,
    pub proposal_sha256: String,
    /// The input the pipeline called the tool with: the reviewed input.
    pub applied_input: Value,
    pub state: MutationReceiptState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The review attempt whose approval authorized the application.
    pub review_attempt_id: String,
    pub run_id: String,
    pub recorded_at: DateTime<Utc>,
}
