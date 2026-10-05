//! `help_goldens/**/*.txt` freeze the shipped `--help` contract of the
//! `orbit` binary. Drift is a consumer-visible CLI surface change. If the new
//! help is intentional, regenerate with `ORBIT_UPDATE_HELP_GOLDENS=1 cargo test
//! -p orbit-cli --test output help_goldens::` (or `make goldens UPDATE=1`) and review
//! the diff.
#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use tempfile::tempdir;

const UPDATE_HELP_GOLDENS_ENV: &str = "ORBIT_UPDATE_HELP_GOLDENS";

/// Each argv (after `orbit`) whose `--help` is pinned, and its golden path
/// under `tests/help_goldens/`.
const CASES: &[(&[&str], &str)] = &[
    (&["run", "agent"], "run/agent.txt"),
    (&["run", "logs"], "run/logs.txt"),
    (&["friction"], "friction/root.txt"),
    (&["friction", "add"], "friction/add.txt"),
    (&["friction", "list"], "friction/list.txt"),
    (&["friction", "show"], "friction/show.txt"),
    (&["friction", "stats"], "friction/stats.txt"),
    (&["friction", "tags"], "friction/tags.txt"),
    (&["friction", "update"], "friction/update.txt"),
    (&["friction", "resolve"], "friction/resolve.txt"),
    (&["friction", "rehome"], "friction/rehome.txt"),
    (&["plugin"], "plugin/root.txt"),
    (&["plugin", "upgrade"], "plugin/upgrade.txt"),
    (&["plugin", "remove"], "plugin/remove.txt"),
    (&["plugin", "validate"], "plugin/validate.txt"),
    (&["plugin", "scaffold"], "plugin/scaffold.txt"),
    (&["plugin", "test"], "plugin/test.txt"),
    (&["mcp", "listen"], "mcp/listen.txt"),
    (&["tool", "run"], "tool/run.txt"),
];

fn golden_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/help_goldens")
        .join(relative)
}

#[test]
fn help_matches_the_shipped_surface() {
    // An empty HOME and cwd: no workspace and no installed plugin groups, so
    // the help is the binary's own surface.
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    fs::create_dir_all(&home).expect("fixture home");
    fs::create_dir_all(&work).expect("fixture work");
    let update = std::env::var(UPDATE_HELP_GOLDENS_ENV).as_deref() == Ok("1");

    for (args, relative) in CASES {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        let output = command
            .current_dir(&work)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env_remove("ORBIT_FORMAT")
            .args(*args)
            .arg("--help")
            .output()
            .expect("run orbit --help");
        assert!(
            output.status.success(),
            "`orbit {} --help` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        let actual = String::from_utf8(output.stdout).expect("help is UTF-8");
        let path = golden_path(relative);
        if update {
            fs::write(&path, &actual)
                .unwrap_or_else(|err| panic!("write help golden {}: {err}", path.display()));
            continue;
        }
        let expected = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read help golden {}: {err}", path.display()));
        assert_eq!(
            actual,
            expected,
            "`orbit {} --help` drifted from {relative}. If the new help is intentional, \
             regenerate with `{UPDATE_HELP_GOLDENS_ENV}=1 cargo test -p orbit-cli --test \
             help_goldens` or `make goldens UPDATE=1`, then review the diff.",
            args.join(" ")
        );
    }
}
