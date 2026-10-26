//! Grants must be usable by an agent before either task write persists them.

use orbit_core::OrbitRuntime;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_types::tool::StoredTool;

fn granted_tool_writes_reject_unusable_tools_atomically() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Grant update target".into(),
            ..Default::default()
        })
        .unwrap();

    let mut failures = Vec::new();
    for tool in [
        "orbit.command.exec",
        "orbit.pipeline.invoke",
        "orbit.workflow.run.resume",
        "orbit.workflow.ship",
        "orbit.task.locks.release",
    ] {
        let before = runtime.get_task(&task.id).unwrap();
        let history = runtime.get_task_history(&task.id).unwrap();
        let add = runtime.add_task(TaskAddParams {
            title: "Unusable grant".into(),
            granted_tools: vec![tool.into()],
            ..Default::default()
        });
        let update = runtime.update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                granted_tools: Some(vec![tool.into()]),
                ..Default::default()
            },
            None,
            None,
        );
        for (write, error) in [("add", add.err()), ("update", update.err())] {
            match error {
                Some(error) => {
                    let message = error.to_string();
                    assert!(message.contains("governed"), "{write}: {message}");
                    assert!(message.contains(tool), "{write}: {message}");
                    assert!(message.contains("agent"), "{write}: {message}");
                }
                None => failures.push(format!("{write} accepted operator-only {tool}")),
            }
        }
        if failures.is_empty() {
            assert_eq!(runtime.get_task(&task.id).unwrap(), before);
            assert_eq!(runtime.get_task_history(&task.id).unwrap(), history);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("; "));

    let usable = "orbit.auto_task.update";
    let granted = runtime
        .add_task(TaskAddParams {
            title: "Usable grant".into(),
            granted_tools: vec![usable.into()],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        runtime.get_task(&granted.id).unwrap().granted_tools,
        [usable]
    );
    runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                granted_tools: Some(vec![usable.into()]),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    assert_eq!(runtime.get_task(&task.id).unwrap().granted_tools, [usable]);

    runtime
        .stores()
        .tools()
        .insert_tool(&StoredTool {
            name: usable.into(),
            path: String::new(),
            description: String::new(),
            enabled: false,
            builtin: true,
            parameters: Vec::new(),
        })
        .unwrap();
    // Requirements retain their established warning-only handling of a
    // temporarily disabled tool; granting one must be a filing error.
    assert_eq!(
        runtime
            .validate_required_tools(&[usable.into()])
            .unwrap()
            .len(),
        1
    );
    for (tool, reason) in [
        (usable, "inactive"),
        ("orbit.auto_task.*", "wildcard"),
        ("orbit_auto_task_update", "canonical"),
        ("Orbit.auto_task.update", "canonical"),
        (" orbit.auto_task.update", "canonical"),
        ("orbit.missing.tool", "unknown"),
        ("orbit.friction.show", "agent-facing"),
    ] {
        let before = runtime.get_task(&task.id).unwrap();
        let history = runtime.get_task_history(&task.id).unwrap();
        let add_error = runtime
            .add_task(TaskAddParams {
                title: "Invalid grant".into(),
                granted_tools: vec![tool.into()],
                ..Default::default()
            })
            .expect_err("creation must refuse an unavailable grant");
        let update_error = runtime
            .update_task_with_identity(
                &task.id,
                TaskUpdateParams {
                    granted_tools: Some(vec![tool.into()]),
                    ..Default::default()
                },
                None,
                None,
            )
            .expect_err("replacement, including an unchanged disabled grant, must be refused");
        for error in [add_error, update_error] {
            let message = error.to_string();
            assert!(message.contains(reason), "{tool}: {message}");
            assert!(message.contains(tool), "{tool}: {message}");
        }
        assert_eq!(runtime.get_task(&task.id).unwrap(), before);
        assert_eq!(runtime.get_task_history(&task.id).unwrap(), history);
    }
    runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                granted_tools: Some(Vec::new()),
                ..Default::default()
            },
            None,
            None,
        )
        .expect("an unusable grant can still be revoked before dispatch");
}

crate::scenario_table!(
    granted_tool_write_scenarios,
    granted_tool_writes_reject_unusable_tools_atomically
);
