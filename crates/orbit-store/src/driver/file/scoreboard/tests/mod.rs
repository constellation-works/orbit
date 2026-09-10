use std::fs;

use super::common::increment_model_metric;

#[test]
fn increment_creates_and_updates_a_scoreboard_file() {
    let temp = tempfile::tempdir().expect("create scoreboard parent");
    let scoreboard_dir = temp.path().join("scoreboard");

    increment_model_metric(
        &scoreboard_dir,
        "pr.json",
        "test scoreboard",
        "pr-count",
        "codex",
        |_| {},
    )
    .expect("create scoreboard");
    increment_model_metric(
        &scoreboard_dir,
        "pr.json",
        "test scoreboard",
        "pr-count",
        "codex",
        |_| {},
    )
    .expect("update scoreboard");

    let content = fs::read_to_string(scoreboard_dir.join("pr.json")).expect("read scoreboard");
    let scoreboard: serde_json::Value = serde_json::from_str(&content).expect("parse scoreboard");
    assert_eq!(scoreboard["pr-count"]["codex"], 2);
}

#[cfg(unix)]
#[test]
fn increment_rejects_a_symlinked_scoreboard_file() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("create scoreboard parent");
    let outside = tempfile::tempdir().expect("create outside parent");
    fs::write(outside.path().join("pr.json"), "{}").expect("write outside scoreboard");
    symlink(outside.path().join("pr.json"), root.path().join("pr.json"))
        .expect("create scoreboard symlink");

    let error = increment_model_metric(
        root.path(),
        "pr.json",
        "test scoreboard",
        "pr-count",
        "codex",
        |_| {},
    )
    .expect_err("reject scoreboard symlink");

    assert!(error.to_string().contains("must not be a symlink"));
    assert_eq!(
        fs::read_to_string(outside.path().join("pr.json")).expect("read outside scoreboard"),
        "{}"
    );
}
