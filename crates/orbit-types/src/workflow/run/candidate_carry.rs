//! The persisted output of the claimed leaf's candidate-preservation hook.

use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};

use super::FailureActivityCheckpoint;

/// Where the failed leaf's candidate can be resumed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CandidateCarry {
    /// The candidate was pushed to its durable ref.
    #[serde(rename = "durable")]
    Durable,
    /// Preservation or the push failed; the reason stays in the output.
    #[serde(rename = "failed")]
    Failed,
    /// The leaf already published its current branch tip.
    #[serde(rename = "published")]
    Published,
    /// No candidate, or no recognized carry information in an older record.
    #[default]
    #[serde(rename = "none", other)]
    None,
}

/// Checkpointed `claim_candidate_carry` output, with the original optional keys.
///
/// Missing carry information means [`CandidateCarry::None`]. A failed carry
/// without a reason keeps the consumer's existing default diagnostic. There is
/// no migration of older checkpoints. Serialization preserves even the original
/// key order: unpublished candidates added `carry` after their branch fields,
/// whereas published candidates and early exits wrote it immediately after phase.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CandidateCarryOutput {
    /// Activity phase, normally `candidate_carry`.
    #[serde(default)]
    pub phase: String,
    /// The hook's outcome; absent or unknown values carry no information.
    #[serde(default)]
    pub carry: CandidateCarry,
    /// The single task named by the leaf, when available.
    #[serde(default)]
    pub task_id: Option<String>,
    /// Candidate branch, when it could be resolved.
    #[serde(default)]
    pub branch: Option<String>,
    /// Candidate branch tip, including any unpublished reviewer fix.
    #[serde(default)]
    pub head_sha: Option<String>,
    /// Earlier published tip when the candidate advanced past it.
    #[serde(default)]
    pub published_sha: Option<String>,
    /// Ref on origin that holds a successfully carried candidate.
    #[serde(default)]
    pub durable_ref: Option<String>,
    /// Present and true for a snapshot made before the normal commit.
    #[serde(default)]
    pub partial: Option<bool>,
    /// Why there was no candidate, or why preservation or push failed.
    #[serde(default)]
    pub reason: Option<String>,
}

impl CandidateCarryOutput {
    /// Start a hook output with the phase and outcome and no optional keys.
    #[must_use]
    pub fn new(carry: CandidateCarry) -> Self {
        Self {
            phase: "candidate_carry".into(),
            carry,
            task_id: None,
            branch: None,
            head_sha: None,
            published_sha: None,
            durable_ref: None,
            partial: None,
            reason: None,
        }
    }

    /// Read this hook's checkpoint once at the boundary. Other activities and
    /// unparseable outputs provide no carry information.
    #[must_use]
    pub fn from_checkpoint(checkpoint: &FailureActivityCheckpoint) -> Option<Self> {
        (checkpoint.activity_name == "claim_candidate_carry")
            .then(|| serde_json::from_value(checkpoint.output.clone()).ok())
            .flatten()
    }
}

impl Serialize for CandidateCarryOutput {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("phase", &self.phase)?;
        let early_carry = self.branch.is_none()
            || self.carry == CandidateCarry::Published
            || self.published_sha.is_some();
        if early_carry {
            map.serialize_entry("carry", &self.carry)?;
        }
        if let Some(task_id) = &self.task_id {
            map.serialize_entry("task_id", task_id)?;
        }
        if let Some(branch) = &self.branch {
            map.serialize_entry("branch", branch)?;
        }
        if let Some(head_sha) = &self.head_sha {
            map.serialize_entry("head_sha", head_sha)?;
        }
        if let Some(published_sha) = &self.published_sha {
            map.serialize_entry("published_sha", published_sha)?;
        }
        if let Some(partial) = &self.partial {
            map.serialize_entry("partial", partial)?;
        }
        if !early_carry {
            map.serialize_entry("carry", &self.carry)?;
        }
        if let Some(durable_ref) = &self.durable_ref {
            map.serialize_entry("durable_ref", durable_ref)?;
        }
        if let Some(reason) = &self.reason {
            map.serialize_entry("reason", reason)?;
        }
        map.end()
    }
}
