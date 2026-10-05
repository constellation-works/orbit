use crate::application::job::pipeline::run_definition_snapshot_path;

use crate::OrbitRuntime;

#[cfg(unix)]
fn test_runtime() -> (tempfile::TempDir, OrbitRuntime) {
    let root = tempfile::tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let workspace_root = root.path().join("repo").join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime)
}

const SNAPSHOT_YAML: &str = "schemaVersion: 2\nkind: Job\nmetadata:\n  name: snapshot_fixture\nspec:\n  state: enabled\n  kind: workflow\n  steps:\n    - id: pinned_step\n      spec:\n        type: deterministic\n        action: sleep\n        config: {}\n";

#[cfg(unix)]
#[test]
fn definition_snapshot_refuses_external_internal_and_dangling_symlinks() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &definition_snapshot_refuses_external_internal_and_dangling_symlinks,
    )) {
        return;
    }
    let (root, runtime) = test_runtime();
    let dir = &runtime.paths().job_runs_dir;
    std::fs::create_dir_all(dir).unwrap();
    let outside = root.path().join("outside.yaml");
    let inside = dir.join("other.job.yaml");
    let missing = root.path().join("absent.yaml");
    std::fs::write(&outside, SNAPSHOT_YAML).unwrap();
    std::fs::write(&inside, SNAPSHOT_YAML).unwrap();
    let run_id = "snapshot_link";
    let path = run_definition_snapshot_path(dir, run_id).unwrap();

    for target in [&outside, &inside, &missing] {
        std::os::unix::fs::symlink(target, &path).unwrap();
        runtime
            .read_run_definition_snapshot(run_id)
            .expect_err("a snapshot symlink must never supply a definition or catalog fallback");
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        std::fs::remove_file(&path).unwrap();
    }
    assert_eq!(std::fs::read_to_string(outside).unwrap(), SNAPSHOT_YAML);
    assert_eq!(std::fs::read_to_string(inside).unwrap(), SNAPSHOT_YAML);
    assert!(!missing.exists());
}
