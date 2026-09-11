use crate::application::task::{TaskAddParams, compute_task_add_warnings};
use orbit_common::OrbitError;
use orbit_types::task::{TaskStatus, TaskType};

use super::test_runtime;

#[test]
fn task_add_enters_proposed_and_requires_approval_before_backlog() {
    let (_root, runtime) = test_runtime();

    let task = runtime
        .add_task(TaskAddParams {
            title: "Create orbit hello".to_string(),
            description: "Add a small hello file.".to_string(),
            acceptance_criteria: vec!["orbit-hello.txt exists.".to_string()],
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("human task add succeeds");

    assert_eq!(task.status, TaskStatus::Proposed);

    let approved = runtime
        .approve_task(&task.id, Some("LGTM".to_string()), None)
        .expect("proposed task can be approved into backlog");
    assert_eq!(approved.status, TaskStatus::Backlog);

    let started = runtime
        .start_task(&task.id, Some("start approved task".to_string()), None)
        .expect("backlog task starts directly");
    assert_eq!(started.status, TaskStatus::InProgress);
}

#[test]
fn task_context_selector_round_trips_from_repository_root() {
    let (root, runtime) = test_runtime();
    let repo_root = root.path().join("repo");
    std::fs::create_dir_all(repo_root.join("docs")).expect("create docs directory");
    std::fs::write(repo_root.join("docs/readme.md"), b"# readme\n").expect("write context file");

    let task = runtime
        .add_task(TaskAddParams {
            title: "Read back a subdirectory selector".to_string(),
            description: "Keep the selector rooted at the repository.".to_string(),
            context_files: vec!["file:docs/readme.md".to_string()],
            // Retained for internal parameter compatibility; it must not
            // change the canonical root used by task selectors.
            workspace_path: Some("docs".to_string()),
            ..Default::default()
        })
        .expect("create task with repository-relative context selector");

    assert_eq!(task.context_files, ["file:docs/readme.md"]);
    assert_eq!(
        runtime
            .get_task(&task.id)
            .expect("read task back")
            .context_files,
        ["file:docs/readme.md"]
    );

    let page = runtime
        .query_task_rows(&crate::application::task::TaskListQuery {
            path: Some("docs/readme.md".to_string()),
            ..Default::default()
        })
        .expect("filter tasks by the selector path");
    assert_eq!(
        page.items
            .iter()
            .map(|row| &row.task.id)
            .collect::<Vec<_>>(),
        [&task.id]
    );
    assert!(runtime.dry_run_prune_context_files(&task).is_empty());

    let updated = runtime
        .update_task(
            &task.id,
            crate::application::task::TaskUpdateParams {
                context_files: Some(vec!["file:docs/readme.md".to_string()]),
                ..Default::default()
            },
        )
        .expect("update the selector through the same repository root");
    assert_eq!(updated.context_files, ["file:docs/readme.md"]);
}

#[test]
fn task_start_event_records_when_start_approves_a_proposal() {
    let (_root, runtime) = test_runtime();

    let proposed = runtime
        .add_task(TaskAddParams {
            title: "Start a proposed task".to_string(),
            description: "Exercise the approval-start audit event.".to_string(),
            plan: "Start the task.".to_string(),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("create proposed task");
    runtime
        .start_task(&proposed.id, None, None)
        .expect("proposed task starts");

    let backlog = runtime
        .add_task(TaskAddParams {
            title: "Start a backlog task".to_string(),
            description: "Exercise the ordinary start audit event.".to_string(),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("create backlog task");
    runtime
        .approve_task(&backlog.id, None, None)
        .expect("proposed task enters backlog");
    runtime
        .start_task(&backlog.id, None, None)
        .expect("backlog task starts");

    let events = runtime
        .list_session_events(10)
        .expect("list session events");
    let started_from_proposed = events
        .iter()
        .find(|event| {
            event.event_type == "TaskStarted" && event.payload["data"]["id"] == proposed.id
        })
        .expect("proposed start event");
    let started_from_backlog = events
        .iter()
        .find(|event| {
            event.event_type == "TaskStarted" && event.payload["data"]["id"] == backlog.id
        })
        .expect("backlog start event");

    assert_eq!(
        started_from_proposed.payload["data"]["approved_from_proposed"],
        true
    );
    assert_eq!(
        started_from_backlog.payload["data"]["approved_from_proposed"],
        false
    );
}

#[test]
fn task_add_does_not_scan_unrelated_corrupt_bundles() {
    let (root, runtime) = test_runtime();
    let task_a = runtime
        .add_task(TaskAddParams {
            title: "Readable A".to_string(),
            description: "A remains readable.".to_string(),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("create task A");
    let task_c = runtime
        .add_task(TaskAddParams {
            title: "Corrupt C".to_string(),
            description: "C will be malformed.".to_string(),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("create task C");

    let workspace_bundles = root.path().join("global/tasks/workspaces");
    let workspace_dir = std::fs::read_dir(&workspace_bundles)
        .expect("read workspace bundle roots")
        .next()
        .expect("one workspace bundle root")
        .expect("workspace bundle entry")
        .path();
    let corrupt_dir = workspace_dir.join(&task_c.id);
    std::fs::remove_file(corrupt_dir.join("description.md")).expect("malform task C");

    assert_eq!(
        runtime
            .get_task(&task_a.id)
            .expect("show unrelated task A")
            .id,
        task_a.id
    );
    let task_b = runtime
        .add_task(TaskAddParams {
            title: "New B".to_string(),
            description: "B must not scan C.".to_string(),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("add task B despite corrupt task C");
    assert_ne!(task_b.id, task_c.id);
    assert!(matches!(
        runtime.list_tasks(),
        Err(OrbitError::TaskBundleCorrupt { task_id, .. }) if task_id == task_c.id
    ));
    assert!(
        corrupt_dir.is_dir(),
        "diagnosis must not quarantine or delete C"
    );
}

// --- ORB-00251: context_files omission / over-inclusion warning helper tests ---

#[test]
fn add_task_warnings_omission_for_non_chore_empty_context() {
    // (a) non-chore + empty -> omission present, over absent
    let w = compute_task_add_warnings(&[], TaskType::Feature);
    assert_eq!(w.len(), 1);
    assert!(w[0].contains("without context_files"));
    assert!(!w[0].contains("reference material"));
}

#[test]
fn add_task_warnings_none_for_non_chore_with_only_targets() {
    // (b)
    let w = compute_task_add_warnings(
        &["file:src/main.rs".to_string(), "dir:crates/foo".to_string()],
        TaskType::Bug,
    );
    assert!(w.is_empty());
}

#[test]
fn add_task_warnings_none_for_chore_empty() {
    // (c) chore + empty -> no warnings
    let w = compute_task_add_warnings(&[], TaskType::Chore);
    assert!(w.is_empty());
}

#[test]
fn add_task_warnings_over_inclusion_for_design_patterns() {
    // (d) non-chore + design-patterns entry -> over present, omission absent
    let w = compute_task_add_warnings(
        &["file:docs/design-patterns/test_layout.md".to_string()],
        TaskType::Refactor,
    );
    assert_eq!(w.len(), 1);
    assert!(w[0].contains("reference material"));
    assert!(w[0].contains("docs/design-patterns/test_layout.md"));
    assert!(!w[0].contains("without context_files"));
}

#[test]
fn add_task_warnings_no_over_inclusion_for_feature_design_doc() {
    // (e) feature design docs are excluded from over-inclusion
    let w = compute_task_add_warnings(
        &["file:docs/design/some-feature/2_design.md".to_string()],
        TaskType::Feature,
    );
    assert!(w.is_empty());
}

#[test]
fn add_task_warnings_mixed_valid_and_claude_over_only() {
    // (f) mix valid + CLAUDE.md -> over naming only the bad one; no omission
    let w = compute_task_add_warnings(
        &["file:src/foo.rs".to_string(), "file:CLAUDE.md".to_string()],
        TaskType::Feature,
    );
    assert_eq!(w.len(), 1);
    assert!(w[0].contains("reference material"));
    assert!(w[0].contains("CLAUDE.md"));
    assert!(!w[0].contains("without context_files"));
    assert!(!w[0].contains("src/foo.rs"));
}

#[test]
fn task_add_redacts_secrets_in_stored_fields() {
    // [ORB-00417] A pasted key in title/description/plan/acceptance_criteria/
    // comment must be redacted at write time so it never lands in the task
    // registry.
    let (_root, runtime) = test_runtime();

    let sk_key = "sk-ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcd";
    let bearer_token = "abc123def456ghi789SECRETTOKEN";
    let task = runtime
        .add_task(TaskAddParams {
            title: format!("Fix auth using {sk_key}"),
            description: format!("Header pasted: Authorization: Bearer {bearer_token}"),
            acceptance_criteria: vec![format!("no leak of {sk_key}")],
            plan: format!("call the API with {sk_key}"),
            comment: Some(format!("context: reproduce with {sk_key}")),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("task add succeeds");

    let check = |task: &orbit_types::task::Task, label: &str| {
        assert!(
            !task.title.contains(sk_key),
            "{label}: title leaked key: {}",
            task.title
        );
        assert!(
            !task.description.contains(bearer_token),
            "{label}: description leaked bearer token: {}",
            task.description
        );
        assert!(
            !task.plan.contains(sk_key),
            "{label}: plan leaked key: {}",
            task.plan
        );
        assert!(
            !task.acceptance_criteria.iter().any(|c| c.contains(sk_key)),
            "{label}: acceptance criteria leaked key: {:?}",
            task.acceptance_criteria
        );
        assert!(
            task.title.contains("[REDACTED"),
            "{label}: title should carry a redaction placeholder: {}",
            task.title
        );
    };

    // The returned (just-created) record is redacted...
    check(&task, "returned");
    // ...and so is the persisted record read back from the store.
    let reloaded = runtime.get_task(&task.id).expect("get task");
    check(&reloaded, "reloaded");

    // The creation comment is persisted separately — it must be redacted too.
    let comments = runtime.get_task_comments(&task.id).expect("get comments");
    assert!(
        !comments.iter().any(|c| c.message.contains(sk_key)),
        "creation comment leaked key: {comments:?}"
    );
    assert!(
        comments.iter().any(|c| c.message.contains("[REDACTED")),
        "creation comment should carry a redaction placeholder: {comments:?}"
    );
}

#[test]
fn task_add_applies_normalized_provenance_title_prefixes() {
    for (tag, expected_prefix) in [
        (" qa-SWEEP ", "[qa-sweep] "),
        ("security-review", "[security-review] "),
        ("code-review", "[code-review] "),
        ("friction-curation", "[friction-curation] "),
    ] {
        let (_root, runtime) = test_runtime();
        let task = runtime
            .add_task(TaskAddParams {
                title: "Confirmed finding".to_string(),
                tags: vec![tag.to_string()],
                workspace_path: Some(".".to_string()),
                ..Default::default()
            })
            .expect("task add succeeds");

        assert_eq!(task.title, format!("{expected_prefix}Confirmed finding"));
    }
}

#[test]
fn task_add_does_not_double_the_applicable_provenance_prefix() {
    let (_root, runtime) = test_runtime();

    let task = runtime
        .add_task(TaskAddParams {
            title: "[code-review] Confirmed finding".to_string(),
            tags: vec!["code-review".to_string()],
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("task add succeeds");

    assert_eq!(task.title, "[code-review] Confirmed finding");
}

#[test]
fn task_add_uses_fixed_provenance_precedence_independent_of_tag_order() {
    for tags in [
        vec!["friction-curation", "security-review", "qa-sweep"],
        vec!["qa-sweep", "friction-curation", "security-review"],
    ] {
        let (_root, runtime) = test_runtime();
        let task = runtime
            .add_task(TaskAddParams {
                title: "Confirmed finding".to_string(),
                tags: tags.into_iter().map(str::to_string).collect(),
                workspace_path: Some(".".to_string()),
                ..Default::default()
            })
            .expect("task add succeeds");

        assert_eq!(task.title, "[qa-sweep] Confirmed finding");
    }
}

#[test]
fn task_add_preserves_auto_task_title_prefix_behavior() {
    for title in ["Scheduled work", "[auto-task] Scheduled work"] {
        let (_root, runtime) = test_runtime();
        let task = runtime
            .add_task(TaskAddParams {
                title: title.to_string(),
                tags: vec!["auto-task:qa-sweep".to_string(), "qa-sweep".to_string()],
                workspace_path: Some(".".to_string()),
                ..Default::default()
            })
            .expect("task add succeeds");

        assert_eq!(task.title, "[auto-task] Scheduled work");
    }
}

/// `add_task` is the shared core write path: task-pilot apply, automation
/// seeding, and the runtime host all create tasks whose context names files
/// the task will produce. Existence is enforced on the operator surfaces
/// instead (CLI `task add`, `orbit.task.add`).
#[test]
fn task_add_keeps_context_selectors_that_do_not_exist_yet() {
    let (_root, runtime) = test_runtime();

    let task = runtime
        .add_task(TaskAddParams {
            title: "Future context".to_string(),
            context_files: vec!["file:src/future.rs".to_string()],
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("core add_task must accept a not-yet-existing file selector");

    assert_eq!(task.context_files, vec!["file:src/future.rs".to_string()]);
}

#[test]
fn task_add_accepts_valid_context_selectors() {
    let (root, runtime) = test_runtime();
    let repo_dir = root.path().join("repo");
    std::fs::create_dir_all(repo_dir.join("src")).expect("create src");
    std::fs::write(repo_dir.join("src/lib.rs"), b"pub fn run() {}\n").expect("write lib.rs");

    let task = runtime
        .add_task(TaskAddParams {
            title: "Valid context".to_string(),
            context_files: vec![
                "file:src/lib.rs".to_string(),
                "dir:src".to_string(),
                "symbol:src/lib.rs#run:function".to_string(),
            ],
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("task add with valid selectors succeeds");

    assert_eq!(
        task.context_files,
        vec![
            "file:src/lib.rs".to_string(),
            "dir:src".to_string(),
            "symbol:src/lib.rs#run:function".to_string(),
        ]
    );
}
