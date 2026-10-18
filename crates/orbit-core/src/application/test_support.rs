use std::collections::HashMap;
use std::path::PathBuf;

use chrono::Utc;
use orbit_store::contracts::{
    AdmissionReceipt, AdmissionRequest, AdmissionRunContext, AdmissionShipContract,
    AdmissionTaskSummary, DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA, ExecutionClaim,
};
use orbit_types::workflow::{ExecutorDef, ExecutorSandboxKind, ExecutorType};
use serde_json::json;
use tempfile::tempdir;

use crate::OrbitRuntime;

/// Supply required wire fields and let serde default unrelated capabilities.
/// Callers keep their review settings explicit with struct update syntax.
pub(crate) fn admission_request(
    request_id: String,
    run_context: AdmissionRunContext,
    ship: AdmissionShipContract,
) -> AdmissionRequest {
    serde_json::from_value(json!({
        "request_id": request_id,
        "caller_version": "fixture",
        "caller_schema": DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
        "run_context": run_context,
        "ship": ship,
    }))
    .expect("admission request fixture matches the wire contract")
}

/// A claimed receipt with no diagnostics; optional capability/evidence fields
/// use their wire defaults. Claim identity, phase and task stay with the caller.
pub(crate) fn admission_receipt(
    request: AdmissionRequest,
    machine_id: String,
    claim: ExecutionClaim,
    task: AdmissionTaskSummary,
) -> AdmissionReceipt {
    serde_json::from_value(json!({
        "schema_version": 1,
        "request": request,
        "machine_id": machine_id,
        "claim": claim,
        "task": task,
        "invalid_candidates": [],
        "deferred_conflicts": [],
        "queue_depth": 0,
    }))
    .expect("admission receipt fixture matches the wire contract")
}

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
            host_os: None,
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
