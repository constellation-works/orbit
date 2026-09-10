use std::path::{Path, PathBuf};

use orbit_types::workspace::WorkspacePaths;
use serde_json::{Value, json};

/// Holds the resolved paths for all persistent artifact stores.
///
/// These are derived from the two roots, never from the config document, so a
/// `config.toml` cannot relocate a store.
///
/// - Skills: workspace override directory layered over global defaults
/// - Activities/Jobs/Executors/Policies: global only
/// - Audit: global only (single SQLite database)
#[derive(Debug, Clone)]
pub struct PersistenceConfig {
    /// Global activity definitions.
    pub activity_dir: PathBuf,
    /// Global job definitions.
    pub job_dir: PathBuf,
    /// Skill directory (workspace overrides layered over global).
    pub skill_dir: PathBuf,
    /// Global executor definitions.
    pub executor_dir: PathBuf,
    /// Single global audit database.
    pub audit_db: PathBuf,
    /// Workspace semantic index database.
    pub semantic_db: PathBuf,
    /// Global policy definitions.
    pub policy_dir: PathBuf,
}

impl PersistenceConfig {
    /// Defaults for a single root used as both layers.
    pub fn default_for_data_root(data_root: &Path) -> Self {
        Self::default_for_roots(data_root, data_root)
    }

    /// Two-root defaults (raw paths), resolved through `WorkspacePaths`.
    pub fn default_for_roots(global_root: &Path, workspace_root: &Path) -> Self {
        let repo_root = workspace_root
            .parent()
            .unwrap_or(workspace_root)
            .to_path_buf();
        let paths = WorkspacePaths::new(
            repo_root,
            workspace_root.to_path_buf(),
            global_root.to_path_buf(),
        );
        Self::from_workspace_paths(&paths)
    }

    /// Build persistence config from [`WorkspacePaths`]. This is the single
    /// source of truth for artifact path resolution.
    fn from_workspace_paths(paths: &WorkspacePaths) -> Self {
        let global_resources_dir = paths.global_dir.join("resources");

        Self {
            activity_dir: global_resources_dir.join("activities"),
            job_dir: global_resources_dir.join("jobs"),
            skill_dir: paths.skills_dir.clone(),
            executor_dir: global_resources_dir.join("executors"),
            policy_dir: global_resources_dir.join("policies"),
            audit_db: paths.global_dir.join("orbit.db"),
            semantic_db: paths.state_dir.join("semantic.db"),
        }
    }

    /// JSON projection of every resolved store path, for `orbit doctor` and
    /// runtime diagnostics.
    pub fn as_json_value(&self) -> Value {
        json!({
            "activity": { "path": self.activity_dir.to_string_lossy() },
            "job": { "path": self.job_dir.to_string_lossy() },
            "skill": { "path": self.skill_dir.to_string_lossy() },
            "executor": { "path": self.executor_dir.to_string_lossy() },
            "policy": { "path": self.policy_dir.to_string_lossy() },
            "audit": { "path": self.audit_db.to_string_lossy() },
            "semantic": { "path": self.semantic_db.to_string_lossy() },
        })
    }
}
