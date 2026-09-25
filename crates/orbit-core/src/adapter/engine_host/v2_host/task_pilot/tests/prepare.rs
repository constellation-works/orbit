//! Task-pilot discovery, readiness and explicit-mode preparation.

use orbit_types::task::{TaskComplexity, TaskStatus};
use serde_json::{Value, json};

use super::super::{apply, member_ready, prepare};
use super::apply::{
    partition_result, prepared, seed_active_preparation, seed_task,
    selector_assessment_with_complexity,
};
use crate::adapter::engine_host::v2_host::test_support::{
    runtime_with_non_git_workspace_layout as runtime_with_workspace_layout, write_workspace_file,
};
use crate::application::task::TaskUpdateParams;

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
fn automatic_discovery_excludes_no_diff_tasks_regardless_of_mint_provenance() {
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
    assert_eq!(output["task_count"], 7);
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
        2
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
    for task in [no_diff_needed, no_diff_expected, no_diff_auto_task] {
        assert!(
            excluded
                .iter()
                .any(|entry| { entry["task_id"] == task.id && entry["reason"] == "no_diff_task" })
        );
    }
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
