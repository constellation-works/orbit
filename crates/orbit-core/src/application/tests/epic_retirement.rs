use orbit_store::contracts::ActiveTaskReservation;
use orbit_types::task::Task;
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::{Value, json};

use crate::application::epic_retirement::{EpicRetirementSnapshot, assess_epic_retirement};

fn task(id: &str, status: &str, extra: Value) -> Task {
    let mut value = json!({
        "id": id,
        "title": format!("Fixture {id}"),
        "description": "Fixture task",
        "context_files": [],
        "status": status,
        "priority": "medium",
        "task_type": "chore",
        "created_at": "2026-09-01T00:00:00Z",
        "updated_at": "2026-09-01T00:00:00Z",
    });
    let object = value.as_object_mut().expect("task fixture object");
    for (key, extra_value) in extra.as_object().expect("extra fixture object") {
        object.insert(key.clone(), extra_value.clone());
    }
    serde_json::from_value(value).expect("deserialize task fixture")
}

fn epic_root(id: &str, status: &str, context_files: Value) -> Task {
    task(
        id,
        status,
        json!({ "tags": ["epic"], "context_files": context_files }),
    )
}

fn child(id: &str, status: &str, parent_id: &str, context_files: Value) -> Task {
    task(
        id,
        status,
        json!({
            "relations": [{ "type": "child_of", "target": parent_id }],
            "context_files": context_files,
        }),
    )
}

fn run(run_id: &str, job_id: &str, state: JobRunState, input: Value) -> JobRun {
    serde_json::from_value(json!({
        "run_id": run_id,
        "job_id": job_id,
        "attempt": 1,
        "state": state,
        "scheduled_at": "2026-09-01T00:00:00Z",
        "created_at": "2026-09-01T00:00:00Z",
        "input": input,
    }))
    .expect("deserialize job run fixture")
}

fn reservation(reservation_id: &str, task_ids: &[&str]) -> ActiveTaskReservation {
    ActiveTaskReservation {
        reservation_id: reservation_id.to_string(),
        workspace_id: Some("ws".to_string()),
        task_ids: task_ids.iter().map(|id| (*id).to_string()).collect(),
        files: vec!["file:crates/alpha/src/lib.rs".to_string()],
        actor: "fixture".to_string(),
        created_at: "2026-09-01T00:00:00Z".to_string(),
        expires_at: "2026-09-01T04:00:00Z".to_string(),
        owner_run_id: None,
        owner_metadata_json: None,
    }
}

fn assess(tasks: &[Task], runs: &[JobRun], reservations: &[ActiveTaskReservation]) -> Value {
    serde_json::to_value(assess_epic_retirement(EpicRetirementSnapshot {
        tasks,
        runs,
        reservations,
    }))
    .expect("serialize report")
}

fn blocker_kinds(report: &Value) -> Vec<String> {
    report["blockers"]
        .as_array()
        .expect("blockers array")
        .iter()
        .map(|blocker| blocker["kind"].as_str().expect("kind").to_string())
        .collect()
}

#[test]
fn reconciled_workspace_is_ready_and_keeps_tags_and_hierarchy() {
    let tasks = vec![
        epic_root("T-root", "done", json!(["file:crates/alpha/src/lib.rs"])),
        child(
            "T-child",
            "done",
            "T-root",
            json!(["file:crates/beta/src/lib.rs"]),
        ),
    ];
    let runs = vec![run(
        "jrun-epic-1",
        "epic_pipeline",
        JobRunState::Success,
        json!({ "epic_task_id": "T-root" }),
    )];

    let report = assess(&tasks, &runs, &[]);

    assert_eq!(report["ready"], json!(true));
    assert_eq!(report["blockers"], json!([]));
    assert_eq!(report["inherited_only_roots"], json!([]));
    // The tag survives and the historical run stays discoverable for GC.
    assert_eq!(report["epic_tagged_tasks"], json!(["T-root"]));
    assert_eq!(report["historical_worktree_runs"], json!(["jrun-epic-1"]));
}

#[test]
fn live_epic_run_refuses_even_when_its_root_is_already_terminal() {
    let tasks = vec![epic_root(
        "T-root",
        "done",
        json!(["file:crates/alpha/src/lib.rs"]),
    )];
    let runs = vec![run(
        "jrun-epic-live",
        "epic_pipeline",
        JobRunState::Running,
        json!({ "epic_task_id": "T-root" }),
    )];

    let report = assess(&tasks, &runs, &[]);

    assert_eq!(report["ready"], json!(false));
    assert_eq!(blocker_kinds(&report), vec!["active_epic_run"]);
    assert_eq!(report["blockers"][0]["run_id"], json!("jrun-epic-live"));
    // Still discoverable? No: a live run is not historical worktree evidence.
    assert_eq!(report["historical_worktree_runs"], json!([]));
}

#[test]
fn review_root_with_a_live_completion_run_refuses() {
    // The status-only check this replaces reads `review` as settled; the run
    // is what proves `complete_pr` may still be executing.
    let tasks = vec![epic_root(
        "T-root",
        "review",
        json!(["file:crates/alpha/src/lib.rs"]),
    )];
    let runs = vec![run(
        "jrun-epic-completing",
        "epic_pipeline",
        JobRunState::Running,
        json!({ "epic_task_id": "T-root", "completion": "done" }),
    )];

    let report = assess(&tasks, &runs, &[]);

    assert_eq!(report["ready"], json!(false));
    assert_eq!(blocker_kinds(&report), vec!["active_epic_run"]);
}

#[test]
fn live_child_execution_refuses_under_a_terminal_root() {
    let tasks = vec![
        epic_root("T-root", "done", json!(["file:crates/alpha/src/lib.rs"])),
        child(
            "T-child",
            "in-progress",
            "T-root",
            json!(["file:crates/beta/src/lib.rs"]),
        ),
    ];
    let runs = vec![run(
        "jrun-child",
        "task_local_pipeline",
        JobRunState::Pending,
        json!({ "task_ids": ["T-child"] }),
    )];

    let report = assess(&tasks, &runs, &[]);

    assert_eq!(report["ready"], json!(false));
    assert_eq!(blocker_kinds(&report), vec!["active_family_run"]);
    assert_eq!(report["blockers"][0]["task_ids"], json!(["T-child"]));
}

#[test]
fn unrelated_live_work_does_not_refuse() {
    let tasks = vec![
        epic_root("T-root", "done", json!(["file:crates/alpha/src/lib.rs"])),
        task("T-loose", "in-progress", json!({})),
    ];
    let runs = vec![run(
        "jrun-loose",
        "task_pr_pipeline",
        JobRunState::Running,
        json!({ "task_ids": ["T-loose"] }),
    )];

    let report = assess(&tasks, &runs, &[]);

    assert_eq!(report["ready"], json!(true));
}

#[test]
fn uncertain_landing_refuses_on_a_non_success_terminal_run() {
    let tasks = vec![epic_root(
        "T-root",
        "review",
        json!(["file:crates/alpha/src/lib.rs"]),
    )];
    let runs = vec![run(
        "jrun-epic-interrupted",
        "epic_pipeline",
        JobRunState::Interrupted,
        json!({ "epic_task_id": "T-root" }),
    )];

    let report = assess(&tasks, &runs, &[]);

    assert_eq!(report["ready"], json!(false));
    assert_eq!(blocker_kinds(&report), vec!["uncertain_landing"]);
    // Interrupted is terminal, so its worktree is still GC's to find.
    assert_eq!(
        report["historical_worktree_runs"],
        json!(["jrun-epic-interrupted"])
    );
}

#[test]
fn uncertain_landing_refuses_when_an_active_task_names_an_unknown_run() {
    let tasks = vec![
        epic_root("T-root", "done", json!(["file:crates/alpha/src/lib.rs"])),
        task(
            "T-child",
            "review",
            json!({
                "relations": [{ "type": "child_of", "target": "T-root" }],
                "context_files": ["file:crates/beta/src/lib.rs"],
                "job_run_id": "jrun-vanished",
            }),
        ),
    ];

    let report = assess(&tasks, &[], &[]);

    assert_eq!(report["ready"], json!(false));
    assert_eq!(blocker_kinds(&report), vec!["uncertain_landing"]);
    assert_eq!(report["blockers"][0]["run_id"], json!("jrun-vanished"));
}

#[test]
fn unreleased_reservation_refuses() {
    let tasks = vec![
        epic_root("T-root", "done", json!(["file:crates/alpha/src/lib.rs"])),
        child(
            "T-child",
            "done",
            "T-root",
            json!(["file:crates/beta/src/lib.rs"]),
        ),
    ];

    let report = assess(&tasks, &[], &[reservation("res-1", &["T-child"])]);

    assert_eq!(report["ready"], json!(false));
    assert_eq!(blocker_kinds(&report), vec!["active_reservation"]);
    assert_eq!(report["blockers"][0]["reservation_id"], json!("res-1"));
}

#[test]
fn inherited_only_roots_are_reported_without_rewriting_them() {
    let tasks = vec![
        epic_root("T-root", "backlog", json!([])),
        child(
            "T-child",
            "backlog",
            "T-root",
            json!(["file:crates/beta/src/lib.rs"]),
        ),
        // A grandchild is inherited-only evidence too: the union the root
        // relied on was the whole family, not just its direct children.
        child(
            "T-grandchild",
            "backlog",
            "T-child",
            json!(["file:crates/gamma/src/lib.rs"]),
        ),
    ];

    let report = assess(&tasks, &[], &[]);

    // Reporting is not a refusal: nothing is live here.
    assert_eq!(report["ready"], json!(true));
    assert_eq!(
        report["inherited_only_roots"],
        json!([{
            "task_id": "T-root",
            "status": "backlog",
            "descendants_with_context": ["T-child", "T-grandchild"],
        }])
    );
    assert_eq!(report["epic_tagged_tasks"], json!(["T-root"]));
}

#[test]
fn a_root_declaring_its_own_context_is_not_reported() {
    let tasks = vec![
        epic_root("T-root", "backlog", json!(["file:crates/alpha/src/lib.rs"])),
        child(
            "T-child",
            "backlog",
            "T-root",
            json!(["file:crates/beta/src/lib.rs"]),
        ),
    ];

    let report = assess(&tasks, &[], &[]);

    assert_eq!(report["inherited_only_roots"], json!([]));
}

#[test]
fn a_childless_empty_root_is_not_reported_as_inherited_only() {
    let tasks = vec![epic_root("T-root", "backlog", json!([]))];

    let report = assess(&tasks, &[], &[]);

    assert_eq!(report["inherited_only_roots"], json!([]));
    assert_eq!(report["ready"], json!(true));
}

#[test]
fn a_parent_cycle_does_not_hang_the_walk() {
    let mut left = child("T-left", "backlog", "T-right", json!([]));
    let right = child("T-right", "backlog", "T-left", json!([]));
    left.tags = vec!["epic".to_string()];
    let tasks = vec![left, right];

    let report = assess(&tasks, &[], &[]);

    assert_eq!(report["ready"], json!(true));
    assert_eq!(report["epic_tagged_tasks"], json!(["T-left"]));
}

#[test]
fn every_unreconciled_record_is_reported_together() {
    let tasks = vec![
        epic_root("T-root", "review", json!(["file:crates/alpha/src/lib.rs"])),
        child(
            "T-child",
            "in-progress",
            "T-root",
            json!(["file:crates/beta/src/lib.rs"]),
        ),
    ];
    let runs = vec![
        run(
            "jrun-epic-live",
            "epic_pipeline",
            JobRunState::Running,
            json!({ "epic_task_id": "T-root" }),
        ),
        run(
            "jrun-child-failed",
            "task_local_pipeline",
            JobRunState::Failed,
            json!({ "task_ids": ["T-child"] }),
        ),
    ];

    let report = assess(&tasks, &runs, &[reservation("res-1", &["T-root"])]);

    assert_eq!(report["ready"], json!(false));
    let mut kinds = blocker_kinds(&report);
    kinds.sort();
    assert_eq!(
        kinds,
        vec!["active_epic_run", "active_reservation", "uncertain_landing"]
    );
}
