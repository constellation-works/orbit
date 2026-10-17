//! Cancelled owner-local source recovery through the public tool/runtime carrier.
#![cfg(unix)]

use chrono::Utc;
use orbit_core::TaskStatus;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate, execute_deterministic_action};
use orbit_types::workflow::JobRunState;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;

#[path = "source_repair/association.rs"]
mod association;
#[path = "source_repair/failed_publication.rs"]
mod failed_publication;
#[path = "source_repair/fixture.rs"]
mod fixture;
use fixture::{Fixture, git};
#[path = "source_repair/completion.rs"]
mod completion;
#[path = "source_repair/generations.rs"]
mod generations;
#[path = "source_repair/publication.rs"]
mod publication;
#[path = "source_repair/published_owner.rs"]
mod published_owner;
#[path = "source_repair/replay.rs"]
mod replay;
#[path = "source_repair/resumed_recovery.rs"]
mod resumed_recovery;
#[path = "source_repair/reviewers.rs"]
mod reviewers;
use completion::mutation::{
    enabled_completion_refuses_repair_and_mutation_provenance,
    expired_completion_budget_never_merges,
};
use completion::prerequisite::{
    later_prerequisite_read_failure_resumes_only_completion,
    prerequisite_read_after_mutation_provenance_never_resumes,
};
use completion::{
    changed_base_gets_fresh_native_validation, completion_reuse_refuses_drift,
    concurrent_and_lost_admission_keep_one_completion_budget,
    configured_review_requires_unchanged_host_certificate,
    refused_receipts_stop_before_agent_or_republication,
    unchanged_publication_retries_only_completion,
    unknown_merge_reply_is_reconciled_without_republication,
};
use generations::cancelled_published_repair_accepts_fresh_generation;
use published_owner::{
    failed_requeued_direct_publisher_retains_authority_and_drift_refusals,
    failed_requeued_publisher_is_inspected_and_submitted_without_a_successor,
    legacy_publication_behind_a_stopped_successor_cannot_start_fresh,
    published_completion_requeue_adopts_without_reimplementation,
    published_requeue_refuses_changed_scope_head_and_incomplete_source,
    stopped_successor_is_explicitly_bound_when_repairing_older_publication,
};
use replay::lost_spawn_reply_reconciles_the_reserved_run_after_older_retries;

fn cancelled_candidate_requires_explicit_intent_and_continues_same_branch() {
    let f = Fixture::new();
    let error = f
        .runtime
        .submit_resume_run(&f.source.run_id, Some("operator"), None)
        .unwrap_err();
    assert!(
        error.to_string().contains("cancelled"),
        "generic resume retains its terminal-state boundary: {error}"
    );
    let crew_source = f.runtime.get_task(&f.task).unwrap().crew_source;
    let input = f.submit(f.inspect());
    let result = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", input.clone())
        .unwrap();
    let id = result["run_id"].as_str().unwrap();
    f.wait(id);
    let run = f.runtime.show_job_run_observed(id).unwrap();
    assert_eq!(
        run.retry_source_run_id.as_deref(),
        Some(f.source.run_id.as_str())
    );
    assert_eq!(
        run.input.as_ref().unwrap()["crew_selection"]["source"],
        json!(crew_source),
        "source recovery must retain normal crew provenance, not invent an explicit override"
    );
    let reserved = f
        .runtime
        .get_task_history(&f.task)
        .unwrap()
        .into_iter()
        .find(|entry| entry.event == "source_repair_reserved")
        .unwrap();
    assert!(
        reserved.at <= run.created_at,
        "authority must be durable before run admission"
    );
    assert_eq!(run.input.as_ref().unwrap()["completion"], "review");
    assert_eq!(run.input.as_ref().unwrap()["auto_fix"], false);
    let state = f.runtime.read_run_state(id).unwrap().unwrap();
    assert!(
        state.step_states.is_empty(),
        "no review/validation/commit checkpoint is inherited"
    );
    let replay = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", input.clone())
        .unwrap();
    assert_eq!(replay["run_id"], id);
    assert_eq!(replay["replayed"], true);
    let mut changed = input;
    changed["source_repair"]["diagnosis"] = json!("different authority");
    assert!(
        f.runtime
            .run_tool_as_human("orbit.workflow.run.resume", changed)
            .unwrap_err()
            .to_string()
            .contains("different input")
    );
    assert!(
        f.runtime
            .submit_resume_run(id, Some("operator"), None)
            .unwrap_err()
            .to_string()
            .contains("one attempt")
    );
    f.start_actions(id);
    let setup = f.action(
        "worktree_setup",
        json!({"job_run_id":id,"task_ids":[f.task],"base":"main","base_sync":"remote"}),
    );
    let checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
    // Linked worktrees share origin but not per-worktree URL rewriting config.
    let resumed=f.action("candidate_resume",json!({"job_run_id":id,"task_ids":[f.task],"workspace_path":checkout,"base_sha":setup["base_sha"],"prior_job_run_id":f.source.run_id}));
    assert_eq!(resumed["implement"], true);
    assert_eq!(resumed["repair"]["trigger"], "continuation");
    assert!(
        resumed["repair"]["output"]
            .as_str()
            .unwrap()
            .contains("E0603")
    );
    assert_eq!(resumed["reused_branch"], f.branch);
    assert_eq!(resumed["reused_head_sha"], f.head);
    assert_eq!(resumed["prior_pull_request"], "17");
    assert_eq!(git(&checkout, &["branch", "--show-current"]), f.branch);
    assert_eq!(
        std::fs::read_to_string(checkout.join("source.txt")).unwrap(),
        "candidate needing repair\n"
    );
    assert_eq!(
        git(
            &f.repo,
            &["ls-remote", "origin", &format!("refs/heads/{}", f.branch)]
        )
        .split_whitespace()
        .next(),
        Some(f.head.as_str())
    );
    f.assert_original();
}

fn cancelled_admission_refuses_identity_liveness_lease_and_evidence_drift() {
    for case in [
        "scope",
        "footprint",
        "binding",
        "binding_task",
        "binding_pr",
        "binding_repo",
        "claimed_input",
        "before_head",
        "after_head",
        "checkpoint_base",
        "checkpoint_pr",
        "checkpoint_branch",
        "checkpoint_lease",
        "push_lease",
        "rewrite_flag",
        "checkpoint_state",
        "checkpoint_copy",
        "missing_prepare",
        "host_action",
        "live",
        "local_head",
        "remote_head",
        "closed",
        "merged",
        "merged_at",
        "merge_intent",
        "missing_intent",
        "wrong_pr",
        "wrong_repo",
        "wrong_branch",
        "wrong_base",
        "missing_checkpoint",
        "foreign",
        "rebound",
    ] {
        let mut f = Fixture::new();
        let binding = f.inspect();
        let input = f.submit(binding);
        match case {
            "scope" | "footprint" => {
                f.runtime
                    .update_task_with_identity(
                        &f.task,
                        orbit_core::application::task::TaskUpdateParams {
                            description: (case == "scope").then(|| "new work".into()),
                            context_files: (case == "footprint")
                                .then(|| vec!["file:source.txt".into()]),
                            ..Default::default()
                        },
                        None,
                        Some("codex".into()),
                    )
                    .unwrap();
            }
            "claimed_input" => {
                f.source.input.as_mut().unwrap()["claimed"] = json!(true);
                f.runtime
                    .sqlite_store()
                    .unwrap()
                    .upsert_job_run_for_workspace(
                        &f.runtime.workspace_id().unwrap(),
                        &f.source,
                        None,
                    )
                    .unwrap();
            }
            "host_action" => {
                let path = f
                    ._root
                    .path()
                    .join("global/resources/jobs/task_pr_pipeline.yaml");
                let yaml = std::fs::read_to_string(&path).unwrap();
                std::fs::write(
                    path,
                    yaml.replace(
                        "id: prepare_branch, target: 'activity:pr_prepare'",
                        "id: prepare_branch, target: 'activity:git_rebase'",
                    ),
                )
                .unwrap();
            }
            "before_head" | "after_head" | "checkpoint_base" | "checkpoint_pr"
            | "checkpoint_branch" | "checkpoint_lease" | "push_lease" | "rewrite_flag"
            | "checkpoint_state" | "checkpoint_copy" | "missing_prepare" => {
                let (step, index, field, value) = match case {
                    "before_head" => ("sync_base", 3, "head_sha_before", json!(f.base)),
                    "after_head" => ("sync_base", 3, "head_sha", json!(f.base)),
                    "checkpoint_base" => ("sync_base", 3, "base_sha", json!(f.head)),
                    "checkpoint_pr" => ("pr_open", 5, "pr_number", json!("18")),
                    "checkpoint_branch" => ("push", 4, "branch", json!("foreign")),
                    "checkpoint_lease" => ("sync_base", 3, "remote_sha_before", json!(f.base)),
                    "push_lease" => ("push", 4, "remote_sha_before", json!(f.base)),
                    "rewrite_flag" => ("sync_base", 3, "rewritten", json!(false)),
                    _ => ("prepare_branch", 2, "head_sha", json!(f.base)),
                };
                if case == "missing_prepare" {
                    f.state.pipeline.as_object_mut().unwrap().remove(step);
                } else if case == "checkpoint_state" {
                    f.state.step_states.insert(index, JobRunState::Failed);
                } else {
                    f.state.pipeline[step][field] = value;
                    if case != "checkpoint_copy" {
                        f.state
                            .step_outputs
                            .insert(index, f.state.pipeline[step].clone());
                    }
                }
                f.runtime
                    .write_run_state(&f.source.run_id, &f.state)
                    .unwrap();
                // Re-inspection also refuses a self-consistent but foreign chain.
                assert!(
                    f.runtime
                        .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
                        .is_err(),
                    "{case}"
                );
            }
            "binding" | "binding_task" | "binding_pr" | "binding_repo" => {
                let mut input = input.clone();
                let (field, value) = match case {
                    "binding_task" => ("task_id", "foreign-task".into()),
                    "binding_pr" => ("pull_request", "18".into()),
                    "binding_repo" => ("repository", "foreign/repository".into()),
                    _ => ("head_sha", "a".repeat(40)),
                };
                input["source_repair"]["binding"][field] = json!(value);
                assert!(
                    f.runtime
                        .run_tool_as_human("orbit.workflow.run.resume", input)
                        .is_err()
                );
                continue;
            }
            "live" => {
                f.source.pid = Some(std::process::id());
                f.runtime
                    .sqlite_store()
                    .unwrap()
                    .upsert_job_run_for_workspace(
                        &f.runtime.workspace_id().unwrap(),
                        &f.source,
                        None,
                    )
                    .unwrap();
            }
            "local_head" => {
                git(&f.repo, &["reset", "--hard", &f.base]);
            }
            "remote_head" => {
                git(
                    &f.repo,
                    &[
                        "push",
                        "-q",
                        "--force",
                        "origin",
                        &format!("{}:refs/heads/{}", f.base, f.branch),
                    ],
                );
            }
            "missing_checkpoint" => {
                f.state.pipeline.as_object_mut().unwrap().remove("push");
                f.runtime
                    .write_run_state(&f.source.run_id, &f.state)
                    .unwrap();
            }
            "foreign" => {
                // The normal upsert preserves the trusted origin. Corrupt it
                // explicitly as hostile persisted evidence for this boundary case.
                f.runtime
                    .sqlite_store()
                    .unwrap()
                    .with_transaction(|tx| {
                        tx.connection()
                            .execute(
                                "UPDATE job_runs SET executed_on_json = ?1 WHERE run_id = ?2",
                                [
                                    json!({"machine_id":"hm_foreign","machine_name":null})
                                        .to_string(),
                                    f.source.run_id.clone(),
                                ],
                            )
                            .unwrap();
                        Ok(())
                    })
                    .unwrap();
            }
            "rebound" => {
                f.runtime
                    .apply_task_automation_update(
                        &f.task,
                        TaskAutomationUpdate {
                            job_run_id: Some("other".into()),
                            ..Default::default()
                        },
                    )
                    .unwrap();
            }
            _ => {
                let mut answer: Value =
                    serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
                match case {
                    "closed" => answer["state"] = json!("CLOSED"),
                    "merged" => answer["state"] = json!("MERGED"),
                    "merged_at" => answer["mergedAt"] = json!("2026-10-15T00:00:00Z"),
                    "merge_intent" => answer["autoMergeRequest"] = json!({"enabledAt":"now"}),
                    "missing_intent" => {
                        answer.as_object_mut().unwrap().remove("autoMergeRequest");
                    }
                    "wrong_pr" => answer["number"] = json!(18),
                    "wrong_repo" => answer["url"] = json!("https://github.com/other/repo/pull/17"),
                    "wrong_branch" => answer["headRefName"] = json!("other"),
                    "wrong_base" => answer["baseRefName"] = json!("other"),
                    _ => unreachable!(),
                };
                std::fs::write(&f.answer, answer.to_string()).unwrap();
            }
        }
        assert!(
            f.runtime
                .run_tool_as_human("orbit.workflow.run.resume", input)
                .is_err(),
            "{case} must refuse before admission"
        );
        assert!(
            !f.runtime
                .get_task_history(&f.task)
                .unwrap()
                .iter()
                .any(|e| e.event == "source_repair_reserved"),
            "{case} must leave no reservation"
        );
    }
}

fn current_policy_is_captured_and_review_runs_at_its_scheduled_boundary() {
    for timing in ["none", "before-landing"] {
        let f = Fixture::with_policy(timing);
        // Explicitly revised canonical remaining work is admitted, rather than
        // replaying the original submitted task or implementation instructions.
        f.runtime
            .update_task_with_identity(
                &f.task,
                orbit_core::application::task::TaskUpdateParams {
                    description: Some("New authorized remaining repair specification".into()),
                    context_files: Some(vec!["file:source.txt".into()]),
                    ..Default::default()
                },
                None,
                Some("codex".into()),
            )
            .unwrap();
        let inspection = f
            .runtime
            .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
            .unwrap();
        assert_eq!(inspection["review"]["timing"], timing);
        assert_eq!(
            inspection["review"]["required_validation_commands"],
            json!(["true"])
        );
        let result = f
            .runtime
            .run_tool_as_human(
                "orbit.workflow.run.resume",
                f.submit(inspection["binding"].clone()),
            )
            .unwrap();
        let id = result["run_id"].as_str().unwrap();
        f.wait(id);
        let run = f.runtime.show_job_run_observed(id).unwrap();
        assert_eq!(run.input.as_ref().unwrap()["review"]["timing"], timing);
        assert_eq!(
            run.input.as_ref().unwrap()["__source_repair"]["review"],
            run.input.as_ref().unwrap()["review"]
        );
        f.start_actions(id);
        let setup = f.action(
            "worktree_setup",
            json!({"job_run_id":id,"task_ids":[f.task],"base":"main","base_sync":"remote"}),
        );
        let checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
        f.action("candidate_resume", json!({"job_run_id":id,"task_ids":[f.task],"workspace_path":checkout,"base_sha":setup["base_sha"]}));
        std::fs::write(checkout.join("source.txt"), "repaired source\n").unwrap();
        f.action("git_commit", json!({"job_run_id":id,"scope":"all","workspace_path":checkout,"base_sha":setup["base_sha"],"base_ref":setup["base_ref"]}));
        let validated = f.action("candidate_validate", json!({"job_run_id":id,"completed_task_ids":[f.task],"workspace_path":checkout,"base_sha":setup["base_sha"],"base_ref":setup["base_ref"]}));
        assert_eq!(validated["decision"], "passed");
        // Publication must use the durable owner admission, even without the
        // claimed pipeline's candidate_resume argument.
        let mut answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
        answer["autoMergeRequest"] = json!({"enabledAt":"now"});
        std::fs::write(&f.answer, answer.to_string()).unwrap();
        let push_input = json!({"job_run_id":id,"workspace_path":checkout,"branch":f.branch,"base":"main","base_ref":setup["base_ref"]});
        let refused = execute_deterministic_action(
            &f.runtime,
            "git_push",
            &json!({}),
            &push_input,
            false,
            &HashMap::new(),
            None,
        )
        .unwrap_err();
        assert!(refused.to_string().contains("push_lease_lost"), "{refused}");
        assert_eq!(
            git(
                &f.repo,
                &["ls-remote", "origin", &format!("refs/heads/{}", f.branch)]
            )
            .split_whitespace()
            .next(),
            Some(f.head.as_str())
        );
        answer["autoMergeRequest"] = Value::Null;
        std::fs::write(&f.answer, answer.to_string()).unwrap();
        let pushed = f.action("git_push", push_input);
        assert_eq!(pushed["force_with_lease"], true);
        let repaired_head = git(&checkout, &["rev-parse", "HEAD"]);
        answer["headRefOid"] = json!(repaired_head);
        answer["number"] = json!(18);
        std::fs::write(&f.answer, answer.to_string()).unwrap();
        let open_input = json!({"job_run_id":id,"completed_task_ids":[f.task],"workspace_path":checkout,"head":f.branch,"base":"main","base_ref":setup["base_ref"],"base_sha":setup["base_sha"]});
        let refused = execute_deterministic_action(
            &f.runtime,
            "pr_open",
            &json!({}),
            &open_input,
            false,
            &HashMap::new(),
            None,
        )
        .unwrap_err();
        assert!(
            refused.to_string().contains("creating or replacing"),
            "{refused}"
        );
        answer["number"] = json!(17);
        std::fs::write(&f.answer, answer.to_string()).unwrap();
        let opened = f.action("pr_open", open_input);
        assert_eq!(opened["pr_number"], "17");
        assert_eq!(opened["pr_reused"], true);
        if timing == "before-landing" {
            std::fs::write(checkout.join("source.txt"), "reviewer fixed source\n").unwrap();
            git(&checkout, &["add", "source.txt"]);
            git(&checkout, &["commit", "-qm", "reviewer correction"]);
            let followup = json!({"job_run_id":id,"workspace_path":checkout,"branch":f.branch,"lease_remote_sha":repaired_head,"pr_number":"17"});
            let refused = execute_deterministic_action(
                &f.runtime,
                "git_push",
                &json!({}),
                &followup,
                false,
                &HashMap::new(),
                None,
            )
            .unwrap_err();
            assert!(
                refused
                    .to_string()
                    .contains("lacks this run's successful publication"),
                "{refused}"
            );
            let mut state = f.runtime.read_run_state(id).unwrap().unwrap();
            state.record_step(4, JobRunState::Success, Some(pushed.clone()), None);
            state.record_pipeline_output("push", pushed);
            f.runtime.write_run_state(id, &state).unwrap();
            f.action("candidate_validate",json!({"job_run_id":id,"completed_task_ids":[f.task],"workspace_path":checkout,"base_sha":setup["base_sha"],"base_ref":setup["base_ref"]}));
            let published = f.action("git_push", followup);
            assert_eq!(published["force_with_lease"], true);
            answer["headRefOid"] = json!(git(&checkout, &["rev-parse", "HEAD"]));
            std::fs::write(&f.answer, answer.to_string()).unwrap();
        }

        let answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
        assert_eq!(
            answer["isDraft"], true,
            "source recovery preserves the explicit draft hold"
        );

        let mut gate = json!({"job_run_id":id,"completed_task_ids":[f.task],"workspace_path":checkout,"base":"main","base_sha":setup["base_sha"],"base_sync":"remote","mode":"pr"});
        let pre_push = f.action("review_gate_admit", gate.clone());
        assert_eq!(pre_push["applies"], false, "{pre_push}");
        f.runtime
            .apply_task_automation_update(
                &f.task,
                TaskAutomationUpdate {
                    status: Some(TaskStatus::Review),
                    ..Default::default()
                },
            )
            .unwrap();
        gate["before_landing"] = json!(true);
        let admission = f.action("review_gate_admit", gate.clone());
        assert_eq!(
            admission["applies"],
            timing == "before-landing",
            "{admission}"
        );
        if timing == "before-landing" {
            gate["admission"] = admission;
            let report = json!({"schema_version":1,"attempt_id":gate["admission"]["attempt_id"],"verdict":"reject","summary":"Independent reviewer found remaining repair work.","findings":[],"validation":[{"id":"V1","command":"true","outcome":"passed","role":"required"}],"escalation":"Source still needs correction."});
            let path = f.repo.join(".orbit/tmp/review-report.json");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, report.to_string()).unwrap();
            f.runtime.run_tool("orbit.task.artifact.put",json!({"id":f.task,"model":"codex","path":"review-report.json","source_path":path})).unwrap();
            let error = f
                .runtime
                .run_deterministic("review_gate_settle", &json!({}), &gate, Default::default())
                .unwrap_err();
            assert!(error.to_string().contains("review_gate_blocked"), "{error}");
            assert_eq!(
                f.runtime.get_task(&f.task).unwrap().status,
                TaskStatus::Review
            );
        } else {
            gate["admission"] = admission;
            let settled = f.action("review_gate_settle", gate);
            assert_eq!(settled["gate"], "not_required");
        }
        f.assert_original();
    }
}

fn concurrent_replay_and_cancelled_attempt_keep_one_durable_reservation() {
    let f = Fixture::new();
    let input = f.submit(f.inspect());
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let submit = || {
            barrier.wait();
            f.runtime
                .run_tool_as_human("orbit.workflow.run.resume", input.clone())
                .unwrap()
        };
        let a = scope.spawn(submit);
        let b = scope.spawn(submit);
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_eq!(results.0["run_id"], results.1["run_id"]);
    let id = results.0["run_id"].as_str().unwrap();
    f.wait(id);
    let mut run = f.runtime.show_job_run_observed(id).unwrap();
    run.state = JobRunState::Cancelled;
    run.finished_at = Some(Utc::now());
    f.runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&f.runtime.workspace_id().unwrap(), &run, None)
        .unwrap();
    let replay = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", input.clone())
        .unwrap();
    assert_eq!(replay["run_id"], id);
    assert_eq!(replay["state"], "cancelled");
    let history = f.runtime.get_task_history(&f.task).unwrap();
    assert_eq!(
        history
            .iter()
            .filter(|e| e.event == "source_repair_reserved")
            .count(),
        1
    );
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                job_run_id: Some(f.source.run_id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    let mut another = input;
    another["source_repair"]["request_key"] = json!("new-key-after-cancel");
    let error = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", another)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("already has a repair reservation"),
        "{error}"
    );
    assert_eq!(
        f.runtime
            .get_task_history(&f.task)
            .unwrap()
            .iter()
            .filter(|e| e.event == "source_repair_reserved")
            .count(),
        1
    );
    f.assert_original();
}

fn exhausted_budget_and_unreadable_ledger_refuse_without_run_admission() {
    use orbit_types::task::TaskHistoryEntry;
    for corrupt in [false, true] {
        let f = Fixture::new();
        let input = f.submit(f.inspect());
        let entries = if corrupt {
            vec![("source_repair_reserved", "{".to_string())]
        } else {
            (0..orbit_types::workflow::PR_AUTO_FIX_BUDGET).map(|i| (
                "pr_auto_fix_admitted",
                json!({"pr_number":"17","head_sha":format!("{i:040x}"),"blocker":"failed_checks","attempt":i+1,"budget":3,"run_id":format!("old-{i}"),"crew":"fixture","crew_source":"fixture"}).to_string()
            )).collect()
        };
        f.runtime
            .apply_task_automation_update(
                &f.task,
                TaskAutomationUpdate {
                    append_history: entries
                        .into_iter()
                        .map(|(event, note)| TaskHistoryEntry {
                            at: Utc::now(),
                            by: "system".into(),
                            event: event.into(),
                            note: Some(note),
                            from_status: None,
                            to_status: None,
                            operation_id: None,
                            decision: None,
                        })
                        .collect(),
                    ..Default::default()
                },
            )
            .unwrap();
        let error = f
            .runtime
            .run_tool_as_human("orbit.workflow.run.resume", input)
            .unwrap_err();
        assert!(
            error.to_string().contains(if corrupt {
                "unreadable repair reservation"
            } else {
                "budget exhausted"
            }),
            "{error}"
        );
        assert_eq!(
            f.runtime.get_task(&f.task).unwrap().status,
            TaskStatus::Review
        );
        f.assert_original();
    }
}

fn scope_and_cancellation_are_rechecked_before_source_effects() {
    for case in ["scope", "cancelled", "association"] {
        let f = Fixture::new();
        let result = f
            .runtime
            .run_tool_as_human("orbit.workflow.run.resume", f.submit(f.inspect()))
            .unwrap();
        let id = result["run_id"].as_str().unwrap();
        f.wait(id);
        f.start_actions(id);
        let setup = f.action(
            "worktree_setup",
            json!({"job_run_id":id,"task_ids":[f.task],"base":"main","base_sync":"remote"}),
        );
        let checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
        let before = git(&checkout, &["branch", "--show-current"]);
        if case == "cancelled" {
            let mut run = f.runtime.show_job_run_observed(id).unwrap();
            run.state = JobRunState::Cancelled;
            run.finished_at = Some(Utc::now());
            f.runtime
                .sqlite_store()
                .unwrap()
                .upsert_job_run_for_workspace(&f.runtime.workspace_id().unwrap(), &run, None)
                .unwrap();
        } else if case == "association" {
            f.replace_refs(Vec::new());
        } else {
            f.runtime
                .update_task_with_identity(
                    &f.task,
                    orbit_core::application::task::TaskUpdateParams {
                        context_files: Some(vec!["file:base-advance.txt".into()]),
                        ..Default::default()
                    },
                    None,
                    Some("codex".into()),
                )
                .unwrap();
        }
        let error = execute_deterministic_action(&f.runtime,"candidate_resume",&json!({}),
        &json!({"job_run_id":id,"task_ids":[f.task],"workspace_path":checkout,"base_sha":setup["base_sha"]}),
        false,&HashMap::new(),None).unwrap_err();
        assert!(
            error.to_string().contains(if case == "association" {
                "task PR association"
            } else {
                "changed before source application"
            }),
            "{case}: {error}"
        );
        assert_eq!(git(&checkout, &["branch", "--show-current"]), before);
        assert_eq!(
            std::fs::read_to_string(checkout.join("source.txt")).unwrap(),
            "base\n"
        );
        f.assert_original();
    }
}

fn model_context_and_forged_run_inputs_cannot_grant_repair_authority() {
    let f = Fixture::as_agent();
    let error = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
        .unwrap_err();
    assert!(
        error.to_string().contains("operator on the owning host"),
        "{error}"
    );
    let forged = json!({"task_ids":[f.task],"__source_repair":{"request_key":"forged"}});
    let error = f
        .runtime
        .submit_job_run("task_pr_pipeline", forged.clone(), Some("operator"))
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("reserved for explicit operator admission"),
        "{error}"
    );
    let path = f
        ._root
        .path()
        .join("global/resources/jobs/task_pr_pipeline.yaml");
    let error = f.runtime.run_job_v2_from_yaml(&path, forged).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("reserved for explicit operator admission"),
        "{error}"
    );
    assert!(
        !f.runtime
            .get_task_history(&f.task)
            .unwrap()
            .iter()
            .any(|e| e.event == "source_repair_reserved")
    );
    f.assert_original();
}

crate::scenario_table!(
    cancelled_source_repair_scenarios,
    failed_requeued_publisher_is_inspected_and_submitted_without_a_successor,
    failed_requeued_direct_publisher_retains_authority_and_drift_refusals,
    published_completion_requeue_adopts_without_reimplementation,
    published_requeue_refuses_changed_scope_head_and_incomplete_source,
    stopped_successor_is_explicitly_bound_when_repairing_older_publication,
    legacy_publication_behind_a_stopped_successor_cannot_start_fresh,
    cancelled_published_repair_accepts_fresh_generation,
    cancelled_candidate_requires_explicit_intent_and_continues_same_branch,
    cancelled_admission_refuses_identity_liveness_lease_and_evidence_drift,
    current_policy_is_captured_and_review_runs_at_its_scheduled_boundary,
    concurrent_replay_and_cancelled_attempt_keep_one_durable_reservation,
    exhausted_budget_and_unreadable_ledger_refuse_without_run_admission,
    scope_and_cancellation_are_rechecked_before_source_effects,
    model_context_and_forged_run_inputs_cannot_grant_repair_authority,
    lost_spawn_reply_reconciles_the_reserved_run_after_older_retries
);

crate::scenario_table!(
    published_completion_scenarios,
    unchanged_publication_retries_only_completion,
    completion_reuse_refuses_drift,
    changed_base_gets_fresh_native_validation,
    concurrent_and_lost_admission_keep_one_completion_budget,
    unknown_merge_reply_is_reconciled_without_republication,
    configured_review_requires_unchanged_host_certificate,
    refused_receipts_stop_before_agent_or_republication,
    enabled_completion_refuses_repair_and_mutation_provenance,
    expired_completion_budget_never_merges,
    later_prerequisite_read_failure_resumes_only_completion,
    prerequisite_read_after_mutation_provenance_never_resumes
);
