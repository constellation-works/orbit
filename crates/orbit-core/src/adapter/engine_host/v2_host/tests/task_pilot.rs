use chrono::Utc;
use orbit_types::task::{Task, TaskComplexity, TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::{JobRunState, PipelineState};
use serde_json::{Value, json};

use super::super::task_pilot::{
    apply, inject_concurrent_edit_before_locked_apply, member_ready, prepare,
};
use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::test_support::{
    runtime_with_workspace_layout, write_workspace_file,
};
use crate::application::task::{TaskAddParams, TaskUpdateParams};

fn seed_task(
    runtime: &OrbitRuntime,
    title: &str,
    status: TaskStatus,
    tags: &[&str],
    context_files: &[&str],
) -> Task {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture task: {title}"),
            acceptance_criteria: vec!["The fixture outcome is observable.".to_string()],
            tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
            plan: "Inspect and update the fixture.".to_string(),
            context_files: context_files
                .iter()
                .map(|selector| (*selector).to_string())
                .collect(),
            priority: TaskPriority::Medium,
            task_type: Some(TaskType::Chore),
            status: Some(status),
            ..TaskAddParams::default()
        })
        .expect("seed task")
}

fn prepared(runtime: &OrbitRuntime, repo_root: &std::path::Path, task_ids: &[String]) -> Value {
    prepared_with_partition_size(runtime, repo_root, task_ids, 5)
}

fn prepared_with_partition_size(
    runtime: &OrbitRuntime,
    repo_root: &std::path::Path,
    task_ids: &[String],
    max_partition_size: usize,
) -> Value {
    prepare(
        runtime,
        "prepare_task_pilot",
        &json!({
            "task_ids": task_ids,
            "workspace_path": repo_root,
            "max_partition_size": max_partition_size,
        }),
    )
    .expect("prepare explicit task-pilot selection")
}

fn seed_active_preparation(runtime: &OrbitRuntime, prepared: Value) -> String {
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("task_pilot_pipeline", 1, Utc::now(), Some(json!({})), None)
        .expect("insert active pilot run");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("mark pilot run running");
    let mut state = PipelineState::new(
        run.run_id.clone(),
        "task_pilot_pipeline".to_string(),
        json!({}),
    );
    state.record_step(0, JobRunState::Success, Some(prepared), None);
    runtime
        .write_run_state(&run.run_id, &state)
        .expect("checkpoint prepared pilot output");
    run.run_id
}

fn selector_assessment(task: &Task, after: Vec<&str>) -> Value {
    selector_assessment_with_complexity(task, after, "medium")
}

fn selector_assessment_with_complexity(task: &Task, after: Vec<&str>, complexity: &str) -> Value {
    json!({
        "task_id": task.id,
        "context_files_before": task.context_files,
        "context_files_after": after,
        "disposition": "selectors",
        "recommended_crew": "luna",
        "recommended_complexity": complexity,
        "assessment_rationale": "The repair changes one known caller and has bounded validation.",
        "confidence": "high",
        "evidence_gaps": [],
        "validation_approach": "Run the focused caller tests.",
        "reassessment_triggers": ["the target API changes"],
        "blocked_by": [],
        "duplicate_of": null,
        "already_landed": null,
        "adr_conflicts": [],
        "utility_warnings": [],
        "surface_warnings": [],
    })
}

fn partition_result(partition_index: usize, task_ids: &[String], tasks: Vec<Value>) -> Value {
    json!({
        "partition_index": partition_index,
        "task_ids": task_ids,
        "tasks": tasks,
        "summary": "fixture partition",
    })
}

/// Materialize `count` workspace files and return the canonical `file:`
/// selectors naming them, so an over-attachment fixture proposes selectors
/// that all resolve.
fn workspace_selectors(repo_root: &std::path::Path, count: usize) -> Vec<String> {
    (0..count)
        .map(|index| {
            let relative = format!("src/module_{index:02}.rs");
            write_workspace_file(repo_root, &relative);
            format!("file:{relative}")
        })
        .collect()
}

#[test]
fn automatic_readiness_requires_selectors_and_no_deferring_finding() {
    let mut assessment = json!({
        "task_id": "ORB-FIXTURE",
        "disposition": "selectors",
        "context_files_after": ["file:src/existing.rs"],
        "recommended_complexity": "medium",
        "blocked_by": [],
        "duplicate_of": null,
        "already_landed": null,
        "release_action_required": null,
        "adr_conflicts": [],
        "utility_warnings": [],
        "surface_warnings": [],
    });
    assert!(member_ready(&assessment));

    // A pilot may return the selectors it would change and still report that
    // the correct repair is an operator-reserved release action. That finding
    // withholds automatic promotion the same way a duplicate does.
    assessment["release_action_required"] = json!({
        "action": "publish the recorded release version as a release operation",
        "evidence": "the failing job resolves a version this repository records but never published",
    });
    assert!(!member_ready(&assessment));

    assessment["release_action_required"] = Value::Null;
    assert!(member_ready(&assessment));
    assessment["disposition"] = json!("verified_no_diff");
    assert!(!member_ready(&assessment));
}

#[test]
fn automatic_discovery_admits_only_provenance_tagged_no_diff_expected_auto_tasks() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "src/existing.rs");
    let mut eligible = Vec::new();
    for index in 0..7 {
        eligible.push(seed_task(
            &runtime,
            &format!("eligible-{index}"),
            if index % 2 == 0 {
                TaskStatus::Proposed
            } else {
                TaskStatus::Backlog
            },
            &[],
            &[],
        ));
    }
    let active = seed_task(&runtime, "active", TaskStatus::InProgress, &[], &[]);
    let review = seed_task(&runtime, "review", TaskStatus::Review, &[], &[]);
    let terminal = seed_task(&runtime, "terminal", TaskStatus::Done, &[], &[]);
    let no_diff_needed = seed_task(
        &runtime,
        "no-diff-needed",
        TaskStatus::Backlog,
        &["no-diff-needed"],
        &[],
    );
    let no_diff_expected = seed_task(
        &runtime,
        "no-diff-expected",
        TaskStatus::Proposed,
        &["no-diff-expected"],
        &[],
    );
    let no_diff_auto_task = seed_task(
        &runtime,
        "no-diff-expected auto-task",
        TaskStatus::Backlog,
        &["no-diff-expected", "auto-task:review"],
        &[],
    );
    let scoped = seed_task(
        &runtime,
        "already-scoped",
        TaskStatus::Backlog,
        &[],
        &["file:src/existing.rs"],
    );
    runtime
        .update_task(
            &scoped.id,
            TaskUpdateParams {
                complexity: Some(TaskComplexity::Medium),
                ..TaskUpdateParams::default()
            },
        )
        .expect("mark manually scoped fixture as assessed");

    let output = prepare(
        &runtime,
        "prepare_task_pilot",
        &json!({ "workspace_path": repo_root }),
    )
    .expect("automatic discovery");

    assert_eq!(output["mode"], "automatic");
    assert_eq!(output["task_count"], 8);
    assert_eq!(output["partition_count"], 2);
    assert_eq!(
        output["partitions"][0]["task_ids"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    assert_eq!(
        output["partitions"][1]["task_ids"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let selected = output["task_ids"].as_array().expect("selected task ids");
    for task in eligible {
        assert!(selected.iter().any(|value| value == &json!(task.id)));
    }
    let excluded = output["excluded"].as_array().expect("excluded entries");
    for task in [active, review, terminal] {
        assert!(excluded.iter().any(|entry| {
            entry["task_id"] == task.id && entry["reason"] == "status_not_eligible"
        }));
    }
    for task in [no_diff_needed, no_diff_expected] {
        assert!(
            excluded
                .iter()
                .any(|entry| { entry["task_id"] == task.id && entry["reason"] == "no_diff_task" })
        );
    }
    assert!(
        selected
            .iter()
            .any(|value| value == &json!(no_diff_auto_task.id))
    );
    assert!(excluded.iter().any(|entry| {
        entry["task_id"] == scoped.id && entry["reason"] == "context_files_not_empty"
    }));
}

#[test]
fn automatic_discovery_bounds_excluded_evidence_as_terminal_history_grows() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    let eligible = (0..3)
        .map(|index| {
            seed_task(
                &runtime,
                &format!("eligible-{index}"),
                TaskStatus::Backlog,
                &[],
                &[],
            )
        })
        .collect::<Vec<_>>();
    const TERMINAL_COUNT: usize = 200;
    for index in 0..TERMINAL_COUNT {
        seed_task(
            &runtime,
            &format!("terminal-{index}"),
            TaskStatus::Done,
            &[],
            &[],
        );
    }

    let started = std::time::Instant::now();
    let output = prepare(
        &runtime,
        "prepare_task_pilot",
        &json!({ "workspace_path": repo_root }),
    )
    .expect("automatic discovery bounds evidence over a large terminal history");
    let elapsed = started.elapsed();

    assert_eq!(output["mode"], "automatic");
    assert_eq!(
        output["task_ids"],
        json!(
            eligible
                .iter()
                .map(|task| task.id.clone())
                .collect::<Vec<_>>()
        ),
        "a large terminal history must not change the selected tasks or their order"
    );

    let excluded_sample = output["excluded"].as_array().expect("excluded sample");
    assert!(
        excluded_sample.len() <= 20,
        "the itemized sample must stay capped regardless of terminal history size, got {}",
        excluded_sample.len()
    );
    assert_eq!(output["excluded_total"], json!(TERMINAL_COUNT));
    assert_eq!(
        output["excluded_by_reason"]["status_not_eligible"],
        json!(TERMINAL_COUNT),
        "omitted exclusion detail must still be explicitly counted by reason"
    );
    assert_eq!(output["excluded_sample_truncated"], true);
    assert_eq!(
        output["excluded_omitted_count"],
        json!(TERMINAL_COUNT - excluded_sample.len())
    );

    // Representative preparation benchmark: this records that payload bytes
    // and wall-clock time stay within a generous, non-pathological bound at
    // this terminal-history size. It does not assert a latency improvement
    // over the prior unbounded implementation (no such baseline was measured
    // here) — only that bounding the evidence keeps both bytes and time sane.
    let serialized = serde_json::to_string(&output).expect("serialize prepare output");
    assert!(
        serialized.len() < 8_000,
        "prepare payload grew unexpectedly large ({} bytes) with {TERMINAL_COUNT} terminal tasks",
        serialized.len()
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "prepare took unexpectedly long ({elapsed:?}) for {TERMINAL_COUNT} terminal tasks"
    );
}

#[test]
fn discovery_reuses_active_preparation_evidence_and_selects_only_new_work() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    let already_prepared = seed_task(&runtime, "already prepared", TaskStatus::Backlog, &[], &[]);
    let first_snapshot = prepared(
        &runtime,
        &repo_root,
        std::slice::from_ref(&already_prepared.id),
    );
    let active_run_id = seed_active_preparation(&runtime, first_snapshot);
    let new_task = seed_task(
        &runtime,
        "new after preparation",
        TaskStatus::Backlog,
        &[],
        &[],
    );

    let output = prepare(
        &runtime,
        "prepare_task_pilot",
        &json!({ "workspace_path": repo_root }),
    )
    .expect("automatic discovery excludes durable active preparation");

    assert_eq!(output["task_ids"], json!([new_task.id]));
    assert!(output["excluded"].as_array().unwrap().iter().any(|entry| {
        entry["task_id"] == already_prepared.id
            && entry["reason"] == "active_pilot_prepared"
            && entry["prepared_by_run_ids"] == json!([active_run_id])
    }));
}

#[test]
fn explicit_discovery_refuses_duplicate_active_preparation() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    let task = seed_task(&runtime, "already prepared", TaskStatus::Backlog, &[], &[]);
    let first_snapshot = prepared(&runtime, &repo_root, std::slice::from_ref(&task.id));
    let active_run_id = seed_active_preparation(&runtime, first_snapshot);

    let error = prepare(
        &runtime,
        "prepare_task_pilot",
        &json!({
            "task_ids": [task.id],
            "workspace_path": repo_root,
        }),
    )
    .expect_err("explicit overlap must not launch duplicate pilot work");

    assert!(error.to_string().contains("already prepared"));
    assert!(error.to_string().contains(&active_run_id));
}

#[test]
fn explicit_mode_selects_exact_ids_even_with_nonempty_context_or_active_status() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "src/existing.rs");
    let scoped = seed_task(
        &runtime,
        "scoped",
        TaskStatus::Review,
        &[],
        &["file:src/existing.rs"],
    );
    let backlog = seed_task(&runtime, "backlog", TaskStatus::Backlog, &[], &[]);

    let output = prepared(
        &runtime,
        &repo_root,
        &[scoped.id.clone(), backlog.id.clone()],
    );

    assert_eq!(output["mode"], "explicit");
    assert_eq!(output["task_ids"], json!([scoped.id, backlog.id]));
    assert_eq!(output["excluded"], json!([]));
}

#[test]
fn explicit_audit_does_not_rewrite_in_progress_work() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "src/existing.rs");
    write_workspace_file(&repo_root, "src/new.rs");
    let running = seed_task(
        &runtime,
        "running",
        TaskStatus::InProgress,
        &[],
        &["file:src/existing.rs"],
    );
    let snapshot = prepared(&runtime, &repo_root, std::slice::from_ref(&running.id));
    let output = apply(
        &runtime,
        "apply_task_pilot_results",
        &json!({
            "prepared": snapshot,
            "results": [partition_result(
                0,
                std::slice::from_ref(&running.id),
                vec![selector_assessment_with_complexity(
                    &running,
                    vec!["file:src/new.rs"],
                    "hard",
                )],
            )],
            "workspace_path": repo_root,
        }),
    )
    .expect("active task is a structured stale outcome");

    assert_eq!(output["task_outcomes"][0]["reason"], "status_not_mutable");
    let current = runtime.get_task(&running.id).expect("running task");
    assert_eq!(current.context_files, vec!["file:src/existing.rs"]);
    assert_eq!(current.complexity, Some(TaskComplexity::Unassessed));
}

#[test]
fn apply_validates_all_results_then_mutates_assessment_fields_only() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "src/alpha.rs");
    let alpha = seed_task(&runtime, "alpha", TaskStatus::Backlog, &["pilot"], &[]);
    let operational = seed_task(
        &runtime,
        "operational",
        TaskStatus::Backlog,
        &["host-operation"],
        &[],
    );
    let task_ids = vec![alpha.id.clone(), operational.id.clone()];
    runtime
        .update_task(
            &alpha.id,
            TaskUpdateParams {
                crew: Some(Some("sol".to_string())),
                ..TaskUpdateParams::default()
            },
        )
        .expect("set explicit crew override");
    let prepared_snapshot = prepared(&runtime, &repo_root, &task_ids);
    let before_alpha = runtime.get_task(&alpha.id).expect("alpha before");
    let before_operational = runtime
        .get_task(&operational.id)
        .expect("operational before");
    let result = partition_result(
        0,
        &task_ids,
        vec![
            selector_assessment(&alpha, vec!["file:src/alpha.rs"]),
            json!({
                "task_id": operational.id,
                "context_files_before": [],
                "context_files_after": [],
                "disposition": "host_operational",
                "evidence": "Changes host service state only; no repository artifact is modified.",
                "recommended_crew": "luna",
                "recommended_complexity": "low",
                "assessment_rationale": "No repository change is required.",
                "confidence": "high",
                "evidence_gaps": [],
                "validation_approach": "Verify the current source evidence.",
                "reassessment_triggers": ["the source revision changes"],
                "blocked_by": [],
                "duplicate_of": null,
                "already_landed": null,
                "adr_conflicts": [],
                "utility_warnings": ["requires host access"],
                "surface_warnings": [],
            }),
        ],
    );

    let output = apply(
        &runtime,
        "apply_task_pilot_results",
        &json!({
            "prepared": prepared_snapshot,
            "results": [result],
            "workspace_path": repo_root,
        }),
    )
    .expect("apply validated pilot results");

    let after_alpha = runtime.get_task(&alpha.id).expect("alpha after");
    let after_operational = runtime
        .get_task(&operational.id)
        .expect("operational after");
    assert_eq!(after_alpha.context_files, vec!["file:src/alpha.rs"]);
    assert_eq!(after_operational.context_files, Vec::<String>::new());
    assert_eq!(after_alpha.complexity, Some(TaskComplexity::Medium));
    assert_eq!(after_operational.complexity, Some(TaskComplexity::Low));
    assert_eq!(after_alpha.title, before_alpha.title);
    assert_eq!(after_alpha.status, before_alpha.status);
    assert_eq!(after_alpha.tags, before_alpha.tags);
    assert_eq!(after_alpha.plan, before_alpha.plan);
    assert_eq!(after_alpha.crew.as_deref(), Some("sol"));
    assert_eq!(after_operational.title, before_operational.title);
    assert_eq!(after_operational.status, before_operational.status);
    assert_eq!(output["status"], "succeeded");
    assert_eq!(output["partition_decisions"][0]["outcome"], "applied");
    assert_eq!(output["tasks"][0]["context_files_before"], json!([]));
    assert_eq!(
        output["tasks"][0]["context_files_after"],
        json!(["file:src/alpha.rs"])
    );
    assert_eq!(output["tasks"][0]["applied"], true);
    assert_eq!(output["tasks"][1]["applied"], true);
    assert_eq!(
        output["tasks"][1]["utility_warnings"],
        json!(["requires host access"])
    );
}

/// [ORB-11261] A concrete conflict-plus-evidence recommendation, expressed the
/// way the instruction now documents it (one string per finding), must
/// validate and apply normally.
#[test]
fn adr_conflicts_accepts_nonempty_string_array_with_conflict_and_evidence() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "src/alpha.rs");
    let alpha = seed_task(&runtime, "alpha", TaskStatus::Backlog, &[], &[]);
    let task_ids = vec![alpha.id.clone()];
    let prepared_snapshot = prepared(&runtime, &repo_root, &task_ids);
    let mut assessment = selector_assessment(&alpha, vec!["file:src/alpha.rs"]);
    assessment["adr_conflicts"] = json!([
        "crates/orbit-core/src/adapter/engine_host/v2_host/task_pilot.rs still \
         exports validate_recommendations with the signature this task would \
         change (verified via `rg -n \"fn validate_recommendations\"` at \
         source_revision)"
    ]);
    let result = partition_result(0, &task_ids, vec![assessment]);

    let output = apply(
        &runtime,
        "apply_task_pilot_results",
        &json!({
            "prepared": prepared_snapshot,
            "results": [result],
            "workspace_path": repo_root,
        }),
    )
    .expect("apply accepts a nonempty string-array adr_conflicts");

    assert_eq!(output["status"], "succeeded");
    assert_eq!(output["partition_decisions"][0]["outcome"], "applied");
    assert_eq!(output["tasks"][0]["applied"], true);
}

/// [ORB-11261] Reproduces the ORB-11259 incident directly: an agent that
/// returns `adr_conflicts` as an array of `{conflict, evidence}` objects
/// instead of plain strings must fail deterministic apply with a clear,
/// field-specific error before any task is mutated — never be coerced into a
/// successful typed response.
#[test]
fn adr_conflicts_rejects_object_array_conflict_evidence_shape() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "src/alpha.rs");
    let alpha = seed_task(&runtime, "alpha", TaskStatus::Backlog, &[], &[]);
    let task_ids = vec![alpha.id.clone()];
    let prepared_snapshot = prepared(&runtime, &repo_root, &task_ids);
    let mut assessment = selector_assessment(&alpha, vec!["file:src/alpha.rs"]);
    assessment["adr_conflicts"] = json!([
        {
            "conflict": "legacy ADR mandates a different storage layout",
            "evidence": "docs/design/foo/decisions.md",
        }
    ]);
    let result = partition_result(0, &task_ids, vec![assessment]);

    let output = apply(
        &runtime,
        "apply_task_pilot_results",
        &json!({
            "prepared": prepared_snapshot,
            "results": [result],
            "workspace_path": repo_root,
        }),
    )
    .expect("malformed adr_conflicts is a durable failed decision, not an error");

    assert_eq!(output["status"], "failed");
    assert_eq!(output["partition_decisions"][0]["outcome"], "failed");
    let error = output["partition_decisions"][0]["error"]
        .as_str()
        .expect("failed partition carries an error message");
    assert!(error.contains("adr_conflicts"));
    assert!(error.contains("must contain only strings"));
    assert!(
        runtime
            .get_task(&alpha.id)
            .unwrap()
            .context_files
            .is_empty()
    );
}

#[test]
fn invalid_selector_does_not_discard_valid_sibling_in_same_partition() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "src/alpha.rs");
    let alpha = seed_task(&runtime, "alpha", TaskStatus::Backlog, &[], &[]);
    let beta = seed_task(&runtime, "beta", TaskStatus::Backlog, &[], &[]);
    let task_ids = vec![alpha.id.clone(), beta.id.clone()];
    let prepared_snapshot = prepared(&runtime, &repo_root, &task_ids);
    let result = partition_result(
        0,
        &task_ids,
        vec![
            selector_assessment(&alpha, vec!["file:src/alpha.rs"]),
            selector_assessment(&beta, vec!["file:src/missing.rs"]),
        ],
    );

    let output = apply(
        &runtime,
        "apply_task_pilot_results",
        &json!({
            "prepared": prepared_snapshot,
            "results": [result],
            "workspace_path": repo_root,
        }),
    )
    .expect("invalid partition is reported as durable output");

    assert_eq!(output["status"], "failed");
    assert_eq!(output["partition_decisions"][0]["outcome"], "partial");
    assert!(
        output["partition_decisions"][0]["error"]
            .as_str()
            .unwrap()
            .contains("does not resolve")
    );
    assert_eq!(
        runtime.get_task(&alpha.id).unwrap().context_files,
        vec!["file:src/alpha.rs"]
    );
    assert!(runtime.get_task(&beta.id).unwrap().context_files.is_empty());

    let repaired = apply(
        &runtime,
        "apply_task_pilot_results",
        &json!({
            "prepared": output["repair_prepared"],
            "results": [partition_result(
                0,
                std::slice::from_ref(&beta.id),
                vec![selector_assessment(&beta, vec!["file:src/alpha.rs"])],
            )],
            "workspace_path": repo_root,
            "prior_applied_count": output["applied_count"],
            "carried_task_outcomes": output["non_repairable_outcomes"],
        }),
    )
    .expect("targeted repair reuses the deterministic apply boundary");
    assert_eq!(output["repair_count"], 1);
    assert_eq!(
        output["repair_partitions"][0]["validation_errors"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(repaired["status"], "succeeded");
    assert_eq!(repaired["applied_count"], 2);
    assert_eq!(
        runtime.get_task(&beta.id).unwrap().context_files,
        vec!["file:src/alpha.rs"]
    );
}

#[test]
fn replay_returns_already_applied_without_a_second_mutation() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "src/alpha.rs");
    write_workspace_file(&repo_root, "src/beta.rs");
    let task = seed_task(&runtime, "replay", TaskStatus::Backlog, &[], &[]);
    let task_ids = vec![task.id.clone()];
    let snapshot = prepared(&runtime, &repo_root, &task_ids);
    let result = partition_result(
        0,
        &task_ids,
        vec![selector_assessment(&task, vec!["file:src/alpha.rs"])],
    );
    let input = json!({
        "prepared": snapshot,
        "results": [result],
        "workspace_path": repo_root,
    });

    let first = apply(&runtime, "apply_task_pilot_results", &input).expect("first apply");
    let replay = apply(&runtime, "apply_task_pilot_results", &input).expect("replay apply");
    let changed = apply(
        &runtime,
        "apply_task_pilot_results",
        &json!({
            "prepared": input["prepared"],
            "results": [partition_result(
                0,
                &task_ids,
                vec![selector_assessment(&task, vec!["file:src/beta.rs"])],
            )],
            "workspace_path": repo_root,
        }),
    )
    .expect("changed replay is a structured stale result");

    assert_eq!(first["tasks"][0]["outcome"], "applied");
    assert_eq!(replay["tasks"][0]["outcome"], "already_applied", "{replay}");
    assert_eq!(changed["task_outcomes"][0]["outcome"], "stale");
    assert_eq!(
        runtime.get_task(&task.id).unwrap().context_files,
        vec!["file:src/alpha.rs"]
    );
    assert_eq!(
        runtime.get_task(&task.id).unwrap().complexity,
        Some(TaskComplexity::Medium)
    );
    let pilot_events = runtime
        .get_task_history(&task.id)
        .unwrap()
        .into_iter()
        .filter(|event| event.event == "task_pilot_applied")
        .collect::<Vec<_>>();
    assert_eq!(pilot_events.len(), 1);
    let audit = pilot_events[0].note.as_deref().expect("audit note");
    assert!(audit.starts_with("operation_id="), "{audit}");
    assert!(audit.contains("assessment_rationale"), "{audit}");
    assert!(audit.contains("validation_approach"), "{audit}");

    let reassessed_task = runtime.get_task(&task.id).expect("reassessed task");
    let fresh = prepared(
        &runtime,
        &repo_root,
        std::slice::from_ref(&reassessed_task.id),
    );
    let reassessment = partition_result(
        0,
        std::slice::from_ref(&reassessed_task.id),
        vec![selector_assessment_with_complexity(
            &reassessed_task,
            vec!["file:src/beta.rs"],
            "hard",
        )],
    );
    let changed = apply(
        &runtime,
        "apply_task_pilot_results",
        &json!({
            "prepared": fresh,
            "results": [reassessment],
            "workspace_path": repo_root,
        }),
    )
    .expect("fresh changed evidence is a new assessment");
    assert_eq!(changed["status"], "succeeded");
    let current = runtime.get_task(&task.id).expect("changed assessment");
    assert_eq!(current.context_files, vec!["file:src/beta.rs"]);
    assert_eq!(current.complexity, Some(TaskComplexity::Hard));
    let audits = runtime
        .get_task_history(&task.id)
        .unwrap()
        .into_iter()
        .filter(|event| event.event == "task_pilot_applied")
        .filter_map(|event| event.note)
        .collect::<Vec<_>>();
    assert_eq!(audits.len(), 2);
    assert_ne!(audits[0].lines().next(), audits[1].lines().next());
}

#[test]
fn stale_partition_does_not_discard_independent_valid_partition() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "src/first.rs");
    write_workspace_file(&repo_root, "src/new.rs");
    let first = seed_task(&runtime, "first pilot", TaskStatus::Backlog, &[], &[]);
    let new_task = seed_task(&runtime, "new pilot", TaskStatus::Backlog, &[], &[]);
    let task_ids = vec![first.id.clone(), new_task.id.clone()];
    let snapshot = prepared_with_partition_size(&runtime, &repo_root, &task_ids, 1);

    runtime
        .update_task(
            &first.id,
            TaskUpdateParams {
                context_files: Some(vec!["file:src/first.rs".to_string()]),
                ..TaskUpdateParams::default()
            },
        )
        .expect("overlapping pilot applies the first task");

    let mut stale_assessment = selector_assessment(&first, vec!["file:src/new.rs"]);
    stale_assessment["context_files_before"] = json!(["file:src/first.rs"]);
    let output = apply(
        &runtime,
        "apply_task_pilot_results",
        &json!({
            "prepared": snapshot,
            "results": [
                partition_result(
                    0,
                    std::slice::from_ref(&first.id),
                    vec![stale_assessment],
                ),
                partition_result(
                    1,
                    std::slice::from_ref(&new_task.id),
                    vec![selector_assessment(&new_task, vec!["file:src/new.rs"])],
                ),
            ],
            "workspace_path": repo_root,
        }),
    )
    .expect("partition outcomes remain durable even when the run must fail");

    assert_eq!(output["status"], "failed");
    assert_eq!(output["partition_decisions"][0]["outcome"], "skipped_stale");
    assert_eq!(
        output["partition_decisions"][0]["stale_tasks"][0]["reason"],
        "reported_context_snapshot_mismatch"
    );
    assert_eq!(output["partition_decisions"][1]["outcome"], "applied");
    assert_eq!(
        runtime.get_task(&first.id).unwrap().context_files,
        vec!["file:src/first.rs"]
    );
    assert_eq!(
        runtime.get_task(&new_task.id).unwrap().context_files,
        vec!["file:src/new.rs"]
    );
}

#[test]
fn edit_between_validation_and_locked_write_is_stale_and_preserved() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "src/new.rs");
    let raced = seed_task(&runtime, "raced", TaskStatus::Backlog, &[], &[]);
    let sibling = seed_task(&runtime, "sibling", TaskStatus::Backlog, &[], &[]);
    let task_ids = vec![raced.id.clone(), sibling.id.clone()];
    let snapshot = prepared(&runtime, &repo_root, &task_ids);
    inject_concurrent_edit_before_locked_apply();

    let output = apply(
        &runtime,
        "apply_task_pilot_results",
        &json!({
            "prepared": snapshot,
            "results": [partition_result(
                0,
                &task_ids,
                vec![
                    selector_assessment(&raced, vec!["file:src/new.rs"]),
                    selector_assessment(&sibling, vec!["file:src/new.rs"]),
                ],
            )],
            "workspace_path": repo_root,
        }),
    )
    .expect("concurrent edit is a structured task outcome");

    assert_eq!(output["task_outcomes"][0]["outcome"], "stale");
    assert_eq!(output["task_outcomes"][0]["reason"], "tags_changed");
    assert_eq!(
        runtime.get_task(&raced.id).unwrap().tags,
        vec!["concurrent-edit"]
    );
    assert!(
        runtime
            .get_task(&raced.id)
            .unwrap()
            .context_files
            .is_empty()
    );
    assert_eq!(
        runtime.get_task(&sibling.id).unwrap().context_files,
        vec!["file:src/new.rs"]
    );
}

#[test]
fn all_stale_partition_diagnostic_identifies_zero_apply() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "src/new.rs");
    let task = seed_task(&runtime, "status changed", TaskStatus::Backlog, &[], &[]);
    let snapshot = prepared(&runtime, &repo_root, std::slice::from_ref(&task.id));
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::InProgress),
                ..TaskUpdateParams::default()
            },
        )
        .expect("operator advances task status");

    let output = apply(
        &runtime,
        "apply_task_pilot_results",
        &json!({
            "prepared": snapshot,
            "results": [partition_result(
                0,
                std::slice::from_ref(&task.id),
                vec![selector_assessment(&task, vec!["file:src/new.rs"])],
            )],
            "workspace_path": repo_root,
        }),
    )
    .expect("stale outcome is retained as a durable failed decision");

    let error = output["error"]
        .as_str()
        .expect("all-stale apply carries a diagnostic");
    assert_eq!(output["status"], "failed");
    assert_eq!(
        output["skipped_stale_partitions"].as_array().unwrap().len(),
        1
    );
    assert!(error.contains("1 unresolved"));
    assert!(error.contains("status_changed"));
    assert!(error.contains(&task.id));
    assert!(
        !error.contains("valid partitions were applied"),
        "zero-apply diagnostic must not claim that valid partitions were applied: {error}"
    );
    assert_eq!(
        runtime.get_task(&task.id).unwrap().status,
        TaskStatus::InProgress
    );
    assert!(runtime.get_task(&task.id).unwrap().context_files.is_empty());
}

#[test]
fn malformed_partition_does_not_discard_independent_valid_partition() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "src/new.rs");
    let malformed = seed_task(&runtime, "malformed", TaskStatus::Backlog, &[], &[]);
    let valid = seed_task(&runtime, "valid", TaskStatus::Backlog, &[], &[]);
    let task_ids = vec![malformed.id.clone(), valid.id.clone()];
    let snapshot = prepared_with_partition_size(&runtime, &repo_root, &task_ids, 1);

    let output = apply(
        &runtime,
        "apply_task_pilot_results",
        &json!({
            "prepared": snapshot,
            "results": [
                {"partition_index": 0, "task_ids": [malformed.id], "tasks": "invalid"},
                partition_result(
                    1,
                    std::slice::from_ref(&valid.id),
                    vec![selector_assessment(&valid, vec!["file:src/new.rs"])],
                ),
            ],
            "workspace_path": repo_root,
        }),
    )
    .expect("malformed partition remains a durable failed decision");

    assert_eq!(output["status"], "failed");
    assert_eq!(output["partition_decisions"][0]["outcome"], "failed");
    assert_eq!(output["partition_decisions"][1]["outcome"], "applied");
    assert!(
        runtime
            .get_task(&malformed.id)
            .unwrap()
            .context_files
            .is_empty()
    );
    assert_eq!(
        runtime.get_task(&valid.id).unwrap().context_files,
        vec!["file:src/new.rs"]
    );
}

#[test]
fn task_status_change_and_deletion_are_explicit_stale_outcomes() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "src/new.rs");
    let changed = seed_task(&runtime, "status changed", TaskStatus::Backlog, &[], &[]);
    let deleted = seed_task(&runtime, "deleted", TaskStatus::Backlog, &[], &[]);
    let task_ids = vec![changed.id.clone(), deleted.id.clone()];
    let snapshot = prepared_with_partition_size(&runtime, &repo_root, &task_ids, 1);
    runtime
        .update_task(
            &changed.id,
            TaskUpdateParams {
                status: Some(TaskStatus::InProgress),
                ..TaskUpdateParams::default()
            },
        )
        .expect("operator advances task status");
    runtime
        .delete_task(&deleted.id)
        .expect("operator deletes task");

    let output = apply(
        &runtime,
        "apply_task_pilot_results",
        &json!({
            "prepared": snapshot,
            "results": [
                partition_result(
                    0,
                    std::slice::from_ref(&changed.id),
                    vec![selector_assessment(&changed, vec!["file:src/new.rs"])],
                ),
                partition_result(
                    1,
                    std::slice::from_ref(&deleted.id),
                    vec![selector_assessment(&deleted, vec!["file:src/new.rs"])],
                ),
            ],
            "workspace_path": repo_root,
        }),
    )
    .expect("stale outcomes are structured instead of overwriting live state");

    assert_eq!(
        output["partition_decisions"][0]["stale_tasks"][0]["reason"],
        "status_changed"
    );
    assert_eq!(
        output["partition_decisions"][1]["stale_tasks"][0]["reason"],
        "task_deleted"
    );
    assert_eq!(
        runtime.get_task(&changed.id).unwrap().status,
        TaskStatus::InProgress
    );
}

#[test]
fn empty_context_requires_verified_no_diff_or_host_operational_evidence() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    let task = seed_task(&runtime, "no-diff", TaskStatus::Backlog, &[], &[]);
    let task_ids = vec![task.id.clone()];
    let invalid_prepared = prepared(&runtime, &repo_root, &task_ids);
    let invalid = partition_result(
        0,
        &task_ids,
        vec![json!({
            "task_id": task.id,
            "context_files_before": [],
            "context_files_after": [],
            "disposition": "selectors",
        })],
    );

    let invalid_output = apply(
        &runtime,
        "apply_task_pilot_results",
        &json!({
            "prepared": invalid_prepared,
            "results": [invalid],
            "workspace_path": repo_root,
        }),
    )
    .expect("invalid partition is retained as a failed decision");
    assert_eq!(invalid_output["status"], "failed");
    assert!(
        invalid_output["partition_decisions"][0]["error"]
            .as_str()
            .unwrap()
            .contains("verified_no_diff")
    );

    let valid_prepared = prepared(&runtime, &repo_root, &task_ids);
    let valid = partition_result(
        0,
        &task_ids,
        vec![json!({
            "task_id": task.id,
            "context_files_before": [],
            "context_files_after": [],
            "disposition": "verified_no_diff",
            "evidence": "The requested behavior already exists on the target branch.",
            "recommended_crew": "luna",
            "recommended_complexity": "low",
            "assessment_rationale": "The current source already contains the behavior.",
            "confidence": "high",
            "evidence_gaps": [],
            "validation_approach": "Inspect the current source and history.",
            "reassessment_triggers": ["the source revision changes"],
            "blocked_by": [],
            "duplicate_of": null,
            "already_landed": null,
            "adr_conflicts": [],
            "utility_warnings": [],
            "surface_warnings": [],
        })],
    );
    let output = apply(
        &runtime,
        "apply_task_pilot_results",
        &json!({
            "prepared": valid_prepared,
            "results": [valid],
            "workspace_path": repo_root,
        }),
    )
    .expect("verified no-diff result is valid");
    assert_eq!(output["tasks"][0]["applied"], true);
}

#[test]
fn out_of_workspace_selector_is_rejected_before_mutation() {
    let (root, runtime, repo_root) = runtime_with_workspace_layout();
    let outside = root.path().join("outside.rs");
    std::fs::write(&outside, "outside").expect("write outside fixture");
    let task = seed_task(&runtime, "escape", TaskStatus::Backlog, &[], &[]);
    let task_ids = vec![task.id.clone()];
    let prepared = prepared(&runtime, &repo_root, &task_ids);
    let outside_selector = format!("file:{}", outside.display());
    let result = partition_result(
        0,
        &task_ids,
        vec![json!({
            "task_id": task.id,
            "context_files_before": [],
            "context_files_after": [outside_selector],
            "disposition": "selectors",
        })],
    );

    let output = apply(
        &runtime,
        "apply_task_pilot_results",
        &json!({
            "prepared": prepared,
            "results": [result],
            "workspace_path": repo_root,
        }),
    )
    .expect("out-of-workspace selector is retained as a failed decision");
    assert_eq!(output["status"], "failed");
    assert!(
        output["partition_decisions"][0]["error"]
            .as_str()
            .unwrap()
            .contains("inside workspace")
    );
    assert!(runtime.get_task(&task.id).unwrap().context_files.is_empty());
}

/// [ORB-12228] Reproduces F2026-09-137: the pilot swept every module in a
/// crate instead of deriving targets from the references to the symbol, and
/// apply accepted the oversized list silently. Apply must keep applying it —
/// a genuinely wide repair has to stay proposable — while attaching a finding
/// that names the proposed count and the budget it exceeded.
#[test]
fn over_attached_proposal_is_reported_and_still_applied() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    let selectors = workspace_selectors(&repo_root, 12);
    let task = seed_task(&runtime, "swept-crate", TaskStatus::Backlog, &[], &[]);
    let task_ids = vec![task.id.clone()];
    let prepared_snapshot = prepared(&runtime, &repo_root, &task_ids);
    let mut assessment = selector_assessment_with_complexity(
        &task,
        selectors.iter().map(String::as_str).collect(),
        "unassessed",
    );
    assessment["evidence_gaps"] = json!(["the caller set for the removed symbol is unverified"]);
    let result = partition_result(0, &task_ids, vec![assessment]);

    let output = apply(
        &runtime,
        "apply_task_pilot_results",
        &json!({
            "prepared": prepared_snapshot,
            "results": [result],
            "workspace_path": repo_root,
        }),
    )
    .expect("an over-attached proposal is reported, not refused");

    assert_eq!(output["status"], "succeeded");
    assert_eq!(output["tasks"][0]["context_files_after"], json!(selectors));
    assert_eq!(
        runtime
            .get_task(&task.id)
            .expect("task after apply")
            .context_files,
        selectors
    );
    let findings = output["tasks"][0]["context_attachment_warnings"]
        .as_array()
        .expect("context_attachment_warnings array");
    assert_eq!(findings.len(), 1);
    let finding = findings[0].as_str().expect("finding string");
    assert!(
        finding.contains("12 selectors") && finding.contains("10-selector budget"),
        "finding must name the proposed count and the cap: {finding}"
    );
    assert!(
        finding.contains("unassessed"),
        "finding must name the tier whose budget was exceeded: {finding}"
    );
}

/// [ORB-12228] The budget is per recommended complexity, and staying inside it
/// attaches nothing: the same twelve selectors are ordinary for a `medium`
/// repair, and a proposal exactly at the strictest budget is still inside it.
#[test]
fn proposal_within_its_complexity_budget_attaches_no_finding() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    let selectors = workspace_selectors(&repo_root, 12);
    let medium = seed_task(&runtime, "wide-medium", TaskStatus::Backlog, &[], &[]);
    let unassessed = seed_task(&runtime, "at-budget", TaskStatus::Backlog, &[], &[]);
    let task_ids = vec![medium.id.clone(), unassessed.id.clone()];
    let prepared_snapshot = prepared(&runtime, &repo_root, &task_ids);
    let mut at_budget = selector_assessment_with_complexity(
        &unassessed,
        selectors[..10].iter().map(String::as_str).collect(),
        "unassessed",
    );
    at_budget["evidence_gaps"] = json!(["the reproduction for the failure is not yet known"]);
    let result = partition_result(
        0,
        &task_ids,
        vec![
            selector_assessment_with_complexity(
                &medium,
                selectors.iter().map(String::as_str).collect(),
                "medium",
            ),
            at_budget,
        ],
    );

    let output = apply(
        &runtime,
        "apply_task_pilot_results",
        &json!({
            "prepared": prepared_snapshot,
            "results": [result],
            "workspace_path": repo_root,
        }),
    )
    .expect("apply proposals inside their budgets");

    assert_eq!(output["status"], "succeeded");
    let applied = |task_id: &str| -> Value {
        output["tasks"]
            .as_array()
            .expect("applied assessments")
            .iter()
            .find(|assessment| assessment["task_id"] == task_id)
            .expect("assessment for task")
            .clone()
    };
    for task_id in [&medium.id, &unassessed.id] {
        assert_eq!(
            applied(task_id)["context_attachment_warnings"],
            json!([]),
            "task {task_id} must carry no over-attachment finding"
        );
    }
    assert_eq!(
        applied(&medium.id)["context_files_after"],
        json!(selectors),
        "the wider medium proposal still applies in full"
    );
    assert_eq!(
        applied(&unassessed.id)["context_files_after"],
        json!(selectors[..10])
    );
}
