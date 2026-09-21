use chrono::{Duration, TimeZone, Utc};
use orbit_types::task::{Task, TaskPriority, TaskStatus, TaskType};

use super::super::highlights::{
    NOTABLE_COMPLETIONS_LIMIT, NOTABLE_SELECTION_LABEL, NOTABLE_SELECTION_METHOD,
    SUMMARY_EXCERPT_MAX_CHARS, excerpt_execution_summary,
};
use super::super::*;

fn task_at(
    id: &str,
    status: TaskStatus,
    priority: TaskPriority,
    completed_at: chrono::DateTime<Utc>,
    summary: &str,
    tags: &[&str],
) -> Task {
    Task {
        job_run_machine: None,
        id: id.to_string(),
        title: format!("title {id}"),
        description: String::new(),
        acceptance_criteria: Vec::new(),
        tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
        required_tools: Vec::new(),
        plan: String::new(),
        execution_summary: summary.to_string(),
        context_files: Vec::new(),
        created_by: None,
        planned_by: None,
        implemented_by: None,
        status,
        priority,
        complexity: None,
        task_type: TaskType::Chore,
        pr_status: None,
        external_refs: Vec::new(),
        relations: Vec::new(),
        job_run_id: None,
        crew: None,
        orchestrator: None,
        created_at: completed_at,
        updated_at: completed_at,
    }
}

#[test]
fn notable_completions_order_by_priority_then_recency_then_id() {
    let t0 = Utc.with_ymd_and_hms(2026, 8, 1, 12, 0, 0).unwrap();
    let tasks = vec![
        task_at(
            "ORB-3",
            TaskStatus::Done,
            TaskPriority::High,
            t0 + Duration::hours(1),
            "later high",
            &[],
        ),
        task_at(
            "ORB-1",
            TaskStatus::Done,
            TaskPriority::Critical,
            t0,
            "older critical",
            &["impact:cleanup"],
        ),
        task_at(
            "ORB-2",
            TaskStatus::Done,
            TaskPriority::High,
            t0 + Duration::hours(2),
            "newest high",
            &[],
        ),
        task_at(
            "ORB-4",
            TaskStatus::Done,
            TaskPriority::High,
            t0 + Duration::hours(2),
            "tie recency, later id",
            &[],
        ),
        task_at(
            "ORB-skip",
            TaskStatus::Backlog,
            TaskPriority::Critical,
            t0 + Duration::hours(3),
            "not completed",
            &[],
        ),
    ];

    let selected = select_notable_completions(&tasks, None);
    assert_eq!(selected.method, NOTABLE_SELECTION_METHOD);
    assert_eq!(selected.label, NOTABLE_SELECTION_LABEL);
    assert_eq!(selected.limit, NOTABLE_COMPLETIONS_LIMIT);
    let ids: Vec<&str> = selected
        .items
        .iter()
        .map(|item| item.task_id.as_str())
        .collect();
    assert_eq!(ids, vec!["ORB-1", "ORB-2", "ORB-4", "ORB-3"]);
    assert_eq!(
        selected.items[0].impact_tag.as_deref(),
        Some("impact:cleanup")
    );
    assert_eq!(selected.items[0].priority, "critical");
    assert_eq!(selected.items[0].task_type, "chore");
}

#[test]
fn notable_completions_truncate_to_limit_and_bound_excerpt() {
    let t0 = Utc.with_ymd_and_hms(2026, 8, 2, 0, 0, 0).unwrap();
    let long = "word ".repeat(80);
    let mut tasks = Vec::new();
    for index in 0..(NOTABLE_COMPLETIONS_LIMIT + 3) {
        tasks.push(task_at(
            &format!("ORB-{index:02}"),
            TaskStatus::Done,
            TaskPriority::Medium,
            t0 + Duration::minutes(index as i64),
            &long,
            &[],
        ));
    }

    let selected = select_notable_completions(&tasks, None);
    assert_eq!(selected.items.len(), NOTABLE_COMPLETIONS_LIMIT);
    let first = &selected.items[0];
    let excerpt = first.summary_excerpt.as_deref().expect("excerpt present");
    assert!(excerpt.ends_with('…'));
    assert!(excerpt.chars().count() <= SUMMARY_EXCERPT_MAX_CHARS + 1);
    assert!(!excerpt.contains("  "));
}

#[test]
fn notable_completions_omit_missing_summary_and_non_impact_tags() {
    let t0 = Utc.with_ymd_and_hms(2026, 8, 3, 0, 0, 0).unwrap();
    let tasks = vec![task_at(
        "ORB-empty",
        TaskStatus::Archived,
        TaskPriority::Low,
        t0,
        "   \n\t  ",
        &["dashboard", "ImpactIgnored", "impact"],
    )];

    let selected = select_notable_completions(&tasks, None);
    assert_eq!(selected.items.len(), 1);
    assert!(selected.items[0].summary_excerpt.is_none());
    assert!(
        selected.items[0].impact_tag.is_none(),
        "only an explicit impact: prefix is an impact tag"
    );
}

#[test]
fn notable_completions_honor_window_cutoff() {
    let now = Utc.with_ymd_and_hms(2026, 8, 10, 0, 0, 0).unwrap();
    let inside = now - Duration::hours(2);
    let outside = now - Duration::hours(48);
    let tasks = vec![
        task_at(
            "ORB-in",
            TaskStatus::Done,
            TaskPriority::Low,
            inside,
            "inside",
            &[],
        ),
        task_at(
            "ORB-out",
            TaskStatus::Done,
            TaskPriority::Critical,
            outside,
            "outside",
            &[],
        ),
    ];

    let selected = select_notable_completions(&tasks, Some(now - Duration::hours(24)));
    let ids: Vec<&str> = selected
        .items
        .iter()
        .map(|item| item.task_id.as_str())
        .collect();
    assert_eq!(ids, vec!["ORB-in"]);
}

#[test]
fn coverage_distinguishes_windowed_snapshot_from_observed_lifetime() {
    let windowed = snapshot_coverage(true);
    assert_eq!(
        windowed.review.availability,
        CoverageAvailability::Unavailable
    );
    assert!(windowed.review.detail.contains("omitted from this window"));
    assert_eq!(
        windowed.snapshot_metrics.availability,
        CoverageAvailability::Unavailable
    );

    let lifetime = snapshot_coverage(false);
    assert_eq!(lifetime.review.availability, CoverageAvailability::Observed);
    assert!(
        lifetime
            .review
            .detail
            .contains("no observed review comments")
    );
}

#[test]
fn generate_summary_includes_highlights_and_windowed_coverage() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let now = Utc.with_ymd_and_hms(2026, 8, 16, 12, 0, 0).unwrap();
    let tasks = vec![
        task_at(
            "ORB-keep",
            TaskStatus::Done,
            TaskPriority::High,
            now - Duration::hours(1),
            "Outcome: success Changes: structural cleanup Assessment: landed",
            &["impact:maintenance"],
        ),
        task_at(
            "ORB-old",
            TaskStatus::Done,
            TaskPriority::Critical,
            now - Duration::days(10),
            "old",
            &[],
        ),
    ];

    let summary = generate_summary_with_inputs(
        temp.path(),
        &tasks,
        &ScoreboardInputs {
            now: Some(now),
            window: ScoreboardWindow::Day,
            ..ScoreboardInputs::default()
        },
    )
    .expect("generate summary");

    assert_eq!(summary.schema_version, 9);
    assert_eq!(summary.notable_completions.items.len(), 1);
    assert_eq!(summary.notable_completions.items[0].task_id, "ORB-keep");
    assert_eq!(
        summary.notable_completions.items[0].impact_tag.as_deref(),
        Some("impact:maintenance")
    );
    assert_eq!(
        summary.coverage.review.availability,
        CoverageAvailability::Unavailable
    );
}

#[test]
fn excerpt_collapses_whitespace_and_keeps_short_text() {
    assert_eq!(
        excerpt_execution_summary("  Outcome: success \n\n landed  "),
        Some("Outcome: success landed".to_string())
    );
    assert_eq!(excerpt_execution_summary(" \n "), None);
}
