//! The guarded-write selector keys of `orbit.task.add` and
//! `orbit.task.update` [ORB-26098].
//!
//! A crate-root leaf, so the task tools' guarded-write parser and the MCP
//! workspace-echo rules read one list without `mcp` naming the tool modules
//! (and without the tools reaching into `mcp`, which would close an
//! `mcp` → `registry` → tools → `mcp` cycle).

/// Input keys that select `orbit.task.add`/`orbit.task.update`'s guarded
/// write; any one of them present makes the call guarded.
pub(crate) const GUARDED_SELECTOR_KEYS: &[&str] =
    &["request_id", "expected_revision", "verdict", "complete"];
