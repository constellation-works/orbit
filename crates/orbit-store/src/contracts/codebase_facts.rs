//! Derived per-task codebase and agent analytics facts, kept so a scan need
//! not reread every task bundle.

/// One task's stored facts. The store keeps the bytes and the labels; what
/// they mean, and whether they are still current, is the caller's to decide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodebaseTaskFactsRow {
    pub task_id: String,
    /// The fact format the row was written in. A reader skips a row in any
    /// other format and replaces it.
    pub format_version: u32,
    /// What the facts were computed from, such as the task's history stamp.
    pub source: String,
    pub facts_json: String,
}

/// One task's stored agent-scorecard facts. The store keeps the bytes and the
/// labels; what they mean, and whether they are still current, is the caller's
/// to decide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTaskFactsRow {
    pub task_id: String,
    /// The fact format the row was written in. A reader skips a row in any
    /// other format and replaces it.
    pub format_version: u32,
    /// What the facts were computed from: the task's history stamp and per-task
    /// input digest.
    pub source: String,
    pub facts_json: String,
}
