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

    assert!(matches!(
        dispatch_error_to_orbit(DispatchError::TaskCompletionLiveRun {
            task_id: "T1".into(),
            run_id: "jrun-live".into(),
        }),
        OrbitError::TaskCompletionLiveRun { task_id, run_id }
            if task_id == "T1" && run_id == "jrun-live"
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

/// [ORB-13115] A deterministic step's tools learn which task and run they
/// serve from the dispatcher, not from the tool arguments the step forwards.
/// The host binds the run; the dispatcher adds the task from the run's own
/// input, the value a CLI agent step would export as `ORBIT_TASK_ID`.
#[test]
fn a_deterministic_step_binds_its_task_and_run_from_the_dispatch_not_the_tool_args() {
    use std::sync::{Arc, Mutex};

    use orbit_agent::loop_engine::audit::{AuditSink, NullSink};
    use orbit_tools::{ActivityBinding, ToolContext};
    use orbit_types::workflow::activity_job::{ActivityV2Spec, DeterministicSpec};
    use serde_json::{Value, json};

    use super::super::audit_writer::V2AuditWriter;
    use super::super::dispatcher::{DispatchError, V2DispatchInput, dispatch_v2_activity};
    use crate::context::RuntimeHost;

    #[derive(Default)]
    struct BindingHost {
        seen: Mutex<Option<ToolContext>>,
    }

    impl RuntimeHost for BindingHost {
        fn tool_context_for_activity(
            &self,
            run_id: Option<&str>,
            _: Option<&str>,
            _: Option<Arc<dyn orbit_tools::FsAuditLogger>>,
            _: Option<&[String]>,
        ) -> ToolContext {
            ToolContext {
                activity_binding: run_id.map(|job_run_id| ActivityBinding {
                    job_run_id: job_run_id.to_string(),
                    task_id: None,
                }),
                ..ToolContext::default()
            }
        }

        fn run_deterministic(
            &self,
            _: &str,
            _: &Value,
            _: &Value,
            tool_context: ToolContext,
        ) -> Result<Value, DispatchError> {
            *self.seen.lock().expect("seen") = Some(tool_context);
            Ok(json!({}))
        }
    }

    let spec = ActivityV2Spec::Deterministic(DeterministicSpec {
        action: "plugin.tool_call".to_string(),
        config: Value::Null,
    });
    let sink: Arc<dyn AuditSink> = Arc::new(NullSink);
    let dispatch = |host: &BindingHost, input: Value| {
        dispatch_v2_activity(V2DispatchInput {
            activity_name: "publish",
            spec: &spec,
            fs_profile: None,
            input,
            audit: Arc::new(V2AuditWriter::new("jrun-host", "test", sink.clone())),
            run_id: "jrun-host",
            host: Some(host),
        })
        .expect("dispatch");
        host.seen
            .lock()
            .expect("seen")
            .take()
            .and_then(|context| context.activity_binding)
            .expect("an activity context carries its binding")
    };

    let host = BindingHost::default();
    let binding = dispatch(
        &host,
        json!({
            "task_id": "ORB-7",
            "tool": "pulsar.publish",
            "input": { "task_id": "ORB-999", "job_run_id": "jrun-forged" },
        }),
    );
    assert_eq!(
        binding,
        ActivityBinding {
            job_run_id: "jrun-host".to_string(),
            task_id: Some("ORB-7".to_string()),
        },
        "the plugin's own arguments must not name the task or run it is bound to"
    );

    let binding = dispatch(&host, json!({ "tool": "pulsar.publish" }));
    assert_eq!(binding.job_run_id, "jrun-host");
    assert_eq!(binding.task_id, None, "a step serving no task names none");
}
