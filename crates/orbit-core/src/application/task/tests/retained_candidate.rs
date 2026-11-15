//! A status listing cut by the capture limit must not report an exact omitted count.

use std::process::Command;

use chrono::Utc;
use orbit_engine::run_worktree_paths;
use orbit_types::task::TaskStatus;

use super::{enter_isolated_child, test_runtime};
use crate::application::task::TaskAddParams;
use crate::application::task::retained_candidate::CANDIDATE_STATUS_LIMIT_OVERRIDE;

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
fn truncated_candidate_status_marks_the_omitted_count_as_a_lower_bound() {
    if !enter_isolated_child(
        module_path!(),
        "truncated_candidate_status_marks_the_omitted_count_as_a_lower_bound",
    ) {
        return;
    }
    let (root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Retained candidate".into(),
            description: "The failed run left a worktree.".into(),
            acceptance_criteria: vec!["The omitted count says the listing was cut.".into()],
            status: Some(TaskStatus::Blocked),
            ..Default::default()
        })
        .expect("add task");
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), None, None)
        .expect("insert run");
    let worktrees = root.path().join("worktrees");
    // The isolated child can inherit a host worktree root. Pin it for this process.
    unsafe { std::env::set_var("ORBIT_WORKTREE_ROOT", &worktrees) };
    let path = run_worktree_paths(&runtime.paths().repo_root, &run)
        .expect("worktree path")
        .into_iter()
        .next()
        .expect("one path");
    std::fs::create_dir_all(&path).expect("worktree dir");
    git(&path, &["init", "-b", "main"]);
    std::fs::write(path.join("README"), "kept\n").expect("readme");
    git(&path, &["add", "README"]);
    git(&path, &["commit", "-m", "base"]);
    for index in 0..8 {
        std::fs::write(path.join(format!("untracked-{index:02}")), "changed\n").expect("file");
    }

    CANDIDATE_STATUS_LIMIT_OVERRIDE.with(|limit| limit.set(Some(48)));
    let truncated = runtime.retained_candidate(&task, Some(&run.run_id), Some(&run));
    let value = truncated.to_json();
    assert_eq!(value["changed_paths_truncated"], true, "{value}");
    let listed = value["changed_paths"]
        .as_array()
        .expect("listed paths")
        .len();
    assert!(listed > 0, "the retained prefix is still named: {value}");
    assert!(listed < 8, "the cap must drop part of the listing: {value}");
    assert!(
        truncated.summary().contains("listing truncated"),
        "{}",
        truncated.summary()
    );

    CANDIDATE_STATUS_LIMIT_OVERRIDE.with(|limit| limit.set(None));
    let complete = runtime.retained_candidate(&task, Some(&run.run_id), Some(&run));
    let value = complete.to_json();
    assert!(value.get("changed_paths_truncated").is_none(), "{value}");
    assert_eq!(value["changed_paths"].as_array().expect("paths").len(), 8);
    assert!(!complete.summary().contains("listing truncated"));
}
