//! Experiment overlays carried across delivery hosts, and what one activity
//! dispatch received from a live arm.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

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
    /// Repository skill bundles resolved by the owner at the claim base.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<ExperimentSkillBundle>,
}

/// One immutable skill tree. Paths are relative to its root; symlinks and
/// submodules are never captured. The digest covers paths, bytes and modes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentSkillBundle {
    pub name: String,
    pub path: String,
    pub revision: String,
    pub files: BTreeMap<String, ExperimentSkillFile>,
    pub digest: String,
}

/// A regular repository file, including its executable bit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentSkillFile {
    pub bytes: Vec<u8>,
    pub executable: bool,
}

/// Whether this host can isolate the provider's skill discovery without
/// changing any shared discovery directory.
pub fn experiment_skill_provider_supported(provider: &str) -> bool {
    cfg!(unix) && provider == "claude"
}

impl ExperimentSkillBundle {
    /// Maximum raw bytes in one bundle; the whole claim also has a wire cap.
    pub const MAX_BYTES: usize = 64 * 1024;
    /// Maximum regular files in one bundle.
    pub const MAX_FILES: usize = 256;

    /// Content digest, independent of the source revision and directory.
    pub fn content_digest(&self) -> String {
        let mut hash = Sha256::new();
        for (path, file) in &self.files {
            hash.update((path.len() as u64).to_be_bytes());
            hash.update(path.as_bytes());
            hash.update([u8::from(file.executable)]);
            hash.update((file.bytes.len() as u64).to_be_bytes());
            hash.update(&file.bytes);
        }
        format!("sha256:{}", lower_hex(hash.finalize()))
    }

    /// Validate a captured tree before joining any of its names to a local
    /// path. This also detects corrupt captures on followers.
    pub fn verify(&self) -> Result<(), String> {
        let safe_path = |path: &str| {
            !path.is_empty()
                && !path.contains('\\')
                && !path.chars().any(char::is_control)
                && path.split('/').all(|part| {
                    !part.is_empty()
                        && part != "."
                        && part != ".."
                        && !part.eq_ignore_ascii_case(".git")
                        && !part.eq_ignore_ascii_case(".orbit")
                        && !part.contains(':')
                })
        };
        if self.name.is_empty()
            || self.name.len() > 64
            || !self
                .name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || !safe_path(&self.path)
            || self.revision.len() != 40
            || !self.revision.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("invalid skill bundle identity or repository path".into());
        }
        if self.files.len() > Self::MAX_FILES
            || !self.files.contains_key("SKILL.md")
            || self.files.keys().any(|path| !safe_path(path))
            || self
                .files
                .values()
                .map(|file| file.bytes.len())
                .sum::<usize>()
                > Self::MAX_BYTES
        {
            return Err(
                "skill bundle needs SKILL.md, safe file paths, at most 256 files and 64 KiB".into(),
            );
        }
        if self.content_digest() != self.digest {
            return Err("skill bundle digest mismatch".into());
        }
        Ok(())
    }
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<ExperimentSkillBundle>,
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
    /// Skill name to the digest of the bundle actually delivered.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub skills: BTreeMap<String, String>,
    /// `sha256:<hex>` over the received instruction hash, tool delta and
    /// (when present) skill bundle digests.
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
        let skills: BTreeMap<_, _> = overlay
            .skills
            .iter()
            .map(|bundle| (bundle.name.clone(), bundle.digest.clone()))
            .collect();
        let mut received = serde_json::json!({
            "instruction_sha256": instruction.as_ref().map(|record| record.sha256.as_str()),
            "suppressed_default_instructions": overlay.suppress_default_instructions,
            "tools_added": tools_added,
            "tools_removed": tools_removed,
        });
        if !skills.is_empty() {
            received["skills"] = serde_json::json!(skills);
        }
        Self {
            experiment: overlay.experiment.clone(),
            arm: overlay.arm.clone(),
            segment: overlay.segment,
            role: overlay.role,
            instruction,
            suppressed_default_instructions: overlay.suppress_default_instructions,
            tools_added,
            tools_removed,
            skills,
            digest: format!(
                "sha256:{}",
                lower_hex(Sha256::digest(received.to_string().as_bytes()))
            ),
        }
    }
}
