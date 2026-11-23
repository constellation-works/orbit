//! Evidence capture isolation [STD-05 §R13].

use std::fs;
use std::path::Path;

use super::super::capture_uncommitted_diff;

fn git(repo: &Path, args: &[&str]) -> String {
    let output = crate::git_test_support::command()
        .arg("-C")
        .arg(repo)
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(["-c", "commit.gpgsign=false"])
        .args(["-c", "gc.auto=0"])
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn object_exists(repo: &Path, blob: &str) -> bool {
    crate::git_test_support::command()
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "-e", blob])
        .output()
        .expect("cat-file")
        .status
        .success()
}

fn repo_in(parent: &Path) -> std::path::PathBuf {
    let repo = parent.join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.email", "capture@example.test"]);
    git(&repo, &["config", "user.name", "capture"]);
    fs::write(repo.join("README"), "hello\n").expect("readme");
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-m", "init"]);
    repo
}

#[test]
fn capture_keeps_ordinary_file_bytes_out_of_the_repository_object_store() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = repo_in(temp.path());
    fs::write(repo.join("notes.txt"), "ordinary canary one\n").expect("notes");
    let blob = git(&repo, &["hash-object", "notes.txt"]);

    let diff = capture_uncommitted_diff(&repo)
        .expect("capture")
        .expect("diff");
    assert!(diff.contains("ordinary canary one"), "{diff}");
    assert!(!object_exists(&repo, &blob), "blob {blob} leaked");
}

#[cfg(unix)]
#[test]
fn capture_refuses_before_staging_when_the_object_store_cannot_be_isolated() {
    // `:` is the alternate-directory separator, so this valid path cannot be
    // passed through `GIT_ALTERNATE_OBJECT_DIRECTORIES`.
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = repo_in(&temp.path().join("with:colon"));
    fs::write(repo.join("notes.txt"), "ordinary canary two\n").expect("notes");
    let blob = git(&repo, &["hash-object", "notes.txt"]);

    let error = capture_uncommitted_diff(&repo).expect_err("isolation failure is reported");
    assert!(
        error
            .to_string()
            .contains("cannot isolate scratch diff objects"),
        "{error}"
    );
    assert!(!object_exists(&repo, &blob), "blob {blob} leaked");
    assert_eq!(git(&repo, &["diff", "--cached", "--name-only"]), "");
}
