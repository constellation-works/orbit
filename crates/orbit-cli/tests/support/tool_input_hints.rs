//! Real tool calls shared by CLI and MCP regression tests for the 2026-10-04 audit misses.

use serde_json::{Value, json};

pub(crate) struct HintCase {
    pub(crate) tool: &'static str,
    pub(crate) input: Value,
    pub(crate) suggestions: Vec<&'static str>,
}

pub(crate) fn cases() -> Vec<HintCase> {
    let mut cases = Vec::new();
    for tool in [
        "orbit.task.show",
        "orbit.task.update",
        "orbit.task.artifact.put",
        "orbit.task.artifact.get",
    ] {
        for field in ["task_id", "taskId"] {
            cases.push(HintCase {
                tool,
                input: json!({field: "ORB-99999999", "model": "codex"}),
                suggestions: vec!["id"],
            });
        }
    }
    // The audit also recorded artifact uploads with both task_id and name;
    // an earlier unhinted field must not hide the actionable id suggestion.
    cases.push(HintCase {
        tool: "orbit.task.artifact.put",
        input: json!({"name": "evidence.json", "task_id": "ORB-99999999", "model": "codex"}),
        suggestions: vec!["id"],
    });
    for tool in ["github.run.view", "github.run.logs"] {
        cases.push(HintCase {
            tool,
            input: json!({"run_id": "123"}),
            suggestions: vec!["run"],
        });
    }
    cases.push(HintCase {
        tool: "orbit.task.add",
        input: json!({"task_type": "bug"}),
        suggestions: vec!["type"],
    });
    cases.push(HintCase {
        tool: "orbit.task.add",
        input: json!({
            "title": "Type hint regression", "description": "Refuse fix with a bug suggestion",
            "complexity": "low", "type": "fix", "model": "codex",
        }),
        suggestions: vec!["bug"],
    });
    for (value, suggestions) in [
        ("rejected", vec!["task:rejected"]),
        ("resolved", vec!["friction:resolved"]),
        ("triaged", vec!["friction:triaged"]),
        ("open", vec!["task:open", "friction:open"]),
    ] {
        for status in [value.to_string(), format!("status:{value}")] {
            cases.push(HintCase {
                tool: "orbit.search",
                input: json!({"query": "hint regression", "status": status}),
                suggestions: suggestions.clone(),
            });
        }
    }
    cases
}

pub(crate) fn assert_hint(case: &HintCase, error: &Value, message_field: &str) {
    assert_eq!(error["code"], "invalid_input", "{}: {error}", case.tool);
    assert_eq!(
        error["did_you_mean"],
        json!(case.suggestions),
        "{} input {}: {error}",
        case.tool,
        case.input,
    );
    let message = error[message_field].as_str().expect("error message");
    for suggestion in &case.suggestions {
        assert!(
            message.contains(&format!("'{suggestion}'"))
                || message.contains(&format!("`{suggestion}`")),
            "{} must name the suggested input in its error: {error}",
            case.tool,
        );
    }
    assert!(message.contains("did you mean"), "{error}");
}
