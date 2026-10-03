#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Reservation and portable archive behavior through isolated real CLI processes.

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
        self.command_at_root(&self.root, args)
    }

    fn command_at_root(&self, root: &std::path::Path, args: &[&str]) -> assert_cmd::Command {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("xdg"));
        if !root.as_os_str().is_empty() {
            command.arg("--root").arg(root);
        }
        command.args(args);
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
        if std::env::var_os("ORBIT_QA_TRACE_CLI").is_some() {
            eprintln!(
                "QA_CLI {}",
                serde_json::json!({
                    "test": std::thread::current().name(), "argv": args, "exit_code": 0,
                })
            );
        }
        serde_json::from_slice(&output).unwrap_or_else(|error| {
            panic!(
                "JSON for {args:?}: {error}; {}",
                String::from_utf8_lossy(&output)
            )
        })
    }
}

#[test]
fn reservation_cli_conflicts_and_confirmation_preserve_claims() {
    let fixture = Fixture::new();
    fs::write(fixture.repo.join("README.md"), "fixture\n").unwrap();
    let reserve = || {
        let output = fixture
            .command(&[
                "task",
                "locks",
                "reserve",
                "--file",
                "file:README.md",
                "--ttl",
                "1m",
                "--json",
            ])
            .env("ORBIT_OPERATOR", "1")
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice::<Value>(&output).unwrap()
    };
    let first = reserve();
    assert_eq!(first["reserved"], true);
    let id = first["reservation_id"].as_str().unwrap();
    let locks = fixture.json(&["task", "locks", "list", "--json"]);
    assert_eq!(locks["by_reservation"][0]["reservation_id"], id);
    let denied = fixture
        .command(&[
            "task",
            "locks",
            "reserve",
            "--file",
            "file:README.md",
            "--json",
        ])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .code(3)
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        serde_json::from_slice::<Value>(&denied).unwrap()["reserved"],
        false
    );
    fixture
        .command(&["task", "locks", "release", id])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .failure();
    assert_eq!(fixture.json(&["task", "locks", "list", "--json"]), locks);
    fixture
        .command(&[
            "task",
            "locks",
            "reserve",
            "--file",
            "file:README.md",
            "--ttl",
            "0s",
            "--json",
        ])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .failure();
    assert_eq!(fixture.json(&["task", "locks", "list", "--json"]), locks);
    fixture
        .command(&["task", "locks", "release", id, "--confirm"])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .success();
    assert!(
        fixture.json(&["task", "locks", "list", "--json"])["by_reservation"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(reserve()["reserved"], true);
}

#[test]
fn portable_archive_cli_round_trip_and_corrupt_input_preserve_tasks() {
    let fixture = Fixture::new();
    let archive = fixture.repo.join("tasks.tar.zst");
    let archive = archive.to_str().unwrap();
    let exported = fixture.json(&["task", "export", "--output", archive, "--all", "--json"]);
    assert_eq!(exported["count"], 1);
    let id = exported["task_ids"][0].as_str().unwrap();
    let before = fixture.json(&["task", "show", id, "--json"]);
    fixture.json(&[
        "task",
        "update",
        id,
        "--title",
        "Local changed title",
        "--json",
    ]);
    let local = fixture.json(&["task", "show", id, "--json"]);
    fixture
        .command(&["task", "import", archive, "--on-conflict", "fail", "--json"])
        .assert()
        .failure();
    assert_eq!(fixture.json(&["task", "show", id, "--json"]), local);
    let imported = fixture.json(&[
        "task",
        "import",
        archive,
        "--on-conflict",
        "renumber",
        "--json",
    ]);
    let action = imported["tasks"][0]["action"].as_str().unwrap();
    assert_eq!(action, "renumbered");
    assert_ne!(imported["tasks"][0]["final_id"], id);
    let final_id = imported["tasks"][0]["final_id"].as_str().unwrap();
    let restored = fixture.json(&["task", "show", final_id, "--json"]);
    assert_eq!(restored["title"], before["title"]);
    assert_eq!(
        restored["acceptance_criteria"],
        before["acceptance_criteria"]
    );
    assert_eq!(fixture.json(&["task", "show", id, "--json"]), local);
    let skipped = fixture.json(&["task", "import", archive, "--on-conflict", "skip", "--json"]);
    assert!(matches!(
        skipped["tasks"][0]["action"].as_str().unwrap(),
        "already-present" | "skipped"
    ));
    let corrupt = fixture.repo.join("corrupt.tar.zst");
    fs::write(&corrupt, b"invalid archive").unwrap();
    fixture
        .command(&["task", "import", corrupt.to_str().unwrap(), "--json"])
        .assert()
        .failure();
    assert_eq!(fixture.json(&["task", "show", id, "--json"]), local);
}

#[cfg(unix)]
#[test]
fn skill_cli_links_only_owned_catalog_entries_and_preserves_user_files() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let first = fixture.json(&["skill", "link", "--json"]);
    let roots = first["roots"].as_array().unwrap();
    assert_eq!(roots.len(), 2);
    let external = fixture.repo.join("user-skill");
    fs::create_dir_all(&external).unwrap();
    fs::write(external.join("SKILL.md"), "user-owned skill bytes").unwrap();
    for root in roots {
        let root = PathBuf::from(root.as_str().unwrap());
        assert!(
            root.starts_with(fixture._temp.path()),
            "fixture root must stay disposable"
        );
        fs::write(root.join("user-note.txt"), "preserve me").unwrap();
        symlink(&external, root.join("user-skill")).unwrap();
    }
    let listed = fixture.json(&["skill", "list", "--json"]);
    let selected = &listed.as_array().unwrap()[0];
    let id = selected["id"].as_str().unwrap();
    let shown = fixture.json(&["skill", "show", id, "--json"]);
    assert_eq!(shown["id"], id);
    assert_eq!(shown["content_hash"], selected["content_hash"]);
    assert!(!shown["content"].as_str().unwrap().is_empty());
    let doctor = fixture.json(&["skill", "doctor", "--json"]);
    assert!(
        doctor
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["skill_id"] == id)
    );
    fixture
        .command(&["skill", "show", "missing-fixture-skill", "--json"])
        .assert()
        .failure();
    let unlinked = fixture.json(&["skill", "unlink", "--json"]);
    assert!(unlinked["removed_count"].as_u64().unwrap() > 0);
    for root in roots {
        let root = PathBuf::from(root.as_str().unwrap());
        assert_eq!(
            fs::read_to_string(root.join("user-note.txt")).unwrap(),
            "preserve me"
        );
        assert_eq!(fs::read_link(root.join("user-skill")).unwrap(), external);
    }
    assert_eq!(
        fs::read_to_string(external.join("SKILL.md")).unwrap(),
        "user-owned skill bytes"
    );
    assert_eq!(
        fixture.json(&["skill", "unlink", "--json"])["removed_count"],
        0
    );
    assert!(
        fixture.json(&["skill", "link", "--json"])["linked_count"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(
        fixture.json(&["skill", "link", "--json"])["linked_count"],
        0
    );
    assert_eq!(fixture.json(&["skill", "show", id, "--json"]), shown);
}

#[test]
fn lock_contention_cli_reports_shared_backlog_surface_without_reserving() {
    let fixture = Fixture::new();
    fs::write(fixture.repo.join("README.md"), "contention fixture\n").unwrap();
    let mut ids = Vec::new();
    for title in ["Contending first task", "Contending second task"] {
        let task = fixture.json(&[
            "task",
            "add",
            "--title",
            title,
            "--complexity",
            "low",
            "--status",
            "backlog",
            "--context",
            "file:README.md",
            "--acceptance-criteria",
            "bounded contention",
            "--json",
        ]);
        ids.push(task["id"].as_str().unwrap().to_string());
    }
    let before = fixture.json(&["task", "locks", "list", "--json"]);
    let report = fixture.json(&["task", "locks", "contention", "--limit", "1", "--json"]);
    assert_eq!(report["pending"]["constrained"], 2);
    let hotspots = report["hotspots"].as_array().unwrap();
    assert_eq!(hotspots.len(), 1);
    assert_eq!(hotspots[0]["selector"], "file:README.md");
    assert_eq!(hotspots[0]["tasks"], 2);
    for id in ids {
        assert!(
            hotspots[0]["task_ids"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == &id)
        );
    }
    assert_eq!(
        fixture.json(&["task", "locks", "list", "--json"]),
        before,
        "contention is a diagnostic, not a reservation"
    );
}

#[cfg(unix)]
#[test]
fn external_tool_cli_lifecycle_executes_fixture_and_preserves_builtin_catalog() {
    let fixture = Fixture::new();
    let script = fixture.repo.join("fixture_tool.py");
    let script = script.to_str().unwrap();
    let name = "qa.local_echo";
    let scaffold = fixture.json(&[
        "tool", "scaffold", script, "--name", name, "--format", "json",
    ]);
    assert_eq!(scaffold["tool"], name);
    let script_bytes = fs::read(script).unwrap();
    fixture
        .command(&["tool", "scaffold", script, "--name", name])
        .assert()
        .failure();
    assert_eq!(fs::read(script).unwrap(), script_bytes);
    let added = fixture.json(&["tool", "add", script, "--format", "json"]);
    assert_eq!(added["tool"], name);
    let show = fixture.json(&["tool", "show", name, "--format", "json"]);
    assert_eq!(show["name"], name);
    assert_eq!(show["builtin"], false);
    assert_eq!(show["enabled"], true);
    assert!(
        show["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "name")
    );
    let input = r#"{"name":"Fixture","include_context":true}"#;
    let output = fixture.json(&["tool", "run", name, "--input", input, "--format", "json"]);
    assert_eq!(output["ok"], true);
    assert_eq!(output["tool"], name);
    assert_eq!(output["message"], "Hello, Fixture!");
    assert_eq!(
        PathBuf::from(output["context"]["workspace_root"].as_str().unwrap()),
        fixture.repo.canonicalize().unwrap()
    );
    fixture.json(&["tool", "disable", name, "--format", "json"]);
    assert_eq!(
        fixture.json(&["tool", "show", name, "--format", "json"])["enabled"],
        false
    );
    fixture
        .command(&["tool", "run", name, "--input", input, "--format", "json"])
        .assert()
        .failure();
    let doctor = fixture.json(&["tool", "doctor", "--format", "json"]);
    assert!(
        doctor
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["tool_name"] == name && row["status"] == "warning")
    );
    fixture.json(&["tool", "enable", name, "--format", "json"]);
    assert_eq!(
        fixture.json(&["tool", "run", name, "--input", input, "--format", "json"])["message"],
        "Hello, Fixture!"
    );
    let builtin = fixture.json(&["tool", "show", "orbit.task.list", "--format", "json"]);
    fixture
        .command(&["tool", "add", script, "--name", "orbit.task.list"])
        .assert()
        .failure();
    fixture
        .command(&["tool", "remove", "orbit.task.list"])
        .assert()
        .failure();
    assert_eq!(
        fixture.json(&["tool", "show", "orbit.task.list", "--format", "json"]),
        builtin
    );
    fixture.json(&["tool", "remove", name, "--format", "json"]);
    fixture
        .command(&["tool", "show", name, "--format", "json"])
        .assert()
        .failure();
    assert!(
        !fixture
            .json(&["tool", "list", "--json", "--all"])
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == name)
    );
    assert_eq!(
        fs::read(script).unwrap(),
        script_bytes,
        "registry removal must preserve the user's executable"
    );
}

#[test]
fn friction_cli_triage_stats_and_rehome_preserve_workspace_ownership() {
    let mut fixture = Fixture::new();
    // Cross-workspace fixtures use the shared HOME registry and distinct checkout data.
    fixture.root = PathBuf::new();
    fixture
        .command(&[
            "init",
            "--non-interactive",
            "--machine-name",
            "friction-qa",
            "--task-prefix",
            "FQ",
        ])
        .assert()
        .success();
    fixture
        .command(&["workspace", "init", "--name", "friction-source"])
        .assert()
        .success();
    let tags = fixture.json(&["friction", "tags", "--json"]);
    let tag = tags.as_array().unwrap()[0].as_str().unwrap();
    let mut created = fixture.json(&[
        "friction",
        "add",
        "--body",
        "Original fixture body",
        "--title",
        "Fixture friction",
        "--model",
        "codex",
        "--tag",
        tag,
        "--json",
    ]);
    // Creation adds redaction diagnostics; persisted reads contain the record itself.
    created.as_object_mut().unwrap().remove("redactions");
    created
        .as_object_mut()
        .unwrap()
        .remove("redactions_applied");
    let id = created["id"].as_str().unwrap();
    assert_eq!(created["status"], "open");
    assert_eq!(fixture.json(&["friction", "show", id, "--json"]), created);
    let stats = fixture.json(&["friction", "stats", "--json"]);
    assert_eq!(stats["total"], 1);
    assert_eq!(stats["open"], 1);
    let listed = fixture.json(&[
        "friction", "list", "--status", "open", "--tag", tag, "--json",
    ]);
    assert!(listed.as_array().unwrap().iter().any(|row| row["id"] == id));
    fixture
        .command(&["friction", "update", id, "--json"])
        .assert()
        .failure();
    fixture
        .command(&[
            "friction",
            "update",
            id,
            "--tag",
            "unknown-isolated-taxonomy-tag",
            "--json",
        ])
        .assert()
        .failure();
    assert_eq!(fixture.json(&["friction", "show", id, "--json"]), created);
    let mut triaged = fixture.json(&[
        "friction",
        "update",
        id,
        "--status",
        "triaged",
        "--title",
        "Triaged fixture",
        "--body",
        "Updated fixture body",
        "--json",
    ]);
    triaged.as_object_mut().unwrap().remove("redactions");
    triaged
        .as_object_mut()
        .unwrap()
        .remove("redactions_applied");
    assert_eq!(triaged["status"], "triaged");
    assert_eq!(triaged["title"], "Triaged fixture");
    assert_eq!(fixture.json(&["friction", "stats", "--json"])["triaged"], 1);
    fixture
        .command(&[
            "friction",
            "rehome",
            id,
            "--to-workspace",
            "missing-fixture-workspace",
            "--json",
        ])
        .assert()
        .failure();
    assert_eq!(fixture.json(&["friction", "show", id, "--json"]), triaged);

    let target_repo = fixture._temp.path().join("target-repo");
    fs::create_dir_all(&target_repo).unwrap();
    fixture
        .command(&["workspace", "init", "--name", "friction-target"])
        .current_dir(&target_repo)
        .assert()
        .success();
    let target = target_repo.to_str().unwrap();
    assert_eq!(
        fixture.json(&["--workspace", target, "friction", "stats", "--json"])["total"],
        0
    );
    let moved = fixture.json(&[
        "friction",
        "rehome",
        id,
        "--to-workspace",
        "friction-target",
        "--json",
    ]);
    assert_eq!(moved["status"], "resolved");
    let target_id = moved["rehomed_as"]["id"].as_str().unwrap();
    let owner = fixture.json(&[
        "--workspace",
        target,
        "friction",
        "show",
        target_id,
        "--json",
    ]);
    assert_eq!(owner["title"], "Triaged fixture");
    assert_eq!(owner["status"], "triaged");
    assert_eq!(owner["tags"], triaged["tags"]);
    assert!(
        owner["body"]
            .as_str()
            .unwrap()
            .starts_with("Updated fixture body")
    );
    assert_eq!(
        fixture.json(&["friction", "stats", "--json"])["resolved"],
        1
    );
    assert_eq!(
        fixture.json(&["--workspace", target, "friction", "stats", "--json"])["triaged"],
        1
    );
    let resolved = fixture.json(&[
        "--workspace",
        target,
        "friction",
        "resolve",
        target_id,
        "--json",
    ]);
    assert_eq!(resolved["status"], "resolved");
    assert!(resolved["resolved_at"].is_string());
    assert!(
        fixture
            .json(&[
                "--workspace",
                target,
                "friction",
                "list",
                "--status",
                "open",
                "--json"
            ])
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fixture.json(&["--workspace", target, "friction", "stats", "--json"])["resolved"],
        1
    );
    assert_eq!(
        fixture.json(&["friction", "show", id, "--json"])["status"],
        "resolved"
    );
}

#[test]
fn local_read_projections_return_registered_keys_task_flow_and_artifact_manifest() {
    let fixture = Fixture::new();
    let keys = fixture.json(&["config", "keys", "--json"]);
    let supported = keys["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["key"] == "automation.stall_window_minutes")
        .unwrap();
    assert!(
        supported["description"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    fixture
        .command(&[
            "config",
            "set",
            "--global",
            "automation.stall_window_minutes",
            "45",
        ])
        .assert()
        .success();
    assert_eq!(
        fixture.json(&["config", "get", "automation.stall_window_minutes", "--json"])["value"],
        45
    );

    let tasks = fixture.json(&["task", "list", "--json"]);
    let id = tasks.as_array().unwrap()[0]["id"].as_str().unwrap();
    let flow = fixture.json(&["task", "flow", "--window", "1d", "--buckets", "2", "--json"]);
    assert_eq!(flow["buckets"].as_array().unwrap().len(), 2);
    assert_eq!(flow["totals"]["filed"], 1);
    assert_eq!(flow["totals"]["open_now"], 1);
    assert_eq!(flow["totals"]["net"], 1);
    let filtered = fixture.json(&["task", "flow", "--tag", "absent-fixture-tag", "--json"]);
    assert_eq!(filtered["totals"]["filed"], 0);
    assert_eq!(filtered["totals"]["open_now"], 0);
    assert!(filtered["verdict"].as_str().unwrap().starts_with("no data"));

    let source = fixture.repo.join("report.txt");
    fs::write(&source, "fixture artifact\n").unwrap();
    let stored = fixture.json(&[
        "task",
        "artifact",
        "put",
        id,
        source.to_str().unwrap(),
        "--path",
        "reports/fixture.txt",
        "--json",
    ]);
    let before = fixture.json(&["task", "show", id, "--json"]);
    let manifest = fixture.json(&["artifacts", id, "--task", "--json"]);
    assert_eq!(manifest[0]["path"], stored["artifacts"][0]["path"]);
    assert_eq!(
        manifest[0]["media_type"],
        stored["artifacts"][0]["media_type"]
    );
    assert_eq!(
        manifest[0]["created_by"],
        stored["artifacts"][0]["created_by"]
    );
    assert_eq!(manifest[0]["size"], stored["artifacts"][0]["size_bytes"]);
    assert_eq!(manifest.as_array().unwrap().len(), 1);
    assert_eq!(manifest[0]["path"], "reports/fixture.txt");
    assert_eq!(manifest[0]["size"], 17);
    assert_eq!(fixture.json(&["task", "show", id, "--json"]), before);
}
