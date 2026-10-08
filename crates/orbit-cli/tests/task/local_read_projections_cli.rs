#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;

use crate::isolated_cli_fixture;
use isolated_cli_fixture::Fixture;

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

    let before_invalid_flow = fixture.json(&["task", "show", id, "--json"]);
    fixture
        .command(&["task", "flow", "--window", "invalid-duration", "--json"])
        .assert()
        .failure();
    assert_eq!(
        fixture.json(&["task", "show", id, "--json"]),
        before_invalid_flow
    );

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

#[test]
fn task_show_formats_status_history_and_whole_second_timestamps_without_changing_json() {
    let fixture = Fixture::new();
    let id = fixture.json(&["task", "list", "--json"])[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let changed = fixture.json(&["task", "update", &id, "--status", "backlog", "--json"]);
    assert!(changed["history"].as_array().unwrap().iter().any(|entry| {
        entry["event"] == "status_changed"
            && entry["from_status"] == "proposed"
            && entry["to_status"] == "backlog"
    }));
    fixture.json(&[
        "task",
        "update",
        &id,
        "--comment",
        "timestamp comment",
        "--json",
    ]);

    let before = fixture
        .command(&["task", "show", &id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human = fixture
        .command(&["task", "show", &id])
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human = String::from_utf8(human).unwrap();
    assert!(human.contains("status proposed → backlog"), "{human}");
    assert!(human.contains("timestamp comment"), "{human}");
    assert!(
        human
            .lines()
            .any(|line| line.starts_with("Created: ") && line.ends_with('Z'))
    );
    assert!(
        human
            .lines()
            .any(|line| line.starts_with("Updated: ") && line.ends_with('Z'))
    );
    assert!(
        human
            .lines()
            .any(|line| line.starts_with("  [") && line.contains("timestamp comment"))
    );
    for line in human.lines() {
        if let Some(rest) = line.trim_start().strip_prefix('[')
            && let Some((stamp, _)) = rest.split_once(']')
        {
            assert_eq!(
                stamp.len(),
                20,
                "timestamp must have seconds and a zone: {line}"
            );
            assert!(
                !stamp.contains('.'),
                "timestamp must not have fractions: {line}"
            );
            assert!(
                stamp.ends_with('Z'),
                "timestamp must include a zone: {line}"
            );
        }
        for label in ["Created: ", "Updated: "] {
            if let Some(stamp) = line.strip_prefix(label) {
                assert_eq!(
                    stamp.len(),
                    20,
                    "timestamp must have seconds and a zone: {line}"
                );
                assert!(
                    !stamp.contains('.'),
                    "timestamp must not have fractions: {line}"
                );
                assert!(
                    stamp.ends_with('Z'),
                    "timestamp must include a zone: {line}"
                );
            }
        }
    }

    let after = fixture
        .command(&["task", "show", &id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        before, after,
        "human rendering must leave the JSON bytes unchanged"
    );
}
