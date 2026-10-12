//! Real runtime persistence of flaky signatures and task history.

use orbit_core::OrbitRuntime;
use orbit_engine::RuntimeHost;
use orbit_store::contracts::FrictionListFilter;
use orbit_store::friction_store::FrictionStore;
use orbit_types::workflow::{FLAKY_VALIDATION_EVENT, FlakyValidation};
use serde_json::json;
use tempfile::TempDir;

#[test]
fn flaky_signatures_are_filed_once_across_runs_and_tasks() {
    let root = TempDir::new().unwrap();
    let repo = root.path().join("repo");
    let workspace = repo.join(".orbit");
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&root.path().join("global"), &workspace).unwrap();
    let mut record = FlakyValidation {
        command: "make ci-test-affected".into(),
        base_sha: "a".repeat(40),
        tested_head: "b".repeat(40),
        run_id: "jrun-first".into(),
        failures: ["test suite::flaky".into()].into(),
        selection: json!({"packages": ["orbit-engine"], "failing_tests": ["suite::flaky"]}),
        original_exit_code: 2,
        original_output: "test suite::flaky ... FAILED".into(),
        base_output: "1 passed".into(),
        rerun_output: "1 passed".into(),
        rerun_tests_run: 1,
        validation_env: json!({"source": "login_shell"}),
    };
    for index in 0..2 {
        let task = runtime.run_tool("orbit.task.add", json!({
            "title": format!("Flaky replay task {index}"), "description": "Fixture for validation history.",
            "acceptance_criteria": ["Captured replay is durable."], "type": "feature", "model": "codex",
            "workspace": repo, "complexity": "low",
        })).unwrap();
        let id = task["id"].as_str().unwrap();
        record.run_id = format!("jrun-{index}");
        for _ in 0..2 {
            RuntimeHost::record_flaky_validation(&runtime, id, &record).unwrap();
        }
        let history = runtime.get_task_history(id).unwrap();
        let observed = history
            .iter()
            .filter(|entry| entry.event == FLAKY_VALIDATION_EVENT)
            .map(|entry| FlakyValidation::from_text(entry.note.as_deref().unwrap()).unwrap())
            .collect::<Vec<_>>();
        assert!(!observed.is_empty());
        assert!(observed.iter().all(|entry| entry == &record));
    }
    let store = FrictionStore::open(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
        runtime.data_root().join("frictions"),
    )
    .unwrap();
    let filed = store.list(&FrictionListFilter::default()).unwrap();
    assert_eq!(
        filed.len(),
        1,
        "one command/test signature across tasks and repeated reports"
    );
    assert!(filed[0].record.body.contains(&record.command));
    assert!(filed[0].record.body.contains("suite::flaky"));
}
