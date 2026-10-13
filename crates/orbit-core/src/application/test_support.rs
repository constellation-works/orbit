use std::collections::HashMap;
use std::path::PathBuf;

use chrono::Utc;
use orbit_types::workflow::{ExecutorDef, ExecutorSandboxKind, ExecutorType};
use tempfile::tempdir;

use crate::OrbitRuntime;

pub(crate) fn seed_executor(
    runtime: &OrbitRuntime,
    name: &str,
    sandbox: Option<ExecutorSandboxKind>,
) {
    let now = Utc::now();
    runtime
        .upsert_executor_def(&ExecutorDef {
            name: name.to_string(),
            executor_type: ExecutorType::DirectAgent,
            command: Some(name.to_string()),
            args: vec!["exec".to_string(), "--json".to_string()],
            stdout_format: None,
            model_pair_override: None,
            model_flag: None,
            timeout_seconds: None,
            auth_probe: None,
            env: HashMap::new(),
            sandbox,
            allow_fallback: false,
            created_at: Some(now),
            updated_at: Some(now),
        })
        .expect("seed executor");
}

pub(crate) fn runtime_with_workspace_layout() -> (tempfile::TempDir, OrbitRuntime, PathBuf) {
    runtime_with_workspace_config(None)
}

/// The same layout with an optional workspace `config.toml` written before the
/// runtime opens — the only way to give a fixture a `[crews.*]` registry, which
/// resolved configuration reads once at open.
pub(crate) fn runtime_with_workspace_config(
    config_toml: Option<&str>,
) -> (tempfile::TempDir, OrbitRuntime, PathBuf) {
    let root = tempdir().expect("create tempdir");
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).expect("global orbit dir");
    std::fs::create_dir_all(&workspace).expect("workspace orbit dir");
    if let Some(config_toml) = config_toml {
        std::fs::write(workspace.join("config.toml"), config_toml).expect("write workspace config");
    }
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("build runtime");
    let repo_root = root.path().join("repo");
    (root, runtime, repo_root)
}
