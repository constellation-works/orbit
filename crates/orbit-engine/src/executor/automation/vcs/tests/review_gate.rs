use std::path::Path;
use std::process::Command;

use tempfile::tempdir;

use super::super::review_gate::{
    commit_reviewer_repairs, committed_paths, fetch_landed_commit, review_repair_at_head, revision,
    uncommitted_paths,
};

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

fn seeded_repo(repo: &Path) -> String {
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["config", "user.name", "Orbit Test"]);
    git(repo, &["config", "user.email", "orbit-test@example.com"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("src.txt"), "candidate\n").expect("write source");
    git(repo, &["add", "src.txt"]);
    git(repo, &["commit", "-qm", "candidate"]);
    revision(repo, "HEAD").expect("candidate").commit
}

/// A restart after the repair commit must recognize exactly that commit as
/// the attempt's own, and nothing that merely resembles it.
#[test]
fn review_repair_at_head_recognizes_only_the_attempts_own_repair_commit() {
    let temp = tempdir().expect("temporary repository");
    let repo = temp.path();
    let candidate = seeded_repo(repo);
    std::fs::write(repo.join("src.txt"), "candidate\nrepaired\n").expect("repair");
    let repair = commit_reviewer_repairs(
        repo,
        "codex / review-model",
        "review: repairs\n\nOrbit-Review-Attempt: rvw-1",
    )
    .expect("commit repair")
    .expect("a repair commit");

    let owned = review_repair_at_head(repo, &candidate, "rvw-1", "codex / review-model")
        .expect("inspect head");
    assert_eq!(owned, Some(repair.clone()));
    assert_eq!(
        review_repair_at_head(repo, &candidate, "rvw-2", "codex / review-model").expect("inspect"),
        None,
        "another attempt's trailer is not this attempt's repair"
    );
    assert_eq!(
        review_repair_at_head(repo, &repair.commit, "rvw-1", "codex / review-model")
            .expect("inspect"),
        None,
        "the repair must sit directly on the admitted candidate"
    );
    assert_eq!(
        review_repair_at_head(repo, &candidate, "rvw-1", "claude / review-model").expect("inspect"),
        None,
        "a different reviewer attribution is not the gate's commit"
    );

    // A commit carrying the trailer under someone else's identity is foreign.
    git(repo, &["reset", "-q", "--hard", &candidate]);
    std::fs::write(repo.join("src.txt"), "candidate\nforged\n").expect("edit");
    git(
        repo,
        &["commit", "-qam", "forged\n\nOrbit-Review-Attempt: rvw-1"],
    );
    assert_eq!(
        review_repair_at_head(repo, &candidate, "rvw-1", "codex / review-model").expect("inspect"),
        None
    );
}

#[test]
fn committed_paths_report_a_rename_by_its_destination_like_the_worktree_did() {
    let temp = tempdir().expect("temporary repository");
    let repo = temp.path();
    seeded_repo(repo);
    std::fs::write(repo.join("notes.txt"), "notes\n").expect("write notes");
    git(repo, &["mv", "src.txt", "renamed.txt"]);
    let before_commit = uncommitted_paths(repo).expect("status");
    git(repo, &["add", "--all"]);
    git(repo, &["commit", "-qm", "rename"]);

    let committed = committed_paths(repo, "HEAD").expect("diff");
    assert_eq!(committed, vec!["notes.txt", "renamed.txt"]);
    assert_eq!(committed, before_commit);
}
