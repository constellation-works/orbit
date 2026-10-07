#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use serde_json::Value;
use std::fs;

use crate::isolated_cli_fixture;
use isolated_cli_fixture::Fixture;

#[test]
fn task_updates_and_approval_redact_prose_before_persistence() {
    let fixture = Fixture::new();
    let token = format!("ghp_{}", "a".repeat(36));
    let text = format!("diagnostic GITHUB_TOKEN={token}");
    let safe = "diagnostic GITHUB_TOKEN=[REDACTED_SECRET]";
    let task = fixture.json(&[
        "task",
        "add",
        "--title",
        "Redaction fixture",
        "--complexity",
        "low",
        "--acceptance-criteria",
        "Persist scrubbed prose",
        "--json",
    ]);
    let id = task["id"].as_str().unwrap();
    let updated = fixture.json(&[
        "task",
        "update",
        id,
        "--title",
        &text,
        "--description",
        &text,
        "--plan",
        &text,
        "--execution-summary",
        &text,
        "--acceptance-criteria",
        &text,
        "--acceptance-criteria",
        "ordinary criterion",
        "--comment",
        &text,
        "--json",
    ]);
    for field in ["title", "description", "plan", "execution_summary"] {
        assert_eq!(updated[field], safe, "updated {field}");
    }
    assert_eq!(
        updated["acceptance_criteria"],
        serde_json::json!([safe, "ordinary criterion"])
    );
    let approved = fixture.json(&[
        "task",
        "update",
        id,
        "--approve",
        "--note",
        &text,
        "--comment",
        &text,
        "--json",
    ]);
    assert_eq!(approved["status"], "backlog");
    assert!(
        approved["history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| { event["event"] == "proposal_approved" && event["note"] == safe })
    );
    fixture.json(&[
        "task",
        "update",
        id,
        "--status",
        "in-progress",
        "--comment",
        &text,
        "--json",
    ]);
    let persisted = fixture.json(&["task", "show", id, "--json"]);
    let comments = persisted["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 3);
    assert!(comments.iter().all(|comment| comment["message"] == safe));

    // Inspect bytes in the canonical bundle, rather than trusting a reader
    // that might mask an already-persisted secret.
    let bundle = fs::read_dir(fixture.root.join("tasks/workspaces"))
        .unwrap()
        .map(|entry| entry.unwrap().path().join(id))
        .find(|path| path.is_dir())
        .expect("persisted task bundle");
    for name in [
        "task.yaml",
        "description.md",
        "plan.md",
        "execution-summary.md",
        "acceptance.md",
        "comments.jsonl",
        "events.jsonl",
    ] {
        let content = fs::read_to_string(bundle.join(name)).unwrap();
        assert!(!content.contains(&token), "task secret leaked into {name}");
        assert!(
            content.contains("[REDACTED_SECRET]"),
            "scrubbed prose missing from {name}"
        );
    }
    let audit = fixture.json(&["audit", "list", "--json"]);
    assert!(
        !audit.to_string().contains(&token),
        "task secret leaked into command audit"
    );
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
