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
    if !super::dispatch_admission::isolated(
        "flaky_validation::flaky_signatures_are_filed_once_across_runs_and_tasks",
    ) {
        return;
    }
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let repo = root.path().join("repo");
    let workspace = repo.join(".orbit");
    let global = root.path().join("global");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&global).unwrap();
    let mut git = std::process::Command::new("git");
    orbit_common::test_env::clear_inherited_authority(|key| {
        git.env_remove(key);
    });
    let boundary = git
        .args(["init", "--bare", "--initial-branch=main", "-q"])
        .arg(repo.join(".git"))
        .output()
        .unwrap();
    assert!(
        boundary.status.success(),
        "initialize fixture Git discovery boundary: {}",
        String::from_utf8_lossy(&boundary.stderr)
    );
    // Discovery must stop at this checkout even when TMPDIR is inside a
    // managed repository. Verify the task tool's workspace before any writes.
    let repo = repo.canonicalize().unwrap();
    let workspace = workspace.canonicalize().unwrap();
    let global = global.canonicalize().unwrap();
    let roots = OrbitRuntime::resolve_roots_for_cwd(&repo, None).unwrap();
    assert_eq!(roots.shared_root, workspace);
    assert_eq!(roots.local_root, workspace);
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    assert_eq!(runtime.paths().repo_root, repo);
    assert_eq!(runtime.paths().orbit_dir, workspace);
    assert_eq!(runtime.global_root(), global);
    assert_eq!(runtime.data_root(), workspace);
    assert_eq!(runtime.local_root(), workspace);
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
