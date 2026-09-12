use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use orbit_types::task::{TaskPriority, TaskStatus};

#[derive(Debug, Clone)]
pub struct BindWorkspaceParams {
    /// Task-store partition id to bind the checkout to, or `None` to mint one.
    /// A distinct namespace from the workspace-registry id, even when a caller
    /// passes a `ws_*` id in; see `task_workspaces_dir`.
    pub partition_id: Option<String>,
    pub slug: String,
    pub repo_root: PathBuf,
    pub workspace_path: PathBuf,
    pub orbit_dir: PathBuf,
    pub repo_fingerprint: Option<String>,
}

/// Path-free coordination record for a logical workspace.
#[derive(Debug, Clone)]
pub struct RegisterWorkspaceParams {
    /// Task-store partition the imported workspace's bundles live in.
    pub partition_id: String,
    pub slug: String,
    pub repo_fingerprint: Option<String>,
}

/// Logical task-registry row for one task-store partition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceBinding {
    /// Names the partition directory under `tasks/workspaces/`, not a
    /// workspace-registry row.
    pub partition_id: String,
    pub slug: String,
    pub repo_fingerprint: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Machine-local checkout attached to a logical workspace, when one exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceCheckoutBinding {
    /// Partition this checkout's task bundles live in.
    pub partition_id: String,
    pub repo_root: PathBuf,
    pub workspace_path: PathBuf,
    pub orbit_dir: PathBuf,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskBundleBinding {
    pub task_id: String,
    pub partition_id: String,
    pub canonical_path: PathBuf,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskIndexFilter {
    pub status: Option<TaskStatus>,
    pub priority: Option<TaskPriority>,
    pub job_run_id: Option<String>,
    pub tags: Vec<String>,
}

/// A relation edge whose target uses a locally known task prefix but does not
/// resolve to any registered task bundle in the coordination registry — the
/// exact condition
/// the registry rejects at index-rebuild time. These are the "grandfathered" relations that
/// make an index rebuild fail its validator and fall back to a full bundle
/// scan (see ORB-10305).
///
/// `relation_type` carries the canonical snake_case name as stored in the
/// registry (`related_to`, `blocked_by`, …); non-task artifact targets and
/// foreign-prefix task references that legitimately remain unresolved are
/// never reported here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DanglingRelationTarget {
    pub partition_id: String,
    pub source_task_id: String,
    pub relation_type: String,
    pub target_task_id: String,
}

/// Outcome of seeding the task-id allocator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllocatorSeedOutcome {
    /// Counter value before the seed.
    pub previous: u32,
    /// Counter value after the seed (the id the next allocation will hand out).
    pub next: u32,
    /// Whether the seed changed the counter (`false` when it already matched).
    pub changed: bool,
}

/// Status distribution for one complexity bucket, including the explicit
/// [`orbit_types::task::UNSET_BUCKET`] for tasks with no assessed complexity
/// (unindexed, empty, or `unassessed` — see
/// [`orbit_types::task::complexity_bucket`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCompletionByComplexity {
    pub complexity: String,
    pub total: i64,
    pub by_status: BTreeMap<String, i64>,
}
