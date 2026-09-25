//! Linked-worktree workspace resolution and the provider boundary guard.

use std::path::PathBuf;
use std::sync::Arc;

use super::audit_writer::V2AuditWriter;
use super::dispatcher::DispatchError;

mod boundary_guard;
mod cwd;
mod declared_pair;
pub(crate) mod fingerprint;
mod rebase_recovery;
mod recovery;

pub(crate) use cwd::canonicalize_dir;
pub use cwd::resolve_subprocess_cwd;
pub(crate) use declared_pair::validate_declared_worktree_pair;

use fingerprint::GitWorktreeFingerprint;
use rebase_recovery::RebaseRecoveryCheckpoint;

/// A pipeline-declared linked-worktree assignment that has been validated
/// against the runtime's registered primary checkout before provider setup.
#[derive(Debug, Clone)]
pub(crate) struct DeclaredWorktreePair {
    requested_workspace_path: String,
    requested_repo_root: String,
    assigned_root: PathBuf,
    primary_root: PathBuf,
}

/// Pre-spawn boundary guard for a linked-worktree provider invocation.
///
/// The registered primary checkout comes from the runtime's tool context; the
/// assigned checkout comes from the rendered `workspace_path`. The guard is
/// enabled only for a validated declared worktree pair. A direct invocation
/// may bypass the guard only when its cwd and registered root are the same
/// checkout; distinct roots without the pair fail before spawn.
pub(crate) struct WorktreeBoundaryGuard {
    task_id: String,
    run_id: String,
    provider: String,
    requested_workspace_path: String,
    requested_repo_root: Option<String>,
    assigned_root: PathBuf,
    primary_root: PathBuf,
    assigned_before: GitWorktreeFingerprint,
    primary_before: GitWorktreeFingerprint,
    rebase_recovery: Option<RebaseRecoveryCheckpoint>,
    /// Sink for the full fingerprint evidence a violation would otherwise have
    /// to inline into its error string. Absent only where no run audit exists.
    audit: Option<Arc<V2AuditWriter>>,
}
