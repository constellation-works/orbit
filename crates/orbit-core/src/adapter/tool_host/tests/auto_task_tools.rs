//! The auto-task tool adapters, exercised through the registered tools
//! [ORB-10798].
//!
//! `crud.rs` owns the behavior; these are the boundary tests for the thin
//! adapters — what the tool input has to carry, what comes back, and that
//! `mint` still delegates to the unconditional, cursor-neutral runtime path.

use orbit_types::workflow::auto_task_tag;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use orbit_automation::auto_tasks::crud::AutoTaskAddParams;
use orbit_automation::auto_tasks::cursor_state_path;

use super::super::test_support::{invalid_input_message, run_tool_as_operator, test_runtime};

/// A disabled definition on a schedule whose next slot is an hour away: every
/// condition a scheduler fire would check is arranged to say "not now", so a
/// successful mint can only come from the unconditional path.
fn params(name: &str) -> AutoTaskAddParams {
    AutoTaskAddParams {
        name: name.to_string(),
        description: format!("Auto-task {name}"),
        schedule: orbit_types::workflow::AutoTaskSchedule::Interval { every_minutes: 60 },
        template: orbit_types::workflow::AutoTaskTemplate {
            title: format!("Chore for {name}"),
            description: "Recurring chore body.".to_string(),
            acceptance_criteria: vec!["Chore is observable.".to_string()],
            task_type: orbit_types::task::TaskType::Chore,
            tags: vec![],
            required_tools: Vec::new(),
            priority: orbit_types::task::TaskPriority::Medium,
            crew: None,
            status: orbit_types::task::TaskStatus::Backlog,
        },
        dedupe: orbit_types::workflow::DedupePolicy::SkipIfOpen,
    }
}

fn with_definition(name: &str) -> (tempfile::TempDir, OrbitRuntime) {
    let (temp, runtime, _repo) = test_runtime();
    runtime.auto_task_add(params(name)).expect("add definition");
    (temp, runtime)
}

#[test]
fn list_returns_every_definition_through_the_tool_surface() {
    let (_temp, runtime) = with_definition("chore");

    let listed = run_tool_as_operator(&runtime, "orbit.auto_task.list", json!({})).expect("list");

    let definitions = listed.as_array().expect("definition array");
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0]["name"], json!("chore"));
}

#[test]
fn mint_returns_the_minted_task_with_its_provenance_tag() {
    let (_temp, runtime) = with_definition("chore");
    runtime
        .auto_task_toggle("chore", false)
        .expect("disable the definition");

    let minted = run_tool_as_operator(&runtime, "orbit.auto_task.mint", json!({ "name": "chore" }))
        .expect("mint");

    assert_eq!(minted["title"], json!("[auto-task] Chore for chore"));
    assert_eq!(minted["status"], json!("backlog"));
    assert!(
        minted["tags"]
            .as_array()
            .expect("tags array")
            .contains(&Value::String(auto_task_tag("chore"))),
        "expected provenance tag, got {}",
        minted["tags"]
    );
    // The full task projection, not a bare id: the CLI subcommand and this
    // adapter answer with the same record.
    assert!(minted["id"].as_str().is_some());
    assert!(minted["history"].is_array());
    assert!(
        !cursor_state_path(&runtime.paths().state_dir).exists(),
        "a manual mint must not write the scheduler cursor"
    );
}

/// ORB-12253: a minted task's `complexity: "unassessed"` must never block a
/// later update that leaves complexity untouched — the value auto-task mint
/// writes has to round-trip through ordinary `task.update` calls, since
/// `complexity` on that surface is patched only when the caller supplies it
/// (ORB-12116 rejects `unassessed` only when a caller explicitly names it).
#[test]
fn minted_tasks_unassessed_complexity_survives_a_later_no_op_update() {
    let (_temp, runtime) = with_definition("chore");
    runtime
        .auto_task_toggle("chore", false)
        .expect("disable the definition");

    let minted = run_tool_as_operator(&runtime, "orbit.auto_task.mint", json!({ "name": "chore" }))
        .expect("mint");
    assert_eq!(minted["complexity"], json!("unassessed"));
    let task_id = minted["id"].as_str().expect("task id").to_string();

    let updated = run_tool_as_operator(
        &runtime,
        "orbit.task.update",
        json!({ "id": task_id, "comment": "no-op touch" }),
    )
    .expect("an update that omits complexity must succeed on a minted task");
    assert_eq!(updated["complexity"], json!("unassessed"));

    let shown =
        run_tool_as_operator(&runtime, "orbit.task.show", json!({ "id": task_id })).expect("show");
    assert_eq!(shown["complexity"], json!("unassessed"));
}

#[test]
fn mint_requires_a_definition_name_and_names_an_unknown_one() {
    let (_temp, runtime) = with_definition("chore");

    let missing = invalid_input_message(run_tool_as_operator(
        &runtime,
        "orbit.auto_task.mint",
        json!({}),
    ));
    assert!(missing.contains("`name`"), "{missing}");

    let unknown = invalid_input_message(run_tool_as_operator(
        &runtime,
        "orbit.auto_task.mint",
        json!({ "name": "nope" }),
    ));
    assert!(unknown.contains("nope"), "{unknown}");
}

#[test]
fn auto_task_add_rejects_unknown_required_tools_with_suggestions() {
    let (_temp, runtime, _repo) = test_runtime();

    let error = run_tool_as_operator(
        &runtime,
        "orbit.auto_task.add",
        json!({
            "name": "invalid-tools",
            "schedule": {"every_minutes": 60},
            "template": {
                "title": "Invalid requirement",
                "required_tools": ["orbit.task.shwo"]
            }
        }),
    )
    .expect_err("unknown template tool must be rejected");

    assert!(
        error
            .to_string()
            .contains("unregistered tool 'orbit.task.shwo'")
    );
    assert!(
        error
            .did_you_mean()
            .is_some_and(|names| { names.iter().any(|name| name == "orbit.task.show") })
    );
}

#[test]
fn auto_task_update_rejects_unknown_required_tools_with_suggestions() {
    let (_temp, runtime) = with_definition("update-tools");
    let existing = runtime
        .auto_task_show("update-tools")
        .expect("show definition")
        .expect("definition exists");
    let mut template = serde_json::to_value(existing.template).expect("serialize template");
    template["required_tools"] = json!(["orbit.task.shwo"]);

    let error = run_tool_as_operator(
        &runtime,
        "orbit.auto_task.update",
        json!({"name": "update-tools", "template": template}),
    )
    .expect_err("unknown template tool must be rejected on update");

    assert!(
        error
            .to_string()
            .contains("unregistered tool 'orbit.task.shwo'")
    );
    assert!(
        error
            .did_you_mean()
            .is_some_and(|names| { names.iter().any(|name| name == "orbit.task.show") })
    );
}

#[test]
fn auto_task_mint_rechecks_persisted_template_requirements() {
    let (_temp, runtime, _repo) = test_runtime();
    let path = runtime.paths().local_dir.join("auto_tasks/drift.yaml");
    std::fs::create_dir_all(path.parent().expect("auto-task parent")).expect("create directory");
    std::fs::write(
        &path,
        "schemaVersion: 1\nname: drift\nschedule:\n  every_minutes: 60\ntemplate:\n  title: Drift\n  required_tools:\n    - orbit.task.shwo\n",
    )
    .expect("write drift definition");

    let error = run_tool_as_operator(&runtime, "orbit.auto_task.mint", json!({"name": "drift"}))
        .expect_err("mint must revalidate template requirements");
    assert!(
        error
            .to_string()
            .contains("unregistered tool 'orbit.task.shwo'")
    );
    assert!(error.did_you_mean().is_some());
    assert!(runtime.list_tasks().expect("list tasks").is_empty());
}
