use std::fs;

use super::common::increment_model_metric;
#[cfg(unix)]
use super::common::increment_model_metric_after_check;

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

/// Replace the checked scoreboard file with a symlink to `target`.
#[cfg(unix)]
fn swap_in_symlink(
    checked: &std::path::Path,
    target: &std::path::Path,
) -> Result<(), orbit_common::OrbitError> {
    fs::remove_file(checked).expect("remove checked scoreboard");
    std::os::unix::fs::symlink(target, checked).expect("swap in a symlink");
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

#[cfg(unix)]
#[test]
fn increment_refuses_a_symlink_swapped_in_after_validation() {
    let root = tempfile::tempdir().expect("create scoreboard parent");
    let outside = tempfile::tempdir().expect("create outside parent");
    let outside_scoreboard = outside.path().join("pr.json");
    let outside_contents = r#"{"pr-count":{"leaked":7}}"#;
    fs::write(&outside_scoreboard, outside_contents).expect("write outside scoreboard");
    fs::write(root.path().join("pr.json"), r#"{"pr-count":{"codex":1}}"#).expect("seed scoreboard");

    let error = increment_model_metric_after_check(
        root.path(),
        "pr.json",
        "pr-count",
        "codex",
        |checked| swap_in_symlink(checked, &outside_scoreboard),
    )
    .expect_err("symlink swapped in after validation");

    assert!(
        error.to_string().contains("must not be a symlink"),
        "{error}"
    );
    assert_eq!(
        fs::read_to_string(&outside_scoreboard).expect("read outside scoreboard"),
        outside_contents,
        "the outside file must be neither rewritten nor consumed"
    );
    assert!(
        fs::symlink_metadata(root.path().join("pr.json"))
            .expect("inspect swapped path")
            .file_type()
            .is_symlink(),
        "a refused increment must not replace the swapped path"
    );
}

#[cfg(unix)]
#[test]
fn increment_does_not_block_on_a_fifo_swapped_in_after_validation() {
    let root = tempfile::tempdir().expect("create scoreboard parent");
    let scoreboard_dir = root.path().to_path_buf();
    fs::write(scoreboard_dir.join("pr.json"), "{}").expect("seed scoreboard");
    let (sender, receiver) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let result = increment_model_metric_after_check(
            &scoreboard_dir,
            "pr.json",
            "pr-count",
            "codex",
            swap_in_fifo,
        );
        sender.send(result).expect("report increment result");
    });

    let result = receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("a FIFO swap must not block the scoreboard increment");
    let error = result.expect_err("a swapped FIFO must be rejected");
    assert!(error.to_string().contains("regular file"), "{error}");
}

#[cfg(unix)]
#[test]
fn increment_follows_a_configured_symlinked_scoreboard_root() {
    let temp = tempfile::tempdir().expect("create scoreboard parent");
    let real_dir = temp.path().join("real-scoreboard");
    fs::create_dir(&real_dir).expect("create real scoreboard dir");
    let linked_dir = temp.path().join("linked-scoreboard");
    std::os::unix::fs::symlink(&real_dir, &linked_dir).expect("link scoreboard root");

    for _ in 0..2 {
        increment_model_metric(
            &linked_dir,
            "pr.json",
            "test scoreboard",
            "pr-count",
            "codex",
            |_| {},
        )
        .expect("increment through symlinked root");
    }

    let content = fs::read_to_string(real_dir.join("pr.json")).expect("read scoreboard");
    let scoreboard: serde_json::Value = serde_json::from_str(&content).expect("parse scoreboard");
    assert_eq!(scoreboard["pr-count"]["codex"], 2);
}
