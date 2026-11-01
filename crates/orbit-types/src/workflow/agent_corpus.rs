//! Pinned workspace agent-corpus rows and deterministic append proposals.

use serde::{Deserialize, Serialize};

/// Maximum injected corpus row text, including a truncation note.
pub const MAX_AGENT_CORPUS_BYTES: usize = 8 * 1024;

/// One generated index row. Paths are relative to the index's directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCorpusRow {
    pub path: String,
    #[serde(rename = "type")]
    pub doc_type: String,
    pub status: String,
    pub applies_to: Vec<String>,
    pub title: String,
    pub links: Vec<String>,
    /// Engine-classified infrastructure signature, never agent free text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_infra_class: Option<String>,
}

/// Exactly what an activity receives, also captured in an owner's receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCorpusInjection {
    pub index: String,
    pub revision: String,
    pub rows: Vec<AgentCorpusRow>,
    pub truncated: bool,
    pub text: String,
    pub sha256: String,
}

/// A deterministic outbox record stored beside its source in task history.
/// Materializing these proposals never commits or merges repository changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCorpusAppend {
    pub schema_version: u32,
    pub id: String,
    /// Full repository-relative destination doc.
    pub path: String,
    pub section: String,
    /// Structured source, rendered as data by the materializer.
    pub entry: serde_json::Value,
}
