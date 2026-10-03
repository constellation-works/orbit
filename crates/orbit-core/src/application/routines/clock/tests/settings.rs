use tempfile::tempdir;

use super::super::settings::{ClockSettings, load_clock_settings, save_clock_settings};

#[cfg(unix)]
#[test]
fn clock_settings_reject_symlink_escape() {
    use std::os::unix::fs::symlink;

    let root = tempdir().expect("create global root");
    let outside = tempdir().expect("create outside root");
    let outside_settings = outside.path().join("clock.toml");
    save_clock_settings(outside.path(), ClockSettings::default())
        .expect("write outside clock settings");
    symlink(&outside_settings, root.path().join("clock.toml")).expect("create settings symlink");

    let error = load_clock_settings(root.path()).expect_err("reject escaped settings path");

    assert!(
        error
            .to_string()
            .contains("clock configuration must be a regular clock.toml directly under")
    );
}
