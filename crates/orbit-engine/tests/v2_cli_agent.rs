#![allow(missing_docs)]
// Integration fixtures exercise public behavior and unwrap setup invariants.
#![allow(
    clippy::expect_used,
    clippy::print_stderr,
    clippy::print_stdout,
    clippy::unwrap_used
)]

//! v2 CLI agent-dispatch integration coverage — T20260419-0104.
//!
//! Exercises the agent dispatch path, the §7.6 envelope events, the §6
//! harness-delegated allowlist advisory, the [ORB-10801] retired-declaration
//! rejections, argv redaction, and wall-clock timeout.
//!
//! The smoke substitutes the real `claude` CLI with tempdir shell scripts
//! named `claude` so `AgentConfig::from_cli_config` resolves them to the
//! retained `ClaudeRuntime` (§10.1 keep/delete table). This is what the task
//! AC #11 means by "substitutable" CLI.
//!
//! Runs under `cargo nextest run -p orbit-engine --test v2_cli_agent`.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use orbit_engine::activity_job::load_activity_asset;
use orbit_engine::{
    DispatchError, ResolvedCliExecutor, RuntimeHost, V2AuditWriter, V2DispatchInput,
    dispatch_v2_activity,
};
use orbit_types::workflow::JobScheduleState;
use orbit_types::workflow::activity_job::{
    ActivityV2Spec, AgentLoopSpec, JobKind, JobV2, JobV2Step, JobV2StepBody, LoopBlock, OnDenial,
    Provider, RetiredFeatureError, TargetStep, validate_job_retired_sessions,
};
use serde_json::Value;
use tempfile::TempDir;

#[test]
fn cli_agent_dispatch_regressions() -> Result<(), Box<dyn std::error::Error>> {
    scenario_a_cli_dispatch_emits_envelope_events()?;
    scenario_b_argv_redaction()?;
    scenario_c_wall_clock_timeout()?;
    scenario_e_loader_rejection_retired_session()?;
    scenario_f_loader_rejection_retired_backend_value()?;
    scenario_h_cli_reference_asset_round_trip()?;
    scenario_i_existing_agent_loop_assets_still_deserialize()?;
    scenario_j_cli_executor_static_args_are_audited()?;

    Ok(())
}

/// A: `backend: cli` against a fake `claude` binary produces
/// `tool_allowlist.harness_delegated`, `cli.invocation.started`, and
/// `cli.invocation.finished` envelope events.
fn scenario_a_cli_dispatch_emits_envelope_events() -> Result<(), Box<dyn std::error::Error>> {
    println!("  A) cli dispatch emits §6 + §7.6 envelope events");
    let tmp_audit = tempfile::tempdir()?;
    let (writer, _sink) = build_writer(tmp_audit.path(), "smoke-cli-a")?;

    // `claude` that ignores stdin and prints a canned reply. [ORB-10449] The
    // reply is a real Orbit response envelope: exiting 0 without one is now a
    // step-completion protocol violation, so a bare `{"status":"ok"}` would
    // fail the step for a reason this scenario is not about.
    let fake = fake_cli(
        "claude",
        "#!/bin/sh\ncat > /dev/null\necho '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
    )?;

    let spec = cli_agent_loop_spec(None);
    let host = ScriptHost::new(fake.cli_path());
    let outcome = dispatch_v2_activity(V2DispatchInput {
        activity_name: "cli_smoke_a",
        spec: &ActivityV2Spec::AgentLoop(spec),
        fs_profile: None,
        input: serde_json::json!({ "prompt": "hello" }),
        audit: writer.clone(),
        run_id: "smoke-cli-a",
        host: Some(&host),
    })?;

    assert!(outcome.success, "fake claude should exit 0");

    let events = writer.events_snapshot()?;
    let types: Vec<&str> = events
        .iter()
        .map(|e| e.envelope.event_type.as_str())
        .collect();
    must_contain(&types, "tool_allowlist.harness_delegated");
    must_contain(&types, "cli.invocation.started");
    must_contain(&types, "cli.invocation.finished");
    println!("    events: {:?}", types);
    Ok(())
}

/// B: argv carrying an `sk-...` token (via the `--model` flag set by
/// `claude_cli.rs`) is redacted in the persisted envelope event.
fn scenario_b_argv_redaction() -> Result<(), Box<dyn std::error::Error>> {
    println!("  B) argv redaction scrubs sk-... from --model arg");
    let tmp_audit = tempfile::tempdir()?;
    let (writer, _sink) = build_writer(tmp_audit.path(), "smoke-cli-b")?;

    let fake = fake_cli("claude", "#!/bin/sh\ncat > /dev/null\necho ok\n")?;

    // Plant the token in the spec's `model` field. ClaudeCliTransport passes
    // it on argv as `--model <value>` — the redactor sees it there.
    let mut spec = cli_agent_loop_spec(None);
    spec.model = Some("sk-ant-LEAKEDKEY99999".to_string());
    let host = ScriptHost::new(fake.cli_path());

    let _ = dispatch_v2_activity(V2DispatchInput {
        activity_name: "cli_smoke_b",
        spec: &ActivityV2Spec::AgentLoop(spec),
        fs_profile: None,
        input: serde_json::json!({ "prompt": "redact me" }),
        audit: writer.clone(),
        run_id: "smoke-cli-b",
        host: Some(&host),
    })?;

    let events = writer.events_snapshot()?;
    let started = events
        .iter()
        .find(|e| e.envelope.event_type == "cli.invocation.started")
        .ok_or("missing cli.invocation.started")?;
    let serialized = serde_json::to_string(started)?;
    assert!(
        !serialized.contains("sk-ant-LEAKEDKEY"),
        "sk- token leaked into envelope: {}",
        serialized
    );
    assert!(
        serialized.contains("[REDACTED_API_KEY]"),
        "expected [REDACTED_API_KEY] marker in {}",
        serialized
    );
    println!("    envelope redacted sk- token");
    Ok(())
}

/// C: 2s timeout against a 10s `sleep` — finishes within 5s with
/// `timed_out: true`.
fn scenario_c_wall_clock_timeout() -> Result<(), Box<dyn std::error::Error>> {
    println!("  C) wall_clock_timeout kills long-running subprocess");
    let tmp_audit = tempfile::tempdir()?;
    let (writer, _sink) = build_writer(tmp_audit.path(), "smoke-cli-c")?;

    let fake = fake_cli("claude", "#!/bin/sh\ncat > /dev/null\nexec sleep 10\n")?;

    let mut spec = cli_agent_loop_spec(None);
    spec.wall_clock_timeout_seconds = 2;
    let host = ScriptHost::new(fake.cli_path());
    let started = Instant::now();
    let _ = dispatch_v2_activity(V2DispatchInput {
        activity_name: "cli_smoke_c",
        spec: &ActivityV2Spec::AgentLoop(spec),
        fs_profile: None,
        input: serde_json::json!({ "prompt": "ignored" }),
        audit: writer.clone(),
        run_id: "smoke-cli-c",
        host: Some(&host),
    })?;
    let elapsed = started.elapsed();
    // 5s, matching this scenario's documented contract above. The point of the
    // bound is that the 2s timeout kills the `sleep 10` rather than waiting it
    // out, so anything well under 10s proves it. The previous 3s left only ~1s
    // of slack for subprocess teardown and flaked on loaded CI runners.
    assert!(
        elapsed < Duration::from_secs(5),
        "AC #7: timeout did not kill subprocess within 5s (elapsed {:?})",
        elapsed
    );

    let events = writer.events_snapshot()?;
    let finished = events
        .iter()
        .find(|e| e.envelope.event_type == "cli.invocation.finished")
        .ok_or("missing cli.invocation.finished")?;
    let body = serde_json::to_value(&finished.kind)?;
    assert_eq!(
        body.get("timed_out"),
        Some(&Value::Bool(true)),
        "expected timed_out=true, got {body:?}"
    );
    println!("    timed_out=true (elapsed {:?})", elapsed);
    Ok(())
}

/// E: [ORB-10801] a `session:` binding is refused at load time, so a run
/// never starts a DAG whose cross-iteration semantics no longer exist.
fn scenario_e_loader_rejection_retired_session() -> Result<(), Box<dyn std::error::Error>> {
    println!("  E) loader rejects a retired `session:` binding");
    let job = synthetic_loop_session_job();
    let err = validate_job_retired_sessions(&job, "synthetic/loop_session.yaml")
        .expect_err("expected rejection");
    let RetiredFeatureError::SessionBinding {
        asset_path,
        step_id,
        session_name,
    } = &err;
    assert_eq!(asset_path, "synthetic/loop_session.yaml");
    assert_eq!(step_id, "assess");
    assert_eq!(session_name, "assessor");
    assert!(
        err.to_string().contains("CLI agent path"),
        "rejection must carry the migration: {err}"
    );
    Ok(())
}

/// F: [ORB-10801] the removed `backend:` values are refused at parse time, and
/// never quietly remapped onto the surviving CLI agent path.
fn scenario_f_loader_rejection_retired_backend_value() -> Result<(), Box<dyn std::error::Error>> {
    println!("  F) parser rejects retired backend values");
    for removed in ["http", "auto"] {
        let yaml = format!(
            "schemaVersion: 2\nkind: Activity\nmetadata:\n  name: retired\nspec:\n  type: agent_loop\n  description: retired\n  instruction: hi\n  backend: {removed}\n"
        );
        let err = load_activity_asset(&yaml).expect_err("removed backend must fail closed");
        assert!(
            err.to_string()
                .contains(&format!("`backend: {removed}` is no longer supported")),
            "unexpected message: {err}"
        );
    }
    // `cli` named the surviving path, so it stays accepted and inert.
    let yaml = "schemaVersion: 2\nkind: Activity\nmetadata:\n  name: retained\nspec:\n  type: agent_loop\n  description: retained\n  instruction: hi\n  backend: cli\n";
    load_activity_asset(yaml).expect("`backend: cli` must keep loading");
    Ok(())
}

/// H: load `agent_loop_cli_reference.yaml` from disk and execute it
/// end-to-end through the CLI runner using a substitutable `claude` script.
/// Proves the YAML parses with the new `backend:` / `provider:` /
/// `wall_clock_timeout_seconds:` fields and routes correctly to the CLI
/// runner.
fn scenario_h_cli_reference_asset_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    println!("  H) agent_loop_cli_reference.yaml round-trips + dispatches");
    let repo_root = repo_root();
    let path = repo_root
        .join("crates/orbit-core/assets/activities/examples/agent_loop_cli_reference.yaml");
    let yaml = fs::read_to_string(&path)?;
    let asset = load_activity_asset(&yaml)?;
    match &asset.spec.spec {
        ActivityV2Spec::AgentLoop(spec) => {
            assert_eq!(spec.provider, Provider::Claude);
            assert_eq!(spec.wall_clock_timeout_seconds, 30);
        }
        other => panic!("expected agent_loop spec, got {other:?}"),
    }

    let tmp_audit = tempfile::tempdir()?;
    let (writer, _sink) = build_writer(tmp_audit.path(), "smoke-cli-h")?;
    // [ORB-10449] A round-tripped asset gets the default completion contract,
    // so the fake must terminate with a real envelope like a live provider.
    let fake = fake_cli(
        "claude",
        "#!/bin/sh\ncat > /dev/null\necho '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
    )?;
    let host = ScriptHost::new(fake.cli_path());
    let outcome = dispatch_v2_activity(V2DispatchInput {
        activity_name: &asset.name,
        spec: &asset.spec.spec,
        fs_profile: asset.spec.fs_profile.as_deref(),
        input: serde_json::json!({ "prompt": "hello from the yaml round-trip" }),
        audit: writer.clone(),
        run_id: "smoke-cli-h",
        host: Some(&host),
    })?;
    assert!(outcome.success);
    println!(
        "    asset dispatched, events={}",
        writer.events_snapshot()?.len()
    );
    Ok(())
}

/// I: existing Phase 3 v2 `agent_loop` YAML assets still deserialize with the
/// new `AgentLoopSpec` fields (serde defaults). Covers the default CLI
/// + the loop/denial samples under `jobs/v2_samples/`.
fn scenario_i_existing_agent_loop_assets_still_deserialize()
-> Result<(), Box<dyn std::error::Error>> {
    println!("  I) existing v2 agent_loop assets deserialize unchanged");
    let repo_root = repo_root();
    let asset_path =
        repo_root.join("crates/orbit-core/assets/activities/examples/agent_loop_reference.yaml");
    let yaml = fs::read_to_string(&asset_path)?;
    let asset = load_activity_asset(&yaml)?;
    if let ActivityV2Spec::AgentLoop(spec) = &asset.spec.spec {
        assert_eq!(spec.provider, Provider::Claude);
        assert!(spec.wall_clock_timeout_seconds > 0);
    } else {
        panic!("expected agent_loop");
    }
    println!("    {}: provider=claude (default)", asset.name);
    Ok(())
}

/// J: static args from the resolved executor are prepended before per-provider
/// runtime args and appear in the persisted invocation argv.
fn scenario_j_cli_executor_static_args_are_audited() -> Result<(), Box<dyn std::error::Error>> {
    println!("  J) cli executor static args are included in audited argv");
    let tmp_audit = tempfile::tempdir()?;
    let (writer, _sink) = build_writer(tmp_audit.path(), "smoke-cli-j")?;

    // [ORB-10449] Terminate with a real envelope; this scenario is about argv,
    // not about the step-completion protocol.
    let fake = fake_cli(
        "codex",
        "#!/bin/sh\ncat > /dev/null\necho '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
    )?;

    let mut spec = cli_agent_loop_spec(Some(Provider::Codex));
    spec.model = None;
    let host = ScriptHost::new_with_args(fake.cli_path(), vec!["exec".into(), "--json".into()]);

    let outcome = dispatch_v2_activity(V2DispatchInput {
        activity_name: "cli_smoke_j",
        spec: &ActivityV2Spec::AgentLoop(spec),
        fs_profile: None,
        input: serde_json::json!({ "prompt": "hello" }),
        audit: writer.clone(),
        run_id: "smoke-cli-j",
        host: Some(&host),
    })?;

    assert!(outcome.success, "fake codex should exit 0");

    let events = writer.events_snapshot()?;
    let argv = events
        .iter()
        .find_map(|event| match &event.kind {
            orbit_types::workflow::activity_job::V2AuditEventKind::CliInvocationStarted {
                argv_redacted,
                ..
            } => Some(argv_redacted),
            _ => None,
        })
        .expect("cli.invocation.started event");

    assert_eq!(
        argv,
        &vec![
            fake.cli_path().display().to_string(),
            "exec".to_string(),
            "--json".to_string(),
            "--sandbox".to_string(),
            "workspace-write".to_string(),
        ]
    );
    Ok(())
}

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn cli_agent_loop_spec(provider: Option<Provider>) -> AgentLoopSpec {
    AgentLoopSpec {
        instruction: "cli smoke".to_string(),
        tools: vec!["orbit.task.show".to_string(), "proc.spawn".to_string()],
        on_denial: OnDenial::Terminate,
        model: Some(orbit_common::model_defaults::CLAUDE_DEFAULT_STRONG.to_string()),
        reasoning_effort: None,
        max_iterations: 1,
        backend: None,
        provider: provider.unwrap_or(Provider::Claude),
        wall_clock_timeout_seconds: 30,
        require_response_envelope: false,
        require_completion_envelope: true,
        proc_allowed_programs: None,
        trusted_host_execution: false,
    }
}

fn must_contain(types: &[&str], needle: &str) {
    assert!(
        types.contains(&needle),
        "expected `{}` in event types, got {:?}",
        needle,
        types
    );
}

fn build_writer(
    root: &Path,
    run_id: &str,
) -> Result<(Arc<V2AuditWriter>, ()), Box<dyn std::error::Error>> {
    let audit_root = root.join("audit");
    fs::create_dir_all(&audit_root)?;
    let writer = V2AuditWriter::with_disk_sinks(
        &audit_root,
        Arc::new(orbit_store::Store::open_in_memory()?),
        "ws_smoke",
        run_id,
        "smoke".to_string(),
        None,
    )?;
    Ok((writer, ()))
}

/// Write a shell-script "fake CLI" into a tempdir using the chosen basename
/// so `AgentConfig::from_cli_config` resolves it to the matching factory.
/// The struct retains ownership of the `TempDir` so the file lives for the
/// whole scenario.
struct FakeCli {
    _tempdir: TempDir,
    path: PathBuf,
}
impl FakeCli {
    fn cli_path(&self) -> &Path {
        &self.path
    }
}

fn fake_cli(basename: &str, body: &str) -> Result<FakeCli, Box<dyn std::error::Error>> {
    let tempdir = tempfile::tempdir()?;
    let path = tempdir.path().join(basename);
    {
        let mut f = fs::File::create(&path)?;
        f.write_all(body.as_bytes())?;
    }
    let mut perms = fs::metadata(&path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms)?;
    Ok(FakeCli {
        _tempdir: tempdir,
        path,
    })
}

fn synthetic_loop_session_job() -> JobV2 {
    let assess_step = JobV2Step {
        id: "assess".to_string(),
        when: None,
        retry: None,
        recovery_activity: None,
        resolved_recovery_activity: None,
        body: JobV2StepBody::Target(TargetStep {
            spec: ActivityV2Spec::AgentLoop(AgentLoopSpec {
                instruction: String::new(),
                tools: vec![],
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
            }),
            activity_name: None,
            input_schema_json: None,
            fs_profile: None,
            default_input: None,
            timeout_seconds: 0,
            session: Some("assessor".to_string()),
        }),
    };
    let loop_step = JobV2Step {
        id: "assess_fix".to_string(),
        when: None,
        retry: None,
        recovery_activity: None,
        resolved_recovery_activity: None,
        body: JobV2StepBody::Loop {
            loop_: LoopBlock {
                items: None,
                max_iterations: 3,
                break_when: None,
                steps: vec![assess_step],
            },
        },
    };
    JobV2 {
        state: JobScheduleState::Enabled,
        default_input: None,
        recovery_activity: None,
        resolved_recovery_activity: None,
        failure_activity: None,
        resolved_failure_activity: None,
        max_active_runs: 1,
        kind: JobKind::Workflow,
        steps: vec![loop_step],
    }
}

// Hosts --------------------------------------------------------------------

struct ScriptHost {
    command: String,
    args: Vec<String>,
}
impl ScriptHost {
    fn new(path: &Path) -> Self {
        Self::new_with_args(path, Vec::new())
    }

    fn new_with_args(path: &Path, args: Vec<String>) -> Self {
        Self {
            command: path.to_string_lossy().into_owned(),
            args,
        }
    }
}
impl RuntimeHost for ScriptHost {
    fn run_deterministic(
        &self,
        _action: &str,
        _config: &Value,
        _input: &Value,
        _tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        Err(DispatchError::DeterministicActionNotRegistered(
            "unused".to_string(),
        ))
    }
    fn resolve_cli_executor(&self, _provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        Ok(ResolvedCliExecutor {
            command: self.command.clone(),
            args: self.args.clone(),
        })
    }

    fn tool_context_for_activity(
        &self,
        _run_id: Option<&str>,
        _fs_profile: Option<&str>,
        _fs_audit: Option<std::sync::Arc<dyn orbit_tools::FsAuditLogger>>,
        _proc_allowed_programs: Option<&[String]>,
    ) -> orbit_tools::ToolContext {
        orbit_tools::ToolContext::default()
    }
}
