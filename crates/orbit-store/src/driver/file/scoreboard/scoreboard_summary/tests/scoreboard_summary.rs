// Migrated from file/scoreboard/scoreboard_summary.rs per ORB-00231
use super::super::*;
use orbit_common::test_fixtures::{TEST_CLAUDE_MODEL, TEST_CODEX_MODEL, TEST_GROK_MODEL};

#[cfg(unix)]
use std::os::unix::fs::symlink;

#[test]
fn summary_overlays_audit_tool_call_counts_by_normalized_model() {
    let temp = tempfile::tempdir().expect("create tempdir");

    let summary = generate_summary_with_audit_tool_calls(
        temp.path(),
        &[],
        &[
            AuditToolCallCountsByRole {
                role: "codex / gpt-5".to_string(),
                total: 2,
                failed: 1,
            },
            AuditToolCallCountsByRole {
                role: "gpt-5".to_string(),
                total: 1,
                failed: 1,
            },
        ],
    )
    .expect("generate summary");

    let codex = summary.agents.get("codex").expect("codex summary");
    assert_eq!(codex.tool_calls, 3);
    assert_eq!(codex.failed_tool_calls, 2);
}

#[test]
fn summary_includes_zero_rows_for_known_families() {
    let temp = tempfile::tempdir().expect("create tempdir");

    let summary = generate_summary(temp.path(), &[]).expect("generate summary");

    let grok = summary.agents.get("grok").expect("grok summary");
    assert_eq!(grok.tasks_completed, 0);
}

#[test]
fn summary_agent_keys_are_families_not_models() {
    let temp = tempfile::tempdir().expect("create tempdir");
    fs::create_dir_all(temp.path()).expect("create scoreboard dir");
    fs::write(
        temp.path().join("tokens.json"),
        r#"{
              "agents": [
                { "agent": "codex", "model": "gpt-5.5", "total_tokens": 1 },
                { "agent": "claude", "model": "claude-opus-4-7", "total_tokens": 1 },
                { "agent": "grok", "model": "grok-4", "total_tokens": 1 }
              ]
            }"#,
    )
    .expect("write tokens scoreboard");

    let summary = generate_summary(temp.path(), &[]).expect("generate summary");
    for forbidden in [TEST_GROK_MODEL, TEST_CLAUDE_MODEL, TEST_CODEX_MODEL] {
        assert!(
            !summary.agents.contains_key(forbidden),
            "model key leaked into summary agents: {forbidden}"
        );
    }
    for family in ["codex", "claude", "gemini", "grok"] {
        assert!(summary.agents.contains_key(family));
    }
}

#[test]
fn audit_tool_calls_do_not_double_count_token_scoreboard_tool_calls() {
    let temp = tempfile::tempdir().expect("create tempdir");
    fs::create_dir_all(temp.path()).expect("create scoreboard dir");
    fs::write(
        temp.path().join("tokens.json"),
        r#"{
              "agents": [
                {
                  "agent": "codex",
                  "model": "gpt-5",
                  "total_tokens": 10,
                  "total_output_tokens": 4,
                  "total_tool_calls": 5
                }
              ]
            }"#,
    )
    .expect("write tokens scoreboard");

    let summary = generate_summary_with_audit_tool_calls(
        temp.path(),
        &[],
        &[AuditToolCallCountsByRole {
            role: "gpt-5".to_string(),
            total: 3,
            failed: 2,
        }],
    )
    .expect("generate summary");

    let codex = summary.agents.get("codex").expect("codex summary");
    assert_eq!(codex.tokens.total, 10);
    assert_eq!(codex.tokens.output, 4);
    assert_eq!(codex.tool_calls, 5);
    assert_eq!(codex.failed_tool_calls, 2);
}

#[test]
fn audit_tool_calls_win_when_larger_than_token_scoreboard_tool_calls() {
    let temp = tempfile::tempdir().expect("create tempdir");
    fs::create_dir_all(temp.path()).expect("create scoreboard dir");
    fs::write(
        temp.path().join("tokens.json"),
        r#"{
              "agents": [
                {
                  "agent": "codex",
                  "model": "gpt-5",
                  "total_tokens": 10,
                  "total_output_tokens": 4,
                  "total_tool_calls": 2
                }
              ]
            }"#,
    )
    .expect("write tokens scoreboard");

    let summary = generate_summary_with_audit_tool_calls(
        temp.path(),
        &[],
        &[AuditToolCallCountsByRole {
            role: "gpt-5".to_string(),
            total: 7,
            failed: 3,
        }],
    )
    .expect("generate summary");

    let codex = summary.agents.get("codex").expect("codex summary");
    assert_eq!(codex.tokens.total, 10);
    assert_eq!(codex.tokens.output, 4);
    assert_eq!(codex.tool_calls, 7);
    assert_eq!(codex.failed_tool_calls, 3);
}

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

#[cfg(unix)]
#[test]
fn summary_rejects_a_symlinked_pr_scoreboard() {
    let root = tempfile::tempdir().expect("create scoreboard dir");
    let outside = tempfile::tempdir().expect("create outside dir");
    let outside_scoreboard = outside.path().join(PR_SCOREBOARD_FILENAME);
    fs::write(
        &outside_scoreboard,
        r#"{"pr-review-comments":{"gpt-reviewer":1}}"#,
    )
    .expect("write outside scoreboard");
    symlink(
        &outside_scoreboard,
        root.path().join(PR_SCOREBOARD_FILENAME),
    )
    .expect("create scoreboard symlink");

    let error = generate_summary(root.path(), &[]).expect_err("reject symlinked scoreboard");

    assert!(
        error
            .to_string()
            .contains("scoreboard file must not be a symlink"),
        "unexpected error: {error}"
    );
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
fn summary_overlays_per_surface_tool_call_counts() {
    let temp = tempfile::tempdir().expect("create tempdir");

    let surface_rows = vec![
        AuditToolCallCountsBySurfaceAndRole {
            surface: "graph".to_string(),
            role: TEST_CLAUDE_MODEL.to_string(),
            total: 56,
            failed: 2,
        },
        AuditToolCallCountsBySurfaceAndRole {
            surface: "graph".to_string(),
            role: TEST_CODEX_MODEL.to_string(),
            total: 697,
            failed: 5,
        },
        AuditToolCallCountsBySurfaceAndRole {
            surface: "task".to_string(),
            role: TEST_CODEX_MODEL.to_string(),
            total: 410,
            failed: 1,
        },
    ];

    let summary = generate_summary_with_inputs(
        temp.path(),
        &[],
        &ScoreboardInputs {
            audit_tool_calls_by_surface: &surface_rows,
            ..ScoreboardInputs::default()
        },
    )
    .expect("generate summary");

    let claude = summary.agents.get("claude").expect("claude summary");
    assert_eq!(claude.tool_calls_by_surface.get("graph").copied(), Some(56));
    assert_eq!(claude.tool_calls_by_surface.get("task"), None);

    let codex = summary.agents.get("codex").expect("codex summary");
    assert_eq!(codex.tool_calls_by_surface.get("graph").copied(), Some(697));
    assert_eq!(codex.tool_calls_by_surface.get("task").copied(), Some(410));
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

fn test_task(
    id: &str,
    status: TaskStatus,
    created_by: &str,
    planned_by: &str,
) -> orbit_types::task::Task {
    let mut task = test_task_no_attrib(id, status);
    task.created_by = Some(created_by.to_string());
    task.planned_by = Some(planned_by.to_string());
    task
}

fn test_task_no_attrib(id: &str, status: TaskStatus) -> orbit_types::task::Task {
    use orbit_types::task::{Task, TaskPriority, TaskType};
    Task {
        id: id.to_string(),
        title: id.to_string(),
        description: String::new(),
        acceptance_criteria: Vec::new(),
        tags: Vec::new(),
        required_tools: Vec::new(),
        plan: String::new(),
        execution_summary: String::new(),
        context_files: Vec::new(),
        created_by: None,
        planned_by: None,
        implemented_by: None,
        status,
        priority: TaskPriority::Medium,
        complexity: None,
        task_type: TaskType::Chore,
        pr_status: None,
        external_refs: Vec::new(),
        relations: Vec::new(),
        job_run_id: None,
        crew: None,
        orchestrator: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn test_job_run(
    run_id: &str,
    job_id: &str,
    state: JobRunState,
    finished_at: chrono::DateTime<Utc>,
) -> JobRun {
    JobRun {
        run_id: run_id.to_string(),
        job_id: job_id.to_string(),
        attempt: 1,
        state,
        scheduled_at: finished_at,
        started_at: Some(finished_at),
        finished_at: Some(finished_at),
        duration_ms: Some(0),
        created_at: finished_at,
        pid: None,
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
    }
}

#[test]
fn summary_exposes_friction_reported_counts_from_records() {
    // Deterministic test per ORB-00143: seeds friction records for >=2 families
    // and asserts the generated scoreboard exposes nonzero `friction.reported`
    // (and zero for families with none). Uses the inputs path so it does not
    // depend on disk state.
    let temp = tempfile::tempdir().expect("create tempdir");

    let friction_reported = vec![
        crate::friction_store::FrictionReportedCount {
            model: "codex".to_string(),
            count: 1,
        },
        crate::friction_store::FrictionReportedCount {
            model: "claude-3-opus".to_string(),
            count: 1,
        },
    ];

    let summary = generate_summary_with_inputs(
        temp.path(),
        &[],
        &ScoreboardInputs {
            friction_reported: &friction_reported,
            ..ScoreboardInputs::default()
        },
    )
    .expect("generate summary with seeded frictions");

    let codex = summary.agents.get("codex").expect("codex summary");
    assert_eq!(
        codex.friction.reported, 1,
        "codex should report 1 friction record"
    );

    let claude = summary.agents.get("claude").expect("claude summary");
    assert_eq!(
        claude.friction.reported, 1,
        "claude (from claude-3-opus) should report 1"
    );

    let gemini = summary.agents.get("gemini").expect("gemini summary");
    assert_eq!(
        gemini.friction.reported, 0,
        "gemini with no records must expose 0, not fall back"
    );

    let grok = summary.agents.get("grok").expect("grok summary");
    assert_eq!(grok.friction.reported, 0);
}

// ----- ORB-00337: window-aware summary tests -----

/// Helper: write `pr.json` + `tokens.json` snapshots into a tempdir.
fn write_snapshot_fixtures(dir: &std::path::Path) {
    fs::create_dir_all(dir).expect("create scoreboard dir");
    fs::write(
        dir.join("pr.json"),
        r#"{
              "pr-review-comments": { "codex": 4 },
              "pr-count-without-revision": { "codex": 2 },
              "pr-count-with-revision": { "claude": 1 }
            }"#,
    )
    .expect("write pr.json");
    fs::write(
        dir.join("tokens.json"),
        r#"{
              "agents": [
                {
                  "agent": "claude",
                  "model": "claude-opus-4-7",
                  "total_tokens": 1000,
                  "total_output_tokens": 250,
                  "total_tool_calls": 7
                }
              ]
            }"#,
    )
    .expect("write tokens.json");
}

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

#[test]
fn window_string_round_trips_for_all_variants() {
    // ORB-00337 AC#1 — every variant must round-trip through `as_str`/
    // `from_str`, and unknown strings yield `OrbitError::InvalidInput`.
    for w in [
        ScoreboardWindow::Hour,
        ScoreboardWindow::Day,
        ScoreboardWindow::Week,
        ScoreboardWindow::Month,
        ScoreboardWindow::All,
    ] {
        assert_eq!(w.as_str().parse::<ScoreboardWindow>().ok(), Some(w));
    }
    assert!(matches!(
        "bogus".parse::<ScoreboardWindow>(),
        Err(OrbitError::InvalidInput(_))
    ));
}

#[test]
fn orchestration_section_is_independently_versioned_and_legacy_reads_default_it() {
    let now = Utc::now();
    let temp = tempfile::tempdir().expect("create tempdir");
    let summary = generate_summary_with_inputs(
        temp.path(),
        &[],
        &ScoreboardInputs {
            orchestration: Some(OrchestrationSummary {
                schema_version: ORCHESTRATION_SCHEMA_VERSION,
                scope: "managed_execution".to_string(),
                as_of: now,
                since: Some(now - chrono::Duration::hours(1)),
                until: now,
                buckets: vec![OrchestrationBucketSummary {
                    kind: OrchestrationBucketKind::Orchestrator,
                    orchestrator: Some("sol".to_string()),
                    invocation_count: 1,
                    linked_task_count: 1,
                    input_tokens: 0,
                    cache_read_tokens: 0,
                    cache_create_tokens: 0,
                    cache_create_1h_tokens: 0,
                    output_tokens: 0,
                    provider_cost_usd: 0.0,
                    provider_cost_count: 1,
                    derived_cost_usd: 0.0,
                    derived_cost_count: 0,
                    comparable_provider_cost_usd: 0.0,
                    comparable_derived_cost_usd: 0.0,
                    comparable_cost_count: 0,
                    comparable_cost_delta_usd: 0.0,
                    missing_provider_count: 0,
                    unpriced_derived_count: 1,
                    normalized_tokens: NormalizedTokenSummary::default(),
                    models: Vec::new(),
                }],
                normalized_tokens: NormalizedTokenSummary::default(),
                previous_normalized_tokens: None,
            }),
            ..ScoreboardInputs::default()
        },
    )
    .expect("generate summary");

    let encoded = serde_json::to_value(&summary).expect("serialize summary");
    assert_eq!(encoded["schema_version"], CURRENT_SCHEMA_VERSION);
    assert!(encoded.get("duels").is_none());
    assert!(encoded.get("planning_duels").is_none());
    assert_eq!(
        encoded["orchestration"]["schema_version"],
        ORCHESTRATION_SCHEMA_VERSION
    );
    assert_eq!(
        encoded["orchestration"]["buckets"][0]["provider_cost_usd"],
        0.0
    );
    assert_eq!(
        encoded["orchestration"]["buckets"][0]["unpriced_derived_count"],
        1
    );

    let legacy: ScoreboardSummary = serde_json::from_value(serde_json::json!({
        "schema_version": 6,
        "generated_at": now.to_rfc3339(),
        "agents": {}
    }))
    .expect("deserialize v6 summary without orchestration");
    assert!(legacy.orchestration.is_none());
}
