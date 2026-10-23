//! Derived per-task codebase analytics facts, kept so a scan need not reread
//! every task bundle.

use orbit_common::OrbitError;

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

/// A derived, rebuildable cache: never authority. Dropping its table, or
/// any of its rows, loses nothing but the work of computing them again.
pub trait CodebaseFactsStoreBackend: Send + Sync {
    /// Every stored row for `workspace_id`; empty when none was written or
    /// the table does not exist.
    fn codebase_task_facts(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<CodebaseTaskFactsRow>, OrbitError>;

    /// Upsert `rows` and delete the rows of `removed` tasks, in one
    /// transaction, creating the table when it is missing.
    fn write_codebase_task_facts(
        &self,
        workspace_id: &str,
        rows: &[CodebaseTaskFactsRow],
        removed: &[String],
    ) -> Result<(), OrbitError>;
}
