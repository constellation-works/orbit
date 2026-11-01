//! Recovery authentication through the public CLI runner, readiness and purpose draw.

use chrono::{DateTime, Duration, Utc};
use orbit_engine::{RuntimeHost, V2AuditWriter};
use orbit_store::contracts::JobRunStepParams;
use orbit_types::resource::ExecutorResource;
use orbit_types::task::ExecutionLocation;
use orbit_types::workflow::activity_job::{AgentLoopSpec, OnDenial, Provider};
use orbit_types::workflow::{
    ExecutorDef, ExecutorSandboxKind, JobRunState, PipelineState, V2AuditEventKind,
};
use serde_json::{Value, json};

use super::provider_failure_hold::{Fixture, fixture_with_global};

const CONFIG: &str = r#"
[workflow]
default_crew = "sol"
[workflow.purpose_crews]
final_recovery = ["credits", "sol"]
failure_recovery = ["credits", "sol"]
[crews.sol]
provider = "codex"
model = "fixture"
[crews.opus]
provider = "claude"
model = "fixture"
[crews.credits]
provider = "claude"
model = "fixture"
"#;

fn audit(fx: &Fixture, run: &str) -> std::sync::Arc<V2AuditWriter> {
    V2AuditWriter::with_disk_sinks(
        &fx.repo.join(".orbit/audit"),
        std::sync::Arc::new(fx.runtime.sqlite_store().unwrap()),
        fx.runtime.workspace_id().unwrap(),
        run,
        "fixture",
        None,
    )
    .unwrap()
}

fn auth_card(fx: &Fixture) -> Value {
    fx.runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap()["provider_auth_failures"]
        .clone()
}

fn purpose_draw(fx: &Fixture, only_credits: bool) -> String {
    let run = fx
        .runtime
        .insert_job_run("fixture_draw", 1, Utc::now(), Some(json!({})), None)
        .unwrap();
    fx.runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id, json!({})),
        )
        .unwrap();
    fx.runtime
        .agent_crew_config_for_input(&json!({
            "job_run_id":run.run_id, "crew_config_key":"workflow.purpose_crews.failure_recovery",
            "allowed_crews": if only_credits { vec!["credits"] } else { vec!["credits", "sol"] },
        }))
        .unwrap()
        .unwrap()
        .name
        .unwrap()
}

/// Persist a controlled later/earlier invocation interval through the same
/// public audit API the runner uses; no sleeps or machine load are involved.
fn success(
    fx: &Fixture,
    provider: &str,
    account: Option<&str>,
    host: Option<&str>,
    started: DateTime<Utc>,
) {
    let run = fx
        .runtime
        .insert_job_run("fixture_success", 1, Utc::now(), None, None)
        .unwrap();
    let mut stored = fx.runtime.show_job_run(&run.run_id).unwrap();
    // Execution location is immutable on upsert; insert a distinct evidence row.
    stored.run_id.push_str("-evidence");
    stored.executed_on = host.map(|name| ExecutionLocation {
        machine_id: name.into(),
        machine_name: None,
    });
    fx.runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&fx.runtime.workspace_id().unwrap(), &stored, None)
        .unwrap();
    audit(fx, &stored.run_id)
        .emit(V2AuditEventKind::ProviderInvocationObserved {
            provider: provider.into(),
            crew: account.map(str::to_string),
            account: account.map(str::to_string),
            started_at: started,
            finished_at: Utc::now().max(started),
            success: true,
            failure_class: None,
        })
        .unwrap();
}

fn recovery_failure_is_observed_and_cleared_only_by_its_account_and_host() {
    use std::os::unix::fs::PermissionsExt;
    let fx = fixture_with_global(
        &format!("{CONFIG}env = [\"FIXTURE_KEY=RECOVERY_FIXTURE_VALUE\"]\n"),
        CONFIG,
    );
    let task = fx.task("sol");
    let run = fx.admit(&task, "sol");
    let before_recovery = Utc::now() - Duration::minutes(1);
    let primary_at = Utc::now();
    fx.runtime
        .complete_job_run_step(
            &run,
            &JobRunStepParams {
                step_index: 1,
                target_type: orbit_types::workflow::JobTargetType::Activity,
                target_id: "implement_one".into(),
                started_at: primary_at,
                finished_at: primary_at,
                duration_ms: Some(1),
                lock_wait_ms: None,
                exit_code: Some(1),
                agent_response_json: None,
                state: JobRunState::Failed,
                error_code: Some("STEP_FAILED".into()),
                error_message: Some("primary validation failed: incomplete evidence".into()),
            },
        )
        .unwrap();
    let bin = fx.repo.join("claude-fixture");
    std::fs::write(&bin, "#!/bin/sh\ncat > /dev/null\nprintf 'Authentication Error: HTTP 401 OAuth token revoked\\n' >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let mut executor: ExecutorResource = serde_yaml::from_str(
        &std::fs::read_to_string(assets.join("executors/claude.yaml")).unwrap(),
    )
    .unwrap();
    executor.spec.command = Some(bin.display().to_string());
    executor.spec.sandbox = Some(ExecutorSandboxKind::Off);
    fx.runtime
        .upsert_executor_def(&ExecutorDef::from_resource_spec(
            executor.metadata.name,
            executor.spec,
            None,
            None,
        ))
        .unwrap();
    let spec = AgentLoopSpec {
        crew_name: Some("credits".into()),
        crew_account: Some("credits".into()),
        instruction: "Return a recovery decision.".into(),
        steering_instruction: None,
        regression_risk_instruction: None,
        explore_instruction: None,
        explore_brief_instruction: None,
        tools: vec![],
        tool_disallow_list: None,
        on_denial: OnDenial::Terminate,
        model: None,
        reasoning_effort: None,
        max_iterations: 1,
        backend: None,
        provider: Provider::Claude,
        wall_clock_timeout_seconds: 10,
        require_response_envelope: true,
        require_completion_envelope: true,
        proc_allowed_programs: None,
        proc_disallowed_programs: None,
        trusted_host_execution: false,
        crew_env: vec![],
        crew_vision_disabled: false,
    };
    let outcome = orbit_engine::activity_job::cli_runner::run_cli_backend(
        &fx.runtime,
        &spec,
        "final_recovery",
        &run,
        audit(&fx, &run),
        &json!({"workspace_path":fx.repo, "crew":"credits"}),
        None,
    )
    .unwrap();
    assert!(!outcome.success);
    fx.runtime
        .finalize_job_run(&run, JobRunState::Failed, Utc::now(), Some(1))
        .unwrap();
    let primary = fx.runtime.show_job_run(&run).unwrap();
    assert_eq!(primary.state, JobRunState::Failed);
    assert_eq!(primary.steps[0].error_code.as_deref(), Some("STEP_FAILED"));
    assert_eq!(
        primary.steps[0].error_message.as_deref(),
        Some("primary validation failed: incomplete evidence")
    );
    let card = auth_card(&fx);
    assert_eq!(card.as_array().unwrap().len(), 1, "{card}");
    assert_eq!(
        (&card[0]["provider"], &card[0]["account"], &card[0]["host"]),
        (&json!("claude"), &json!("credits"), &json!("this host"))
    );
    assert_eq!(card[0]["runs"][0]["run_id"], run);
    assert_eq!(
        purpose_draw(&fx, false),
        "sol",
        "the unavailable account cannot be drawn"
    );

    // The enclosing run may recover or end held; neither outcome erases a
    // failed secondary invocation or counts its primary crew as recovery success.
    for state in [JobRunState::Success, JobRunState::Held] {
        let mut parent = primary.clone();
        parent.state = state;
        fx.runtime
            .sqlite_store()
            .unwrap()
            .upsert_job_run_for_workspace(&fx.runtime.workspace_id().unwrap(), &parent, None)
            .unwrap();
        assert_eq!(auth_card(&fx), card, "{state:?}");
        assert_eq!(purpose_draw(&fx, false), "sol");
    }
    for (provider, account, host, started) in [
        ("codex", Some("credits"), None, Utc::now()),
        ("claude", None, None, Utc::now()),
        ("claude", Some("credits"), Some("other-host"), Utc::now()),
        ("claude", Some("credits"), None, before_recovery),
    ] {
        success(&fx, provider, account, host, started);
        assert_eq!(
            auth_card(&fx),
            card,
            "unrelated/earlier success cannot clear: {provider} {account:?} {host:?} {started}"
        );
        assert_eq!(purpose_draw(&fx, false), "sol");
    }
    std::fs::write(&bin, r#"#!/bin/sh
cat > /dev/null
printf '%s\n' '{"type":"result","is_error":false,"result":"{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}"}'
"#).unwrap();
    let next = fx
        .runtime
        .insert_job_run("recovery_probe_fixture", 1, Utc::now(), None, None)
        .unwrap();
    assert!(
        orbit_engine::activity_job::cli_runner::run_cli_backend(
            &fx.runtime,
            &spec,
            "final_recovery",
            &next.run_id,
            audit(&fx, &next.run_id),
            &json!({"workspace_path":fx.repo, "crew":"credits"}),
            None,
        )
        .unwrap()
        .success
    );
    assert_eq!(auth_card(&fx), json!([]));
    assert_eq!(purpose_draw(&fx, true), "credits");
}

crate::scenario_table!(
    recovery_provider_auth_scenarios,
    recovery_failure_is_observed_and_cleared_only_by_its_account_and_host
);
