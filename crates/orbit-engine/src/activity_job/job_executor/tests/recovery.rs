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

// ----- [DANI-10438] Declared-failed outcomes reach the recovery leaf --------

/// Fake `claude` that replays the on-call shape: the first `failures`
/// invocations exit 0 with a `status: "failed"` envelope carrying a red-gate
/// diagnostic, and every later invocation declares `success`. Invocations are
/// counted in `<dir>/invocations` so a test can see the re-attempt happen.
fn declared_failure_provider(dir: &std::path::Path, failures: u32) -> std::path::PathBuf {
    let script = dir.join("claude");
    let counter = dir.join("invocations");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\ncat > /dev/null\n\
             n=$(cat '{counter}' 2>/dev/null || echo 0)\n\
             n=$((n + 1))\nprintf '%s' \"$n\" > '{counter}'\n\
             if [ \"$n\" -le {failures} ]; then\n\
             printf '%s\\n' '{DECLARED_FAILED_ENVELOPE}'\n\
             else\n\
             printf '%s\\n' '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}'\n\
             fi\n",
            counter = counter.display(),
        ),
    )
    .expect("write fake provider");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).expect("stat").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).expect("chmod");
    }
    script
}

const DECLARED_FAILED_ENVELOPE: &str = "{\"schemaVersion\":1,\"status\":\"failed\",\"result\":{},\
    \"error\":{\"code\":\"validation_failed\",\"message\":\"make ci-fast failed on pre-existing issues\"}}";
const DECLARED_FAILED_DIAGNOSTIC: &str = "make ci-fast failed on pre-existing issues";
/// More invocations than any test performs, so the provider never succeeds.
const ALWAYS_FAIL: u32 = 1_000;

fn provider_invocations(dir: &std::path::Path) -> usize {
    std::fs::read_to_string(dir.join("invocations"))
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

/// `implement_one` as shipped: a CLI agent loop with a step-level
/// `recovery_activity`, resolved to the deterministic `recover` action.
fn implement_step_with_recovery(recovery_name: &str, retry: Option<RetrySpec>) -> JobV2Step {
    let mut step = super::step::agent_implement_shaped_step("implement_one", retry);
    step.recovery_activity = Some(recovery_name.to_string());
    step.resolved_recovery_activity = Some(deterministic_activity(recovery_name, None));
    step
}

fn event_index(events: &[V2AuditEvent], pred: impl Fn(&V2AuditEventKind) -> bool) -> Option<usize> {
    events.iter().position(|event| pred(&event.kind))
}

/// The no-retry branch: the agent declares `failed` once, the step's recovery
/// leaf runs once, and the single re-attempt succeeds. The audit log carries
/// `step.recovery_attempted` before `step.post_recovery_attempt`.
#[test]
fn declared_failed_outcome_without_retry_dispatches_recovery_then_reattempts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let script = declared_failure_provider(temp.path(), 1);
    let host = ScriptedHost::new([("recover", vec![Action::Ok(json!({"recovered": true}))])])
        .with_cli_program(script.display().to_string());
    let job = job_with_steps(vec![implement_step_with_recovery("recover", None)]);
    let writer = std::sync::Arc::new(test_writer("run-declared-failed-recovery"));

    let outcome = execute_job(
        &job,
        json!({"task_id": "DANI-10438"}),
        "run-declared-failed-recovery",
        writer.clone(),
        &host,
    )
    .expect("execute_job ok");

    assert!(outcome.success, "{:?}", outcome.message);
    assert_eq!(
        host.call_count("recover"),
        1,
        "recovery must run exactly once"
    );
    assert_eq!(
        provider_invocations(temp.path()),
        2,
        "one declared failure, then exactly one post-recovery re-attempt"
    );
    let input = host.input_for_action("recover").expect("recovery input");
    assert_eq!(input["failed_step_id"], "implement_one");
    assert_eq!(input["attempt"], 1);
    assert_eq!(input["max_attempts"], 1);
    let error_message = input["error_message"].as_str().expect("error_message");
    assert!(error_message.contains("implement_one"), "{error_message}");
    assert!(
        error_message.contains(DECLARED_FAILED_DIAGNOSTIC),
        "{error_message}"
    );

    let events = writer.events_snapshot().expect("audit snapshot");
    let recovered = event_index(&events, |kind| {
        matches!(
            kind,
            V2AuditEventKind::StepRecoveryAttempted {
                step_id,
                recovery_activity,
                recovery_succeeded: true,
                ..
            } if step_id == "implement_one" && recovery_activity == "recover"
        )
    })
    .expect("step.recovery_attempted must be emitted");
    let reattempted = event_index(&events, |kind| {
        matches!(
            kind,
            V2AuditEventKind::StepPostRecoveryAttempt { step_id, outcome, .. }
                if step_id == "implement_one" && outcome == "success"
        )
    })
    .expect("step.post_recovery_attempt must be emitted");
    assert!(recovered < reattempted, "recovery precedes the re-attempt");
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        V2AuditEventKind::StepFinished { step_id, outcome, .. }
            if step_id == "implement_one" && outcome == "success"
    )));
}

/// The retry branch: recovery runs once, after retries are exhausted, with
/// the exhausted attempt count. When the re-attempt declares `failed` again,
/// the terminal error carries the agent's diagnostic [ORB-10449].
#[test]
fn declared_failed_outcome_after_retry_exhaustion_recovers_once_and_keeps_diagnostic() {
    let temp = tempfile::tempdir().expect("tempdir");
    let script = declared_failure_provider(temp.path(), ALWAYS_FAIL);
    let host = ScriptedHost::new([("recover", vec![Action::Ok(json!({"recovered": true}))])])
        .with_cli_program(script.display().to_string());
    let job = job_with_steps(vec![implement_step_with_recovery(
        "recover",
        Some(RetrySpec {
            max_attempts: 2,
            initial_backoff_ms: 1,
            backoff_cap_ms: 1,
            backoff_strategy: BackoffStrategy::Linear,
        }),
    )]);
    let writer = std::sync::Arc::new(test_writer("run-declared-failed-retry"));

    let err = execute_job(
        &job,
        json!({"task_id": "DANI-10438"}),
        "run-declared-failed-retry",
        writer.clone(),
        &host,
    )
    .expect_err("a failed re-attempt is terminal");

    let message = err.to_string();
    assert!(
        message.contains("post-recovery attempt failed"),
        "{message}"
    );
    assert!(message.contains("implement_one"), "{message}");
    assert!(message.contains(DECLARED_FAILED_DIAGNOSTIC), "{message}");
    assert!(
        !message.contains("completed with success=false"),
        "the generic fallback hides the real cause: {message}"
    );
    assert_eq!(
        host.call_count("recover"),
        1,
        "recovery runs once, after retries"
    );
    assert_eq!(
        provider_invocations(temp.path()),
        3,
        "two retried attempts, then one post-recovery re-attempt"
    );
    let input = host.input_for_action("recover").expect("recovery input");
    assert_eq!(input["attempt"], 2);
    assert_eq!(input["max_attempts"], 2);
    assert!(
        input["error_message"]
            .as_str()
            .is_some_and(|text| text.contains(DECLARED_FAILED_DIAGNOSTIC)),
        "{input}"
    );

    let events = writer.events_snapshot().expect("audit snapshot");
    let retried = event_index(&events, |kind| {
        matches!(kind, V2AuditEventKind::StepRetry { attempt: 1, .. })
    })
    .expect("retry precedes recovery");
    let recovered = event_index(&events, |kind| {
        matches!(
            kind,
            V2AuditEventKind::StepRecoveryAttempted {
                recovery_succeeded: true,
                ..
            }
        )
    })
    .expect("step.recovery_attempted must be emitted");
    let reattempted = event_index(&events, |kind| {
        matches!(
            kind,
            V2AuditEventKind::StepPostRecoveryAttempt { outcome, error_message: Some(error_message), .. }
                if outcome == "failed" && error_message.contains(DECLARED_FAILED_DIAGNOSTIC)
        )
    })
    .expect("step.post_recovery_attempt must be emitted");
    assert!(retried < recovered && recovered < reattempted);
}

/// A recovery leaf that fails leaves the declared-failed outcome exactly as it
/// was: same `failed` classification, same agent diagnostic, no re-attempt.
#[test]
fn failed_recovery_of_declared_failed_outcome_returns_it_unchanged() {
    let temp = tempfile::tempdir().expect("tempdir");
    let script = declared_failure_provider(temp.path(), ALWAYS_FAIL);
    let host = ScriptedHost::new([(
        "recover",
        vec![Action::Err(retryable_error("recover", "could not fix"))],
    )])
    .with_cli_program(script.display().to_string());
    let job = job_with_steps(vec![implement_step_with_recovery("recover", None)]);
    let writer = std::sync::Arc::new(test_writer("run-declared-failed-unrecovered"));

    let outcome = execute_job(
        &job,
        json!({"task_id": "DANI-10438"}),
        "run-declared-failed-unrecovered",
        writer.clone(),
        &host,
    )
    .expect("an unrecovered declared failure stays a failed outcome, not an error");

    assert!(!outcome.success);
    let message = outcome.message.expect("terminal message");
    assert!(message.contains("implement_one"), "{message}");
    assert!(message.contains(DECLARED_FAILED_DIAGNOSTIC), "{message}");
    assert_eq!(host.call_count("recover"), 1);
    assert_eq!(
        provider_invocations(temp.path()),
        1,
        "no re-attempt without recovery"
    );

    let events = writer.events_snapshot().expect("audit snapshot");
    assert!(matches!(
        recovery_events(&events)[0].kind,
        V2AuditEventKind::StepRecoveryAttempted {
            recovery_succeeded: false,
            ..
        }
    ));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, V2AuditEventKind::StepPostRecoveryAttempt { .. }))
    );
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        V2AuditEventKind::StepFinished { step_id, outcome, .. }
            if step_id == "implement_one" && outcome == "failed"
    )));
}

/// `pr_conflict_recovery` is still keyed on `RecoverableVcsConflict` alone: a
/// declared-failed outcome on a step that names it never dispatches the leaf.
#[test]
fn pr_conflict_recovery_ignores_declared_failed_outcome() {
    let temp = tempfile::tempdir().expect("tempdir");
    let script = declared_failure_provider(temp.path(), ALWAYS_FAIL);
    let host = ScriptedHost::new([]).with_cli_program(script.display().to_string());
    let job = job_with_steps(vec![implement_step_with_recovery(
        "pr_conflict_recovery",
        None,
    )]);
    let writer = std::sync::Arc::new(test_writer("run-declared-failed-pr-conflict"));

    let outcome = execute_job(
        &job,
        json!({"task_id": "DANI-10438"}),
        "run-declared-failed-pr-conflict",
        writer.clone(),
        &host,
    )
    .expect("execute_job ok");

    assert!(!outcome.success);
    assert_eq!(host.call_count("pr_conflict_recovery"), 0);
    assert_eq!(provider_invocations(temp.path()), 1);
    let events = writer.events_snapshot().expect("audit snapshot");
    assert!(recovery_events(&events).is_empty());
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

/// [ORB-12467] A 2.5 MB `primary_checkout_drift` diagnostic reached the
/// recovery input verbatim and was then copied into the envelope's `prompt`,
/// so `codex exec` rejected the 5.1 MB turn with `input_too_large` and the
/// recovery agent never started. The bound keeps both ends of the diagnostic
/// and leaves the envelope an order of magnitude below the 1 MiB ceiling.
#[test]
fn oversized_error_message_is_bounded_so_the_provider_envelope_stays_under_one_mib() {
    let head = "PRIMARY_CHECKOUT_DRIFT_HEAD";
    let tail = "PRIMARY_CHECKOUT_DRIFT_TAIL";
    let huge = format!(
        "{head}{}{tail}",
        "path_states-sha256-noise ".repeat(120_000)
    );
    assert!(huge.len() > 3_000_000, "fixture must exceed 3 MB");
    let original = retryable_error("flaky", &huge);
    let host = RecoveryHost::new([
        (
            "flaky",
            vec![Err(original.clone()), Ok(json!({"recovered": true}))],
        ),
        ("step_failure_recovery", vec![Ok(json!({}))]),
    ]);
    let mut job = recovery_job(Some("step_failure_recovery"), None, "flaky", None, 1);
    job.steps[0].id = "implement_one".to_string();
    let JobV2StepBody::Target(target) = &mut job.steps[0].body else {
        panic!("resolved failed target")
    };
    target.default_input = Some(json!({
        "task_id": "ORB-12467",
        "workspace_path": "/assigned/worktree",
        "repo_root": "/assigned/worktree",
    }));
    let run_id = "run-oversized-error-message";
    let writer = Arc::new(test_writer(run_id));

    execute_job(&job, json!({"task_id": "ORB-12467"}), run_id, writer, &host)
        .expect("bounded recovery input must reach dispatch");

    let input = host
        .input_for_action("step_failure_recovery")
        .expect("recovery dispatched");
    let error_message = input["error_message"].as_str().expect("string message");
    assert!(
        error_message.len() <= 64 * 1024,
        "error_message is {} B",
        error_message.len()
    );
    assert!(error_message.contains(head), "head must survive");
    assert!(error_message.contains(tail), "tail must survive");
    assert!(
        error_message.contains("[truncated: error_message is")
            && error_message.contains(&format!("orbit run show {run_id} --json")),
        "marker must name the original size and where the full text is: {}",
        &error_message[..error_message.len().min(400)]
    );

    // The shipped strict schema still accepts the bounded input.
    let asset = load_activity_asset(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../orbit-core/assets/activities/step_failure_recovery.yaml"
    )))
    .expect("shipped recovery activity");
    let schema =
        jsonschema::JSONSchema::compile(&asset.spec.input_schema_json).expect("compile schema");
    let mut agent_input = input.clone();
    agent_input
        .as_object_mut()
        .expect("object input")
        .remove("step_id");
    assert!(
        schema.is_valid(&agent_input),
        "bounded recovery input must satisfy the shipped schema"
    );

    let ActivityV2Spec::AgentLoop(spec) = &asset.spec.spec else {
        panic!("step failure recovery must remain an agent leaf")
    };
    let envelope = crate::activity_job::cli_runner::cli_agent_envelope_json(
        spec,
        run_id,
        &agent_input,
        None,
        &[],
        &[],
    )
    .expect("serialize provider envelope");
    assert!(
        envelope.len() < 1_048_576,
        "provider envelope is {} B; codex exec refuses above 1,048,576",
        envelope.len()
    );
}

/// [ORB-12467] `failed_step_input` carries the whole rendered target input, so
/// a single oversized leaf can blow the same ceiling. It is bounded in place:
/// the value stays the object both recovery schemas declare.
#[test]
fn oversized_failed_step_input_is_bounded_without_losing_its_object_shape() {
    let original = retryable_error("flaky", "implement_one failed");
    let host = RecoveryHost::new([
        (
            "flaky",
            vec![Err(original.clone()), Ok(json!({"recovered": true}))],
        ),
        ("step_failure_recovery", vec![Ok(json!({}))]),
    ]);
    let mut job = recovery_job(Some("step_failure_recovery"), None, "flaky", None, 1);
    job.steps[0].id = "implement_one".to_string();
    let JobV2StepBody::Target(target) = &mut job.steps[0].body else {
        panic!("resolved failed target")
    };
    target.default_input = Some(json!({
        "task_id": "ORB-12467",
        "workspace_path": "/assigned/worktree",
        "repo_root": "/assigned/worktree",
        "rendered_prompt": format!("PROMPT_HEAD{}PROMPT_TAIL", "x".repeat(3_000_000)),
    }));
    let run_id = "run-oversized-failed-step-input";
    let writer = Arc::new(test_writer(run_id));

    execute_job(&job, json!({"task_id": "ORB-12467"}), run_id, writer, &host)
        .expect("bounded recovery input must reach dispatch");

    let input = host
        .input_for_action("step_failure_recovery")
        .expect("recovery dispatched");
    let failed_step_input = &input["failed_step_input"];
    assert!(
        failed_step_input.is_object(),
        "failed_step_input must stay an object"
    );
    assert_eq!(failed_step_input["task_id"], "ORB-12467");
    assert_eq!(failed_step_input["workspace_path"], "/assigned/worktree");
    let serialized = serde_json::to_string(failed_step_input).expect("serialize bounded input");
    assert!(
        serialized.len() <= 64 * 1024,
        "failed_step_input is {} B",
        serialized.len()
    );
    let prompt = failed_step_input["rendered_prompt"]
        .as_str()
        .expect("oversized leaf stays a string");
    assert!(prompt.starts_with("PROMPT_HEAD"));
    assert!(prompt.ends_with("PROMPT_TAIL"));
    assert!(prompt.contains("[truncated: failed_step_input field is"));
}

#[test]
fn step_failure_recovery_projects_managed_context_for_implement_and_commit_failures() {
    let asset = load_activity_asset(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../orbit-core/assets/activities/step_failure_recovery.yaml"
    )))
    .unwrap();
    let schema = jsonschema::JSONSchema::compile(&asset.spec.input_schema_json).unwrap();
    for (failed_step_id, failed_input, job_input, expected_task_field) in [
        (
            "implement_one",
            json!({
                "task_id": "ORB-IMPLEMENT",
                "workspace_path": "{{ input.assigned }}",
                "repo_root": "{{ input.assigned }}",
            }),
            json!({
                "task_id": "ORB-IMPLEMENT",
                "assigned": "/assigned/implement-worktree",
            }),
            ("task_id", json!("ORB-IMPLEMENT")),
        ),
        (
            "commit",
            json!({
                "job_run_id": "{{ input.run_id }}",
                "workspace_path": "{{ input.assigned }}",
            }),
            json!({
                "task_ids": ["ORB-COMMIT"],
                "assigned": "/assigned/commit-worktree",
                "run_id": "stale-step-run",
            }),
            ("task_ids", json!(["ORB-COMMIT"])),
        ),
    ] {
        let original = retryable_error("flaky", &format!("{failed_step_id} failed"));
        let host = RecoveryHost::new([
            (
                "flaky",
                vec![Err(original.clone()), Ok(json!({"recovered": true}))],
            ),
            ("step_failure_recovery", vec![Ok(json!({}))]),
        ]);
        let mut job = recovery_job(Some("step_failure_recovery"), None, "flaky", None, 1);
        job.steps[0].id = failed_step_id.to_string();
        let JobV2StepBody::Target(target) = &mut job.steps[0].body else {
            panic!("resolved failed target")
        };
        target.default_input = Some(failed_input);
        let run_id = format!("run-{failed_step_id}-recovery");
        let writer = Arc::new(test_writer(&run_id));

        let outcome = execute_job(&job, job_input, &run_id, writer.clone(), &host)
            .expect("managed recovery context should reach dispatch");
        assert!(outcome.success);

        let input = host
            .input_for_action("step_failure_recovery")
            .expect("recovery dispatch input");
        assert_eq!(input["failed_step_id"], failed_step_id);
        assert_eq!(input["run_id"], run_id);
        assert_eq!(input[expected_task_field.0], expected_task_field.1);
        assert_eq!(input["repo_root"], input["workspace_path"]);
        assert_eq!(
            input["failed_step_input"]["workspace_path"],
            input["workspace_path"]
        );
        assert_eq!(input["system_crew"], true);
        let mut agent_input = input.clone();
        agent_input.as_object_mut().unwrap().remove("step_id");
        assert!(
            schema.is_valid(&agent_input),
            "recovery input must satisfy the shipped strict schema: {agent_input}"
        );

        let events = writer.events_snapshot().expect("audit snapshot");
        assert!(matches!(
            recovery_events(&events)[0].kind,
            V2AuditEventKind::StepRecoveryAttempted {
                recovery_succeeded: true,
                failure_phase: None,
                error_message: None,
                ..
            }
        ));
    }
}

#[test]
fn step_failure_recovery_without_managed_context_fails_closed_with_audit_diagnostic() {
    for invalid_input in [
        json!({"task_id": "ORB-NO-WORKTREE"}),
        json!({
            "task_id": "ORB-INVALID-WORKTREE",
            "workspace_path": null,
            "repo_root": "/primary/checkout",
        }),
    ] {
        let original = retryable_error("flaky", "failure before managed context existed");
        let host = RecoveryHost::new([
            ("flaky", vec![Err(original.clone())]),
            ("step_failure_recovery", vec![Ok(json!({}))]),
        ]);
        let mut job = recovery_job(Some("step_failure_recovery"), None, "flaky", None, 1);
        let JobV2StepBody::Target(target) = &mut job.steps[0].body else {
            panic!("resolved failed target")
        };
        target.default_input = Some(invalid_input);
        let writer = Arc::new(test_writer("run-missing-recovery-context"));

        let error = execute_job(
            &job,
            Value::Null,
            "run-missing-recovery-context",
            writer.clone(),
            &host,
        )
        .expect_err("recovery must not fall back to direct primary execution");

        assert_eq!(error.to_string(), original.to_string());
        assert_eq!(host.action_count("step_failure_recovery"), 0);
        let events = writer.events_snapshot().expect("audit snapshot");
        assert!(matches!(
            recovery_events(&events)[0].kind,
            V2AuditEventKind::StepRecoveryAttempted {
                recovery_succeeded: false,
                ref failure_phase,
                ref error_message,
                ..
            } if failure_phase.as_deref() == Some("input")
                && error_message.as_deref().is_some_and(|message| {
                    message.contains("managed step recovery requires a task ID")
                        && message.contains("refuses primary-checkout or unrestricted execution")
                })
        ));
    }
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
        Some("gpt-6-sol"),
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
        pipeline: std::sync::Arc::new(std::sync::Mutex::new(PipelineSteps::default())),
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
