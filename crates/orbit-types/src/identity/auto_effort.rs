//! `<crew>@auto` pool entries: the effort a crew runs at, chosen per draw
//! from the signal its subject offers, and the record a draw keeps of that
//! choice.
//!
//! `@auto` is a pool-entry form only. A task crew, an explicit run crew and
//! every recorded run name the resolved `<crew>@<effort>` (or the bare crew
//! when the crew's own effort applied), never `@auto`.

use serde::{Deserialize, Serialize};

use super::agent_pair::CREW_EFFORT_SEPARATOR;
use crate::task::TaskComplexity;

/// The effort-slot value that asks for a per-draw effort: `opus@auto`.
pub const AUTO_EFFORT: &str = "auto";

/// The crew a `<crew>@auto` reference names, or `None` for any other
/// reference.
#[must_use]
pub fn auto_effort_base(reference: &str) -> Option<&str> {
    let (base, effort) = reference.trim().split_once(CREW_EFFORT_SEPARATOR)?;
    (effort.trim() == AUTO_EFFORT).then(|| base.trim())
}

/// The `<crew>@auto` reference for `base`.
#[must_use]
pub fn auto_effort_reference(base: &str) -> String {
    format!("{}{CREW_EFFORT_SEPARATOR}{AUTO_EFFORT}", base.trim())
}

/// What a draw knew about its subject when it chose an effort. Each field is
/// absent when that role has no such signal at admission.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoEffortSignal {
    /// The subject's complexity tier: filed for a task pilot, assessed for
    /// an implementer, settled for a reviewer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complexity: Option<TaskComplexity>,
    /// Reviewable diff lines; only a reviewer has them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<u64>,
    /// Files: the declared context-file targets for an implementer (the
    /// task-risk forecast's pre-dispatch size), the changed files for a
    /// reviewer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<u64>,
}

impl AutoEffortSignal {
    /// Whether the subject offered no signal at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.complexity
            .is_none_or(|complexity| !complexity.is_assessed())
            && self.lines.is_none()
            && self.files.is_none()
    }
}

/// Why a draw ran at the effort it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoEffortBasis {
    /// A configured band matched the signal.
    Band,
    /// No band matched, or there was no signal: the crew's configured
    /// effort applied.
    Fallback,
    /// A task-pilot escalation re-ran the pilot at a higher effort.
    Escalation,
}

/// The record of one `<crew>@auto` resolution, kept on the draw it decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoEffortChoice {
    /// The pool entry: `<crew>@auto`.
    pub requested: String,
    /// The reference that ran: `<crew>@<effort>`, or the bare crew when the
    /// crew's configured effort applied and it names none.
    pub resolved: String,
    /// The reference the bands chose before exploration; equal to
    /// `resolved` unless `explored`.
    pub policy: String,
    pub basis: AutoEffortBasis,
    /// The zero-based index of the band that matched, for `band`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub band: Option<usize>,
    /// Whether the exploration share dispatched an effort other than the
    /// policy's.
    #[serde(default)]
    pub explored: bool,
    pub signal: AutoEffortSignal,
}
