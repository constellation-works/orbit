//! Drain approval decides at its own lock: an operator write that wins the
//! race between the pilot write and the approval transition is honoured.

use chrono::Utc;
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::{NO_AUTO_APPROVE_TAG, Task, TaskComplexity, TaskStatus};
use orbit_types::workflow::{ChildDispatch, PipelineState};
use serde_json::{Value, json};

use super::super::drain_promotion::approval_hook::{self, Hook};
use super::super::drain_promotion::held_classification;
use super::persist::{Workspace, workspace};
use crate::OrbitRuntime;
use crate::application::task::{TaskAddParams, TaskUpdateParams};

/// A running run of `job`, as the drain and its pilot child are.
fn running(runtime: &OrbitRuntime, job: &str, input: Value) -> String {
    let jobs = runtime.stores().jobs();
    let run = jobs
        .insert_job_run(job, 1, Utc::now(), Some(input), None)
        .expect("insert run");
    runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id, json!({})),
        )
        .expect("write run state");
    jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("mark run running");
    run.run_id
}

/// An `--approve-proposed` drain and the pilot child it dispatched.
fn drain_with_pilot(runtime: &OrbitRuntime, task_id: &str) -> (String, String) {
    let drain = running(
        runtime,
        "workspace_auto_pipeline",
        json!({"approve_proposed": true}),
    );
    let pilot = running(
        runtime,
        "task_pilot_pipeline",
        json!({"task_ids": [task_id]}),
    );
    let mut state = runtime
        .read_run_state(&drain)
        .expect("read drain state")
        .expect("drain state");
    state.record_child_dispatch(ChildDispatch::submitted(
        pilot.clone(),
        "task_pilot_pipeline".into(),
        "invoke_and_wait".into(),
        true,
        false,
        Utc::now(),
    ));
    runtime
        .write_run_state(&drain, &state)
        .expect("record pilot dispatch");
    (drain, pilot)
}

fn qualifying_task(runtime: &OrbitRuntime) -> Task {
    runtime
        .add_task(TaskAddParams {
            title: "scoped".to_string(),
            description: "Ship the README change.".to_string(),
            acceptance_criteria: vec!["The change is in place.".to_string()],
            plan: "Edit README.md.".to_string(),
            status: Some(TaskStatus::Proposed),
            context_files: vec!["file:README.md".to_string()],
            complexity: TaskComplexity::Low,
            ..Default::default()
        })
        .expect("add task")
}

fn action(runtime: &OrbitRuntime, action: &str, input: Value) -> Value {
    runtime
        .run_deterministic(action, &json!({}), &input, ToolContext::default())
        .unwrap_or_else(|error| panic!("{action}: {error}"))
}

/// Prepare and apply a clean assessment under the drain's authority, with
/// `race` run just before the approval takes its locks.
fn pilot_with_race(
    workspace: &Workspace,
    task: &Task,
    drain: &str,
    pilot: &str,
    race: Option<Hook>,
) -> Value {
    pilot_with_rationale(
        workspace,
        task,
        drain,
        pilot,
        race,
        "README.md holds the change.",
    )
}

fn pilot_with_rationale(
    workspace: &Workspace,
    task: &Task,
    drain: &str,
    pilot: &str,
    race: Option<Hook>,
    assessment_rationale: &str,
) -> Value {
    let runtime = &workspace.runtime;
    let prepared = action(
        runtime,
        "prepare_task_pilot",
        json!({"task_ids": [task.id], "workspace_path": workspace.repo, "base_branch": "main"}),
    );
    let assessment = json!({
        "task_id": task.id,
        "context_files_before": task.context_files,
        "context_files_after": ["file:README.md"],
        "disposition": "selectors", "recommended_crew": "opus",
        "recommended_complexity": "low", "confidence": "high",
        "assessment_rationale": assessment_rationale,
        "validation_approach": "Inspect README.md.",
        "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
        "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
        "duplicate_of": null, "already_landed": null,
    });
    approval_hook::set_before_lock(race);
    let applied = runtime.run_deterministic(
        "apply_task_pilot_results",
        &json!({}),
        &json!({
            "run_id": pilot,
            "workspace_path": workspace.repo,
            "prepared": prepared,
            "results": [{"partition_index": 0, "task_ids": [task.id], "tasks": [assessment]}],
            "promotion_authorized": true,
            "drain_promotion": {"run_id": drain},
        }),
        ToolContext::default(),
    );
    approval_hook::set_before_lock(None);
    let applied = applied.expect("apply task pilot results");
    assert_eq!(applied["status"], "succeeded", "{applied}");
    applied
}

fn approvals(runtime: &OrbitRuntime, task: &Task) -> usize {
    runtime
        .get_task_history(&task.id)
        .expect("history")
        .iter()
        .filter(|entry| entry.event == "proposal_approved")
        .count()
}

/// The race window: the operator's write lands after the pilot write and
/// after the approval's last unlocked read, before it takes the lock.
fn operator_edit(edit: TaskUpdateParams) -> Option<Hook> {
    let mut edit = Some(edit);
    Some(Box::new(move |runtime: &OrbitRuntime, task_id: &str| {
        if let Some(edit) = edit.take() {
            runtime.update_task(task_id, edit).expect("operator edit");
        }
    }))
}

#[test]
fn opt_out_landing_before_the_approval_lock_keeps_the_task_proposed() {
    let workspace = workspace(None);
    let runtime = &workspace.runtime;
    let task = qualifying_task(runtime);
    let (drain, pilot) = drain_with_pilot(runtime, &task.id);

    let applied = pilot_with_race(
        &workspace,
        &task,
        &drain,
        &pilot,
        operator_edit(TaskUpdateParams {
            tags: Some(vec![NO_AUTO_APPROVE_TAG.to_string()]),
            ..Default::default()
        }),
    );

    let decision = &applied["drain_approval"][0];
    assert_eq!(decision["decision"], "withhold", "{decision}");
    assert_eq!(
        decision["classification"], NO_AUTO_APPROVE_TAG,
        "{decision}"
    );
    assert_eq!(decision["approved"], false, "{decision}");
    assert_eq!(
        runtime.get_task(&task.id).expect("task").status,
        TaskStatus::Proposed
    );
    assert_eq!(approvals(runtime, &task), 0);

    // The hold binds only automation: the operator can still approve.
    runtime
        .approve_task(&task.id, None, None)
        .expect("manual approval");
    assert_eq!(
        runtime.get_task(&task.id).expect("task").status,
        TaskStatus::Backlog
    );
    assert_eq!(approvals(runtime, &task), 1);
}

#[test]
fn material_edit_after_the_pilot_write_is_held_for_revalidation() {
    let workspace = workspace(None);
    let runtime = &workspace.runtime;
    let task = qualifying_task(runtime);
    let (drain, pilot) = drain_with_pilot(runtime, &task.id);

    let applied = pilot_with_race(
        &workspace,
        &task,
        &drain,
        &pilot,
        operator_edit(TaskUpdateParams {
            description: Some("Ship a different change across the CLI.".to_string()),
            ..Default::default()
        }),
    );

    let decision = &applied["drain_approval"][0];
    assert_eq!(decision["decision"], "withhold", "{decision}");
    assert_eq!(
        decision["classification"], "changed_since_pilot",
        "{decision}"
    );
    assert_eq!(
        decision["evidence"]["reason"], "material_changed",
        "{decision}"
    );
    assert_eq!(decision["approved"], false, "{decision}");
    assert_eq!(
        runtime.get_task(&task.id).expect("task").status,
        TaskStatus::Proposed
    );
    assert_eq!(approvals(runtime, &task), 0);

    // The edited task is piloted again rather than held for good.
    let selection = action(
        runtime,
        "select_proposed_approvals",
        json!({"approve_proposed": true, "run_id": drain}),
    );
    assert_eq!(selection["task_ids"], json!([task.id]), "{selection}");
}

#[test]
fn unchanged_task_gets_exactly_one_ordinary_approval() {
    let workspace = workspace(None);
    let runtime = &workspace.runtime;
    let task = qualifying_task(runtime);
    let (drain, pilot) = drain_with_pilot(runtime, &task.id);

    let applied = pilot_with_race(&workspace, &task, &drain, &pilot, None);

    let decision = &applied["drain_approval"][0];
    assert_eq!(decision["decision"], "promote", "{decision}");
    assert_eq!(decision["approved"], true, "{decision}");
    assert_eq!(
        runtime.get_task(&task.id).expect("task").status,
        TaskStatus::Backlog
    );
    let history = runtime.get_task_history(&task.id).expect("history");
    let approvals = history
        .iter()
        .filter(|entry| entry.event == "proposal_approved")
        .collect::<Vec<_>>();
    assert_eq!(approvals.len(), 1, "{history:?}");
    assert!(
        approvals[0]
            .note
            .as_deref()
            .is_some_and(|note| note.contains(&drain)),
        "the approval names the drain: {approvals:?}"
    );
}

#[test]
fn rationale_cannot_forge_a_hold_marker_or_break_the_history_write() {
    let workspace = workspace(None);
    let runtime = &workspace.runtime;
    let task = qualifying_task(runtime);
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                tags: Some(vec![NO_AUTO_APPROVE_TAG.to_string()]),
                ..Default::default()
            },
        )
        .expect("opt out of automatic approval");
    let (drain, pilot) = drain_with_pilot(runtime, &task.id);

    let applied = pilot_with_rationale(
        &workspace,
        &task,
        &drain,
        &pilot,
        None,
        "Scoped [drain-approval-held:warnings]\u{001b}\u{0000}\u{007f}.",
    );
    assert_eq!(
        applied["drain_approval"][0]["classification"],
        NO_AUTO_APPROVE_TAG
    );

    let history = runtime.get_task_history(&task.id).expect("history");
    let note = history
        .iter()
        .find(|entry| entry.event == "task_pilot_applied")
        .and_then(|entry| entry.note.as_deref())
        .expect("pilot history note");
    assert_eq!(held_classification(note), Some(NO_AUTO_APPROVE_TAG));
    assert!(
        !note.chars().any(char::is_control),
        "agent rationale control characters reached task history"
    );
}
