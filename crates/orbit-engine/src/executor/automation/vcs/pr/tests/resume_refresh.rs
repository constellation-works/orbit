use std::fs;
use std::sync::Arc;

use orbit_agent::loop_engine::audit::NullSink;
use orbit_types::task::{ExternalRef, TaskStatus};
use orbit_types::workflow::activity_job::{ActivityV2, JobKind, JobV2};
use orbit_types::workflow::{JobRunState, JobScheduleState, PipelineState};
use serde_json::{Value, json};

use super::resume_failure::{
    CHECKPOINT_RUN_ID, FIRST_RESUME_RUN_ID, ResumeFailureHost, SECOND_RESUME_RUN_ID, TASK_ID,
    deterministic_activity, deterministic_step,
};
use super::test_support::*;
use crate::context::{RuntimeHost, TaskAutomationUpdate};
use crate::executor::automation::vcs::failure::pr_failure_handoff;
use crate::{V2AuditWriter, execute_job_with_resume};

fn stale_checkpoint_delivery_job() -> JobV2 {
    let mut sync_base = deterministic_step(
        "sync_base",
        "git_rebase",
        Some(json!({
            "job_run_id": "{{ steps.worktree.output.job_run_id }}",
            "completed_task_ids": [TASK_ID],
            "workspace_path": "{{ steps.worktree.output.workspace_path }}",
            "head": "{{ steps.prepare_branch.output.head }}",
            "head_sha": "{{ steps.prepare_branch.output.head_sha }}",
            "base": "{{ steps.prepare_branch.output.base }}",
            "base_ref": "{{ steps.prepare_branch.output.base_ref }}",
            "base_sha": "{{ steps.prepare_branch.output.base_sha }}",
            "remote_sha": "{{ steps.prepare_branch.output.remote_sha }}",
            "commits_behind": "{{ steps.prepare_branch.output.commits_behind }}",
            "sync_required": "{{ steps.prepare_branch.output.sync_required }}",
        })),
    );
    sync_base.recovery_activity = Some("pr_conflict_recovery".to_string());
    sync_base.resolved_recovery_activity = Some(ActivityV2 {
        description: "resolve the proven test conflict".to_string(),
        input_schema_json: Value::Null,
        output_schema_json: Value::Null,
        fs_profile: None,
        spec: deterministic_activity("test_resolve_rebase_conflict").spec,
    });

    JobV2 {
        state: JobScheduleState::Enabled,
        default_input: None,
        recovery_activity: None,
        resolved_recovery_activity: None,
        failure_activity: None,
        resolved_failure_activity: None,
        max_active_runs: 1,
        kind: JobKind::Workflow,
        steps: vec![
            deterministic_step("worktree", "unused_worktree_setup", None),
            deterministic_step("implement_bundle", "unused_implementation", None),
            deterministic_step("commit", "unused_commit", None),
            deterministic_step(
                "prepare_branch",
                "pr_prepare",
                Some(json!({
                    "job_run_id": "{{ steps.worktree.output.job_run_id }}",
                    "completed_task_ids": [TASK_ID],
                    "workspace_path": "{{ steps.worktree.output.workspace_path }}",
                    "base": "agent-main",
                    "base_sync": "local",
                })),
            ),
            sync_base,
            deterministic_step(
                "push",
                "git_push",
                Some(json!({
                    "job_run_id": "{{ steps.worktree.output.job_run_id }}",
                    "completed_task_ids": [TASK_ID],
                    "workspace_path": "{{ steps.worktree.output.workspace_path }}",
                    "branch": "{{ steps.sync_base.output.head }}",
                    "rewrite_performed": "{{ steps.sync_base.output.rewritten }}",
                    "rewrite_head_before": "{{ steps.sync_base.output.head_sha_before }}",
                    "expected_remote_sha": "{{ steps.sync_base.output.remote_sha_before }}",
                })),
            ),
            deterministic_step(
                "pr_open",
                "pr_open",
                Some(json!({
                    "job_run_id": "{{ steps.worktree.output.job_run_id }}",
                    "completed_task_ids": [TASK_ID],
                    "workspace_path": "{{ steps.worktree.output.workspace_path }}",
                    "head": "{{ steps.sync_base.output.head }}",
                    "base": "{{ steps.sync_base.output.base }}",
                    "base_ref": "{{ steps.sync_base.output.base_ref }}",
                    "base_sha": "{{ steps.sync_base.output.base_sha }}",
                    "base_sync": "local",
                    "landing_branch": "agent-main",
                })),
            ),
            deterministic_step(
                "promote_tasks",
                "pr_promote",
                Some(json!({
                    "job_run_id": "{{ steps.worktree.output.job_run_id }}",
                    "completed_task_ids": [TASK_ID],
                    "workspace_path": "{{ steps.worktree.output.workspace_path }}",
                    "head": "{{ steps.sync_base.output.head }}",
                    "base": "{{ steps.sync_base.output.base }}",
                    "base_ref": "{{ steps.sync_base.output.base_ref }}",
                    "base_sha": "{{ steps.sync_base.output.base_sha }}",
                    "base_sync": "local",
                    "landing_branch": "agent-main",
                    "pr_number": "{{ steps.pr_open.output.pr_number }}",
                    "pr_url": "{{ steps.pr_open.output.pr_url }}",
                })),
            ),
            deterministic_step(
                "complete_pr",
                "pr_complete",
                Some(json!({
                    "job_run_id": "{{ steps.worktree.output.job_run_id }}",
                    "completed_task_ids": [TASK_ID],
                    "workspace_path": "{{ steps.worktree.output.workspace_path }}",
                    "pr_number": "{{ steps.pr_open.output.pr_number }}",
                    "poll_interval_seconds": 0,
                    "max_wait_seconds": 0,
                })),
            ),
        ],
    }
}

#[test]
fn stale_preserved_checkpoint_refreshes_then_recovers_and_completes_the_same_pr() {
    preserved_checkpoint_resume(ResumeCheckpoint::StalePreparation);
}

#[test]
fn recovered_candidate_survives_restart_and_failure_handoff_before_step_checkpoint() {
    preserved_checkpoint_resume(ResumeCheckpoint::RecoveredAfterHandoff);
}

#[test]
fn recovered_candidate_resumes_after_restart_without_a_failure_handoff_or_step_success() {
    preserved_checkpoint_resume(ResumeCheckpoint::RecoveredBeforeHandoff);
}

enum ResumeCheckpoint {
    StalePreparation,
    RecoveredAfterHandoff,
    RecoveredBeforeHandoff,
}

fn preserved_checkpoint_resume(checkpoint: ResumeCheckpoint) {
    let recovered_before_handoff = !matches!(checkpoint, ResumeCheckpoint::StalePreparation);
    let publish_handoff = !matches!(checkpoint, ResumeCheckpoint::RecoveredBeforeHandoff);
    let workspace = pr_workspace();
    if recovered_before_handoff {
        // The observed run had no published remote branch before its recovery.
        // Diverged failure-handoff publication is the separate rewrite-lease task.
        git(
            &workspace.repo,
            &["push", "origin", "--delete", "orbit/test-batch"],
        );
    }
    let original_base = git(&workspace.repo, &["rev-parse", "agent-main"]);
    let candidate_before = git(&workspace.repo, &["rev-parse", "orbit/test-batch"]);
    let mut task = batch_task(
        TASK_ID,
        "Refresh stale delivery",
        "Outcome: success\nChanges:\n- Candidate is ready.",
    );
    task.job_run_id = Some(CHECKPOINT_RUN_ID.to_string());
    let host = ResumeFailureHost::new(
        PrOpenTestHost::new(vec![task], workspace.repo.clone())
            .with_job_run(CHECKPOINT_RUN_ID, None)
            .with_job_run(FIRST_RESUME_RUN_ID, Some(CHECKPOINT_RUN_ID))
            .with_job_run(SECOND_RESUME_RUN_ID, Some(FIRST_RESUME_RUN_ID)),
    );
    let input = json!({
        "task_ids": [TASK_ID],
        "base_branch": "agent-main",
        "base_sync": "local",
        "completion": "done",
    });
    let worktree = json!({
        "workspace_path": workspace.repo,
        "job_run_id": CHECKPOINT_RUN_ID,
        "base_ref": "agent-main",
        "base_sha": original_base,
    });
    let prepare_input = json!({
        "job_run_id": CHECKPOINT_RUN_ID,
        "completed_task_ids": [TASK_ID],
        "workspace_path": workspace.repo,
        "base": "agent-main",
        "base_sync": "local",
    });
    let prepared = crate::executor::automation::vcs::prepare_pr_handoff(&host, &prepare_input)
        .expect("pin the pre-advance base");

    let mut source = PipelineState::new(
        FIRST_RESUME_RUN_ID.to_string(),
        "task_pr_pipeline".to_string(),
        input.clone(),
    );
    source.record_step(0, JobRunState::Success, Some(worktree.clone()), None);
    source.record_step(
        1,
        JobRunState::Success,
        Some(json!({"implemented": true})),
        None,
    );
    source.record_step(
        2,
        JobRunState::Success,
        Some(json!({
            "phase": "commit",
            "task_id": TASK_ID,
            "commit_sha": candidate_before,
        })),
        None,
    );
    source.record_step(3, JobRunState::Success, Some(prepared.clone()), None);
    source.sync_pipeline(json!({
        "worktree": worktree,
        "implement_bundle": {"implemented": true},
        "commit": {
            "phase": "commit",
            "task_id": TASK_ID,
            "commit_sha": candidate_before,
        },
        "prepare_branch": prepared,
    }));

    git(&workspace.repo, &["checkout", "agent-main"]);
    fs::create_dir_all(workspace.repo.join("src")).expect("create base source directory");
    fs::write(
        workspace.repo.join("src/lib.rs"),
        "pub fn base_advanced() {}\n",
    )
    .expect("write conflicting base advance");
    git(&workspace.repo, &["add", "src/lib.rs"]);
    git(
        &workspace.repo,
        &["commit", "-m", "advance base after prepare"],
    );
    let advanced_base = git(&workspace.repo, &["rev-parse", "HEAD"]);
    git(&workspace.repo, &["push", "origin", "agent-main"]);
    git(&workspace.repo, &["checkout", "orbit/test-batch"]);

    let stale_error = crate::executor::automation::vcs::rebase_pr_branch(
        &host,
        &json!({
            "job_run_id": CHECKPOINT_RUN_ID,
            "completed_task_ids": [TASK_ID],
            "workspace_path": workspace.repo,
            "head": source.step_outputs[&3]["head"],
            "head_sha": source.step_outputs[&3]["head_sha"],
            "base": source.step_outputs[&3]["base"],
            "base_ref": source.step_outputs[&3]["base_ref"],
            "base_sha": source.step_outputs[&3]["base_sha"],
            "remote_sha": source.step_outputs[&3]["remote_sha"],
            "commits_behind": source.step_outputs[&3]["commits_behind"],
            "sync_required": source.step_outputs[&3]["sync_required"],
        }),
    )
    .expect_err("the original preparation is stale");
    assert!(stale_error.to_string().contains(&advanced_base));

    let mut error_message = stale_error.to_string();
    if recovered_before_handoff {
        let prepared =
            crate::executor::automation::vcs::prepare_pr_handoff(&host, &prepare_input).unwrap();
        source.step_outputs.insert(3, prepared.clone());
        source.pipeline["prepare_branch"] = prepared.clone();
        host.write_state(source.clone());
        let mut rebase_input = prepared;
        rebase_input["job_run_id"] = json!(CHECKPOINT_RUN_ID);
        rebase_input["run_id"] = json!(FIRST_RESUME_RUN_ID);
        rebase_input["completed_task_ids"] = json!([TASK_ID]);
        rebase_input["workspace_path"] = json!(workspace.repo);
        let conflict =
            crate::executor::automation::vcs::rebase_pr_branch(&host, &rebase_input).unwrap_err();
        error_message = conflict.to_string();
        host.run_deterministic(
            "test_resolve_rebase_conflict",
            &Value::Null,
            &json!({
                "run_id": FIRST_RESUME_RUN_ID,
                "workspace_path": workspace.repo,
                "original_base_sha": original_base,
                "failed_step_input": rebase_input,
            }),
            Default::default(),
        )
        .unwrap();
        // The host completion is durable, but sync_base never checkpointed:
        // simulate process restart by reloading only serialized run state.
        source = serde_json::from_slice(
            &serde_json::to_vec(&host.read_run_state(FIRST_RESUME_RUN_ID).unwrap().unwrap())
                .unwrap(),
        )
        .unwrap();
        assert!(!source.step_states.contains_key(&4));
        assert!(!source.step_outputs.contains_key(&4));
    }

    if publish_handoff {
        let handoff = pr_failure_handoff(
            &host,
            &json!({
                "failed_step_id": "sync_base",
                "activity_name": "git_rebase",
                "error_code": "pipeline_step_failed",
                "error_message": error_message,
                "run_id": FIRST_RESUME_RUN_ID,
                "job_input": input,
                "pipeline": source.pipeline,
            }),
        )
        .expect("publish the preserved candidate to a blocked PR");
        assert_eq!(handoff["decision"], "blocked_failure_pr");
        assert_eq!(handoff["conflicting_paths"], json!([]));
        assert_eq!(handoff["original_base_sha"], original_base);
        assert_eq!(handoff["pr_number"], "42");
        assert_eq!(handoff["preservation_commit_created"], false);
        source.record_failure_activity(
            "pr_failure_handoff".to_string(),
            "sync_base".to_string(),
            handoff,
        );
    }
    host.write_state(source.clone());
    let historical_source = source.clone();

    let mut resumed = source;
    resumed.run_id = SECOND_RESUME_RUN_ID.to_string();
    host.write_state(resumed.clone());
    host.apply_task_automation_update(
        TASK_ID,
        TaskAutomationUpdate {
            status: Some(TaskStatus::InProgress),
            ..TaskAutomationUpdate::default()
        },
    )
    .expect("re-admit the preserved task");
    host.inner.queue_pr_status([
        json!({
            "number": 42,
            "state": "OPEN",
            "mergedAt": Value::Null,
            "mergeStateStatus": "CLEAN",
            "headRefName": "orbit/test-batch",
            "baseRefName": "agent-main",
        }),
        json!({
            "number": 42,
            "state": "MERGED",
            "mergedAt": "2026-09-07T00:00:00Z",
        }),
    ]);

    let delivered = execute_job_with_resume(
        &stale_checkpoint_delivery_job(),
        json!({
            "task_ids": [TASK_ID],
            "completion": "done",
        }),
        SECOND_RESUME_RUN_ID,
        Arc::new(V2AuditWriter::new(
            SECOND_RESUME_RUN_ID,
            "test-agent",
            Arc::new(NullSink),
        )),
        &host,
        Some(&resumed),
    )
    .expect("fresh preparation reaches bounded conflict recovery and completion");

    assert!(delivered.success);
    assert_eq!(
        delivered.pipeline["prepare_branch"]["base_sha"],
        advanced_base
    );
    if publish_handoff {
        assert_eq!(
            delivered.pipeline["prepare_branch"]["resume_refresh"],
            json!({
                "kind": "stale_delivery_checkpoint",
                "source_run_id": FIRST_RESUME_RUN_ID,
                "previous_base_sha": if recovered_before_handoff { &advanced_base } else { &original_base },
                "attempt": 1,
                "max_attempts": 1,
            })
        );
    }
    assert_eq!(
        delivered.pipeline["sync_base"]["decision"],
        if recovered_before_handoff && publish_handoff {
            "skipped_current"
        } else {
            "reused_recovery"
        }
    );
    assert_eq!(
        delivered.pipeline["pr_open"]["decision"],
        if publish_handoff {
            "reused"
        } else {
            "performed"
        }
    );
    assert_eq!(delivered.pipeline["pr_open"]["pr_number"], "42");
    assert_eq!(host.inner.task_status(TASK_ID), TaskStatus::Done);
    assert_eq!(
        fs::read_to_string(workspace.repo.join("src/lib.rs")).expect("resolved source"),
        "pub fn base_advanced() {}\npub fn changed() {}\n",
        "the recovered candidate contains both the intervening base and task changes",
    );
    assert_eq!(
        host.inner
            .vcs_calls()
            .iter()
            .filter(|call| call.operation == PR_CREATE_OPERATION)
            .count(),
        1,
        "the failure handoff creates one PR and resume reuses it",
    );
    assert_eq!(
        host.read_run_state(FIRST_RESUME_RUN_ID)
            .expect("read source state")
            .expect("source state exists"),
        historical_source,
        "checkpoint refresh writes only the descendant run state",
    );
    let active = host
        .read_run_state(SECOND_RESUME_RUN_ID)
        .expect("read active state")
        .expect("active state exists");
    assert_eq!(active.step_outputs[&3]["base_sha"], advanced_base);
    if publish_handoff {
        assert_eq!(active.step_outputs[&3]["resume_refresh"]["attempt"], 1);
    }
}

#[test]
fn repeated_base_advancement_stops_at_the_resume_refresh_limit() {
    let workspace = pr_workspace();
    let original_base = git(&workspace.repo, &["rev-parse", "agent-main"]);
    let candidate = git(&workspace.repo, &["rev-parse", "orbit/test-batch"]);
    let mut task = batch_task(
        TASK_ID,
        "Bound stale delivery refresh",
        "Outcome: success\nChanges:\n- Candidate is ready.",
    );
    task.job_run_id = Some(CHECKPOINT_RUN_ID.to_string());
    task.external_refs = vec![ExternalRef::github_pr("42").expect("PR ref")];
    let host = ResumeFailureHost::new(
        PrOpenTestHost::new(vec![task], workspace.repo.clone())
            .with_existing_pr()
            .with_job_run(CHECKPOINT_RUN_ID, None)
            .with_job_run(FIRST_RESUME_RUN_ID, Some(CHECKPOINT_RUN_ID))
            .with_job_run(SECOND_RESUME_RUN_ID, Some(FIRST_RESUME_RUN_ID)),
    );
    let input = json!({
        "task_ids": [TASK_ID],
        "base_branch": "agent-main",
        "base_sync": "local",
        "completion": "done",
    });
    let worktree = json!({
        "workspace_path": workspace.repo,
        "job_run_id": CHECKPOINT_RUN_ID,
        "base_ref": "agent-main",
        "base_sha": original_base,
    });
    let mut prepared = crate::executor::automation::vcs::prepare_pr_handoff(
        &host,
        &json!({
            "job_run_id": CHECKPOINT_RUN_ID,
            "completed_task_ids": [TASK_ID],
            "workspace_path": workspace.repo,
            "base": "agent-main",
            "base_sync": "local",
        }),
    )
    .expect("prepare the candidate");
    prepared["resume_refresh"] = json!({
        "kind": "stale_delivery_checkpoint",
        "source_run_id": "jrun-earlier-refresh",
        "previous_base_sha": original_base,
        "attempt": 1,
        "max_attempts": 1,
    });

    let mut source = PipelineState::new(
        FIRST_RESUME_RUN_ID.to_string(),
        "task_pr_pipeline".to_string(),
        input,
    );
    source.record_step(0, JobRunState::Success, Some(worktree.clone()), None);
    source.record_step(
        1,
        JobRunState::Success,
        Some(json!({"implemented": true})),
        None,
    );
    source.record_step(
        2,
        JobRunState::Success,
        Some(json!({
            "phase": "commit",
            "task_id": TASK_ID,
            "commit_sha": candidate,
        })),
        None,
    );
    source.record_step(3, JobRunState::Success, Some(prepared.clone()), None);
    source.sync_pipeline(json!({
        "worktree": worktree,
        "implement_bundle": {"implemented": true},
        "commit": {
            "phase": "commit",
            "task_id": TASK_ID,
            "commit_sha": candidate,
        },
        "prepare_branch": prepared,
    }));
    source.record_failure_activity(
        "pr_failure_handoff".to_string(),
        "sync_base".to_string(),
        json!({
            "phase": "failure_handoff",
            "decision": "blocked_failure_pr",
            "task_id": TASK_ID,
            "handoff_run_id": FIRST_RESUME_RUN_ID,
            "checkpoint_owner": CHECKPOINT_RUN_ID,
            "preservation_commit_created": false,
            "failed_step_id": "sync_base",
            "branch": "orbit/test-batch",
            "head_sha": candidate,
            "original_base_sha": original_base,
            "target_base_sha": original_base,
            "pr_number": "42",
        }),
    );
    host.write_state(source.clone());
    let historical_source = source.clone();
    let mut resumed = source;
    resumed.run_id = SECOND_RESUME_RUN_ID.to_string();
    host.write_state(resumed.clone());

    git(&workspace.repo, &["checkout", "agent-main"]);
    fs::write(
        workspace.repo.join("advanced-again.txt"),
        "second advance\n",
    )
    .expect("write second base advance");
    git(&workspace.repo, &["add", "advanced-again.txt"]);
    git(&workspace.repo, &["commit", "-m", "advance base again"]);
    git(&workspace.repo, &["checkout", "orbit/test-batch"]);
    let head_before = git(&workspace.repo, &["rev-parse", "HEAD"]);
    let status_before = git(&workspace.repo, &["status", "--porcelain"]);

    for state in [JobRunState::Running, JobRunState::Cancelled] {
        host.set_job_run_state(CHECKPOINT_RUN_ID, state);
        let error = execute_job_with_resume(
            &stale_checkpoint_delivery_job(),
            json!({"task_ids": [TASK_ID], "completion": "done"}),
            SECOND_RESUME_RUN_ID,
            Arc::new(V2AuditWriter::new(
                SECOND_RESUME_RUN_ID,
                "test-agent",
                Arc::new(NullSink),
            )),
            &host,
            Some(&resumed),
        )
        .expect_err("live or cancelled checkpoint ownership must refuse refresh");

        let message = error.to_string();
        assert!(message.contains(&format!("is {state}")), "{message}");
        assert!(message.contains("only failed, timed-out, or interrupted"));
        assert_eq!(git(&workspace.repo, &["rev-parse", "HEAD"]), head_before);
        assert_eq!(
            git(&workspace.repo, &["status", "--porcelain"]),
            status_before
        );
        assert_eq!(
            host.read_run_state(FIRST_RESUME_RUN_ID)
                .expect("read source state")
                .expect("source state exists"),
            historical_source,
        );
        assert!(host.inner.vcs_calls().is_empty());
    }
    host.set_job_run_state(CHECKPOINT_RUN_ID, JobRunState::Failed);

    let error = execute_job_with_resume(
        &stale_checkpoint_delivery_job(),
        json!({"task_ids": [TASK_ID], "completion": "done"}),
        SECOND_RESUME_RUN_ID,
        Arc::new(V2AuditWriter::new(
            SECOND_RESUME_RUN_ID,
            "test-agent",
            Arc::new(NullSink),
        )),
        &host,
        Some(&resumed),
    )
    .expect_err("a second preparation refresh is refused");

    let message = error.to_string();
    assert!(
        message.contains("resume_checkpoint_refresh_exhausted"),
        "{message}"
    );
    assert!(message.contains("bounded limit of 1"), "{message}");
    assert!(message.contains(&original_base), "{message}");
    assert_eq!(git(&workspace.repo, &["rev-parse", "HEAD"]), head_before);
    assert_eq!(
        git(&workspace.repo, &["status", "--porcelain"]),
        status_before
    );
    assert_eq!(
        host.read_run_state(FIRST_RESUME_RUN_ID)
            .expect("read source state")
            .expect("source state exists"),
        historical_source,
    );
    assert!(
        host.inner.vcs_calls().is_empty(),
        "bounded refusal does not push, create, or merge a PR",
    );
}
