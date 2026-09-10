use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::publication::WorkspacePublicationBinding;

/// Current on-disk schema version for `~/.orbit/workspaces.json`.
pub const WORKSPACE_REGISTRY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStatus {
    Active,
    Invalid,
}

impl fmt::Display for WorkspaceStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WorkspaceStatus::Active => write!(f, "active"),
            WorkspaceStatus::Invalid => write!(f, "invalid"),
        }
    }
}

impl FromStr for WorkspaceStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "active" => Ok(WorkspaceStatus::Active),
            "invalid" => Ok(WorkspaceStatus::Invalid),
            other => Err(format!("unknown workspace status: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    /// Stable owner identity. Standalone registries created before host
    /// identity existed may omit this; hub and spoke registries may not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_machine_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_remote: Option<String>,
    /// Explicit ship-pipeline mode for this workspace: `"pr"` or `"local"`.
    /// When unset, the effective mode is `"pr"` (see
    /// `orbit_core::resolved_ship_mode`). Stored on the registry entry
    /// rather than `config.toml` because per-workspace `[workflow]` config
    /// is stripped by task-mutation commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ship_mode: Option<String>,
    #[serde(default = "default_base_branch")]
    pub base_branch: String,
    #[serde(default = "default_status")]
    pub status: WorkspaceStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn default_base_branch() -> String {
    "main".to_string()
}

fn default_status() -> WorkspaceStatus {
    WorkspaceStatus::Active
}

/// This machine's role for a local checkout of a logical workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceCheckoutRole {
    /// This machine owns the canonical checkout.
    Owner,
    /// This checkout is a replica of a workspace owned by another machine.
    Replica,
}

impl WorkspaceCheckoutRole {
    /// The persisted lowercase string form.
    pub fn as_str(self) -> &'static str {
        match self {
            WorkspaceCheckoutRole::Owner => "owner",
            WorkspaceCheckoutRole::Replica => "replica",
        }
    }
}

impl std::fmt::Display for WorkspaceCheckoutRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Machine-local filesystem binding for a logical [`Workspace`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceCheckout {
    pub workspace_id: String,
    pub repo_root: PathBuf,
    pub orbit_dir: PathBuf,
    /// Missing only while loading a legacy/partially migrated standalone
    /// registry; registry validation canonicalizes it to `owner`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<WorkspaceCheckoutRole>,
    /// Required only for replicas and always names the stable owner machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_machine_id: Option<String>,
    /// Additional roots (for example linked worktrees) that resolve to this
    /// local checkout. These never appear on the logical workspace record.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path_overrides: Vec<PathBuf>,
}

impl WorkspaceCheckout {
    pub fn owner(workspace_id: String, repo_root: PathBuf, orbit_dir: PathBuf) -> Self {
        Self {
            workspace_id,
            repo_root,
            orbit_dir,
            role: Some(WorkspaceCheckoutRole::Owner),
            owner_machine_id: None,
            path_overrides: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRegistry {
    pub schema_version: u32,
    /// Human host names known through this machine's local workspace records,
    /// keyed by stable owner machine id. This is not a fleet inventory: an
    /// entry exists only for an owner named by a local workspace record.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub owner_host_ids: BTreeMap<String, String>,
    #[serde(default)]
    pub workspaces: Vec<Workspace>,
    #[serde(default)]
    pub checkouts: Vec<WorkspaceCheckout>,
    /// Owner-local bindings to dedicated task-publication repositories.
    /// Absent from registries that have never bound a publication remote.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publication_bindings: Vec<WorkspacePublicationBinding>,
}

impl Default for WorkspaceRegistry {
    fn default() -> Self {
        Self {
            schema_version: WORKSPACE_REGISTRY_SCHEMA_VERSION,
            owner_host_ids: BTreeMap::new(),
            workspaces: Vec::new(),
            checkouts: Vec::new(),
            publication_bindings: Vec::new(),
        }
    }
}

/// Derived directory layout for a workspace.
///
/// All sub-paths are derived from `orbit_dir` in the constructor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePaths {
    pub repo_root: PathBuf,
    pub orbit_dir: PathBuf,
    pub local_dir: PathBuf,
    pub global_dir: PathBuf,
    pub resources_dir: PathBuf,
    pub state_dir: PathBuf,
    pub knowledge_dir: PathBuf,
    pub activities_dir: PathBuf,
    pub jobs_dir: PathBuf,
    pub skills_dir: PathBuf,
    pub executors_dir: PathBuf,
    pub policies_dir: PathBuf,
    pub audit_dir: PathBuf,
    pub job_runs_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub scoreboard_dir: PathBuf,
    pub diagnostics_dir: PathBuf,
    pub worktrees_dir: PathBuf,
}

impl WorkspacePaths {
    pub fn new(repo_root: PathBuf, orbit_dir: PathBuf, global_dir: PathBuf) -> Self {
        Self::new_with_local(repo_root, orbit_dir.clone(), orbit_dir, global_dir)
    }

    pub fn new_with_local(
        repo_root: PathBuf,
        orbit_dir: PathBuf,
        local_dir: PathBuf,
        global_dir: PathBuf,
    ) -> Self {
        let resources_dir = orbit_dir.join("resources");
        let state_dir = orbit_dir.join("state");
        Self {
            resources_dir: resources_dir.clone(),
            state_dir: state_dir.clone(),
            knowledge_dir: orbit_dir.join("knowledge"),
            activities_dir: resources_dir.join("activities"),
            jobs_dir: resources_dir.join("jobs"),
            skills_dir: resources_dir.join("skills"),
            executors_dir: resources_dir.join("executors"),
            policies_dir: resources_dir.join("policies"),
            audit_dir: state_dir.join("audit"),
            job_runs_dir: state_dir.join("job-runs"),
            logs_dir: state_dir.join("logs"),
            scoreboard_dir: state_dir.join("scoreboard"),
            diagnostics_dir: state_dir.join("diagnostics"),
            worktrees_dir: state_dir.join("worktrees"),
            repo_root,
            orbit_dir,
            local_dir,
            global_dir,
        }
    }
}
