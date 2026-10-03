#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;

#[path = "support/isolated_cli_fixture.rs"]
mod isolated_cli_fixture;
use isolated_cli_fixture::Fixture;

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
