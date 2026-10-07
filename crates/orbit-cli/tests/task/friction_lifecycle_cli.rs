#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;

use serde_json::json;

use crate::{git_repo, isolated_cli_fixture};
use isolated_cli_fixture::Fixture;

#[test]
fn friction_update_empty_title_restores_derivation_like_the_tool() {
    let fixture = Fixture::new();
    let created = fixture.json(&[
        "friction",
        "add",
        "--body",
        "Derived friction title\n\nDetails.",
        "--title",
        "Explicit title",
        "--model",
        "codex",
        "--json",
    ]);
    let id = created["id"].as_str().unwrap();
    let input = json!({"id": id, "title": "", "model": "codex"}).to_string();
    let tool_cleared = fixture.json(&["tool", "run", "orbit.friction.update", "--input", &input]);
    assert_eq!(tool_cleared["title"], "Derived friction title");

    for value in ["", " \t "] {
        fixture.json(&[
            "friction",
            "update",
            id,
            "--title",
            "Replacement title",
            "--json",
        ]);
        let cleared = fixture.json(&["friction", "update", id, "--title", value, "--json"]);
        assert_eq!(cleared["title"], tool_cleared["title"]);
        assert_eq!(
            fixture.json(&["friction", "show", id, "--json"])["title"],
            tool_cleared["title"]
        );
    }
}

#[test]
fn friction_update_empty_rehome_to_clears_alone_and_with_other_edits() {
    let fixture = Fixture::new();
    let created = fixture.json(&[
        "friction",
        "add",
        "--body",
        "Disposition fixture",
        "--model",
        "codex",
        "--json",
    ]);
    let id = created["id"].as_str().unwrap();
    for (value, other_flags, status) in [
        ("", vec![], "open"),
        ("", vec!["--status", "triaged"], "triaged"),
        (" \t ", vec![], "triaged"),
    ] {
        let recorded = fixture.json(&[
            "friction",
            "update",
            id,
            "--rehome-to",
            "unregistered-owner",
            "--move",
            "false",
            "--json",
        ]);
        assert_eq!(recorded["rehome_to"], "unregistered-owner");
        let mut args = vec!["friction", "update", id, "--rehome-to", value, "--json"];
        args.extend(other_flags);
        let cleared = fixture.json(&args);
        assert!(cleared["rehome_to"].is_null());
        assert_eq!(cleared["status"], status);
        let stored = fixture.json(&["friction", "show", id, "--json"]);
        assert!(stored["rehome_to"].is_null());
        assert_eq!(stored["status"], status);
    }
}

#[test]
fn friction_optional_blank_strings_without_clear_semantics_remain_omitted() {
    let fixture = Fixture::new();
    let created = fixture.json(&[
        "friction",
        "add",
        "--body",
        "Blank omission fixture",
        "--title",
        " \t ",
        "--during-task",
        " \t ",
        "--model",
        "codex",
        "--json",
    ]);
    let id = created["id"].as_str().unwrap();
    assert_eq!(created["title"], "Blank omission fixture");
    assert!(created["during_task"].is_null());
    fixture
        .command(&[
            "friction", "update", id, "--status", " \t ", "--body", " \t ", "--json",
        ])
        .assert()
        .failure();
    let updated = fixture.json(&[
        "friction", "update", id, "--status", "triaged", "--body", " \t ", "--json",
    ]);
    assert_eq!(updated["status"], "triaged");
    assert_eq!(updated["body"], created["body"]);
    assert_eq!(updated["title"], created["title"]);
    let listed = fixture.json(&[
        "friction", "list", "--status", " \t ", "--model", " \t ", "--month", " \t ", "--json",
    ]);
    assert!(listed.as_array().unwrap().iter().any(|row| row["id"] == id));
}

#[test]
fn replica_closes_legacy_local_frictions_with_audit_without_owner_mutations() {
    let mut fixture = Fixture::new();
    fixture.root = PathBuf::new();
    fixture
        .command(&[
            "init",
            "--non-interactive",
            "--machine-name",
            "legacy-friction-qa",
            "--task-prefix",
            "LF",
        ])
        .assert()
        .success();
    fixture
        .command(&["workspace", "init", "--name", "legacy-friction"])
        .assert()
        .success();
    let task = fixture.json(&[
        "task",
        "add",
        "--title",
        "Legacy friction task",
        "--complexity",
        "low",
        "--json",
    ]);
    let mut ids = Vec::new();
    for body in ["Legacy open report", "Legacy triaged report"] {
        let created = fixture.json(&[
            "friction",
            "add",
            "--body",
            body,
            "--model",
            "codex",
            "--during-task",
            task["id"].as_str().unwrap(),
            "--json",
        ]);
        ids.push(created["id"].as_str().unwrap().to_string());
    }
    fixture.json(&[
        "friction", "update", &ids[1], "--status", "triaged", "--json",
    ]);
    let before = fixture.json(&["friction", "list", "--json"]);

    // Monthly IDs may collide with unrelated records on the owner. All writes
    // must stay in the replica's local partition, including a missing-local ID.
    let owner_repo = fixture._temp.path().join("owner-repo");
    git_repo::init(&owner_repo);
    fixture
        .command(&["workspace", "init", "--name", "friction-owner"])
        .current_dir(&owner_repo)
        .assert()
        .success();
    let owner = owner_repo.to_str().unwrap();
    let mut owner_only_id = String::new();
    for _ in 0..3 {
        let created = fixture.json(&[
            "--workspace",
            owner,
            "friction",
            "add",
            "--body",
            "Unrelated owner report",
            "--model",
            "codex",
            "--json",
        ]);
        owner_only_id = created["id"].as_str().unwrap().to_string();
    }
    let owner_before = fixture.json(&["--workspace", owner, "friction", "list", "--json"]);
    assert!(
        owner_before
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == ids[0])
    );

    // Exercise the supported owner-to-replica registration change, preserving
    // the checkout and its host store instead of seeding replica rows directly.
    fixture
        .command(&["workspace", "remove", "legacy-friction"])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .success();
    fixture
        .command(&[
            "workspace",
            "init",
            "--name",
            "legacy-friction",
            "--role",
            "replica",
            "--owner",
            "hm_fixture_remote",
        ])
        .assert()
        .success();
    assert_eq!(fixture.json(&["friction", "list", "--json"]), before);

    for args in [
        vec![
            "friction",
            "add",
            "--body",
            "Refused new report",
            "--model",
            "codex",
            "--json",
        ],
        vec![
            "friction",
            "update",
            &ids[0],
            "--status",
            "triaged",
            "--body",
            "Refused edit",
            "--json",
        ],
        vec![
            "friction",
            "update",
            &ids[0],
            "--body",
            "Refused evidence-only edit",
            "--json",
        ],
        vec![
            "friction",
            "rehome",
            &ids[0],
            "--to-workspace",
            "friction-owner",
            "--json",
        ],
        vec![
            "task",
            "add",
            "--title",
            "Refused replica task",
            "--complexity",
            "low",
            "--json",
        ],
    ] {
        fixture
            .command(&args)
            .env("ORBIT_OPERATOR", "1")
            .assert()
            .failure();
        assert_eq!(fixture.json(&["friction", "list", "--json"]), before);
    }
    let refused_move = json!({
        "id": ids[0], "status": "resolved", "body": "Must remain unchanged",
        "rehome_to": "friction-owner", "model": "codex",
    })
    .to_string();
    fixture
        .command(&[
            "tool",
            "run",
            "orbit.friction.update",
            "--input",
            &refused_move,
        ])
        .assert()
        .failure();
    fixture
        .command(&["friction", "resolve", &owner_only_id, "--json"])
        .assert()
        .failure();
    assert_eq!(fixture.json(&["friction", "list", "--json"]), before);
    let prior_audit = fixture.json(&["audit", "list", "--json"]);
    let prior_audit_id = prior_audit
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["id"].as_i64())
        .max()
        .unwrap();

    let evidence = "Legacy open report\n\nDisposition: the covering fix was verified on this host.";
    let update = json!({
        "id": ids[0], "status": "resolved", "body": evidence, "model": "codex",
        "rehome_to": "hm_fixture_remote/ws_owner", "move": false,
    })
    .to_string();
    let resolved = fixture.json(&["tool", "run", "orbit.friction.update", "--input", &update]);
    assert_eq!(resolved["status"], "resolved");
    assert_eq!(resolved["body"], evidence);
    assert_eq!(resolved["rehome_to"], "hm_fixture_remote/ws_owner");
    assert_eq!(resolved["during_task"], task["id"]);
    let first_resolution = resolved["resolved_at"].as_str().unwrap();
    let resolved_again = fixture.json(&["friction", "resolve", &ids[0], "--json"]);
    assert_eq!(resolved_again["resolved_at"], first_resolution);
    let triaged_resolution = fixture.json(&["friction", "resolve", &ids[1], "--json"]);
    assert_eq!(triaged_resolution["status"], "resolved");
    assert!(triaged_resolution["resolved_at"].is_string());
    assert!(
        fixture
            .json(&["friction", "list", "--status", "open", "--json"])
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fixture.json(&["friction", "stats", "--json"])["resolved"],
        2
    );
    let after = fixture.json(&["friction", "list", "--json"]);
    for status in ["open", "triaged"] {
        fixture
            .command(&["friction", "update", &ids[0], "--status", status, "--json"])
            .assert()
            .failure();
        assert_eq!(fixture.json(&["friction", "list", "--json"]), after);
    }
    assert_eq!(
        fixture.json(&["--workspace", owner, "friction", "list", "--json"]),
        owner_before
    );

    let audit = fixture.json(&["audit", "list", "--status", "success", "--json"]);
    let rows = audit.as_array().unwrap();
    assert!(
        rows.iter().any(|row| {
            row["tool_name"] == "orbit.friction.update"
                && row["id"].as_i64().unwrap() > prior_audit_id
        }),
        "the update tool must leave durable audit evidence: {audit}"
    );
    assert!(
        rows.iter().any(|row| {
            row["command"] == "friction"
                && row["subcommand"] == "resolve"
                && row["target_id"] == ids[1]
                && row["id"].as_i64().unwrap() > prior_audit_id
        }),
        "CLI resolution must leave durable audit evidence: {audit}"
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
    git_repo::init(&target_repo);
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
