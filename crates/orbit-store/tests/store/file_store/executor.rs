use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::compose::global_executor_def_store;
use orbit_types::workflow::ExecutorDef;
use orbit_types::workflow::ExecutorType;
use std::collections::HashMap;
use tempfile::tempdir;

fn baseline_def(name: &str) -> ExecutorDef {
    let now = Utc::now();
    ExecutorDef {
        name: name.to_string(),
        executor_type: ExecutorType::DirectAgent,
        command: Some(name.to_string()),
        args: vec!["--flag".to_string()],
        stdout_format: None,
        model_pair_override: None,
        model_flag: None,
        timeout_seconds: None,
        auth_probe: None,
        env: HashMap::new(),
        sandbox: None,
        allow_fallback: false,
        created_at: Some(now),
        updated_at: Some(now),
    }
}

#[test]
fn rejects_traversal_executor_name_without_external_write() {
    if !crate::support::isolated("executor::rejects_traversal_executor_name_without_external_write")
    {
        return;
    }
    let dir = tempdir().expect("tempdir");
    let store = global_executor_def_store(dir.path().join("executors"));

    let err = store
        .upsert_executor_def(&baseline_def("../x"))
        .expect_err("traversal name must fail");
    assert!(matches!(err, OrbitError::InvalidInput(_)));
    assert!(!dir.path().join("x.yaml").exists());

    let err = store
        .get_executor_def("../x")
        .expect_err("traversal lookup must fail");
    assert!(matches!(err, OrbitError::InvalidInput(_)));
}

#[test]
fn rejects_traversal_executor_metadata_name_when_loading() {
    if !crate::support::isolated("executor::rejects_traversal_executor_metadata_name_when_loading")
    {
        return;
    }
    let dir = tempdir().expect("tempdir");
    let executors_dir = dir.path().join("executors");
    std::fs::create_dir_all(&executors_dir).expect("mkdir");
    std::fs::write(
            executors_dir.join("bad.yaml"),
            "schemaVersion: 2\nkind: Executor\nmetadata:\n  name: ../x\nspec:\n  executor_type: direct_agent\n",
        )
        .expect("seed");

    let store = global_executor_def_store(executors_dir);
    let err = store
        .list_executor_defs()
        .expect_err("traversal metadata name must fail");
    assert!(matches!(err, OrbitError::InvalidInput(_)));
    assert!(!dir.path().join("x.yaml").exists());
}
