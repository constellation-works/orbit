//! `orbit <ns> <verb>` end to end (design `docs/design/plugins` §4.6, §5).
//!
//! The claim under test is that the derived group is a *spelling*: the same
//! result and the same audited operation as `orbit tool run <ns>.<verb>`.
//! Everything here runs the real binary against a disposable home, because
//! the clap tree is built from the host's installed manifests at startup —
//! an in-process test could not observe that.
#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use predicates::prelude::*;
use rusqlite::Connection;
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::git_repo;

struct Fixture {
    _temp: TempDir,
    home: std::path::PathBuf,
    work: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        // Plugin commands report resolved paths (a plugin root, a skill link's
        // target, the workspace a render names), and the assertions compare
        // them with these, so root the fixture at a resolved temp directory.
        // The default macOS one sits behind the `/var` symlink.
        let temp = tempfile::tempdir_in(test_env::canonical_temp_dir()).expect("tempdir");
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        std::fs::create_dir_all(&home).expect("create home");
        git_repo::init(&work);
        let fixture = Self {
            _temp: temp,
            home,
            work,
        };
        fixture
            .orbit()
            .args([
                "init",
                "--non-interactive",
                "--machine-name",
                "plugin-cli-host",
                "--task-prefix",
                "TST",
            ])
            .assert()
            .success();
        fixture
            .orbit()
            .args(["workspace", "init", "--name", "plugin-cli"])
            .assert()
            .success();
        fixture
    }

    /// An invocation pinned to this fixture's home, with no inherited
    /// authority — including the managed-run activity allowlist, which would
    /// otherwise decide whether a plugin tool may be called at all.
    fn orbit(&self) -> assert_cmd::Command {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        for name in ["ORBIT_TASK_ACTOR_KIND", "ORBIT_ACTIVITY_TOOLS"] {
            command.env_remove(name);
        }
        command
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home);
        command
    }

    /// The same invocation, naming the caller as this machine's operator.
    /// A test binary is not a terminal, so a plugin tool call would
    /// otherwise be refused as an unidentified caller.
    fn orbit_as_operator(&self) -> assert_cmd::Command {
        let mut command = self.orbit();
        command.env("ORBIT_OPERATOR", "1");
        command
    }

    fn source(&self, namespace: &str) -> std::path::PathBuf {
        self.home.join("plugin-sources").join(namespace)
    }

    /// Audit rows for one tool, as `(command, subcommand, target_type,
    /// target_id, status)`.
    fn audit_rows(&self, tool: &str) -> Vec<(String, String, String, String, String)> {
        let connection =
            Connection::open(self.home.join(".orbit/orbit.db")).expect("open the audit database");
        let mut statement = connection
            .prepare(
                "SELECT command, subcommand, target_type, target_id, status FROM audit_events \
                 WHERE tool_name = ?1 ORDER BY id",
            )
            .expect("prepare the audit query");
        let rows = statement
            .query_map([tool], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    row.get::<_, String>(4)?,
                ))
            })
            .expect("query the audit rows");
        rows.collect::<Result<Vec<_>, _>>().expect("audit rows")
    }
}

/// A plugin whose one tool's schema covers every shape the mapping names:
/// string, integer, boolean, enum, array and a nested object. The backend
/// echoes the input back, so the two spellings can be compared by result.
/// Written into the source's `.orbit-plugin/`.
fn write_fixture_plugin(source: &Path) {
    let root = &source.join(".orbit-plugin");
    std::fs::create_dir_all(root.join("bin")).expect("create plugin dirs");
    let backend = root.join("bin/backend.sh");
    std::fs::write(
        &backend,
        "#!/bin/sh\ninput=$(cat)\ncase \"$input\" in *'shapes.fail'*) exit 91;; esac\nprintf '{\"ok\":true,\"output\":{\"echo\":%s}}\\n' \"$input\"\n",
    )
    .expect("write backend");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o755))
            .expect("chmod backend");
    }
    std::fs::write(
        root.join("plugin.yaml"),
        r#"schemaVersion: 2
kind: Plugin
metadata:
  name: shapes
  version: 0.1.0
  description: Every schema shape the CLI mapping covers.
spec:
  backend:
    type: exec
    command: bin/backend.sh
  tools:
    - name: recommend
      description: Recommend files for a query.
      execution_kind: read_only
      mcp_scope: workspace
      input_schema:
        type: object
        properties:
          query: { type: string, description: What to look for. }
          max_depth: { type: integer, description: How deep to walk. }
          loud: { type: boolean, description: Say more about it. }
          mode: { type: string, enum: [fast, thorough], description: How hard to look. }
          tags: { type: array, items: { type: string }, description: Repeatable tag filter. }
          filters: { type: object, description: Nested filter object. }
      cli:
        positional: [query]
    - name: maintain
      description: Rebuild the index.
      execution_kind: mutating
      mcp_scope: workspace
      input_schema:
        type: object
        properties:
          force: { type: boolean }
    - name: fail
      description: Exit if the backend is actually executed.
      execution_kind: read_only
      mcp_scope: workspace
"#,
    )
    .expect("write manifest");
}

/// A one-tool plugin written into the source's `.orbit-plugin/`.
fn write_status_plugin(source: &Path, namespace: &str, extra_spec: &str) {
    let root = &source.join(".orbit-plugin");
    std::fs::create_dir_all(root.join("bin")).expect("create plugin dirs");
    let backend = root.join("bin/backend.sh");
    std::fs::write(
        &backend,
        "#!/bin/sh\nprintf '{\"ok\":true,\"output\":{}}\\n'\n",
    )
    .expect("write backend");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o755))
            .expect("chmod backend");
    }
    std::fs::write(
        root.join("plugin.yaml"),
        format!(
            "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: {namespace}\n  version: 0.1.0\n  description: Status projection fixture.\nspec:\n{extra_spec}  backend:\n    type: exec\n    command: bin/backend.sh\n  tools:\n    - name: status\n      description: Report status.\n      execution_kind: read_only\n      mcp_scope: workspace\n"
        ),
    )
    .expect("write manifest");
}

#[test]
fn upgrade_refuses_a_tampered_recorded_source_and_accepts_an_explicit_source() {
    let fixture = Fixture::new();
    let source = fixture.source("victim");
    let source_arg = source.to_str().expect("utf8 source");
    let permissions = "  permissions:\n    network: any\n";
    write_status_plugin(&source, "victim", permissions);
    let payload = "upgrade-payload.txt";
    std::fs::write(source.join(".orbit-plugin").join(payload), b"trusted-v1")
        .expect("write trusted payload");
    fixture
        .orbit()
        .args([
            "plugin", "add", source_arg, "--enable", "--grant", "network",
        ])
        .assert()
        .success();

    // A backend with orbit_tools can write its own state tree and orbit.db,
    // but cannot write the victim's installed tree or grant witness.
    let attacker = fixture
        .home
        .join(".orbit/state/plugins/attacker/replacement");
    write_status_plugin(&attacker, "victim", permissions);
    std::fs::write(attacker.join(".orbit-plugin").join(payload), b"attacker")
        .expect("write attacker payload");
    let connection =
        Connection::open(fixture.home.join(".orbit/orbit.db")).expect("open the plugin store");
    assert_eq!(
        connection
            .execute(
                "UPDATE plugins SET source = ?1 WHERE name = 'victim'",
                [attacker.to_str().expect("utf8 attacker source")],
            )
            .expect("tamper with the recorded source"),
        1
    );
    let record = || {
        connection
            .query_row(
                "SELECT source, enabled, grants_json, manifest_digest, install_path \
                 FROM plugins WHERE name = 'victim'",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, bool>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .expect("read victim record")
    };
    let tampered_record = record();
    let installed = fixture.home.join(".orbit/plugins/victim/0.1.0");
    let witness = fixture.home.join(".orbit/plugins/.grants/victim.json");
    let authorized = std::fs::read(&witness).expect("read grant witness");

    // Missing and blank explicit sources must fail in the upgrade use case,
    // before the forged same-namespace source can inherit the victim's grants.
    for explicit in [None, Some(""), Some(" \t ")] {
        let mut command = fixture.orbit();
        command.args(["plugin", "upgrade", "victim"]);
        if let Some(explicit) = explicit {
            command.arg(explicit);
        }
        command.assert().failure().stderr(predicate::str::contains(
            "orbit plugin upgrade victim <source>",
        ));
        assert_eq!(
            std::fs::read(installed.join(payload)).expect("read installed payload"),
            b"trusted-v1",
            "an untrusted recorded source must not replace the installed code"
        );
        assert_eq!(record(), tampered_record, "refusal preserves the host row");
        assert_eq!(
            std::fs::read(&witness).expect("read grant witness after refusal"),
            authorized,
            "refusal preserves the victim's authorization"
        );
    }

    std::fs::write(source.join(".orbit-plugin").join(payload), b"trusted-v2")
        .expect("update trusted payload");
    fixture
        .orbit()
        .args(["plugin", "upgrade", "victim", source_arg])
        .assert()
        .success();
    assert_eq!(
        std::fs::read(installed.join(payload)).expect("read upgraded payload"),
        b"trusted-v2",
        "the caller's explicit source takes precedence over the forged database value"
    );
    let upgraded = record();
    assert_eq!(upgraded.0, source_arg);
    assert!(
        upgraded.1,
        "a safe explicit upgrade preserves enabled state"
    );
    assert_eq!(upgraded.2, tampered_record.2, "safe grants carry over");
    assert_eq!(
        std::fs::read(&witness).expect("read carried grant witness"),
        authorized
    );
}

fn stdout_json(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "expected JSON on stdout: {error}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[cfg(unix)]
#[test]
fn plugin_error_is_json_on_tool_run_with_a_nonzero_exit() {
    if !orbit_exec::macos_sandbox_test_guard("plugin_error_is_json_on_tool_run_with_a_nonzero_exit")
    {
        return;
    }
    let fixture = Fixture::new();
    let source = fixture.source("errors");
    write_status_plugin(&source, "errors", "");
    std::fs::write(
        source.join(".orbit-plugin/bin/backend.sh"),
        "#!/bin/sh\nprintf '{\"ok\":false,\"error\":{\"code\":\"budget_exceeded\",\"message\":\"wait\",\"retryable\":true,\"detail\":{\"retry_after\":30}}}\\n'\n",
    )
    .expect("write error backend");
    fixture
        .orbit()
        .args(["plugin", "add", source.to_str().expect("utf8 source")])
        .assert()
        .success();
    fixture
        .orbit()
        .args(["plugin", "enable", "errors"])
        .assert()
        .success();

    let output = fixture
        .orbit()
        .env("ORBIT_AGENT_NAME", "codex")
        .args(["tool", "run", "errors.status", "--input", "{}"])
        .output()
        .expect("run plugin tool");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let error: Value = serde_json::from_slice(&output.stderr).unwrap_or_else(|parse_error| {
        panic!(
            "JSON error on stderr ({parse_error}): {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(
        error,
        json!({
            "code": "budget_exceeded", "message": "wait", "retryable": true,
            "detail": {"retry_after": 30}
        })
    );
}

#[cfg(unix)]
#[test]
fn derived_plugin_groups_expand_tilde_roots_and_anchor_relative_roots() {
    let fixture = Fixture::new();
    let source = fixture.source("shapes");
    write_fixture_plugin(&source);
    fixture
        .orbit()
        .args(["plugin", "add", source.to_str().expect("UTF-8 source")])
        .assert()
        .success();
    fixture
        .orbit()
        .args(["plugin", "enable", "shapes"])
        .assert()
        .success();

    for raw in ["~/.orbit", "~//.orbit", ".orbit"] {
        let cwd = if raw == ".orbit" {
            &fixture.home
        } else {
            &fixture.work
        };
        for use_flag in [false, true] {
            let mut command = fixture.orbit();
            command.current_dir(cwd);
            if use_flag {
                command
                    .args(["--root", raw])
                    .env("ORBIT_ROOT", "unused-env-root");
            } else {
                command.env("ORBIT_ROOT", raw);
            }
            let output = command
                .args([
                    "shapes",
                    "recommend",
                    "roots",
                    "--explain",
                    "--format",
                    "json",
                ])
                .output()
                .expect("explain plugin command under selected root");
            assert!(output.status.success(), "{raw}: {output:?}");
            let explained = stdout_json(&output);
            assert_eq!(explained["tool"], "shapes.recommend");
            assert_eq!(explained["input"]["query"], "roots");
            assert!(!cwd.join("~").exists());
            assert!(!cwd.join("unused-env-root").exists());
        }
    }
}

#[test]
fn a_derived_group_is_the_same_operation_and_result_as_tool_run() {
    if !orbit_exec::macos_sandbox_test_guard(
        "a_derived_group_is_the_same_operation_and_result_as_tool_run",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let source = fixture.source("shapes");
    write_fixture_plugin(&source);

    // Installed but not enabled: no group, and `orbit shapes` is the
    // ordinary unknown-command error rather than a passthrough.
    fixture
        .orbit()
        .args(["plugin", "add", source.to_str().expect("utf8 source")])
        .assert()
        .success();
    fixture
        .orbit()
        .args(["shapes", "recommend", "leakage"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand"));

    fixture
        .orbit()
        .args(["plugin", "enable", "shapes"])
        .assert()
        .success();

    let explained = fixture
        .orbit_as_operator()
        .args(["shapes", "fail", "--explain", "--format", "json"])
        .output()
        .expect("explain the derived tool call");
    assert!(
        explained.status.success(),
        "--explain must not execute the backend that exits 91: {explained:?}"
    );
    let explained = stdout_json(&explained);
    assert_eq!(explained["tool"], "shapes.fail");
    assert_eq!(
        explained["command"],
        "orbit tool run shapes.fail --input '{}'"
    );

    // `orbit <ns> --help` lists the verbs with the manifest's descriptions.
    fixture
        .orbit()
        .args(["shapes", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("recommend"))
        .stdout(predicate::str::contains("Recommend files for a query."))
        .stdout(predicate::str::contains("Rebuild the index."));

    // `orbit --help` lists the group under its own heading.
    fixture
        .orbit()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Plugins:"))
        .stdout(predicate::str::contains(
            "Every schema shape the CLI mapping covers.",
        ));

    let flags = fixture
        .orbit_as_operator()
        .args([
            "shapes",
            "recommend",
            "leakage",
            "--max-depth",
            "3",
            "--loud",
            "--mode",
            "thorough",
            "--tags",
            "rust",
            "--tags",
            "cli",
            "--filters-json",
            r#"{"since":"2026-01-01"}"#,
            "--format",
            "json",
        ])
        .output()
        .expect("run the derived group");
    assert!(flags.status.success(), "{flags:?}");

    let input = json!({
        "query": "leakage",
        "max_depth": 3,
        "loud": true,
        "mode": "thorough",
        "tags": ["rust", "cli"],
        "filters": { "since": "2026-01-01" },
    });
    let via_tool_run = fixture
        .orbit_as_operator()
        .args([
            "tool",
            "run",
            "shapes.recommend",
            "--input",
            &input.to_string(),
            "--format",
            "json",
        ])
        .output()
        .expect("run the tool directly");
    assert!(via_tool_run.status.success(), "{via_tool_run:?}");

    assert_eq!(
        stdout_json(&flags),
        stdout_json(&via_tool_run),
        "the derived flags must assemble exactly the input `--input` carries"
    );
    assert_eq!(
        stdout_json(&flags)["echo"]["input"],
        input,
        "the backend received exactly the assembled input"
    );

    // The audited operation is the same row, twice.
    let rows = fixture.audit_rows("shapes.recommend");
    let successes: Vec<_> = rows.iter().filter(|row| row.4 == "success").collect();
    assert!(successes.len() >= 2, "both spellings are audited: {rows:?}");
    assert_eq!(
        successes[0], successes[1],
        "the derived group declares the same operation as `orbit tool run`"
    );
    assert_eq!(successes[0].0, "tool");
    assert_eq!(successes[0].1, "run");
    assert_eq!(successes[0].3, "shapes.recommend");

    // `--input` overrides the flags beside it.
    let overridden = fixture
        .orbit_as_operator()
        .args([
            "shapes",
            "recommend",
            "ignored",
            "--max-depth",
            "9",
            "--input",
            r#"{"query":"explicit"}"#,
            "--format",
            "json",
        ])
        .output()
        .expect("run with --input");
    assert!(overridden.status.success(), "{overridden:?}");
    assert_eq!(
        stdout_json(&overridden)["echo"]["input"],
        json!({ "query": "explicit" })
    );

    // Dry-run reaches the same validation path as `orbit tool run --dry-run`.
    fixture
        .orbit_as_operator()
        .args(["shapes", "maintain", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("shapes.maintain"));

    // A disabled plugin's group disappears again.
    fixture
        .orbit()
        .args(["plugin", "disable", "shapes"])
        .assert()
        .success();
    fixture
        .orbit()
        .args(["shapes", "recommend", "leakage"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand"));
}

#[cfg(unix)]
#[test]
fn unmanaged_cli_can_read_a_plugin_but_cannot_run_its_mutating_tool() {
    if !orbit_exec::macos_sandbox_test_guard(
        "unmanaged_cli_can_read_a_plugin_but_cannot_run_its_mutating_tool",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let source = fixture.source("shapes");
    write_fixture_plugin(&source);
    fixture
        .orbit()
        .args(["plugin", "add", source.to_str().expect("utf8 source")])
        .assert()
        .success();
    fixture
        .orbit()
        .args(["plugin", "enable", "shapes"])
        .assert()
        .success();

    for args in [
        vec!["shapes", "recommend", "cli", "--format", "json"],
        vec![
            "tool",
            "run",
            "shapes.recommend",
            "--input",
            r#"{"query":"cli"}"#,
            "--format",
            "json",
        ],
    ] {
        let output = fixture
            .orbit()
            .env_remove("AGENT_RUN_ID")
            .args(&args)
            .output()
            .expect("run read-only plugin tool without a declared agent or operator");
        assert!(output.status.success(), "{args:?}: {output:?}");
        assert_eq!(stdout_json(&output)["echo"]["input"]["query"], "cli");
    }

    for args in [
        vec!["shapes", "maintain"],
        vec!["tool", "run", "shapes.maintain", "--input", "{}"],
    ] {
        let output = fixture
            .orbit()
            .env_remove("AGENT_RUN_ID")
            .args(&args)
            .output()
            .expect("run mutating plugin tool without a declared agent or operator");
        assert!(!output.status.success(), "{args:?}: {output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("plugin.tool.mutating"), "{stderr}");
        assert!(stderr.contains("operator or runner"), "{stderr}");
        assert!(stderr.contains("local-cli` holding [agent]"), "{stderr}");
    }
}

#[cfg(unix)]
#[test]
fn enable_prints_the_projected_refusal_and_returns_inactive_json() {
    let fixture = Fixture::new();
    let cases = [
        (
            "guarded",
            "  permissions:\n    fs:\n      write: [\"{{plugin_state}}\"]\n",
            "has not granted",
        ),
        (
            "future",
            "  requires:\n    orbit: \">=99.0.0\"\n",
            "requires orbit >=99.0.0",
        ),
    ];

    for (namespace, extra_spec, expected) in cases {
        let source = fixture.source(namespace);
        write_status_plugin(&source, namespace, extra_spec);
        fixture
            .orbit()
            .args(["plugin", "add", source.to_str().expect("utf8 source")])
            .assert()
            .success();

        fixture
            .orbit()
            .args(["plugin", "enable", namespace])
            .assert()
            .success()
            .stdout(predicate::str::contains("Plugin is inactive"))
            .stdout(predicate::str::contains(expected));

        let json = fixture
            .orbit()
            .args(["plugin", "enable", namespace, "--format", "json"])
            .output()
            .expect("enable with JSON output");
        assert!(json.status.success(), "{json:?}");
        let json = stdout_json(&json);
        assert_eq!(json["status"], "inactive", "{json}");
        assert!(
            json["diagnostic"]
                .as_str()
                .is_some_and(|diagnostic| diagnostic.contains(expected)),
            "{json}"
        );
        let tool = format!("{namespace}.status");
        let preview = fixture
            .orbit_as_operator()
            .args(["tool", "run", &tool, "--dry-run", "--format", "json"])
            .output()
            .expect("preview inactive plugin");
        assert!(preview.status.success(), "{preview:?}");
        let preview = stdout_json(&preview);
        assert_eq!(preview["policy_allowed"], false, "{preview}");
        assert_eq!(preview["policy_denial_reason"], json["diagnostic"]);
    }
}

#[cfg(unix)]
#[test]
fn enable_warns_when_a_grant_was_not_requested() {
    let fixture = Fixture::new();
    let source = fixture.source("plain");
    write_status_plugin(&source, "plain", "");
    fixture
        .orbit()
        .args(["plugin", "add", source.to_str().expect("utf8 source")])
        .assert()
        .success();

    fixture
        .orbit()
        .args(["plugin", "enable", "plain", "--grant", "fs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("warning:"))
        .stdout(predicate::str::contains("grant `fs`"))
        .stdout(predicate::str::contains("does not request"));

    let json = fixture
        .orbit()
        .args([
            "plugin", "enable", "plain", "--grant", "network", "--format", "json",
        ])
        .output()
        .expect("enable with JSON output");
    assert!(json.status.success(), "{json:?}");
    let json = stdout_json(&json);
    assert_eq!(json["status"], "active", "{json}");
    assert!(
        json["warnings"][0]
            .as_str()
            .is_some_and(|warning| warning.contains("grant `network`")
                && warning.contains("does not request")),
        "{json}"
    );
}

/// `--grant fs=data --grant fs=cache` used to record `fs=data,cache`. The next
/// process then refused the row: `cache` is not a path continuation, so the
/// plugin could not run even though enable had succeeded.
#[test]
fn enable_records_bare_fs_roots_that_load_as_the_same_roots() {
    let fixture = Fixture::new();
    let source = fixture.source("scoped");
    write_status_plugin(
        &source,
        "scoped",
        "  permissions:\n    fs:\n      read: [\"{{plugin_state}}\"]\n",
    );
    fixture
        .orbit()
        .args(["plugin", "add", source.to_str().expect("utf8 source")])
        .assert()
        .success();
    fixture
        .orbit()
        .args([
            "plugin", "enable", "scoped", "--grant", "fs=data", "--grant", "fs=cache",
        ])
        .assert()
        .success();

    let shown = fixture
        .orbit()
        .args(["plugin", "show", "scoped", "--format", "json"])
        .output()
        .expect("show the plugin from a fresh process");
    assert!(shown.status.success(), "{shown:?}");
    let shown = stdout_json(&shown);
    assert_eq!(shown["status"], "active", "{shown}");
    assert!(shown["diagnostic"].is_null(), "{shown}");
    assert_eq!(shown["granted"], json!(["fs=data,./cache"]), "{shown}");
    let permissions = shown["permissions"]
        .as_array()
        .unwrap_or_else(|| panic!("permissions: {shown}"));
    let fs = permissions
        .iter()
        .find(|row| row["grant"] == "fs")
        .unwrap_or_else(|| panic!("fs permission row: {shown}"));
    assert_eq!(fs["granted"], true, "{fs}");
    assert_eq!(fs["granted_roots"], json!(["data", "cache"]), "{fs}");
    let network = permissions
        .iter()
        .find(|row| row["grant"] == "network")
        .unwrap_or_else(|| panic!("network permission row: {shown}"));
    assert_eq!(
        network["granted"], false,
        "the stored row must not gain network: {network}"
    );
}

#[test]
fn doctor_exits_non_zero_when_a_plugin_needs_attention() {
    let fixture = Fixture::new();
    fixture
        .orbit()
        .args(["plugin", "doctor"])
        .assert()
        .success()
        .code(0);

    let source = fixture.source("future");
    write_status_plugin(&source, "future", "  requires:\n    orbit: \">=99.0.0\"\n");
    fixture
        .orbit()
        .args(["plugin", "add", source.to_str().expect("utf8 source")])
        .assert()
        .success();
    fixture
        .orbit()
        .args(["plugin", "enable", "future"])
        .assert()
        .success();

    fixture
        .orbit()
        .args(["plugin", "doctor"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("plugin(s) need attention"));
}

/// `--scope workspace` switches the plugin off here only: list/show/doctor
/// report the host state beside the effective one, doctor treats it as a
/// choice rather than a finding, and the derived group refuses with the
/// typed code instead of "unknown command".
#[cfg(unix)]
#[test]
fn a_workspace_disable_is_reported_and_refuses_cli_dry_run_with_typed_code() {
    let fixture = Fixture::new();
    let source = fixture.source("switch");
    write_status_plugin(&source, "switch", "");
    fixture
        .orbit()
        .args([
            "plugin",
            "add",
            source.to_str().expect("utf8 source"),
            "--enable",
        ])
        .assert()
        .success();
    fixture
        .orbit()
        .args(["plugin", "disable", "switch", "--scope", "workspace"])
        .assert()
        .success();

    let listed = stdout_json(
        &fixture
            .orbit()
            .args(["plugin", "list", "--format", "json"])
            .output()
            .expect("run plugin list"),
    );
    let shown = stdout_json(
        &fixture
            .orbit()
            .args(["plugin", "show", "switch", "--format", "json"])
            .output()
            .expect("run plugin show"),
    );
    let row = listed
        .as_array()
        .expect("plugin rows")
        .iter()
        .find(|row| row["name"] == "switch")
        .expect("the plugin's row")
        .clone();
    for record in [&row, &shown] {
        assert_eq!(record["status"], "disabled", "{record}");
        assert_eq!(record["host_enabled"], true, "{record}");
        assert_eq!(record["workspace_toggle"], false, "{record}");
        assert_eq!(record["disabled_by"], "workspace", "{record}");
    }

    let doctor = fixture
        .orbit()
        .args(["plugin", "doctor", "--format", "json"])
        .output()
        .expect("run plugin doctor");
    assert!(
        doctor.status.success(),
        "a switched-off plugin is not a finding: {doctor:?}"
    );
    let doctor = stdout_json(&doctor);
    let finding = doctor
        .as_array()
        .expect("doctor rows")
        .iter()
        .find(|row| row["plugin"] == "switch")
        .expect("a doctor row for the plugin");
    assert_eq!(finding["intentional"], true, "{finding}");

    let refused = fixture
        .orbit_as_operator()
        .args(["--format", "json", "switch", "status"])
        .output()
        .expect("run the derived group");
    assert!(!refused.status.success(), "{refused:?}");
    let error: Value = serde_json::from_slice(&refused.stderr).unwrap_or_else(|parse_error| {
        panic!(
            "JSON error on stderr ({parse_error}): {}",
            String::from_utf8_lossy(&refused.stderr)
        )
    });
    assert_eq!(error["code"], "plugin_disabled_in_workspace", "{error}");

    let ordinary_run = fixture
        .orbit_as_operator()
        .args(["tool", "run", "switch.status", "--format", "json"])
        .output()
        .expect("run the disabled plugin tool");
    assert!(!ordinary_run.status.success(), "{ordinary_run:?}");
    let ordinary_error: Value =
        serde_json::from_slice(&ordinary_run.stderr).unwrap_or_else(|parse_error| {
            panic!(
                "JSON error on stderr ({parse_error}): {}",
                String::from_utf8_lossy(&ordinary_run.stderr)
            )
        });

    let dry_run = fixture
        .orbit_as_operator()
        .args([
            "tool",
            "run",
            "switch.status",
            "--dry-run",
            "--format",
            "json",
        ])
        .output()
        .expect("preview the disabled plugin tool");
    assert!(!dry_run.status.success(), "{dry_run:?}");
    let dry_run_error: Value =
        serde_json::from_slice(&dry_run.stderr).unwrap_or_else(|parse_error| {
            panic!(
                "JSON error on stderr ({parse_error}): {}",
                String::from_utf8_lossy(&dry_run.stderr)
            )
        });
    assert_eq!(ordinary_error["code"], "plugin_disabled_in_workspace");
    assert_eq!(dry_run_error["code"], ordinary_error["code"]);

    // A host disable leaves nothing for a workspace enable to widen.
    fixture
        .orbit()
        .args(["plugin", "disable", "switch"])
        .assert()
        .success();
    let refused = fixture
        .orbit()
        .args([
            "--format",
            "json",
            "plugin",
            "enable",
            "switch",
            "--scope",
            "workspace",
        ])
        .output()
        .expect("run plugin enable");
    assert!(!refused.status.success(), "{refused:?}");
    let error: Value = serde_json::from_slice(&refused.stderr).unwrap_or_else(|parse_error| {
        panic!(
            "JSON error on stderr ({parse_error}): {}",
            String::from_utf8_lossy(&refused.stderr)
        )
    });
    assert_eq!(error["code"], "plugin_disabled_on_host", "{error}");
}

#[test]
fn plugin_inspection_from_unregistered_cwd_creates_no_files() {
    let fixture = Fixture::new();
    let scratch = fixture._temp.path().join("scratch");
    std::fs::create_dir(&scratch).expect("create unregistered cwd");
    let source = fixture.source("sample");
    write_status_plugin(&source, "sample", "");
    let source_arg = source.to_str().expect("utf8 source");
    fixture
        .orbit()
        .args(["plugin", "add", source_arg, "--enable"])
        .assert()
        .success();

    for args in [
        vec!["plugin", "list"],
        vec!["plugin", "validate", source_arg],
        vec!["plugin", "show", "sample"],
        vec!["plugin", "doctor"],
    ] {
        fixture
            .orbit()
            .current_dir(&scratch)
            .args(&args)
            .assert()
            .success();
        assert_eq!(
            std::fs::read_dir(&scratch)
                .expect("inspect unregistered cwd")
                .count(),
            0,
            "{args:?} created files in an unregistered cwd"
        );
    }

    let rendered = fixture
        .orbit()
        .current_dir(&scratch)
        .args([
            "plugin", "validate", source_arg, "--render", "--format", "json",
        ])
        .output()
        .expect("render plugin from unregistered cwd");
    assert!(rendered.status.success(), "{rendered:?}");
    let rendered = stdout_json(&rendered);
    assert_eq!(
        rendered["rendered"]["workspace"],
        fixture.home.join(".orbit").to_string_lossy().as_ref()
    );
    assert_eq!(
        std::fs::read_dir(&scratch)
            .expect("inspect unregistered cwd")
            .count(),
        0,
        "plugin validate --render created files in an unregistered cwd"
    );
}

#[test]
fn plugin_validate_warns_when_timeout_exceeds_the_host_ceiling() {
    let fixture = Fixture::new();
    let source = fixture.source("timeout");
    write_status_plugin(&source, "timeout", "");
    let manifest = source.join(".orbit-plugin/plugin.yaml");
    let original = std::fs::read_to_string(&manifest).expect("read manifest");
    let command = "    command: bin/backend.sh\n";

    std::fs::write(
        &manifest,
        original.replace(command, &format!("{command}    timeout_ms: 300000\n")),
    )
    .expect("set timeout at the host ceiling");
    let boundary = fixture
        .orbit()
        .args(["plugin", "validate", source.to_str().expect("utf8 source")])
        .output()
        .expect("validate timeout at the host ceiling");
    assert!(boundary.status.success(), "{boundary:?}");
    assert!(
        !String::from_utf8_lossy(&boundary.stdout).contains("backend.timeout_ms"),
        "the host ceiling itself should not warn: {}",
        String::from_utf8_lossy(&boundary.stdout)
    );

    std::fs::write(
        &manifest,
        original.replace(command, &format!("{command}    timeout_ms: 300001\n")),
    )
    .expect("set timeout above the host ceiling");
    let source_arg = source.to_str().expect("utf8 source");
    let text = fixture
        .orbit()
        .args(["plugin", "validate", source_arg])
        .output()
        .expect("validate timeout in text format");
    assert!(text.status.success(), "{text:?}");
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.contains("PLUGIN_TIMEOUT_CEILING_MS") && text.contains("300000 ms"),
        "text output must name the host ceiling: {text}"
    );

    let json = fixture
        .orbit()
        .args(["plugin", "validate", source_arg, "--format", "json"])
        .output()
        .expect("validate timeout in JSON format");
    assert!(json.status.success(), "{json:?}");
    let json = stdout_json(&json);
    assert!(
        json["warnings"]
            .as_array()
            .is_some_and(
                |warnings| warnings
                    .iter()
                    .any(|warning| warning.as_str().is_some_and(|warning| {
                        warning.contains("PLUGIN_TIMEOUT_CEILING_MS")
                            && warning.contains("300000 ms")
                    }))
            ),
        "JSON warnings must name the host ceiling: {json}"
    );
}

#[test]
fn scaffold_defaults_to_the_current_directory_and_honors_explicit_dir() {
    let fixture = Fixture::new();
    let scratch = fixture._temp.path().join("scratch");
    std::fs::create_dir(&scratch).expect("create unregistered cwd");

    fixture
        .orbit()
        .current_dir(&scratch)
        .args(["plugin", "scaffold", "local"])
        .assert()
        .success();
    assert!(scratch.join("local/.orbit-plugin/plugin.yaml").is_file());
    assert!(!scratch.join(".orbit").exists());
    assert!(!fixture.home.join(".orbit/scaffold/local").exists());

    let explicit = fixture.source("explicit");
    fixture
        .orbit()
        .current_dir(&scratch)
        .args([
            "plugin",
            "scaffold",
            "explicit",
            "--dir",
            explicit.to_str().expect("utf8 destination"),
        ])
        .assert()
        .success();
    assert!(explicit.join(".orbit-plugin/plugin.yaml").is_file());
    assert!(!scratch.join("explicit").exists());
    assert!(!scratch.join(".orbit").exists());
}

/// `orbit plugin scaffold` tells operators to run `add <dir> --enable`; that
/// one-step path used to report only the install summary and drop the seeded
/// auto-task, skill link and warnings `orbit plugin enable` reports for the
/// identical enable [ORB-12807].
#[cfg(unix)]
#[test]
fn add_enable_renders_the_seeded_skill_and_warning_report_like_enable() {
    let fixture = Fixture::new();
    let source = fixture.source("demo");
    let source_arg = source.to_str().expect("utf8 source");

    fixture
        .orbit()
        .args(["plugin", "scaffold", "demo", "--dir", source_arg])
        .assert()
        .success();

    fixture
        .orbit()
        .args(["plugin", "add", source_arg, "--enable"])
        .assert()
        .success()
        .stdout(predicate::str::contains("auto_task demo-review created"))
        .stdout(predicate::str::contains(
            "Seeded schedules are disabled; review one, then set `enabled: true` to run it.",
        ))
        .stdout(predicate::str::contains("skill demo-demo linked at"));

    let json = fixture
        .orbit()
        .args([
            "plugin", "add", source_arg, "--enable", "--force", "--format", "json",
        ])
        .output()
        .expect("re-add enabled plugin with json output");
    assert!(json.status.success(), "{json:?}");
    let json = stdout_json(&json);
    assert_eq!(json["seeded"][0]["name"], "demo-review", "{json}");
    assert_eq!(json["seeded"][0]["kind"], "auto_task", "{json}");
    assert!(
        json["skills"]
            .as_array()
            .is_some_and(|skills| skills.iter().any(|skill| skill["skill_id"] == "demo-demo")),
        "{json}"
    );
    assert_eq!(json["warnings"], serde_json::json!([]), "{json}");
}

#[cfg(unix)]
#[test]
fn overridden_root_enable_and_sync_keep_skill_links_out_of_home() {
    for (root_relative, sync) in [
        ("isolated/orbit-root", false),
        ("job/.orbit/tmp/jrun-test/orbit-root2", true),
    ] {
        let fixture = Fixture::new();
        let source = fixture.source("demo");
        let source_arg = source.to_str().expect("utf8 source");
        fixture
            .orbit()
            .args(["plugin", "scaffold", "demo", "--dir", source_arg])
            .assert()
            .success();

        let selected_root = fixture._temp.path().join(root_relative);
        let root_arg = selected_root.to_str().expect("utf8 selected root");
        let discovery_base = selected_root.parent().expect("selected root parent");
        let home_skills =
            [".agents", ".claude"].map(|provider| fixture.home.join(provider).join("skills"));
        let home_before = home_skills.clone().map(|path| {
            if path.exists() {
                Some(tree_listing(&path))
            } else {
                None
            }
        });

        fixture
            .orbit()
            .args([
                "--root",
                root_arg,
                "init",
                "--non-interactive",
                "--machine-name",
                "isolated-root-host",
                "--task-prefix",
                "ISO",
            ])
            .assert()
            .success();
        fixture
            .orbit()
            .args([
                "--root",
                root_arg,
                "workspace",
                "init",
                "--name",
                "isolated-plugin",
            ])
            .assert()
            .success();

        let action_output = if sync {
            std::fs::write(
                selected_root.join("plugins.yaml"),
                format!(
                    "schemaVersion: 1\nplugins:\n  - name: demo\n    source: {}\n    enabled: true\n",
                    source.display()
                ),
            )
            .expect("write workspace plugin pin");
            fixture
                .orbit()
                .args(["--root", root_arg, "plugin", "sync"])
                .output()
                .expect("sync pinned plugin")
        } else {
            fixture
                .orbit()
                .args(["--root", root_arg, "plugin", "add", source_arg])
                .assert()
                .success();
            fixture
                .orbit()
                .args(["--root", root_arg, "plugin", "enable", "demo"])
                .output()
                .expect("enable installed plugin")
        };
        assert!(action_output.status.success(), "{action_output:?}");

        let expected_target = selected_root.join("plugins/demo/0.1.0/skills/demo");
        for provider in [".agents", ".claude"] {
            let link = discovery_base.join(provider).join("skills/demo-demo");
            assert!(
                std::fs::symlink_metadata(&link)
                    .is_ok_and(|metadata| metadata.file_type().is_symlink()),
                "plugin skill must link beside {root_relative} at {}; output: {action_output:?}",
                link.display(),
            );
            assert_eq!(
                std::fs::read_link(&link).expect("read plugin skill link"),
                expected_target
            );
        }
        for (path, before) in home_skills.iter().zip(home_before) {
            let after = path.exists().then(|| tree_listing(path));
            assert_eq!(
                after,
                before,
                "plugin must not write under HOME at {}",
                path.display()
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn scaffold_validate_test_and_install_run_end_to_end() {
    if !orbit_exec::macos_sandbox_test_guard("scaffold_validate_test_and_install_run_end_to_end") {
        return;
    }
    let fixture = Fixture::new();
    // Installation refuses sources inside a workspace repository, so this
    // end-to-end install exercises the explicit external destination.
    let root = fixture.source("demo");
    let root_arg = root.to_str().expect("utf8 path").to_string();

    let scaffold = fixture
        .orbit()
        .args(["plugin", "scaffold", "demo", "--dir", &root_arg])
        .output()
        .expect("scaffold plugin");
    assert!(scaffold.status.success(), "{scaffold:?}");
    let scaffold_stdout = String::from_utf8_lossy(&scaffold.stdout);
    assert!(scaffold_stdout.contains("plugin.yaml"), "{scaffold_stdout}");
    assert!(root.join(".orbit-plugin/plugin.yaml").is_file());

    let rendered = fixture
        .orbit()
        .args([
            "plugin",
            "validate",
            &root_arg,
            "--render",
            "--workspace",
            fixture.work.to_str().expect("utf8 workspace"),
            "--format",
            "json",
        ])
        .output()
        .expect("render the validated backend profile");
    assert!(rendered.status.success(), "{rendered:?}");
    let rendered = stdout_json(&rendered);
    let canonical_workspace = fixture
        .work
        .canonicalize()
        .expect("canonical workspace")
        .to_string_lossy()
        .into_owned();
    assert_eq!(rendered["rendered"]["workspace"], canonical_workspace);
    assert_eq!(rendered["rendered"]["network"], "none");
    assert_eq!(
        rendered["rendered"]["environments"][0]["variables"]["ORBIT_WORKSPACE_ROOT"],
        canonical_workspace
    );

    // Exercise template substitution in both input and output plus the
    // structured error expectation through the public CLI.
    let backend = root.join(".orbit-plugin/bin/backend.py");
    let backend_text = std::fs::read_to_string(&backend).expect("read scaffold backend");
    std::fs::write(
        &backend,
        backend_text.replace(
            "    return {\n",
            "    if payload.get(\"subject\") == \"fail\":\n        raise ValueError(\"requested failure\")\n    return {\n",
        ),
    )
    .expect("add error path to scaffold backend");
    let golden = root.join(".orbit-plugin/tests/conformance/status.yaml");
    let golden_text = std::fs::read_to_string(&golden).expect("read scaffold golden");
    std::fs::write(
        &golden,
        format!(
            "{golden_text}  - name: status_renders_workspace\n    tool: status\n    input:\n      subject: \"{{{{workspace}}}}\"\n    expect:\n      output:\n        plugin: demo\n        status: ready\n        subject: \"{{{{workspace}}}}\"\n  - name: status_reports_backend_error\n    tool: status\n    input:\n      subject: fail\n    expect:\n      error:\n        code: backend_error\n"
        ),
    )
    .expect("add templated and error goldens");

    // Run every command that scaffold printed, word-for-word, while remaining
    // in the repository root. This makes the advertised `add` command an e2e
    // contract instead of a hand-maintained variant of it.
    let printed_steps: Vec<_> = scaffold_stdout
        .lines()
        .filter_map(|line| line.strip_prefix("  orbit "))
        .collect();
    assert_eq!(printed_steps.len(), 4, "{scaffold_stdout}");
    for step in printed_steps {
        if step.starts_with("demo status") {
            fixture
                .orbit_as_operator()
                .args(step.split_whitespace())
                .assert()
                .success();
        } else {
            fixture
                .orbit()
                .args(step.split_whitespace())
                .assert()
                .success();
        }
    }

    // The certification the passing run recorded is what `plugin show`
    // prints back.
    let shown = fixture
        .orbit()
        .args(["plugin", "show", "demo", "--format", "json"])
        .output()
        .expect("show the plugin");
    assert!(shown.status.success(), "{shown:?}");
    let shown = stdout_json(&shown);
    assert_eq!(
        shown["certified_orbit_version"],
        Value::String(env!("CARGO_PKG_VERSION").to_string())
    );

    // The scaffolded plugin is callable through its derived group.
    let called = fixture
        .orbit_as_operator()
        .args(["demo", "status", "conformance", "--format", "json"])
        .output()
        .expect("call the scaffolded tool");
    assert!(called.status.success(), "{called:?}");
    assert_eq!(stdout_json(&called)["subject"], "conformance");

    // A deliberately wrong expectation fails, naming the test.
    let contents = std::fs::read_to_string(&golden).expect("read the golden");
    std::fs::write(&golden, contents.replace("status: ready", "status: broken"))
        .expect("write the golden");
    fixture
        .orbit()
        .args(["plugin", "test", &root_arg])
        .assert()
        .failure()
        .stdout(predicate::str::contains("status_reports_ready"))
        .stdout(predicate::str::contains("failed"));

    fixture
        .orbit()
        .args([
            "plugin",
            "test",
            &root_arg,
            "--case",
            "status_reports_ready",
            "--update-goldens",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("updated"));
    let updated = std::fs::read_to_string(&golden).expect("read updated golden");
    assert!(updated.contains("status: ready"), "{updated}");

    // Filtering one case ignores a deliberately broken sibling and does not
    // claim that the full suite was certified.
    std::fs::write(
        &golden,
        updated.replacen("subject: conformance", "subject: still-broken", 1),
    )
    .expect("break the unselected case");
    fixture
        .orbit()
        .args([
            "plugin",
            "test",
            &root_arg,
            "--case",
            "status_reports_ready",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 of 1"));
}

#[cfg(unix)]
#[test]
fn remove_requires_confirmation_unlinks_skills_and_renders_json() {
    let fixture = Fixture::new();
    let source = fixture.source("demo");
    let source_arg = source.to_str().expect("utf8 source");

    fixture
        .orbit()
        .args(["plugin", "scaffold", "demo", "--dir", source_arg])
        .assert()
        .success();
    let added = fixture
        .orbit()
        .args(["plugin", "add", source_arg, "--enable", "--format", "json"])
        .output()
        .expect("add enabled plugin");
    assert!(added.status.success(), "{added:?}");
    let added = stdout_json(&added);
    let install_path = Path::new(added["install_path"].as_str().expect("install_path string"));
    let state_dir = fixture.home.join(".orbit/state/plugins/demo");
    std::fs::create_dir_all(&state_dir).expect("plugin state");
    std::fs::write(state_dir.join("keep.txt"), "retained").expect("plugin state sentinel");
    let link_roots = [".agents", ".claude"].map(|dir| fixture.home.join(dir).join("skills"));
    for root in &link_roots {
        assert!(
            std::fs::symlink_metadata(root.join("demo-demo"))
                .expect("plugin skill link")
                .file_type()
                .is_symlink(),
            "add --enable must link the scaffolded skill into {}",
            root.display()
        );
    }

    fixture
        .orbit()
        .args(["plugin", "remove", "demo"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("pass --yes to proceed"));

    let removed = fixture
        .orbit()
        .args(["plugin", "remove", "demo", "--yes", "--format", "json"])
        .output()
        .expect("remove plugin");
    assert!(removed.status.success(), "{removed:?}");
    let removed = stdout_json(&removed);
    assert_eq!(removed["name"], "demo");
    assert_eq!(removed["removed"], true);
    assert_eq!(removed["plugin_data_retained"], true);
    assert_eq!(removed["plugin_state_removed"], false);
    assert_eq!(
        removed["plugin_state_path"],
        state_dir.to_string_lossy().as_ref()
    );
    assert_eq!(
        std::fs::read_to_string(state_dir.join("keep.txt")).expect("state retained"),
        "retained"
    );
    for root in &link_roots {
        assert!(
            std::fs::symlink_metadata(root.join("demo-demo")).is_err(),
            "remove must not leave a demo-* link in {}",
            root.display()
        );
    }

    // Reproduce the legacy leftover before reinstalling. Its target is in the
    // same namespace's install family but names an absent prior version.
    let previous_skill = install_path
        .parent()
        .expect("plugin install family")
        .join("0.0.0/skills/demo");
    for root in &link_roots {
        std::os::unix::fs::symlink(&previous_skill, root.join("demo-demo"))
            .expect("create dangling plugin-owned link");
    }

    fixture
        .orbit()
        .args(["plugin", "add", source_arg, "--enable"])
        .assert()
        .success();
    for root in &link_roots {
        assert_eq!(
            root.join("demo-demo")
                .canonicalize()
                .expect("relinked skill resolves"),
            install_path
                .join("skills/demo")
                .canonicalize()
                .expect("reinstalled skill resolves"),
            "re-add must replace the dangling plugin-owned link in {}",
            root.display()
        );
    }

    fixture
        .orbit()
        .args(["plugin", "remove", "demo", "--yes"])
        .assert()
        .success();
}

#[test]
fn remove_prints_retained_state_path_and_purge_removes_only_that_state() {
    for purge_state in [false, true] {
        let fixture = Fixture::new();
        let source = fixture.source("demo");
        let source_arg = source.to_str().expect("utf8 source");
        fixture
            .orbit()
            .args(["plugin", "scaffold", "demo", "--dir", source_arg])
            .assert()
            .success();
        fixture
            .orbit()
            .args(["plugin", "add", source_arg])
            .assert()
            .success();
        let state_dir = fixture.home.join(".orbit/state/plugins/demo");
        let other_state = fixture.home.join(".orbit/state/plugins/other");
        let outside = fixture.home.join("notes");
        for dir in [&state_dir, &other_state, &outside] {
            std::fs::create_dir_all(dir).expect("create sentinel tree");
            std::fs::write(dir.join("keep.txt"), "keep").expect("sentinel");
        }

        let mut args = vec!["plugin", "remove", "demo", "--yes"];
        if purge_state {
            args.push("--purge-state");
        }
        let output = fixture.orbit().args(args).output().expect("remove plugin");
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).expect("utf8 output");
        assert!(
            stdout.contains(&state_dir.display().to_string()),
            "{stdout}"
        );
        assert!(
            stdout.contains(if purge_state {
                "was removed"
            } else {
                "retained at"
            }),
            "{stdout}"
        );
        assert_eq!(state_dir.exists(), !purge_state);
        for dir in [&other_state, &outside] {
            assert_eq!(
                std::fs::read_to_string(dir.join("keep.txt")).expect("sentinel survives"),
                "keep"
            );
        }
    }
}

#[test]
fn tool_scaffold_still_works_and_points_at_plugin_scaffold() {
    let fixture = Fixture::new();
    let script = fixture.home.join("legacy/hello_orbit.py");

    fixture
        .orbit()
        .args([
            "tool",
            "scaffold",
            script.to_str().expect("utf8 path"),
            "--name",
            "demo.hello",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("deprecated"))
        .stderr(predicate::str::contains("orbit plugin scaffold"))
        .stdout(predicate::str::contains("Created starter plugin"));

    assert!(script.is_file(), "the v1 executable is still written");
    assert!(
        fixture
            .home
            .join("legacy/hello_orbit.orbit-tool.yaml")
            .is_file(),
        "the v1 sidecar is still written"
    );
}

/// A backend that can write `orbit.db` can point its own row anywhere, and the
/// loader's refusal used to recommend `orbit plugin remove`, which deleted
/// whatever the row named. Run against the real binary and a real home: the
/// three verbs that touch the recorded tree refuse, the unrelated directory
/// survives, and the record-only removal the refusal recommends clears the row
/// without touching it [ORB-12800].
#[cfg(unix)]
#[test]
fn lifecycle_verbs_refuse_a_relocated_row_and_record_only_clears_it() {
    let fixture = Fixture::new();
    let source = fixture.source("demo");
    let source_arg = source.to_str().expect("utf8 source").to_string();
    fixture
        .orbit()
        .args(["plugin", "scaffold", "demo", "--dir", &source_arg])
        .assert()
        .success();
    fixture
        .orbit()
        .args(["plugin", "add", &source_arg, "--enable"])
        .assert()
        .success();

    // An operator directory Orbit never installed into, with a file that has
    // to be there afterwards.
    let sentinel = fixture.home.join("notes");
    std::fs::create_dir_all(&sentinel).expect("create the sentinel tree");
    let keep = sentinel.join("keep.txt");
    std::fs::write(&keep, "operator data").expect("write the sentinel file");
    let sentinel_arg = sentinel.to_str().expect("utf8 sentinel").to_string();

    let connection =
        Connection::open(fixture.home.join(".orbit/orbit.db")).expect("open the store");
    connection
        .execute(
            "UPDATE plugins SET install_path = ?1 WHERE name = 'demo'",
            [&sentinel_arg],
        )
        .expect("the row write succeeds; the commands are what refuse it");
    drop(connection);

    let expected = fixture
        .home
        .join(".orbit/plugins/demo")
        .to_str()
        .expect("utf8 install root")
        .to_string();
    for args in [
        vec!["plugin", "remove", "demo", "--yes"],
        vec!["plugin", "enable", "demo"],
        vec!["plugin", "disable", "demo"],
    ] {
        fixture
            .orbit()
            .args(&args)
            .assert()
            .failure()
            .stderr(predicate::str::contains(sentinel_arg.clone()))
            .stderr(predicate::str::contains(expected.clone()))
            .stderr(predicate::str::contains("--record-only"));
        assert_eq!(
            std::fs::read_to_string(&keep).expect("the sentinel file survives"),
            "operator data",
            "`orbit {}` deleted a tree this host never installed",
            args.join(" ")
        );
    }

    // The refusals left the row, so the command they recommend still has
    // something to clear.
    let removed = fixture
        .orbit()
        .args([
            "plugin",
            "remove",
            "demo",
            "--yes",
            "--record-only",
            "--format",
            "json",
        ])
        .output()
        .expect("record-only removal");
    assert!(removed.status.success(), "{removed:?}");
    let removed = stdout_json(&removed);
    assert_eq!(removed["record_only"], true);
    assert_eq!(removed["install_removed"], false);
    assert_eq!(
        std::fs::read_to_string(&keep).expect("the sentinel file survives the recovery"),
        "operator data"
    );
    fixture
        .orbit()
        .args(["plugin", "show", "demo"])
        .assert()
        .failure();
}

/// Every path beneath `root`, relative and sorted: what a tree holds, so a
/// write into it shows up as a difference.
fn tree_listing(root: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("read tree") {
            let path = entry.expect("tree entry").path();
            out.push(
                path.strip_prefix(root)
                    .expect("entry below root")
                    .to_string_lossy()
                    .into_owned(),
            );
            if path.is_dir() {
                walk(root, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create copy root");
    for entry in std::fs::read_dir(from).expect("read fixture tree") {
        let entry = entry.expect("fixture entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("fixture entry type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            // `fs::copy` keeps the permission bits, so the shim stays executable.
            std::fs::copy(entry.path(), &target).expect("copy fixture file");
        }
    }
}

/// The supported shape for a Python exec backend with third-party
/// dependencies: a committed `uv.lock`, and a `uv run --frozen` shim that
/// keeps the environment, uv's cache and interpreters under
/// `{{plugin_state}}`. Certified under the sandbox, installed through the
/// symlink walk, and re-synced by `plugin upgrade` after a lockfile change.
/// The manifest requests `network: none`, so the sandbox itself proves the
/// dependency came from the wheel inside the plugin tree and not an index.
#[cfg(unix)]
#[test]
fn a_uv_locked_python_backend_runs_from_plugin_state_and_follows_a_lockfile_upgrade() {
    if !orbit_exec::macos_sandbox_test_guard(
        "a_uv_locked_python_backend_runs_from_plugin_state_and_follows_a_lockfile_upgrade",
    ) {
        return;
    }
    let fixture = Fixture::new();
    // The fixture is a plugin root; this source is that `.orbit-plugin/`.
    let source = fixture.source("uvdemo").join(".orbit-plugin");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../orbit-tools/tests/fixtures/plugins/uv-example"),
        &source,
    );
    let source_arg = source.to_str().expect("utf8 source").to_string();
    let shipped = tree_listing(&source);

    fixture
        .orbit()
        .args(["plugin", "test", &source_arg])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 of 1"));
    assert_eq!(
        tree_listing(&source),
        shipped,
        "the conformance run must build nothing inside the plugin root"
    );

    fixture
        .orbit()
        .args(["plugin", "add", &source_arg, "--enable", "--grant", "fs"])
        .assert()
        .success();
    let call = || {
        let output = fixture
            .orbit_as_operator()
            .args(["uvdemo", "status", "--format", "json"])
            .output()
            .expect("call the uv backend");
        assert!(output.status.success(), "{output:?}");
        stdout_json(&output)
    };
    let first = call();
    assert_eq!(first["dependency_version"], "1.0.0", "{first}");
    assert_eq!(first["environment_in_plugin_state"], true, "{first}");
    assert_eq!(first["cache_in_plugin_state"], true, "{first}");
    let install_root = fixture.home.join(".orbit/plugins/uvdemo/0.1.0");
    assert_eq!(
        tree_listing(&install_root),
        shipped,
        "the installed tree holds no environment, bytecode or cache"
    );
    assert!(
        fixture
            .home
            .join(".orbit/state/plugins/uvdemo/venv")
            .is_dir(),
        "the environment lives in the plugin's state directory"
    );

    // An author moves the dependency to 2.0.0 and relocks, offline against
    // the wheel the tree already carries; the manifest itself is unchanged.
    let pyproject = source.join("pyproject.toml");
    let text = std::fs::read_to_string(&pyproject).expect("read pyproject");
    std::fs::write(
        &pyproject,
        text.replace("fixture_dep-1.0.0-py3", "fixture_dep-2.0.0-py3"),
    )
    .expect("write pyproject");
    let relock = std::process::Command::new("uv")
        .args(["lock", "--offline", "--quiet"])
        .current_dir(&source)
        .env("UV_CACHE_DIR", fixture.home.join("author-uv-cache"))
        .output()
        .expect("run uv lock");
    assert!(relock.status.success(), "{relock:?}");

    fixture
        .orbit()
        .args(["plugin", "upgrade", "uvdemo", &source_arg])
        .assert()
        .success();
    let upgraded = call();
    assert_eq!(
        upgraded["dependency_version"], "2.0.0",
        "the first call after the upgrade runs against the new lock, not the old environment: \
         {upgraded}"
    );
    assert_eq!(upgraded["environment_in_plugin_state"], true, "{upgraded}");
    assert_eq!(tree_listing(&install_root), tree_listing(&source));
}

#[cfg(unix)]
#[test]
fn migrate_legacy_sidecars_through_cli_preserves_sources_and_refuses_overwrite() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    let legacy = fixture.source("legacy-input");
    std::fs::create_dir_all(&legacy).unwrap();
    let backend = legacy.join("legacy-backend");
    let backend_bytes = b"#!/bin/sh\nprintf '{\"ok\":true}\\n'\n";
    std::fs::write(&backend, backend_bytes).unwrap();
    std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o700)).unwrap();
    let sidecar = legacy.join("legacy-status.orbit-tool.yaml");
    let sidecar_bytes =
        "schemaVersion: 1\nname: legacy.status\ndescription: Local status.\nparameters: []\n";
    std::fs::write(&sidecar, sidecar_bytes).unwrap();
    let binary = backend.to_str().unwrap();
    let printed = || {
        let output = fixture
            .orbit()
            .args(["plugin", "migrate", binary, "--format", "json"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        stdout_json(&output)
    };
    let first = printed();
    assert_eq!(first["path"], Value::Null);
    assert_eq!(printed(), first, "print-only migration is deterministic");
    assert!(!legacy.join(".orbit-plugin").exists());
    assert_eq!(std::fs::read(&backend).unwrap(), backend_bytes);
    assert_eq!(std::fs::read_to_string(&sidecar).unwrap(), sidecar_bytes);

    let destination = fixture.source("migrated");
    let destination_arg = destination.to_str().unwrap();
    let args = [
        "plugin",
        "migrate",
        binary,
        "--out-dir",
        destination_arg,
        "--version",
        "0.2.0",
        "--format",
        "json",
    ];
    let output = fixture.orbit().args(args).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let migrated = stdout_json(&output);
    let root = destination.join(".orbit-plugin");
    let manifest = root.join("plugin.yaml");
    assert_eq!(migrated["path"], manifest.to_string_lossy().as_ref());
    let yaml = std::fs::read_to_string(&manifest).unwrap();
    assert_eq!(migrated["manifest"], yaml);
    assert!(yaml.contains("version: 0.2.0"), "{yaml}");
    assert!(yaml.contains("command: bin/legacy-backend"), "{yaml}");
    assert!(yaml.contains("name: status"), "{yaml}");
    assert!(
        !yaml.contains("origin:"),
        "migration grants no first-party provenance"
    );
    let copied = root.join("bin/legacy-backend");
    assert_eq!(std::fs::read(&copied).unwrap(), backend_bytes);
    fixture
        .orbit()
        .args(["plugin", "validate", destination_arg])
        .assert()
        .success();

    fixture
        .orbit()
        .args(args)
        .assert()
        .failure()
        .stderr(predicate::str::contains("refusing to overwrite"));
    assert_eq!(std::fs::read_to_string(&manifest).unwrap(), yaml);
    assert_eq!(std::fs::read(&copied).unwrap(), backend_bytes);
    assert_eq!(std::fs::read(&backend).unwrap(), backend_bytes);
    assert_eq!(std::fs::read_to_string(&sidecar).unwrap(), sidecar_bytes);

    let invalid = legacy.join("invalid.orbit-tool.yaml");
    std::fs::write(&invalid, "schemaVersion: 2\nname: legacy.invalid\n").unwrap();
    let refused_destination = fixture.source("refused-migration");
    fixture
        .orbit()
        .args([
            "plugin",
            "migrate",
            binary,
            "--sidecar",
            invalid.to_str().unwrap(),
            "--out-dir",
            refused_destination.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("reads v1 sidecars only"));
    assert!(
        !refused_destination.exists(),
        "invalid input is rejected before writing output"
    );
    assert_eq!(std::fs::read_to_string(&manifest).unwrap(), yaml);
    assert_eq!(std::fs::read(&copied).unwrap(), backend_bytes);
}

#[test]
fn plugin_show_and_derived_groups_accept_the_global_json_shorthand() {
    let fixture = Fixture::new();
    let source = fixture.source("jsonfixture");
    write_status_plugin(&source, "jsonfixture", "");
    fixture
        .orbit()
        .args([
            "plugin",
            "add",
            source.to_str().expect("utf8 source"),
            "--enable",
        ])
        .assert()
        .success();
    for args in [
        &["plugin", "show", "jsonfixture", "--json"][..],
        &["jsonfixture", "status", "--explain", "--json"],
        &["--json", "jsonfixture", "status", "--explain"],
    ] {
        let output = fixture
            .orbit()
            .args(args)
            .output()
            .expect("run JSON shorthand");
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let document: Value = serde_json::from_slice(&output.stdout).expect("one JSON document");
        assert!(document.is_object(), "{args:?}");
    }
}

#[test]
fn plugin_json_tool_input_keeps_its_meaning_alongside_global_output_json() {
    let fixture = Fixture::new();
    let source = fixture.source("jsoninput");
    write_status_plugin(&source, "jsoninput", "");
    let manifest = source.join(".orbit-plugin/plugin.yaml");
    let mut text = std::fs::read_to_string(&manifest).expect("fixture manifest");
    text.push_str("      input_schema:\n        type: object\n        properties:\n          json: { type: boolean }\n");
    std::fs::write(manifest, text).expect("write tool-input schema");
    fixture
        .orbit()
        .args([
            "plugin",
            "add",
            source.to_str().expect("source"),
            "--enable",
        ])
        .assert()
        .success();
    for args in [
        &["--json", "jsoninput", "status", "--json", "--explain"][..],
        &["jsoninput", "--json", "status", "--json", "--explain"],
        &[
            "jsoninput",
            "status",
            "--json",
            "--format",
            "json",
            "--explain",
        ],
    ] {
        let output = fixture.orbit().args(args).output().expect("plugin explain");
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let document: Value = serde_json::from_slice(&output.stdout).expect("JSON output");
        assert_eq!(document["input"], json!({"json": true}));
    }
}
