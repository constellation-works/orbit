//! Unknown task selectors point callers to the supported source or fields.

use orbit_core::{OrbitError, OrbitRuntime};
use orbit_types::task::TASK_SHOW_PROJECTION_FIELDS_CSV;
use serde_json::{Value, json};
use tempfile::TempDir;

fn test_runtime() -> (TempDir, OrbitRuntime, String) {
    let root =
        TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).expect("create tempdir");
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    let workspace = repo.join(".orbit");
    std::fs::create_dir_all(&global).expect("create global root");
    std::fs::create_dir_all(&workspace).expect("create workspace root");
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("build test runtime");
    (root, runtime, repo.to_string_lossy().into_owned())
}

fn invalid_input_message(result: Result<Value, OrbitError>) -> String {
    match result.expect_err("unknown selector must be refused") {
        OrbitError::InvalidInput(message) => message,
        error => panic!("expected an invalid-input refusal, got {error:?}"),
    }
}

#[test]
fn task_tool_unknown_selectors_give_specific_recovery_hints() {
    let (_root, runtime, workspace) = test_runtime();
    let added = runtime
        .run_tool(
            "orbit.task.add",
            json!({
                "title": "Selector hint fixture",
                "description": "Exercise task selector validation.",
                "complexity": "low",
                "workspace": workspace.clone(),
                "model": "codex"
            }),
        )
        .expect("seed a task through the task API");
    let id = added["id"].as_str().expect("task id");

    let final_validation_error = format!(
        "unknown field selector `final_validation_commands`. Valid values: {TASK_SHOW_PROJECTION_FIELDS_CSV}. Read `final_validation_commands` only from the injected execution envelope as `task.final_validation_commands`; it is absent when empty and is not an `orbit.task.show` field."
    );
    let shown = invalid_input_message(runtime.run_tool(
        "orbit.task.show",
        json!({"id": id, "fields": ["final_validation_commands"], "model": "codex"}),
    ));
    assert_eq!(shown, final_validation_error);

    let listed = invalid_input_message(runtime.run_tool(
        "orbit.task.list",
        json!({"fields": ["final_validation_commands"], "model": "codex"}),
    ));
    assert_eq!(listed, shown, "list uses the same shared selector message");

    let added_with_invalid_projection = invalid_input_message(runtime.run_tool(
        "orbit.task.add",
        json!({
            "title": "Invalid projection",
            "description": "The invalid selector is rejected before a write.",
            "complexity": "low",
            "workspace": workspace.clone(),
            "fields": ["final_validation_commands"],
            "model": "codex"
        }),
    ));
    assert_eq!(
        added_with_invalid_projection, shown,
        "add uses the same shared selector message"
    );

    let blocked_by = invalid_input_message(runtime.run_tool(
        "orbit.task.show",
        json!({"id": id, "fields": ["blocked_by"], "model": "codex"}),
    ));
    assert_eq!(
        blocked_by,
        format!(
            "unknown field selector `blocked_by`. Valid values: {TASK_SHOW_PROJECTION_FIELDS_CSV}. Use `dependencies`, `resolved_dependencies` or `relations` instead."
        )
    );

    let tasks_before = runtime
        .run_tool(
            "orbit.task.list",
            json!({"fields": ["id"], "model": "codex"}),
        )
        .expect("list tasks before the refused add");
    let added_with_warnings = invalid_input_message(runtime.run_tool(
        "orbit.task.add",
        json!({
            "title": "Warnings projection",
            "description": "Warnings is response metadata, not a selectable field.",
            "complexity": "low",
            "workspace": workspace,
            "fields": ["warnings"],
            "model": "codex"
        }),
    ));
    assert!(
        added_with_warnings.starts_with(&format!(
            "unknown field selector `warnings`. Valid values: {TASK_SHOW_PROJECTION_FIELDS_CSV}. "
        )) && added_with_warnings.contains("omit `fields` on the original valid `orbit.task.add`"),
        "warnings refusal names the unprojected recovery: {added_with_warnings}"
    );
    assert!(
        !TASK_SHOW_PROJECTION_FIELDS_CSV
            .split(", ")
            .any(|field| field == "warnings"),
        "warnings stays out of the projection vocabulary"
    );
    let tasks_after = runtime
        .run_tool(
            "orbit.task.list",
            json!({"fields": ["id"], "model": "codex"}),
        )
        .expect("list tasks after the refused add");
    assert_eq!(tasks_before, tasks_after, "the refused add creates no task");

    let unknown = invalid_input_message(runtime.run_tool(
        "orbit.task.show",
        json!({"id": id, "fields": ["not_a_task_field"], "model": "codex"}),
    ));
    assert_eq!(
        unknown,
        format!(
            "unknown field selector `not_a_task_field`. Valid values: {TASK_SHOW_PROJECTION_FIELDS_CSV}"
        )
    );
}
