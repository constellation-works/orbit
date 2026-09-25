use crate::task::{Task, TaskStatus, normalize_task_tags};

#[test]
fn task_deserializes_missing_collection_fields_as_empty_vecs() {
    let task = serde_yaml::from_str::<Task>(
        r#"id: T20260101-1
title: Legacy task
description: Existing task record.
acceptance_criteria: []
dependencies: []
plan: ""
execution_summary: ""
context_files: []
status: backlog
priority: medium
task_type: chore
created_at: 2026-01-01T00:00:00Z
updated_at: 2026-01-01T00:00:00Z
"#,
    )
    .expect("task without tags deserializes");

    assert_eq!(task.tags, Vec::<String>::new());
    assert_eq!(task.required_tools, Vec::<String>::new());
    assert_eq!(task.crew, None);
}

#[test]
fn task_round_trips_with_crew_set() {
    let task = serde_yaml::from_str::<Task>(
        r#"id: T20260101-1
title: Crew task
description: Existing task record.
acceptance_criteria: []
dependencies: []
plan: ""
execution_summary: ""
context_files: []
status: backlog
priority: medium
task_type: chore
crew: codex
created_at: 2026-01-01T00:00:00Z
updated_at: 2026-01-01T00:00:00Z
"#,
    )
    .expect("task with crew deserializes");

    let serialized = serde_yaml::to_string(&task).expect("serialize task");
    let reparsed = serde_yaml::from_str::<Task>(&serialized).expect("reparse task");

    assert_eq!(reparsed, task);
    assert_eq!(reparsed.crew.as_deref(), Some("codex"));
}

#[test]
fn normalize_task_tags_trims_lowercases_and_dedupes() {
    let tags = normalize_task_tags(vec![
        "  Perf ".to_string(),
        "BENCH".to_string(),
        "perf".to_string(),
        "   ".to_string(),
    ]);

    assert_eq!(tags, vec!["perf", "bench"]);
}

#[test]
fn task_deserializes_missing_complexity_as_none() {
    let task = serde_yaml::from_str::<Task>(
        r#"id: T20260101-1
title: Legacy unlabeled task
description: Existing task record.
acceptance_criteria: []
dependencies: []
plan: ""
execution_summary: ""
context_files: []
status: backlog
priority: medium
task_type: chore
created_at: 2026-01-01T00:00:00Z
updated_at: 2026-01-01T00:00:00Z
"#,
    )
    .expect("task without complexity deserializes");

    assert_eq!(task.complexity, None);
}

#[test]
fn task_complexity_unassessed_round_trips_and_is_not_assessed() {
    use crate::task::TaskComplexity;
    use std::str::FromStr;

    assert_eq!(
        TaskComplexity::from_str("unassessed").expect("parse"),
        TaskComplexity::Unassessed
    );
    assert_eq!(TaskComplexity::Unassessed.to_string(), "unassessed");
    assert!(!TaskComplexity::Unassessed.is_assessed());
    assert!(TaskComplexity::Low.is_assessed());
    assert!(TaskComplexity::Unassessed.require_assessed().is_err());
    assert!(
        TaskComplexity::Unassessed
            .require_assessed()
            .expect_err("unassessed is refused")
            .contains("xhard"),
        "the refusal must name every assessed value an operator may pick"
    );
    assert_eq!(
        TaskComplexity::Hard.require_assessed().expect("assessed"),
        TaskComplexity::Hard
    );
}

/// [ORB-12605] `xhard` is one word on every surface: serde's snake_case
/// rename and clap's kebab-case value naming would both otherwise split it
/// into `x_hard` / `x-hard`, which no operator or agent writes.
#[test]
fn task_complexity_xhard_round_trips_as_one_word_and_outranks_hard() {
    use crate::task::TaskComplexity;
    use std::str::FromStr;

    assert_eq!(
        TaskComplexity::from_str("xhard").expect("parse"),
        TaskComplexity::XHard
    );
    assert_eq!(TaskComplexity::XHard.as_str(), "xhard");
    assert_eq!(TaskComplexity::XHard.to_string(), "xhard");
    assert_eq!(
        serde_json::to_string(&TaskComplexity::XHard).expect("ser"),
        "\"xhard\""
    );
    assert_eq!(
        serde_json::from_str::<TaskComplexity>("\"xhard\"").expect("de"),
        TaskComplexity::XHard
    );
    assert!(TaskComplexity::XHard.is_assessed());
    assert_eq!(
        TaskComplexity::XHard.require_assessed().expect("assessed"),
        TaskComplexity::XHard
    );
    assert!(
        TaskComplexity::XHard.assessment_rank() > TaskComplexity::Hard.assessment_rank(),
        "xhard is the top assessed tier"
    );
    assert!(
        TaskComplexity::Unassessed.assessment_rank() < TaskComplexity::Low.assessment_rank(),
        "the absence of an assessment never reads as an escalation"
    );
}

#[test]
fn task_status_deserializes_both_hyphen_and_snake_for_in_progress() {
    let snake: TaskStatus = serde_json::from_str("\"in_progress\"").expect("snake de");
    let hyphen: TaskStatus = serde_json::from_str("\"in-progress\"").expect("hyphen de");
    assert_eq!(snake, TaskStatus::InProgress);
    assert_eq!(hyphen, TaskStatus::InProgress);
    // serialize remains snake_case for persisted history/events compat with prior records
    assert_eq!(
        serde_json::to_string(&TaskStatus::InProgress).expect("ser"),
        "\"in_progress\""
    );
}

#[test]
fn task_status_rejects_removed_friction_variant() {
    assert!(serde_json::from_str::<TaskStatus>("\"friction\"").is_err());
    assert!("friction".parse::<TaskStatus>().is_err());
}
