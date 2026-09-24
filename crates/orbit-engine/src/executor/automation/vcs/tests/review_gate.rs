use std::path::Path;
use std::process::Command;

use tempfile::tempdir;

use super::super::review_gate::{fetch_landed_commit, uncommitted_paths};

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed:\n{}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn uncommitted_paths_ignores_the_non_ascii_source_of_a_staged_rename() {
    let temp = tempdir().expect("temporary repository");
    let repo = temp.path();
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.name", "Orbit Test"]);
    git(repo, &["config", "user.email", "orbit-test@example.com"]);
    std::fs::write(repo.join("éétest.txt"), "content\n").expect("write source file");
    git(repo, &["add", "éétest.txt"]);
    git(repo, &["commit", "-qm", "add source file"]);
    git(repo, &["mv", "éétest.txt", "renamed.txt"]);

    let paths = uncommitted_paths(repo).expect("read status");

    assert_eq!(paths, vec!["renamed.txt"]);
}

/// A landed commit id comes from another machine's record; it must reach
/// `git fetch` as a revision, never as an option such as `--upload-pack`.
#[test]
fn fetch_landed_commit_never_reads_the_commit_as_an_option() {
    let temp = tempdir().expect("temporary repositories");
    let origin = temp.path().join("origin");
    let checkout = temp.path().join("checkout");
    std::fs::create_dir_all(&origin).expect("origin dir");
    git(&origin, &["init", "-q"]);
    git(&origin, &["config", "user.name", "Orbit Test"]);
    git(&origin, &["config", "user.email", "orbit-test@example.com"]);
    git(&origin, &["commit", "-q", "--allow-empty", "-m", "root"]);
    git(
        temp.path(),
        &["clone", "-q", &origin.to_string_lossy(), "checkout"],
    );
    let marker = temp.path().join("injected");

    let result = fetch_landed_commit(
        &checkout,
        &format!("--upload-pack=touch {}", marker.display()),
    );

    assert!(result.is_err(), "an option-shaped commit is not fetchable");
    assert!(
        !marker.exists(),
        "the commit id must not run as an upload-pack command"
    );
}
