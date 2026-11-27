#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use serde_json::{Value, json};
use std::fs;

use crate::isolated_cli_fixture;
use isolated_cli_fixture::Fixture;

#[test]
fn task_update_with_resident_mandate_activates_and_warns() {
    let fixture = Fixture::new();
    let config = fixture.root.join("config.toml");
    let mut settings = fs::read_to_string(&config).unwrap();
    settings.push_str("\n[crews.mandate-fixture]\nprovider = \"codex\"\nmodel = \"gpt-6-sol\"\n");
    fs::write(config, settings).unwrap();
    let routine = fixture.repo.join(".orbit/routines/resident_cycle.yaml");
    fs::create_dir_all(routine.parent().unwrap()).unwrap();
    fs::write(
        routine,
        "schemaVersion: 1\nname: artifact-cycle\nenabled: false\ntrigger:\n  cron: '0 * * * *'\ntarget: job:resident_cycle_pipeline\n",
    )
    .unwrap();
    let mandate = json!({
        "schema": "resident.mandate/v1",
        "goal": "Keep the release branch green",
        "exit_criteria": ["All release checks pass"],
        "workspaces": ["audit-qa"],
        "crew": "mandate-fixture",
        "cadence": {"floor_minutes": 10, "heartbeat_minutes": 120}
    });
    let attachment = format!("resident/mandate.json={mandate}");

    for status in ["proposed", "backlog"] {
        let task = fixture.json(&[
            "task",
            "add",
            "--title",
            "Release resident",
            "--status",
            status,
            "--complexity",
            "low",
            "--tag",
            "resident",
            "--crew",
            "sol",
            "--json",
        ]);
        let id = task["id"].as_str().unwrap();
        assert_eq!(task["status"], status, "{task}");
        assert_eq!(task["crew"], "sol", "{task}");
        let updated = fixture.json(&["task", "update", id, "--artifact", &attachment, "--json"]);
        assert_eq!(updated["crew"], "mandate-fixture", "{updated}");
        assert_eq!(updated["status"], "in-progress", "{updated}");
        let resident = fixture.json(&["resident", "show", id, "--json"]);
        assert_eq!(resident["status"], "in_progress", "{resident}");
        assert_eq!(resident["cycle_routine_disabled"], true, "{resident}");
        let routine_name = resident["cycle_routine_name"].as_str().unwrap();
        let warnings = updated["warnings"].as_array().unwrap();
        assert!(
            warnings.iter().any(|warning| {
                warning
                    .as_str()
                    .is_some_and(|text| text.contains(&format!("`{routine_name}` is disabled")))
            }),
            "{updated}"
        );
        assert!(updated.get("cycle_routine_disabled").is_none(), "{updated}");
        assert!(updated.get("cycle_routine_name").is_none(), "{updated}");
        assert!(updated.get("warning").is_none(), "{updated}");
    }
}

fn mandate_fixture() -> (Fixture, String) {
    let fixture = Fixture::new();
    let config = fixture.root.join("config.toml");
    let mut settings = fs::read_to_string(&config).unwrap();
    settings.push_str("\n[crews.mandate-fixture]\nprovider = \"codex\"\nmodel = \"gpt-6-sol\"\n");
    fs::write(config, settings).unwrap();
    let mandate = json!({
        "schema": "resident.mandate/v1",
        "goal": "Keep the release branch green",
        "exit_criteria": ["All release checks pass"],
        "workspaces": ["audit-qa"],
        "crew": "mandate-fixture",
        "cadence": {"floor_minutes": 10, "heartbeat_minutes": 120}
    });
    (fixture, mandate.to_string())
}

fn add_plain_task(fixture: &Fixture, status: &str, tag: &str) -> String {
    let task = fixture.json(&[
        "task",
        "add",
        "--title",
        "Release resident",
        "--status",
        status,
        "--complexity",
        "low",
        "--tag",
        tag,
        "--crew",
        "sol",
        "--json",
    ]);
    task["id"].as_str().unwrap().to_string()
}

#[test]
fn task_update_adding_resident_tag_with_mandate_syncs_crew_and_activates() {
    let (fixture, mandate) = mandate_fixture();
    let attachment = format!("resident/mandate.json={mandate}");
    for status in ["proposed", "backlog"] {
        let id = add_plain_task(&fixture, status, "ordinary");
        let updated = fixture.json(&[
            "task",
            "update",
            &id,
            "--tag",
            "resident",
            "--artifact",
            &attachment,
            "--json",
        ]);
        assert_eq!(updated["crew"], "mandate-fixture", "{updated}");
        assert_eq!(updated["status"], "in-progress", "{updated}");
    }
}

#[test]
fn task_update_removing_resident_tag_with_mandate_keeps_crew() {
    let (fixture, mandate) = mandate_fixture();
    let attachment = format!("resident/mandate.json={mandate}");
    let id = add_plain_task(&fixture, "backlog", "resident");
    let updated = fixture.json(&[
        "task",
        "update",
        &id,
        "--tag",
        "ordinary",
        "--artifact",
        &attachment,
        "--json",
    ]);
    assert_eq!(updated["crew"], "sol", "{updated}");
    assert_ne!(updated["status"], "in-progress", "{updated}");
}

#[test]
fn task_update_with_non_canonical_mandate_path_syncs_crew_and_activates() {
    let (fixture, mandate) = mandate_fixture();
    for path in ["./resident/mandate.json", "resident//mandate.json"] {
        let id = add_plain_task(&fixture, "backlog", "resident");
        let attachment = format!("{path}={mandate}");
        let updated = fixture.json(&["task", "update", &id, "--artifact", &attachment, "--json"]);
        assert_eq!(updated["crew"], "mandate-fixture", "{path}: {updated}");
        assert_eq!(updated["status"], "in-progress", "{path}: {updated}");
    }
}

#[test]
fn task_artifact_put_with_non_canonical_mandate_path_matches_control_and_refuses_traversal() {
    let (fixture, mandate) = mandate_fixture();
    let routine = fixture.repo.join(".orbit/routines/resident_cycle.yaml");
    fs::create_dir_all(routine.parent().unwrap()).unwrap();
    fs::write(
        routine,
        "schemaVersion: 1\nname: artifact-cycle\nenabled: false\ntrigger:\n  cron: '0 * * * *'\ntarget: job:resident_cycle_pipeline\n",
    )
    .unwrap();
    let source = fixture.repo.join(".orbit/tmp/mandate.json");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, &mandate).unwrap();
    let source = source.to_str().unwrap();

    let control_id = add_plain_task(&fixture, "backlog", "resident");
    let control = fixture
        .command(&[
            "task",
            "artifact",
            "put",
            &control_id,
            source,
            "--path",
            "resident/mandate.json",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let control: Value = serde_json::from_slice(&control).unwrap();

    let alternate_id = add_plain_task(&fixture, "backlog", "resident");
    let alternate = fixture
        .command(&[
            "task",
            "artifact",
            "put",
            &alternate_id,
            source,
            "--path",
            "./resident/mandate.json",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let alternate: Value = serde_json::from_slice(&alternate).unwrap();

    let control_task = fixture.json(&["task", "show", &control_id, "--json"]);
    let alternate_task = fixture.json(&["task", "show", &alternate_id, "--json"]);
    let control_resident = fixture.json(&["resident", "show", &control_id, "--json"]);
    let alternate_resident = fixture.json(&["resident", "show", &alternate_id, "--json"]);
    let disabled_warning = |result: &Value| {
        result["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .find(|warning| {
                warning.contains("resident cycle routine") && warning.contains(" is disabled")
            })
            .and_then(|warning| {
                warning
                    .split_once(", but ")
                    .map(|(_, detail)| detail.to_string())
            })
    };
    let metadata = |result: &Value, task: &Value, resident: &Value| {
        json!({
            "crew": task["crew"],
            "task_status": task["status"],
            "resident_status": resident["status"],
            "cycle_routine_disabled": resident["cycle_routine_disabled"],
            "cycle_routine_name": resident["cycle_routine_name"],
            "warning": disabled_warning(result),
        })
    };
    assert_eq!(
        metadata(&alternate, &alternate_task, &alternate_resident),
        metadata(&control, &control_task, &control_resident)
    );
    for (result, task, resident) in [
        (&control, &control_task, &control_resident),
        (&alternate, &alternate_task, &alternate_resident),
    ] {
        assert_eq!(task["crew"], "mandate-fixture", "{task}");
        assert_eq!(task["status"], "in-progress", "{task}");
        assert_eq!(resident["status"], "in_progress", "{resident}");
        assert_eq!(resident["cycle_routine_disabled"], true, "{resident}");
        let routine_name = resident["cycle_routine_name"].as_str().unwrap();
        assert!(
            disabled_warning(result)
                .is_some_and(|warning| warning.contains(&format!("`{routine_name}` is disabled"))),
            "{result}"
        );
        assert!(
            task["artifacts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|artifact| { artifact["path"] == "resident/mandate.json" }),
            "{task}"
        );
    }

    let invalid_id = add_plain_task(&fixture, "backlog", "resident");
    let before = fixture.json(&["task", "show", &invalid_id, "--json"]);
    let before_state = json!({
        "crew": before["crew"],
        "status": before["status"],
        "artifacts": before["artifacts"],
    });
    fixture
        .command(&[
            "task",
            "artifact",
            "put",
            &invalid_id,
            source,
            "--path",
            "../resident/mandate.json",
            "--json",
        ])
        .assert()
        .failure();
    let after = fixture.json(&["task", "show", &invalid_id, "--json"]);
    assert_eq!(
        json!({
            "crew": after["crew"],
            "status": after["status"],
            "artifacts": after["artifacts"],
        }),
        before_state,
        "invalid artifact paths must not write or activate the resident"
    );
    assert_eq!(after["crew"], "sol", "{after}");
    assert_eq!(after["status"], "backlog", "{after}");
}

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
        "--note",
        &text,
        "--comment",
        &text,
        "--json",
    ]);
    let persisted = fixture.json(&["task", "show", id, "--json"]);
    assert!(
        persisted["history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["event"] == "status_changed" && event["note"] == safe)
    );
    for args in [
        vec!["task", "update", id, "--note", "orphan", "--json"],
        vec![
            "task",
            "update",
            id,
            "--status",
            "in-progress",
            "--note",
            "orphan",
            "--json",
        ],
    ] {
        let output = fixture
            .command(&args)
            .assert()
            .failure()
            .get_output()
            .clone();
        let message = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            message.contains(
                "`note` requires a status change; use `comment` for free-form discussion"
            ),
            "{message}"
        );
    }
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
    // The read keeps its invocation row under the operation the run app used.
    let audited: Vec<_> = fixture
        .json(&["audit", "list", "--json"])
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["command"] == "task" && row["subcommand"] == "locks-contention")
        .cloned()
        .collect();
    assert_eq!(audited.len(), 1, "one audit row for the contention read");
    assert_eq!(audited[0]["status"], "success");
}

#[test]
fn task_admin_persistence_and_conflicts() {
    for (name, check) in [
        (
            "task_updates_and_approval_redact_prose_before_persistence",
            task_updates_and_approval_redact_prose_before_persistence as fn(),
        ),
        (
            "reservation_cli_conflicts_and_confirmation_preserve_claims",
            reservation_cli_conflicts_and_confirmation_preserve_claims as fn(),
        ),
        (
            "portable_archive_cli_round_trip_and_corrupt_input_preserve_tasks",
            portable_archive_cli_round_trip_and_corrupt_input_preserve_tasks as fn(),
        ),
        (
            "lock_contention_cli_reports_shared_backlog_surface_without_reserving",
            lock_contention_cli_reports_shared_backlog_surface_without_reserving as fn(),
        ),
    ] {
        tracing::info!(case = name, "CLI contract row");
        check();
        orbit_common::test_env::record_scenario_pass(name);
    }
}
