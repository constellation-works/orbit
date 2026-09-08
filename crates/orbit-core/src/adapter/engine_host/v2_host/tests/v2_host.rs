//! Sibling tests for `mod.rs` (the v2_host module root; migrated per ORB-10387 /
//! docs/design-patterns/test_layout.md).

use orbit_store::InvocationQuery;
use orbit_types::telemetry::{InvocationTrace, TokenUsage, ToolCallTrace};
use orbit_types::workflow::JobRunState;

use super::super::test_support::runtime_with_workspace_layout;
use super::super::*;

fn seed_running_job_run(runtime: &OrbitRuntime, job_id: &str) -> String {
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(job_id, 1, chrono::Utc::now(), None, None)
        .expect("insert job run");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, chrono::Utc::now(), std::process::id())
        .expect("mark run running");
    run.run_id
}

fn runtime_with_recovery_config(config: &str) -> (tempfile::TempDir, OrbitRuntime) {
    let root = tempfile::tempdir().expect("create tempdir");
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).expect("global orbit dir");
    std::fs::create_dir_all(&workspace).expect("workspace orbit dir");
    std::fs::write(workspace.join("config.toml"), config).expect("write recovery config");
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("build runtime");
    (root, runtime)
}

#[test]
fn system_crew_dispatch_uses_configuration_and_records_selected_provider_model() {
    let config = r#"
[workflow]
default_crew = "sol"
system_crew = "qa"

[crews.sol]
model = "gpt-5.6-sol"
provider = "codex"
backend = "cli"

[crews.qa]
model = "gpt-5.6-terra"
provider = "codex"
backend = "cli"
"#;
    let (_root, runtime) = runtime_with_recovery_config(config);
    let run_id = seed_running_job_run(&runtime, "recovery_telemetry_job");
    assert_eq!(
        RuntimeHost::system_crew_for_dispatch(&runtime).as_deref(),
        Some("qa")
    );
    let recovery = RuntimeHost::agent_crew_config_for_input(
        &runtime,
        &serde_json::json!({ "crew": "qa", "crew_config_key": "workflow.system_crew" }),
    )
    .expect("resolve configured system crew")
    .expect("configured system crew exists");

    RuntimeHost::persist_invocation_trace(
        &runtime,
        &run_id,
        "step_failure_recovery",
        recovery.provider.expect("configured provider").as_str(),
        recovery.model.as_deref(),
        &serde_json::json!({ "task_id": "ORB-10621" }),
        &InvocationTrace::default(),
    )
    .expect("persist recovery invocation");

    let records = runtime
        .invocation_records(InvocationQuery {
            job_run_id: Some(run_id),
            limit: 1,
            ..InvocationQuery::default()
        })
        .expect("query recovery invocation");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].agent, "codex");
    assert_eq!(records[0].model.as_deref(), Some("gpt-5.6-terra"));
}

fn payload_tool_call(seq: u32, tool_name: &str, payload: Value) -> ToolCallTrace {
    ToolCallTrace {
        seq,
        tool_name: tool_name.to_string(),
        result_bytes: serde_json::to_vec(&payload)
            .expect("serialize payload")
            .len() as u64,
        result_payload: Some(payload),
    }
}

fn byte_count_tool_call(seq: u32, tool_name: &str, result_bytes: u64) -> ToolCallTrace {
    ToolCallTrace {
        seq,
        tool_name: tool_name.to_string(),
        result_bytes,
        result_payload: None,
    }
}

fn trace_with_tool_calls(input_tokens: u64, tool_calls: Vec<ToolCallTrace>) -> InvocationTrace {
    InvocationTrace {
        usage: TokenUsage {
            input: input_tokens,
            cache_read: 0,
            cache_create: 0,
            cache_create_1h: 0,
            output: 0,
        },
        tool_calls,
        duration_ms: 10,
        provider_model: None,
        provider_cost_usd: None,
    }
}

#[test]
fn persist_invocation_trace_prefers_provider_model_over_requested_alias() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let run_id = seed_running_job_run(&runtime, "provider_model_job");
    let trace = InvocationTrace {
        provider_model: Some("claude-fable-5".to_string()),
        ..InvocationTrace::default()
    };

    RuntimeHost::persist_invocation_trace(
        &runtime,
        &run_id,
        "implement_one",
        "claude",
        Some("fable"),
        &serde_json::json!({ "task_id": "ORB-10370" }),
        &trace,
    )
    .expect("persist provider model");

    let records = runtime
        .invocation_records(InvocationQuery {
            job_run_id: Some(run_id),
            limit: 1,
            ..InvocationQuery::default()
        })
        .expect("query invocation records");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].agent, "claude");
    assert_eq!(records[0].model.as_deref(), Some("claude-fable-5"));
}

#[test]
fn persist_invocation_trace_defers_token_scoreboard_refresh_to_the_sweep() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    let run_id = seed_running_job_run(&runtime, "deferred_scoreboard_job");

    RuntimeHost::persist_invocation_trace(
        &runtime,
        &run_id,
        "implement_one",
        "codex",
        Some("gpt-test"),
        &serde_json::json!({}),
        &InvocationTrace::default(),
    )
    .expect("persist invocation trace");

    assert!(
        !repo_root
            .join(".orbit/state/scoreboard/tokens.json")
            .exists(),
        "trace persistence must not synchronously refresh the scoreboard"
    );
}

fn persist_test_trace(runtime: &OrbitRuntime, run_id: &str, trace: &InvocationTrace) {
    RuntimeHost::persist_invocation_trace(
        runtime,
        run_id,
        "knowledge_step",
        "codex",
        Some("gpt-test"),
        &serde_json::json!({ "task_id": "ORB-KNOWLEDGE-TEST" }),
        trace,
    )
    .expect("persist invocation trace");
}

#[test]
fn persist_invocation_trace_no_longer_measures_removed_pack_tool() {
    // ORB-00391 / ORB-10828: retired measured builtins no longer produce
    // knowledge metrics. A trace whose only payload tool is a former measured
    // tool records none.
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let run_id = seed_running_job_run(&runtime, "knowledge_pack_job");
    let trace = trace_with_tool_calls(
        155,
        vec![payload_tool_call(
            1,
            "orbit.graph.pack",
            serde_json::json!({
                "raw_read_token_baseline": 400,
                "knowledge_pack_tokens": 100,
                "entries": [{ "selector": "file:src/lib.rs", "source": "pub fn demo() {}" }],
                "unresolved_selectors": [],
            }),
        )],
    );

    persist_test_trace(&runtime, &run_id, &trace);

    let run = runtime.show_job_run(&run_id).expect("show job run");
    assert_eq!(run.state, JobRunState::Running);
    assert!(
        run.knowledge_metrics.is_none(),
        "the removed pack tool must not produce knowledge metrics"
    );
    assert_eq!(run.job_id, "knowledge_pack_job");
}

#[test]
fn persist_invocation_trace_does_not_record_retired_read_token_metrics() {
    // ORB-10828: read-token gauges are historical only. A fresh trace must not
    // invent knowledge metrics from a leftover measured-tool name.
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();

    let fallback_run_id = seed_running_job_run(&runtime, "knowledge_fallback_job");
    let fallback_trace =
        trace_with_tool_calls(50, vec![byte_count_tool_call(1, "orbit.task.show", 120)]);

    persist_test_trace(&runtime, &fallback_run_id, &fallback_trace);

    let fallback_run = runtime
        .show_job_run(&fallback_run_id)
        .expect("show fallback job run");
    assert!(
        fallback_run.knowledge_metrics.is_none(),
        "new traces must not populate retired read-token gauges"
    );
}

#[test]
fn tool_context_for_activity_passes_proc_allowlist() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();

    // No allowlist -> still activity-scoped, so the empty list denies every
    // program instead of degrading to allow-all. [ORB-10959]
    let undeclared = <OrbitRuntime as RuntimeHost>::tool_context_for_activity(
        &runtime,
        Some("run-allowlist-test"),
        None,
        None,
        None,
    );
    assert!(undeclared.proc_allowed_programs.is_empty());
    assert!(
        undeclared.proc_spawn_activity_scoped,
        "a v2 activity context must never hand proc.spawn an unscoped allow-all"
    );

    // Activity-scoped allowlist propagates verbatim and flips the bool.
    let programs = vec!["git".to_string(), "rg".to_string()];
    let scoped = <OrbitRuntime as RuntimeHost>::tool_context_for_activity(
        &runtime,
        Some("run-allowlist-test"),
        None,
        None,
        Some(programs.as_slice()),
    );
    assert_eq!(scoped.proc_allowed_programs, programs);
    assert!(scoped.proc_spawn_activity_scoped);

    // Empty Some([]) is meaningful: fail-closed when activity-scoped.
    let empty_scoped = <OrbitRuntime as RuntimeHost>::tool_context_for_activity(
        &runtime,
        Some("run-allowlist-test"),
        None,
        None,
        Some(&[]),
    );
    assert!(empty_scoped.proc_allowed_programs.is_empty());
    assert!(empty_scoped.proc_spawn_activity_scoped);
}

/// Run the real recovery dispatch through crew resolution, task loading,
/// worktree validation and Linux policy compilation. Stop at the launcher with
/// a deterministic failure so this test needs no provider or user namespaces.
#[cfg(target_os = "linux")]
#[test]
fn conflict_recovery_prepares_assigned_context_and_persists_launch_failure() {
    use super::super::test_support::{runtime_with_workspace_config, seed_executor};
    use orbit_engine::activity_job::{V2ActivityCatalog, load_job_asset};
    use orbit_engine::{DispatchError, V2AuditWriter, execute_job_with_resume};
    use orbit_types::workflow::PipelineState;

    let (_root, runtime, primary) = runtime_with_workspace_config(Some(
        r#"
[workflow]
default_crew = "repair"
system_crew = "repair"
[crews.repair]
provider = "codex"
model = "gpt-5.6-luna"
"#,
    ));
    seed_executor(
        &runtime,
        "codex",
        Some(orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap),
    );
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&primary)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init"]);
    git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "commit",
        "--allow-empty",
        "-m",
        "base",
    ]);
    let assigned = runtime
        .paths()
        .orbit_dir
        .join("state/worktrees/orbit-recovery-fixture");
    git(&[
        "worktree",
        "add",
        "-b",
        "candidate",
        assigned.to_str().unwrap(),
    ]);
    let task = runtime
        .add_task(crate::application::task::TaskAddParams {
            title: "Conflict recovery boundary".to_string(),
            description: "Retain the candidate assignment.".to_string(),
            plan: "Exercise preparation.".to_string(),
            ..Default::default()
        })
        .unwrap();
    let mut job = load_job_asset(include_str!(
        "../../../../../assets/jobs/task_pr_pipeline.yaml"
    ))
    .unwrap()
    .spec;
    let mut catalog = V2ActivityCatalog::new();
    catalog
        .load_dir(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/activities"))
        .unwrap();
    orbit_engine::resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();
    let sync_base = job
        .steps
        .iter_mut()
        .find(|step| step.id == "sync_base")
        .unwrap();
    let orbit_types::workflow::activity_job::JobV2StepBody::Target(target) = &mut sync_base.body
    else {
        panic!("resolved target")
    };
    let orbit_types::workflow::activity_job::ActivityV2Spec::Deterministic(spec) = &mut target.spec
    else {
        panic!("deterministic rebase")
    };
    // Supply the typed conflict at the VCS boundary; all shipped templates,
    // recovery routing and downstream preparation remain real.
    spec.action = "conflict_fixture".to_string();
    // Candidate publication is covered by the VCS handoff tests. This fixture
    // stops at recovery preparation and must not publish any test candidate.
    job.failure_activity = None;
    job.resolved_failure_activity = None;
    let run_id = "run-conflict-boundary";
    let input = serde_json::json!({"task_ids": [task.id], "allowed_crews": ["repair"]});
    let mut resume = PipelineState::new(
        run_id.to_string(),
        "task_pr_pipeline".to_string(),
        input.clone(),
    );
    for (index, output) in [
        serde_json::json!({"job_run_id": run_id, "workspace_path": assigned}),
        serde_json::json!({}),
        serde_json::json!({"skipped_no_diff_expected": false}),
        serde_json::json!({
            "head": "candidate", "head_sha": "candidate-sha", "base": "agent-main",
            "base_ref": "origin/agent-main", "base_sha": "target", "remote_sha": null,
            "commits_behind": 1, "sync_required": true,
        }),
    ]
    .into_iter()
    .enumerate()
    {
        resume
            .step_states
            .insert(index as u32, JobRunState::Success);
        resume.step_outputs.insert(index as u32, output);
    }
    let audit = V2AuditWriter::with_disk_sinks(
        &runtime.paths().orbit_dir.join("state/audit"),
        runtime.v2_audit_store().unwrap(),
        "fixture",
        run_id,
        "test",
        Some(&primary),
    )
    .unwrap();
    let host = RecoveryPreparationHost {
        runtime: &runtime,
        assigned: &assigned,
        task_id: &task.id,
    };
    let error = execute_job_with_resume(&job, input, run_id, audit.clone(), &host, Some(&resume))
        .unwrap_err();
    assert!(
        matches!(error, DispatchError::RecoverableVcsConflict { .. }),
        "unexpected error: {error:?}"
    );
    let stored = runtime
        .v2_audit_store()
        .unwrap()
        .list_v2_audit_events(&orbit_store::contracts::V2AuditEventFilter {
            workspace_id: "fixture".to_string(),
            run_id: Some(run_id.to_string()),
            event_type: Some("step.recovery_attempted".to_string()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(stored.len(), 1);
    let persisted: Value = serde_json::from_str(&stored[0].payload_json).unwrap();
    assert_eq!(persisted["failure_phase"], "dispatch");
    assert!(
        !stored[0]
            .payload_json
            .contains("sk-proj-abcdefghijklmnopqrstuvwxyz0123456789")
    );
    let events = audit.events_snapshot().unwrap();
    let event = events
        .iter()
        .find(|event| event.envelope.event_type == "step.recovery_attempted")
        .unwrap();
    let event = serde_json::to_value(event).unwrap();
    assert_eq!(event["failure_phase"], "dispatch");
    assert!(
        event["error_message"]
            .as_str()
            .unwrap()
            .contains("fixture launcher refused after managed sandbox compilation")
    );
    assert!(
        !event
            .to_string()
            .contains("sk-proj-abcdefghijklmnopqrstuvwxyz0123456789")
    );
    assert!(
        !events
            .iter()
            .any(|event| event.envelope.event_type == "cli.invocation.started")
    );
}

#[cfg(target_os = "linux")]
struct RecoveryPreparationHost<'a> {
    runtime: &'a OrbitRuntime,
    assigned: &'a std::path::Path,
    task_id: &'a str,
}

#[cfg(target_os = "linux")]
impl RuntimeHost for RecoveryPreparationHost<'_> {
    fn run_deterministic(
        &self,
        _: &str,
        _: &Value,
        _: &Value,
        _: orbit_tools::ToolContext,
    ) -> Result<Value, orbit_engine::DispatchError> {
        Err(orbit_engine::DispatchError::RecoverableVcsConflict {
            operation: "git_rebase".to_string(),
            original_base_sha: "original".to_string(),
            target_base_sha: "target".to_string(),
            conflicting_paths: vec!["src/lib.rs".to_string()],
            diagnostic: "original conflict".to_string(),
        })
    }

    fn system_crew_for_dispatch(&self) -> Option<String> {
        self.runtime.system_crew_for_dispatch()
    }

    fn agent_crew_config_for_input(
        &self,
        input: &Value,
    ) -> Result<Option<orbit_engine::CrewConfig>, orbit_engine::DispatchError> {
        assert_eq!(input["crew"], "repair");
        assert_eq!(input["allowed_crews"], serde_json::json!(["repair"]));
        self.runtime.agent_crew_config_for_input(input)
    }

    fn resolve_cli_executor(
        &self,
        provider: &str,
    ) -> Result<orbit_engine::ResolvedCliExecutor, orbit_engine::DispatchError> {
        assert_eq!(provider, "codex");
        self.runtime.resolve_cli_executor(provider)
    }

    fn resolve_activity_tools(
        &self,
        ids: &[String],
        baseline: &[String],
    ) -> Result<orbit_engine::ResolvedActivityTools, orbit_engine::DispatchError> {
        assert_eq!(ids, &[self.task_id.to_string()]);
        self.runtime.resolve_activity_tools(ids, baseline)
    }

    fn task_context_for_agent_input(
        &self,
        input: &Value,
    ) -> Result<Option<Value>, orbit_engine::DispatchError> {
        assert_eq!(input["failed_step_input"]["head"], "candidate");
        assert_eq!(input["failed_step_input"]["head_sha"], "candidate-sha");
        assert_eq!(
            input["failed_step_input"]["job_run_id"],
            "run-conflict-boundary"
        );
        let context = self.runtime.task_context_for_agent_input(input)?;
        assert_eq!(context.as_ref().unwrap()["id"], self.task_id);
        assert_eq!(
            context.as_ref().unwrap()["workspace_path"],
            self.assigned.display().to_string()
        );
        Ok(context)
    }

    fn tool_context_for_activity(
        &self,
        run: Option<&str>,
        profile: Option<&str>,
        audit: Option<std::sync::Arc<dyn orbit_tools::FsAuditLogger>>,
        programs: Option<&[String]>,
    ) -> orbit_tools::ToolContext {
        self.runtime
            .tool_context_for_activity(run, profile, audit, programs)
    }

    fn resolve_executor_sandbox(
        &self,
        provider: &str,
        profile: Option<&str>,
        cwd: Option<&std::path::Path>,
    ) -> Result<Option<orbit_engine::ResolvedSandbox>, orbit_engine::DispatchError> {
        assert_eq!(cwd, Some(self.assigned));
        let sandbox = self
            .runtime
            .resolve_executor_sandbox(provider, profile, cwd)?
            .unwrap();
        assert!(sandbox.managed_worktree);
        let grants =
            orbit_exec::prepare_linux_bwrap_write_grants(&sandbox.fs_profile, self.assigned)
                .unwrap();
        assert!(grants.unsatisfied.is_empty());
        orbit_exec::compile_linux_bwrap_argv(
            &sandbox.fs_profile,
            "/bin/true",
            &[],
            cwd,
            sandbox.managed_worktree,
        )
        .unwrap();

        // Reproduce the original missing-worktree preparation using the same
        // host/policy: default cwd is the primary and the direct writer fails.
        let primary = &self.runtime.paths().repo_root;
        let direct = self
            .runtime
            .resolve_executor_sandbox(provider, profile, Some(primary))?
            .unwrap();
        let error = orbit_exec::compile_linux_bwrap_argv(
            &direct.fs_profile,
            "/bin/true",
            &[],
            Some(primary),
            direct.managed_worktree,
        )
        .unwrap_err();
        assert!(error.to_string().contains("non-subtree denyModify"));
        Err(orbit_engine::DispatchError::CliInvocationPermanent(
            "fixture launcher refused after managed sandbox compilation; token=sk-proj-abcdefghijklmnopqrstuvwxyz0123456789".to_string(),
        ))
    }
}
