//! A `backlog` task's `not_before` holds it from every dispatcher until that
//! time, and the hold lifts on the first admission pass after it.
//!
//! The auto drain's listing and readiness report the task waiting
//! `scheduled` with its time; an explicit ship, through the runtime and the
//! `orbit.workflow.ship` tool, is refused with `task_scheduled`; only the
//! operator's `--ignore-schedule` starts it, recording `schedule_overridden`.
//! Fixed far-future and past times stand in for waiting, so no test sleeps.

use orbit_core::application::task::TaskAddParams;
use std::collections::BTreeSet;

use orbit_core::application::job::JobRunListParams;
use orbit_core::{
    CompletionPolicy, OrbitError, OrbitRuntime, ShipMode, Task, TaskComplexity, TaskStatus,
};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::policy::Role;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use orbit_types::workflow::JobRunTrigger;
use serde_json::{Value, json};

const CONFIG: &str = "[crews.coders]\nmodel = \"code-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[workflow]\ndefault_crew = \"coders\"\n";
const FUTURE: &str = "2099-01-01T09:00:00Z";
const PAST: &str = "2020-01-01T09:00:00Z";

fn at(time: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(time)
        .unwrap()
        .with_timezone(&chrono::Utc)
}

fn task(runtime: &OrbitRuntime, title: &str, not_before: Option<&str>) -> Task {
    let file = format!(
        "fixture-{}.txt",
        runtime.list_task_metadata().unwrap().len()
    );
    std::fs::write(runtime.paths().repo_root.join(&file), "fixture\n").unwrap();
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture task: {title}"),
            acceptance_criteria: vec!["Fixture task is observable.".to_string()],
            plan: "Fixture plan.".to_string(),
            complexity: TaskComplexity::Medium,
            context_files: vec![format!("file:{file}")],
            status: Some(TaskStatus::Backlog),
            not_before: not_before.map(at),
            ..Default::default()
        })
        .expect("seed task")
}

fn backlog(runtime: &OrbitRuntime) -> Value {
    runtime
        .run_deterministic(
            "list_backlog_tasks",
            &json!({}),
            &json!({}),
            ToolContext::default(),
        )
        .expect("list backlog tasks")
}

fn admitted(output: &Value) -> Vec<String> {
    output["task_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap().to_string())
        .collect()
}

fn excluded<'a>(output: &'a Value, task: &str) -> Option<&'a Value> {
    output["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == task)
}

fn readiness_entry(runtime: &OrbitRuntime, task: &str) -> Value {
    let readiness = runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    readiness["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == task)
        .unwrap_or_else(|| panic!("readiness lists {task}: {readiness}"))
        .clone()
}

fn ship(
    runtime: &OrbitRuntime,
    task: &str,
    ignore_schedule: bool,
) -> Result<orbit_core::PipelineInvokeResult, OrbitError> {
    runtime.submit_ship_run_with_containment(
        ShipMode::Local,
        Some("main"),
        &[task.to_string()],
        CompletionPolicy::Review,
        &[],
        Some("operator:fixture"),
        None,
        JobRunTrigger::cli(),
        false,
        None,
        ignore_schedule,
    )
}

/// `orbit.workflow.ship` as an operator session calls it.
fn ship_tool(runtime: &OrbitRuntime, input: Value) -> Result<Value, OrbitError> {
    runtime.run_tool_with_context_and_role(
        "orbit.workflow.ship",
        input,
        Role::Admin,
        ToolContext {
            session_context: ToolSessionContext {
                effective_capabilities: BTreeSet::from([McpCapability::Operator]),
                ..ToolSessionContext::default()
            },
            ..ToolContext::default()
        },
    )
}

fn runs(runtime: &OrbitRuntime) -> usize {
    runtime
        .list_job_runs(JobRunListParams::default())
        .unwrap()
        .len()
}

fn overrides(runtime: &OrbitRuntime, task: &str) -> Vec<String> {
    runtime
        .get_task_history(task)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.event == "schedule_overridden")
        .map(|entry| format!("{}: {}", entry.by, entry.note.unwrap_or_default()))
        .collect()
}

/// A backlog task scheduled ahead is withheld from the drain's listing and
/// reported by readiness as waiting `scheduled` with its time, never
/// runnable. An explicit ship of it, through the runtime and through the
/// `orbit.workflow.ship` tool, is refused with `task_scheduled` naming the
/// time and persists no run. The operator override submits it and records
/// `schedule_overridden` for that time, after which the drain no longer
/// withholds it. A task scheduled in the past is admitted beside it.
fn a_future_schedule_holds_the_task_until_the_operator_overrides_it() {
    let (_root, runtime) = super::review_run_option::open(ShipMode::Local, CONFIG, true);
    let scheduled = task(&runtime, "scheduled", Some(FUTURE));
    let due = task(&runtime, "due", Some(PAST));
    let wait = format!(
        "scheduled: not before {FUTURE}; the hold lifts on the first admission pass after that time"
    );

    let listed = backlog(&runtime);
    assert_eq!(admitted(&listed), vec![due.id.clone()], "{listed}");
    let skipped = excluded(&listed, &scheduled.id).expect("the scheduled task is excluded");
    assert_eq!(skipped["reason"], "scheduled", "{listed}");
    assert_eq!(skipped["detail"], wait.as_str(), "{listed}");

    let entry = readiness_entry(&runtime, &scheduled.id);
    assert_eq!(entry["eligible"], false, "{entry}");
    assert_eq!(entry["reason"], "scheduled", "{entry}");
    assert_eq!(entry["detail"], wait.as_str(), "{entry}");
    assert_eq!(entry["not_before"], FUTURE, "{entry}");
    assert_eq!(readiness_entry(&runtime, &due.id)["eligible"], true);

    let refused = ship(&runtime, &scheduled.id, false).expect_err("a scheduled task is refused");
    assert!(
        matches!(&refused, OrbitError::TaskScheduled { task_id, not_before }
            if *task_id == scheduled.id && not_before == FUTURE),
        "{refused:?}"
    );
    assert!(
        refused.to_string().contains("--ignore-schedule"),
        "{refused}"
    );
    let tool = ship_tool(
        &runtime,
        json!({"task_ids": [scheduled.id], "mode": "local"}),
    )
    .expect_err("the tool refuses a scheduled task");
    assert!(
        matches!(&tool, OrbitError::TaskScheduled { not_before, .. } if not_before == FUTURE),
        "{tool:?}"
    );
    assert_eq!(runs(&runtime), 0, "a refused ship persists no run");
    assert!(overrides(&runtime, &scheduled.id).is_empty());

    // The fixture's worker cannot start, so an admitted submission persists
    // its run and then reports the failed spawn.
    match ship(&runtime, &scheduled.id, true) {
        Ok(_) | Err(OrbitError::Execution(_)) => {}
        Err(other) => panic!("the operator override admits the task: {other:?}"),
    }
    assert_eq!(runs(&runtime), 1);
    assert_eq!(
        overrides(&runtime, &scheduled.id),
        vec![format!(
            "operator:fixture: scheduled for {FUTURE}; operator `operator:fixture` started it \
             early with `orbit run ship --ignore-schedule`"
        )]
    );
    let after = backlog(&runtime);
    assert!(
        excluded(&after, &scheduled.id).is_none_or(|entry| entry["reason"] != "scheduled"),
        "the override lifts the hold: {after}"
    );
}

/// Fractional instants remain future between their whole-second boundary and
/// requested time. The public task tool stores and shows that instant, and an
/// override for it matches the same precise time.
fn a_fractional_schedule_retains_its_instant_through_show_and_override() {
    const REQUESTED: &str = "2099-01-01T09:00:00.900Z";
    const SHOWN: &str = "2099-01-01T09:00:00.900Z";

    let (_root, runtime) = super::review_run_option::open(ShipMode::Local, CONFIG, true);
    std::fs::write(
        runtime.paths().repo_root.join("fractional-tool.txt"),
        "fixture\n",
    )
    .unwrap();
    let workspace = runtime.paths().repo_root.to_string_lossy().into_owned();
    let added = runtime
        .run_tool(
            "orbit.task.add",
            json!({
                "title": "Fractionally scheduled by tool", "description": "Fixture.",
                "acceptance_criteria": ["Observable."], "complexity": "medium",
                "context_files": ["file:fractional-tool.txt"],
                "not_before": "2099-01-01T11:00:00.900+02:00",
                "model": "codex", "workspace": workspace,
            }),
        )
        .expect("add a fractionally scheduled task");
    let id = added["id"].as_str().expect("added id").to_string();
    runtime
        .run_tool(
            "orbit.task.update",
            json!({"id": id, "status": "backlog", "model": "codex", "workspace": workspace}),
        )
        .expect("promote the task");

    let scheduled = runtime.get_task(&id).expect("read the scheduled task");
    let requested = at(REQUESTED);
    assert_eq!(scheduled.not_before, Some(requested));
    assert_eq!(
        scheduled.scheduled_after(at("2099-01-01T09:00:00.500Z")),
        Some(requested),
        "the public schedule API keeps the hold after the whole-second boundary"
    );
    assert_eq!(
        scheduled.scheduled_after(at("2099-01-01T09:00:00.900Z")),
        None,
        "the schedule is due at its requested instant"
    );

    let shown = runtime
        .run_tool("orbit.task.show", json!({"id": id}))
        .expect("show the scheduled task");
    assert_eq!(shown["not_before"], SHOWN, "{shown}");

    // The fixture worker cannot start; the override still records its event.
    match ship(&runtime, &id, true) {
        Ok(_) | Err(OrbitError::Execution(_)) => {}
        Err(other) => panic!("the operator override admits the task: {other:?}"),
    }
    assert_eq!(
        overrides(&runtime, &id),
        vec![format!(
            "operator:fixture: scheduled for {SHOWN}; operator `operator:fixture` started it early with `orbit run ship --ignore-schedule`"
        )]
    );
    let after = backlog(&runtime);
    assert!(
        excluded(&after, &id).is_none_or(|entry| entry["reason"] != "scheduled"),
        "the precise override lifts the hold: {after}"
    );
}

/// The override names the tasks it starts: without task ids it is refused
/// before any run exists.
fn the_override_needs_named_tasks() {
    let (_root, runtime) = super::review_run_option::open(ShipMode::Local, CONFIG, true);
    let refused = runtime
        .submit_ship_run_with_containment(
            ShipMode::Local,
            Some("main"),
            &[],
            CompletionPolicy::Review,
            &[],
            Some("operator:fixture"),
            None,
            JobRunTrigger::cli(),
            false,
            None,
            true,
        )
        .expect_err("an auto ship has no task to override");
    assert!(
        matches!(&refused, OrbitError::InvalidInput(message) if message.contains("--ignore-schedule")),
        "{refused:?}"
    );
    assert_eq!(runs(&runtime), 0);
}

/// `orbit.task.add` and `orbit.task.update` set `not_before` (stored in
/// UTC), an empty string clears it while `null` leaves it, and a malformed time is refused naming the input
/// and writing nothing. A task that never had one shows no `not_before`, and
/// moving it into the past releases it on the next listing.
fn add_and_update_set_clear_and_refuse_the_schedule() {
    let (_root, runtime) = super::review_run_option::open(ShipMode::Local, CONFIG, true);
    std::fs::write(runtime.paths().repo_root.join("tool.txt"), "fixture\n").unwrap();
    let workspace = runtime.paths().repo_root.to_string_lossy().into_owned();
    let added = runtime
        .run_tool(
            "orbit.task.add",
            json!({
                "title": "Scheduled by tool", "description": "Fixture.",
                "acceptance_criteria": ["Observable."], "complexity": "medium",
                "context_files": ["file:tool.txt"],
                "not_before": "2099-01-01T11:00:00+02:00", "model": "codex", "workspace": workspace,
            }),
        )
        .expect("add a scheduled task");
    let id = added["id"].as_str().expect("added id").to_string();
    runtime
        .run_tool(
            "orbit.task.update",
            json!({"id": id, "status": "backlog", "model": "codex", "workspace": workspace}),
        )
        .expect("promote the task");
    let shown = runtime
        .run_tool("orbit.task.show", json!({"id": id}))
        .unwrap();
    assert_eq!(shown["not_before"], FUTURE, "{shown}");
    assert_eq!(runtime.get_task(&id).unwrap().not_before, Some(at(FUTURE)));
    assert!(excluded(&backlog(&runtime), &id).is_some());

    let malformed = runtime
        .run_tool(
            "orbit.task.update",
            json!({"id": id, "not_before": "next tuesday", "model": "codex", "workspace": workspace}),
        )
        .expect_err("a malformed time is refused");
    assert!(
        matches!(&malformed, OrbitError::InvalidInput(message) if message.contains("`next tuesday`")),
        "{malformed:?}"
    );
    assert_eq!(runtime.get_task(&id).unwrap().not_before, Some(at(FUTURE)));

    runtime
        .run_tool(
            "orbit.task.update",
            json!({"id": id, "not_before": PAST, "model": "codex", "workspace": workspace}),
        )
        .expect("move the schedule into the past");
    assert_eq!(admitted(&backlog(&runtime)), vec![id.clone()]);

    // MCP clients send null for fields they leave unset, so null keeps it.
    runtime
        .run_tool(
            "orbit.task.update",
            json!({"id": id, "title": "Renamed", "not_before": null, "model": "codex",
                "workspace": workspace}),
        )
        .expect("null leaves the schedule");
    assert_eq!(runtime.get_task(&id).unwrap().not_before, Some(at(PAST)));
    runtime
        .run_tool(
            "orbit.task.update",
            json!({"id": id, "not_before": "", "model": "codex", "workspace": workspace}),
        )
        .expect("an empty string clears the schedule");
    assert_eq!(runtime.get_task(&id).unwrap().not_before, None);
    let cleared = runtime
        .run_tool("orbit.task.show", json!({"id": id}))
        .unwrap();
    assert!(cleared.get("not_before").is_none(), "{cleared}");

    let refused = runtime
        .run_tool(
            "orbit.task.add",
            json!({
                "title": "Badly scheduled", "description": "Fixture.",
                "acceptance_criteria": ["Observable."], "complexity": "medium",
                "not_before": "2099-13-01", "model": "codex", "workspace": workspace,
            }),
        )
        .expect_err("a malformed time is refused on add");
    assert!(
        matches!(&refused, OrbitError::InvalidInput(message) if message.contains("`2099-13-01`")),
        "{refused:?}"
    );
    assert_eq!(
        runtime.list_tasks().unwrap().len(),
        1,
        "nothing was created"
    );
}

crate::scenario_table!(
    a_future_schedule_holds_the_task_until_the_operator_overrides_it_scenarios,
    a_future_schedule_holds_the_task_until_the_operator_overrides_it,
    a_fractional_schedule_retains_its_instant_through_show_and_override,
    the_override_needs_named_tasks,
    add_and_update_set_clear_and_refuse_the_schedule
);
