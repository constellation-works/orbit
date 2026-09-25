use super::super::*;

use super::super::files::{PR_SCOREBOARD_FILENAME, TOKEN_SCOREBOARD_FILENAME};
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
