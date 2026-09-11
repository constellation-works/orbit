use orbit_engine::{RuntimeHost, TaskAutomationUpdate, WORKFLOW_RUN_FAILED_EVENT};
use orbit_types::task::TaskStatus;
use serde_json::json;

use crate::OrbitRuntime;
use crate::application::task::{TaskAddParams, TaskUpdateParams};

#[test]
fn task_context_for_agent_input_embeds_canonical_task_with_input_overrides() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task = runtime
        .add_task(TaskAddParams {
            title: "Envelope task".to_string(),
            description: "Task description for agent context.".to_string(),
            acceptance_criteria: vec!["Agent can recover the task id.".to_string()],
            plan: "Read the task and implement it.".to_string(),
            ..Default::default()
        })
        .expect("add task");

    let context = runtime
        .task_context_for_agent_input(&json!({
            "task_id": task.id.clone(),
            "workspace_path": "/override/worktree",
            "repo_root": "/override/repo"
        }))
        .expect("build task context")
        .expect("task context present");

    assert_eq!(context["id"], task.id);
    assert_eq!(context["title"], "Envelope task");
    assert_eq!(
        context["description"],
        "Task description for agent context."
    );
    assert_eq!(
        context["acceptance_criteria"][0],
        "Agent can recover the task id."
    );
    assert_eq!(context["plan"], "Read the task and implement it.");
    assert_eq!(context["workspace_path"], "/override/worktree");
    assert_eq!(context["repo_root"], "/override/repo");
    assert_eq!(context["status"], task.status.cli_name());
    assert_eq!(context["terminal"], false);
    assert!(context.get("execution_summary").is_none());
    assert!(context.get("status_note").is_none());

    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                execution_summary: Some("Prior attempt needs a missing capability.".to_string()),
                ..Default::default()
            },
        )
        .expect("record prior execution summary");
    runtime
        .apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Blocked),
                status_event: Some(WORKFLOW_RUN_FAILED_EVENT.to_string()),
                status_note: Some("workflow run failed: missing provider capability".to_string()),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("record workflow failure");
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::InProgress),
                ..Default::default()
            },
        )
        .expect("redispatch task");

    let redispatched_context = runtime
        .task_context_for_agent_input(&json!({ "task_id": task.id.clone() }))
        .expect("build redispatched task context")
        .expect("task context present");

    assert_eq!(
        redispatched_context["execution_summary"],
        "Prior attempt needs a missing capability."
    );
    assert_eq!(
        redispatched_context["status_note"],
        "workflow run failed: missing provider capability"
    );
}

/// [ORB-10499]: `agent_implement` can be dispatched against a task that has
/// already gone terminal — via the executor's single post-recovery attempt, or
/// via a promotion through the approve surface. The envelope has to name that
/// up front so the invocation can exit before doing the work rather than at its
/// final persist call.
#[test]
fn task_context_for_agent_input_marks_write_gated_statuses_terminal() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task = runtime
        .add_task(TaskAddParams {
            title: "Already finished by a prior attempt".to_string(),
            description: "Task description for agent context.".to_string(),
            ..Default::default()
        })
        .expect("add task");

    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
        )
        .expect("drive task to done");

    let context = runtime
        .task_context_for_agent_input(&json!({ "task_id": task.id.clone() }))
        .expect("build task context")
        .expect("task context present");

    assert_eq!(context["status"], "done");
    assert_eq!(context["terminal"], true);
}

/// [ORB-11327]: a task with no comments must still project a `comments` array
/// rather than a null/absent field, so downstream consumers never special-case
/// the empty case.
#[test]
fn task_context_for_agent_input_has_empty_comments_when_none_posted() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task = runtime
        .add_task(TaskAddParams {
            title: "No comments yet".to_string(),
            description: "Task description for agent context.".to_string(),
            ..Default::default()
        })
        .expect("add task");

    let context = runtime
        .task_context_for_agent_input(&json!({ "task_id": task.id.clone() }))
        .expect("build task context")
        .expect("task context present");

    assert_eq!(context["comments"], json!([]));
    assert!(context.get("comments_truncated").is_none());
    assert!(context.get("comments_omitted_count").is_none());
}

/// [ORB-11327]: comments are the mechanism an orchestrator uses to supersede a
/// stale "suggested direction" left in the description (the ORB-11248
/// incident). The envelope must carry them, in order, with author and
/// timestamp so the executor can see a later refinement without an extra call.
#[test]
fn task_context_for_agent_input_includes_ordered_comments_with_author_and_timestamp() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task = runtime
        .add_task(TaskAddParams {
            title: "Suggested direction gets superseded".to_string(),
            description: "## Suggested direction\n\nDo the naive thing.".to_string(),
            ..Default::default()
        })
        .expect("add task");

    runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                comment: Some("First pass looks reasonable.".to_string()),
                ..Default::default()
            },
            Some("codex".to_string()),
            None,
        )
        .expect("post first comment");
    runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                comment: Some(
                    "Orchestrator refinement before implementation: do not adopt the \
                     suggested direction as-is."
                        .to_string(),
                ),
                ..Default::default()
            },
            Some("codex".to_string()),
            None,
        )
        .expect("post superseding comment");

    let context = runtime
        .task_context_for_agent_input(&json!({ "task_id": task.id.clone() }))
        .expect("build task context")
        .expect("task context present");

    let comments = context["comments"]
        .as_array()
        .expect("comments is an array");
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[0]["message"], "First pass looks reasonable.");
    assert_eq!(comments[0]["by"], "codex");
    assert!(comments[0]["at"].is_string());
    assert_eq!(
        comments[1]["message"],
        "Orchestrator refinement before implementation: do not adopt the suggested \
         direction as-is."
    );
    assert_eq!(comments[1]["by"], "codex");
    // Chronological order: the superseding comment is last, not first.
    assert!(comments[0]["at"].as_str() <= comments[1]["at"].as_str());
    assert!(context.get("comments_truncated").is_none());
}

/// [ORB-11327]: comment history is unbounded in principle, so the envelope
/// caps it. Truncation must drop the *oldest* entries first — the newest
/// comments are the ones that can supersede the description — and must signal
/// that truncation happened rather than silently dropping entries.
#[test]
fn task_context_for_agent_input_truncates_oldest_comments_over_the_count_cap() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task = runtime
        .add_task(TaskAddParams {
            title: "Long comment history".to_string(),
            description: "Task description for agent context.".to_string(),
            ..Default::default()
        })
        .expect("add task");

    const POSTED: usize = 25;
    const MAX_KEPT: usize = 20;
    for index in 0..POSTED {
        runtime
            .update_task_with_identity(
                &task.id,
                TaskUpdateParams {
                    comment: Some(format!("comment {index}")),
                    ..Default::default()
                },
                Some("codex".to_string()),
                None,
            )
            .expect("post comment");
    }

    let context = runtime
        .task_context_for_agent_input(&json!({ "task_id": task.id.clone() }))
        .expect("build task context")
        .expect("task context present");

    let comments = context["comments"]
        .as_array()
        .expect("comments is an array");
    assert_eq!(comments.len(), MAX_KEPT);
    // Oldest-first truncation: the surviving window is the newest MAX_KEPT
    // comments, still in chronological order, ending on the very last post.
    assert_eq!(
        comments[0]["message"],
        format!("comment {}", POSTED - MAX_KEPT)
    );
    assert_eq!(
        comments[MAX_KEPT - 1]["message"],
        format!("comment {}", POSTED - 1)
    );
    assert_eq!(context["comments_truncated"], true);
    assert_eq!(
        context["comments_omitted_count"],
        (POSTED - MAX_KEPT) as u64
    );
}

/// [ORB-11338]: comment creation imposes no size limit, so the newest retained
/// comment can exceed the envelope's byte budget on its own. Dropping older
/// entries cannot help there — the projection has to cut the body itself
/// rather than hand the agent an unbounded prompt.
#[test]
fn task_context_for_agent_input_truncates_a_single_oversized_comment_body() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task = runtime
        .add_task(TaskAddParams {
            title: "One enormous comment".to_string(),
            description: "Task description for agent context.".to_string(),
            ..Default::default()
        })
        .expect("add task");

    const MAX_BYTES: usize = 16 * 1024;
    let oversized = format!("HEAD MARKER {}", "x".repeat(MAX_BYTES * 2));
    runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                comment: Some(oversized),
                ..Default::default()
            },
            Some("codex".to_string()),
            None,
        )
        .expect("post oversized comment");

    let context = runtime
        .task_context_for_agent_input(&json!({ "task_id": task.id.clone() }))
        .expect("build task context")
        .expect("task context present");

    let comments = context["comments"]
        .as_array()
        .expect("comments is an array");
    // The comment is retained, not dropped: it is the newest one.
    assert_eq!(comments.len(), 1);
    let message = comments[0]["message"]
        .as_str()
        .expect("comment message is a string");
    assert!(
        message.len() <= MAX_BYTES,
        "projected comment body is {} bytes, over the {MAX_BYTES}-byte budget",
        message.len()
    );
    assert!(message.starts_with("HEAD MARKER "));
    assert!(message.ends_with("[comment truncated to fit the envelope byte budget]"));
    // Truncation is reported, and no whole entry was omitted.
    assert_eq!(context["comments_truncated"], true);
    assert!(context.get("comments_omitted_count").is_none());
}
