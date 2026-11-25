use std::process::Command;

use orbit_common::{process, test_env};
use orbit_core::runtime::JobRuns;
use orbit_store::Store;

//  Automation checkpoints through public composition, a separate store area
//  from task admission and readiness. Mutable fixtures run in isolated children.

fn automation_isolated(test: &str) -> bool {
    const MARKER: &str = "ORBIT_TEST_AUTOMATION_STORE_CHILD";
    if std::env::var(MARKER).as_deref() == Ok(test) {
        return false;
    }
    let home = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(MARKER, test)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path());
    let output =
        process::run_bounded_capped(&mut command, test_env::CHILD_TEST_DEADLINE, 64 * 1024)
            .expect("run isolated automation fixture");
    test_env::assert_child_test_passed(test, output.status, output.stdout, output.stderr);
    true
}

#[test]
fn concurrent_action_key_admission_initializes_one_run_and_resolves_without_writes() {
    if automation_isolated(
        "job_run_records::automation_admission::concurrent_action_key_admission_initializes_one_run_and_resolves_without_writes",
    ) {
        return;
    }
    use orbit_store::contracts::KeyedJobRunAdmission;
    use serde_json::json;
    use std::sync::{Arc, Barrier};

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let base = Store::open(&path).unwrap();
    let jobs = JobRuns::shared(base.clone(), "workspace");
    let other = JobRuns::shared(Store::open(&path).unwrap(), "workspace");
    let barrier = Arc::new(Barrier::new(2));
    let input = json!({"batch": "fixture"});
    let admissions = [jobs.clone(), other].map(|store| {
        let barrier = barrier.clone();
        let input = input.clone();
        std::thread::spawn(move || {
            barrier.wait();
            store
                .insert_automation_job_run("automation_fixture", input, "action")
                .unwrap()
        })
    });
    let outcomes = admissions.map(|thread| thread.join().unwrap());
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, KeyedJobRunAdmission::Admitted(_)))
            .count(),
        1,
        "concurrent submissions must identify exactly one initializer"
    );
    let run_ids = outcomes.map(|outcome| match outcome {
        KeyedJobRunAdmission::Admitted(run) | KeyedJobRunAdmission::Existing(run) => run.run_id,
    });
    assert_eq!(run_ids[0], run_ids[1]);
    assert_eq!(jobs.list_job_runs("automation_fixture").unwrap().len(), 1);
    let run_id = &run_ids[0];
    let mut state = jobs.read_run_state(run_id).unwrap().unwrap();
    state.next_step_index = 3;
    state.step_outputs.insert(2, json!({"checkpoint": true}));
    jobs.write_run_state(run_id, &state).unwrap();
    base.connection()
        .lock()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER refuse_run_rewrite BEFORE UPDATE ON job_runs \
         BEGIN SELECT RAISE(ABORT, 'existing admission rewrote a run'); END;",
        )
        .unwrap();

    let existing = jobs
        .insert_automation_job_run("automation_fixture", input.clone(), "action")
        .unwrap();
    assert!(matches!(existing, KeyedJobRunAdmission::Existing(run) if run.run_id == *run_id));
    for (job, altered) in [
        ("automation_fixture", json!({"batch": "changed"})),
        ("other_job", input),
    ] {
        assert!(matches!(
            jobs.insert_automation_job_run(job, altered, "action"),
            Err(orbit_common::OrbitError::InvalidInput(_))
        ));
    }
    assert_eq!(jobs.read_run_state(run_id).unwrap(), Some(state));
}
