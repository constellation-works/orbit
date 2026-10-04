#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Audit behavior through the real CLI, with disposable machine/workspace state.

use std::fs;
use std::path::PathBuf;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    repo: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempdir().unwrap();
        let fixture = Self {
            home: temp.path().join("home"),
            repo: temp.path().join("repo"),
            root: temp.path().join("state"),
            _temp: temp,
        };
        fs::create_dir_all(&fixture.home).unwrap();
        fs::create_dir_all(&fixture.repo).unwrap();
        let output = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&fixture.repo)
            .output()
            .unwrap();
        assert!(output.status.success());
        fixture
            .command(&[
                "init",
                "--non-interactive",
                "--machine-name",
                "audit-qa",
                "--task-prefix",
                "AQ",
            ])
            .assert()
            .success();
        fixture
            .command(&["workspace", "init", "--name", "audit-qa"])
            .assert()
            .success();
        fixture.json(&[
            "task",
            "add",
            "--title",
            "Audit fixture",
            "--complexity",
            "low",
            "--acceptance-criteria",
            "Persist audit evidence",
            "--json",
        ]);
        fixture
    }

    fn command(&self, args: &[&str]) -> assert_cmd::Command {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("xdg"))
            .arg("--root")
            .arg(&self.root)
            .args(args);
        command
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self
            .command(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice(&output).unwrap_or_else(|error| {
            panic!(
                "JSON for {args:?}: {error}; {}",
                String::from_utf8_lossy(&output)
            )
        })
    }

    fn task_events(&self) -> Value {
        self.json(&[
            "audit", "list", "--kind", "task", "--status", "success", "--json",
        ])
    }
}

#[test]
fn audit_cli_round_trips_real_mutation_filters_stats_and_export() {
    let fixture = Fixture::new();
    let events = fixture.task_events();
    let rows = events.as_array().unwrap();
    assert!(
        !rows.is_empty(),
        "successful task creation must leave audit evidence"
    );
    assert!(
        rows.iter()
            .any(|row| row["command"] == "task" && row["subcommand"] == "add")
    );
    assert!(
        rows.iter()
            .all(|row| row["status"] == "success" && row["target_type"] == "task")
    );
    let event = &rows[0];
    let id = event["id"].as_i64().unwrap().to_string();
    assert_eq!(fixture.json(&["audit", "show", &id, "--json"]), *event);

    let workspace = fixture.json(&["workspace", "show", "--format", "json"]);
    let workspace_id = workspace["workspace"]["id"].as_str().unwrap();
    let scoped = fixture.json(&["audit", "list", "--workspace-id", workspace_id, "--json"]);
    assert!(
        scoped
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["workspace_id"] == workspace_id)
    );
    assert_eq!(
        fixture.json(&["audit", "list", "--workspace-id", "ws_absent", "--json"]),
        serde_json::json!([])
    );
    assert_eq!(
        fixture
            .json(&["audit", "list", "--limit", "1", "--json"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let stats = fixture.json(&["audit", "stats", "--json"]);
    assert!(stats["total"].as_u64().unwrap() >= rows.len() as u64);
    assert!(stats["success_count"].as_u64().unwrap() >= rows.len() as u64);
    let filtered = fixture.json(&["audit", "stats", "--tool", "absent.tool", "--json"]);
    assert_eq!(filtered["total"], 0);
    assert_eq!(filtered["denied_by_operation"], serde_json::json!([]));

    let export = fixture.repo.join("audit.json");
    fixture
        .command(&[
            "audit",
            "export",
            "--format",
            "json",
            "--output",
            export.to_str().unwrap(),
        ])
        .assert()
        .success();
    let exported: Value = serde_json::from_slice(&fs::read(export).unwrap()).unwrap();
    assert!(
        exported.as_array().unwrap().contains(event),
        "export must preserve persisted event fields"
    );
    let csv_export = fixture.repo.join("audit.csv");
    fixture
        .command(&[
            "audit",
            "export",
            "--format",
            "csv",
            "--output",
            csv_export.to_str().unwrap(),
        ])
        .assert()
        .success();
    let mut csv = csv::Reader::from_path(csv_export).unwrap();
    let id_column = csv
        .headers()
        .unwrap()
        .iter()
        .position(|column| column == "id")
        .unwrap();
    assert!(
        csv.records()
            .any(|record| record.unwrap().get(id_column) == Some(id.as_str())),
        "CSV export must preserve the audited event identity"
    );
    assert!(
        fs::read_dir(&fixture.home).unwrap().next().is_none(),
        "explicit root must keep isolated HOME untouched"
    );
}

#[test]
fn audit_cli_refusals_preserve_existing_events_and_export_bytes() {
    let fixture = Fixture::new();
    let before = fixture.task_events();
    fixture
        .command(&["audit", "show", "9223372036854775807", "--json"])
        .assert()
        .failure();
    fixture
        .command(&["audit", "list", "--since", "not-a-duration", "--json"])
        .assert()
        .failure();
    fixture
        .command(&["audit", "prune", "--older-than", "0s"])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .failure();
    assert_eq!(
        fixture.task_events(),
        before,
        "unconfirmed prune must preserve all matching task events"
    );

    let export = fixture.repo.join("retained.json");
    fs::write(&export, b"retained evidence\n").unwrap();
    fixture
        .command(&[
            "audit",
            "export",
            "--since",
            "not-a-duration",
            "--output",
            export.to_str().unwrap(),
        ])
        .assert()
        .failure();
    assert_eq!(
        fs::read(export).unwrap(),
        b"retained evidence\n",
        "invalid export arguments must be validated before opening the destination"
    );
    // Confirmation alone does not grant authority to destroy audit evidence.
    fixture
        .command(&["audit", "prune", "--older-than", "0s", "--confirm"])
        .assert()
        .failure();
    assert_eq!(fixture.task_events(), before);
    let output = fixture
        .command(&[
            "audit",
            "prune",
            "--older-than",
            "0s",
            "--confirm",
            "--format",
            "json",
        ])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let pruned: Value = serde_json::from_slice(&output).unwrap();
    assert!(pruned["pruned"].as_u64().unwrap() >= before.as_array().unwrap().len() as u64);
    assert_eq!(
        fixture.task_events(),
        serde_json::json!([]),
        "confirmed prune must remove matching persisted events"
    );
}
