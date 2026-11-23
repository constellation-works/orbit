//! The guarded-write selector keys of `orbit.task.add` and
//! `orbit.task.update` [ORB-26098].
//!
//! The task tools' guarded-write parser and the MCP workspace-echo rules
//! share this list.

/// Input keys that select `orbit.task.add`/`orbit.task.update`'s guarded
/// write; any one of them present makes the call guarded.
pub(crate) const GUARDED_SELECTOR_KEYS: &[&str] =
    &["request_id", "expected_revision", "verdict", "complete"];
