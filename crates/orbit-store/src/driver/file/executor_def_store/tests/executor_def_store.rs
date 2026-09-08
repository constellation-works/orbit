// Migrated from file/executor_def_store.rs per ORB-00231
use super::super::*;
use chrono::Utc;
use orbit_types::workflow::{ExecutorSandboxKind, ExecutorType, ModelPairOverride};
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
        env: HashMap::new(),
        sandbox: None,
        allow_fallback: false,
        created_at: Some(now),
        updated_at: Some(now),
    }
}

#[test]
fn roundtrips_sandbox_and_allow_fallback_fields() {
    let dir = tempdir().expect("tempdir");
    let store = ExecutorDefFileStore::new(dir.path().to_path_buf());

    let mut def = baseline_def("claude");
    def.sandbox = Some(ExecutorSandboxKind::MacosSandboxExec);
    def.allow_fallback = true;
    store.upsert_executor_def(&def).expect("upsert");

    let loaded = store
        .get_executor_def("claude")
        .expect("get")
        .expect("present");
    assert_eq!(loaded.name, "claude");
    assert_eq!(loaded.sandbox, Some(ExecutorSandboxKind::MacosSandboxExec));
    assert!(loaded.allow_fallback);
}

#[test]
fn roundtrips_external_executor_type() {
    let dir = tempdir().expect("tempdir");
    let store = ExecutorDefFileStore::new(dir.path().to_path_buf());

    let mut def = baseline_def("acme-harness");
    def.executor_type = ExecutorType::External;
    store.upsert_executor_def(&def).expect("upsert");

    let loaded = store
        .get_executor_def("acme-harness")
        .expect("get")
        .expect("present");
    assert_eq!(loaded.executor_type, ExecutorType::External);
    assert_eq!(loaded.command.as_deref(), Some("acme-harness"));

    let on_disk =
        std::fs::read_to_string(dir.path().join("acme-harness.yaml")).expect("read on-disk def");
    assert!(
        on_disk.contains("executor_type: external"),
        "external executor_type should persist to disk: {on_disk}"
    );
}

#[test]
fn roundtrips_model_flag_field() {
    let dir = tempdir().expect("tempdir");
    let store = ExecutorDefFileStore::new(dir.path().to_path_buf());

    let mut def = baseline_def("gemini");
    def.model_flag = Some("-m".to_string());
    store.upsert_executor_def(&def).expect("upsert");

    let loaded = store
        .get_executor_def("gemini")
        .expect("get")
        .expect("present");
    assert_eq!(loaded.model_flag.as_deref(), Some("-m"));

    let on_disk = std::fs::read_to_string(dir.path().join("gemini.yaml")).expect("read");
    assert!(
        on_disk.contains("model_flag: -m"),
        "model_flag should be persisted: {on_disk}"
    );
}

#[test]
fn roundtrips_legacy_model_pair_override_field() {
    let dir = tempdir().expect("tempdir");
    let store = ExecutorDefFileStore::new(dir.path().to_path_buf());

    let mut def = baseline_def("claude");
    def.model_pair_override = Some(ModelPairOverride {
        strong: "custom-strong".to_string(),
        weak: "custom-weak".to_string(),
    });
    store.upsert_executor_def(&def).expect("upsert");

    let loaded = store
        .get_executor_def("claude")
        .expect("get")
        .expect("present");
    assert_eq!(loaded.model_pair_override, def.model_pair_override);

    let on_disk = std::fs::read_to_string(dir.path().join("claude.yaml")).expect("read");
    assert!(
        on_disk.contains("model_pair_override:"),
        "legacy override should remain persisted for customized executors: {on_disk}"
    );
}

#[test]
fn omits_sandbox_fields_when_default() {
    let dir = tempdir().expect("tempdir");
    let store = ExecutorDefFileStore::new(dir.path().to_path_buf());

    let def = baseline_def("codex");
    store.upsert_executor_def(&def).expect("upsert");

    let on_disk = std::fs::read_to_string(dir.path().join("codex.yaml")).expect("read");
    assert!(
        !on_disk.contains("sandbox"),
        "sandbox should be omitted when None: {on_disk}"
    );
    assert!(
        !on_disk.contains("allow_fallback"),
        "allow_fallback should be omitted when false: {on_disk}"
    );
}

#[test]
fn loads_executor_yaml_with_explicit_sandbox_kind() {
    let dir = tempdir().expect("tempdir");
    let yaml = "schemaVersion: 2\nkind: Executor\nmetadata:\n  name: gemini\nspec:\n  executor_type: direct_agent\n  command: gemini\n  args: []\n  sandbox: macos-sandbox-exec\n  allow_fallback: true\n";
    std::fs::write(dir.path().join("gemini.yaml"), yaml).expect("seed");

    let store = ExecutorDefFileStore::new(dir.path().to_path_buf());
    let loaded = store
        .get_executor_def("gemini")
        .expect("get")
        .expect("present");
    assert_eq!(loaded.sandbox, Some(ExecutorSandboxKind::MacosSandboxExec));
    assert!(loaded.allow_fallback);
}

#[test]
fn rejects_traversal_executor_name_without_external_write() {
    let dir = tempdir().expect("tempdir");
    let store = ExecutorDefFileStore::new(dir.path().join("executors"));

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
    let dir = tempdir().expect("tempdir");
    let executors_dir = dir.path().join("executors");
    std::fs::create_dir_all(&executors_dir).expect("mkdir");
    std::fs::write(
            executors_dir.join("bad.yaml"),
            "schemaVersion: 2\nkind: Executor\nmetadata:\n  name: ../x\nspec:\n  executor_type: direct_agent\n",
        )
        .expect("seed");

    let store = ExecutorDefFileStore::new(executors_dir);
    let err = store
        .list_executor_defs()
        .expect_err("traversal metadata name must fail");
    assert!(matches!(err, OrbitError::InvalidInput(_)));
    assert!(!dir.path().join("x.yaml").exists());
}
