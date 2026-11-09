//! Friction submissions project committed records into the operational feed.

use serde_json::{Value, json};

use crate::isolated_cli_fixture::Fixture;

#[test]
fn friction_submissions_emit_once_after_persistence_without_body_or_secrets() {
    let fixture = Fixture::new();
    let json = |args: &[&str]| -> Value {
        let output = fixture
            .command(args)
            .env("RUST_LOG", "warn")
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice(&output).unwrap()
    };
    let task = json(&[
        "task",
        "add",
        "--title",
        "Friction correlation",
        "--complexity",
        "low",
        "--json",
    ]);
    let task_id = task["id"].as_str().unwrap();
    let token = format!("ghp_{}", "a".repeat(36));
    let title = format!("Build exposed GITHUB_TOKEN={token}");
    let body = "Private body details that must never enter the live projection";
    let cli = json(&[
        "friction",
        "add",
        "--title",
        &title,
        "--body",
        body,
        "--model",
        "codex",
        "--during-task",
        task_id,
        "--json",
    ]);
    let input =
        json!({"action": "add", "body": "Independent report", "model": "claude"}).to_string();
    let api = json(&["tool", "run", "orbit.friction", "--input", &input]);

    // Reject both before persistence and at store validation, after a valid
    // submission, so an accidental event on a failed write adds to the count.
    for input in [
        json!({"action": "add", "action": "add", "body": "Missing identity"}),
        json!({"action": "add", "body": "Invalid tag", "model": "codex", "tags": ["unknown-tag"]}),
        json!({"body": "Missing task", "model": "codex", "during_task": "AQ-99999"}),
    ] {
        fixture
            .command(&[
                "tool",
                "run",
                "orbit.friction",
                "--input",
                &input.to_string(),
            ])
            .env("RUST_LOG", "warn")
            .assert()
            .failure();
    }

    for record in [&cli, &api] {
        let stored = json(&["friction", "show", record["id"].as_str().unwrap(), "--json"]);
        assert_eq!(stored["id"], record["id"]);
        assert_eq!(stored["title"], record["title"]);
        assert_eq!(stored["body"], record["body"]);
    }
    let records = json(&["friction", "list", "--status", "all", "--json"]);
    assert_eq!(records.as_array().unwrap().len(), 2);

    // Each CLI child drains its JSONL writer on exit, so no polling is needed.
    let log = match std::fs::read_to_string(fixture.home.join(".orbit/state/logs/orbit.jsonl")) {
        Ok(log) => log,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => panic!("read operational feed: {error}"),
    };
    let events = log
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|event| event["target"] == "orbit.friction.reported")
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 2, "one live event per committed submission");
    for (record, agent) in [(&cli, "codex"), (&api, "claude")] {
        let event = events
            .iter()
            .find(|event| event["fields"]["friction_id"] == record["id"])
            .expect("event correlated with the canonical record");
        assert_eq!(event["level"], "WARN");
        let fields = &event["fields"];
        assert!(
            fields["workspace_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
        );
        assert_eq!(
            fields["task_id"],
            record["during_task"].as_str().unwrap_or("")
        );
        assert_eq!(fields["agent"], agent);
        assert_eq!(fields["model"], record["model"]);
        assert_eq!(fields["summary"], record["title"]);
        assert!(fields.get("body").is_none());
    }
    let projected = serde_json::to_string(&events).unwrap();
    assert!(
        !projected.contains(&token),
        "the projection must redact credentials"
    );
    assert!(
        !projected.contains(body),
        "the projection must omit the record body"
    );
    assert!(cli["title"].as_str().unwrap().contains("[REDACTED_SECRET]"));
}
