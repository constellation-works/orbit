use std::path::Path;
use std::process::Command;

use tempfile::tempdir;

use super::super::review_gate::uncommitted_paths;

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
