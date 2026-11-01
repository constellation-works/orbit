//! A multi-task direct landing records each task's own reviewable lines.

use std::path::Path;
use std::process::Output;

use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitRuntime, TaskComplexity, TaskStatus, TaskType};
use orbit_engine::RuntimeHost;
use orbit_store::contracts::ReviewableLines;
use orbit_types::workflow::automation::DirectLandingRequest;

fn git(repo: &Path, args: &[&str]) -> Output {
    let output = super::git_test_support::command()
        .current_dir(repo)
        .args(["-c", "user.email=test@example.com", "-c", "user.name=Test"])
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn head(repo: &Path) -> String {
    let output = git(repo, &["rev-parse", "HEAD"]);
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// Two tasks land together. Each reviewable total keeps that task's commits,
/// drops the other task, a configured golden, and a pure rename, and still
/// counts a docs file.
#[test]
fn two_task_landing_counts_only_that_tasks_reviewable_lines() {
    if !super::dispatch_admission::isolated(
        "reviewable_lines::two_task_landing_counts_only_that_tasks_reviewable_lines",
    ) {
        return;
    }

    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let repo = root.path().join("repo");
    let workspace = repo.join(".orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        workspace.join("config.toml"),
        "[workflow]\nnon_reviewable_paths = [\"**/*.golden\"]\n",
    )
    .unwrap();

    git(&repo, &["init", "-b", "main"]);
    std::fs::write(repo.join("old.txt"), "old\n").unwrap();
    git(&repo, &["add", "old.txt"]);
    git(&repo, &["commit", "-m", "base"]);
    let before = head(&repo);

    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let task = |title: &str| {
        runtime
            .add_task(TaskAddParams {
                title: title.into(),
                complexity: TaskComplexity::Low,
                task_type: Some(TaskType::Feature),
                status: Some(TaskStatus::Backlog),
                ..TaskAddParams::default()
            })
            .unwrap()
            .id
            .to_string()
    };
    let task_a = task("alpha");
    let task_b = task("beta");

    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(repo.join("notes.golden"), "g\n".repeat(10)).unwrap();
    std::fs::write(repo.join("README.md"), "docs\n").unwrap();
    git(&repo, &["mv", "old.txt", "new.txt"]);
    git(&repo, &["add", "src/a.txt", "notes.golden", "README.md"]);
    git(&repo, &["commit", "-m", &format!("feat: alpha [{task_a}]")]);

    std::fs::write(repo.join("src/b.txt"), "b\n".repeat(7)).unwrap();
    git(&repo, &["add", "src/b.txt"]);
    git(&repo, &["commit", "-m", &format!("feat: beta [{task_b}]")]);

    std::fs::write(repo.join("src/shared.txt"), "s\n".repeat(9)).unwrap();
    git(&repo, &["add", "src/shared.txt"]);
    git(
        &repo,
        &["commit", "-m", &format!("feat: both [{task_a}] [{task_b}]")],
    );
    let after = head(&repo);

    RuntimeHost::record_direct_landing(
        &runtime,
        &DirectLandingRequest {
            run_id: "run-reviewable".into(),
            branch: "main".into(),
            before_commit: before,
            after_commit: after,
            task_ids: vec![task_a.clone(), task_b.clone()],
            handoff_id: None,
        },
    )
    .unwrap();

    let rows = runtime.landed_diff_stats(None).unwrap();
    let reviewable = |id: &str| {
        rows.iter()
            .find(|row| row.task_id == id)
            .unwrap_or_else(|| panic!("no landed row for {id}; rows={rows:?}"))
            .reviewable
    };
    assert_eq!(
        reviewable(&task_a),
        Some(ReviewableLines {
            added: 3,
            deleted: 0
        }),
        "task A keeps its two code lines and the docs line, not the golden, the rename, task B, or the shared commit"
    );
    assert_eq!(
        reviewable(&task_b),
        Some(ReviewableLines {
            added: 7,
            deleted: 0
        }),
        "task B keeps only its own seven lines"
    );
}
