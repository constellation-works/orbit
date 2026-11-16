//! Convergence of stored pre-family tool names [ORB-23552].
//!
//! Tool families [ORB-23280] made an operation such as
//! `orbit.task.artifact.put` an internal identity: a stored tool list names
//! the family action (`orbit.task.artifact:put`) instead. Records written
//! before that change still hold operation names. Admission reads them as
//! family actions ([`orbit_types::tool::tool_requirement_name`]) for the
//! compatibility window; this module rewrites the stored records so they
//! converge and that read path can retire:
//!
//! - auto-task definitions (`template.required_tools` in shipped, forked and
//!   workspace-local YAML), rewritten in place so comments and layout survive.
//!   A file the managed manifest tracks keeps its provenance: the manifest
//!   records the rewritten bytes, so the rename never turns a managed
//!   definition into a fork;
//! - task `required_tools` and `granted_tools`, rewritten in the bundle with a
//!   `tool_names_migrated` history event;
//! - installed plugin manifests' `permissions.orbit_tools`, rewritten only
//!   when the bytes are still the manifest this host recorded at install.
//!   The rewritten digest is recorded with that rewrite. A failed record
//!   restores the consented bytes, unless a competing pass already recorded
//!   this rewrite, in which case the healed file stays. The read, rewrite,
//!   record, rollback and completion marker share one advisory lock. A file
//!   left as that rewrite is finished on the next pass by recording the
//!   digest it already has. Exact originals are retained durably before
//!   publication, so mixed legacy/canonical lists recover without inversion.
//!
//! The mapping is [`orbit_types::tool::family_action_tool_names`]. Every
//! rewrite runs once on upgrade (the first writable runtime open) and again
//! wherever Orbit meets an old name: admission, auto-task load and mint, and
//! `orbit workspace sync`.

mod auto_task_catalog;
mod plugin_manifest;
#[cfg(any(test, feature = "test-support"))]
#[path = "publish_hook.rs"]
pub(crate) mod plugin_manifest_publish_hook;
mod plugin_recovery;
mod task_tools;

use std::path::PathBuf;

use serde::Serialize;

pub(crate) use auto_task_catalog::heal_workspace_sync_auto_task_catalog;

/// The actor a migration's task history event names.
pub(crate) const TOOL_NAMES_MIGRATION_ACTOR: &str = "system:tool-names-migration";

/// The ledger, under a root's `state/` directory, that records each file
/// rewrite: one JSON object per line.
pub(crate) const TOOL_NAME_MIGRATION_LEDGER: &str = "tool-name-migrations.jsonl";

/// The marker, under a root's `state/` directory, that records the upgrade
/// pass completed for that root.
const TOOL_NAME_MIGRATION_MARKER: &str = "tool-family-names-v1.migrated";

/// Revisit plugin pairs the old pass could incorrectly mark complete.
const PLUGIN_TOOL_NAME_MIGRATION_MARKER: &str = "plugin-tool-family-names-v2.migrated";

/// How many task envelopes the upgrade pass reads at a time.
const TASK_ENVELOPE_BATCH: usize = 256;

/// One stored record whose pre-family tool names were rewritten.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolNameRewrite {
    /// `auto_task` or `plugin`.
    pub kind: &'static str,
    /// The definition or plugin name.
    pub name: String,
    pub path: PathBuf,
    pub before: Vec<String>,
    pub after: Vec<String>,
}

impl ToolNameRewrite {
    /// One line for a sync report or a warning.
    pub fn describe(&self) -> String {
        format!(
            "pre-family tool names rewritten to family actions: {} → {}",
            self.before.join(", "),
            self.after.join(", ")
        )
    }
}
