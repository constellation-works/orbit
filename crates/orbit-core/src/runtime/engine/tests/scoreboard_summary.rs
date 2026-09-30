use orbit_types::task::{TaskPriority, TaskStatus};
use tempfile::tempdir;

use crate::OrbitRuntime;
use crate::application::task::{TaskAddParams, TaskUpdateParams};

fn runtime() -> (tempfile::TempDir, OrbitRuntime) {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let workspace_root = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime = OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime");
    (root, runtime)
}

fn add_done_task(
    runtime: &OrbitRuntime,
    title: &str,
    priority: TaskPriority,
    implemented_by: &str,
    execution_summary: &str,
) -> String {
    let id = runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("description of {title}"),
            priority,
            ..TaskAddParams::default()
        })
        .expect("add task")
        .id
        .to_string();
    for status in [TaskStatus::InProgress, TaskStatus::Review] {
        runtime
            .update_task(
                &id,
                TaskUpdateParams {
                    status: Some(status),
                    ..TaskUpdateParams::default()
                },
            )
            .expect("advance task");
    }
    runtime
        .update_task(
            &id,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                implemented_by: Some(Some(implemented_by.to_string())),
                execution_summary: Some(execution_summary.to_string()),
                ..TaskUpdateParams::default()
            },
        )
        .expect("complete task");
    id
}

/// The scoreboard aggregates from envelope metadata; the notable completions
/// are the one place a body document reaches the summary, so the excerpt must
/// still come from the stored execution summary.
#[test]
fn scoreboard_summary_reads_metadata_and_still_excerpts_notable_summaries() {
    let (_root, runtime) = runtime();
    let long_summary = format!("Shipped   the   parser\nfix. {}", "detail ".repeat(60));
    let critical = add_done_task(
        &runtime,
        "critical parser fix",
        TaskPriority::Critical,
        "claude / opus",
        &long_summary,
    );
    let low = add_done_task(
        &runtime,
        "low tidy",
        TaskPriority::Low,
        "claude / opus",
        "Tidied up.",
    );
    runtime
        .add_task(TaskAddParams {
            title: "still open".to_string(),
            description: "open work".to_string(),
            ..TaskAddParams::default()
        })
        .expect("add open task");

    let metadata = runtime.list_task_metadata().expect("list task metadata");
    let full = runtime.list_tasks().expect("list tasks");
    assert_eq!(metadata.len(), full.len());
    for (metadata, full) in metadata.iter().zip(&full) {
        assert_eq!(metadata.id, full.id, "same order as the full listing");
        assert_eq!(metadata.status, full.status);
        assert_eq!(metadata.priority, full.priority);
        assert_eq!(metadata.tags, full.tags);
        assert_eq!(metadata.implemented_by, full.implemented_by);
        assert_eq!(metadata.created_at, full.created_at);
        assert_eq!(metadata.updated_at, full.updated_at);
        assert!(
            metadata.description.is_empty()
                && metadata.plan.is_empty()
                && metadata.execution_summary.is_empty()
                && metadata.acceptance_criteria.is_empty(),
            "the metadata listing carries no body documents"
        );
    }

    let summary = runtime
        .generate_scoreboard_summary(None)
        .expect("generate scoreboard summary");

    let completed: u64 = summary
        .agents
        .values()
        .map(|agent| agent.tasks_completed)
        .sum();
    assert_eq!(completed, 2, "both done tasks are attributed");

    let notable = &summary.notable_completions.items;
    assert_eq!(
        notable
            .iter()
            .map(|item| item.task_id.as_str())
            .collect::<Vec<_>>(),
        [critical.as_str(), low.as_str()],
        "priority orders the highlights"
    );
    let excerpt = notable[0]
        .summary_excerpt
        .as_deref()
        .expect("the critical task keeps its summary excerpt");
    assert!(
        excerpt.starts_with("Shipped the parser fix. detail detail"),
        "whitespace is collapsed: {excerpt}"
    );
    assert!(excerpt.ends_with('…'), "a long summary is truncated");
    assert_eq!(notable[1].summary_excerpt.as_deref(), Some("Tidied up."));
}
