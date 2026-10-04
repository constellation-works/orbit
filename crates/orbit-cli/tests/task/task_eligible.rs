#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::fs;

use serde_json::{Value, json};

use crate::isolated_cli_fixture;
use isolated_cli_fixture::Fixture;

fn add(fixture: &Fixture, title: &str, extra: &[&str]) -> String {
    let mut args = vec![
        "task",
        "add",
        "--title",
        title,
        "--complexity",
        "low",
        "--json",
    ];
    args.extend_from_slice(extra);
    fixture.json(&args)["id"].as_str().unwrap().to_string()
}

fn park(fixture: &Fixture, id: &str, status: &str) {
    fixture
        .command(&["task", "update", id, "--status", status, "--force"])
        .assert()
        .success();
}

fn ids(rows: &Value) -> BTreeSet<String> {
    rows.as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap().to_string())
        .collect()
}

fn conflicts_of(doc: &Value, id: &str) -> Vec<(String, String)> {
    doc["conflicting"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .unwrap_or_else(|| panic!("{id} is listed as conflicting: {doc}"))["conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|conflict| {
            (
                conflict["requested_file"].as_str().unwrap().to_string(),
                conflict["locking_task_id"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn eligible_lists_candidates_clear_of_in_flight_locks_and_explains_the_rest() {
    let fixture = Fixture::new();
    for path in ["src/held.rs", "src/free.rs", "docs/guide.md"] {
        let file = fixture.repo.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, "fixture\n").unwrap();
    }
    let seeded = ids(&fixture.json(&["task", "list", "--json"]));

    let in_progress = add(
        &fixture,
        "In-progress holder",
        &["--context", "file:src/held.rs"],
    );
    park(&fixture, &in_progress, "in-progress");
    let review = add(&fixture, "Review holder", &["--context", "dir:docs"]);
    park(&fixture, &review, "review");
    let blocked_proposed = add(
        &fixture,
        "Proposed behind in-progress",
        &["--context", "file:src/held.rs"],
    );
    let blocked_backlog = add(
        &fixture,
        "Backlog behind review",
        &["--context", "file:docs/guide.md"],
    );
    park(&fixture, &blocked_backlog, "backlog");
    // Its dependency is the in-progress holder, so it is not dependency-ready;
    // it also shares its whole surface with the proposed task below.
    // Neither gate applies to eligibility.
    let unmet_dependency = add(
        &fixture,
        "Backlog with unmet dependency",
        &[
            "--context",
            "file:src/free.rs",
            "--dependencies",
            &in_progress,
        ],
    );
    park(&fixture, &unmet_dependency, "backlog");
    let shares_surface = add(
        &fixture,
        "Proposed sharing a candidate's surface",
        &["--context", "file:src/free.rs"],
    );
    let before = fixture.json(&["task", "list", "--json"]);

    let doc = fixture.json(&["task", "eligible", "--explain", "--json"]);
    let expected: BTreeSet<String> = seeded
        .iter()
        .cloned()
        .chain([unmet_dependency.clone(), shares_surface.clone()])
        .collect();
    assert_eq!(ids(&doc["tasks"]), expected, "{doc}");
    assert_eq!(doc["total"], json!(expected.len()));
    assert_eq!(doc["truncated"], json!(false));
    assert_eq!(
        ids(&doc["conflicting"]),
        BTreeSet::from([blocked_proposed.clone(), blocked_backlog.clone()])
    );
    assert_eq!(
        conflicts_of(&doc, &blocked_proposed),
        [("file:src/held.rs".to_string(), in_progress.clone())]
    );
    assert_eq!(
        conflicts_of(&doc, &blocked_backlog),
        [("file:docs/guide.md".to_string(), review.clone())]
    );

    // The CLI document is the MCP tool's document.
    let tool = fixture.json(&[
        "tool",
        "run",
        "orbit.task.eligible",
        "--input",
        r#"{"explain":true}"#,
        "--format",
        "json",
    ]);
    assert_eq!(tool, doc);

    let backlog_only = fixture.json(&["task", "eligible", "--status", "backlog", "--json"]);
    assert_eq!(
        ids(&backlog_only["tasks"]),
        BTreeSet::from([unmet_dependency.clone()])
    );
    assert!(
        backlog_only.get("conflicting").is_none(),
        "conflicts are reported only with --explain"
    );
    let by_path = fixture.json(&["task", "eligible", "--path", "src", "--json"]);
    assert_eq!(
        ids(&by_path["tasks"]),
        BTreeSet::from([unmet_dependency.clone(), shares_surface.clone()])
    );
    let limited = fixture.json(&["task", "eligible", "--limit", "1", "--json"]);
    assert_eq!(limited["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(limited["total"], json!(expected.len()));
    assert_eq!(limited["truncated"], json!(true));

    // Holders are not candidates.
    fixture
        .command(&["task", "eligible", "--status", "review"])
        .assert()
        .failure();
    fixture
        .command(&[
            "tool",
            "run",
            "orbit.task.eligible",
            "--input",
            r#"{"status":"in-progress"}"#,
        ])
        .assert()
        .failure();

    let table = fixture
        .command(&["task", "eligible", "--explain", "--format", "table"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let table = String::from_utf8(table).unwrap();
    for id in [
        &unmet_dependency,
        &shares_surface,
        &blocked_proposed,
        &review,
    ] {
        assert!(table.contains(id.as_str()), "{id} in:\n{table}");
    }

    assert_eq!(
        fixture.json(&["task", "list", "--json"]),
        before,
        "eligibility is a read: no task changes"
    );
}
