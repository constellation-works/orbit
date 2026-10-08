#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! The `--json` spelling predates the global `--format` and must keep
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
//! outrank it and a compatible explicit `--format json` keeps its bytes. Contradictory
//! formats are usage errors, covered by `global_json`.

use std::path::Path;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::TempDir;

/// Commands with a `--json` flag whose output must not shift. Each is a list
/// or detail command that renders through a different code path.
const JSON_COMMANDS: &[&[&str]] = &[&["task", "list"], &["tool", "list"], &["config", "show"]];

struct Fixture {
    _temp: TempDir,
    home: std::path::PathBuf,
    work: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let checkout = crate::git_repo::WorkCheckout::new();
    Fixture {
        _temp: checkout.temp,
        home: checkout.home,
        work: checkout.work,
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
        // Repeating the same format preserves the historical pretty bytes.
        let mut with_format = args.clone();
        with_format.extend(["--format", "json"]);
        assert_eq!(
            run(&fixture.home, &fixture.work, &with_format, &[]),
            baseline,
            "compatible format changed `orbit {} --json`",
            command.join(" ")
        );
    }
}

#[test]
fn empty_list_json_is_exactly_an_empty_array() {
    let fixture = fixture();

    let command = ["task", "list"];
    let mut args = command.to_vec();
    args.push("--json");

    assert_eq!(
        run(&fixture.home, &fixture.work, &args, &[]),
        b"[]\n",
        "`orbit {} --json` must stay a bare array",
        command.join(" ")
    );
}

/// A temp directory nested in this crate's checkout is the managed-run case:
/// `TMPDIR` lives under the worktree. The shared helper must still answer
/// from its own empty workspace, not the enclosing checkout's config.
#[test]
fn shared_checkout_does_not_inherit_config_when_tempdir_is_nested_in_checkout() {
    let checkout = crate::git_repo::WorkCheckout::new_in(Path::new(env!("CARGO_MANIFEST_DIR")));
    assert_eq!(
        run(
            &checkout.home,
            &checkout.work,
            &["task", "list", "--json"],
            &[]
        ),
        b"[]\n",
        "a fixture checkout nested in another Git checkout must not load that checkout's config"
    );
    let shown = run(
        &checkout.home,
        &checkout.work,
        &["config", "show", "--json"],
        &[],
    );
    let document: Value = serde_json::from_slice(&shown).expect("config show json");
    let workspace_path = document["source"]["workspace_path"]
        .as_str()
        .expect("config show names the workspace config it loaded");
    assert_eq!(
        std::fs::canonicalize(workspace_path).expect("canonical loaded config"),
        std::fs::canonicalize(checkout.work.join(".orbit/config.toml"))
            .expect("canonical fixture config"),
        "lookup must stop at the fixture checkout, not an ancestor workspace config"
    );
}
