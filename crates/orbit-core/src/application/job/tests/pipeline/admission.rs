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

/// Audit failure injection needs the detached-worker seam and crate-private
/// child/automation entry points (unit-test admission criterion 2).
#[cfg(unix)]
#[test]
fn audit_failure_preserves_admitted_runs_and_submission_errors() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &audit_failure_preserves_admitted_runs_and_submission_errors,
    )) {
        return;
    }
    use chrono::Utc;
    use orbit_common::OrbitError;
    use orbit_types::workflow::{JobRunState, JobRunTrigger, PipelineState};
    use serde_json::json;

    use crate::application::job::pipeline::{ChildPipelineAdmission, ChildSubmission};
    use crate::application::job::tests::SubmissionAuditFault;

    for entry in ["catalog", "direct", "automation", "child", "resume"] {
        for enabled in [true, false] {
            let fixture = SubmissionAuditFault::new("audit_fixture", enabled);
            let runtime = &fixture.runtime;
            let mut parent_run_id = None;
            let result = fixture.capture(|| match entry {
                "catalog" => {
                    runtime.submit_pipeline_run("audit_fixture", json!({}), None, Some("operator"))
                }
                "direct" => runtime.submit_job_run(
                    fixture.job_path.to_str().unwrap(),
                    json!({}),
                    Some("operator"),
                ),
                "automation" => runtime.submit_automation_pipeline_run(
                    "audit_fixture",
                    json!({}),
                    "audit-retry",
                    JobRunTrigger::cli(),
                ),
                "child" => {
                    let parent = runtime
                        .stores()
                        .jobs()
                        .insert_job_run(
                            "workspace_auto_pipeline",
                            1,
                            Utc::now(),
                            Some(json!({})),
                            None,
                        )
                        .unwrap();
                    runtime
                        .stores()
                        .jobs()
                        .mark_job_run_running(&parent.run_id, Utc::now(), std::process::id())
                        .unwrap();
                    runtime
                        .write_run_state(
                            &parent.run_id,
                            &PipelineState::new(parent.run_id.clone(), parent.job_id, json!({})),
                        )
                        .unwrap();
                    parent_run_id = Some(parent.run_id.clone());
                    runtime
                        .submit_child_pipeline_run(
                            "audit_fixture",
                            json!({}),
                            None,
                            Some("operator"),
                            &ChildPipelineAdmission {
                                parent_run_id: parent.run_id,
                                parent_step_id: Some("dispatch".to_string()),
                                action: "invoke_and_wait".to_string(),
                                blocking: true,
                            },
                        )
                        .map(|child| match child {
                            ChildSubmission::Submitted(run) => run,
                            other => panic!("a live parent must admit its child: {other:?}"),
                        })
                }
                "resume" => {
                    let source = runtime
                        .stores()
                        .jobs()
                        .insert_job_run("audit_fixture", 1, Utc::now(), Some(json!({})), None)
                        .unwrap();
                    runtime
                        .stores()
                        .jobs()
                        .mark_job_run_running(&source.run_id, Utc::now(), std::process::id())
                        .unwrap();
                    runtime
                        .stores()
                        .jobs()
                        .finalize_job_run(&source.run_id, JobRunState::Failed, Utc::now(), Some(0))
                        .unwrap();
                    runtime.submit_resume_run(&source.run_id, Some("operator"), None)
                }
                _ => unreachable!(),
            });
            if enabled {
                let result = result.unwrap_or_else(|error| {
                    panic!("{entry}: an audit failure must not hide the admitted run: {error}")
                });
                let run = runtime
                    .stores()
                    .jobs()
                    .get_job_run(&result.run_id)
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    run.job_id, "audit_fixture",
                    "{entry}: return the persisted run"
                );
                if let Some(parent) = parent_run_id {
                    let state = runtime.read_run_state(&parent).unwrap().unwrap();
                    assert!(
                        state
                            .child_dispatches
                            .iter()
                            .any(|child| child.child_run_id == result.run_id),
                        "a child dispatch must return the run already recorded by its parent"
                    );
                }
                fixture.assert_warning(Some(&result.run_id));
            } else {
                assert!(
                    matches!(result, Err(OrbitError::InvalidInput(_))),
                    "{entry}: preserve the original submission error: {result:?}"
                );
                fixture.assert_warning(None);
            }
        }
    }
}
