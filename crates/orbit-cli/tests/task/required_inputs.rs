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

#[test]
fn guarded_writes_name_selector_lists_accepted_fields_and_remedy() {
    if crate::isolated_test::run_in_child(
        "required_inputs::guarded_writes_name_selector_lists_accepted_fields_and_remedy",
    ) {
        return;
    }
    let fixture = Fixture::new();

    // [ORB-24434] The `request_id` descriptions must say the parameter selects
    // guarded mode and which fields that mode accepts.
    let add_detail = fixture.json(&[
        "tool",
        "show",
        "orbit.task.add",
        "--parameter",
        "request_id",
        "--json",
    ]);
    let add_description = add_detail["description"].as_str().unwrap_or_default();
    for expected in [
        "guarded creation",
        "'title'",
        "'description'",
        "'acceptance_criteria'",
        "'priority'",
        "'crew'",
    ] {
        assert!(
            add_description.contains(expected),
            "ORB-24434: add request_id description must mention {expected}: {add_description}"
        );
    }
    let update_detail = fixture.json(&[
        "tool",
        "show",
        "orbit.task.update",
        "--parameter",
        "request_id",
        "--json",
    ]);
    let update_description = update_detail["description"].as_str().unwrap_or_default();
    for expected in [
        "guarded update",
        "'verdict'",
        "'complete'",
        "'comment'",
        "'status'",
        "ordinary update",
    ] {
        assert!(
            update_description.contains(expected),
            "ORB-24434: update request_id description must mention {expected}: {update_description}"
        );
    }

    let run_tool = |tool: &str, input: &Value| -> Value {
        let output = fixture
            .command(&["tool", "run", tool, "--input", &input.to_string(), "--json"])
            .assert()
            .code(1)
            .get_output()
            .clone();
        serde_json::from_slice(&output.stderr).unwrap()
    };

    // Guarded creation carrying an ordinary field names the selector, lists the
    // guarded set, and directs the caller to drop `request_id`.
    let refused = run_tool(
        "orbit.task.add",
        &json!({
            "workspace": fixture.repo, "request_id": "guarded-create-remedy",
            "title": "Guarded", "description": "Guarded",
            "acceptance_criteria": ["Guarded"], "complexity": "low", "model": "codex",
        }),
    );
    assert_eq!(refused["code"], "invalid_input", "ORB-24434: {refused}");
    let message = refused["error"].as_str().unwrap();
    for expected in [
        "unknown field 'complexity'",
        "'request_id'",
        "guarded creation",
        "'title'",
        "'acceptance_criteria'",
        "drop 'request_id'",
        "ordinary create",
    ] {
        assert!(
            message.contains(expected),
            "ORB-24434: guarded creation refusal must mention {expected}: {message}"
        );
    }

    // The did-you-mean hint still applies on a guarded refusal.
    let hinted = run_tool(
        "orbit.task.add",
        &json!({
            "workspace": fixture.repo, "request_id": "guarded-create-hint",
            "title": "Guarded", "description": "Guarded",
            "acceptance_criteria": ["Guarded"], "titel": "typo", "model": "codex",
        }),
    );
    assert_eq!(hinted["code"], "invalid_input", "ORB-24434: {hinted}");
    let hinted_message = hinted["error"].as_str().unwrap();
    assert!(
        hinted_message.contains("guarded creation") && hinted_message.contains("'request_id'"),
        "ORB-24434: hinted guarded refusal keeps the selector: {hinted_message}"
    );
    assert!(
        hinted_message.contains("(did you mean 'title'?)"),
        "ORB-24434: hinted guarded refusal keeps the hint: {hinted_message}"
    );
    assert_eq!(
        hinted["did_you_mean"],
        json!(["title"]),
        "ORB-24434: hinted guarded refusal keeps structured hint: {hinted}"
    );

    // Guarded updates in each sub-mode refuse the same way.
    let task_id = fixture.json(&["task", "list", "--json"])[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    for (mode, mut input, accepted) in [
        (
            "edit",
            json!({
                "workspace": fixture.repo, "id": task_id, "request_id": "guarded-edit",
                "expected_revision": "test-revision", "title": "Guarded edit",
                "complexity": "low", "model": "codex",
            }),
            vec!["'status'", "'title'", "'acceptance_criteria'"],
        ),
        (
            "review",
            json!({
                "workspace": fixture.repo, "id": task_id, "request_id": "guarded-review",
                "expected_revision": "test-revision",
                "verdict": {"outcome": "approve"}, "complexity": "low", "model": "codex",
            }),
            vec!["'verdict'", "'complete'"],
        ),
        (
            "comment",
            json!({
                "workspace": fixture.repo, "id": task_id, "request_id": "guarded-comment",
                "expected_revision": "test-revision", "comment": "Guarded note",
                "complexity": "low", "model": "codex",
            }),
            vec!["'comment'"],
        ),
    ] {
        input["workspace"] = json!(fixture.repo);
        let refused = run_tool("orbit.task.update", &input);
        assert_eq!(
            refused["code"], "invalid_input",
            "ORB-24434 guarded update ({mode}): {refused}"
        );
        let message = refused["error"].as_str().unwrap();
        for expected in [
            "unknown field 'complexity'",
            "guarded update",
            "ordinary update",
            "drop 'request_id'",
        ] {
            assert!(
                message.contains(expected),
                "ORB-24434 guarded update ({mode}) must mention {expected}: {message}"
            );
        }
        assert!(
            message.contains("'request_id'") || message.contains("'expected_revision'"),
            "ORB-24434 guarded update ({mode}) must name its selector: {message}"
        );
        if mode == "review" {
            assert!(
                message.contains("'verdict'"),
                "ORB-24434 guarded update ({mode}) must name its selector: {message}"
            );
        }
        for field in accepted {
            assert!(
                message.contains(field),
                "ORB-24434 guarded update ({mode}) must list accepted {field}: {message}"
            );
        }
    }

    // `complete` without a verdict is outside the edit set, and the refusal
    // still names it as a guarded selector.
    let complete_only = run_tool(
        "orbit.task.update",
        &json!({
            "workspace": fixture.repo, "id": task_id, "request_id": "guarded-complete",
            "expected_revision": "test-revision", "title": "Guarded",
            "complete": true, "model": "codex",
        }),
    );
    let complete_message = complete_only["error"].as_str().unwrap();
    assert!(
        complete_message.contains("'complete'")
            && complete_message.contains("guarded update")
            && complete_message.contains("ordinary update"),
        "ORB-24434 guarded update (complete) must name the selector: {complete_message}"
    );
}

#[test]
fn guarded_missing_envelope_names_selector_and_remedy() {
    if crate::isolated_test::run_in_child(
        "required_inputs::guarded_missing_envelope_names_selector_and_remedy",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let input = json!({"workspace": fixture.repo, "id": "T", "verdict": {}});
    let output = fixture
        .command(&[
            "tool",
            "run",
            "orbit.task.update",
            "--input",
            &input.to_string(),
            "--json",
        ])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["code"], "invalid_input");
    let message = error["error"].as_str().unwrap();
    for expected in [
        "missing `request_id`",
        "'verdict' selects guarded update",
        "expected_revision",
        "snapshot",
        "ordinary update",
    ] {
        assert!(message.contains(expected), "missing {expected}: {message}");
    }
}
