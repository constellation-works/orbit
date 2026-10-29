//! Landed creation work retains its grant and waits for operator reconciliation.

use super::*;
use orbit_types::task::TaskOrigin;

fn landed_creation_is_held_without_losing_authority_or_redispatching() {
    let workspace = Workspace::new();
    let operator = workspace.creation_task("operator creation");
    let automatic = workspace.creation_task("automatic creation");
    workspace.edit(
        &operator,
        TaskUpdateParams {
            origin: Some(TaskOrigin::OperatorRequest),
            ..Default::default()
        },
    );
    workspace.edit(
        &automatic,
        TaskUpdateParams {
            tags: Some(vec!["auto-task:fixture".into()]),
            origin: Some(TaskOrigin::Automation),
            ..Default::default()
        },
    );
    let history = workspace.runtime.get_task_history(&operator.id).unwrap();
    let prepared = workspace
        .action(
            "prepare_task_pilot",
            json!({
                "workspace_path": workspace.repo, "task_ids": [operator.id], "base_branch": "main",
            }),
        )
        .unwrap();
    let identity = prepared["tasks"][0]["context_creation_identity"].clone();
    let readme = workspace.commit("remaining scope delivered");
    std::fs::create_dir_all(workspace.repo.join("src")).unwrap();
    let created = workspace.commit_file(
        "src/new.rs",
        "pub fn delivered() {}\n",
        "creation delivered",
    );
    let outstanding = workspace.task("outstanding", &[], &["file:README.md"], TaskComplexity::Low);
    let pending = workspace
        .runtime
        .add_task_screened(
            TaskAddParams {
                title: "unfulfilled creation".into(),
                description: "Create the pending module and update README.md.".into(),
                acceptance_criteria: vec!["The pending module is implemented.".into()],
                plan: "Implement src/pending.rs.".into(),
                status: Some(TaskStatus::Proposed),
                context_files: vec!["file:README.md".into(), "file:src/pending.rs".into()],
                complexity: TaskComplexity::Low,
                ..Default::default()
            },
            &TaskWriteScreen::unbound(true),
            None,
            None,
        )
        .unwrap()
        .task;
    let drain = workspace.running("workspace_auto_pipeline", json!({"approve_proposed": true}));
    let selected = workspace.select(&drain);
    let pilot = workspace.pilot_child(&drain, &selected["task_ids"]);
    let evidence = format!("Complete scope delivered by {readme} and {created}.");
    let mut retained = assessment(&automatic);
    retained["context_files_after"] = json!(automatic.context_files);
    retained["already_landed"] = json!({"evidence": evidence});
    let applied = workspace
        .pilot(
            &drain,
            &pilot,
            vec![
                verified_no_diff(&operator, &evidence),
                retained,
                assessment(&outstanding),
                assessment(&pending),
            ],
        )
        .unwrap();
    assert_eq!(applied["status"], "succeeded", "{applied}");
    for task in [&operator, &automatic] {
        let decision = applied["drain_approval"]
            .as_array()
            .unwrap()
            .iter()
            .find(|decision| decision["task_id"] == task.id.as_str())
            .unwrap();
        assert_eq!(decision["decision"], "withhold", "{decision}");
        assert_eq!(decision["classification"], "already_landed", "{decision}");
        assert_eq!(
            decision["evidence"]["covering_commits"],
            json!([readme, created])
        );
        assert_eq!(
            decision["evidence"]["operator_reconciliation_required"],
            true
        );
        assert_eq!(
            workspace.runtime.get_task(&task.id).unwrap().context_files,
            task.context_files
        );
        assert_eq!(workspace.status(task), TaskStatus::Proposed);
    }
    assert_eq!(workspace.status(&outstanding), TaskStatus::Backlog);
    assert_eq!(workspace.status(&pending), TaskStatus::Backlog);
    assert_eq!(
        workspace
            .runtime
            .get_task(&pending.id)
            .unwrap()
            .context_files,
        pending.context_files
    );
    let after_history = workspace.runtime.get_task_history(&operator.id).unwrap();
    assert_eq!(
        serde_json::to_value(&after_history[..history.len()]).unwrap(),
        serde_json::to_value(history).unwrap()
    );
    let after = workspace
        .action(
            "prepare_task_pilot",
            json!({
                "workspace_path": workspace.repo, "task_ids": [operator.id], "base_branch": "main",
            }),
        )
        .unwrap();
    assert_eq!(after["tasks"][0]["context_creation_identity"], identity);
    assert_eq!(
        after["tasks"][0]["context_creation_selectors"],
        json!([NEW_FILE])
    );
    workspace.comment(&operator);
    workspace.record(&drain);
    let next = workspace.select(&drain);
    assert_eq!(next["task_ids"], json!([]), "{next}");
    for task in [&operator, &automatic] {
        assert_eq!(held_reason(&next, task), Some("already_landed"), "{next}");
    }
    let readiness = workspace
        .runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    assert_eq!(
        held_reason(&readiness["approvals"], &operator),
        Some("already_landed"),
        "{readiness}"
    );
    let comments = workspace.runtime.get_task_comments(&operator.id).unwrap();
    assert!(
        comments
            .iter()
            .any(|comment| comment.by == "task-pilot" && comment.message.contains(&evidence))
    );
    workspace.finish(&drain, JobRunState::Success);
    let later = workspace.running("workspace_auto_pipeline", json!({"approve_proposed": true}));
    let next = workspace.select(&later);
    assert_eq!(next["task_ids"], json!([]), "{next}");
    assert_eq!(
        held_reason(&next, &operator),
        Some("already_landed"),
        "{next}"
    );
    assert_eq!(
        held_reason(&next, &automatic),
        Some("already_landed"),
        "{next}"
    );
}

crate::scenario_table!(
    landed_creation_is_held_without_losing_authority_or_redispatching_scenarios,
    landed_creation_is_held_without_losing_authority_or_redispatching
);
