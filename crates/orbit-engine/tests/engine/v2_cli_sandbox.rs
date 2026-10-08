//! Sandboxed CLI dispatch through the public boundaries: missing trusted
//! macOS wrapper admission (runs on Linux as well, without changing PATH or
//! removing a host executable), and the activity identity a sandboxed
//! pipeline step hands its plugin broker and tool policy.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use orbit_common::OrbitError;
use orbit_engine::activity_job::{V2ActivityCatalog, load_job_asset};
use orbit_engine::{
    DispatchError, PluginBrokerHandle, PluginBrokerRun, ResolvedActivityTools, ResolvedCliExecutor,
    ResolvedSandbox, RuntimeHost, V2AuditWriter, V2DispatchInput, dispatch_v2_activity,
    execute_job_with_resume, resolve_job_catalog_refs_for_execution,
};
use orbit_store::Store;
use orbit_types::policy::ResolvedFsProfile;
use orbit_types::workflow::ExecutorSandboxKind;
use orbit_types::workflow::activity_job::{ActivityV2, ActivityV2Spec, V2AuditEventKind};
use serde_json::Value;

/// A pipeline step's id is not its activity: `task_claimed_pr_pipeline` step
/// `review` targets `agent_review_repair`. Policy, `ORBIT_ACTIVITY_NAME`, tool
/// denials and the plugin broker are keyed by the catalog name; events keep
/// the step id. When the step id leaked through, the claimed review bridge
/// refused every Mac pull reviewer (on-call 2026-10-06).
///
/// The broker is started only for a sandboxed provider, so its half of the
/// check needs the platform sandbox; the policy half runs everywhere.
#[test]
fn pipeline_step_dispatches_its_catalog_activity_to_policy_and_broker() {
    let kind = platform_sandbox();
    // Bubblewrap mounts a private tmpfs over /tmp, so the provider's results
    // must live on disk elsewhere.
    let dir = match kind {
        Some(_) => tempfile::Builder::new()
            .prefix("ostep")
            .tempdir_in("/var/tmp")
            .unwrap(),
        None => tempfile::tempdir().unwrap(),
    };
    let root = dir.path().canonicalize().unwrap();
    let results = root.join("results");
    fs::create_dir(&results).unwrap();
    let command = root.join("codex");
    fs::write(
        &command,
        format!(
            "#!/bin/sh\n\
             printf '%s' \"${{ORBIT_ACTIVITY_NAME-}}\" > '{results}/activity'\n\
             cat > /dev/null\n\
             printf '%s\\n' '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}'\n",
            results = results.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&command, fs::Permissions::from_mode(0o755)).unwrap();

    let mut job = load_job_asset(
        &serde_json::json!({
            "schemaVersion": 2, "kind": "Job", "metadata": {"name": "claimed_review_shape"},
            "spec": {"state": "enabled", "kind": "workflow", "steps": [
                {"id": "review", "target": "activity:agent_review_repair"}
            ]}
        })
        .to_string(),
    )
    .unwrap()
    .spec;
    let mut catalog = V2ActivityCatalog::new();
    catalog.insert(
        "agent_review_repair",
        ActivityV2 {
            description: String::new(),
            input_schema_json: Value::Null,
            output_schema_json: Value::Null,
            fs_profile: None,
            spec: ActivityV2Spec::AgentLoop(
                serde_json::from_value(serde_json::json!({
                    "instruction": "review fixture",
                    "provider": "codex",
                    "tool_disallow_list": ["proc.*"],
                    "wall_clock_timeout_seconds": 30
                }))
                .unwrap(),
            ),
        },
    );
    resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();

    let host = RecordingHost {
        command,
        worktree: root.clone(),
        sandbox: kind.map(|kind| ResolvedSandbox {
            kind,
            fs_profile: ResolvedFsProfile {
                name: "step-identity".into(),
                read: vec!["/**".into()],
                modify: vec![format!("{}/**", results.display())],
            },
            allow_fallback: false,
            managed_worktree: false,
            runtime_write_authority: Vec::new(),
            mask: None,
        }),
        broker_runs: Mutex::new(Vec::new()),
        denial_activities: Mutex::new(Vec::new()),
    };
    let audit = V2AuditWriter::with_disk_sinks(
        &root.join("audit"),
        Arc::new(Store::open_in_memory().unwrap()),
        "ws-sandbox",
        "step-identity",
        "codex".to_string(),
        None,
    )
    .unwrap();
    let outcome = execute_job_with_resume(
        &job,
        serde_json::json!({"prompt": "review"}),
        "step-identity",
        audit.clone(),
        &host,
        None,
    )
    .unwrap();
    assert!(outcome.success, "{outcome:?}");

    let broker_runs = host.broker_runs.lock().unwrap();
    if kind.is_some() {
        assert_eq!(broker_runs.len(), 1, "the sandboxed step starts one broker");
        assert_eq!(
            broker_runs[0].activity_name, "agent_review_repair",
            "the broker authorizes the catalog activity, which the claimed review bridge requires"
        );
        assert_eq!(
            broker_runs[0]
                .tool_deny_policy
                .as_ref()
                .map(|policy| policy.activity.as_str()),
            Some("agent_review_repair")
        );
    } else {
        assert!(broker_runs.is_empty(), "an unsandboxed step has no broker");
    }
    assert_eq!(
        *host.denial_activities.lock().unwrap(),
        ["agent_review_repair"],
        "tool denials resolve for the catalog activity"
    );
    assert_eq!(
        fs::read_to_string(results.join("activity")).unwrap(),
        "agent_review_repair",
        "the provider's ORBIT_ACTIVITY_NAME names the catalog activity"
    );
    let started = audit
        .events_snapshot()
        .unwrap()
        .into_iter()
        .filter_map(|event| match event.kind {
            V2AuditEventKind::ActivityStarted { activity_name, .. } => Some(activity_name),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(started, ["review"], "activity events keep the step id");
}

/// The agent sandbox this host can apply, or `None` after naming why the
/// broker half is skipped (no Bubblewrap user namespaces, e.g. inside a
/// sandbox).
fn platform_sandbox() -> Option<ExecutorSandboxKind> {
    #[cfg(target_os = "linux")]
    let available = {
        let probe = orbit_exec::probe_bwrap();
        (probe.available)
            .then_some(ExecutorSandboxKind::LinuxBwrap)
            .ok_or(probe.detail)
    };
    #[cfg(not(target_os = "linux"))]
    let available = orbit_exec::sandbox_exec_available()
        .then_some(ExecutorSandboxKind::MacosSandboxExec)
        .ok_or_else(|| "sandbox-exec is unavailable".to_string());
    available
        .inspect_err(|reason| {
            let _ = writeln!(
                std::io::stderr(),
                "skipped the plugin broker check: the agent sandbox is unavailable: {reason}"
            );
        })
        .ok()
}

struct RecordingHost {
    command: PathBuf,
    worktree: PathBuf,
    sandbox: Option<ResolvedSandbox>,
    broker_runs: Mutex<Vec<PluginBrokerRun>>,
    denial_activities: Mutex<Vec<String>>,
}

impl RuntimeHost for RecordingHost {
    fn run_deterministic(
        &self,
        _action: &str,
        _config: &Value,
        _input: &Value,
        _tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        Err(DispatchError::DeterministicActionNotRegistered(
            "unused".into(),
        ))
    }

    fn resolve_cli_executor(&self, _provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        Ok(ResolvedCliExecutor {
            command: self.command.display().to_string(),
            args: Vec::new(),
        })
    }

    fn resolve_executor_sandbox(
        &self,
        _provider: &str,
        _fs_profile: Option<&str>,
        _cwd: Option<&Path>,
    ) -> Result<Option<ResolvedSandbox>, DispatchError> {
        Ok(self.sandbox.clone())
    }

    fn resolve_activity_tool_denials(
        &self,
        _task_ids: &[String],
        activity: &str,
        _disallow_list: &[String],
    ) -> Result<ResolvedActivityTools, DispatchError> {
        self.denial_activities
            .lock()
            .unwrap()
            .push(activity.to_string());
        Ok(ResolvedActivityTools {
            requested_tools: Vec::new(),
            effective_tools: Vec::new(),
        })
    }

    fn tool_context_for_activity(
        &self,
        _run_id: Option<&str>,
        _fs_profile: Option<&str>,
        _fs_audit: Option<Arc<dyn orbit_tools::FsAuditLogger>>,
        _proc_allowed_programs: Option<&[String]>,
    ) -> orbit_tools::ToolContext {
        orbit_tools::ToolContext {
            workspace_root: Some(self.worktree.clone()),
            ..Default::default()
        }
    }

    fn start_plugin_broker(
        &self,
        run: &PluginBrokerRun,
    ) -> Result<Option<Box<dyn PluginBrokerHandle>>, OrbitError> {
        self.broker_runs.lock().unwrap().push(run.clone());
        Ok(None)
    }
}

#[test]
fn missing_macos_wrapper_preserves_provider_confinement_and_honest_audit() {
    if orbit_exec::sandbox_exec_available() {
        // This regression exercises a missing trusted wrapper, not the macOS
        // kernel's enforced path. Never remove a host binary to force it.
        return;
    }

    for allow_fallback in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let command = temp.path().join("codex");
        fs::write(
            &command,
            "#!/bin/sh\n\
             printf '%s\\n' \"$@\" > \"$0.args\"\n\
             printf '%s' \"${ORBIT_PLUGIN_BROKER-}\" > \"$0.broker\"\n\
             cat > /dev/null\n\
             printf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
        )
        .unwrap();
        fs::set_permissions(&command, fs::Permissions::from_mode(0o755)).unwrap();
        let host = MissingWrapperHost {
            command: command.clone(),
            allow_fallback,
            broker_starts: AtomicUsize::new(0),
        };
        let audit = V2AuditWriter::with_disk_sinks(
            &temp.path().join("audit"),
            Arc::new(Store::open_in_memory().unwrap()),
            "ws-sandbox",
            "missing-wrapper",
            "codex".to_string(),
            None,
        )
        .unwrap();
        let spec = ActivityV2Spec::AgentLoop(
            serde_json::from_value(serde_json::json!({
                "instruction": "sandbox admission fixture",
                "provider": "codex",
                "tools": [],
                "wall_clock_timeout_seconds": 10
            }))
            .unwrap(),
        );
        let outcome = dispatch_v2_activity(V2DispatchInput {
            activity_name: "sandbox_fixture",
            spec: &spec,
            fs_profile: None,
            input: serde_json::json!({"prompt": "check admission"}),
            audit: audit.clone(),
            run_id: "missing-wrapper",
            host: Some(&host),
        });
        assert_eq!(
            host.broker_starts.load(Ordering::SeqCst),
            0,
            "an unavailable outer wrapper must never authorize a plugin broker"
        );
        let events = audit.events_snapshot().unwrap();
        let started = events.iter().find_map(|event| match &event.kind {
            V2AuditEventKind::CliInvocationStarted {
                argv_redacted,
                sandbox_backend,
                sandbox_write_enforcement,
                sandbox_read_enforcement,
                ..
            } => Some((
                argv_redacted,
                sandbox_backend,
                sandbox_write_enforcement,
                sandbox_read_enforcement,
            )),
            _ => None,
        });
        if allow_fallback {
            assert!(outcome.unwrap().success);
            let (argv, backend, writes, reads) = started.expect("bare invocation is audited");
            assert_eq!(backend.as_deref(), Some("bare-fallback"));
            assert_eq!(writes.as_deref(), Some("write_delegated"));
            assert_eq!(reads.as_deref(), Some("read_delegated"));
            let actual_args = fs::read_to_string(temp.path().join("codex.args")).unwrap();
            let actual_argv = std::iter::once(command.to_str().unwrap())
                .chain(actual_args.lines())
                .collect::<Vec<_>>();
            assert_eq!(
                argv, &actual_argv,
                "audit must describe the bare process argv"
            );
            assert!(
                argv.windows(2)
                    .any(|pair| pair == ["--sandbox", "read-only"]),
                "bare fallback must retain the provider's native write confinement"
            );
            assert_eq!(
                fs::read_to_string(temp.path().join("codex.broker")).unwrap(),
                "",
                "bare fallback must receive no plugin broker capability"
            );
        } else {
            assert!(matches!(
                outcome,
                Err(DispatchError::CliInvocationPermanent(_))
            ));
            assert!(
                started.is_none(),
                "refused admission must not audit a launch"
            );
            assert!(
                !temp.path().join("codex.args").exists(),
                "refused admission must not run the provider"
            );
        }
    }
}

struct MissingWrapperHost {
    command: PathBuf,
    allow_fallback: bool,
    broker_starts: AtomicUsize,
}

impl RuntimeHost for MissingWrapperHost {
    fn run_deterministic(
        &self,
        _action: &str,
        _config: &Value,
        _input: &Value,
        _tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        Err(DispatchError::DeterministicActionNotRegistered(
            "unused".into(),
        ))
    }

    fn resolve_cli_executor(&self, _provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        Ok(ResolvedCliExecutor {
            command: self.command.display().to_string(),
            args: Vec::new(),
        })
    }

    fn provider_cli_config(&self, _provider: &str) -> HashMap<String, String> {
        HashMap::from([("sandbox".into(), "read-only".into())])
    }

    fn resolve_executor_sandbox(
        &self,
        _provider: &str,
        _fs_profile: Option<&str>,
        _cwd: Option<&Path>,
    ) -> Result<Option<ResolvedSandbox>, DispatchError> {
        Ok(Some(ResolvedSandbox {
            kind: ExecutorSandboxKind::MacosSandboxExec,
            fs_profile: ResolvedFsProfile {
                name: "fixture".into(),
                read: vec!["**".into()],
                modify: vec![format!("{}/**", self.command.parent().unwrap().display())],
            },
            allow_fallback: self.allow_fallback,
            managed_worktree: false,
            runtime_write_authority: Vec::new(),
            mask: None,
        }))
    }

    fn tool_context_for_activity(
        &self,
        _run_id: Option<&str>,
        _fs_profile: Option<&str>,
        _fs_audit: Option<Arc<dyn orbit_tools::FsAuditLogger>>,
        _proc_allowed_programs: Option<&[String]>,
    ) -> orbit_tools::ToolContext {
        orbit_tools::ToolContext {
            workspace_root: Some(self.command.parent().unwrap().to_path_buf()),
            ..Default::default()
        }
    }

    fn start_plugin_broker(
        &self,
        _run: &PluginBrokerRun,
    ) -> Result<Option<Box<dyn PluginBrokerHandle>>, OrbitError> {
        self.broker_starts.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }
}

/// Recovery calls enter the same actual sandbox as provider leaves; a marker
/// cannot grant success when the exit is nonzero or a protected read succeeds.
#[test]
fn auth_probe_obeys_leaf_sandbox_and_requires_a_successful_model_response() {
    use orbit_engine::activity_job::cli_runner::run_auth_probe;
    use orbit_types::workflow::{AuthProbe, AuthProbeSuccess};
    let kind = platform_sandbox();
    let scratch = orbit_common::fs::path::ensure_orbit_scratch_dir(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
    )
    .unwrap();
    let dir = tempfile::Builder::new()
        .prefix("auth-probe-")
        .tempdir_in(scratch)
        .unwrap();
    let root = dir.path().canonicalize().unwrap();
    let secret = root.join("secret");
    fs::write(&secret, "protected fixture data").unwrap();
    let command = root.join("claude");
    fs::write(&command, format!(
        "#!/bin/sh\ncat > /dev/null\nif [ \"$1\" = sandbox ]; then\n  cat '{}' 2>/dev/null && exit 23\nfi\nprintf '%s\\n' \"$2\"\nexit \"$3\"\n", secret.display(),
    )).unwrap();
    fs::set_permissions(&command, fs::Permissions::from_mode(0o755)).unwrap();
    let mut host = RecordingHost {
        command,
        worktree: root.clone(),
        sandbox: kind.map(|kind| ResolvedSandbox {
            kind,
            fs_profile: ResolvedFsProfile {
                name: "auth-fixture".into(),
                read: vec!["**".into(), format!("!{}", secret.display())],
                modify: vec![format!("{}/**", root.display())],
            },
            allow_fallback: false,
            managed_worktree: false,
            runtime_write_authority: vec![],
            mask: None,
        }),
        broker_runs: Mutex::new(vec![]),
        denial_activities: Mutex::new(vec![]),
    };
    let mut probe = AuthProbe {
        args: vec!["sandbox".into(), "ORBIT_AUTH_OK".into(), "0".into()],
        stdin: "Minimal model call".into(),
        timeout_seconds: 5,
        success: AuthProbeSuccess::StdoutContains {
            text: "ORBIT_AUTH_OK".into(),
        },
        relogin_hint: "Run `claude login`.".into(),
    };
    if kind.is_some() {
        assert!(
            run_auth_probe(&host, "claude", &probe, "auth-fixture", &root)
                .unwrap()
                .passed,
            "sandbox must deny the protected read while allowing the minimal invocation"
        );
    }
    host.sandbox = None;
    for (response, exit, passed) in [
        ("ORBIT_AUTH_OK", "0", true),
        ("unrelated response", "0", false),
        ("ORBIT_AUTH_OK", "1", false),
    ] {
        probe.args = vec!["plain".into(), response.into(), exit.into()];
        assert_eq!(
            run_auth_probe(&host, "claude", &probe, "auth-fixture", &root)
                .unwrap()
                .passed,
            passed
        );
    }
    assert!(
        host.broker_runs.lock().unwrap().is_empty(),
        "auth probes grant no activity broker"
    );
}
