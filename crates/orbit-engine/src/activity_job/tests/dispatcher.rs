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
        DispatchError::TaskCompletionLiveRun {
            task_id: "T1".into(),
            run_id: "jrun-live".into(),
        },
        DispatchError::DeterministicActionRefused {
            action: "review_gate_settle".into(),
            message: "review_gate_blocked: verdict changes_required".into(),
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
