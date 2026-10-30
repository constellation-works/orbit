//! Condition auto-tasks through the real CLI: an enable preview, an edge
//! that files one task stamped with its origin, explain's durable tracking,
//! and an operator-only, audited clear of a storm stall.

use serde_json::{Value, json};

use crate::isolated_cli_fixture::Fixture;

fn add_condition(fixture: &Fixture, name: &str, condition: Value) {
    let input = json!({
        "name": name,
        "description": "condition fixture",
        "schedule": {"condition": condition},
        "template": {
            "title": "Follow up {{subject.key}}",
            "description": "Filed for {{subject.task.title}}",
        },
        "dedupe": "always",
    })
    .to_string();
    fixture
        .command(&["tool", "run", "orbit.auto_task.add", "--input", &input])
        .assert()
        .success();
}

fn add_task(fixture: &Fixture, title: &str) -> String {
    let added = fixture.json(&[
        "task",
        "add",
        "--title",
        title,
        "--complexity",
        "low",
        "--acceptance-criteria",
        "Condition fixture",
        "--json",
    ]);
    added["id"].as_str().unwrap().to_string()
}

fn tag(fixture: &Fixture, id: &str, tag: &str) {
    fixture
        .command(&["task", "update", id, "--tag", tag, "--json"])
        .assert()
        .success();
}

fn tick(fixture: &Fixture, name: &str) -> Value {
    let report = fixture.json(&["clock", "tick", "--format", "json"]);
    report["reports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == name)
        .unwrap_or_else(|| panic!("{name} reported: {report}"))
        .clone()
}

fn minted(fixture: &Fixture, name: &str) -> Vec<Value> {
    let listed = fixture.json(&[
        "task",
        "list",
        "--tag",
        &format!("auto-task:{name}"),
        "--json",
    ]);
    listed
        .as_array()
        .or_else(|| listed["tasks"].as_array())
        .cloned()
        .unwrap_or_default()
}

#[test]
fn a_task_edge_files_one_stamped_task_and_explain_tracks_it() {
    const TEST: &str =
        "condition_auto_task_cli::a_task_edge_files_one_stamped_task_and_explain_tracks_it";
    if crate::isolated_test::run_in_child(TEST) {
        return;
    }
    let fixture = Fixture::new();
    let subject = add_task(&fixture, "Ship the release notes");
    add_condition(
        &fixture,
        "release-followup",
        json!({"scope": "task", "when": "'release' in task.tags"}),
    );

    let preview = fixture.json(&[
        "auto-task",
        "toggle",
        "release-followup",
        "on",
        "--dry-run",
        "--json",
    ]);
    assert_eq!(preview["matched"], 0, "{preview}");
    assert_eq!(preview["refused"], false, "{preview}");
    let shown = fixture.json(&["auto-task", "show", "release-followup", "--json"]);
    assert_eq!(
        shown["enabled"], false,
        "a dry run enables nothing: {shown}"
    );

    fixture.json(&["auto-task", "toggle", "release-followup", "on", "--json"]);
    let first = tick(&fixture, "release-followup");
    assert_eq!(first["action"], "condition", "{first}");
    assert!(minted(&fixture, "release-followup").is_empty());

    tag(&fixture, &subject, "release");
    let fired = tick(&fixture, "release-followup");
    assert_eq!(fired["reason"], "fired", "{fired}");
    let tasks = minted(&fixture, "release-followup");
    assert_eq!(tasks.len(), 1, "{tasks:?}");
    let child = tasks[0]["id"].as_str().unwrap().to_string();
    assert_eq!(
        tasks[0]["title"],
        format!("[auto-task] Follow up {subject}")
    );

    let history = fixture.json(&["task", "show", &child, "--json"]);
    let origin = history["history"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["event"] == "condition_fired")
        .unwrap_or_else(|| panic!("origin stamped: {history}"));
    let origin: Value = serde_json::from_str(origin["note"].as_str().unwrap()).unwrap();
    assert_eq!(origin["trigger"], "auto-task:release-followup");
    assert_eq!(origin["depth"], 1);
    assert_eq!(origin["subject"], subject.as_str());

    // Still tagged: one edge, one task.
    tick(&fixture, "release-followup");
    assert_eq!(minted(&fixture, "release-followup").len(), 1);

    let explained = fixture.json(&["automation", "explain", "release-followup", "--json"]);
    let fires = explained["tracking"]["recent_fires"].as_array().unwrap();
    assert_eq!(fires.len(), 1, "{explained}");
    assert_eq!(fires[0]["action_id"], child.as_str());
    assert_eq!(fires[0]["state"], "acknowledged");
    let matched = explained["matches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["subject"] == subject.as_str())
        .unwrap_or_else(|| panic!("{explained}"));
    assert_eq!(matched["arm"], "fired");

    // Disabling and re-enabling keeps `self` and the subject's fired state.
    fixture.json(&["auto-task", "toggle", "release-followup", "off", "--json"]);
    tick(&fixture, "release-followup");
    fixture.json(&["auto-task", "toggle", "release-followup", "on", "--json"]);
    tick(&fixture, "release-followup");
    assert_eq!(minted(&fixture, "release-followup").len(), 1);
    let explained = fixture.json(&["automation", "explain", "release-followup", "--json"]);
    assert!(
        explained["tracking"]["self"]["last_fired"].is_string(),
        "{explained}"
    );
}

#[test]
fn excess_matches_refuse_enable_and_a_storm_stall_clears_only_by_an_operator() {
    const TEST: &str = "condition_auto_task_cli::excess_matches_refuse_enable_and_a_storm_stall_clears_only_by_an_operator";
    if crate::isolated_test::run_in_child(TEST) {
        return;
    }
    let fixture = Fixture::new();
    let tasks = [
        add_task(&fixture, "First storm subject"),
        add_task(&fixture, "Second storm subject"),
    ];
    add_condition(
        &fixture,
        "storm",
        json!({"scope": "task", "when": "'storm' in task.tags", "max_matches": 1}),
    );
    for id in &tasks {
        tag(&fixture, id, "storm");
    }
    fixture
        .command(&["auto-task", "toggle", "storm", "on", "--json"])
        .assert()
        .failure();
    let shown = fixture.json(&["auto-task", "show", "storm", "--json"]);
    assert_eq!(shown["enabled"], false, "{shown}");

    for id in &tasks {
        tag(&fixture, id, "calm");
    }
    fixture.json(&["auto-task", "toggle", "storm", "on", "--json"]);
    tick(&fixture, "storm");
    for id in &tasks {
        tag(&fixture, id, "storm");
    }
    let stalled = tick(&fixture, "storm");
    assert_eq!(stalled["reason"], "match_storm", "{stalled}");
    assert!(minted(&fixture, "storm").is_empty());

    fixture
        .command(&[
            "automation",
            "clear-stall",
            "storm",
            "--reason",
            "fixture storm",
        ])
        .assert()
        .failure();
    let explained = fixture.json(&["automation", "explain", "storm", "--json"]);
    assert_eq!(
        explained["tracking"]["stall"]["reason"], "match_storm",
        "a non-operator cannot clear a stall: {explained}"
    );
    let cleared = fixture
        .command(&[
            "automation",
            "clear-stall",
            "storm",
            "--reason",
            "fixture storm",
            "--json",
        ])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let cleared: Value = serde_json::from_slice(&cleared).unwrap();
    assert_eq!(cleared["cleared"]["reason"], "match_storm", "{cleared}");
    let explained = fixture.json(&["automation", "explain", "storm", "--json"]);
    assert!(explained["tracking"]["stall"].is_null(), "{explained}");
}
