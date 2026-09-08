#![allow(missing_docs)]

#[test]
fn dispatch_error_retryability_classification_table() {
    // ORB-10006: retryable-vs-permanent classification consumed by the step
    // retry wrapper. Permanent (non-retryable) errors fail fast; transient
    // ones burn retry attempts.
    use super::super::dispatcher::DispatchError;

    let permanent: Vec<DispatchError> = vec![
        DispatchError::ToolDenied {
            tool_name: "fs.write".into(),
            iteration: 1,
        },
        DispatchError::DeterministicActionNotRegistered("nope".into()),
        DispatchError::JobValidation("bad".into()),
        DispatchError::RetryConfigInvalid {
            step_id: "s".into(),
            field: "max_attempts",
            value: 0,
            invariant: "max_attempts >= 1".into(),
        },
        DispatchError::HostRequired("host"),
        DispatchError::CliInvocationPermanent("agent config: bad model".into()),
        DispatchError::WorktreeIntegrity {
            code: "worktree_escape",
            diagnostic: r#"{"task_id":"ORB-1"}"#.into(),
        },
        DispatchError::RecoverableVcsConflict {
            operation: "git_rebase".into(),
            original_base_sha: "base-before".into(),
            target_base_sha: "base-target".into(),
            conflicting_paths: vec!["src/lib.rs".into()],
            diagnostic: "stopped on unmerged index entries".into(),
        },
    ];
    for err in &permanent {
        assert!(err.is_non_retryable(), "expected non-retryable: {err:?}");
    }

    let transient: Vec<DispatchError> = vec![
        DispatchError::CliInvocationFailed("spawn claude: EAGAIN".into()),
        DispatchError::AgentLoopFailed("overloaded".into()),
        DispatchError::DeterministicActionFailed {
            action: "a".into(),
            message: "flaky".into(),
        },
        DispatchError::JobExecution("executor hiccup".into()),
        DispatchError::AuditFailed("sink".into()),
    ];
    for err in &transient {
        assert!(!err.is_non_retryable(), "expected retryable: {err:?}");
    }
}

#[test]
fn dispatch_error_to_orbit_keeps_validation_variant_and_buckets_the_rest() {
    use orbit_common::OrbitError;

    use super::super::dispatcher::{DispatchError, dispatch_error_to_orbit};

    assert!(matches!(
        dispatch_error_to_orbit(DispatchError::JobValidation("bad spec".into())),
        OrbitError::JobValidation(m) if m == "bad spec"
    ));

    let other = DispatchError::AgentLoopFailed("overloaded".into());
    let expected = other.to_string();
    assert!(matches!(
        dispatch_error_to_orbit(other),
        OrbitError::InvalidInput(m) if m == expected
    ));

    let conflict = DispatchError::RecoverableVcsConflict {
        operation: "git_rebase".into(),
        original_base_sha: "base-before".into(),
        target_base_sha: "base-target".into(),
        conflicting_paths: vec!["src/lib.rs".into()],
        diagnostic: "stopped on unmerged index entries".into(),
    };
    assert!(matches!(
        dispatch_error_to_orbit(conflict),
        OrbitError::RecoverableVcsConflict(details)
            if details.operation == "git_rebase"
                && details.original_base_sha == "base-before"
                && details.target_base_sha == "base-target"
                && details.conflicting_paths == ["src/lib.rs"]
    ));
}

#[test]
fn inject_run_id_fills_absent_run_id() {
    use super::super::dispatcher::inject_run_id;
    use serde_json::json;

    let injected = inject_run_id(&json!({ "task_ids": ["ORB-1"] }), "jrun-admitted");
    assert_eq!(injected["run_id"], "jrun-admitted");
    assert!(injected.get("job_run_id").is_none());
}

#[test]
fn inject_run_id_exposes_the_admitted_job_beside_an_explicit_worktree_token() {
    use super::super::dispatcher::inject_run_id;
    use serde_json::json;

    let injected = inject_run_id(
        &json!({
            "task_ids": ["ORB-EPIC"],
            "run_id": "epic-ORB-EPIC",
        }),
        "jrun-admitted",
    );
    assert_eq!(injected["run_id"], "epic-ORB-EPIC");
    assert_eq!(injected["job_run_id"], "jrun-admitted");
}

#[test]
fn inject_run_id_does_not_overwrite_an_explicit_job_run_id() {
    use super::super::dispatcher::inject_run_id;
    use serde_json::json;

    let injected = inject_run_id(
        &json!({
            "run_id": "epic-ORB-EPIC",
            "job_run_id": "jrun-already",
        }),
        "jrun-admitted",
    );
    assert_eq!(injected["run_id"], "epic-ORB-EPIC");
    assert_eq!(injected["job_run_id"], "jrun-already");
}
