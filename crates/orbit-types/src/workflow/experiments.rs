//! Experiment overlays carried across delivery hosts, and what one activity
//! dispatch received from a live arm.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{TagInstruction, TagInstructionRecord, TagInstructionRole};
use crate::identity::lower_hex;

/// One task's treatment. The overlay is captured even for control.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturedExperiment {
    pub experiment: String,
    pub arm: String,
    pub segment: u32,
    pub forced: bool,
    pub digest: String,
    /// Canonical overlay JSON, preserved verbatim for digest verification.
    pub overlay_json: String,
    /// Whether the experiment applies its overlay; a shadow capture applies
    /// nothing.
    #[serde(default)]
    pub live: bool,
    /// A live arm's instructions, resolved by the owner at the claim base, so
    /// a claimed leaf reads no file and no owner record at dispatch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instructions: Vec<CapturedExperimentInstruction>,
}

/// One role's instruction a live arm resolved at the claim base: the admitted
/// text, nothing when the arm suppresses the shipped defaults, or the refusal
/// that stops a run in that role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturedExperimentInstruction {
    pub role: TagInstructionRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<TagInstruction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
}

/// A present, empty capture means the owner enrolled this task in no experiment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturedExperiments {
    pub task_id: String,
    pub assignments: Vec<CapturedExperiment>,
}

/// What one live experiment arm changes for one activity dispatch. The
/// instruction joins the tag-instruction section; the tool delta is applied
/// after the activity's tool grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentActivityOverlay {
    pub experiment: String,
    pub arm: String,
    pub segment: u32,
    pub role: TagInstructionRole,
    /// The arm's instruction for this role, after any tag instruction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<TagInstruction>,
    /// The arm sets this role to `false`: Orbit's shipped default
    /// instructions are left out.
    #[serde(default)]
    pub suppress_default_instructions: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools_add: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools_remove: Vec<String>,
}

/// What one dispatch received from a live arm, as its audit trail records
/// it: the instruction without its text, and the tools the grant actually
/// gained or lost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentOverlayRecord {
    pub experiment: String,
    pub arm: String,
    pub segment: u32,
    pub role: TagInstructionRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<TagInstructionRecord>,
    #[serde(default)]
    pub suppressed_default_instructions: bool,
    #[serde(default)]
    pub tools_added: Vec<String>,
    #[serde(default)]
    pub tools_removed: Vec<String>,
    /// `sha256:<hex>` over the received instruction hash and tool delta.
    pub digest: String,
}

impl ExperimentOverlayRecord {
    /// The record of `overlay` as received with `tools_added` and
    /// `tools_removed`, digested.
    pub fn received(
        overlay: &ExperimentActivityOverlay,
        mut tools_added: Vec<String>,
        mut tools_removed: Vec<String>,
    ) -> Self {
        tools_added.sort();
        tools_added.dedup();
        tools_removed.sort();
        tools_removed.dedup();
        let instruction = overlay.instruction.as_ref().map(TagInstruction::record);
        let received = serde_json::json!({
            "instruction_sha256": instruction.as_ref().map(|record| record.sha256.as_str()),
            "suppressed_default_instructions": overlay.suppress_default_instructions,
            "tools_added": tools_added,
            "tools_removed": tools_removed,
        });
        Self {
            experiment: overlay.experiment.clone(),
            arm: overlay.arm.clone(),
            segment: overlay.segment,
            role: overlay.role,
            instruction,
            suppressed_default_instructions: overlay.suppress_default_instructions,
            tools_added,
            tools_removed,
            digest: format!(
                "sha256:{}",
                lower_hex(Sha256::digest(received.to_string().as_bytes()))
            ),
        }
    }
}
