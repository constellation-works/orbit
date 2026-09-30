use crate::application::job::pipeline::run_definition_snapshot_path;

use super::super::exec::test_runtime;

const SNAPSHOT_YAML: &str = "schemaVersion: 2\nkind: Job\nmetadata:\n  name: snapshot_fixture\nspec:\n  state: enabled\n  kind: workflow\n  steps:\n    - id: pinned_step\n      spec:\n        type: deterministic\n        action: sleep\n        config: {}\n";

#[test]
fn definition_snapshot_reads_regular_files_and_only_absent_files_are_missing() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &definition_snapshot_reads_regular_files_and_only_absent_files_are_missing,
    )) {
        return;
    }
    let (_root, runtime, _repo, _global) = test_runtime();
    let run_id = "snapshot_fixture";
    assert!(
        runtime
            .read_run_definition_snapshot(run_id)
            .unwrap()
            .is_none()
    );
    let path = run_definition_snapshot_path(&runtime.paths().job_runs_dir, run_id).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, SNAPSHOT_YAML).unwrap();

    let (spec, yaml) = runtime
        .read_run_definition_snapshot(run_id)
        .unwrap()
        .unwrap();
    assert_eq!(spec.steps[0].id, "pinned_step");
    assert_eq!(yaml, SNAPSHOT_YAML);

    std::fs::write(&path, "invalid yaml").unwrap();
    runtime
        .read_run_definition_snapshot(run_id)
        .expect_err("a malformed snapshot must not fall back to the catalog");
}

#[test]
fn definition_snapshot_refuses_a_directory_instead_of_using_the_catalog() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &definition_snapshot_refuses_a_directory_instead_of_using_the_catalog,
    )) {
        return;
    }
    let (_root, runtime, _repo, _global) = test_runtime();
    let run_id = "snapshot_directory";
    let path = run_definition_snapshot_path(&runtime.paths().job_runs_dir, run_id).unwrap();
    std::fs::create_dir_all(&path).unwrap();

    runtime
        .read_run_definition_snapshot(run_id)
        .expect_err("a directory at the snapshot path is not a missing definition");
    assert!(path.is_dir());
}

#[cfg(unix)]
#[test]
fn definition_snapshot_refuses_external_internal_and_dangling_symlinks() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &definition_snapshot_refuses_external_internal_and_dangling_symlinks,
    )) {
        return;
    }
    let (root, runtime, _repo, _global) = test_runtime();
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

#[test]
fn definition_snapshot_refuses_run_ids_with_path_syntax() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &definition_snapshot_refuses_run_ids_with_path_syntax,
    )) {
        return;
    }
    let (_root, runtime, _repo, _global) = test_runtime();
    for run_id in [
        "",
        ".",
        "..",
        "../outside",
        "/absolute",
        "nested/file",
        "nested\\file",
    ] {
        runtime
            .read_run_definition_snapshot(run_id)
            .expect_err("an invalid run ID must be refused before filesystem access");
    }
}
