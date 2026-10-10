//! A branch head that moves under a claimed preparation supersedes only the
//! tasks whose material it changed: their members are claimed afresh at the
//! head, never retried against the frozen source nor retired, and their
//! disjoint siblings still apply.

use orbit_core::application::task::TaskUpdateParams;
use orbit_tools::ReservationOwnerContext;

use super::races::apply_input;
use super::*;

/// Two proposed tasks whose selectors name disjoint tracked files.
fn disjoint_tasks(workspace: &Workspace) -> (Task, Task) {
    workspace.commit_file("a.rs", "fn a() {}\n", "add a");
    workspace.commit_file("b.rs", "fn b() {}\n", "add b");
    let task = |title: &str, selector: &str| {
        workspace
            .runtime
            .add_task(TaskAddParams {
                title: title.into(),
                description: format!("Prepare {title}."),
                acceptance_criteria: vec!["Selectors identify the implementation scope.".into()],
                plan: format!("Inspect {selector}."),
                context_files: vec![selector.into()],
                status: Some(TaskStatus::Proposed),
                ..Default::default()
            })
            .unwrap()
    };
    (
        task("overlapping", "file:a.rs"),
        task("disjoint", "file:b.rs"),
    )
}

fn head(workspace: &Workspace) -> String {
    workspace.git(&["rev-parse", "HEAD"]).trim().to_string()
}

/// Run a claimed deterministic step as the attempt's own run.
pub(super) fn claimed(
    workspace: &Workspace,
    attempt: &MemberAttempt,
    action: &str,
    input: Value,
) -> Value {
    let context = ToolContext {
        reservation_owner: Some(ReservationOwnerContext {
            owner_run_id: attempt.action_id.clone().unwrap(),
            owner_metadata_json: None,
        }),
        ..ToolContext::default()
    };
    workspace
        .runtime
        .run_deterministic(action, &json!({}), &input, context)
        .unwrap_or_else(|error| panic!("{action}: {error}"))
}

/// Prepare `attempt` the way its run does: at the claim's frozen source.
pub(super) fn prepare_claim(workspace: &Workspace, attempt: &MemberAttempt) -> Value {
    claimed(
        workspace,
        attempt,
        "prepare_task_pilot",
        json!({
            "task_ids": attempt.task_ids(), "workspace_path": workspace.repo,
            "base_branch": "main", "state_automation": attempt,
            "source_revision": attempt.member.source.commit,
        }),
    )
}

pub(super) fn apply_claim(
    workspace: &Workspace,
    attempt: &MemberAttempt,
    prepared: &Value,
) -> Value {
    claimed(
        workspace,
        attempt,
        "apply_task_pilot_results",
        apply_input(workspace, prepared),
    )
}

/// Record `steps` as the run's successful step outputs and stop the run.
fn finish_run(workspace: &Workspace, attempt: &MemberAttempt, steps: &[(&str, &Value)]) {
    let run_id = attempt.action_id.clone().unwrap();
    workspace.record_steps(&run_id, steps);
    workspace
        .jobs
        .mark_job_run_running(&run_id, Utc::now(), std::process::id())
        .unwrap();
    workspace
        .jobs
        .finalize_job_run(&run_id, JobRunState::Success, Utc::now(), None)
        .unwrap();
}

fn outcome_for<'a>(output: &'a Value, task: &Task) -> &'a Value {
    output["task_outcomes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|outcome| outcome["task_id"] == task.id)
        .unwrap_or_else(|| panic!("no outcome for {}: {output}", task.id))
}

fn pilot_applied(workspace: &Workspace, task: &Task) -> bool {
    workspace
        .runtime
        .get_task_history(&task.id)
        .unwrap()
        .iter()
        .any(|entry| entry.event == "task_pilot_applied")
}

/// Publish changed task material to origin while leaving the primary branch
/// behind. The initial tracking ref is absent, so selection must fetch it.
fn origin_ahead(workspace: &Workspace) -> (String, String) {
    let local = head(workspace);
    workspace.commit_file("a.rs", "fn a() { supported() }\n", "support a");
    let origin = head(workspace);
    let remote = workspace.root.path().join("origin.git");
    workspace.git(&["clone", "--bare", ".", remote.to_str().unwrap()]);
    workspace.git(&["reset", "--hard", &local]);
    workspace.git(&["remote", "add", "origin", remote.to_str().unwrap()]);
    (local, origin)
}

#[test]
fn a_state_pilot_persists_findings_from_origin_while_the_primary_branch_lags() {
    if !super::super::dispatch_admission::isolated(
        "task_pilot::source_moves::a_state_pilot_persists_findings_from_origin_while_the_primary_branch_lags",
    ) {
        return;
    }
    let workspace = Workspace::new();
    workspace.install_pilot_job();
    let (task, _) = disjoint_tasks(&workspace);
    let (local, origin) = origin_ahead(&workspace);
    let primary_content = std::fs::read_to_string(workspace.repo.join("a.rs")).unwrap();
    let primary_index = workspace.git(&["write-tree"]);
    evaluate_routine(&workspace.runtime, &pilot_routine(), false, Utc::now()).unwrap();
    let attempt = workspace.admitted(&task, 2);
    assert_eq!(attempt.member.source.commit, origin);
    assert_eq!(workspace.object("refs/remotes/origin/main"), origin);

    let prepared = prepare_claim(&workspace, &attempt);
    assert_eq!(prepared["source"]["source_revision"], origin);
    assert_eq!(prepared["task_ids"], json!([task.id]));
    assert_eq!(prepared["superseded_by_source"], json!([]));
    // Stand in for the pilot by inspecting exactly the supplied pinned tree,
    // then persist its finding through the real deterministic apply boundary.
    let revision = prepared["source"]["source_revision"].as_str().unwrap();
    let material = workspace.git(&["show", &format!("{revision}:a.rs")]);
    assert_eq!(material, "fn a() { supported() }\n");
    let mut input = apply_input(&workspace, &prepared);
    let assessment = &mut input["results"][0]["tasks"][0];
    assessment["context_files_after"] = json!(["file:a.rs"]);
    assessment["assessment_rationale"] = json!(format!("At {revision}, a.rs has {material}"));
    let output = claimed(&workspace, &attempt, "apply_task_pilot_results", input);
    assert_eq!(output["status"], "succeeded", "{output}");
    assert_eq!(output["applied_count"], 1, "{output}");
    let comments = workspace.runtime.get_task_comments(&task.id).unwrap();
    let audit = comments.last().unwrap().message.lines().nth(1).unwrap();
    let audit: Value = serde_json::from_str(audit).unwrap();
    assert_eq!(
        audit["assessment"]["assessment_rationale"],
        format!("At {origin}, a.rs has {material}")
    );
    assert_eq!(audit["assessment"]["adr_conflicts"], json!([]));
    assert_eq!(head(&workspace), local);
    assert_eq!(workspace.object("refs/heads/main"), local);
    assert_eq!(workspace.git(&["write-tree"]), primary_index);
    assert_eq!(
        std::fs::read_to_string(workspace.repo.join("a.rs")).unwrap(),
        primary_content
    );
}

#[test]
fn a_failed_state_pilot_fetch_prepares_the_local_head_even_with_a_stale_tracking_ref() {
    if !super::super::dispatch_admission::isolated(
        "task_pilot::source_moves::a_failed_state_pilot_fetch_prepares_the_local_head_even_with_a_stale_tracking_ref",
    ) {
        return;
    }
    let workspace = Workspace::new();
    workspace.install_pilot_job();
    let (task, _) = disjoint_tasks(&workspace);
    let (local, origin) = origin_ahead(&workspace);
    workspace.git(&["fetch", "origin", "main"]);
    workspace.git(&[
        "remote",
        "set-url",
        "origin",
        workspace
            .root
            .path()
            .join("missing-origin.git")
            .to_str()
            .unwrap(),
    ]);
    evaluate_routine(&workspace.runtime, &pilot_routine(), false, Utc::now()).unwrap();
    let attempt = workspace.admitted(&task, 2);
    assert_eq!(attempt.member.source.commit, local);
    assert_eq!(workspace.object("refs/remotes/origin/main"), origin);
    let prepared = prepare_claim(&workspace, &attempt);
    assert_eq!(prepared["source"]["source_revision"], local);
    assert_eq!(prepared["task_ids"], json!([task.id]));
    assert_eq!(prepared["superseded_by_source"], json!([]));
    let output = apply_claim(&workspace, &attempt, &prepared);
    assert_eq!(output["status"], "succeeded", "{output}");
    assert_eq!(output["applied_count"], 1, "{output}");
    assert!(pilot_applied(&workspace, &task));
    assert_eq!(head(&workspace), local);
}

#[test]
fn a_head_move_supersedes_the_overlapping_task_and_reclaims_it_at_the_head() {
    if !super::super::dispatch_admission::isolated(
        "task_pilot::source_moves::a_head_move_supersedes_the_overlapping_task_and_reclaims_it_at_the_head",
    ) {
        return;
    }
    let workspace = Workspace::new();
    workspace.install_pilot_job();
    let (overlapping, disjoint) = disjoint_tasks(&workspace);
    let routine = pilot_routine();
    let now = Utc::now();
    evaluate_routine(&workspace.runtime, &routine, false, now).unwrap();
    let attempt = workspace.admitted_batch(&[&overlapping, &disjoint], 2);
    let at_a = head(&workspace);
    assert_eq!(attempt.member.source.commit, at_a);

    let prepared = prepare_claim(&workspace, &attempt);
    assert_eq!(prepared["superseded_by_source"], json!([]), "{prepared}");
    assert!(prepared["source_age"]["age_seconds"].is_u64(), "{prepared}");
    workspace.commit_file("a.rs", "fn a() { changed() }\n", "touch a");
    let at_b = head(&workspace);

    let output = apply_claim(&workspace, &attempt, &prepared);
    assert_eq!(output["status"], "succeeded", "{output}");
    assert_eq!(output["applied_count"], 1, "{output}");
    assert_eq!(output["superseded_count"], 1, "{output}");
    assert_eq!(
        outcome_for(&output, &overlapping)["reason"],
        "superseded_by_source"
    );
    assert_eq!(outcome_for(&output, &disjoint)["outcome"], "applied");
    assert!(!pilot_applied(&workspace, &overlapping));
    assert!(pilot_applied(&workspace, &disjoint));

    finish_run(
        &workspace,
        &attempt,
        &[("prepare", &prepared), ("apply", &output)],
    );
    // Settle inside the debounce window, so this pass admits nothing.
    evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(1),
    )
    .unwrap();
    let run = workspace
        .runtime
        .show_job_run(attempt.action_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(run.state, JobRunState::Success, "no failed pilot run");
    let members = workspace.routine_state().members.unwrap();
    assert!(members.active.is_none());
    assert_eq!(members.assessed[&disjoint.id].receipt_id, attempt.id);
    assert!(!members.assessed.contains_key(&overlapping.id));
    assert!(
        !members.failed.contains_key(&overlapping.id),
        "a superseded member is not retired at its fingerprint: {:?}",
        members.failed
    );
    assert!(!members.withheld.contains_key(&overlapping.id));
    assert_eq!(members.pending[&overlapping.id].source.commit, at_b);

    // Pending and due again with no edit, at the head.
    let due = evaluate_routine(
        &workspace.runtime,
        &routine,
        true,
        now + Duration::minutes(3),
    )
    .unwrap();
    assert_eq!(due.reason, "would_fire", "{due:?}");
    assert_eq!(due.batch.len(), 1);
    assert_eq!(due.batch[0].task_ids, std::slice::from_ref(&overlapping.id));

    let reclaimed = workspace.admitted(&overlapping, 2);
    assert_eq!(reclaimed.attempt, 1);
    assert_eq!(reclaimed.member.source.commit, at_b);
    let prepared = prepare_claim(&workspace, &reclaimed);
    assert_eq!(prepared["source"]["source_revision"], at_b);
    assert_eq!(prepared["task_ids"], json!([overlapping.id]));
    let output = apply_claim(&workspace, &reclaimed, &prepared);
    assert_eq!(output["status"], "succeeded", "{output}");
    assert_eq!(output["applied_count"], 1, "{output}");
    assert!(pilot_applied(&workspace, &overlapping));
}

#[test]
fn a_run_stopped_after_a_head_move_settles_superseded_without_spending_its_retry() {
    if !super::super::dispatch_admission::isolated(
        "task_pilot::source_moves::a_run_stopped_after_a_head_move_settles_superseded_without_spending_its_retry",
    ) {
        return;
    }
    let workspace = Workspace::new();
    workspace.install_pilot_job();
    let (overlapping, disjoint) = disjoint_tasks(&workspace);
    let routine = pilot_routine();
    let now = Utc::now();
    evaluate_routine(&workspace.runtime, &routine, false, now).unwrap();
    let attempt = workspace.admitted_batch(&[&overlapping, &disjoint], 2);
    workspace.commit_file("a.rs", "fn a() { changed() }\n", "touch a");
    let at_b = head(&workspace);

    // A retry's prepare against the frozen source pilots only what is still
    // fresh there and sets the stale task aside.
    let prepared = prepare_claim(&workspace, &attempt);
    assert_eq!(prepared["task_ids"], json!([disjoint.id]), "{prepared}");
    assert_eq!(
        prepared["superseded_by_source"][0]["task_id"],
        overlapping.id
    );
    assert_eq!(
        prepared["superseded_by_source"][0]["reason"],
        "superseded_by_source"
    );

    // The run stops before apply; it settles instead of retrying.
    workspace
        .jobs
        .finalize_job_run(
            attempt.action_id.as_deref().unwrap(),
            JobRunState::Interrupted,
            Utc::now(),
            None,
        )
        .unwrap();
    evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(1),
    )
    .unwrap();
    let members = workspace.routine_state().members.unwrap();
    assert!(members.active.is_none(), "no retry of the frozen claim");
    assert!(members.failed.is_empty(), "{:?}", members.failed);
    assert!(members.assessed.is_empty());
    for task in [&overlapping, &disjoint] {
        assert_eq!(members.pending[&task.id].source.commit, at_b);
    }
    let due = evaluate_routine(
        &workspace.runtime,
        &routine,
        true,
        now + Duration::minutes(3),
    )
    .unwrap();
    assert_eq!(due.reason, "would_fire", "{due:?}");
    assert_eq!(due.batch.len(), 2, "{due:?}");
}

#[test]
fn context_and_instruction_edits_skip_stale_partitions_and_requeue_without_failures() {
    if !super::super::dispatch_admission::isolated(
        "task_pilot::source_moves::context_and_instruction_edits_skip_stale_partitions_and_requeue_without_failures",
    ) {
        return;
    }
    for instructions in [false, true] {
        let workspace = Workspace::new();
        workspace.install_pilot_job();
        let (edited, sibling) = disjoint_tasks(&workspace);
        let routine = if instructions {
            instructions_routine()
        } else {
            pilot_routine()
        };
        // Prepare/apply resolve a claimed consumer's policy from its durable
        // routine, rather than the definition passed directly to evaluation.
        let routines = workspace.runtime.shared_root().join("routines");
        std::fs::create_dir_all(&routines).unwrap();
        std::fs::write(
            routines.join("fixture-pilot.yaml"),
            serde_yaml::to_string(&routine).unwrap(),
        )
        .unwrap();
        let now = Utc::now();
        evaluate_routine(&workspace.runtime, &routine, false, now).unwrap();
        let attempt = workspace.admitted_batch(&[&edited, &sibling], 2);
        let prepared = claimed(
            &workspace,
            &attempt,
            "prepare_task_pilot",
            json!({
                "task_ids": attempt.task_ids(), "workspace_path": workspace.repo,
                "base_branch": "main", "state_automation": attempt,
                "source_revision": attempt.member.source.commit, "max_partition_size": 1,
            }),
        );
        assert_eq!(prepared["partition_count"], 2, "{prepared}");
        let expected_reason = if instructions {
            // An instruction edit outside both selector paths still invalidates
            // every member when instructions are material (on-call deploy race).
            workspace.commit_file(
                "crates/other/CLAUDE.md",
                "# Updated repository instructions\n",
                "deploy instructions",
            );
            "superseded_by_source"
        } else {
            workspace
                .runtime
                .update_task_as_human(
                    &edited.id,
                    TaskUpdateParams {
                        context_files: Some(vec!["file:README.md".into()]),
                        ..Default::default()
                    },
                    "fixture".into(),
                )
                .unwrap();
            "material_changed"
        };
        let before = workspace.runtime.get_task(&edited.id).unwrap();
        let output = apply_claim(&workspace, &attempt, &prepared);
        let skip_count = if instructions { 2 } else { 1 };
        assert_eq!(output["status"], "succeeded", "{output}");
        assert_eq!(output["outcome"], "superseded", "{output}");
        assert_eq!(output["superseded_count"], skip_count, "{output}");
        assert_eq!(output["applied_count"], 2 - skip_count, "{output}");
        assert_eq!(output["unresolved_count"], 0, "{output}");
        assert_eq!(output["repair_count"], 0, "{output}");
        assert!(output["error"].is_null(), "{output}");
        assert_eq!(outcome_for(&output, &edited)["outcome"], "superseded");
        assert_eq!(outcome_for(&output, &edited)["reason"], expected_reason);
        assert_eq!(output["partition_decisions"][0]["outcome"], "superseded");
        assert_eq!(workspace.runtime.get_task(&edited.id).unwrap(), before);
        assert!(!pilot_applied(&workspace, &edited));
        assert_eq!(pilot_applied(&workspace, &sibling), !instructions);
        assert_eq!(
            workspace.action("pipeline_success_guard", json!({"result": output}))["succeeded"],
            true
        );

        finish_run(
            &workspace,
            &attempt,
            &[("prepare", &prepared), ("apply", &output)],
        );
        evaluate_routine(
            &workspace.runtime,
            &routine,
            false,
            now + Duration::minutes(1),
        )
        .unwrap();
        let run = workspace
            .runtime
            .show_job_run(attempt.action_id.as_deref().unwrap())
            .unwrap();
        assert_eq!(run.state, JobRunState::Success);
        let members = workspace.routine_state().members.unwrap();
        assert!(members.active.is_none(), "no retry of stale preparation");
        assert!(members.failed.is_empty(), "{:?}", members.failed);
        assert!(members.withheld.is_empty(), "{:?}", members.withheld);
        assert!(!members.assessed.contains_key(&edited.id));
        assert_eq!(members.assessed.contains_key(&sibling.id), !instructions);
        assert_eq!(members.pending[&edited.id].source.commit, head(&workspace));
        if !instructions {
            assert_ne!(
                members.pending[&edited.id].fingerprint, attempt.member.fingerprint,
                "the edited context must be prepared afresh"
            );
        }
        let due = evaluate_routine(
            &workspace.runtime,
            &routine,
            true,
            now + Duration::minutes(3),
        )
        .unwrap();
        assert_eq!(due.reason, "would_fire", "{due:?}");
        assert_eq!(due.batch.len(), skip_count);
        assert!(
            due.batch
                .iter()
                .any(|member| member.task_ids == [edited.id.clone()])
        );
        let reclaimed = workspace.admitted(&edited, 2);
        assert_eq!(reclaimed.attempt, 1);
        assert_eq!(reclaimed.member.source.commit, head(&workspace));
        let fresh = prepare_claim(&workspace, &reclaimed);
        assert_eq!(
            fresh["tasks"][0]["context_files_before"],
            json!(before.context_files)
        );
        let applied = apply_claim(&workspace, &reclaimed, &fresh);
        assert_eq!(applied["status"], "succeeded", "{applied}");
        assert_eq!(applied["applied_count"], 1, "{applied}");
        assert!(pilot_applied(&workspace, &edited));
    }
}

#[test]
fn an_instruction_edit_off_every_selector_path_is_ignored_by_default() {
    if !super::super::dispatch_admission::isolated(
        "task_pilot::source_moves::an_instruction_edit_off_every_selector_path_is_ignored_by_default",
    ) {
        return;
    }
    let workspace = Workspace::new();
    workspace.install_pilot_job();
    workspace.commit_file("src/a.rs", "fn a() {}\n", "add a");
    let task = workspace
        .runtime
        .add_task(TaskAddParams {
            title: "nested".into(),
            description: "Prepare nested.".into(),
            acceptance_criteria: vec!["Selectors identify the implementation scope.".into()],
            plan: "Inspect src/a.rs.".into(),
            context_files: vec!["file:src/a.rs".into()],
            status: Some(TaskStatus::Proposed),
            ..Default::default()
        })
        .unwrap();
    let routine = pilot_routine();
    evaluate_routine(&workspace.runtime, &routine, false, Utc::now()).unwrap();
    let attempt = workspace.admitted(&task, 2);
    let prepared = prepare_claim(&workspace, &attempt);
    workspace.commit_file("docs/AGENTS.md", "# Docs agents\n", "docs instructions");

    let output = apply_claim(&workspace, &attempt, &prepared);
    assert_eq!(output["status"], "succeeded", "{output}");
    assert_eq!(output["applied_count"], 1, "{output}");
    assert_eq!(output["superseded_count"], 0, "{output}");

    // An instruction file on the selector's path governs it.
    let task = workspace
        .runtime
        .add_task(TaskAddParams {
            title: "governed".into(),
            description: "Prepare governed.".into(),
            acceptance_criteria: vec!["Selectors identify the implementation scope.".into()],
            plan: "Inspect src/a.rs.".into(),
            context_files: vec!["file:src/a.rs".into()],
            status: Some(TaskStatus::Proposed),
            ..Default::default()
        })
        .unwrap();
    finish_run(
        &workspace,
        &attempt,
        &[("prepare", &prepared), ("apply", &output)],
    );
    evaluate_routine(&workspace.runtime, &routine, false, Utc::now()).unwrap();
    let attempt = workspace.admitted(&task, 2);
    let prepared = prepare_claim(&workspace, &attempt);
    workspace.commit_file("src/AGENTS.md", "# Source agents\n", "src instructions");
    let output = apply_claim(&workspace, &attempt, &prepared);
    assert_eq!(output["applied_count"], 0, "{output}");
    assert_eq!(output["superseded_count"], 1, "{output}");
    assert_eq!(output["task_outcomes"][0]["reason"], "superseded_by_source");
}
