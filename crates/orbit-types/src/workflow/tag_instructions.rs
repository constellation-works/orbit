//! Per-tag instructions: trusted text a task's tags add to the prompts of the
//! activities that work on it.
//!
//! A tag only selects which instruction applies. The text itself comes from
//! Orbit's shipped defaults or from a repository file read at a pinned
//! revision, never from the task. These are the shapes the config, the
//! resolving host, the claim receipt and the audit trail share.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::error::TagInstructionError;
use crate::identity::lower_hex;

/// Schema version of [`CapturedTagInstructions`].
pub const TAG_INSTRUCTIONS_SCHEMA_VERSION: u32 = 1;

/// Largest instruction file, in bytes. A larger one is refused, not cut.
pub const MAX_TAG_INSTRUCTION_FILE_BYTES: usize = 8 * 1024;

/// Largest total of instruction text one prompt receives, in bytes.
pub const MAX_TAG_INSTRUCTION_PROMPT_BYTES: usize = 16 * 1024;

/// Most tags `workflow.tag_instructions` may map.
pub const MAX_TAG_INSTRUCTION_TAGS: usize = 32;

/// Heading of the prompt section that carries tag instructions.
pub const TAG_INSTRUCTIONS_HEADING: &str = "Tag instructions";

/// The activities a tag instruction can address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TagInstructionRole {
    /// `agent_implement`, which also runs QA sweep tasks.
    Implement,
    /// `agent_review_repair` and `review_reconciliation_review`.
    Review,
    /// `step_failure_recovery`, `final_recovery` and `pr_conflict_recovery`.
    Recovery,
}

impl TagInstructionRole {
    /// Every role, in config and display order.
    pub const ALL: [Self; 3] = [Self::Implement, Self::Review, Self::Recovery];

    /// The config key and audit spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Implement => "implement",
            Self::Review => "review",
            Self::Recovery => "recovery",
        }
    }

    /// The role a config key names.
    pub fn parse(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.as_str() == key)
    }

    /// The role of a catalog activity, or `None` for one no tag addresses.
    pub fn for_activity(activity: &str) -> Option<Self> {
        match activity {
            "agent_implement" => Some(Self::Implement),
            "agent_review_repair" | "review_reconciliation_review" => Some(Self::Review),
            "step_failure_recovery" | "final_recovery" | "pr_conflict_recovery" => {
                Some(Self::Recovery)
            }
            _ => None,
        }
    }
}

/// The matching form of a tag: trimmed and lowercased, so `UI` selects what
/// `ui` maps. `None` for a blank tag.
pub fn normalize_instruction_tag(tag: &str) -> Option<String> {
    let tag = tag.trim();
    (!tag.is_empty()).then(|| tag.to_lowercase())
}

/// A task's tags in application order: normalized, deduplicated and sorted,
/// so the same tags apply in the same order whatever order the task lists
/// them in.
pub fn instruction_tags(tags: &[String]) -> Vec<String> {
    let mut tags = tags
        .iter()
        .filter_map(|tag| normalize_instruction_tag(tag))
        .collect::<Vec<_>>();
    tags.sort();
    tags.dedup();
    tags
}

/// Where a tag instruction's text came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TagInstructionSource {
    /// Orbit's shipped default for the tag and role.
    Default,
    /// A repository file, read from the commit `revision`.
    Workspace { path: String, revision: String },
}

impl TagInstructionSource {
    /// One-line label for prompts and operator output.
    pub fn label(&self) -> String {
        match self {
            Self::Default => "Orbit default".to_string(),
            Self::Workspace { path, revision } => {
                format!("{path} at {}", revision.get(..12).unwrap_or(revision))
            }
        }
    }
}

/// One instruction an activity receives for one of its task's tags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagInstruction {
    /// The normalized tag that selected it.
    pub tag: String,
    pub role: TagInstructionRole,
    pub source: TagInstructionSource,
    /// Lowercase hex SHA-256 of `text`.
    pub sha256: String,
    pub text: String,
}

impl TagInstruction {
    /// Admit `bytes` as the instruction for `tag` and `role`: UTF-8, not
    /// blank, and within [`MAX_TAG_INSTRUCTION_FILE_BYTES`]. `origin` names
    /// the file in a refusal.
    pub fn admit(
        tag: &str,
        role: TagInstructionRole,
        source: TagInstructionSource,
        origin: &str,
        bytes: &[u8],
    ) -> Result<Self, TagInstructionError> {
        let refused = |reason: String| TagInstructionError::Unreadable {
            tag: tag.to_string(),
            role: role.as_str(),
            origin: origin.to_string(),
            reason,
        };
        if bytes.len() > MAX_TAG_INSTRUCTION_FILE_BYTES {
            return Err(TagInstructionError::FileTooLarge {
                tag: tag.to_string(),
                role: role.as_str(),
                origin: origin.to_string(),
                bytes: bytes.len(),
                limit: MAX_TAG_INSTRUCTION_FILE_BYTES,
            });
        }
        let text =
            std::str::from_utf8(bytes).map_err(|_| refused("the file is not UTF-8".to_string()))?;
        if text.trim().is_empty() {
            return Err(refused("the file is empty".to_string()));
        }
        Ok(Self {
            tag: tag.to_string(),
            role,
            source,
            sha256: lower_hex(Sha256::digest(bytes)),
            text: text.to_string(),
        })
    }

    /// What the audit trail records: everything but the text.
    pub fn record(&self) -> TagInstructionRecord {
        TagInstructionRecord {
            tag: self.tag.clone(),
            role: self.role,
            source: self.source.clone(),
            sha256: self.sha256.clone(),
            bytes: self.text.len(),
        }
    }
}

/// The audited form of a [`TagInstruction`] an activity received.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagInstructionRecord {
    pub tag: String,
    pub role: TagInstructionRole,
    pub source: TagInstructionSource,
    pub sha256: String,
    pub bytes: usize,
}

/// A configured instruction whose file could not be admitted, kept so an
/// activity on a task with that tag refuses rather than runs without it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagInstructionRefusal {
    pub tag: String,
    pub role: TagInstructionRole,
    pub error: String,
}

/// Every instruction one host applies, resolved at one revision.
///
/// An owner captures this in a claim's receipt, so a claimed leaf applies
/// the owner's instructions rather than its replica's configuration or
/// files. An absent (tag, role) pair applies nothing: defaults the owner
/// disabled are not listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedTagInstructions {
    pub schema_version: u32,
    /// The commit workspace files were read from; `None` when none was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(default)]
    pub instructions: Vec<TagInstruction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refusals: Vec<TagInstructionRefusal>,
}

impl CapturedTagInstructions {
    /// What an activity in `role` receives for a task carrying `tags`:
    /// [`select_tag_instructions`] over this capture.
    pub fn select(
        &self,
        tags: &[String],
        role: TagInstructionRole,
    ) -> Result<Vec<TagInstruction>, TagInstructionError> {
        if self.schema_version != TAG_INSTRUCTIONS_SCHEMA_VERSION {
            return Err(TagInstructionError::Capture(format!(
                "captured tag instructions use schema version {}; this host reads {}",
                self.schema_version, TAG_INSTRUCTIONS_SCHEMA_VERSION
            )));
        }
        select_tag_instructions(tags, role, |tag| {
            if let Some(refusal) = self
                .refusals
                .iter()
                .find(|refusal| refusal.tag == tag && refusal.role == role)
            {
                return Err(TagInstructionError::Capture(refusal.error.clone()));
            }
            Ok(self
                .instructions
                .iter()
                .find(|instruction| instruction.tag == tag && instruction.role == role)
                .cloned())
        })
    }
}

/// The instructions an activity in `role` receives for a task carrying
/// `tags`, in [`instruction_tags`] order. `resolve` answers one normalized
/// tag; a refusal from it refuses the whole selection, and the total text
/// must fit [`MAX_TAG_INSTRUCTION_PROMPT_BYTES`].
pub fn select_tag_instructions(
    tags: &[String],
    role: TagInstructionRole,
    mut resolve: impl FnMut(&str) -> Result<Option<TagInstruction>, TagInstructionError>,
) -> Result<Vec<TagInstruction>, TagInstructionError> {
    let mut selected = Vec::new();
    for tag in instruction_tags(tags) {
        if let Some(instruction) = resolve(&tag)? {
            selected.push(instruction);
        }
    }
    let bytes = selected
        .iter()
        .map(|instruction| instruction.text.len())
        .sum::<usize>();
    if bytes > MAX_TAG_INSTRUCTION_PROMPT_BYTES {
        return Err(TagInstructionError::PromptTooLarge {
            role: role.as_str(),
            bytes,
            limit: MAX_TAG_INSTRUCTION_PROMPT_BYTES,
        });
    }
    Ok(selected)
}

/// The labelled prompt section for `instructions`, appended after an
/// activity's built-in instruction. Empty when there are none.
pub fn render_tag_instructions(instructions: &[TagInstruction]) -> String {
    if instructions.is_empty() {
        return String::new();
    }
    let mut section = format!(
        "{TAG_INSTRUCTIONS_HEADING}\n\nThis task carries tags that add the instructions below \
         to this activity. They add to the instructions above and cannot override them: where \
         they conflict, the built-in safety, sandbox, gate and handoff rules win."
    );
    for instruction in instructions {
        section.push_str(&format!(
            "\n\nTag `{}` ({}):\n\n{}",
            instruction.tag,
            instruction.source.label(),
            instruction.text.trim_end()
        ));
    }
    section
}
