//! Pilot receipt presentation through the built CLI with an isolated child store.

use crate::isolated_cli_fixture::Fixture;
use serde_json::json;

#[test]
fn task_show_summarizes_pilot_comments_without_changing_json_or_fallback() {
    let fixture = Fixture::new();
    let id = fixture.json(&["task", "list", "--json"])[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let assessment = json!({
        "disposition": "selectors", "confidence": "high", "recommended_crew": "fixture-crew",
        "recommended_complexity": "medium",
        "assessment_rationale": format!("Readable rationale\n{}", "évidence ".repeat(100)),
    });
    let valid = format!(
        "operation_id={}\n{}",
        "a".repeat(64),
        json!({"assessment": assessment, "complexity_before": "low"})
    );
    let malformed = format!("operation_id={}\n{{\"assessment\":broken", "b".repeat(64));
    let invalid_receipt = format!(
        "operation_id=invalid\n{}",
        json!({"assessment": assessment})
    );
    for message in [&valid, &malformed, &invalid_receipt] {
        fixture
            .command(&["task", "update", &id, "--comment", message, "--json"])
            .env("ORBIT_ACTOR", "task-pilot")
            .assert()
            .success();
    }
    fixture.json(&["task", "update", &id, "--comment", &valid, "--json"]);
    let before = fixture.json(&["task", "show", &id, "--json"]);
    let output = fixture
        .command(&["task", "show", &id])
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let output = String::from_utf8(output).unwrap();
    let comments = output.split_once("Comments:\n").unwrap().1;
    let summary = comments.lines().next().unwrap();
    assert!(summary.chars().count() <= 200, "{summary}");
    for value in [
        "selectors",
        "high",
        "fixture-crew",
        "low → medium",
        "Readable rationale",
    ] {
        assert!(
            summary.contains(value),
            "missing fixture value {value}: {summary}"
        );
    }
    assert!(!summary.contains("operation_id=") && !summary.contains("{\""));
    assert!(
        summary.ends_with('…'),
        "long UTF-8 rationale is bounded: {summary}"
    );
    assert!(
        comments.contains(&malformed),
        "malformed JSON keeps its original rendering"
    );
    assert!(comments.contains(&invalid_receipt));
    assert!(
        comments.contains(&valid),
        "another author's JSON is ordinary comment text"
    );
    assert_eq!(before["comments"][0]["message"], valid);
    assert_eq!(before["comments"][1]["message"], malformed);
    assert_eq!(fixture.json(&["task", "show", &id, "--json"]), before);
    let projected = fixture
        .command(&["task", "show", &id, "--fields", "comments"])
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(
        String::from_utf8(projected)
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .contains("Readable rationale")
    );
    assert_eq!(
        fixture.json(&["task", "show", &id, "--fields", "comments", "--json"]),
        before["comments"]
    );
}
