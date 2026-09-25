use super::super::*;
use orbit_common::test_fixtures::{TEST_CLAUDE_MODEL, TEST_CODEX_MODEL};

use super::super::types::CURRENT_SCHEMA_VERSION;
use super::{test_job_run, test_task, test_task_no_attrib, write_snapshot_fixtures};
use crate::{AuditToolCallCountsByRole, AuditToolCallCountsBySurfaceAndRole, AuditTopToolCall};
use chrono::Utc;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::JobRunState;
use std::fs;

#[test]
fn summary_exposes_pr_comments() {
    let temp = tempfile::tempdir().expect("create tempdir");
    fs::create_dir_all(temp.path()).expect("create scoreboard dir");
    fs::write(
        temp.path().join("pr.json"),
        r#"{"pr-review-comments":{"gpt-reviewer":1}}"#,
    )
    .expect("write pr scoreboard");

    let summary = generate_summary(temp.path(), &[]).expect("generate summary");

    assert_eq!(summary.schema_version, CURRENT_SCHEMA_VERSION);
    let reviewer = summary.agents.get("codex").expect("reviewer summary");
    assert_eq!(reviewer.pr.review_comments, 1);
}

#[test]
fn summary_counts_tasks_created_and_planned_across_all_statuses() {
    let temp = tempfile::tempdir().expect("create tempdir");

    // Mix of statuses including ones excluded from `tasks_completed`.
    let tasks = vec![
        test_task("T1", TaskStatus::Done, TEST_CLAUDE_MODEL, TEST_CLAUDE_MODEL),
        test_task(
            "T2",
            TaskStatus::Backlog,
            TEST_CLAUDE_MODEL,
            TEST_CODEX_MODEL,
        ),
        test_task(
            "T3",
            TaskStatus::Rejected,
            TEST_CLAUDE_MODEL,
            TEST_CLAUDE_MODEL,
        ),
        test_task(
            "T4",
            TaskStatus::Someday,
            TEST_CODEX_MODEL,
            TEST_CODEX_MODEL,
        ),
        test_task_no_attrib("T5", TaskStatus::Done),
    ];

    let summary = generate_summary(temp.path(), &tasks).expect("generate summary");

    let claude = summary.agents.get("claude").expect("claude summary");
    // Three tasks were created by claude (Done, Backlog, Rejected).
    assert_eq!(claude.tasks_created, 3);
    // Two were planned by claude (Done, Rejected).
    assert_eq!(claude.tasks_planned, 2);
    // Only Done counts toward Completed (no `task.model` here, so it
    // attributes via `implemented_by`-equivalent — but we left model None;
    // verify the attribution still ignores Backlog/Rejected/Someday).
    // T1 (Done) has implemented_by=None and model=None, so it does not
    // attribute to Completed.
    assert_eq!(claude.tasks_completed, 0);

    let codex = summary.agents.get("codex").expect("codex summary");
    assert_eq!(codex.tasks_created, 1); // T4
    assert_eq!(codex.tasks_planned, 2); // T2, T4

    // T5 has no created_by/planned_by — must not crash and must not
    // create a phantom agent bucket.
    assert!(!summary.agents.contains_key(""));
}

#[test]
fn summary_aggregates_workflows_run_for_successful_runs() {
    let temp = tempfile::tempdir().expect("create tempdir");

    let now = Utc::now();
    let runs = vec![
        test_job_run("r1", "task_local_pipeline", JobRunState::Success, now),
        test_job_run("r2", "task_local_pipeline", JobRunState::Success, now),
        test_job_run("r3", "task_local_pipeline", JobRunState::Failed, now),
        test_job_run("r4", "task_auto_pipeline", JobRunState::Success, now),
        test_job_run("r5", "task_pr_pipeline", JobRunState::Cancelled, now),
    ];

    let summary = generate_summary_with_inputs(
        temp.path(),
        &[],
        &ScoreboardInputs {
            job_runs: &runs,
            ..ScoreboardInputs::default()
        },
    )
    .expect("generate summary");

    // Sorted descending by count, then job_id ascending.
    assert_eq!(
        summary.workflows_run,
        vec![
            WorkflowRunCount {
                job_id: "task_local_pipeline".to_string(),
                count: 2,
            },
            WorkflowRunCount {
                job_id: "task_auto_pipeline".to_string(),
                count: 1,
            },
        ]
    );
}

#[test]
fn recent_7d_filters_tasks_workflows_and_surface_calls_by_window() {
    let temp = tempfile::tempdir().expect("create tempdir");

    let now = Utc::now();
    let inside = now - chrono::Duration::days(3);
    let outside = now - chrono::Duration::days(30);

    // Two created in-window, one outside.
    let mut t_inside = test_task(
        "T-in",
        TaskStatus::Done,
        TEST_CLAUDE_MODEL,
        TEST_CLAUDE_MODEL,
    );
    t_inside.created_at = inside;
    t_inside.updated_at = inside;

    let mut t_inside2 = test_task(
        "T-in2",
        TaskStatus::Backlog,
        TEST_CODEX_MODEL,
        TEST_CODEX_MODEL,
    );
    t_inside2.created_at = inside;

    let mut t_outside = test_task(
        "T-out",
        TaskStatus::Done,
        TEST_CLAUDE_MODEL,
        TEST_CLAUDE_MODEL,
    );
    t_outside.created_at = outside;
    t_outside.updated_at = outside; // legacy: no history transition
    // No history on t_outside — task_done_at falls back to updated_at.

    let tasks = vec![t_inside, t_inside2, t_outside];

    let surface_recent = vec![AuditToolCallCountsBySurfaceAndRole {
        surface: "graph".to_string(),
        role: TEST_CLAUDE_MODEL.to_string(),
        total: 12,
        failed: 0,
    }];

    let runs = vec![
        test_job_run(
            "r-recent",
            "task_local_pipeline",
            JobRunState::Success,
            inside,
        ),
        test_job_run(
            "r-old",
            "task_local_pipeline",
            JobRunState::Success,
            outside,
        ),
    ];

    let summary = generate_summary_with_inputs(
        temp.path(),
        &tasks,
        &ScoreboardInputs {
            audit_tool_calls_by_surface_recent: &surface_recent,
            job_runs: &runs,
            now: Some(now),
            ..ScoreboardInputs::default()
        },
    )
    .expect("generate summary");

    let recent = summary
        .recent_7d
        .expect("recent_7d populated when now is set");
    // Two tasks created in window (T-in, T-in2). T-out is older.
    assert_eq!(recent.tasks_created, 2);
    // One task transitioned to Done in window (T-in). T-out's
    // updated_at is older than the window.
    assert_eq!(recent.tasks_completed, 1);
    // Surface row total flows through.
    assert_eq!(recent.tool_calls_by_surface.get("graph").copied(), Some(12));
    // Only the recent run counts.
    assert_eq!(recent.workflows_run, 1);
}

#[test]
fn summary_passes_top_tools_through_unchanged() {
    let temp = tempfile::tempdir().expect("create tempdir");

    let rows = vec![
        AuditTopToolCall {
            role: TEST_CODEX_MODEL.to_string(),
            tool_name: "orbit.task.show".to_string(),
            total: 355,
        },
        AuditTopToolCall {
            role: TEST_CLAUDE_MODEL.to_string(),
            tool_name: "orbit.search".to_string(),
            total: 45,
        },
    ];

    let summary = generate_summary_with_inputs(
        temp.path(),
        &[],
        &ScoreboardInputs {
            top_tool_calls: &rows,
            ..ScoreboardInputs::default()
        },
    )
    .expect("generate summary");

    assert_eq!(
        summary.top_tools,
        vec![
            TopToolCall {
                role: TEST_CODEX_MODEL.to_string(),
                tool_name: "orbit.task.show".to_string(),
                count: 355,
            },
            TopToolCall {
                role: TEST_CLAUDE_MODEL.to_string(),
                tool_name: "orbit.search".to_string(),
                count: 45,
            },
        ]
    );
}

#[test]
fn recent_7d_absent_when_now_not_provided() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let summary = generate_summary(temp.path(), &[]).expect("generate summary");
    assert!(summary.recent_7d.is_none());
}

// ----- ORB-00337: window-aware summary tests -----

#[test]
fn snapshot_sourced_fields_zero_under_non_all_window() {
    // ORB-00337 AC#4 — snapshot reads (pr.json, tokens.json) have no
    // per-event timestamp, so anything other than `ScoreboardWindow::All`
    // must zero out those fields.
    let temp = tempfile::tempdir().expect("create tempdir");
    write_snapshot_fixtures(temp.path());

    let summary_all = generate_summary_with_inputs(
        temp.path(),
        &[],
        &ScoreboardInputs {
            window: ScoreboardWindow::All,
            now: Some(Utc::now()),
            ..ScoreboardInputs::default()
        },
    )
    .expect("generate summary all");
    let codex_all = summary_all.agents.get("codex").expect("codex (all)");
    assert_eq!(codex_all.pr.review_comments, 4, "lifetime preserves pr");
    assert_eq!(codex_all.pr.merged_clean, 2);
    let claude_all = summary_all.agents.get("claude").expect("claude (all)");
    assert_eq!(claude_all.tokens.total, 1000);
    assert_eq!(claude_all.tokens.output, 250);
    assert_eq!(summary_all.window, "all");
    assert!(summary_all.window_since.is_none());

    let summary_day = generate_summary_with_inputs(
        temp.path(),
        &[],
        &ScoreboardInputs {
            window: ScoreboardWindow::Day,
            now: Some(Utc::now()),
            ..ScoreboardInputs::default()
        },
    )
    .expect("generate summary day");
    let codex_day = summary_day.agents.get("codex").expect("codex (day)");
    assert_eq!(codex_day.pr.review_comments, 0, "windowed must zero pr");
    assert_eq!(codex_day.pr.merged_clean, 0);
    let claude_day = summary_day.agents.get("claude").expect("claude (day)");
    assert_eq!(claude_day.tokens.total, 0, "windowed must zero tokens");
    assert_eq!(claude_day.tokens.output, 0);
    assert_eq!(summary_day.window, "24h");
    assert!(summary_day.window_since.is_some());
}

#[test]
fn audit_inputs_flow_through_under_windowed_call() {
    // ORB-00337 AC#5 (scoreboard_summary layer) — the function honors the
    // caller-supplied `audit_tool_calls` slice unchanged under `window =
    // Day`. The caller (`orbit-core::OrbitRuntime`) is responsible for
    // re-querying the audit store with the matching cutoff; the
    // end-to-end runtime path is exercised separately.
    let temp = tempfile::tempdir().expect("create tempdir");

    let audit_windowed = vec![AuditToolCallCountsByRole {
        role: "codex / gpt-5".to_string(),
        total: 2,
        failed: 0,
    }];
    let surface_windowed = vec![AuditToolCallCountsBySurfaceAndRole {
        surface: "graph".to_string(),
        role: "codex / gpt-5".to_string(),
        total: 2,
        failed: 0,
    }];

    let summary = generate_summary_with_inputs(
        temp.path(),
        &[],
        &ScoreboardInputs {
            audit_tool_calls: &audit_windowed,
            audit_tool_calls_by_surface: &surface_windowed,
            window: ScoreboardWindow::Day,
            now: Some(Utc::now()),
            ..ScoreboardInputs::default()
        },
    )
    .expect("generate summary day");

    let codex = summary.agents.get("codex").expect("codex summary");
    assert_eq!(codex.tool_calls, 2, "windowed audit slice flows through");
    assert_eq!(codex.tool_calls_by_surface.get("graph").copied(), Some(2));
}

#[test]
fn windowed_tasks_filter_by_created_at_and_done_at() {
    // ORB-00337 AC#5 (tasks-filter spirit) — under `window = Day` only
    // tasks whose `created_at` (for created/planned) or `task_done_at`
    // (for completed) falls within the last 24h are counted.
    let temp = tempfile::tempdir().expect("create tempdir");

    let now = Utc::now();
    let inside = now - chrono::Duration::hours(1);
    let outside = now - chrono::Duration::days(7);

    let mut t_in_created = test_task(
        "T-in-c",
        TaskStatus::Backlog,
        TEST_CLAUDE_MODEL,
        TEST_CLAUDE_MODEL,
    );
    t_in_created.created_at = inside;

    let mut t_in_done = test_task(
        "T-in-d",
        TaskStatus::Done,
        TEST_CODEX_MODEL,
        TEST_CODEX_MODEL,
    );
    t_in_done.created_at = outside; // not in created/planned window
    t_in_done.updated_at = inside; // task_done_at == updated_at, in window
    t_in_done.implemented_by = Some(TEST_CODEX_MODEL.to_string());

    let mut t_out = test_task(
        "T-out",
        TaskStatus::Done,
        TEST_CLAUDE_MODEL,
        TEST_CLAUDE_MODEL,
    );
    t_out.created_at = outside;
    t_out.updated_at = outside;
    t_out.implemented_by = Some(TEST_CLAUDE_MODEL.to_string());

    let tasks = vec![t_in_created, t_in_done, t_out];

    let summary_all = generate_summary_with_inputs(
        temp.path(),
        &tasks,
        &ScoreboardInputs {
            window: ScoreboardWindow::All,
            now: Some(now),
            ..ScoreboardInputs::default()
        },
    )
    .expect("generate summary all");
    let claude_all = summary_all.agents.get("claude").expect("claude (all)");
    assert_eq!(
        claude_all.tasks_created, 2,
        "lifetime counts both claude tasks"
    );
    assert_eq!(claude_all.tasks_completed, 1, "lifetime counts old done");

    let summary_day = generate_summary_with_inputs(
        temp.path(),
        &tasks,
        &ScoreboardInputs {
            window: ScoreboardWindow::Day,
            now: Some(now),
            ..ScoreboardInputs::default()
        },
    )
    .expect("generate summary day");
    let claude_day = summary_day.agents.get("claude").expect("claude (day)");
    assert_eq!(
        claude_day.tasks_created, 1,
        "windowed drops the old created task"
    );
    assert_eq!(
        claude_day.tasks_completed, 0,
        "windowed drops the old done (updated_at outside window)"
    );
    let codex_day = summary_day.agents.get("codex").expect("codex (day)");
    assert_eq!(
        codex_day.tasks_completed, 1,
        "old-created-but-recent-updated task counts as completed in window"
    );
    assert_eq!(
        codex_day.tasks_created, 0,
        "but does not re-count as created (created_at is old)"
    );
}
