#![allow(missing_docs)]

//! Step retry, short-circuit, and backoff invariants for `step.rs`.
//! Each test names the specific invariant or failure mode it guards.
//! See task T20260509-7.

use super::*;

#[test]
fn linear_step_success_propagates_output_to_pipeline() {
    // Invariant: a successful step's value lands in `pipeline[step.id]` so
    // downstream steps can consume it via `{{ steps.<id>.output.* }}`.
    let host = ScriptedHost::new([("build", vec![Action::Ok(json!({"ok": true}))])]);
    let job = job_with_steps(vec![target_step("build", "build")]);

    let outcome = run_job(&host, &job, Value::Null, "run-linear-success");

    assert!(outcome.success);
    let pipeline = outcome.pipeline.as_object().expect("pipeline is an object");
    assert_eq!(pipeline.get("build"), Some(&json!({"ok": true})));
}

#[test]
fn when_false_literal_skips_step_without_failing_job() {
    let host = ScriptedHost::new([("disabled", vec![Action::Ok(json!({"ran": true}))])]);
    let mut skipped = target_step("safety", "disabled");
    skipped.when = Some("false".to_string());
    let job = job_with_steps(vec![skipped]);
    let writer = std::sync::Arc::new(test_writer("run-when-false-literal"));

    let outcome = execute_job(
        &job,
        Value::Null,
        "run-when-false-literal",
        writer.clone(),
        &host,
    )
    .expect("when:false should skip cleanly");

    assert!(outcome.success);
    assert_eq!(host.call_count("disabled"), 0, "skipped step must not run");
    let events = writer.events_snapshot().expect("audit");
    assert!(
        events.iter().any(|event| matches!(
            &event.kind,
            V2AuditEventKind::StepSkipped { step_id, reason }
                if step_id == "safety" && reason == "when:false => false"
        )),
        "expected StepSkipped audit event for when:false"
    );
}

#[test]
fn step_failure_short_circuits_remaining_steps() {
    // Invariant: a failed step terminates the linear loop in `execute_job`
    // (mod.rs:131-148). Without retry/recovery, a retryable
    // DeterministicActionFailed bubbles up as Err — and crucially, later
    // steps must not have been invoked.
    let host = ScriptedHost::new([
        (
            "first",
            vec![Action::Err(DispatchError::DeterministicActionFailed {
                action: "first".into(),
                message: "boom".into(),
            })],
        ),
        ("second", vec![Action::Ok(json!({"ran": true}))]),
    ]);
    let job = job_with_steps(vec![
        target_step("step1", "first"),
        target_step("step2", "second"),
    ]);
    let writer = std::sync::Arc::new(test_writer("run-shortcircuit"));

    let err = execute_job(&job, Value::Null, "run-shortcircuit", writer.clone(), &host)
        .expect_err("first step error must surface");

    assert!(matches!(
        err,
        DispatchError::DeterministicActionFailed { ref action, .. } if action == "first"
    ));
    assert_eq!(host.call_count("second"), 0, "second step must not run");
    let events = writer.events_snapshot().expect("audit");
    let step_finished = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::StepFinished {
                step_id,
                outcome,
                error_message,
            } if step_id == "step1" => Some((outcome, error_message)),
            _ => None,
        })
        .expect("step finished event");
    assert_eq!(step_finished.0, "error");
    assert_eq!(
        step_finished.1.as_deref(),
        Some("deterministic action `first` failed: boom")
    );
}

#[test]
fn retry_runs_max_attempts_then_surfaces_last_error() {
    // Invariant: with retry, a deterministic action that always errors is
    // retried up to `max_attempts`; the final error surfaces as Err and
    // `StepRetry` is emitted between attempts (N attempts → N-1 retries).
    let host = ScriptedHost::new([(
        "flaky",
        vec![
            Action::Err(DispatchError::DeterministicActionFailed {
                action: "flaky".into(),
                message: "1".into(),
            }),
            Action::Err(DispatchError::DeterministicActionFailed {
                action: "flaky".into(),
                message: "2".into(),
            }),
            Action::Err(DispatchError::DeterministicActionFailed {
                action: "flaky".into(),
                message: "3".into(),
            }),
        ],
    )]);
    let job = job_with_steps(vec![target_step_with_retry("flaky", "flaky", 3)]);
    let writer = std::sync::Arc::new(test_writer("run-retry-max"));
    let err = execute_job(&job, Value::Null, "run-retry-max", writer.clone(), &host)
        .expect_err("retry exhaustion must surface as Err");

    assert!(matches!(
        err,
        DispatchError::DeterministicActionFailed { ref message, .. } if message == "3"
    ));
    assert_eq!(host.call_count("flaky"), 3);
    let events = writer.events_snapshot().expect("audit");
    let retries: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.kind {
            V2AuditEventKind::StepRetry { attempt, .. } => Some(*attempt),
            _ => None,
        })
        .collect();
    assert_eq!(retries, vec![1, 2]);
}

#[test]
fn retry_stops_immediately_on_non_retryable_error() {
    // Invariant: `is_non_retryable()` errors (e.g. `ToolDenied`) skip the
    // retry loop and surface a `StepDenied` audit event.
    let host = ScriptedHost::new([(
        "denied",
        vec![Action::Err(DispatchError::ToolDenied {
            tool_name: "fs.write".into(),
            iteration: 1,
        })],
    )]);
    let job = job_with_steps(vec![target_step_with_retry("denied", "denied", 5)]);
    let writer = std::sync::Arc::new(test_writer("run-non-retryable"));

    let err = execute_job(
        &job,
        Value::Null,
        "run-non-retryable",
        writer.clone(),
        &host,
    )
    .expect_err("tool denial bubbles up");

    assert!(matches!(err, DispatchError::ToolDenied { .. }));
    assert_eq!(host.call_count("denied"), 1, "must not retry after denial");
    let events = writer.events_snapshot().expect("audit");
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.kind, V2AuditEventKind::StepDenied { .. })),
        "expected StepDenied audit event"
    );
}

#[test]
fn retry_returns_success_on_intermediate_attempt_without_extra_calls() {
    // Invariant: once an attempt succeeds, no further attempts run.
    let host = ScriptedHost::new([(
        "settle",
        vec![
            Action::Err(DispatchError::DeterministicActionFailed {
                action: "settle".into(),
                message: "1".into(),
            }),
            Action::Ok(json!({"settled": true})),
            Action::Ok(json!({"would-be-extra": true})),
        ],
    )]);
    let job = job_with_steps(vec![target_step_with_retry("settle", "settle", 5)]);

    let outcome = run_job(&host, &job, Value::Null, "run-retry-settle");

    assert!(outcome.success);
    assert_eq!(host.call_count("settle"), 2);
}

#[test]
fn compute_backoff_ms_respects_initial_max_and_zero_attempt_boundary() {
    // Invariant: `compute_backoff_ms` is monotonic with attempt index (linear
    // strategy) and never exceeds the cap. Pure unit test — no host required.
    let retry = RetrySpec {
        max_attempts: 5,
        initial_backoff_ms: 100,
        backoff_cap_ms: 250,
        backoff_strategy: BackoffStrategy::Linear,
    };
    // Linear: shifted = initial * (attempt_index + 1)
    assert_eq!(compute_backoff_ms(&retry, 0), 100); // 100 * 1
    assert_eq!(compute_backoff_ms(&retry, 1), 200); // 100 * 2
    assert_eq!(compute_backoff_ms(&retry, 2), 250); // 300 capped to 250
    assert_eq!(compute_backoff_ms(&retry, 5), 250); // capped

    let exp = RetrySpec {
        max_attempts: 5,
        initial_backoff_ms: 50,
        backoff_cap_ms: 1000,
        backoff_strategy: BackoffStrategy::Exponential,
    };
    assert_eq!(compute_backoff_ms(&exp, 0), 50); // 50 << 0
    assert_eq!(compute_backoff_ms(&exp, 1), 100); // 50 << 1
    assert_eq!(compute_backoff_ms(&exp, 4), 800); // 50 << 4
    assert_eq!(compute_backoff_ms(&exp, 10), 1000); // capped
}

#[test]
fn jittered_backoff_stays_within_deterministic_bound() {
    // ORB-10006: the actual sleep is a full-jitter draw over the
    // deterministic cap-growth bound — always within [0, bound].
    let retry = RetrySpec {
        max_attempts: 5,
        initial_backoff_ms: 100,
        backoff_cap_ms: 1_000,
        backoff_strategy: BackoffStrategy::Exponential,
    };
    let mut rng = orbit_common::process::jitter::JitterRng::from_seed(0xdead_beef);
    for attempt in 0..8u32 {
        let bound = compute_backoff_ms(&retry, attempt);
        for _ in 0..128 {
            let sleep = rng.full_jitter(bound);
            assert!(
                sleep <= bound,
                "attempt {attempt}: jittered sleep {sleep} exceeded bound {bound}"
            );
        }
    }
}

#[test]
fn backoff_bound_grows_monotonically_to_cap_under_exponential() {
    // ORB-10006: jitter randomizes the sleep but the *bound* still grows
    // monotonically with attempt index and saturates at the cap.
    let retry = RetrySpec {
        max_attempts: 10,
        initial_backoff_ms: 50,
        backoff_cap_ms: 750,
        backoff_strategy: BackoffStrategy::Exponential,
    };
    let mut previous = 0u64;
    for attempt in 0..12u32 {
        let bound = compute_backoff_ms(&retry, attempt);
        assert!(
            bound >= previous,
            "bound shrank at attempt {attempt}: {previous} -> {bound}"
        );
        assert!(bound <= retry.backoff_cap_ms, "bound exceeded cap");
        previous = bound;
    }
    assert_eq!(previous, retry.backoff_cap_ms, "bound must saturate at cap");
}

// ----- [ORB-10449] Step-completion protocol -------------------------------

use orbit_types::workflow::activity_job::{AgentLoopSpec, OnDenial, Provider};

/// Build the shipped `implement_one` shape: a `backend: cli` agent loop that is
/// artifact-backed, so the *content* contract stays off and only the
/// step-completion contract can catch a stalled agent.
fn agent_implement_shaped_step(id: &str, retry: Option<RetrySpec>) -> JobV2Step {
    let spec = AgentLoopSpec {
        instruction: "implement the task".to_string(),
        tools: Vec::new(),
        on_denial: OnDenial::Terminate,
        model: None,
        reasoning_effort: None,
        max_iterations: 1,
        backend: None,
        provider: Provider::Claude,
        wall_clock_timeout_seconds: 30,
        require_response_envelope: false,
        require_completion_envelope: true,
        proc_allowed_programs: None,
        trusted_host_execution: false,
    };
    JobV2Step {
        id: id.to_string(),
        when: None,
        retry,
        recovery_activity: None,
        resolved_recovery_activity: None,
        body: JobV2StepBody::Target(TargetStep {
            spec: ActivityV2Spec::AgentLoop(spec),
            activity_name: None,
            input_schema_json: None,
            fs_profile: None,
            default_input: None,
            timeout_seconds: 0,
            session: None,
        }),
    }
}

/// Write a fake provider that exits 0 after printing `stdout` and nothing else.
fn stalled_provider(dir: &std::path::Path, stdout: &str) -> std::path::PathBuf {
    let script = dir.join("claude");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{stdout}'\n"),
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

/// `jrun-20260726-1758-5` replayed end to end: the implementer exits 0 with
/// prose and no response envelope. The run must fail *at that step* — not
/// checkpoint it and surface a downstream symptom several steps later.
#[test]
fn stalled_agent_step_fails_at_the_step_that_produced_it() {
    let temp = tempfile::tempdir().expect("tempdir");
    let script = stalled_provider(
        temp.path(),
        "{\"type\":\"result\",\"subtype\":\"success\",\"stop_reason\":\"end_turn\",\
         \"result\":\"Waiting on the background nextest run before continuing.\"}",
    );
    let host = ScriptedHost::new([("commit", vec![Action::Ok(json!({"committed": true}))])])
        .with_cli_program(script.display().to_string());
    let job = job_with_steps(vec![
        agent_implement_shaped_step("implement_one", None),
        target_step("commit", "commit"),
    ]);

    let writer = std::sync::Arc::new(test_writer("run-stalled"));
    let outcome = execute_job(
        &job,
        json!({"task_id": "ORB-10436"}),
        "run-stalled",
        writer.clone(),
        &host,
    )
    .expect("execute_job ok");

    assert!(!outcome.success, "a stalled implementer must fail its step");
    let message = outcome.message.expect("terminal message");
    // Criterion: the terminal error names the step *and* the protocol
    // violation, rather than whatever gate would have tripped later.
    assert!(message.contains("implement_one"), "{message}");
    assert!(message.contains("agent step did not complete"), "{message}");
    assert!(
        message.contains("does not contain an Orbit response envelope"),
        "{message}"
    );

    // Recovery/retry contract: the step is not retried, and the run stops
    // there — no later step runs on work that never happened.
    assert_eq!(
        host.call_count("commit"),
        0,
        "downstream steps must not run after a protocol violation"
    );
    // The step is recorded as failed, so a resume cannot skip past it.
    let events = writer.events_snapshot().expect("audit");
    assert!(
        events.iter().any(|event| matches!(
            &event.kind,
            V2AuditEventKind::StepFinished { step_id, outcome, .. }
                if step_id == "implement_one" && outcome == "failed"
        )),
        "implement_one must finish as failed, never as a success checkpoint"
    );
}

/// A valid failed envelope terminates the provider protocol, but it still
/// represents a failed implementation outcome. It must audit the implementer
/// as failed and stop before the delivery action can create a bad checkpoint.
#[test]
fn declared_failed_agent_step_is_audited_and_never_checkpointed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let script = stalled_provider(
        temp.path(),
        "{\"schemaVersion\":1,\"status\":\"failed\",\"result\":{},\"error\":{\"code\":\"blocked\",\"message\":\"cannot proceed\"}}",
    );
    let host = ScriptedHost::new([("commit", vec![Action::Ok(json!({"committed": true}))])])
        .with_cli_program(script.display().to_string());
    let job = job_with_steps(vec![
        agent_implement_shaped_step("implement_one", None),
        target_step("git_commit", "commit"),
    ]);
    let writer = std::sync::Arc::new(test_writer("run-declared-failure"));

    let outcome = execute_job(
        &job,
        json!({"task_id": "ORB-10733"}),
        "run-declared-failure",
        writer.clone(),
        &host,
    )
    .expect("execute_job ok");

    assert!(!outcome.success, "declared failure must fail implement_one");
    let message = outcome.message.expect("terminal message");
    assert!(message.contains("implement_one"), "{message}");
    assert!(message.contains("failed"), "{message}");
    assert_eq!(
        host.call_count("commit"),
        0,
        "git_commit must not run after an explicit failed implementation"
    );

    let events = writer.events_snapshot().expect("audit");
    assert!(
        events.iter().any(|event| matches!(
            &event.kind,
            V2AuditEventKind::StepFinished { step_id, outcome, .. }
                if step_id == "implement_one" && outcome == "failed"
        )),
        "implement_one must be audited as failed instead of checkpointed"
    );
    assert!(
        !events.iter().any(|event| matches!(
            &event.kind,
            V2AuditEventKind::StepFinished { step_id, outcome, .. }
                if step_id == "git_commit" && outcome == "succeeded"
        )),
        "git_commit must have no success checkpoint"
    );
}

/// An agent step that *does* terminate properly still succeeds, so the gate
/// costs nothing on the healthy path.
#[test]
fn completed_agent_step_still_checkpoints_and_advances() {
    let temp = tempfile::tempdir().expect("tempdir");
    let script = stalled_provider(
        temp.path(),
        "{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}",
    );
    let host = ScriptedHost::new([("commit", vec![Action::Ok(json!({"committed": true}))])])
        .with_cli_program(script.display().to_string());
    let job = job_with_steps(vec![
        agent_implement_shaped_step("implement_one", None),
        target_step("commit", "commit"),
    ]);

    let outcome = run_job(
        &host,
        &job,
        json!({"task_id": "ORB-10449"}),
        "run-completed",
    );

    assert!(outcome.success, "{:?}", outcome.message);
    assert_eq!(host.call_count("commit"), 1);
}

/// [ORB-10449] Retry exhaustion must preserve the last attempt's diagnostic.
/// A step that fails via `Ok(success: false)` — which is every CLI agent-loop
/// failure — used to have its message dropped, leaving the run's terminal
/// error as the generic "completed with success=false" fallback.
#[test]
fn retry_exhaustion_preserves_the_last_failure_message() {
    let temp = tempfile::tempdir().expect("tempdir");
    let script = stalled_provider(
        temp.path(),
        "{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"still working\"}",
    );
    let host = ScriptedHost::new([]).with_cli_program(script.display().to_string());
    let job = job_with_steps(vec![agent_implement_shaped_step(
        "implement_one",
        Some(RetrySpec {
            max_attempts: 2,
            initial_backoff_ms: 1,
            backoff_cap_ms: 1,
            backoff_strategy: BackoffStrategy::Linear,
        }),
    )]);

    let outcome = run_job(
        &host,
        &job,
        json!({"task_id": "ORB-10449"}),
        "run-retry-msg",
    );

    assert!(!outcome.success);
    let message = outcome.message.expect("terminal message");
    assert!(
        !message.contains("completed with success=false"),
        "the generic fallback hides the real cause: {message}"
    );
    assert!(message.contains("agent step did not complete"), "{message}");
}
