//! `uncommitted_paths` at the `review_gate` boundary: a rename reports both its
//! destination and its source, whichever status column carries it. An
//! intent-to-add rename (`git add -N`) is reported with only the worktree
//! column set (` R`), and its source record was misread as a path of its own.

use std::fs;
use std::path::Path;

use orbit_common::test_env::canonical_temp_dir;
use orbit_engine::review_gate::uncommitted_paths;

/// A repository whose HEAD tracks `a.txt`, so a rename has a source to pair.
fn repo_tracking_a_file() -> tempfile::TempDir {
    let repo = tempfile::tempdir_in(canonical_temp_dir()).unwrap();
    git_ok(repo.path(), &["init", "-q", "--template=", "."]);
    fs::write(repo.path().join("a.txt"), "alpha\n").unwrap();
    git_ok(repo.path(), &["add", "a.txt"]);
    git_ok(
        repo.path(),
        &[
            "-c",
            "user.name=Orbit Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "base",
        ],
    );
    repo
}

fn git_ok(repo: &Path, args: &[&str]) {
    let mut command = crate::git_test_support::command();
    command.args(args).current_dir(repo);
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("spawn {command:?}: {error}"));
    assert!(
        output.status.success(),
        "{command:?} failed ({}):\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn intent_to_add_rename_reports_destination_and_source() {
    let repo = repo_tracking_a_file();
    fs::rename(repo.path().join("a.txt"), repo.path().join("b.txt")).unwrap();
    git_ok(repo.path(), &["add", "-N", "b.txt"]);

    assert_eq!(
        uncommitted_paths(&orbit_common::Env::capture(), repo.path()).unwrap(),
        vec!["a.txt", "b.txt"]
    );
}

#[test]
fn staged_rename_reports_destination_and_source() {
    let repo = repo_tracking_a_file();
    git_ok(repo.path(), &["mv", "a.txt", "c.txt"]);

    assert_eq!(
        uncommitted_paths(&orbit_common::Env::capture(), repo.path()).unwrap(),
        vec!["a.txt", "c.txt"]
    );
}
