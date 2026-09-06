#![allow(missing_docs)]

//! Retry-config validation invariants for `validate.rs` (ORB-10006), plus the
//! catalog-versus-runtime action-availability gate (ORB-10385).

use super::*;

fn retry(max_attempts: u32, initial_backoff_ms: u64, backoff_cap_ms: u64) -> RetrySpec {
    RetrySpec {
        max_attempts,
        initial_backoff_ms,
        backoff_cap_ms,
        backoff_strategy: BackoffStrategy::Exponential,
    }
}

fn job_with_retry(spec: RetrySpec) -> JobV2 {
    let mut step = target_step("build", "build");
    step.retry = Some(spec);
    job_with_steps(vec![step])
}

#[test]
fn validate_job_accepts_well_formed_retry_config() {
    for spec in [
        retry(1, 1, 1),
        retry(3, 100, 100),
        retry(4, 200, 2_000),
        retry(10, 1_000, 60_000),
    ] {
        let job = job_with_retry(spec.clone());
        assert!(
            validate_job(&job).is_ok(),
            "expected valid retry config to pass: {spec:?}"
        );
    }
}

#[test]
fn validate_job_rejects_zero_max_attempts() {
    let err = validate_job(&job_with_retry(retry(0, 100, 1_000)))
        .expect_err("zero max_attempts must be rejected");
    match err {
        DispatchError::RetryConfigInvalid {
            step_id,
            field,
            value,
            ..
        } => {
            assert_eq!(step_id, "build");
            assert_eq!(field, "max_attempts");
            assert_eq!(value, 0);
        }
        other => panic!("expected RetryConfigInvalid, got {other:?}"),
    }
}

#[test]
fn validate_job_rejects_zero_initial_backoff() {
    let err = validate_job(&job_with_retry(retry(3, 0, 1_000)))
        .expect_err("zero initial_backoff_ms must be rejected");
    match err {
        DispatchError::RetryConfigInvalid { field, value, .. } => {
            assert_eq!(field, "initial_backoff_ms");
            assert_eq!(value, 0);
        }
        other => panic!("expected RetryConfigInvalid, got {other:?}"),
    }
}

#[test]
fn validate_job_rejects_cap_below_initial_backoff_naming_both_values() {
    let err = validate_job(&job_with_retry(retry(3, 500, 100)))
        .expect_err("inverted cap must be rejected");
    match &err {
        DispatchError::RetryConfigInvalid {
            field,
            value,
            invariant,
            ..
        } => {
            assert_eq!(*field, "backoff_cap_ms");
            assert_eq!(*value, 100);
            assert!(
                invariant.contains("500"),
                "invariant should name initial_backoff_ms: {invariant}"
            );
        }
        other => panic!("expected RetryConfigInvalid, got {other:?}"),
    }
    // The rendered message names the offending field and value.
    let rendered = err.to_string();
    assert!(rendered.contains("backoff_cap_ms"), "message: {rendered}");
    assert!(rendered.contains("100"), "message: {rendered}");
    assert!(rendered.contains("500"), "message: {rendered}");
}

#[test]
fn validate_job_checks_retry_config_in_nested_blocks() {
    // Retry invariants are enforced recursively (parallel branches, loop
    // bodies, fan-out workers), not just on top-level steps.
    let mut nested = target_step("inner", "inner");
    nested.retry = Some(retry(2, 300, 10));
    let parallel = JobV2Step {
        id: "outer".to_string(),
        when: None,
        retry: None,
        recovery_activity: None,
        resolved_recovery_activity: None,
        body: JobV2StepBody::Parallel {
            parallel: ParallelBlock {
                join: JoinMode::All,
                branches: vec![nested],
            },
        },
    };
    let err = validate_job(&job_with_steps(vec![parallel]))
        .expect_err("nested invalid retry config must be rejected");
    assert!(
        matches!(
            err,
            DispatchError::RetryConfigInvalid { ref step_id, .. } if step_id == "inner"
        ),
        "got {err:?}"
    );
}

#[test]
fn retry_config_invalid_is_non_retryable() {
    let err = DispatchError::RetryConfigInvalid {
        step_id: "s".into(),
        field: "backoff_cap_ms",
        value: 1,
        invariant: "backoff_cap_ms >= initial_backoff_ms (2)".into(),
    };
    assert!(err.is_non_retryable());
}

// --------------------------------------------------------------------------
// [ORB-10385] Catalog asset versus installed-runtime action skew
// --------------------------------------------------------------------------

fn deterministic_catalog_activity(action: &str) -> ActivityV2 {
    ActivityV2 {
        description: format!("deterministic {action}"),
        input_schema_json: json!({}),
        output_schema_json: json!({}),
        fs_profile: None,
        spec: ActivityV2Spec::Deterministic(DeterministicSpec {
            action: action.to_string(),
            config: Value::Null,
        }),
    }
}

/// The shipping shape of the incident: a checked-in `task_pr_pipeline`-style
/// job whose first step admits a task and whose terminal `failure_activity`
/// binds `pr_failure_handoff`, resolved against a catalog that has the asset.
fn pr_pipeline_shaped_job() -> JobV2 {
    let yaml = r#"
schemaVersion: 2
kind: Job
metadata:
  name: skew_pipeline
spec:
  state: enabled
  failure_activity: pr_failure_handoff
  steps:
    - id: setup
      target: activity:worktree_setup
"#;
    let mut job = load_job_asset(yaml).expect("job yaml").spec;
    let mut catalog = V2ActivityCatalog::new();
    catalog.insert(
        "worktree_setup",
        deterministic_catalog_activity("test_worktree_setup"),
    );
    catalog.insert(
        "pr_failure_handoff",
        deterministic_catalog_activity("pr_failure_handoff"),
    );
    resolve_job_catalog_refs_for_execution(&mut job, &catalog).expect("resolve catalog refs");
    job
}

#[test]
fn catalog_action_missing_from_runtime_fails_before_any_step_dispatches() {
    // The catalog carries the ORB-10363 `pr_failure_handoff` asset; the
    // installed runtime predates its deterministic action. The run must fail
    // validation rather than admit a task, build a worktree, and discover the
    // skew from the terminal hook.
    let job = pr_pipeline_shaped_job();
    let host = ScriptedHost::new([("test_worktree_setup", vec![Action::Ok(json!({"ok": true}))])])
        .without_registered_actions(&["pr_failure_handoff"]);
    let writer = std::sync::Arc::new(test_writer("run-catalog-skew"));

    let err = execute_job(&job, json!({}), "run-catalog-skew", writer, &host)
        .expect_err("unavailable failure-activity action must fail the job");

    match &err {
        DispatchError::DeterministicActionUnavailable { activity, action } => {
            assert_eq!(activity, "pr_failure_handoff");
            assert_eq!(action, "pr_failure_handoff");
        }
        other => panic!("expected DeterministicActionUnavailable, got {other:?}"),
    }
    // No deterministic action ran at all: `worktree_setup` is what performs
    // task workflow admission and creates the worktree, so zero calls proves
    // the run produced no task-lifecycle or worktree side effect.
    assert_eq!(
        host.total_calls(),
        0,
        "validation must reject the job before the first step dispatches"
    );
    assert_eq!(host.call_count("test_worktree_setup"), 0);
}

#[test]
fn unavailable_action_diagnostic_names_activity_and_action() {
    let job = pr_pipeline_shaped_job();
    let host = ScriptedHost::new([]).without_registered_actions(&["pr_failure_handoff"]);

    let rendered = validate_job_deterministic_actions(&job, &host)
        .expect_err("unavailable action must be rejected")
        .to_string();

    assert!(
        rendered.contains("pr_failure_handoff"),
        "diagnostic must name the activity and action: {rendered}"
    );
    assert!(
        rendered.contains("not registered"),
        "diagnostic must state the action is unavailable: {rendered}"
    );
}

#[test]
fn registered_failure_activity_still_loads_and_runs() {
    // The healthy pairing — catalog asset plus a runtime that implements the
    // action — is untouched, and unknown actions are never silently skipped.
    let job = pr_pipeline_shaped_job();
    let host = ScriptedHost::new([("test_worktree_setup", vec![Action::Ok(json!({"ok": true}))])]);
    let writer = std::sync::Arc::new(test_writer("run-catalog-healthy"));

    let outcome = execute_job(&job, json!({}), "run-catalog-healthy", writer, &host)
        .expect("healthy pipeline must still run");

    assert!(outcome.success);
    assert_eq!(host.call_count("test_worktree_setup"), 1);
}

#[test]
fn unavailable_action_is_rejected_inside_nested_step_bodies() {
    let mut worker = target_step("worker", "publish");
    worker.body = JobV2StepBody::Target(TargetStep {
        activity_name: Some("publish_report".to_string()),
        ..deterministic_target("publish")
    });
    let job = job_with_steps(vec![fanout_step(
        "fan",
        "{{ input.items }}",
        2,
        worker,
        JoinMode::All,
        None,
    )]);
    let host = ScriptedHost::new([]).without_registered_actions(&["publish"]);

    let err = validate_job_deterministic_actions(&job, &host)
        .expect_err("nested unavailable action must be rejected");

    match &err {
        DispatchError::DeterministicActionUnavailable { activity, action } => {
            assert_eq!(activity, "publish_report");
            assert_eq!(action, "publish");
        }
        other => panic!("expected DeterministicActionUnavailable, got {other:?}"),
    }
}

#[test]
fn unavailable_action_is_non_retryable_and_translates_to_job_validation() {
    let err = DispatchError::DeterministicActionUnavailable {
        activity: "pr_failure_handoff".to_string(),
        action: "pr_failure_handoff".to_string(),
    };
    assert!(err.is_non_retryable());
    assert!(matches!(
        crate::dispatch_error_to_orbit(err),
        orbit_common::OrbitError::JobValidation(_)
    ));
}

#[test]
fn failure_activity_unavailable_after_admission_preserves_original_step_error() {
    // Validation passed (the host advertised the action), then the registry
    // answered "not registered" at dispatch — an in-flight skew. The terminal
    // hook's own failure must never displace the failed step's error.
    let mut job = pr_pipeline_shaped_job();
    job.resolved_failure_activity = Some(deterministic_catalog_activity("test_pr_failure_handoff"));
    job.steps
        .push(target_step("implement", "agent_implement_stub"));
    let host = ScriptedHost::new([
        ("test_worktree_setup", vec![Action::Ok(json!({"ok": true}))]),
        (
            "agent_implement_stub",
            vec![Action::Err(DispatchError::JobExecution(
                "original step failure".to_string(),
            ))],
        ),
        (
            "test_pr_failure_handoff",
            vec![Action::Err(
                DispatchError::DeterministicActionNotRegistered(
                    "test_pr_failure_handoff".to_string(),
                ),
            )],
        ),
    ]);
    let writer = std::sync::Arc::new(test_writer("run-late-skew"));

    let err = execute_job(&job, json!({}), "run-late-skew", writer, &host)
        .expect_err("the failing step must still fail the job");

    assert!(
        matches!(&err, DispatchError::JobExecution(message) if message == "original step failure"),
        "failure hook must not replace the original error, got {err:?}"
    );
    assert_eq!(
        host.call_count("test_pr_failure_handoff"),
        1,
        "the terminal hook is still attempted once"
    );
}

// --------------------------------------------------------------------------
// [ORB-11325] A `when:` / `break_when:` condition may only read the output
// of a step that always runs — see design doc §8.2.
// --------------------------------------------------------------------------

fn step_with_when(id: &str, when: &str, action: &str) -> JobV2Step {
    JobV2Step {
        id: id.to_string(),
        when: Some(when.to_string()),
        retry: None,
        recovery_activity: None,
        resolved_recovery_activity: None,
        body: JobV2StepBody::Target(deterministic_target(action)),
    }
}

#[test]
fn validate_job_rejects_when_reading_output_of_a_conditionally_run_step() {
    let conditional = step_with_when("maybe_run", "{{ input.flag }} == true", "maybe_run_action");
    let reader = step_with_when(
        "reader",
        "{{ steps.maybe_run.output.done }} == true",
        "reader_action",
    );

    let err = validate_job(&job_with_steps(vec![conditional, reader]))
        .expect_err("reading a conditional step's output must be rejected");

    match &err {
        DispatchError::JobValidation(message) => {
            assert!(
                message.contains("reader"),
                "message must name the reading step: {message}"
            );
            assert!(
                message.contains("maybe_run"),
                "message must name the conditional step: {message}"
            );
        }
        other => panic!("expected JobValidation, got {other:?}"),
    }
}

#[test]
fn validate_job_accepts_when_reading_output_of_an_always_run_step() {
    let always = target_step("always_run", "always_run_action");
    let reader = step_with_when(
        "reader",
        "{{ steps.always_run.output.done }} == true",
        "reader_action",
    );

    assert!(
        validate_job(&job_with_steps(vec![always, reader])).is_ok(),
        "reading an always-run step's output must be accepted"
    );
}

#[test]
fn validate_job_rejects_break_when_reading_output_of_a_conditionally_run_step_in_a_loop() {
    let conditional = step_with_when("maybe_run", "{{ input.flag }} == true", "maybe_run_action");
    let loop_step = JobV2Step {
        id: "loop_step".to_string(),
        when: None,
        retry: None,
        recovery_activity: None,
        resolved_recovery_activity: None,
        body: JobV2StepBody::Loop {
            loop_: LoopBlock {
                items: None,
                max_iterations: 3,
                break_when: Some("{{ steps.maybe_run.output.done }} == true".to_string()),
                steps: vec![conditional],
            },
        },
    };

    let err = validate_job(&job_with_steps(vec![loop_step]))
        .expect_err("break_when reading a conditional step's output must be rejected");

    assert!(
        matches!(err, DispatchError::JobValidation(ref message) if message.contains("maybe_run")),
        "got {err:?}"
    );
}

// --------------------------------------------------------------------------
// [ORB-11346] A parent's `when:` skips its whole body, so "always runs" is an
// ancestor-chain property, not a per-step flag.
// --------------------------------------------------------------------------

/// The reproducer shape: a container guarded by `when:` whose nested step
/// carries no guard of its own.
fn conditional_parent_with_unguarded_child(parent: JobV2Step) -> JobV2Step {
    JobV2Step {
        when: Some("{{ input.run }} == true".to_string()),
        ..parent
    }
}

#[test]
fn a_false_parent_guard_leaves_its_nested_step_with_no_recorded_output() {
    // The runtime fact the validator has to model: `run_step` returns before
    // running the body, so `produce` never records anything and a later
    // reader — here an ordinary input template, which validation does not
    // inspect — fails on the false branch.
    let gate = conditional_parent_with_unguarded_child(parallel_step(
        "gate",
        JoinMode::All,
        vec![target_step("produce", "produce_action")],
    ));
    let mut reader = target_step("reader", "reader_action");
    reader.body = JobV2StepBody::Target(TargetStep {
        default_input: Some(json!({ "done": "{{ steps.produce.output.done }}" })),
        ..deterministic_target("reader_action")
    });
    let job = job_with_steps(vec![gate, reader]);
    let host = ScriptedHost::new([
        ("produce_action", vec![Action::Ok(json!({ "done": true }))]),
        ("reader_action", vec![Action::Ok(json!({}))]),
    ]);
    let writer = std::sync::Arc::new(test_writer("run-false-parent"));

    let err = execute_job(
        &job,
        json!({ "run": false }),
        "run-false-parent",
        writer,
        &host,
    )
    .expect_err("the reader must fail once the parent guard skipped `produce`");

    assert!(
        err.to_string()
            .contains("no data recorded for step 'produce'"),
        "expected the skipped-step template failure, got {err:?}"
    );
    assert_eq!(
        host.call_count("produce_action"),
        0,
        "a false parent guard must skip the nested step entirely"
    );
}

#[test]
fn validate_job_rejects_when_reading_a_step_nested_in_a_conditional_parallel() {
    let gate = conditional_parent_with_unguarded_child(parallel_step(
        "gate",
        JoinMode::All,
        vec![target_step("produce", "produce_action")],
    ));
    let reader = step_with_when(
        "reader",
        "{{ steps.produce.output.done }} == true",
        "reader_action",
    );

    let err = validate_job(&job_with_steps(vec![gate, reader]))
        .expect_err("reading a step nested under a conditional parallel must be rejected");

    match &err {
        DispatchError::JobValidation(message) => {
            assert!(
                message.contains("reader"),
                "message must name the reading step: {message}"
            );
            assert!(
                message.contains("produce"),
                "message must name the referenced step: {message}"
            );
            assert!(
                message.contains("gate"),
                "message must name the guarding ancestor: {message}"
            );
        }
        other => panic!("expected JobValidation, got {other:?}"),
    }
}

#[test]
fn validate_job_rejects_when_reading_a_step_nested_in_a_conditional_loop() {
    let gate = conditional_parent_with_unguarded_child(loop_step(
        "gate",
        None,
        3,
        None,
        vec![target_step("produce", "produce_action")],
    ));
    let reader = step_with_when(
        "reader",
        "{{ steps.produce.output.done }} == true",
        "reader_action",
    );

    let err = validate_job(&job_with_steps(vec![gate, reader]))
        .expect_err("reading a step nested under a conditional loop must be rejected");

    assert!(
        matches!(&err, DispatchError::JobValidation(message)
            if message.contains("produce") && message.contains("gate")),
        "got {err:?}"
    );
}

#[test]
fn validate_job_rejects_when_reading_a_worker_nested_in_a_conditional_fan_out() {
    let gate = conditional_parent_with_unguarded_child(fanout_step(
        "gate",
        "{{ input.items }}",
        2,
        target_step("produce", "produce_action"),
        JoinMode::All,
        None,
    ));
    let reader = step_with_when(
        "reader",
        "{{ steps.produce.output.done }} == true",
        "reader_action",
    );

    let err = validate_job(&job_with_steps(vec![gate, reader]))
        .expect_err("reading a worker nested under a conditional fan-out must be rejected");

    assert!(
        matches!(&err, DispatchError::JobValidation(message)
            if message.contains("produce") && message.contains("gate")),
        "got {err:?}"
    );
}

#[test]
fn validate_job_accepts_when_reading_a_step_under_an_always_run_ancestor_chain() {
    // No ancestor carries a guard, so `produce` runs whenever the job runs —
    // the shape every shipped asset relies on.
    let outer = parallel_step(
        "outer",
        JoinMode::All,
        vec![loop_step(
            "inner",
            None,
            2,
            None,
            vec![target_step("produce", "produce_action")],
        )],
    );
    let reader = step_with_when(
        "reader",
        "{{ steps.produce.output.done }} == true",
        "reader_action",
    );

    assert!(
        validate_job(&job_with_steps(vec![outer, reader])).is_ok(),
        "an unguarded ancestor chain must stay valid"
    );
}

#[test]
fn validate_job_accepts_a_reader_that_shares_every_guard_with_the_step_it_reads() {
    // `break_when` is only evaluated while the loop is running, and the body
    // reader only runs when the same guard passed: a guard both sides sit
    // under skips them together and can never strand the reader.
    let mut gate = loop_step(
        "gate",
        None,
        3,
        Some("{{ steps.produce.output.done }} == true"),
        vec![
            target_step("produce", "produce_action"),
            step_with_when(
                "body_reader",
                "{{ steps.produce.output.done }} == false",
                "reader_action",
            ),
        ],
    );
    gate.when = Some("{{ input.run }} == true".to_string());

    assert!(
        validate_job(&job_with_steps(vec![gate])).is_ok(),
        "a guard shared by reader and referenced step must stay valid"
    );
}
