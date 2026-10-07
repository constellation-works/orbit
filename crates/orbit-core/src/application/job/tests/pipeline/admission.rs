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

/// SQLite busy-handler fault injection forces the competing checkpoint/control
/// commit at the trigger writer's lock acquisition (unit admission criterion 2).
#[cfg(unix)]
#[test]
fn trigger_recording_preserves_a_competing_checkpoint_and_drain_controls() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &trigger_recording_preserves_a_competing_checkpoint_and_drain_controls,
    )) {
        return;
    }
    use chrono::Utc;
    use orbit_types::workflow::{DrainAdmissionsStop, DrainCancelRequest, JobRunTrigger};
    use serde_json::json;
    use std::cell::RefCell;
    use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
    use std::time::Duration;

    type UnlockGate = (SyncSender<()>, Receiver<()>);
    thread_local! {
        static UNLOCK: RefCell<Option<UnlockGate>> = const { RefCell::new(None) };
    }

    let (_root, runtime) = test_runtime();
    let jobs = runtime.stores().jobs();
    let orbit_store::contracts::KeyedJobRunAdmission::Admitted(run) = jobs
        .insert_automation_job_run("trigger_fixture", json!({}), "action")
        .unwrap()
    else {
        panic!("fresh action must be admitted");
    };
    let mut expected = runtime.read_run_state(&run.run_id).unwrap().unwrap();
    expected.next_step_index = 1;
    expected.step_outputs.insert(0, json!({"checkpoint": true}));
    expected.drain_admissions_stop = Some(DrainAdmissionsStop {
        actor: "operator".into(),
        reason: Some("stop admissions".into()),
        stopped_at: Utc::now(),
    });
    expected.drain_cancel = Some(DrainCancelRequest {
        actor: "operator".into(),
        source: "fixture".into(),
        reason: None,
        requested_at: Utc::now(),
    });

    let store = runtime.sqlite_store().unwrap();
    let path = store
        .connection()
        .lock()
        .unwrap()
        .path()
        .unwrap()
        .to_owned();
    let mut competing = rusqlite::Connection::open(path).unwrap();
    let transaction = competing
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    transaction
        .execute(
            "UPDATE job_runs SET pipeline_state_json = ?1 WHERE workspace_id = ?2 AND run_id = ?3",
            rusqlite::params![
                serde_json::to_string(&expected).unwrap(),
                runtime.workspace_id().unwrap(),
                run.run_id
            ],
        )
        .unwrap();
    let (waiting_tx, waiting_rx) = sync_channel(1);
    let (committed_tx, committed_rx) = sync_channel(1);
    store
        .connection()
        .lock()
        .unwrap()
        .busy_handler(Some(|_| {
            UNLOCK.with(|gate| {
                let Some((waiting, committed)) = gate.borrow_mut().take() else {
                    return false;
                };
                waiting.send(()).unwrap();
                committed.recv_timeout(Duration::from_secs(5)).is_ok()
            })
        }))
        .unwrap();
    let worker_runtime = runtime.clone();
    let run_id = run.run_id.clone();
    let writer = std::thread::spawn(move || {
        UNLOCK.with(|gate| *gate.borrow_mut() = Some((waiting_tx, committed_rx)));
        worker_runtime.record_run_trigger(&run_id, &JobRunTrigger::cli())
    });
    waiting_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("trigger writer reached the competing transaction");
    transaction.commit().unwrap();
    committed_tx.send(()).unwrap();
    writer.join().unwrap().unwrap();
    store
        .connection()
        .lock()
        .unwrap()
        .busy_handler(None)
        .unwrap();

    expected.trigger = Some(JobRunTrigger::cli());
    assert_eq!(
        runtime.read_run_state(&run.run_id).unwrap(),
        Some(expected),
        "trigger recording must preserve checkpoints and ORB-11283 operator controls committed before its write"
    );
}

/// A rejecting SQLite trigger detects even an otherwise identical state write
/// through the crate-private automation entry point (unit criterion 2).
#[cfg(unix)]
#[test]
fn repeated_automation_admission_preserves_pending_running_and_terminal_state() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &repeated_automation_admission_preserves_pending_running_and_terminal_state,
    )) {
        return;
    }
    use chrono::Utc;
    use orbit_types::workflow::{JobRunState, JobRunTrigger};
    use serde_json::json;

    let (_root, runtime) = test_runtime();
    let jobs_dir = runtime.global_root().join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).unwrap();
    std::fs::write(jobs_dir.join("snapshot_fixture.yaml"), SNAPSHOT_YAML).unwrap();
    crate::test_support::install_substitute_pipeline_worker(["sh", "-c", "exit 0"]);
    let admitted = runtime
        .submit_automation_pipeline_run(
            "snapshot_fixture",
            json!({}),
            "action",
            JobRunTrigger::cli(),
        )
        .unwrap();
    assert_eq!(
        runtime
            .read_run_state(&admitted.run_id)
            .unwrap()
            .unwrap()
            .trigger,
        Some(JobRunTrigger::cli())
    );
    let jobs = runtime.stores().jobs();
    let orbit_store::contracts::KeyedJobRunAdmission::Admitted(run) = jobs
        .insert_automation_job_run("snapshot_fixture", json!({"guard": true}), "repeat-action")
        .unwrap()
    else {
        panic!("fresh action must be admitted");
    };
    // A live incumbent makes the harmless replacement workers benign
    // duplicate deliveries; they cannot race this fixture's lifecycle changes.
    jobs.claim_pending_job_run_owner(&run.run_id, std::process::id())
        .unwrap();
    let mut expected = runtime.read_run_state(&run.run_id).unwrap().unwrap();
    expected.trigger = Some(JobRunTrigger::cli());
    expected.next_step_index = 1;
    expected.step_outputs.insert(0, json!({"checkpoint": true}));
    runtime.write_run_state(&run.run_id, &expected).unwrap();

    for state in [
        JobRunState::Pending,
        JobRunState::Running,
        JobRunState::Success,
    ] {
        match state {
            JobRunState::Running => {
                jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
                    .unwrap();
            }
            JobRunState::Success => {
                jobs.finalize_job_run(&run.run_id, state, Utc::now(), None)
                    .unwrap();
            }
            _ => {}
        }
        runtime.sqlite_store().unwrap().connection().lock().unwrap().execute_batch(
            "CREATE TRIGGER refuse_pipeline_rewrite BEFORE UPDATE OF pipeline_state_json ON job_runs \
             WHEN json_extract(OLD.input_json, '$.guard') = 1 \
             BEGIN SELECT RAISE(ABORT, 'repeated admission rewrote pipeline state'); END;"
        ).unwrap();
        let repeated = runtime
            .submit_automation_pipeline_run(
                "snapshot_fixture",
                json!({"guard": true}),
                "repeat-action",
                JobRunTrigger::child(),
            )
            .unwrap();
        assert_eq!(repeated.run_id, run.run_id);
        assert_eq!(jobs.get_job_run(&run.run_id).unwrap().unwrap().state, state);
        assert_eq!(
            runtime.read_run_state(&run.run_id).unwrap(),
            Some(expected.clone()),
            "repeated {state:?} admission must preserve the original trigger and every checkpoint"
        );
        runtime
            .sqlite_store()
            .unwrap()
            .connection()
            .lock()
            .unwrap()
            .execute_batch("DROP TRIGGER refuse_pipeline_rewrite;")
            .unwrap();
    }
    assert_eq!(jobs.list_job_runs("snapshot_fixture").unwrap().len(), 2);
}

/// SQLite trigger fault injection fails the first pipeline-state write after
/// the run row is committed, on every post-insert submission surface
/// (unit admission criterion 2: fault injection at a crate-private seam).
/// A failed submission must leave no pending run without a worker.
#[cfg(unix)]
#[test]
fn failure_after_run_insert_terminalizes_the_run_with_a_startup_diagnostic() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &failure_after_run_insert_terminalizes_the_run_with_a_startup_diagnostic,
    )) {
        return;
    }
    use orbit_common::OrbitError;
    use orbit_types::workflow::{JobRunState, JobRunTrigger};
    use serde_json::json;

    let (_root, runtime) = test_runtime();
    let jobs_dir = runtime.global_root().join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).unwrap();
    let job_path = jobs_dir.join("snapshot_fixture.yaml");
    std::fs::write(&job_path, SNAPSHOT_YAML).unwrap();
    crate::test_support::install_substitute_pipeline_worker(["sh", "-c", "exit 0"]);
    runtime
        .sqlite_store()
        .unwrap()
        .connection()
        .lock()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER fail_post_insert_state BEFORE UPDATE OF pipeline_state_json ON job_runs \
             WHEN json_extract(OLD.input_json, '$.fail_after_insert') = 1 AND NEW.state = 'pending' \
             BEGIN SELECT RAISE(ABORT, 'injected post-insert failure'); END;",
        )
        .unwrap();
    let input = json!({"fail_after_insert": true});
    let jobs = runtime.stores().jobs();

    let assert_terminalized = |entry: &str, error: OrbitError| {
        assert!(
            error.to_string().contains("injected post-insert failure"),
            "{entry}: the original error must be returned: {error}"
        );
        let runs = jobs.list_job_runs("snapshot_fixture").unwrap();
        assert_eq!(runs.len(), 1, "{entry}: exactly the one inserted run");
        let run = jobs.get_job_run(&runs[0].run_id).unwrap().unwrap();
        assert_eq!(
            run.state,
            JobRunState::Interrupted,
            "{entry}: the inserted run must not stay pending without a worker"
        );
        assert!(
            run.steps.iter().any(|step| step
                .error_message
                .as_deref()
                .is_some_and(|message| message.contains("injected post-insert failure"))),
            "{entry}: the terminal run must carry a startup-failure diagnostic: {:?}",
            run.steps
        );
        reset_runs(&runtime, &run.run_id);
    };
    // Each entry observes only its own run.
    fn reset_runs(runtime: &OrbitRuntime, run_id: &str) {
        runtime
            .sqlite_store()
            .unwrap()
            .connection()
            .lock()
            .unwrap()
            .execute("DELETE FROM job_runs WHERE run_id = ?1", [run_id])
            .unwrap();
    }

    // Seed failure on the plain detached insert.
    let error = runtime
        .submit_pipeline_run("snapshot_fixture", input.clone(), None, Some("operator"))
        .expect_err("seed failure must fail the submission");
    assert_terminalized("detached", error);

    // Trigger-recording failure on a freshly admitted automation run.
    let error = runtime
        .submit_automation_pipeline_run(
            "snapshot_fixture",
            input.clone(),
            "fail-action",
            JobRunTrigger::cli(),
        )
        .expect_err("trigger recording failure must fail the submission");
    assert_terminalized("automation", error);

    // The foreground path inserts and seeds in this process, with no worker.
    let error = runtime
        .run_job_v2_from_yaml(&job_path, input)
        .expect_err("seed failure must fail the foreground run");
    assert_terminalized("foreground", error);
}
