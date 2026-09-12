//! Explainable task-pilot complexity assessment and persistence fixtures.

use chrono::{Duration, Utc};
use orbit_types::task::{Task, TaskComplexity, TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::{AutoTaskSchedule, AutoTaskTemplate, DedupePolicy};
use serde_json::{Value, json};

use super::super::task_pilot::{apply, member_ready, prepare};
use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::test_support::{
    runtime_with_workspace_layout, write_workspace_file,
};
use crate::application::auto_tasks::AutoTaskAddParams;
use crate::application::auto_tasks::scheduler::{SchedulerOptions, run_auto_task_scheduler_at};
use crate::application::task::{TaskAddParams, TaskUpdateParams};

fn seed_task(runtime: &OrbitRuntime, title: &str) -> Task {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture task: {title}"),
            acceptance_criteria: vec!["The fixture outcome is observable.".to_string()],
            plan: "Inspect and update the fixture.".to_string(),
            priority: TaskPriority::Medium,
            complexity: TaskComplexity::Unassessed,
            task_type: Some(TaskType::Chore),
            status: Some(TaskStatus::Backlog),
            ..TaskAddParams::default()
        })
        .expect("seed assessment task")
}

fn prepare_task(runtime: &OrbitRuntime, repo_root: &std::path::Path, task: &Task) -> Value {
    prepare(
        runtime,
        "prepare_task_pilot",
        &json!({"task_ids": [task.id], "workspace_path": repo_root}),
    )
    .expect("prepare assessment task")
}

fn assessment(task: &Task, selector: &str, complexity: &str) -> Value {
    json!({
        "task_id": task.id,
        "context_files_before": task.context_files,
        "context_files_after": [selector],
        "disposition": "selectors",
        "recommended_crew": "luna",
        "recommended_complexity": complexity,
        "assessment_rationale": "The repair certainty, behavioral change, coupling, and validation boundary support this rating.",
        "confidence": "high",
        "evidence_gaps": [],
        "validation_approach": "Run the focused behavioral checks.",
        "reassessment_triggers": ["the owning boundary changes"],
        "blocked_by": [],
        "duplicate_of": null,
        "already_landed": null,
        "release_action_required": null,
        "adr_conflicts": [],
        "utility_warnings": [],
        "surface_warnings": [],
    })
}

fn apply_assessment(
    runtime: &OrbitRuntime,
    repo_root: &std::path::Path,
    prepared: Value,
    task: &Task,
    assessment: Value,
) -> Value {
    apply(
        runtime,
        "apply_task_pilot_results",
        &json!({
            "prepared": prepared,
            "results": [{
                "partition_index": 0,
                "task_ids": [task.id],
                "tasks": [assessment],
                "summary": "assessment fixture",
            }],
            "workspace_path": repo_root,
        }),
    )
    .expect("apply assessment")
}

#[test]
fn representative_repairs_persist_complexity_independently_of_urgency() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    let cases = [
        ("critical compatible bump", "src/dependency.lock", "low"),
        ("caller migration", "src/caller.rs", "medium"),
        ("trust-boundary redesign", "src/trust.rs", "hard"),
    ];

    for (title, path, complexity) in cases {
        write_workspace_file(&repo_root, path);
        let task = seed_task(&runtime, title);
        if title.starts_with("critical") {
            runtime
                .update_task(
                    &task.id,
                    TaskUpdateParams {
                        priority: Some(TaskPriority::Critical),
                        ..TaskUpdateParams::default()
                    },
                )
                .expect("raise fixture urgency");
        }
        let task = runtime.get_task(&task.id).expect("reload assessment task");
        let prepared = prepare_task(&runtime, &repo_root, &task);
        let output = apply_assessment(
            &runtime,
            &repo_root,
            prepared,
            &task,
            assessment(&task, &format!("file:{path}"), complexity),
        );

        assert_eq!(output["status"], "succeeded");
        assert_eq!(
            runtime
                .get_task(&task.id)
                .expect("assessed task")
                .complexity,
            Some(complexity.parse::<TaskComplexity>().expect("complexity"))
        );
    }
}

#[test]
fn missing_evidence_stays_unassessed_with_an_actionable_preparation_outcome() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "src/investigate.rs");
    let task = seed_task(&runtime, "uncertain repair");
    let prepared = prepare_task(&runtime, &repo_root, &task);
    let mut result = assessment(&task, "file:src/investigate.rs", "unassessed");
    result["confidence"] = json!("low");
    result["assessment_rationale"] =
        json!("The failing boundary is known, but the owning caller has not been identified.");
    result["evidence_gaps"] = json!(["Trace the caller that supplies the untrusted value."]);
    result["validation_approach"] =
        json!("Inspect callers, then rerun task-pilot with the discovered owner.");

    let output = apply_assessment(&runtime, &repo_root, prepared, &task, result);

    assert_eq!(output["status"], "succeeded");
    assert!(!member_ready(&output["tasks"][0]));
    let current = runtime.get_task(&task.id).expect("unassessed task");
    assert_eq!(current.complexity, Some(TaskComplexity::Unassessed));
    assert_eq!(current.context_files, vec!["file:src/investigate.rs"]);
}

/// The operational shape behind ORB-12099: an auto-task-minted
/// `no-diff-expected` review whose durable result lives outside the repository,
/// so its assessment never produces selectors.
fn host_operational_assessment(task: &Task, complexity: &str) -> Value {
    // The selector the shared fixture proposes is replaced by the empty list
    // this disposition requires.
    let mut assessment = assessment(task, "file:src/unused.rs", complexity);
    assessment["context_files_after"] = json!([]);
    assessment["disposition"] = json!("host_operational");
    assessment["evidence"] = json!(
        "The task is tagged no-diff-expected; its durable deliverable is filed Orbit tasks plus a review cursor."
    );
    assessment["recommended_crew"] = json!("opus");
    assessment
}

fn task_pilot_audits(runtime: &OrbitRuntime, task_id: &str) -> Vec<Value> {
    runtime
        .get_task_history(task_id)
        .expect("task history")
        .into_iter()
        .filter(|event| event.event == "task_pilot_applied")
        .map(|event| {
            let note = event.note.expect("task_pilot_applied note");
            let (_receipt, audit) = note.split_once('\n').expect("audit payload");
            serde_json::from_str::<Value>(audit).expect("audit json")
        })
        .collect()
}

/// ORB-12099 reported a successful task-pilot event (`unassessed` -> `hard`)
/// against a task that later read `unassessed`, and asked whether the
/// assessment failed to persist. It did not: the durable history carries two
/// `task_pilot_applied` audits, and the second one — a later pass that reported
/// `unassessed` with evidence gaps — is the legitimate write that changed the
/// value. This pins both halves: an applied assessment is durably readable
/// across a runtime reopen and survives an auto-task refresh untouched, and a
/// subsequent assessment changes it only through its own audited write.
#[test]
fn an_applied_assessment_is_durable_and_only_a_later_audited_pass_changes_it() {
    let (root, runtime, repo_root) = runtime_with_workspace_layout();
    runtime
        .auto_task_add(AutoTaskAddParams {
            name: "code-review".to_string(),
            description: "Review recently merged changes".to_string(),
            schedule: AutoTaskSchedule::Interval { every_minutes: 60 },
            template: AutoTaskTemplate {
                title: "[auto-task] Review recently merged changes".to_string(),
                description: "Review the code merged since the last sweep.".to_string(),
                acceptance_criteria: vec!["Findings are filed as tasks.".to_string()],
                task_type: TaskType::Chore,
                tags: vec!["code-review".to_string(), "no-diff-expected".to_string()],
                required_tools: vec![],
                priority: TaskPriority::Medium,
                crew: Some("opus".to_string()),
                status: TaskStatus::Backlog,
            },
            dedupe: DedupePolicy::SkipIfOpen,
        })
        .expect("add auto-task definition");
    let minted = runtime.auto_task_mint("code-review").expect("mint review");
    assert_eq!(minted.complexity, Some(TaskComplexity::Unassessed));

    let prepared = prepare_task(&runtime, &repo_root, &minted);
    let applied = apply_assessment(
        &runtime,
        &repo_root,
        prepared,
        &minted,
        host_operational_assessment(&minted, "hard"),
    );
    assert_eq!(applied["status"], "succeeded");
    assert_eq!(
        runtime.get_task(&minted.id).expect("assessed").complexity,
        Some(TaskComplexity::Hard)
    );

    // Durable, not merely in-memory: a second runtime over the same roots reads
    // the assessed value back.
    let reopened = OrbitRuntime::from_roots(
        &root.path().join("home/.orbit"),
        &root.path().join("repo/.orbit"),
    )
    .expect("reopen runtime");
    assert_eq!(
        reopened.get_task(&minted.id).expect("reopened").complexity,
        Some(TaskComplexity::Hard)
    );

    // Auto-task refresh sees an open instance, skips, and rewrites nothing.
    let outcome = run_auto_task_scheduler_at(
        &reopened,
        Utc::now() + Duration::hours(2),
        SchedulerOptions::default(),
    )
    .expect("auto-task refresh pass");
    assert!(
        outcome
            .reports
            .iter()
            .all(|report| report.action != "fired"),
        "an open instance must suppress a duplicate mint"
    );
    assert_eq!(
        reopened
            .get_task(&minted.id)
            .expect("after refresh")
            .complexity,
        Some(TaskComplexity::Hard)
    );

    // A later pass that cannot support a rating is a legitimate audited write,
    // not a lost one: both audits remain readable, in order.
    let reprepared = prepare_task(
        &runtime,
        &repo_root,
        &runtime.get_task(&minted.id).expect("current"),
    );
    let mut regression = host_operational_assessment(&minted, "unassessed");
    regression["confidence"] = json!("low");
    regression["evidence_gaps"] =
        json!(["The full cursor-to-HEAD review has not been completed at the pinned revision."]);
    let reapplied = apply_assessment(&runtime, &repo_root, reprepared, &minted, regression);
    assert_eq!(reapplied["status"], "succeeded");

    let audits = task_pilot_audits(&runtime, &minted.id);
    assert_eq!(audits.len(), 2);
    assert_eq!(audits[0]["complexity_before"], json!("unassessed"));
    assert_eq!(audits[0]["complexity_after"], json!("hard"));
    assert_eq!(audits[1]["complexity_before"], json!("hard"));
    assert_eq!(audits[1]["complexity_after"], json!("unassessed"));
    assert_eq!(
        runtime.get_task(&minted.id).expect("reassessed").complexity,
        Some(TaskComplexity::Unassessed)
    );
}

#[test]
fn assessed_complexity_with_evidence_gaps_persists_when_confidence_is_low() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "src/gap.rs");
    let task = seed_task(&runtime, "assessed repair with rating-changing unknowns");
    let prepared = prepare_task(&runtime, &repo_root, &task);
    let mut result = assessment(&task, "file:src/gap.rs", "medium");
    result["confidence"] = json!("low");
    result["evidence_gaps"] =
        json!(["Unresolved fixture semantics might change the repair to hard."]);

    let output = apply_assessment(&runtime, &repo_root, prepared, &task, result);
    assert_eq!(output["status"], "succeeded");
    let updated = runtime.get_task(&task.id).expect("reload assessed task");
    assert_eq!(updated.complexity, Some(TaskComplexity::Medium));
}

#[test]
fn assessed_complexity_with_evidence_gaps_is_rejected_when_confidence_is_high() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "src/gap.rs");
    let task = seed_task(&runtime, "high-confidence repair claiming gaps");
    let prepared = prepare_task(&runtime, &repo_root, &task);
    let mut result = assessment(&task, "file:src/gap.rs", "medium");
    result["confidence"] = json!("high");
    result["evidence_gaps"] = json!(["Contradictory gap under high confidence."]);

    let output = apply(
        &runtime,
        "apply_task_pilot_results",
        &json!({
            "prepared": prepared,
            "results": [{
                "partition_index": 0,
                "task_ids": [task.id],
                "tasks": [result],
                "summary": "assessment fixture",
            }],
            "workspace_path": &repo_root,
        }),
    )
    .expect("apply result");
    assert_eq!(output["status"], "failed");
    assert_eq!(output["repair_count"], 1);
    let errors = &output["repair_partitions"][0]["validation_errors"];
    assert!(
        errors.as_array().unwrap().iter().any(|e| e
            .as_str()
            .unwrap()
            .contains("with high confidence must not have evidence_gaps")),
        "expected high-confidence evidence_gaps rejection, got: {errors:?}"
    );
}
