//! Required task inputs are discoverable before a write is attempted.

use crate::isolated_cli_fixture::Fixture;
use serde_json::{Value, json};

#[test]
fn task_add_reports_required_complexity_before_dispatch() {
    if crate::isolated_test::run_in_child(
        "required_inputs::task_add_reports_required_complexity_before_dispatch",
    ) {
        return;
    }
    let fixture = Fixture::new();
    for args in [vec!["task", "add"], vec!["task", "add", "--title", "x"]] {
        let output = fixture.command(&args).assert().code(2).get_output().clone();
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("--complexity"), "{error}");
        for value in ["low", "medium", "hard", "xhard"] {
            assert!(
                error.contains(value),
                "missing accepted value {value}: {error}"
            );
        }
    }
    let help = fixture
        .command(&["task", "add", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let help = String::from_utf8_lossy(&help);
    let usage = help
        .lines()
        .find(|line| line.starts_with("Usage:"))
        .unwrap();
    assert!(usage.contains("--complexity"), "{usage}");
    let created = fixture.json(&[
        "task",
        "add",
        "--title",
        "x",
        "--complexity",
        "low",
        "--json",
    ]);
    assert_eq!(created["description"], "");
    assert_eq!(created["complexity"], "low");
}

#[test]
fn tool_show_preserves_conditional_input_requirements() {
    if crate::isolated_test::run_in_child(
        "required_inputs::tool_show_preserves_conditional_input_requirements",
    ) {
        return;
    }
    let fixture = Fixture::new();
    for (tool, parameter) in [
        ("orbit.task.add", "description"),
        ("orbit.task.add", "complexity"),
        ("orbit.task.update", "expected_revision"),
        ("orbit.pipeline.invoke", "input"),
    ] {
        let shown = fixture.json(&["tool", "show", tool, "--json"]);
        assert!(shown["input_schema"]["allOf"].is_array(), "{shown}");
        let listed_parameter = shown["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|value| value["name"] == parameter)
            .unwrap();
        assert_eq!(listed_parameter["requirement"], "conditional");
        let output = fixture
            .command(&["tool", "show", tool])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let output = String::from_utf8_lossy(&output);
        let row = output
            .lines()
            .find(|line| line.starts_with(parameter))
            .unwrap();
        assert!(row.contains("conditional"), "{row}");
        assert!(output.contains("Input constraints"), "{output}");
        let detail = fixture.json(&["tool", "show", tool, "--parameter", parameter, "--json"]);
        assert_eq!(detail["requirement"], "conditional", "{detail}");
        assert_eq!(detail["input_schema"], shown["input_schema"]);
        let output = fixture
            .command(&["tool", "show", tool, "--parameter", parameter])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let output = String::from_utf8_lossy(&output);
        assert!(output.contains("conditional"), "{output}");
        assert!(output.contains("Input constraints"), "{output}");
    }
    let shown = fixture.json(&["tool", "show", "orbit.task.add", "--json"]);
    let branches = &shown["input_schema"]["allOf"][0];
    assert_eq!(branches["if"], json!({"required":["request_id"]}));
    assert_eq!(
        branches["else"]["required"],
        json!(["description", "complexity"])
    );
    assert_eq!(
        branches["then"]["required"],
        json!(["workspace", "description", "acceptance_criteria"])
    );
    assert!(
        !branches["then"]["propertyNames"]["enum"]
            .as_array()
            .unwrap()
            .contains(&json!("complexity"))
    );
    for (input, missing) in [
        (json!({"title":"x","complexity":"low"}), "description"),
        (json!({"title":"x","description":"d"}), "complexity"),
    ] {
        let output = fixture
            .command(&[
                "tool",
                "run",
                "orbit.task.add",
                "--input",
                &input.to_string(),
                "--json",
            ])
            .assert()
            .failure()
            .get_output()
            .clone();
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert!(
            error["error"].as_str().unwrap().contains(missing),
            "{error}"
        );
    }
    let mut guarded = json!({
        "workspace": fixture.repo, "request_id": "required-inputs-regression",
        "title": "Guarded creation", "description": "Created without complexity",
        "acceptance_criteria": ["Guarded creation succeeds"], "model": "codex",
    });
    fixture.json(&[
        "tool",
        "run",
        "orbit.task.add",
        "--input",
        &guarded.to_string(),
        "--json",
    ]);
    guarded["complexity"] = json!("low");
    let refused = fixture
        .command(&[
            "tool",
            "run",
            "orbit.task.add",
            "--input",
            &guarded.to_string(),
            "--json",
        ])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let refused: Value = serde_json::from_slice(&refused.stderr).unwrap();
    assert!(
        refused["error"].as_str().unwrap().contains("complexity"),
        "{refused}"
    );
}
