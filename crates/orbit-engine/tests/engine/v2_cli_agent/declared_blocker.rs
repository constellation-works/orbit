//! A declared-failed implementer envelope (`status: "failed"`, exit 0)
//! whose `error.code` names a task-level stop is held as
//! `task_blocked_by_agent` at the implement-step boundary, like the
//! `blocker: {kind, evidence}` shape [ORB-25393].

use std::sync::Mutex;

use orbit_types::workflow::HeldFailure;
use serde_json::json;

use super::*;

const VALIDATION_BLOCKER: &str = r#"{"schemaVersion":1,"status":"failed","result":{},"error":{"code":"validation_blocker","message":"Criterion 2 needs a live pilot check that this lane cannot run."}}"#;

struct DeclaredBlockerHost {
    cli: ScriptHost,
    calls: Mutex<Vec<(String, Value)>>,
}

impl DeclaredBlockerHost {
    fn actions(&self) -> Vec<String> {
        self.calls
            .lock()
            .expect("call log")
            .iter()
            .map(|(action, _)| action.clone())
            .collect()
    }

    fn preserve_call(&self) -> Value {
        self.calls
            .lock()
            .expect("call log")
            .iter()
            .find(|(action, _)| action == "preserve_candidate")
            .map(|(_, input)| input.clone())
            .expect("failure activity ran")
    }
}

impl orbit_engine::test_support::NullRuntimeHost for DeclaredBlockerHost {
    fn agent_subprocess_environment(&self, required_env_vars: &[&str]) -> Vec<(String, String)> {
        RuntimeHost::agent_subprocess_environment(&self.cli, required_env_vars)
    }

    fn run_deterministic(
        &self,
        action: &str,
        config: &Value,
        input: &Value,
        tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        self.calls
            .lock()
            .expect("call log")
            .push((action.to_string(), input.clone()));
        // The recovery activity must never run for a held blocker; record it
        // if it does so the action assertion below catches it, but do not
        // delegate to the CLI host (which refuses deterministic actions).
        let _ = (config, tool_context);
        Ok(json!({}))
    }

    fn resolve_cli_executor(&self, provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        RuntimeHost::resolve_cli_executor(&self.cli, provider)
    }

    fn tool_context_for_activity(
        &self,
        run_id: Option<&str>,
        fs_profile: Option<&str>,
        fs_audit: Option<Arc<dyn orbit_tools::FsAuditLogger>>,
        proc_allowed_programs: Option<&[String]>,
    ) -> orbit_tools::ToolContext {
        RuntimeHost::tool_context_for_activity(
            &self.cli,
            run_id,
            fs_profile,
            fs_audit,
            proc_allowed_programs,
        )
    }
}

fn scripted_activity(action: &str) -> ActivityV2 {
    ActivityV2 {
        description: String::new(),
        input_schema_json: Value::Null,
        output_schema_json: Value::Null,
        fs_profile: None,
        spec: ActivityV2Spec::Deterministic(DeterministicSpec {
            action: action.to_string(),
            config: Value::Null,
        }),
    }
}

#[cfg(unix)]
#[test]
fn a_validation_blocker_envelope_holds_the_implement_step_and_preserves_the_candidate() {
    let audit = tempfile::tempdir().unwrap();
    let count_path = audit.path().join("count");
    let fake = fake_cli(
        "claude",
        &format!(
            "#!/bin/sh\ncat > /dev/null\nn=$(cat '{}' 2>/dev/null || echo 0)\nn=$((n+1))\necho \"$n\" > '{}'\necho 'partial candidate' >> '{}'\ncat <<'PAYLOAD'\n{VALIDATION_BLOCKER}\nPAYLOAD\nexit 0\n",
            count_path.display(),
            count_path.display(),
            audit.path().join("candidate.txt").display(),
        ),
    )
    .unwrap();
    let host = DeclaredBlockerHost {
        cli: ScriptHost::new(fake.cli_path()),
        calls: Mutex::new(Vec::new()),
    };
    let (writer, _store) = build_writer(audit.path(), "declared-blocker").unwrap();
    let mut job = load_job_asset(
        &json!({
            "schemaVersion": 2, "kind": "Job", "metadata": {"name": "declared_blocker_fixture"},
            "spec": {"state": "enabled", "kind": "workflow", "failure_activity": "preserve_candidate",
                "final_recovery_activity": "final_look", "steps": [
                    {"id": "implement_one", "recovery_activity": "step_recovery",
                     "retry": {"max_attempts": 3, "initial_backoff_ms": 1, "backoff_cap_ms": 1},
                     "spec": ActivityV2Spec::AgentLoop(cli_agent_loop_spec(None))},
                    {"id": "commit", "spec": {"type": "deterministic", "action": "commit", "config": {}}}
                ]}
        })
        .to_string(),
    )
    .unwrap()
    .spec;
    let mut catalog = V2ActivityCatalog::new();
    for name in [
        "step_recovery",
        "preserve_candidate",
        "final_look",
        "commit",
    ] {
        catalog.insert(name, scripted_activity(name));
    }
    resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();
    let outcome = execute_job_with_resume(
        &job,
        json!({"task_id": "fixture-task"}),
        "declared-blocker",
        writer.clone(),
        &host,
        None,
    )
    .expect("the job runs to an outcome");

    assert!(!outcome.success, "{outcome:?}");
    let message = outcome.message.clone().expect("blocker message");
    let marker = orbit_types::workflow::TASK_BLOCKED_BY_AGENT_MARKER;
    assert!(
        message.contains(marker) && message.contains("kind=validation_blocker"),
        "held as a blocker with its kind: {message}"
    );
    assert!(
        message.contains("Criterion 2 needs a live pilot check"),
        "the envelope message is the evidence: {message}"
    );
    assert_eq!(
        HeldFailure::of_local(Some("task_blocked_by_agent"), Some(&message)),
        Some(HeldFailure::AgentBlocker),
        "the run holds, it does not fail"
    );
    assert_eq!(
        fs::read_to_string(&count_path).unwrap().trim(),
        "1",
        "no retry spends another provider invocation"
    );
    assert_eq!(
        host.actions(),
        vec!["preserve_candidate".to_string()],
        "only the failure activity runs: no recovery, no commit"
    );
    let preserve = host.preserve_call();
    assert_eq!(preserve["error_code"], "task_blocked_by_agent");
    let error_message = preserve["error_message"].as_str().expect("error message");
    assert!(
        error_message.contains(marker) && error_message.contains("kind=validation_blocker"),
        "the failure activity receives the kind: {error_message}"
    );
    assert_eq!(
        fs::read_to_string(audit.path().join("candidate.txt")).unwrap(),
        "partial candidate\n",
        "the candidate is preserved, not committed away"
    );
    let events = writer.events_snapshot().expect("audit events");
    assert!(
        !events.iter().any(|event| matches!(
            event.kind,
            orbit_types::workflow::activity_job::V2AuditEventKind::StepRecoveryAttempted { .. }
        )),
        "step recovery is skipped for a blocker"
    );
}
