//! Manual-mint tests [ORB-10439, ORB-10446]: `auto_task_mint` is unconditional
//! (ignores schedule due-math, `dedupe`, and `enabled`), leaves the host-local
//! cursor untouched, and produces a task field-for-field identical to a
//! scheduler fire.

use chrono::{DateTime, Duration, TimeZone, Utc};
use orbit_types::task::{Task, TaskStatus};
use orbit_types::workflow::{DedupePolicy, auto_task_tag};
use serde_json::Value;

use crate::OrbitRuntime;
use orbit_automation::auto_tasks::cursor_state_path;
use orbit_automation::auto_tasks::scheduler::{SchedulerOptions, run_auto_task_scheduler_at};

use super::interval_params;

fn runtime() -> OrbitRuntime {
    OrbitRuntime::in_memory().expect("build in-memory runtime")
}

fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, min, 0)
        .single()
        .expect("valid ts")
}

/// One scheduler pass, projected to `(action, task_id)` per definition.
fn fire(runtime: &OrbitRuntime, now: DateTime<Utc>) -> Vec<(String, Option<String>)> {
    let outcome = run_auto_task_scheduler_at(runtime, now, SchedulerOptions::default())
        .expect("scheduler pass");
    outcome
        .reports
        .iter()
        .map(|report| (report.action.to_string(), report.task_id.clone()))
        .collect()
}

/// Raw bytes of the cursor state file, or `None` when it does not exist.
fn cursor_bytes(runtime: &OrbitRuntime) -> Option<Vec<u8>> {
    std::fs::read(cursor_state_path(&runtime.paths().state_dir)).ok()
}

/// A task's JSON projection with the identity/clock fields stripped, so two
/// tasks minted at different moments compare on their template-derived content
/// alone — and any future `Task` field the mint path starts populating is
/// covered without touching this helper.
fn content(task: &Task) -> Value {
    let mut value = serde_json::to_value(task).expect("task json");
    let object = value.as_object_mut().expect("task json object");
    for volatile in ["id", "created_at", "updated_at"] {
        object.remove(volatile);
    }
    value
}

#[test]
fn mint_matches_a_scheduler_fire_field_for_field() {
    let runtime = runtime();
    let mut params = interval_params("chore", 60);
    params.template.required_tools = vec![
        "github.run.list".to_string(),
        "github.auth.status".to_string(),
        "github.run.list".to_string(),
    ];
    runtime.auto_task_add(params).expect("add");
    let t0 = at(2026, 1, 1, 0, 0);

    fire(&runtime, t0); // baseline
    let reports = fire(&runtime, t0 + Duration::minutes(65));
    assert_eq!(reports[0].0, "fired");
    let fired = runtime
        .get_task(&reports[0].1.clone().expect("task id"))
        .expect("fired task");

    let minted = runtime.auto_task_mint("chore").expect("mint");

    assert_ne!(minted.id, fired.id, "mint creates a distinct task");
    assert_eq!(
        content(&minted),
        content(&fired),
        "a manually minted task must be indistinguishable from a fired one"
    );
    assert!(
        minted.tags.contains(&auto_task_tag("chore")),
        "expected provenance tag, got {:?}",
        minted.tags
    );
    assert_eq!(
        minted.created_by.as_deref(),
        Some("system"),
        "mint uses the system_created identity"
    );
    assert_eq!(
        minted.status,
        TaskStatus::Backlog,
        "mint honors the template-supplied status default"
    );
    assert_eq!(
        minted.complexity,
        Some(orbit_types::task::TaskComplexity::Unassessed),
        "automated mint writes the explicit non-answer, never a fabricated assessment"
    );
    assert_eq!(
        minted.required_tools,
        vec!["github.auth.status", "github.run.list"]
    );
}

#[test]
fn title_prefix_is_applied_once_for_scheduler_and_manual_mint() {
    for (template_title, expected_title) in [
        (
            "Chore from a clean template",
            "[auto-task] Chore from a clean template",
        ),
        (
            "[auto-task] Chore from an already-prefixed template",
            "[auto-task] Chore from an already-prefixed template",
        ),
    ] {
        let runtime = runtime();
        let mut params = interval_params("chore", 60);
        params.template.title = template_title.to_string();
        runtime.auto_task_add(params).expect("add");
        let t0 = at(2026, 1, 1, 0, 0);

        fire(&runtime, t0); // baseline
        let reports = fire(&runtime, t0 + Duration::minutes(65));
        let scheduler_task = runtime
            .get_task(&reports[0].1.clone().expect("scheduler task id"))
            .expect("scheduler task");
        let minted_task = runtime.auto_task_mint("chore").expect("manual mint");

        assert_eq!(scheduler_task.title, expected_title);
        assert_eq!(minted_task.title, expected_title);
    }
}

#[test]
fn mint_succeeds_for_a_disabled_definition() {
    let runtime = runtime();
    runtime
        .auto_task_add(interval_params("chore", 60))
        .expect("add");
    runtime
        .auto_task_toggle("chore", false)
        .expect("toggle off");

    let minted = runtime.auto_task_mint("chore").expect("mint");
    assert!(minted.tags.contains(&auto_task_tag("chore")));
    assert_eq!(runtime.list_tasks().expect("tasks").len(), 1);
}

#[test]
fn mint_succeeds_while_an_instance_is_open_under_skip_if_open() {
    let runtime = runtime();
    let params = interval_params("chore", 60);
    assert_eq!(params.dedupe, DedupePolicy::SkipIfOpen);
    runtime.auto_task_add(params).expect("add");

    let first = runtime.auto_task_mint("chore").expect("mint");
    assert!(first.tags.contains(&auto_task_tag("chore")));

    // The first instance is open (backlog), yet a second manual mint still lands.
    let second = runtime.auto_task_mint("chore").expect("mint again");
    assert_ne!(first.id, second.id);
    assert_eq!(runtime.list_tasks().expect("tasks").len(), 2);
}

#[test]
fn mint_leaves_the_cursor_byte_identical() {
    let runtime = runtime();
    runtime
        .auto_task_add(interval_params("chore", 60))
        .expect("add");
    let t0 = at(2026, 1, 1, 0, 0);

    // Before any scheduler pass the file does not exist — mint must not
    // create it.
    assert!(cursor_bytes(&runtime).is_none());
    runtime.auto_task_mint("chore").expect("mint");
    assert!(
        cursor_bytes(&runtime).is_none(),
        "mint must not create cursor state"
    );

    fire(&runtime, t0); // baseline writes the cursor
    let before = cursor_bytes(&runtime).expect("cursor state after baseline");
    runtime.auto_task_mint("chore").expect("mint");
    assert_eq!(
        cursor_bytes(&runtime).expect("cursor state after mint"),
        before,
        "mint must not read-modify-write the cursor"
    );
}

#[test]
fn mint_does_not_change_the_next_scheduler_decision() {
    // `always` dedupe isolates the cursor question from the provenance-tag
    // question: the only thing that could move the next pass is the cursor.
    let mut params = interval_params("chore", 60);
    params.dedupe = DedupePolicy::Always;
    let t0 = at(2026, 1, 1, 0, 0);

    let control = runtime();
    control.auto_task_add(params.clone()).expect("add");
    fire(&control, t0); // baseline
    let control_outcome = run_auto_task_scheduler_at(
        &control,
        t0 + Duration::minutes(65),
        SchedulerOptions::default(),
    )
    .expect("control pass");

    let minted_runtime = runtime();
    minted_runtime.auto_task_add(params).expect("add");
    fire(&minted_runtime, t0); // baseline
    minted_runtime.auto_task_mint("chore").expect("mint");
    let minted_outcome = run_auto_task_scheduler_at(
        &minted_runtime,
        t0 + Duration::minutes(65),
        SchedulerOptions::default(),
    )
    .expect("pass after mint");

    assert_eq!(control_outcome.reports[0].action, "fired");
    assert_eq!(
        minted_outcome.reports[0].action,
        control_outcome.reports[0].action,
    );
    assert_eq!(
        minted_outcome.reports[0].slot, control_outcome.reports[0].slot,
        "the consumed slot must be unaffected by a manual mint"
    );
}

#[test]
fn an_open_manually_minted_instance_defers_the_next_fire_like_a_fired_one() {
    // The flip side of provenance parity: a manually minted task carries the
    // `auto-task:<name>` tag, so `skip_if_open` sees it exactly as it sees a
    // fired instance. That is the point — a hand-copied task is invisible to
    // dedupe, and this surface exists so a manual mint is not.
    let runtime = runtime();
    runtime
        .auto_task_add(interval_params("chore", 60))
        .expect("add");
    let t0 = at(2026, 1, 1, 0, 0);

    fire(&runtime, t0); // baseline
    let minted = runtime.auto_task_mint("chore").expect("mint");

    let deferred = fire(&runtime, t0 + Duration::minutes(65));
    assert_eq!(deferred[0].0, "skipped");
    assert_eq!(runtime.list_tasks().expect("tasks").len(), 1);

    // The cursor never advanced, so the pending occurrence fires (once) the
    // moment the queue drains — the same recovery as a stalled fired instance.
    runtime
        .update_task(
            &minted.id,
            crate::application::task::TaskUpdateParams {
                status: Some(TaskStatus::Rejected),
                ..Default::default()
            },
        )
        .expect("close manually minted task");
    let drained = fire(&runtime, t0 + Duration::minutes(125));
    assert_eq!(drained[0].0, "fired");
    assert_eq!(runtime.list_tasks().expect("tasks").len(), 2);
}

#[test]
fn mint_with_an_unknown_name_errors_naming_the_definition() {
    let runtime = runtime();
    let error = runtime
        .auto_task_mint("nope")
        .expect_err("unknown definition must fail");
    assert!(
        error.to_string().contains("nope"),
        "error must name the definition, got: {error}"
    );
    assert!(runtime.list_tasks().expect("tasks").is_empty());
}
