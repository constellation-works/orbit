use std::fs;

#[cfg(unix)]
use super::common::increment_model_metric_after_check;

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
