//! `orbit --help` must not walk the JSONL log directory or open the active file.
#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use tempfile::tempdir;

fn orbit_at_home(work: &std::path::Path, home: &std::path::Path) -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(work)
        .env("HOME", home)
        .env("USERPROFILE", home);
    command
}

#[test]
fn orbit_help_does_not_create_or_open_the_jsonl_log() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    fs::create_dir_all(&home).expect("fixture home");
    fs::create_dir_all(&work).expect("fixture work");

    let logs_dir = home.join(".orbit/state/logs");
    fs::create_dir_all(home.join(".orbit")).expect("orbit dir");
    fs::write(
        home.join(".orbit/config.toml"),
        "[runtime]\nlog_retention_days = 7\nlog_max_total_mb = 10\nlog_max_file_mb = 1\n",
    )
    .expect("seed config.toml so a regression that still parses it has a file to open");

    orbit_at_home(&work, &home).arg("--help").assert().success();

    assert!(
        !logs_dir.exists(),
        "orbit --help must not create the JSONL log directory"
    );
    assert!(
        !logs_dir.join("orbit.jsonl").exists(),
        "orbit --help must not open the JSONL log file"
    );
}
