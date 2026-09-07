#![allow(missing_docs)]

use super::*;

use orbit_common::test_fixtures::TEST_GEMINI_MODEL;
use orbit_types::workflow::activity_job::{AgentLoopSpec, OnDenial, Provider};

use crate::CrewConfig;
use crate::activity_job::load_activity_asset;

use super::crew_overridden_recovery_spec;

/// [ORB-00414] Audit-write failures on the recovery path are recorded (counter
/// + degraded flag) but never fatal: recovery still runs and the job succeeds.
#[test]
fn recovery_path_audit_failures_are_recorded_not_fatal() {
    let original_error = retryable_error("flaky", "dirty checkout");
    let host = RecoveryHost::new([
        (
            "flaky",
            vec![
                Err(original_error.clone()),
                Err(original_error.clone()),
                Ok(json!({"fixed": true})),
            ],
        ),
        ("recover", vec![Ok(json!({"recovered": true}))]),
    ]);
    let job = recovery_job(Some("recover"), Some("wide"), "flaky", Some("narrow"), 2);
    let run_id = "run-recovery-degraded-audit";
    let writer = std::sync::Arc::new(failing_sink_writer(run_id));

    let outcome = execute_job(&job, Value::Null, run_id, writer.clone(), &host)
        .expect("job should recover despite audit sink failures");

    assert!(outcome.success, "recovery should still succeed");
    assert_eq!(host.action_count("recover"), 1, "recovery must have run");
    assert!(
        outcome.degraded_audit,
        "audit trail must be marked degraded after recovery-path sink failures"
    );
    assert!(outcome.audit_failures > 0);
    assert!(writer.degraded_audit());
}

#[test]
fn recovery_success_runs_one_post_recovery_attempt_with_exact_input_and_fs_profile() {
    let original_error = retryable_error("flaky", "dirty checkout");
    let host = RecoveryHost::new([
        (
            "flaky",
            vec![
                Err(original_error.clone()),
                Err(original_error.clone()),
                Ok(json!({"fixed": true})),
            ],
        ),
        ("recover", vec![Ok(json!({"recovered": true}))]),
    ]);
    let job = recovery_job(Some("recover"), Some("wide"), "flaky", Some("narrow"), 2);
    let writer = std::sync::Arc::new(test_writer("run-recovery-success"));

    let outcome = execute_job(
        &job,
        Value::Null,
        "run-recovery-success",
        writer.clone(),
        &host,
    )
    .expect("job should recover");

    assert!(outcome.success);
    assert_eq!(host.actions(), vec!["flaky", "flaky", "recover", "flaky"]);
    assert_eq!(host.action_count("recover"), 1);
    assert_eq!(
        host.input_for_action("recover"),
        Some(json!({
            "failed_step_id": "build",
            "step_id": "recover",
            "activity_name": "flaky",
            "error_message": original_error.to_string(),
            "attempt": 2,
            "max_attempts": 2
        }))
    );
    assert_eq!(
        host.fs_profile_for_action("recover"),
        Some(Some("narrow".to_string()))
    );

    let events = writer.events_snapshot().expect("audit snapshot");
    let recovery_events = recovery_events(&events);
    assert_eq!(recovery_events.len(), 1);
    assert!(matches!(
        recovery_events[0].kind,
        V2AuditEventKind::StepRecoveryAttempted {
            ref step_id,
            ref recovery_activity,
            recovery_succeeded: true,
            ..
        } if step_id == "build" && recovery_activity == "recover"
    ));
}

#[test]
fn recovery_success_with_post_recovery_failure_surfaces_re_run_error() {
    let original_error = retryable_error("flaky", "first failure");
    let post_recovery_error = retryable_error("flaky", "post recovery still failing");
    let host = RecoveryHost::new([
        (
            "flaky",
            vec![
                Err(original_error.clone()),
                Err(original_error.clone()),
                Err(post_recovery_error.clone()),
            ],
        ),
        ("recover", vec![Ok(json!({"recovered": true}))]),
    ]);
    let job = recovery_job(Some("recover"), None, "flaky", None, 2);
    let writer = std::sync::Arc::new(test_writer("run-post-recovery-failure"));

    let err = execute_job(
        &job,
        Value::Null,
        "run-post-recovery-failure",
        writer.clone(),
        &host,
    )
    .expect_err("post-recovery failure should surface the re-run error");

    assert!(err.to_string().contains(&post_recovery_error.to_string()));
    assert!(err.to_string().contains(&original_error.to_string()));
    assert_eq!(host.action_count("recover"), 1);
    let events = writer.events_snapshot().expect("audit snapshot");
    assert!(matches!(
        events.iter().find(|event| matches!(
            event.kind,
            V2AuditEventKind::StepPostRecoveryAttempt { .. }
        )).map(|event| &event.kind),
        Some(V2AuditEventKind::StepPostRecoveryAttempt {
            outcome,
            error_message: Some(error_message),
            ..
        }) if outcome == "error" && error_message.contains(&post_recovery_error.to_string())
    ));
    assert!(matches!(
        events.iter().find(|event| matches!(
            event.kind,
            V2AuditEventKind::StepFinished { .. }
        )).map(|event| &event.kind),
        Some(V2AuditEventKind::StepFinished {
            error_message: Some(error_message),
            ..
        }) if error_message.contains(&post_recovery_error.to_string())
    ));
}

#[test]
fn successful_vcs_recovery_reports_the_new_failure_with_original_context() {
    let original = recoverable_vcs_conflict();
    let remaining = retryable_error("flaky", "prepared base moved after recovery");
    let host = RecoveryHost::new([
        ("flaky", vec![Err(original), Err(remaining.clone())]),
        ("recover", vec![Ok(json!({"recovered": true}))]),
    ]);
    let job = recovery_job(Some("recover"), None, "flaky", None, 1);
    let error = execute_job(
        &job,
        Value::Null,
        "run-new-recovery-failure",
        Arc::new(test_writer("run-new-recovery-failure")),
        &host,
    )
    .unwrap_err();
    assert!(error.to_string().contains(&remaining.to_string()));
    assert!(error.to_string().contains("conflicting paths"));
    assert_eq!(host.actions(), vec!["flaky", "recover", "flaky"]);
}

#[test]
fn recovery_activity_error_returns_original_error_text() {
    let original_error = retryable_error("flaky", "precondition failed");
    let host = RecoveryHost::new([
        ("flaky", vec![Err(original_error.clone())]),
        (
            "recover",
            vec![Err(retryable_error("recover", "could not fix"))],
        ),
    ]);
    let job = recovery_job(Some("recover"), None, "flaky", None, 1);
    let writer = std::sync::Arc::new(test_writer("run-recovery-error"));

    let err = execute_job(
        &job,
        Value::Null,
        "run-recovery-error",
        writer.clone(),
        &host,
    )
    .expect_err("recovery error should surface original error");

    assert_eq!(err.to_string(), original_error.to_string());
    assert_eq!(host.action_count("recover"), 1);
    let events = writer.events_snapshot().expect("audit snapshot");
    assert!(matches!(
        recovery_events(&events)[0].kind,
        V2AuditEventKind::StepRecoveryAttempted {
            recovery_succeeded: false,
            ..
        }
    ));
}

#[test]
fn recovery_failure_is_redacted_and_persisted_alongside_original_conflict() {
    let original = recoverable_vcs_conflict();
    let secret = "sk-proj-abcdefghijklmnopqrstuvwxyz0123456789";
    let host = RecoveryHost::new([
        ("flaky", vec![Err(original.clone())]),
        (
            "recover",
            vec![Err(retryable_error(
                "recover",
                &format!(
                    "sandbox preparation failed: Authorization: Bearer {secret} {}",
                    "界".repeat(5000),
                ),
            ))],
        ),
    ]);
    let job = recovery_job(Some("recover"), None, "flaky", None, 1);
    let writer = Arc::new(test_writer("run-redacted-recovery"));
    let error = execute_job(
        &job,
        Value::Null,
        "run-redacted-recovery",
        writer.clone(),
        &host,
    )
    .expect_err("recovery failure must preserve original conflict");
    assert_eq!(error.to_string(), original.to_string());
    let events = writer.events_snapshot().unwrap();
    let event = serde_json::to_value(recovery_events(&events)[0]).unwrap();
    assert_eq!(event["failure_phase"], "dispatch");
    let message = event["error_message"].as_str().unwrap();
    assert!(message.contains("sandbox preparation failed"));
    assert!(!message.contains(secret));
    assert!(message.chars().count() <= 4097);
    assert_eq!(event["recovery_succeeded"], false);

    // Persisted older rows remain readable with the new optional evidence.
    let old: V2AuditEventKind = serde_json::from_value(json!({
        "body_kind": "step_recovery_attempted", "step_id": "sync_base",
        "recovery_activity": "recover", "recovery_succeeded": false,
    }))
    .unwrap();
    assert!(matches!(
        old,
        V2AuditEventKind::StepRecoveryAttempted {
            failure_phase: None,
            error_message: None,
            ..
        }
    ));
}

#[test]
fn pr_recovery_projects_rendered_candidate_context_without_overriding_run_authority() {
    let host = RecoveryHost::new([
        (
            "flaky",
            vec![Err(recoverable_vcs_conflict()), Ok(json!({}))],
        ),
        ("pr_conflict_recovery", vec![Ok(json!({}))]),
    ]);
    let mut job = recovery_job(Some("pr_conflict_recovery"), None, "flaky", None, 1);
    let JobV2StepBody::Target(target) = &mut job.steps[0].body else {
        panic!("target")
    };
    target.default_input = Some(json!({
        "completed_task_ids": ["T-candidate"],
        "workspace_path": "{{ input.assigned }}",
        "head": "candidate-branch", "head_sha": "candidate-sha",
        "pr_number": "123", "run_id": "stale-input-run",
        "completion": "done", "published_head_sha": "candidate-sha",
        "base": "agent-main", "base_sync": "remote",
    }));
    let writer = Arc::new(test_writer("run-candidate-context"));
    execute_job(
        &job,
        json!({"assigned": "/assigned/worktree", "task_ids": ["T-other"], "completion": "done"}),
        "run-candidate-context",
        writer,
        &host,
    )
    .unwrap();
    let input = host.input_for_action("pr_conflict_recovery").unwrap();
    let asset = load_activity_asset(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../orbit-core/assets/activities/pr_conflict_recovery.yaml"
    )))
    .unwrap();
    let ActivityV2Spec::AgentLoop(agent) = &asset.spec.spec else {
        panic!("conflict recovery must remain an agent leaf")
    };
    assert!(
        agent
            .instruction
            .contains("Do not stage, commit, continue, abort")
    );
    assert!(
        asset.spec.output_schema_json["properties"]
            .get("recovered")
            .is_none(),
        "agent output cannot claim Git recovery authority"
    );
    let schema = jsonschema::JSONSchema::compile(&asset.spec.input_schema_json).unwrap();
    let mut agent_input = input.clone();
    agent_input.as_object_mut().unwrap().remove("step_id");
    assert!(
        schema.is_valid(&agent_input),
        "recovery input must satisfy the shipped strict schema: {agent_input}"
    );
    assert_eq!(input["workspace_path"], "/assigned/worktree");
    assert_eq!(input["repo_root"], input["workspace_path"]);
    assert_eq!(input["task_ids"], json!(["T-candidate"]));
    assert_eq!(input["run_id"], "run-candidate-context");
    assert_eq!(input["failed_step_input"]["head"], "candidate-branch");
    assert_eq!(input["failed_step_input"]["head_sha"], "candidate-sha");
    assert_eq!(input["failed_step_input"]["pr_number"], "123");
    assert_eq!(input["failed_step_input"]["completion"], "done");
    assert_eq!(
        input["failed_step_input"]["published_head_sha"],
        "candidate-sha"
    );
    assert!(input.get("completion").is_none());
    assert_eq!(
        host.actions(),
        vec!["flaky", "pr_conflict_recovery", "flaky"]
    );
}

#[test]
fn step_level_recovery_activity_runs_without_job_level_recovery() {
    let original_error = retryable_error("flaky", "dirty checkout");
    let host = RecoveryHost::new([
        (
            "flaky",
            vec![Err(original_error.clone()), Ok(json!({"fixed": true}))],
        ),
        ("recover_step", vec![Ok(json!({"recovered": true}))]),
    ]);
    let mut job = recovery_job(None, None, "flaky", Some("narrow"), 1);
    job.steps[0].recovery_activity = Some("recover_step".to_string());
    job.steps[0].resolved_recovery_activity =
        Some(deterministic_activity("recover_step", Some("wide")));
    let writer = std::sync::Arc::new(test_writer("run-step-recovery-success"));

    let outcome = execute_job(
        &job,
        Value::Null,
        "run-step-recovery-success",
        writer.clone(),
        &host,
    )
    .expect("job should recover through step-level activity");

    assert!(outcome.success);
    assert_eq!(host.actions(), vec!["flaky", "recover_step", "flaky"]);
    assert_eq!(host.action_count("recover_step"), 1);
    assert_eq!(
        host.fs_profile_for_action("recover_step"),
        Some(Some("narrow".to_string()))
    );
    let events = writer.events_snapshot().expect("audit snapshot");
    assert!(matches!(
        recovery_events(&events)[0].kind,
        V2AuditEventKind::StepRecoveryAttempted {
            ref recovery_activity,
            ..
        } if recovery_activity == "recover_step"
    ));
}

#[test]
fn recovery_agent_loop_uses_run_crew_config() {
    let host = RecoveryHost::empty().with_recovery_config(CrewConfig {
        provider: Some(Provider::Gemini),
        model: Some(TEST_GEMINI_MODEL.to_string()),
        reasoning_effort: None,
    });
    let ctx = recovery_exec_ctx(&host);
    let recovery = agent_loop_recovery_activity(recovery_agent_loop_spec(Provider::Claude, None));

    let overridden = crew_overridden_recovery_spec(&recovery, &ctx, &json!({}))
        .expect("generic recovery crew resolution should succeed")
        .expect("run crew should resolve");

    let ActivityV2Spec::AgentLoop(spec) = overridden else {
        panic!("expected agent_loop recovery spec");
    };
    assert_eq!(spec.provider, Provider::Gemini);
    assert_eq!(spec.model.as_deref(), Some(TEST_GEMINI_MODEL));
}

#[test]
fn step_failure_recovery_uses_the_lane_middleweight_config() {
    for (provider, model) in [
        (Provider::Codex, "gpt-5.6-terra"),
        (Provider::Claude, "sonnet"),
    ] {
        let host = RecoveryHost::empty().with_recovery_config(CrewConfig {
            provider: Some(provider),
            model: Some(model.to_string()),
            reasoning_effort: None,
        });
        let ctx = recovery_exec_ctx(&host);
        let recovery = step_failure_recovery_agent_loop_activity(recovery_agent_loop_spec(
            Provider::Gemini,
            Some("inline-model"),
        ));

        let overridden = crew_overridden_recovery_spec(
            &recovery,
            &ctx,
            &json!({ "crew": "qa", "crew_config_key": "workflow.system_crew" }),
        )
        .expect("middleweight recovery config should resolve")
        .expect("agent loop recovery should produce a dispatch spec");
        let ActivityV2Spec::AgentLoop(spec) = overridden else {
            panic!("expected agent_loop recovery spec");
        };
        assert_eq!(spec.provider, provider);
        assert_eq!(spec.model.as_deref(), Some(model));
    }
}

#[test]
fn step_failure_recovery_requires_a_middleweight_config() {
    let host = RecoveryHost::empty();
    let ctx = recovery_exec_ctx(&host);
    let recovery = step_failure_recovery_agent_loop_activity(recovery_agent_loop_spec(
        Provider::Codex,
        Some("gpt-5.6-sol"),
    ));

    let err = crew_overridden_recovery_spec(&recovery, &ctx, &json!({ "system_crew": true }))
        .expect_err("step recovery must not fall back to its implementation crew");

    assert!(err.to_string().contains("workflow.system_crew"));
}

#[test]
fn unknown_step_recovery_activity_name_is_job_validation_during_catalog_resolution() {
    let yaml = r#"
schemaVersion: 2
kind: Job
metadata:
  name: missing_step_recovery
spec:
  state: enabled
  steps:
    - id: build
      recovery_activity: missing
      spec:
        type: deterministic
        action: flaky
"#;
    let mut job = load_job_asset(yaml).expect("job yaml").spec;
    let catalog = V2ActivityCatalog::new();

    let err = resolve_job_catalog_refs_for_execution(&mut job, &catalog)
        .expect_err("missing step recovery activity should fail resolution");

    assert!(matches!(
        err,
        DispatchError::JobValidation(ref message)
            if message.contains("step `build`: recovery_activity `missing` not found")
    ));
}

#[test]
fn non_retryable_failure_skips_recovery_and_audit_event() {
    let host = RecoveryHost::new([
        (
            "flaky",
            vec![Err(DispatchError::ToolDenied {
                tool_name: "fs.write".to_string(),
                iteration: 1,
            })],
        ),
        ("recover", vec![Ok(json!({"recovered": true}))]),
    ]);
    let job = recovery_job(Some("recover"), None, "flaky", None, 2);
    let writer = std::sync::Arc::new(test_writer("run-non-retryable"));

    let err = execute_job(
        &job,
        Value::Null,
        "run-non-retryable",
        writer.clone(),
        &host,
    )
    .expect_err("tool denial should bypass recovery");

    assert!(matches!(err, DispatchError::ToolDenied { .. }));
    assert_eq!(host.action_count("recover"), 0);
    assert!(recovery_events(&writer.events_snapshot().unwrap()).is_empty());
}

#[test]
fn typed_vcs_conflict_invokes_pr_recovery_once_and_retries_the_same_step_once() {
    let conflict = recoverable_vcs_conflict();
    let host = RecoveryHost::new([
        (
            "flaky",
            vec![
                Err(conflict.clone()),
                Ok(json!({"decision": "reused_recovery"})),
            ],
        ),
        (
            "pr_conflict_recovery",
            vec![Ok(json!({"recovered": false}))],
        ),
    ]);
    let mut job = recovery_job(None, None, "flaky", None, 4);
    job.steps[0].recovery_activity = Some("pr_conflict_recovery".to_string());
    job.steps[0].resolved_recovery_activity =
        Some(deterministic_activity("pr_conflict_recovery", None));
    let writer = Arc::new(test_writer("run-pr-conflict-recovered"));

    let outcome = execute_job(
        &job,
        Value::Null,
        "run-pr-conflict-recovered",
        writer.clone(),
        &host,
    )
    .expect("typed conflict should use the bounded recovery seam");

    assert!(outcome.success);
    assert_eq!(
        host.actions(),
        vec!["flaky", "pr_conflict_recovery", "flaky"],
        "typed conflicts bypass ordinary retry and get one recovery plus one deterministic retry"
    );
    assert_eq!(host.action_count("pr_conflict_recovery"), 1);
    let input = host
        .input_for_action("pr_conflict_recovery")
        .expect("recovery input");
    assert_eq!(input["recovery_kind"], "vcs_conflict");
    assert_eq!(input["operation"], "git_rebase");
    assert_eq!(input["original_base_sha"], "base-before");
    assert_eq!(input["target_base_sha"], "base-target");
    assert_eq!(input["conflicting_paths"], json!(["src/lib.rs"]));
    assert_eq!(input["crew"], "qa");
    assert_eq!(input["crew_config_key"], "workflow.system_crew");
    assert_eq!(recovery_events(&writer.events_snapshot().unwrap()).len(), 1);
}

#[test]
fn non_conflict_vcs_failure_does_not_invoke_pr_conflict_recovery() {
    let original = retryable_error("git_rebase", "remote lookup failed");
    let host = RecoveryHost::new([
        ("flaky", vec![Err(original.clone())]),
        ("pr_conflict_recovery", vec![Ok(json!({"recovered": true}))]),
    ]);
    let mut job = recovery_job(None, None, "flaky", None, 1);
    job.steps[0].recovery_activity = Some("pr_conflict_recovery".to_string());
    job.steps[0].resolved_recovery_activity =
        Some(deterministic_activity("pr_conflict_recovery", None));
    let writer = Arc::new(test_writer("run-pr-non-conflict"));

    let error = execute_job(
        &job,
        Value::Null,
        "run-pr-non-conflict",
        writer.clone(),
        &host,
    )
    .expect_err("unrelated VCS failure must remain authoritative");

    assert_eq!(error.to_string(), original.to_string());
    assert_eq!(host.actions(), vec!["flaky"]);
    assert!(recovery_events(&writer.events_snapshot().unwrap()).is_empty());
}

#[test]
fn exhausted_pr_conflict_recovery_preserves_the_original_typed_error() {
    let conflict = recoverable_vcs_conflict();
    let host = RecoveryHost::new([
        ("flaky", vec![Err(conflict.clone())]),
        (
            "pr_conflict_recovery",
            vec![Err(retryable_error(
                "pr_conflict_recovery",
                "validation failed",
            ))],
        ),
    ]);
    let mut job = recovery_job(None, None, "flaky", None, 3);
    job.steps[0].recovery_activity = Some("pr_conflict_recovery".to_string());
    job.steps[0].resolved_recovery_activity =
        Some(deterministic_activity("pr_conflict_recovery", None));

    let writer = Arc::new(test_writer("run-pr-conflict-exhausted"));
    let error = execute_job(
        &job,
        Value::Null,
        "run-pr-conflict-exhausted",
        writer.clone(),
        &host,
    )
    .expect_err("failed recovery must preserve the typed conflict");

    assert_eq!(error.to_string(), conflict.to_string());
    assert_eq!(host.actions(), vec!["flaky", "pr_conflict_recovery"]);
    let event = serde_json::to_value(recovery_events(&writer.events_snapshot().unwrap())[0])
        .expect("serialize recovery event");
    assert_eq!(event["failure_phase"], "dispatch");
    assert!(
        event["error_message"]
            .as_str()
            .is_some_and(|message| message.contains("validation failed"))
    );
}

#[test]
fn worktree_integrity_failure_bypasses_retry_then_recovers_once() {
    let integrity_error = worktree_integrity_error("run-integrity-recovered");
    let host = RecoveryHost::new([
        (
            "flaky",
            vec![Err(integrity_error.clone()), Ok(json!({"fixed": true}))],
        ),
        ("recover", vec![Ok(json!({"recovered": true}))]),
    ]);
    let job = recovery_job(Some("recover"), None, "flaky", None, 3);
    let writer = std::sync::Arc::new(test_writer("run-integrity-recovered"));

    let outcome = execute_job(
        &job,
        Value::Null,
        "run-integrity-recovered",
        writer.clone(),
        &host,
    )
    .expect("configured recovery should get one chance to repair integrity state");

    assert!(outcome.success);
    assert_eq!(
        host.actions(),
        vec!["flaky", "recover", "flaky"],
        "ordinary retry must be skipped before the single recovery attempt"
    );
    assert_eq!(host.action_count("recover"), 1);
    assert_eq!(
        host.input_for_action("recover"),
        Some(json!({
            "failed_step_id": "build",
            "step_id": "recover",
            "activity_name": "flaky",
            "error_message": integrity_error.to_string(),
            "attempt": 1,
            "max_attempts": 3
        }))
    );

    let events = writer.events_snapshot().expect("audit snapshot");
    let recovery_events = recovery_events(&events);
    assert_eq!(recovery_events.len(), 1);
    assert!(matches!(
        recovery_events[0].kind,
        V2AuditEventKind::StepRecoveryAttempted {
            ref step_id,
            ref recovery_activity,
            recovery_succeeded: true,
            ..
        } if step_id == "build" && recovery_activity == "recover"
    ));
}

#[test]
fn worktree_integrity_without_retry_block_uses_step_recovery_once() {
    let integrity_error = worktree_integrity_error("run-integrity-step-recovery");
    let host = RecoveryHost::new([
        (
            "flaky",
            vec![Err(integrity_error), Ok(json!({"fixed": true}))],
        ),
        ("recover_step", vec![Ok(json!({"recovered": true}))]),
    ]);
    let mut job = recovery_job(None, None, "flaky", None, 1);
    job.steps[0].retry = None;
    job.steps[0].recovery_activity = Some("recover_step".to_string());
    job.steps[0].resolved_recovery_activity = Some(deterministic_activity("recover_step", None));
    let writer = std::sync::Arc::new(test_writer("run-integrity-step-recovery"));

    let outcome = execute_job(
        &job,
        Value::Null,
        "run-integrity-step-recovery",
        writer.clone(),
        &host,
    )
    .expect("step-level recovery should run without a retry block");

    assert!(outcome.success);
    assert_eq!(host.actions(), vec!["flaky", "recover_step", "flaky"]);
    assert_eq!(host.action_count("recover_step"), 1);
    assert_eq!(recovery_events(&writer.events_snapshot().unwrap()).len(), 1);
}

#[test]
fn worktree_integrity_without_recovery_returns_original_without_retry() {
    let integrity_error = worktree_integrity_error("run-integrity-no-recovery");
    let host = RecoveryHost::new([("flaky", vec![Err(integrity_error.clone())])]);
    let job = recovery_job(None, None, "flaky", None, 3);
    let writer = std::sync::Arc::new(test_writer("run-integrity-no-recovery"));

    let err = execute_job(
        &job,
        Value::Null,
        "run-integrity-no-recovery",
        writer.clone(),
        &host,
    )
    .expect_err("integrity failure without configured recovery must fail closed");

    assert_eq!(err.to_string(), integrity_error.to_string());
    assert_eq!(host.actions(), vec!["flaky"]);
    assert!(recovery_events(&writer.events_snapshot().unwrap()).is_empty());
}

#[test]
fn worktree_integrity_unsuccessful_recovery_returns_original() {
    let integrity_error = worktree_integrity_error("run-integrity-recovery-failed");
    let host = RecoveryHost::new([
        ("flaky", vec![Err(integrity_error.clone())]),
        (
            "recover",
            vec![Err(retryable_error("recover", "unsafe to reconcile"))],
        ),
    ]);
    let job = recovery_job(Some("recover"), None, "flaky", None, 3);
    let writer = std::sync::Arc::new(test_writer("run-integrity-recovery-failed"));

    let err = execute_job(
        &job,
        Value::Null,
        "run-integrity-recovery-failed",
        writer.clone(),
        &host,
    )
    .expect_err("unsuccessful recovery must preserve the integrity failure");

    assert_eq!(err.to_string(), integrity_error.to_string());
    assert_eq!(host.actions(), vec!["flaky", "recover"]);
    let events = writer.events_snapshot().expect("audit snapshot");
    let recovery_events = recovery_events(&events);
    assert_eq!(recovery_events.len(), 1);
    assert!(matches!(
        recovery_events[0].kind,
        V2AuditEventKind::StepRecoveryAttempted {
            recovery_succeeded: false,
            ..
        }
    ));
}

#[test]
fn worktree_integrity_post_recovery_failure_surfaces_the_new_error() {
    let integrity_error = worktree_integrity_error("run-integrity-post-failed");
    let post_recovery_error = retryable_error("flaky", "still unsafe");
    let host = RecoveryHost::new([
        (
            "flaky",
            vec![
                Err(integrity_error.clone()),
                Err(post_recovery_error.clone()),
            ],
        ),
        ("recover", vec![Ok(json!({"recovered": true}))]),
    ]);
    let job = recovery_job(Some("recover"), None, "flaky", None, 3);
    let writer = std::sync::Arc::new(test_writer("run-integrity-post-failed"));

    let err = execute_job(
        &job,
        Value::Null,
        "run-integrity-post-failed",
        writer.clone(),
        &host,
    )
    .expect_err("failed post-recovery attempt must surface the remaining failure");

    assert!(err.to_string().contains(&post_recovery_error.to_string()));
    assert!(err.to_string().contains(&integrity_error.to_string()));
    assert_eq!(host.actions(), vec!["flaky", "recover", "flaky"]);
    assert_eq!(recovery_events(&writer.events_snapshot().unwrap()).len(), 1);
}

#[test]
fn no_recovery_activity_preserves_success_and_failure_paths() {
    let original_error = retryable_error("flaky", "still failing");
    let failing_host = RecoveryHost::new([("flaky", vec![Err(original_error.clone())])]);
    let failing_job = recovery_job(None, None, "flaky", None, 1);
    let failing_writer = std::sync::Arc::new(test_writer("run-no-recovery-failure"));

    let err = execute_job(
        &failing_job,
        Value::Null,
        "run-no-recovery-failure",
        failing_writer.clone(),
        &failing_host,
    )
    .expect_err("retryable failure should remain the original error");

    assert_eq!(err.to_string(), original_error.to_string());
    assert!(recovery_events(&failing_writer.events_snapshot().unwrap()).is_empty());

    let success_host = RecoveryHost::new([("stable", vec![Ok(json!({"ok": true}))])]);
    let success_job = recovery_job(None, None, "stable", None, 1);
    let success_writer = std::sync::Arc::new(test_writer("run-no-recovery-success"));

    let outcome = execute_job(
        &success_job,
        Value::Null,
        "run-no-recovery-success",
        success_writer.clone(),
        &success_host,
    )
    .expect("success path should remain unchanged");

    assert!(outcome.success);
    assert!(recovery_events(&success_writer.events_snapshot().unwrap()).is_empty());
}

#[test]
fn terminal_failure_activity_runs_once_and_preserves_original_error() {
    let host = ScriptedHost::new([
        (
            "build",
            vec![Action::Err(retryable_error("build", "compile failed"))],
        ),
        (
            "publish_failure",
            vec![Action::Ok(json!({"published": true}))],
        ),
    ]);
    let mut job = recovery_job(None, None, "build", None, 1);
    job.failure_activity = Some("publish_failure".to_string());
    job.resolved_failure_activity = Some(deterministic_activity("publish_failure", None));

    let error = execute_job(
        &job,
        json!({"task_id": "ORB-FAILURE-HOOK"}),
        "run-failure-hook",
        Arc::new(test_writer("run-failure-hook")),
        &host,
    )
    .expect_err("the failure hook must not replace the original step failure");

    assert!(
        error.to_string().contains("compile failed"),
        "original error remains authoritative: {error}"
    );
    assert_eq!(host.call_count("build"), 1);
    assert_eq!(host.call_count("publish_failure"), 1);
}

#[test]
fn unknown_recovery_activity_name_is_job_validation_during_catalog_resolution() {
    let yaml = r#"
schemaVersion: 2
kind: Job
metadata:
  name: missing_recovery
spec:
  state: enabled
  recovery_activity: missing
  steps:
    - id: build
      spec:
        type: deterministic
        action: flaky
"#;
    let mut job = load_job_asset(yaml).expect("job yaml").spec;
    let catalog = V2ActivityCatalog::new();

    let err = resolve_job_catalog_refs_for_execution(&mut job, &catalog)
        .expect_err("missing recovery activity should fail resolution");

    assert!(matches!(
        err,
        DispatchError::JobValidation(ref message)
            if message.contains("recovery_activity `missing` not found")
    ));
}

fn recovery_job(
    recovery_name: Option<&str>,
    recovery_fs_profile: Option<&str>,
    step_action: &str,
    step_fs_profile: Option<&str>,
    max_attempts: u32,
) -> JobV2 {
    JobV2 {
        state: JobScheduleState::Enabled,
        default_input: None,
        recovery_activity: recovery_name.map(str::to_string),
        resolved_recovery_activity: recovery_name
            .map(|name| deterministic_activity(name, recovery_fs_profile)),
        failure_activity: None,
        resolved_failure_activity: None,
        max_active_runs: 1,
        kind: JobKind::Workflow,
        steps: vec![JobV2Step {
            id: "build".to_string(),
            when: None,
            retry: Some(RetrySpec {
                max_attempts,
                initial_backoff_ms: 1,
                backoff_cap_ms: 1,
                backoff_strategy: BackoffStrategy::Linear,
            }),
            recovery_activity: None,
            resolved_recovery_activity: None,
            body: JobV2StepBody::Target(TargetStep {
                spec: deterministic_activity(step_action, None).spec,
                activity_name: None,
                input_schema_json: None,
                fs_profile: step_fs_profile.map(str::to_string),
                default_input: None,
                timeout_seconds: 0,
                session: None,
            }),
        }],
    }
}

fn deterministic_activity(action: &str, fs_profile: Option<&str>) -> ActivityV2 {
    ActivityV2 {
        description: format!("deterministic {action}"),
        input_schema_json: json!({}),
        output_schema_json: json!({}),
        fs_profile: fs_profile.map(str::to_string),
        spec: ActivityV2Spec::Deterministic(DeterministicSpec {
            action: action.to_string(),
            config: Value::Null,
        }),
    }
}

fn agent_loop_recovery_activity(spec: AgentLoopSpec) -> ResolvedRecoveryActivity {
    ResolvedRecoveryActivity {
        name: "recover".to_string(),
        spec: ActivityV2Spec::AgentLoop(spec),
    }
}

fn step_failure_recovery_agent_loop_activity(spec: AgentLoopSpec) -> ResolvedRecoveryActivity {
    ResolvedRecoveryActivity {
        name: "step_failure_recovery".to_string(),
        spec: ActivityV2Spec::AgentLoop(spec),
    }
}

fn recovery_agent_loop_spec(provider: Provider, model: Option<&str>) -> AgentLoopSpec {
    AgentLoopSpec {
        instruction: "recover carefully".to_string(),
        tools: Vec::new(),
        on_denial: OnDenial::Terminate,
        model: model.map(str::to_string),
        reasoning_effort: None,
        max_iterations: 1,
        backend: None,
        provider,
        wall_clock_timeout_seconds: 30,
        require_response_envelope: false,
        require_completion_envelope: true,
        proc_allowed_programs: None,
        trusted_host_execution: false,
    }
}

fn recovery_exec_ctx<'a>(host: &'a dyn RuntimeHost) -> ExecCtx<'a> {
    ExecCtx {
        run_id: "run-recovery-role".to_string(),
        audit: std::sync::Arc::new(test_writer("run-recovery-role")),
        host,
        input: json!({}),
        pipeline: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
        recovery_activity: None,
        failure_activity: None,
        item: None,
        iteration: None,
    }
}

fn retryable_error(action: &str, message: &str) -> DispatchError {
    DispatchError::DeterministicActionFailed {
        action: action.to_string(),
        message: message.to_string(),
    }
}

fn worktree_integrity_error(run_id: &str) -> DispatchError {
    DispatchError::WorktreeIntegrity {
        code: "worktree_integrity_ambiguous",
        diagnostic: format!(
            r#"{{"task_id":"ORB-10306","run_id":"{run_id}","primary_changed":true,"assigned_changed":true}}"#
        ),
    }
}

fn recoverable_vcs_conflict() -> DispatchError {
    DispatchError::RecoverableVcsConflict {
        operation: "git_rebase".to_string(),
        original_base_sha: "base-before".to_string(),
        target_base_sha: "base-target".to_string(),
        conflicting_paths: vec!["src/lib.rs".to_string()],
        diagnostic: "rebase stopped with conflicts".to_string(),
    }
}

fn recovery_events(events: &[V2AuditEvent]) -> Vec<&V2AuditEvent> {
    events
        .iter()
        .filter(|event| matches!(event.kind, V2AuditEventKind::StepRecoveryAttempted { .. }))
        .collect()
}

#[derive(Debug, Clone)]
struct DeterministicCall {
    action: String,
    input: Value,
    fs_profile: Option<String>,
}

struct RecoveryHost {
    responses: StdMutex<HashMap<String, VecDeque<Result<Value, DispatchError>>>>,
    calls: StdMutex<Vec<DeterministicCall>>,
    pending_fs_profiles: StdMutex<VecDeque<Option<String>>>,
    recovery_config: StdMutex<Option<CrewConfig>>,
}

impl RecoveryHost {
    fn empty() -> Self {
        Self::new([])
    }

    fn new<const N: usize>(responses: [(&str, Vec<Result<Value, DispatchError>>); N]) -> Self {
        Self {
            responses: StdMutex::new(
                responses
                    .into_iter()
                    .map(|(action, outcomes)| (action.to_string(), outcomes.into_iter().collect()))
                    .collect(),
            ),
            calls: StdMutex::new(Vec::new()),
            pending_fs_profiles: StdMutex::new(VecDeque::new()),
            recovery_config: StdMutex::new(None),
        }
    }

    fn with_recovery_config(self, config: CrewConfig) -> Self {
        *self.recovery_config.lock().expect("recovery config lock") = Some(config);
        self
    }

    fn actions(&self) -> Vec<String> {
        self.calls
            .lock()
            .expect("calls lock")
            .iter()
            .map(|call| call.action.clone())
            .collect()
    }

    fn action_count(&self, action: &str) -> usize {
        self.calls
            .lock()
            .expect("calls lock")
            .iter()
            .filter(|call| call.action == action)
            .count()
    }

    fn input_for_action(&self, action: &str) -> Option<Value> {
        self.calls
            .lock()
            .expect("calls lock")
            .iter()
            .find(|call| call.action == action)
            .map(|call| call.input.clone())
    }

    fn fs_profile_for_action(&self, action: &str) -> Option<Option<String>> {
        self.calls
            .lock()
            .expect("calls lock")
            .iter()
            .find(|call| call.action == action)
            .map(|call| call.fs_profile.clone())
    }
}

impl RuntimeHost for RecoveryHost {
    fn run_deterministic(
        &self,
        action: &str,
        _config: &Value,
        input: &Value,
        _tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        let fs_profile = self
            .pending_fs_profiles
            .lock()
            .expect("fs profiles lock")
            .pop_front()
            .unwrap_or(None);
        self.calls
            .lock()
            .expect("calls lock")
            .push(DeterministicCall {
                action: action.to_string(),
                input: input.clone(),
                fs_profile,
            });

        self.responses
            .lock()
            .expect("responses lock")
            .get_mut(action)
            .and_then(VecDeque::pop_front)
            .unwrap_or_else(|| Ok(json!({"action": action})))
    }

    fn resolve_cli_executor(
        &self,
        _provider: &str,
    ) -> Result<super::super::super::dispatcher::ResolvedCliExecutor, DispatchError> {
        Err(DispatchError::CliInvocationFailed(
            "test host: no CLI mapping".into(),
        ))
    }

    fn tool_context_for_activity(
        &self,
        _run_id: Option<&str>,
        fs_profile: Option<&str>,
        _fs_audit: Option<std::sync::Arc<dyn orbit_tools::FsAuditLogger>>,
        _proc_allowed_programs: Option<&[String]>,
    ) -> orbit_tools::ToolContext {
        self.pending_fs_profiles
            .lock()
            .expect("fs profiles lock")
            .push_back(fs_profile.map(str::to_string));
        orbit_tools::ToolContext::default()
    }

    fn system_crew_for_dispatch(&self) -> Option<String> {
        Some("qa".to_string())
    }

    fn agent_crew_config_for_input(
        &self,
        _input: &Value,
    ) -> Result<Option<CrewConfig>, DispatchError> {
        self.recovery_config
            .lock()
            .expect("recovery config lock")
            .clone()
            .map(Some)
            .ok_or_else(|| {
                DispatchError::JobValidation(
                    "workflow.system_crew test crew is unavailable".to_string(),
                )
            })
    }
}
