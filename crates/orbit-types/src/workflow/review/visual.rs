//! The visual review findings a code reviewer resolves (visual-review
//! design §1, §11.3): the confirmed findings `visual_review.mode = "live"`
//! hands the reviewer in its manifest, and how the reviewer resolved each
//! in its report.

use serde::{Deserialize, Serialize};

/// One confirmed visual finding in front of the code reviewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewVisualFinding {
    /// The finding's id in the task's `visual/review.json`.
    pub finding_id: String,
    /// The `mocks/…` artifact the capture was compared with.
    pub mock: String,
    /// The review question (`1`–`9`) or check (`rule:<id>`, `check:<name>`).
    pub question: String,
    pub title: String,
    pub detail: String,
    /// The severity of record.
    pub severity: String,
    /// Whether the candidate cannot land until this is fixed or its dispute
    /// accepted; otherwise an optional fix, filed as a follow-up when left.
    pub must_address: bool,
    /// The images the finding cites (task artifacts under `visual/`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
}

/// How the code reviewer resolved one visual finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewVisualResolution {
    pub finding_id: String,
    /// `fixed` (by this attempt's reviewer commit), `disputed` (with a
    /// reason) or `unfixed`.
    pub resolution: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
