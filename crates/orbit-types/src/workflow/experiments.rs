//! Immutable, unapplied experiment overlays carried across delivery hosts.

use serde::{Deserialize, Serialize};

/// One task's shadow treatment. The overlay is captured even for control.
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
}

/// A present, empty capture means the owner enrolled this task in no experiment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturedExperiments {
    pub task_id: String,
    pub assignments: Vec<CapturedExperiment>,
}
