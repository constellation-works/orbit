#![allow(missing_docs)]

//! Parallel-block invariants for `parallel.rs`: JoinMode semantics,
//! `StepJoin` event ordering, and audit parent-stack inheritance into branch
//! threads. See task T20260509-7.

use std::path::PathBuf;
use std::time::Instant;

use super::*;

#[test]
fn parallel_join_all_succeeds_iff_every_branch_succeeds() {
    let host = ScriptedHost::new([
        ("a", vec![Action::Ok(json!({"a": true}))]),
        ("b", vec![Action::Ok(json!({"b": true}))]),
        ("c", vec![Action::Ok(json!({"c": true}))]),
    ]);
    let job = job_with_steps(vec![parallel_step(
        "fan",
        JoinMode::All,
        vec![
            target_step("br_a", "a"),
            target_step("br_b", "b"),
            target_step("br_c", "c"),
        ],
    )]);
    let outcome = run_job(&host, &job, Value::Null, "run-par-all-ok");
    assert!(outcome.success);
}

#[test]
fn parallel_join_all_fails_when_any_branch_fails() {
    let host = ScriptedHost::new([
        ("a", vec![Action::Ok(json!({"a": true}))]),
        (
            "b",
            vec![Action::Err(DispatchError::DeterministicActionFailed {
                action: "b".into(),
                message: "no".into(),
            })],
        ),
    ]);
    let job = job_with_steps(vec![parallel_step(
        "fan",
        JoinMode::All,
        vec![target_step("br_a", "a"), target_step("br_b", "b")],
    )]);
    let writer = std::sync::Arc::new(test_writer("run-par-all-fail"));
    let err = execute_job(&job, Value::Null, "run-par-all-fail", writer, &host)
        .expect_err("first branch error surfaces");
    assert!(matches!(
        err,
        DispatchError::DeterministicActionFailed { ref action, .. } if action == "b"
    ));
}

#[test]
fn parallel_panic_is_recorded_as_a_failed_branch() {
    let host = ScriptedHost::new([
        ("a", vec![Action::Ok(json!({"a": true}))]),
        ("b", vec![Action::Panic]),
    ]);
    let job = job_with_steps(vec![parallel_step(
        "fan",
        JoinMode::All,
        vec![target_step("br_a", "a"), target_step("br_b", "b")],
    )]);
    let writer = std::sync::Arc::new(test_writer("run-par-panic"));
    let err = execute_job(&job, Value::Null, "run-par-panic", writer.clone(), &host)
        .expect_err("a panicked branch must fail the block without panicking the job thread");

    assert!(
        matches!(err, DispatchError::JobExecution(message) if message.contains("branch thread panicked"))
    );
    let events = writer.events_snapshot().expect("audit");
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        V2AuditEventKind::StepJoin { branch_outcomes, .. }
            if branch_outcomes.iter().any(|branch| branch.branch_id == "br_b" && branch.outcome == "error")
    )));
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        V2AuditEventKind::StepFinished { step_id, outcome, .. }
            if step_id == "fan" && outcome == "error"
    )));
}

#[test]
fn parallel_join_any_succeeds_with_one_branch_success() {
    let host = ScriptedHost::new([
        (
            "a",
            vec![Action::Err(DispatchError::DeterministicActionFailed {
                action: "a".into(),
                message: "boom".into(),
            })],
        ),
        ("b", vec![Action::Ok(json!({"b": true}))]),
    ]);
    let job = job_with_steps(vec![parallel_step(
        "fan",
        JoinMode::Any,
        vec![target_step("br_a", "a"), target_step("br_b", "b")],
    )]);
    let outcome = run_job(&host, &job, Value::Null, "run-par-any-ok");
    assert!(outcome.success);
}

#[test]
fn parallel_join_quorum_uses_count_threshold() {
    let host = ScriptedHost::new([
        ("a", vec![Action::Ok(json!({"a": true}))]),
        ("b", vec![Action::Ok(json!({"b": true}))]),
        (
            "c",
            vec![Action::Err(DispatchError::DeterministicActionFailed {
                action: "c".into(),
                message: "no".into(),
            })],
        ),
    ]);
    let job = job_with_steps(vec![parallel_step(
        "fan",
        JoinMode::Quorum { n: 2 },
        vec![
            target_step("br_a", "a"),
            target_step("br_b", "b"),
            target_step("br_c", "c"),
        ],
    )]);
    let outcome = run_job(&host, &job, Value::Null, "run-par-quorum");
    assert!(outcome.success);
}

#[test]
fn parallel_emits_step_join_event_with_branch_outcomes_in_declaration_order() {
    let host = ScriptedHost::new([
        ("a", vec![Action::Ok(json!({"a": true}))]),
        (
            "b",
            vec![Action::Err(DispatchError::DeterministicActionFailed {
                action: "b".into(),
                message: "x".into(),
            })],
        ),
    ]);
    let job = job_with_steps(vec![parallel_step(
        "fan",
        JoinMode::Any,
        vec![target_step("br_a", "a"), target_step("br_b", "b")],
    )]);
    let writer = std::sync::Arc::new(test_writer("run-par-join-event"));
    let _outcome = execute_job(
        &job,
        Value::Null,
        "run-par-join-event",
        writer.clone(),
        &host,
    )
    .expect("execute_job ok");
    let events = writer.events_snapshot().expect("audit");
    let join = events
        .iter()
        .find_map(|e| match &e.kind {
            V2AuditEventKind::StepJoin {
                branch_outcomes, ..
            } => Some(branch_outcomes.clone()),
            _ => None,
        })
        .expect("StepJoin event present");
    let ids: Vec<_> = join.into_iter().map(|b| b.branch_id).collect();
    assert_eq!(ids, vec!["br_a".to_string(), "br_b".to_string()]);
}

#[test]
fn parallel_inherits_audit_parent_stack_into_each_branch() {
    // Regression guard for the `thread::scope` parent-stack inheritance path
    // in parallel.rs:18-21. Each branch must observe the inherited parent
    // stack so events emitted inside the branch carry a parent_event_id.
    let host = ScriptedHost::new([
        ("a", vec![Action::Ok(json!({"a": true}))]),
        ("b", vec![Action::Ok(json!({"b": true}))]),
    ]);
    let job = job_with_steps(vec![parallel_step(
        "outer",
        JoinMode::All,
        vec![target_step("br_a", "a"), target_step("br_b", "b")],
    )]);
    let writer = std::sync::Arc::new(test_writer("run-par-parent"));
    let _ = execute_job(&job, Value::Null, "run-par-parent", writer.clone(), &host)
        .expect("execute_job ok");
    let events = writer.events_snapshot().expect("audit");

    // The outer parallel step's StepStarted is the parent every branch event
    // must reference.
    let outer_started_id = events
        .iter()
        .find_map(|e| match &e.kind {
            V2AuditEventKind::StepStarted { step_id } if step_id == "outer" => {
                Some(e.envelope.event_id.clone())
            }
            _ => None,
        })
        .expect("outer StepStarted event_id");

    for branch_id in ["br_a", "br_b"] {
        let started = events
            .iter()
            .find(|e| {
                matches!(
                    &e.kind,
                    V2AuditEventKind::StepStarted { step_id } if step_id == branch_id
                )
            })
            .unwrap_or_else(|| panic!("missing StepStarted for branch `{branch_id}`"));
        assert_eq!(
            started.envelope.parent_event_id.as_deref(),
            Some(outer_started_id.as_str()),
            "branch `{branch_id}` did not inherit the parallel step's parent_event_id"
        );
    }
}

/// Two `local_shell` branches of `sleep 2` must overlap. Before the
/// signal-handler mutex was refcounted, each `supervise_child` held it for
/// the child's lifetime and the block took ~4 s.
#[cfg(unix)]
#[test]
fn parallel_local_shell_sleeps_overlap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let host = LocalShellHost {
        root: dir.path().to_path_buf(),
    };
    let job = job_with_steps(vec![parallel_step(
        "fan",
        JoinMode::All,
        vec![
            local_shell_sleep_step("br_a", 2),
            local_shell_sleep_step("br_b", 2),
        ],
    )]);
    let started = Instant::now();
    let writer = std::sync::Arc::new(test_writer("run-par-shell-sleep"));
    let outcome = execute_job(&job, Value::Null, "run-par-shell-sleep", writer, &host)
        .expect("execute_job ok");
    assert!(outcome.success, "parallel local_shell job failed");
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(3_000),
        "parallel local_shell sleep 2 branches serialized: {elapsed:?}"
    );
    assert!(
        elapsed >= Duration::from_millis(1_500),
        "sleep 2 branches finished too quickly to have slept: {elapsed:?}"
    );
}

#[cfg(unix)]
struct LocalShellHost {
    root: PathBuf,
}

#[cfg(unix)]
impl RuntimeHost for LocalShellHost {
    fn repo_root(&self) -> Result<String, orbit_common::OrbitError> {
        Ok(self.root.to_string_lossy().into_owned())
    }
}

#[cfg(unix)]
fn local_shell_sleep_step(id: &str, seconds: u32) -> JobV2Step {
    JobV2Step {
        id: id.to_string(),
        when: None,
        retry: None,
        recovery_activity: None,
        resolved_recovery_activity: None,
        body: JobV2StepBody::Target(TargetStep {
            spec: ActivityV2Spec::Deterministic(DeterministicSpec {
                action: "local_shell".to_string(),
                config: json!({
                    "shell": "/bin/sh",
                    "script": format!("sleep {seconds}"),
                    "timeout_ms": 10_000u64,
                }),
            }),
            activity_name: None,
            input_schema_json: None,
            fs_profile: None,
            default_input: None,
            timeout_seconds: 0,
            session: None,
        }),
    }
}
