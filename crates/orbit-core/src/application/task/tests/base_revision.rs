//! An unreadable run record must not be reported as this checkout's HEAD.

use std::process::Command;

use chrono::Utc;
use orbit_types::workflow::PipelineState;

use super::{enter_isolated_child, test_runtime};

fn git(repo: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Orbit Test")
        .env("GIT_AUTHOR_EMAIL", "orbit@example.com")
        .env("GIT_COMMITTER_NAME", "Orbit Test")
        .env("GIT_COMMITTER_EMAIL", "orbit@example.com")
        .status()
        .expect("git");
    assert!(status.success(), "git {args:?}");
}

#[test]
fn unreadable_run_state_does_not_pin_the_base_to_head() {
    if !enter_isolated_child(
        module_path!(),
        "unreadable_run_state_does_not_pin_the_base_to_head",
    ) {
        return;
    }
    let (root, runtime) = test_runtime();
    let repo = root.path().join("repo");
    git(&repo, &["init", "-b", "main"]);
    std::fs::write(repo.join("README"), "base\n").expect("seed");
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-m", "base"]);

    let head = runtime
        .run_base_revision("run-with-no-state")
        .expect("a run with no recorded state still resolves checkout HEAD");
    assert_eq!(head.len(), 40, "{head}");

    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), None, None)
        .expect("insert run");
    let state = PipelineState::new(
        run.run_id.clone(),
        run.job_id.clone(),
        serde_json::json!({}),
    );
    runtime
        .stores()
        .jobs()
        .write_run_state(&run.run_id, &state)
        .expect("write state");
    let database = runtime.global_root().join("orbit.db");
    let connection = rusqlite::Connection::open(&database).expect("open store");
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .expect("busy timeout");
    let updated = connection
        .execute(
            "UPDATE job_run_states SET pipeline_state_json = '{' WHERE run_id = ?1",
            [&run.run_id],
        )
        .expect("corrupt state");
    assert_eq!(updated, 1, "the run's state row is the one made unreadable");

    let error = runtime
        .run_base_revision(&run.run_id)
        .expect_err("an unreadable run state is not this checkout");
    assert!(
        error.contains("unreadable"),
        "the error names the failed read, not a HEAD substitution: {error}"
    );
    assert!(
        !error.contains(&head),
        "the unreadable record must not be answered with HEAD {head}: {error}"
    );
}
