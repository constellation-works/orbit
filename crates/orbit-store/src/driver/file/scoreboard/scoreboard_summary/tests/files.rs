use super::super::*;

use super::super::files::{PR_SCOREBOARD_FILENAME, TOKEN_SCOREBOARD_FILENAME};
use super::super::files::{read_model_scoreboard_after_check, read_token_agents_after_check};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;

#[cfg(unix)]
#[test]
fn summary_rejects_a_symlinked_pr_scoreboard() {
    let root = tempfile::tempdir().expect("create scoreboard dir");
    let outside = tempfile::tempdir().expect("create outside dir");
    let outside_scoreboard = outside.path().join(PR_SCOREBOARD_FILENAME);
    fs::write(
        &outside_scoreboard,
        r#"{"pr-review-comments":{"gpt-reviewer":1}}"#,
    )
    .expect("write outside scoreboard");
    symlink(
        &outside_scoreboard,
        root.path().join(PR_SCOREBOARD_FILENAME),
    )
    .expect("create scoreboard symlink");

    let error = generate_summary(root.path(), &[]).expect_err("reject symlinked scoreboard");

    assert!(
        error
            .to_string()
            .contains("scoreboard file must not be a symlink"),
        "unexpected error: {error}"
    );
}

#[cfg(unix)]
#[test]
fn summary_rejects_a_symlinked_token_scoreboard() {
    let root = tempfile::tempdir().expect("create scoreboard dir");
    let outside = tempfile::tempdir().expect("create outside dir");
    let outside_scoreboard = outside.path().join(TOKEN_SCOREBOARD_FILENAME);
    fs::write(
        &outside_scoreboard,
        r#"{"agents":[{"agent":"codex","model":"gpt-5","total_tokens":1}]}"#,
    )
    .expect("write outside scoreboard");
    symlink(
        &outside_scoreboard,
        root.path().join(TOKEN_SCOREBOARD_FILENAME),
    )
    .expect("create scoreboard symlink");

    let error = generate_summary(root.path(), &[]).expect_err("reject symlinked scoreboard");

    assert!(
        error
            .to_string()
            .contains("scoreboard file must not be a symlink"),
        "unexpected error: {error}"
    );
}

/// Replace the checked scoreboard file with a symlink to `target`.
#[cfg(unix)]
fn swap_in_symlink(
    checked: &std::path::Path,
    target: &std::path::Path,
) -> Result<(), orbit_common::OrbitError> {
    fs::remove_file(checked).expect("remove checked scoreboard");
    symlink(target, checked).expect("swap in a symlink");
    Ok(())
}

/// Replace the checked scoreboard file with a FIFO no writer will open.
#[cfg(unix)]
fn swap_in_fifo(checked: &std::path::Path) -> Result<(), orbit_common::OrbitError> {
    fs::remove_file(checked).expect("remove checked scoreboard");
    let status = std::process::Command::new("mkfifo")
        .arg(checked)
        .status()
        .expect("run mkfifo");
    assert!(status.success(), "mkfifo failed: {status}");
    Ok(())
}

/// Run `read` on a worker thread and fail if it does not finish promptly.
#[cfg(unix)]
fn finishes_promptly<T: Send + 'static>(
    read: impl FnOnce() -> Result<T, orbit_common::OrbitError> + Send + 'static,
) -> Result<T, orbit_common::OrbitError> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        sender.send(read()).expect("report read result");
    });
    receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("a FIFO swap must not block the scoreboard read")
}

#[cfg(unix)]
#[test]
fn pr_scoreboard_read_refuses_a_symlink_swapped_in_after_validation() {
    let root = tempfile::tempdir().expect("create scoreboard dir");
    let outside = tempfile::tempdir().expect("create outside dir");
    let outside_scoreboard = outside.path().join(PR_SCOREBOARD_FILENAME);
    fs::write(
        &outside_scoreboard,
        r#"{"pr-review-comments":{"gpt-reviewer":1}}"#,
    )
    .expect("write outside scoreboard");
    fs::write(root.path().join(PR_SCOREBOARD_FILENAME), "{}").expect("seed scoreboard");

    let error = read_model_scoreboard_after_check(root.path(), |checked| {
        swap_in_symlink(checked, &outside_scoreboard)
    })
    .expect_err("symlink swapped in after validation");

    assert!(
        error.to_string().contains("must not be a symlink"),
        "unexpected error: {error}"
    );
}

#[cfg(unix)]
#[test]
fn token_scoreboard_read_refuses_a_symlink_swapped_in_after_validation() {
    let root = tempfile::tempdir().expect("create scoreboard dir");
    let outside = tempfile::tempdir().expect("create outside dir");
    let outside_scoreboard = outside.path().join(TOKEN_SCOREBOARD_FILENAME);
    fs::write(
        &outside_scoreboard,
        r#"{"agents":[{"agent":"codex","model":"gpt-5","total_tokens":1}]}"#,
    )
    .expect("write outside scoreboard");
    fs::write(root.path().join(TOKEN_SCOREBOARD_FILENAME), "{}").expect("seed scoreboard");

    let error = read_token_agents_after_check(root.path(), |checked| {
        swap_in_symlink(checked, &outside_scoreboard)
    })
    .expect_err("symlink swapped in after validation");

    assert!(
        error.to_string().contains("must not be a symlink"),
        "unexpected error: {error}"
    );
}

#[cfg(unix)]
#[test]
fn pr_scoreboard_read_does_not_block_on_a_fifo_swapped_in_after_validation() {
    let root = tempfile::tempdir().expect("create scoreboard dir");
    fs::write(root.path().join(PR_SCOREBOARD_FILENAME), "{}").expect("seed scoreboard");
    let scoreboard_dir = root.path().to_path_buf();

    let error =
        finishes_promptly(move || read_model_scoreboard_after_check(&scoreboard_dir, swap_in_fifo))
            .expect_err("a swapped FIFO must be rejected");

    assert!(error.to_string().contains("regular file"), "{error}");
}

#[cfg(unix)]
#[test]
fn token_scoreboard_read_does_not_block_on_a_fifo_swapped_in_after_validation() {
    let root = tempfile::tempdir().expect("create scoreboard dir");
    fs::write(root.path().join(TOKEN_SCOREBOARD_FILENAME), "{}").expect("seed scoreboard");
    let scoreboard_dir = root.path().to_path_buf();

    let error =
        finishes_promptly(move || read_token_agents_after_check(&scoreboard_dir, swap_in_fifo))
            .expect_err("a swapped FIFO must be rejected");

    assert!(error.to_string().contains("regular file"), "{error}");
}

#[cfg(unix)]
#[test]
fn summary_reads_scoreboards_through_a_configured_symlinked_root() {
    let temp = tempfile::tempdir().expect("create scoreboard parent");
    let real_dir = temp.path().join("real-scoreboard");
    fs::create_dir(&real_dir).expect("create real scoreboard dir");
    fs::write(
        real_dir.join(PR_SCOREBOARD_FILENAME),
        r#"{"pr-review-comments":{"gpt-reviewer":3}}"#,
    )
    .expect("write pr scoreboard");
    fs::write(
        real_dir.join(TOKEN_SCOREBOARD_FILENAME),
        r#"{"agents":[{"agent":"codex","model":"gpt-5","total_tokens":11}]}"#,
    )
    .expect("write token scoreboard");
    let linked_dir = temp.path().join("linked-scoreboard");
    symlink(&real_dir, &linked_dir).expect("link scoreboard root");

    let pr = read_model_scoreboard_after_check(&linked_dir, |_| Ok(()))
        .expect("read pr scoreboard through symlinked root");
    assert!(!pr.is_empty(), "pr scoreboard rows must be read: {pr:?}");

    let tokens = read_token_agents_after_check(&linked_dir, |_| Ok(()))
        .expect("read token scoreboard through symlinked root");
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].total_tokens, 11);
}

#[test]
fn missing_scoreboard_files_read_as_empty() {
    let root = tempfile::tempdir().expect("create scoreboard dir");

    let pr =
        read_model_scoreboard_after_check(root.path(), |_| Ok(())).expect("missing pr scoreboard");
    assert!(pr.is_empty());
    let tokens =
        read_token_agents_after_check(root.path(), |_| Ok(())).expect("missing token scoreboard");
    assert!(tokens.is_empty());
}
