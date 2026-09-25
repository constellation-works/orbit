use super::*;

#[test]
fn runtime_free_command_set_is_derived_from_operations() {
    let runtime_free: &[&[&str]] = &[
        &["orbit", "init"],
        &["orbit", "workspace", "init"],
        &["orbit", "mcp", "init"],
        &["orbit", "mcp", "remove"],
        &["orbit", "mcp", "serve"],
        &["orbit", "migrate"],
        &["orbit", "migrate", "--dry-run"],
        &["orbit", "run", "ship-sweep", "--dry-run"],
        &["orbit", "sweep", "--dry-run"],
        &["orbit", "routine", "list"],
        &["orbit", "web", "serve", "--no-open"],
        &["orbit", "web", "connect", "example.test", "--no-open"],
    ];

    for args in runtime_free {
        assert_eq!(
            operation_for(args).runtime_need,
            RuntimeNeed::Forbidden,
            "{args:?} must not bootstrap a workspace runtime"
        );
    }

    let runtime_required: &[&[&str]] = &[
        &["orbit", "migrate", "--confirm"],
        &["orbit", "task", "lint", "--restore-pruned"],
        &["orbit", "task", "update", "ORB-10200"],
    ];
    for args in runtime_required {
        assert_eq!(
            operation_for(args).runtime_need,
            RuntimeNeed::Required,
            "{args:?} must bootstrap a workspace runtime"
        );
    }
    assert_eq!(
        operation_for(&["orbit", "config", "set", "machine.name", "new"]).runtime_need,
        RuntimeNeed::Required
    );
}

#[test]
fn observation_commands_use_the_read_only_runtime() {
    let runtime_read_only: &[&[&str]] = &[
        &["orbit", "workspace", "list"],
        &["orbit", "workspace", "show"],
        &["orbit", "task", "list"],
        &["orbit", "task", "show", "ORB-10200"],
        &["orbit", "task", "flow"],
        &["orbit", "task", "lint"],
        &["orbit", "auto-task", "list"],
        &["orbit", "auto-task", "show", "daily"],
        &["orbit", "tool", "list"],
        &["orbit", "search", "registry"],
        &["orbit", "run", "history"],
        &["orbit", "run", "show"],
        &["orbit", "friction", "list"],
    ];

    for args in runtime_read_only {
        assert_eq!(
            operation_for(args).runtime_need,
            RuntimeNeed::ReadOnly,
            "{args:?} must use the read-only runtime"
        );
    }

    assert_eq!(
        operation_for(&["orbit", "task", "show", "ORB-10200"])
            .task_owner_id
            .as_deref(),
        Some("ORB-10200")
    );
    assert_eq!(
        operation_for(&[
            "orbit", "friction", "add", "--body", "note", "--model", "codex",
        ])
        .runtime_need,
        RuntimeNeed::Required
    );
}

#[test]
fn migrate_only_bootstraps_the_applying_form() {
    assert_eq!(
        operation_for(&["orbit", "migrate"]).runtime_need,
        RuntimeNeed::Forbidden
    );
    assert_eq!(
        operation_for(&["orbit", "migrate", "--dry-run"]).runtime_need,
        RuntimeNeed::Forbidden
    );
    assert_eq!(
        operation_for(&["orbit", "migrate", "--confirm"]).runtime_need,
        RuntimeNeed::Required
    );
}

#[test]
fn tool_run_task_show_bootstraps_the_task_owner_from_id_only_input() {
    assert_eq!(
        operation_for(&[
            "orbit",
            "tool",
            "run",
            "orbit.task.show",
            "--input",
            r#"{"id":"ORB-10961","model":"codex"}"#,
        ])
        .runtime_need,
        RuntimeNeed::TaskOwner {
            task_id: "ORB-10961".to_string()
        }
    );
    assert_eq!(
        operation_for(&["orbit", "tool", "run", "orbit.task.show"]).runtime_need,
        RuntimeNeed::Required
    );
    assert_eq!(
        operation_for(&[
            "orbit",
            "tool",
            "run",
            "orbit.task.list",
            "--input",
            r#"{"id":"ORB-10961"}"#,
        ])
        .runtime_need,
        RuntimeNeed::Required
    );
}

#[test]
fn tool_run_and_task_artifact_get_bootstrap_the_task_owner_from_id_only_input() {
    assert_eq!(
        operation_for(&[
            "orbit",
            "tool",
            "run",
            "orbit.task.artifact.get",
            "--input",
            r#"{"id":"ORB-12263","path":"qa/note.md"}"#,
        ])
        .runtime_need,
        RuntimeNeed::TaskOwner {
            task_id: "ORB-12263".to_string()
        }
    );
    assert_eq!(
        operation_for(&[
            "orbit",
            "tool",
            "run",
            "orbit.task.artifact.put",
            "--input",
            r#"{"id":"ORB-12263","source_path":"note.md"}"#,
        ])
        .runtime_need,
        RuntimeNeed::Required,
        "artifact.put's schema does not advertise global id resolution"
    );
    assert_eq!(
        operation_for(&[
            "orbit",
            "task",
            "artifact",
            "get",
            "ORB-12263",
            "qa/note.md",
        ])
        .runtime_need,
        RuntimeNeed::TaskOwner {
            task_id: "ORB-12263".to_string()
        }
    );
    assert_eq!(
        operation_for(&["orbit", "task", "artifact", "put", "ORB-12263", "note.md",]).runtime_need,
        RuntimeNeed::Required,
    );
}
