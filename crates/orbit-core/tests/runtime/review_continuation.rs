//! Review interruption and external evidence through runtime/tool boundaries.

use chrono::Utc;
use orbit_core::TaskStatus;
use orbit_engine::{
    ReviewReleaseRequest, ReviewerInvocationRequest, RuntimeHost, TaskAutomationUpdate,
    execute_deterministic_action,
};
use orbit_types::workflow::{
    JobRunState, REVIEW_EVIDENCE_HOLD_ARTIFACT, REVIEW_GATE_ARTIFACT, REVIEW_MANIFEST_ARTIFACT,
    REVIEW_REPORT_ARTIFACT, ReviewAttemptState, ReviewCertificate, ReviewEvidenceHold,
    ReviewManifest, ReviewVerdict, ReviewerInvocationEvent, ValidationOutcome,
};
use serde_json::{Value, json};

use super::review_gate_audit::Fixture;

pub(super) fn attach(fixture: &Fixture, path: &str, content: &Value) {
    let source = fixture.repo.join(".orbit/tmp").join(path);
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, content.to_string()).unwrap();
    fixture
        .runtime
        .run_tool(
            "orbit.task.artifact.put",
            json!({
                "id": fixture.task_id, "model": "codex", "path": path, "source_path": source,
            }),
        )
        .unwrap();
}

/// Attach `content` as an operator does from the bare CLI: a human actor with
/// no agent identity, a writer review evidence accepts [ORB-14530]. `attach`
/// is an agent's put, which never counts as evidence.
pub(super) fn attach_as_operator(fixture: &Fixture, path: &str, content: &Value) {
    fixture
        .runtime
        .clone()
        .with_actor(orbit_core::ActorIdentity::human("human:operator"))
        .update_task_with_identity(
            &fixture.task_id,
            orbit_core::application::task::TaskUpdateParams {
                upsert_artifacts: vec![orbit_types::task::TaskArtifact {
                    path: path.into(),
                    media_type: "application/json".into(),
                    content: content.to_string().into_bytes(),
                    created_by: None,
                }],
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
}

pub(super) fn interrupted_report(fixture: &Fixture) -> Value {
    json!({
        "schema_version": 1, "attempt_id": fixture.input["admission"]["attempt_id"],
        "verdict": "incomplete", "summary": "Inspected error paths; platform checks remain.",
        "findings": [],
        "validation": [{"id": "V1", "command": "fixture check", "outcome": "passed", "role": "required"}],
        "escalation": "External checks pending",
    })
}

fn fresh_review(fixture: &mut Fixture, hold: &ReviewEvidenceHold) -> String {
    let previous = fixture.runtime.show_job_run(&hold.run_id).unwrap();
    let next = fixture
        .runtime
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), previous.input, None)
        .unwrap();
    fixture
        .runtime
        .update_task_with_identity(
            &fixture.task_id,
            orbit_core::application::task::TaskUpdateParams {
                status: Some(TaskStatus::InProgress),
                job_run_id: Some(Some(next.run_id.clone())),
                ..Default::default()
            },
            Some("codex".into()),
            None,
        )
        .unwrap();
    fixture.input["job_run_id"] = json!(next.run_id);
    fixture.admit();
    assert_ne!(fixture.input["admission"]["attempt_id"], hold.attempt_id);
    next.run_id
}

pub(super) fn manifest(fixture: &Fixture) -> ReviewManifest {
    serde_json::from_slice(
        &fixture
            .runtime
            .get_task_artifact(&fixture.task_id, REVIEW_MANIFEST_ARTIFACT)
            .unwrap()
            .unwrap()
            .content,
    )
    .unwrap()
}

/// Run admission and settlement in the persisted worker, with every recovery
/// hook configured. Reports still arrive through the reviewer's artifact tool.
pub(super) fn run_review_pipeline(fixture: &Fixture) {
    // Shipped job names resolve from the fixture's global catalog.
    let resources = fixture.runtime.paths().global_dir.join("resources");
    std::fs::create_dir_all(resources.join("jobs")).unwrap();
    std::fs::create_dir_all(resources.join("activities")).unwrap();
    std::fs::write(resources.join("activities/unexpected_recovery.yaml"), json!({
        "schemaVersion": 2, "kind": "Activity", "metadata": {"name": "unexpected_recovery"},
        "spec": {"type": "deterministic", "description": "Record any unexpected recovery or publication",
            "input_schema_json": {}, "output_schema_json": {},
            "action": "orbit_tool_call", "config": {
            "tool_name": "orbit.task.update",
            "args": {"id": fixture.task_id, "model": "codex", "comment": "Unexpected recovery ran"},
        }},
    }).to_string()).unwrap();
    let mut settle_input = fixture.input.clone();
    settle_input["admission"] = json!("{{ steps.review_gate_admit.output }}");
    std::fs::write(
        resources.join("jobs/task_pr_pipeline.yaml"),
        json!({
            "schemaVersion": 2, "kind": "Job", "metadata": {"name": "task_pr_pipeline"},
            "spec": {
                "state": "enabled", "kind": "workflow",
                "failure_activity": "unexpected_recovery",
                "final_recovery_activity": "unexpected_recovery",
                "steps": [{
                    "id": "review_gate_admit", "default_input": fixture.input,
                    "spec": {"type": "deterministic", "action": "review_gate_admit", "config": {}},
                }, {
                    "id": "review_gate_settle", "default_input": settle_input,
                    "retry": {"max_attempts": 3, "initial_backoff_ms": 1, "backoff_cap_ms": 1},
                    "recovery_activity": "unexpected_recovery",
                    "spec": {"type": "deterministic", "action": "review_gate_settle", "config": {}},
                }, {
                    "id": "publish", "target": "activity:unexpected_recovery",
                }],
            },
        })
        .to_string(),
    )
    .unwrap();
    fixture
        .runtime
        .execute_pipeline_run_worker(fixture.input["job_run_id"].as_str().unwrap())
        .unwrap();
}

/// [ORB-15094] The manifest advertises the deadline the reviewer process is
/// given, for a `review.minutes` below, at and above the activity's 3600 s wall
/// clock. The deadline is all the seconds the review has left.
#[test]
fn the_manifest_advertises_the_deadline_the_host_hands_the_reviewer() {
    if !super::dispatch_admission::isolated(
        "review_continuation::the_manifest_advertises_the_deadline_the_host_hands_the_reviewer",
    ) {
        return;
    }
    for (minutes, deadline_seconds) in [(30_u32, 1800_u64), (60, 3600), (120, 7200)] {
        let mut fixture = Fixture::new_with_review_minutes(minutes);
        fixture.admit();
        RuntimeHost::mark_job_run_running(
            &fixture.runtime,
            fixture.input["job_run_id"].as_str().unwrap(),
            Utc::now(),
            std::process::id(),
        )
        .unwrap();
        let advertised = manifest(&fixture);
        assert_eq!(advertised.budget.minutes, minutes);
        let bound = RuntimeHost::record_reviewer_invocation(
            &fixture.runtime,
            &ReviewerInvocationRequest {
                run_id: fixture.input["job_run_id"].as_str().unwrap().into(),
                lineage_key: fixture.input["admission"]["lineage_key"]
                    .as_str()
                    .unwrap()
                    .into(),
                attempt_id: fixture.input["admission"]["attempt_id"]
                    .as_str()
                    .unwrap()
                    .into(),
                event: ReviewerInvocationEvent::Started,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            bound, deadline_seconds,
            "review.minutes = {minutes}: the reviewer's bound is not capped by the activity's wall clock"
        );
        assert_eq!(
            advertised.remaining.seconds, bound,
            "review.minutes = {minutes}: the manifest must advertise the deadline the reviewer is given"
        );
        assert_eq!(
            fixture.input["admission"]["remaining"]["seconds"], bound,
            "review.minutes = {minutes}: admission reports the same deadline"
        );
    }
}

#[test]
fn timeout_retains_partial_report_and_budget_and_resumes_the_same_review() {
    if !super::dispatch_admission::isolated(
        "review_continuation::timeout_retains_partial_report_and_budget_and_resumes_the_same_review",
    ) {
        return;
    }
    let mut fixture = Fixture::new();
    fixture.admit();
    RuntimeHost::mark_job_run_running(
        &fixture.runtime,
        fixture.input["job_run_id"].as_str().unwrap(),
        Utc::now(),
        std::process::id(),
    )
    .unwrap();
    let partial = interrupted_report(&fixture);
    fixture.put_report(&partial);
    let request = |event| ReviewerInvocationRequest {
        run_id: fixture.input["job_run_id"].as_str().unwrap().into(),
        lineage_key: fixture.input["admission"]["lineage_key"]
            .as_str()
            .unwrap()
            .into(),
        attempt_id: fixture.input["admission"]["attempt_id"]
            .as_str()
            .unwrap()
            .into(),
        event,
    };
    let bound = RuntimeHost::record_reviewer_invocation(
        &fixture.runtime,
        &request(ReviewerInvocationEvent::Started),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        bound, 600,
        "the reviewer may use all of the captured budget"
    );
    // The reviewer is cut short before its deadline; a reviewer that ran to
    // the deadline would have spent the review's minutes.
    let ran = bound / 2;
    let finished = request(ReviewerInvocationEvent::TimedOut {
        runtime_seconds: ran,
    });
    RuntimeHost::record_reviewer_invocation(&fixture.runtime, &finished).unwrap();
    RuntimeHost::release_review_attempt(
        &fixture.runtime,
        &ReviewReleaseRequest {
            run_id: finished.run_id.clone(),
            lineage_key: finished.lineage_key.clone(),
            attempt_id: finished.attempt_id.clone(),
        },
    )
    .unwrap();
    let ledger = fixture
        .runtime
        .review_store()
        .unwrap()
        .review_ledger(
            &fixture.runtime.workspace_id().unwrap(),
            &finished.lineage_key,
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        ledger.attempts[0].state,
        ReviewAttemptState::Settled {
            verdict: ReviewVerdict::Incomplete
        }
    );
    assert_eq!(
        ledger.consumed_seconds, ran,
        "only runtime actually spent counts"
    );
    assert!(ledger.remaining_at(Utc::now()).seconds > 0);
    let kept = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, REVIEW_REPORT_ARTIFACT)
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&kept.content).unwrap(),
        partial
    );
    RuntimeHost::apply_task_automation_update(
        &fixture.runtime,
        &fixture.task_id,
        TaskAutomationUpdate {
            status: Some(TaskStatus::Backlog),
            status_event: Some("review_timeout_incomplete".into()),
            status_note: Some(format!("run={}, reviewer timed out", finished.run_id)),
            ..Default::default()
        },
    )
    .unwrap();
    RuntimeHost::finalize_job_run(
        &fixture.runtime,
        &finished.run_id,
        JobRunState::Failed,
        Utc::now(),
        None,
    )
    .unwrap();
    assert_eq!(
        fixture.runtime.get_task(&fixture.task_id).unwrap().status,
        TaskStatus::Backlog
    );
    fixture.admit();
    assert_eq!(fixture.input["admission"]["decision"], "resumed");
    assert_eq!(
        fixture.input["admission"]["attempt_id"],
        finished.attempt_id
    );
    let manifest = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, "review-manifest.json")
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&manifest.content).unwrap()["previous_report"],
        partial
    );
    let mut completed = partial;
    completed["verdict"] = json!("accept");
    completed["escalation"] = Value::Null;
    fixture.put_report(&completed);
    assert_eq!(fixture.settle().unwrap()["gate"], "passed");
}

fn git(repo: &std::path::Path, args: &[&str]) -> String {
    let mut command = std::process::Command::new("git");
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let output = command.args(args).current_dir(repo).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// Give the fixture repository a bare `origin` so the failure handoff can push.
fn add_origin(fixture: &Fixture) {
    let remote = fixture._root.path().join("remote.git");
    git(&fixture.repo, &["init", "--bare", remote.to_str().unwrap()]);
    git(
        &fixture.repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
}

/// Start a fresh delivery run for the task, as a requeue does.
fn start_fresh_run(fixture: &mut Fixture) {
    let previous = fixture
        .runtime
        .show_job_run(fixture.input["job_run_id"].as_str().unwrap())
        .unwrap();
    let next = fixture
        .runtime
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), previous.input, None)
        .unwrap();
    assert!(next.retry_source_run_id.is_none());
    assert_ne!(next.run_id, previous.run_id);
    fixture
        .runtime
        .update_task_with_identity(
            &fixture.task_id,
            orbit_core::application::task::TaskUpdateParams {
                status: Some(TaskStatus::InProgress),
                job_run_id: Some(Some(next.run_id.clone())),
                ..Default::default()
            },
            Some("codex".into()),
            None,
        )
        .unwrap();
    fixture.input["job_run_id"] = json!(next.run_id);
}

/// Admit the current run, time its reviewer out, and run the failure handoff.
/// Returns the handoff output; the caller finalizes the run.
fn reviewer_timeout_handoff(fixture: &mut Fixture) -> Value {
    fixture.admit();
    RuntimeHost::mark_job_run_running(
        &fixture.runtime,
        fixture.input["job_run_id"].as_str().unwrap(),
        Utc::now(),
        std::process::id(),
    )
    .unwrap();
    fixture.put_report(&interrupted_report(fixture));
    let request = |event| ReviewerInvocationRequest {
        run_id: fixture.input["job_run_id"].as_str().unwrap().into(),
        lineage_key: fixture.input["admission"]["lineage_key"]
            .as_str()
            .unwrap()
            .into(),
        attempt_id: fixture.input["admission"]["attempt_id"]
            .as_str()
            .unwrap()
            .into(),
        event,
    };
    let bound = RuntimeHost::record_reviewer_invocation(
        &fixture.runtime,
        &request(ReviewerInvocationEvent::Started),
    )
    .unwrap()
    .unwrap();
    RuntimeHost::record_reviewer_invocation(
        &fixture.runtime,
        &request(ReviewerInvocationEvent::TimedOut {
            runtime_seconds: bound,
        }),
    )
    .unwrap();
    execute_deterministic_action(&fixture.runtime, "pr_failure_handoff", &json!({}), &json!({
        "failed_step_id": "review", "error_code": "deterministic_action_refused",
        "error_message": "review_timeout_incomplete: reviewer exceeded its wall clock",
        "run_id": fixture.input["job_run_id"],
        "job_input": {"task_ids": [fixture.task_id], "base_branch": "main", "base_sync": "local"},
        "pipeline": {
            "worktree": {"job_run_id": fixture.input["job_run_id"], "workspace_path": fixture.repo},
            "sync_base": {"base": "main", "base_ref": "main"},
            "review_gate_admit": fixture.input["admission"],
        },
    }), false, &Default::default(), None).unwrap()
}

fn finalize_failed_run(fixture: &Fixture) {
    RuntimeHost::finalize_job_run(
        &fixture.runtime,
        fixture.input["job_run_id"].as_str().unwrap(),
        JobRunState::Failed,
        Utc::now(),
        None,
    )
    .unwrap();
}

/// The latest status decision recorded in task history.
fn latest_decision(fixture: &Fixture) -> orbit_types::task::TaskHistoryEntry {
    fixture
        .runtime
        .get_task_history(&fixture.task_id)
        .unwrap()
        .into_iter()
        .rev()
        .find(|entry| entry.to_status.is_some())
        .unwrap()
}

#[test]
fn fresh_run_timeouts_are_bounded_by_task_and_tree_not_commit_or_lineage() {
    if !super::dispatch_admission::isolated(
        "review_continuation::fresh_run_timeouts_are_bounded_by_task_and_tree_not_commit_or_lineage",
    ) {
        return;
    }
    let mut fixture = Fixture::new();
    let repo = fixture.repo.clone();
    add_origin(&fixture);
    let initial_commit = git(&repo, &["rev-parse", "HEAD"]);
    let initial_tree = git(&repo, &["rev-parse", "HEAD^{tree}"]);
    let mut previous_lineage = Value::Null;
    let mut previous_commit = initial_commit.clone();

    // Re-committing the same tree, then changing and restoring it, must not
    // erase the earlier timeout's durable bound (F2026-10-131).
    for (index, expected_status) in [
        TaskStatus::Backlog,
        TaskStatus::Blocked,
        TaskStatus::Backlog,
        TaskStatus::Blocked,
    ]
    .into_iter()
    .enumerate()
    {
        if index > 0 {
            match index {
                1 => {
                    git(
                        &repo,
                        &["commit", "--allow-empty", "-m", "rewritten candidate"],
                    );
                }
                2 => {
                    std::fs::write(repo.join("candidate.txt"), "repaired\n").unwrap();
                    git(&repo, &["add", "candidate.txt"]);
                    git(&repo, &["commit", "-m", "changed candidate tree"]);
                }
                _ => {
                    git(
                        &repo,
                        &["restore", "--source", &initial_commit, "candidate.txt"],
                    );
                    git(&repo, &["add", "candidate.txt"]);
                    git(&repo, &["commit", "-m", "restore original candidate tree"]);
                }
            }
            start_fresh_run(&mut fixture);
        }
        let handoff = reviewer_timeout_handoff(&mut fixture);
        assert_ne!(
            fixture.input["admission"]["lineage_key"], previous_lineage,
            "each fresh run starts a new review lineage"
        );
        previous_lineage = fixture.input["admission"]["lineage_key"].clone();
        assert_eq!(handoff["pr_created"], false);
        if index != 2 {
            assert_eq!(handoff["candidate_tree"], initial_tree);
        } else {
            assert_ne!(handoff["candidate_tree"], initial_tree);
        }
        if index > 0 {
            assert_ne!(handoff["head_sha"], previous_commit);
        }
        previous_commit = handoff["head_sha"].as_str().unwrap().into();
        let decision = latest_decision(&fixture);
        assert_eq!(decision.to_status, Some(expected_status));
        assert_eq!(
            decision.event,
            if expected_status == TaskStatus::Blocked {
                "review_timeout_requeue_exhausted"
            } else {
                "review_timeout_incomplete"
            },
            "F2026-10-131: the second timeout on a tree must stop automatic requeues"
        );
        if expected_status == TaskStatus::Blocked {
            assert!(
                decision
                    .note
                    .as_deref()
                    .is_some_and(|note| !note.trim().is_empty()),
                "a blocked timeout must retain a reason"
            );
        }
        finalize_failed_run(&fixture);
        assert_eq!(
            fixture.runtime.get_task(&fixture.task_id).unwrap().status,
            expected_status
        );
    }
}

/// The reviewer's own partial commit changes the candidate tree on every
/// timeout, so the bound must key on the implementation tree beneath it
/// [ORB-14808].
#[test]
fn reviewer_partial_repairs_do_not_renew_the_timeout_allowance() {
    if !super::dispatch_admission::isolated(
        "review_continuation::reviewer_partial_repairs_do_not_renew_the_timeout_allowance",
    ) {
        return;
    }
    let mut fixture = Fixture::new();
    let repo = fixture.repo.clone();
    add_origin(&fixture);
    let implementation_tree = git(&repo, &["rev-parse", "HEAD^{tree}"]);
    let mut candidate_trees = Vec::new();

    for (index, expected_status) in [TaskStatus::Backlog, TaskStatus::Blocked]
        .into_iter()
        .enumerate()
    {
        if index > 0 {
            start_fresh_run(&mut fixture);
        }
        // The reviewer timed out after writing a file it never committed.
        std::fs::write(
            repo.join(format!("reviewer-{index}.txt")),
            format!("partial repair {index}\n"),
        )
        .unwrap();
        let handoff = reviewer_timeout_handoff(&mut fixture);
        assert!(
            !handoff["partial_repair_commit"].is_null(),
            "the reviewer's file must be preserved as a partial-repair commit"
        );
        assert_eq!(handoff["implementation_tree"], implementation_tree);
        assert_ne!(handoff["candidate_tree"], implementation_tree);
        candidate_trees.push(handoff["candidate_tree"].clone());
        let decision = latest_decision(&fixture);
        assert_eq!(
            decision.to_status,
            Some(expected_status),
            "{index}: {decision:?}"
        );
        assert_eq!(
            decision.event,
            if expected_status == TaskStatus::Blocked {
                "review_timeout_requeue_exhausted"
            } else {
                "review_timeout_incomplete"
            },
            "ORB-14808: a second timeout on one implementation tree must block even when \
             each timeout committed different partial reviewer repairs"
        );
        finalize_failed_run(&fixture);
    }
    assert_ne!(
        candidate_trees[0], candidate_trees[1],
        "the fixture must exercise distinct partial-repair trees"
    );
}

#[test]
fn named_external_checks_hold_until_every_matching_result_and_log_arrives() {
    if !super::dispatch_admission::isolated(
        "review_continuation::named_external_checks_hold_until_every_matching_result_and_log_arrives",
    ) {
        return;
    }
    for verdict in ["incomplete", "changes_required"] {
        let mut fixture =
            Fixture::new_with_required_commands(&["hosted windows", "native macos", "codeql"]);
        fixture.admit();
        let mut report = interrupted_report(&fixture);
        let requirements = json!([
            {"kind": "hosted_ci", "name": "Windows CI job", "command": "hosted windows", "artifact": "evidence/windows.json"},
            {"kind": "native_os", "name": "macOS native run", "command": "native macos", "artifact": "evidence/macos.json"},
            {"kind": "codeql", "name": "Rust CodeQL extraction", "command": "codeql", "artifact": "evidence/codeql.json"},
        ]);
        report["verdict"] = json!(verdict);
        report["external_evidence"] = requirements.clone();
        for (index, command) in ["hosted windows", "native macos", "codeql"]
            .iter()
            .enumerate()
        {
            report["validation"].as_array_mut().unwrap().push(json!({
                "id": format!("V{}", index + 2),
                "command": command, "outcome": "not_run", "role": "required",
            }));
        }
        fixture.put_report(&report);
        run_review_pipeline(&fixture);
        let hold: ReviewEvidenceHold = serde_json::from_slice(
            &fixture
                .runtime
                .get_task_artifact(&fixture.task_id, REVIEW_EVIDENCE_HOLD_ARTIFACT)
                .unwrap()
                .unwrap()
                .content,
        )
        .unwrap();
        let run = fixture.runtime.show_job_run(&hold.run_id).unwrap();
        assert_eq!(
            run.state,
            JobRunState::Held,
            "ORB-14313: evidence holds must not fail delivery"
        );
        assert!(run.finished_at.is_some());
        let terminal_runs = fixture
            .runtime
            .list_job_runs_observed(orbit_core::application::job::JobRunListParams {
                terminal_only: true,
                ..Default::default()
            })
            .unwrap();
        assert!(
            terminal_runs
                .iter()
                .any(|candidate| candidate.run_id == hold.run_id),
            "held delivery runs must appear in terminal-only history"
        );
        let wait = fixture
            .runtime
            .wait_pipeline_runs(std::slice::from_ref(&hold.run_id), 1, 1, None)
            .unwrap();
        assert_eq!(wait.results[0].status, "held");
        assert!(wait.results[0].error.is_none());
        let reliability = fixture
            .runtime
            .pipeline_reliability(
                &orbit_core::metrics::reliability::ReliabilityWindow::ending_at(
                    "fixture",
                    Utc::now() + chrono::Duration::seconds(1),
                    chrono::Duration::minutes(10),
                ),
            )
            .unwrap();
        assert_eq!(reliability.job_runs.overall.held, 1);
        assert_eq!(reliability.job_runs.overall.failed, 0);
        assert_eq!(reliability.job_runs.overall.excluded(), 1);
        assert!(
            run.steps
                .iter()
                .all(|step| step.state != JobRunState::Failed)
        );
        let events = fixture
            .runtime
            .collect_run_audit_events(&hold.run_id)
            .unwrap();
        assert!(
            !events.iter().any(|event| matches!(
                event.body_kind.as_deref(),
                Some("step_retry" | "step_recovery_attempted" | "final_recovery_attempted")
            )),
            "ORB-14313: evidence holds must bypass retry and both recovery stages"
        );
        let steps = fixture
            .runtime
            .collect_run_audit_steps(&hold.run_id)
            .unwrap();
        assert_eq!(
            steps.len(),
            2,
            "hold must stop before publication and failure handoff"
        );
        assert_eq!(steps[1].state.as_deref(), Some("held"));
        assert_eq!(
            fixture
                .runtime
                .get_task_history(&fixture.task_id)
                .unwrap()
                .last()
                .unwrap()
                .event,
            "review_awaiting_evidence"
        );
        fixture
            .runtime
            .execute_pipeline_run_worker(&hold.run_id)
            .unwrap();
        assert_eq!(
            fixture.runtime.show_job_run(&hold.run_id).unwrap().state,
            JobRunState::Held
        );
        assert_eq!(
            fixture.runtime.get_task(&fixture.task_id).unwrap().status,
            TaskStatus::InProgress
        );
        assert!(
            fixture
                .runtime
                .run_deterministic(
                    "review_gate_admit",
                    &json!({}),
                    &fixture.input,
                    Default::default()
                )
                .is_err()
        );
        attach(&fixture, "unrelated.json", &json!({"outcome": "passed"}));
        for (index, requirement) in hold.requirements.iter().enumerate() {
            let log = format!("evidence/log-{index}.json");
            let mut evidence = json!({
                "schema_version": 1, "attempt_id": hold.attempt_id, "candidate": hold.candidate,
                "kind": requirement.kind, "name": requirement.name, "command": requirement.command,
                "outcome": "passed", "log_artifact": log,
            });
            if index == 0 {
                let candidate = evidence["candidate"].clone();
                evidence["candidate"]["tree"] = json!("different-tree");
                attach_as_operator(&fixture, &requirement.artifact, &evidence);
                attach_as_operator(
                    &fixture,
                    &log,
                    &json!({"captured_output": "passing external check"}),
                );
                assert_eq!(
                    fixture.runtime.get_task(&fixture.task_id).unwrap().status,
                    TaskStatus::InProgress
                );
                evidence["candidate"] = candidate;
            }
            if index + 1 == hold.requirements.len() {
                attach_as_operator(
                    &fixture,
                    &log,
                    &json!({"captured_output": "passing external check"}),
                );
                // All other requirements and this log are satisfied, so each
                // refusal exercises the changed field rather than a missing
                // prerequisite masking an incorrect evidence match.
                for (field, wrong) in [
                    ("kind", json!("native_os")),
                    ("command", json!("another check")),
                    ("schema_version", json!(2)),
                    ("outcome", json!("failed")),
                    ("log_artifact", json!("evidence/missing-log.json")),
                    ("log_artifact", json!(requirement.artifact)),
                    (
                        "log_artifact",
                        json!(format!(" {}/ ", requirement.artifact)),
                    ),
                    ("log_artifact", json!(REVIEW_EVIDENCE_HOLD_ARTIFACT)),
                    (
                        "log_artifact",
                        json!(format!(" {REVIEW_EVIDENCE_HOLD_ARTIFACT}")),
                    ),
                    ("log_artifact", json!(format!("{REVIEW_GATE_ARTIFACT}/"))),
                ] {
                    let mut invalid = evidence.clone();
                    invalid[field] = wrong;
                    attach_as_operator(&fixture, &requirement.artifact, &invalid);
                    assert_eq!(
                        fixture.runtime.get_task(&fixture.task_id).unwrap().status,
                        TaskStatus::InProgress,
                        "mismatched {field} must not satisfy the last requirement"
                    );
                }
            }
            attach_as_operator(&fixture, &requirement.artifact, &evidence);
            if index != 0 && index + 1 != hold.requirements.len() {
                assert_eq!(
                    fixture.runtime.get_task(&fixture.task_id).unwrap().status,
                    TaskStatus::InProgress,
                    "a result without its attached log cannot release the hold"
                );
                attach_as_operator(
                    &fixture,
                    &log,
                    &json!({"captured_output": "passing external check"}),
                );
            }
            assert_eq!(
                fixture.runtime.get_task(&fixture.task_id).unwrap().status,
                if index + 1 == hold.requirements.len() {
                    TaskStatus::Backlog
                } else {
                    TaskStatus::InProgress
                }
            );
        }
        // Receipt is a requeue, never review approval or PR publication.
        assert!(fixture.settle().is_err());
        assert_eq!(
            fixture
                .runtime
                .get_task_history(&fixture.task_id)
                .unwrap()
                .last()
                .unwrap()
                .event,
            "review_evidence_received"
        );
        // A new delivery run reviews the candidate afresh. Evidence receipt
        // itself neither approves it nor rewrites the incomplete certificate.
        // ORB-14328: a fresh reviewer saw a new commit on the evidenced tree,
        // then requested the same unavailable check under its new attempt.
        let mut command = std::process::Command::new("git");
        orbit_common::test_env::clear_inherited_authority(|key| {
            command.env_remove(key);
        });
        let committed = command
            .args(["commit", "--allow-empty", "-m", "same tree, fresh commit"])
            .current_dir(&fixture.repo)
            .output()
            .unwrap();
        assert!(committed.status.success(), "{committed:?}");
        let next_run = fresh_review(&mut fixture, &hold);
        let input = manifest(&fixture);
        assert_ne!(input.candidate.commit, hold.candidate.commit);
        assert_eq!(input.candidate.tree, hold.candidate.tree);
        assert_eq!(
            input.satisfied_external_evidence.len(),
            hold.requirements.len()
        );
        for requirement in &hold.requirements {
            let result = &input.satisfied_external_evidence[&requirement.artifact];
            assert_eq!(result.attempt_id, hold.attempt_id);
            assert_eq!(result.candidate, hold.candidate);
        }
        report["attempt_id"] = fixture.input["admission"]["attempt_id"].clone();
        // Names and result paths are locators, not evidence identity.
        report["external_evidence"][0]["name"] = json!("Renamed Windows job");
        report["external_evidence"][0]["artifact"] = json!("evidence/renamed-windows.json");
        fixture.put_report(&report);
        run_review_pipeline(&fixture);
        assert_eq!(
            fixture.runtime.show_job_run(&next_run).unwrap().state,
            JobRunState::Success
        );
        assert_eq!(
            fixture
                .runtime
                .read_run_state(&next_run)
                .unwrap()
                .unwrap()
                .pipeline["review_gate_settle"]["gate"],
            "passed"
        );
        let certificate: ReviewCertificate = serde_json::from_slice(
            &fixture
                .runtime
                .get_task_artifact(&fixture.task_id, REVIEW_GATE_ARTIFACT)
                .unwrap()
                .unwrap()
                .content,
        )
        .unwrap();
        assert_eq!(certificate.verdict, ReviewVerdict::Accept);
        assert_eq!(certificate.final_candidate, input.candidate);
        assert!(
            certificate.repair_commits.is_empty(),
            "an unchanged review must not mint a commit"
        );
        assert!(
            certificate
                .validation
                .iter()
                .all(|record| record.outcome == ValidationOutcome::Passed)
        );
    }
}

#[test]
fn received_external_evidence_does_not_cover_a_reviewer_repair_on_another_tree() {
    if !super::dispatch_admission::isolated(
        "review_continuation::received_external_evidence_does_not_cover_a_reviewer_repair_on_another_tree",
    ) {
        return;
    }
    let mut fixture = Fixture::new_with_required_commands(&["native macos"]);
    fixture.admit();
    let mut report = interrupted_report(&fixture);
    report["external_evidence"] = json!([{
        "kind": "native_os", "name": "macOS run", "command": "native macos",
        "artifact": "evidence/macos.json",
    }]);
    report["validation"].as_array_mut().unwrap().push(json!({
        "id": "V2", "command": "native macos", "outcome": "not_run", "role": "required",
    }));
    fixture.put_report(&report);
    run_review_pipeline(&fixture);
    let hold: ReviewEvidenceHold = serde_json::from_slice(
        &fixture
            .runtime
            .get_task_artifact(&fixture.task_id, REVIEW_EVIDENCE_HOLD_ARTIFACT)
            .unwrap()
            .unwrap()
            .content,
    )
    .unwrap();
    attach_as_operator(
        &fixture,
        "evidence/macos.json",
        &json!({
            "schema_version": 1, "attempt_id": hold.attempt_id, "candidate": hold.candidate,
            "kind": "native_os", "name": "macOS run", "command": "native macos",
            "outcome": "passed", "log_artifact": "evidence/macos-log.json",
        }),
    );
    attach_as_operator(
        &fixture,
        "evidence/macos-log.json",
        &json!({"output": "passed"}),
    );
    assert_eq!(
        fixture.runtime.get_task(&fixture.task_id).unwrap().status,
        TaskStatus::Backlog
    );
    let next_run = fresh_review(&mut fixture, &hold);
    assert_eq!(manifest(&fixture).satisfied_external_evidence.len(), 1);
    // The admission snapshot covers the old tree, but settlement must re-read
    // against the repaired tree before resolving the repeated requirement.
    std::fs::write(fixture.repo.join("candidate.txt"), "reviewer repaired\n").unwrap();
    report["attempt_id"] = fixture.input["admission"]["attempt_id"].clone();
    report["findings"] = json!([{
        "id": "F1", "severity": "high", "summary": "Repair candidate",
        "paths": ["candidate.txt"], "disposition": {"kind": "repaired"},
        "change": "Repaired candidate behavior",
    }]);
    fixture.put_report(&report);
    run_review_pipeline(&fixture);
    assert_eq!(
        fixture.runtime.show_job_run(&next_run).unwrap().state,
        JobRunState::Held
    );
    let changed: ReviewEvidenceHold = serde_json::from_slice(
        &fixture
            .runtime
            .get_task_artifact(&fixture.task_id, REVIEW_EVIDENCE_HOLD_ARTIFACT)
            .unwrap()
            .unwrap()
            .content,
    )
    .unwrap();
    assert_ne!(changed.candidate.tree, hold.candidate.tree);
    attach(
        &fixture,
        "unrelated.json",
        &json!({"output": "old evidence still attached"}),
    );
    assert_eq!(
        fixture.runtime.get_task(&fixture.task_id).unwrap().status,
        TaskStatus::InProgress
    );
    assert!(
        fixture
            .runtime
            .run_deterministic(
                "review_gate_admit",
                &json!({}),
                &fixture.input,
                Default::default(),
            )
            .is_err(),
        "a different tree must still wait for its own evidence"
    );
}

#[test]
fn external_requirement_cannot_hide_a_reject_open_defect_or_failed_local_check() {
    if !super::dispatch_admission::isolated(
        "review_continuation::external_requirement_cannot_hide_a_reject_open_defect_or_failed_local_check",
    ) {
        return;
    }
    for case in [
        "reject",
        "open_defect",
        "failed_check",
        "unnamed_check",
        "meaning_changed",
    ] {
        let mut fixture = Fixture::new();
        fixture.admit();
        let mut report = interrupted_report(&fixture);
        report["external_evidence"] = json!([{ "kind": "hosted_ci", "name": "Windows CI",
            "command": "hosted windows", "artifact": "evidence/windows.json" }]);
        report["validation"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id": "V2", "command": "hosted windows", "outcome": "not_run"}));
        match case {
            "reject" => {
                report["verdict"] = json!("changes_required");
                report["findings"] = json!([{"id": "F1", "summary": "Wrong approach", "severity": "high", "disposition": "open"}]);
            }
            "open_defect" => {
                report["findings"] = json!([{"id": "F1", "summary": "Wrong approach", "severity": "high", "disposition": "open"}])
            }
            "failed_check" => report["validation"][0]["outcome"] = json!("failed"),
            "unnamed_check" => report["validation"]
                .as_array_mut()
                .unwrap()
                .push(json!({"id": "V3", "command": "local required", "outcome": "not_run"})),
            "meaning_changed" => {
                fixture
                    .runtime
                    .update_task_with_identity(
                        &fixture.task_id,
                        orbit_core::application::task::TaskUpdateParams {
                            description: Some("Changed intent".into()),
                            ..Default::default()
                        },
                        Some("codex".into()),
                        None,
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }
        fixture.put_report(&report);
        attach_as_operator(
            &fixture,
            "evidence/windows.json",
            &json!({
                "schema_version": 1, "attempt_id": fixture.input["admission"]["attempt_id"],
                "candidate": manifest(&fixture).candidate, "kind": "hosted_ci",
                "name": "Windows CI", "command": "hosted windows", "outcome": "passed",
                "log_artifact": "evidence/windows-log.json",
            }),
        );
        attach_as_operator(
            &fixture,
            "evidence/windows-log.json",
            &json!({"output": "passed"}),
        );
        let refused = fixture.settle().unwrap_err();
        assert!(
            refused.to_string().contains("review_gate_blocked:"),
            "{case}: {refused}"
        );
        assert!(
            fixture
                .runtime
                .get_task_artifact(&fixture.task_id, REVIEW_EVIDENCE_HOLD_ARTIFACT)
                .unwrap()
                .is_none(),
            "{case}"
        );
    }
}

#[test]
fn a_late_review_handoff_cannot_overwrite_an_operator_block() {
    if !super::dispatch_admission::isolated(
        "review_continuation::a_late_review_handoff_cannot_overwrite_an_operator_block",
    ) {
        return;
    }
    let fixture = Fixture::new();
    fixture
        .runtime
        .update_task_with_identity(
            &fixture.task_id,
            orbit_core::application::task::TaskUpdateParams {
                status: Some(TaskStatus::Blocked),
                comment: Some("Operator holds delivery".into()),
                ..Default::default()
            },
            Some("codex".into()),
            None,
        )
        .unwrap();
    let refused = RuntimeHost::apply_task_automation_update(
        &fixture.runtime,
        &fixture.task_id,
        TaskAutomationUpdate {
            expected_status: Some(TaskStatus::InProgress),
            status: Some(TaskStatus::Backlog),
            status_event: Some("review_timeout_incomplete".into()),
            ..Default::default()
        },
    );
    assert!(refused.is_err());
    assert_eq!(
        fixture.runtime.get_task(&fixture.task_id).unwrap().status,
        TaskStatus::Blocked
    );
    assert!(
        fixture
            .runtime
            .get_task_history(&fixture.task_id)
            .unwrap()
            .iter()
            .all(|entry| entry.event != "review_timeout_incomplete")
    );
}

/// A before-PR reviewer that exits cleanly with only its initial placeholder
/// report leaves its review unspent, and the handoff blocks delivery without
/// calling the missing verdict an escalation [ORB-15130].
#[test]
fn an_abandoned_before_pr_review_is_released_and_the_handoff_says_so() {
    if !super::dispatch_admission::isolated(
        "review_continuation::an_abandoned_before_pr_review_is_released_and_the_handoff_says_so",
    ) {
        return;
    }
    let mut fixture = Fixture::new();
    add_origin(&fixture);
    fixture.admit();
    RuntimeHost::mark_job_run_running(
        &fixture.runtime,
        fixture.input["job_run_id"].as_str().unwrap(),
        Utc::now(),
        std::process::id(),
    )
    .unwrap();
    attach(
        &fixture,
        REVIEW_REPORT_ARTIFACT,
        &json!({
            "schema_version": 1, "attempt_id": fixture.input["admission"]["attempt_id"],
            "verdict": "incomplete", "summary": "Review still running; validation not yet complete.",
            "findings": [], "validation": [],
        }),
    );
    let refused = fixture
        .settle()
        .expect_err("an abandoned review settles no verdict")
        .to_string();
    assert!(
        refused.contains(orbit_types::workflow::REVIEW_ABANDONED_MARKER),
        "{refused}"
    );
    let ledger = fixture
        .runtime
        .review_store()
        .unwrap()
        .review_ledger(
            &fixture.runtime.workspace_id().unwrap(),
            fixture.input["admission"]["lineage_key"].as_str().unwrap(),
        )
        .unwrap()
        .unwrap();
    let attempt = &ledger.attempts[0];
    assert!(attempt.released_at.is_some());
    assert!(!ledger.reviewed(&attempt.candidate, &attempt.task_meaning_digest));

    let handoff = execute_deterministic_action(&fixture.runtime, "pr_failure_handoff", &json!({}), &json!({
        "failed_step_id": "review_gate_settle", "error_code": "deterministic_action_refused",
        "error_message": refused,
        "run_id": fixture.input["job_run_id"],
        "job_input": {"task_ids": [fixture.task_id], "base_branch": "main", "base_sync": "local"},
        "pipeline": {
            "worktree": {"job_run_id": fixture.input["job_run_id"], "workspace_path": fixture.repo},
            "sync_base": {"base": "main", "base_ref": "main"},
            "review_gate_admit": fixture.input["admission"],
        },
    }), false, &Default::default(), None).unwrap();
    assert_eq!(handoff["decision"], "blocked_review_gate", "{handoff}");
    assert_eq!(
        fixture.runtime.get_task(&fixture.task_id).unwrap().status,
        TaskStatus::Blocked
    );
    let note = latest_decision(&fixture).note.unwrap();
    assert!(
        note.contains("review is not spent") && !note.contains("substantive review escalation"),
        "{note}"
    );
}
