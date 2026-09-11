#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! The per-command `--json` flag predates the global `--format` and must keep
//! producing exactly the bytes it produced before the sink existed
//! ([ORB-10569], `docs/design/terminal-interface/specs/output-modes.md` §7
//! steps 1–3).
//!
//! Two properties, checked separately: the exact bytes of a stable payload,
//! and — for commands whose payload embeds machine-specific values — that
//! nothing *below* `--json` on the precedence ladder perturbs them.
//!
//! Since [ORB-10586] the flag is no longer read by the command body: it is
//! rung 2 of the mode resolution in `main`, so `ORBIT_FORMAT` (rung 3) cannot
//! outrank it and an explicit `--format` (rung 1) is meant to. That asymmetry
//! is the contract §2 describes, and both halves of it are asserted here.

use std::path::Path;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use tempfile::{TempDir, tempdir};

/// Commands with a `--json` flag whose output must not shift. Each is a list
/// or detail command that renders through a different code path.
const JSON_COMMANDS: &[&[&str]] = &[&["task", "list"], &["tool", "list"], &["config", "show"]];

struct Fixture {
    _temp: TempDir,
    home: std::path::PathBuf,
    work: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&work).expect("create work");
    Fixture {
        _temp: temp,
        home,
        work,
    }
}

fn run(home: &Path, work: &Path, args: &[&str], env: &[(&str, &str)]) -> Vec<u8> {
    let mut command = cargo_bin_cmd!("orbit");
    // ORB-11300: `task list` reads whatever workspace the ambient
    // `ORBIT_WORKSPACE`/`ORBIT_REGISTRY_ROOT` pair selects, which is the live
    // one when the suite runs inside a managed Orbit run.
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(work)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("ORBIT_FORMAT")
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env_remove("COLUMNS");
    for (key, value) in env {
        command.env(key, value);
    }
    command
        .args(args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone()
}

#[test]
fn json_flag_output_is_untouched_by_the_global_format_machinery() {
    let fixture = fixture();

    for command in JSON_COMMANDS {
        let mut args = command.to_vec();
        args.push("--json");

        let baseline = run(&fixture.home, &fixture.work, &args, &[]);
        assert!(
            !baseline.is_empty(),
            "`orbit {} --json` produced nothing",
            command.join(" ")
        );

        // Rung 3 (the environment) must never outrank the `--json` rung...
        assert_eq!(
            run(
                &fixture.home,
                &fixture.work,
                &args,
                &[("ORBIT_FORMAT", "table")]
            ),
            baseline,
            "ORBIT_FORMAT changed `orbit {} --json`",
            command.join(" ")
        );
        // ...but an explicit `--format` (rung 1) must, which is the whole
        // point of the ladder: `--json` is a legacy alias for rung 2, not an
        // override of the flag the caller passed on this invocation.
        let mut with_format = args.clone();
        with_format.extend(["--format", "table"]);
        assert_ne!(
            run(&fixture.home, &fixture.work, &with_format, &[]),
            baseline,
            "`--format table` did not outrank `--json` on `orbit {}`",
            command.join(" ")
        );
    }
}

#[test]
fn empty_list_json_is_envelope() {
    let fixture = fixture();

    let command = ["task", "list"];
    let mut args = command.to_vec();
    args.push("--json");

    assert_eq!(
        run(&fixture.home, &fixture.work, &args, &[]),
        b"{\n  \"tasks\": [],\n  \"total\": 0,\n  \"truncated\": false\n}\n",
        "`orbit {} --json` must emit an envelope with total and truncated",
        command.join(" ")
    );
}
