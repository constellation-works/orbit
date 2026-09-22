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
use tempfile::{TempDir, tempdir};

struct Fixture {
    _temp: TempDir,
    home: std::path::PathBuf,
    work: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        std::fs::create_dir_all(&home).expect("create home");
        std::fs::create_dir_all(&work).expect("create work");
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
fn write_fixture_plugin(root: &Path) {
    std::fs::create_dir_all(root.join("bin")).expect("create plugin dirs");
    let backend = root.join("bin/backend.sh");
    std::fs::write(
        &backend,
        "#!/bin/sh\ninput=$(cat)\nprintf '{\"ok\":true,\"output\":{\"echo\":%s}}\\n' \"$input\"\n",
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
"#,
    )
    .expect("write manifest");
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
fn a_derived_group_is_the_same_operation_and_result_as_tool_run() {
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
fn scaffold_validate_test_and_install_run_end_to_end() {
    let fixture = Fixture::new();
    let root = fixture.home.join(".orbit/scaffold/demo");
    let root_arg = root.to_str().expect("utf8 path").to_string();

    let scaffold = fixture
        .orbit()
        .args(["plugin", "scaffold", "demo"])
        .output()
        .expect("scaffold plugin");
    assert!(scaffold.status.success(), "{scaffold:?}");
    let scaffold_stdout = String::from_utf8_lossy(&scaffold.stdout);
    assert!(scaffold_stdout.contains("plugin.yaml"), "{scaffold_stdout}");
    assert!(
        !root.starts_with(&fixture.work),
        "the default scaffold source must be outside the workspace repository"
    );

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
    let golden = root.join("tests/conformance/status.yaml");
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
