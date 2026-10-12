//! Sandboxed CLI dispatch through the public boundaries: missing trusted
//! macOS wrapper admission (runs on Linux as well, without changing PATH or
//! removing a host executable), the activity identity a sandboxed pipeline
//! step hands its plugin broker and tool policy, and the deferral probe Orbit
//! runs in an implementer's lane.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
use orbit_types::workflow::activity_job::{
    ActivityV2, ActivityV2Spec, AgentLoopSpec, V2AuditEventKind,
};
use serde_json::{Value, json};

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
        refuse_registration_after: None,
        panic_on_registration: false,
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
    {
        let probe = orbit_exec::probe_bwrap();
        if !probe.available {
            orbit_exec::report_bwrap_deferral("v2 CLI plugin broker check", &probe.detail);
            return None;
        }
        Some(ExecutorSandboxKind::LinuxBwrap)
    }
    #[cfg(not(target_os = "linux"))]
    {
        orbit_exec::macos_sandbox_test_guard("v2 CLI plugin broker check")
            .then_some(ExecutorSandboxKind::MacosSandboxExec)
    }
}

struct RecordingHost {
    command: PathBuf,
    worktree: PathBuf,
    sandbox: Option<ResolvedSandbox>,
    broker_runs: Mutex<Vec<PluginBrokerRun>>,
    denial_activities: Mutex<Vec<String>>,
    /// Refuses worker registration once the provider has published its
    /// grandchild's pid to this file, so the refusal lands while it is alive.
    refuse_registration_after: Option<PathBuf>,
    panic_on_registration: bool,
}

impl orbit_engine::test_support::NullRuntimeHost for RecordingHost {
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
            omitted_requirement_notes: Vec::new(),
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

    fn register_worker_process(&self, _pid: u32) -> Result<(), OrbitError> {
        let Some(pid_file) = &self.refuse_registration_after else {
            return Ok(());
        };
        assert!(wait_until(Duration::from_secs(10), || pid_file.exists()));
        assert!(!self.panic_on_registration, "worker registration panicked");
        Err(OrbitError::Execution("worker registration refused".into()))
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

impl orbit_engine::test_support::NullRuntimeHost for MissingWrapperHost {
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
/// cannot grant success when the exit is nonzero or a write the profile's
/// `modify` rules deny lands. The confinement is proven with a denied write
/// because Linux Bubblewrap confines writes only: it never compiles `read`
/// denies, so a protected read passed under `sandbox-exec` and failed on every
/// host with working Bubblewrap [ORB-15372]. The denied subtree sits inside
/// the probe's own root, so it stays denied wherever the checkout lives: a
/// `modify` deny follows the host scratch-root allows in the SBPL profile
/// (last match wins) and becomes a read-only bind under Bubblewrap.
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
    let protected = root.join("protected");
    fs::create_dir(&protected).unwrap();
    let escape = protected.join("escape");
    let command = root.join("claude");
    fs::write(&command, format!(
        "#!/bin/sh\ncat > /dev/null\nif [ \"$1\" = sandbox ]; then\n  touch '{}' 2>/dev/null && exit 23\nfi\nprintf '%s\\n' \"$2\"\nexit \"$3\"\n", escape.display(),
    )).unwrap();
    fs::set_permissions(&command, fs::Permissions::from_mode(0o755)).unwrap();
    let mut host = RecordingHost {
        command,
        worktree: root.clone(),
        sandbox: kind.map(|kind| ResolvedSandbox {
            kind,
            fs_profile: ResolvedFsProfile {
                name: "auth-fixture".into(),
                read: vec!["**".into()],
                modify: vec![
                    format!("{}/**", root.display()),
                    format!("!{}/**", protected.display()),
                ],
            },
            allow_fallback: false,
            managed_worktree: false,
            runtime_write_authority: vec![],
            mask: None,
        }),
        broker_runs: Mutex::new(vec![]),
        denial_activities: Mutex::new(vec![]),
        refuse_registration_after: None,
        panic_on_registration: false,
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
            "sandbox must deny the protected write while allowing the minimal invocation"
        );
        assert!(
            !escape.exists(),
            "the sandboxed probe wrote into its denied subtree"
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

/// A provider that forks a grandchild and then has its registration refused.
/// The refused child and the grandchild share a process group, and the
/// grandchild is not in the worker registry, so only the group kill ends it.
#[test]
fn auth_probe_refused_registration_ends_the_grandchild() {
    use orbit_engine::activity_job::cli_runner::run_auth_probe;
    use orbit_types::workflow::{AuthProbe, AuthProbeSuccess};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let grandchild = Grandchild(root.join("grandchild.pid"));
    let command = root.join("claude");
    write_grandchild_provider(&command, &grandchild.0);
    let host = RecordingHost {
        command,
        worktree: root.clone(),
        sandbox: None,
        broker_runs: Mutex::new(Vec::new()),
        denial_activities: Mutex::new(Vec::new()),
        refuse_registration_after: Some(grandchild.0.clone()),
        panic_on_registration: false,
    };
    let probe = AuthProbe {
        args: vec!["grandchild".into()],
        stdin: "Minimal model call".into(),
        timeout_seconds: 5,
        success: AuthProbeSuccess::ExitZero,
        relogin_hint: "Run `claude login`.".into(),
    };

    let outcome = run_auth_probe(&host, "claude", &probe, "registration-refusal", &root);
    assert!(
        outcome.is_err(),
        "a refused registration must fail the probe"
    );
    grandchild.assert_ended();
}

/// The dispatch path has the same refusal: a provider step whose registration
/// is refused or panics must end the entire group its provider started.
#[test]
fn dispatch_registration_failure_ends_the_provider_group() {
    use orbit_engine::activity_job::cli_runner::run_cli_backend;

    for panic_on_registration in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let grandchild = Grandchild(root.join("grandchild.pid"));
        let command = root.join("codex");
        write_grandchild_provider(&command, &grandchild.0);
        let host = RecordingHost {
            command,
            worktree: root.clone(),
            sandbox: None,
            broker_runs: Mutex::new(Vec::new()),
            denial_activities: Mutex::new(Vec::new()),
            refuse_registration_after: Some(grandchild.0.clone()),
            panic_on_registration,
        };
        let spec: AgentLoopSpec = serde_json::from_value(json!({
            "instruction": "review fixture",
            "provider": "codex",
            "tool_disallow_list": ["proc.*"],
            "wall_clock_timeout_seconds": 30
        }))
        .unwrap();
        let audit = V2AuditWriter::with_disk_sinks(
            &root.join("audit"),
            Arc::new(Store::open_in_memory().unwrap()),
            "ws-registration",
            "registration-refusal",
            "codex".to_string(),
            None,
        )
        .unwrap();

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_cli_backend(
                &host,
                &spec,
                "agent_review_repair",
                "registration-refusal",
                audit,
                &json!({"prompt": "review"}),
                None,
            )
        }));
        if panic_on_registration {
            assert!(
                outcome.is_err(),
                "the registration panic must unwind dispatch"
            );
        } else {
            assert!(
                matches!(outcome.unwrap(), Err(DispatchError::CliInvocationPermanent(message))
                    if message.contains("worker registration refused")),
                "a refused registration must fail the dispatch permanently"
            );
        }
        grandchild.assert_ended();
    }
}

/// Writes a provider stand-in that starts a sleeping grandchild, publishes
/// the grandchild's pid at `pid_file`, and then waits on it.
fn write_grandchild_provider(command: &Path, pid_file: &Path) {
    let staging = pid_file.with_extension("staging");
    fs::write(
        command,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" > '{leader_file}'\nsleep 300 &\nprintf '%s\\n' \"$!\" > '{staging}' && mv '{staging}' '{pid_file}'\nwait\n",
            leader_file = pid_file.with_extension("leader").display(),
            staging = staging.display(),
            pid_file = pid_file.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(command, fs::Permissions::from_mode(0o755)).unwrap();
}

/// The grandchild a fake provider published. Dropping it kills the grandchild
/// if an assertion failed before the test saw it end, so no sleep outlives the
/// test.
struct Grandchild(PathBuf);

impl Grandchild {
    fn assert_ended(&self) {
        let leader: u32 = fs::read_to_string(self.0.with_extension("leader"))
            .expect("provider leader pid")
            .trim()
            .parse()
            .unwrap();
        assert!(
            !process_is_running(leader),
            "provider leader survived registration failure"
        );
        assert_eq!(
            orbit_common::process::identity::probe_process_group_liveness(leader as i32),
            orbit_common::process::identity::KernelLiveness::Exited,
            "the provider group must have no live members after registration failure"
        );
        let text = fs::read_to_string(&self.0).expect("the provider publishes its grandchild");
        let pid: u32 = text.trim().parse().expect("grandchild pid");
        assert!(
            wait_until(Duration::from_secs(5), || !process_is_running(pid)),
            "grandchild {pid} outlived the refused worker registration"
        );
    }
}

impl Drop for Grandchild {
    fn drop(&mut self) {
        if let Some(leader) = fs::read_to_string(self.0.with_extension("leader"))
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
        {
            let _ = orbit_common::process::OwnedChild::signal_spawned_group(leader, libc::SIGKILL);
            // SAFETY: only reap this fixture's own provider; it may already have been reaped.
            unsafe { libc::waitpid(leader as libc::pid_t, std::ptr::null_mut(), 0) };
        }
        let Some(pid) = fs::read_to_string(&self.0)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
        else {
            return;
        };
        if process_is_running(pid) {
            // SAFETY: the pid was published by this test's own provider, and
            // signalling it only ends the sleep that provider started.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        }
    }
}

/// A zombie has ended but still answers `kill(pid, 0)` until its parent reaps
/// it, so `ps` decides whether the process still runs.
fn process_is_running(pid: u32) -> bool {
    let Ok(output) = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
    else {
        return true;
    };
    output.status.success()
        && !String::from_utf8_lossy(&output.stdout)
            .trim_start()
            .starts_with('Z')
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    condition()
}

/// A host whose implementer lane has no sandbox, or whose sandbox cannot be
/// resolved, for Orbit's own deferral probe [ORB-15516].
struct ProbeHost {
    unresolvable: bool,
}

impl orbit_engine::test_support::NullRuntimeHost for ProbeHost {
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

    fn resolve_executor_sandbox(
        &self,
        provider: &str,
        _fs_profile: Option<&str>,
        _cwd: Option<&Path>,
    ) -> Result<Option<ResolvedSandbox>, DispatchError> {
        if self.unresolvable {
            return Err(DispatchError::CliInvocationPermanent(format!(
                "no sandbox for `{provider}`: bwrap: No permissions to create new namespace"
            )));
        }
        Ok(None)
    }

    fn run_deferral_probe(
        &self,
        probe: &orbit_types::workflow::DeferralProbe,
        provider: Option<&str>,
        fs_profile: Option<&str>,
        cwd: &Path,
    ) -> orbit_types::workflow::NamespaceProbe {
        orbit_engine::run_deferral_probe(self, probe, provider, fs_profile, cwd)
    }
}

/// A host that has not opted into executing deferral probes.
struct UnsupportedProbeHost;

impl orbit_engine::test_support::NullRuntimeHost for UnsupportedProbeHost {
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
}

/// Orbit runs a deferral's probe itself, in the implementer's lane, and
/// records what the lane answered: its exit status and output. A probe that
/// cannot run, or whose lane cannot be resolved, records why with a nonzero
/// exit, which no deferral accepts [ORB-15516].
#[test]
fn the_deferral_probe_records_what_the_lane_answers() {
    let checkout = tempfile::tempdir().unwrap();
    let refusing = orbit_types::workflow::DeferralProbe {
        command: orbit_types::workflow::BUBBLEWRAP_NAMESPACE_PROBE,
        argv: &[
            "/bin/sh",
            "-c",
            "echo 'bwrap: No permissions to create new namespace' >&2; exit 1",
        ],
    };
    let unrunnable = orbit_types::workflow::DeferralProbe {
        command: "missing probe",
        argv: &["/nonexistent/orbit-probe"],
    };
    let bare = ProbeHost {
        unresolvable: false,
    };

    let refused = bare.run_deferral_probe(&refusing, Some("claude"), None, checkout.path());
    let unsupported =
        UnsupportedProbeHost.run_deferral_probe(&refusing, Some("claude"), None, checkout.path());
    let missing = bare.run_deferral_probe(&unrunnable, None, None, checkout.path());
    let unresolved = ProbeHost { unresolvable: true }.run_deferral_probe(
        &refusing,
        Some("claude"),
        None,
        checkout.path(),
    );

    assert_eq!(
        (
            refused.command.as_str(),
            refused.exit_code,
            refused.output.as_str()
        ),
        (
            orbit_types::workflow::BUBBLEWRAP_NAMESPACE_PROBE,
            1,
            "bwrap: No permissions to create new namespace"
        )
    );
    assert_ne!(missing.exit_code, 0);
    assert_eq!(
        unsupported.exit_code, -1,
        "an unsupported host never executes the probe"
    );
    assert!(
        missing.output.starts_with("the probe could not run"),
        "{missing:?}"
    );
    assert_ne!(unresolved.exit_code, 0);
    assert!(
        unresolved.output.contains("no sandbox for `claude`"),
        "the probe runs in the provider's lane: {unresolved:?}"
    );

    let handoff = orbit_types::workflow::deferral_handoff(&json!({
        "deferred_sandbox_validation": { "summary": ".orbit/tmp/gate.json" },
    }))
    .unwrap()
    .unwrap();
    let summary = serde_json::to_vec(&json!({
        "schema_version": 1,
        "command": "make ci-test-affected",
        "exit_code": 0,
        "tests_run": 1,
        "base": "pinned-base",
        "notices": ["DEFERRED: bubblewrap unavailable: fixture"],
    }))
    .unwrap();
    let gates =
        orbit_types::workflow::HostValidationGates::local(orbit_types::workflow::ValidationGates {
            required_validation_commands: vec!["make ci-test-affected".to_string()],
            baseline_commands: Vec::new(),
        });
    let pending = handoff
        .derive(
            &summary,
            &gates,
            Some("pinned-base"),
            orbit_types::workflow::DeferralLane {
                claimed: false,
                os: Some(orbit_types::workflow::EvidenceHostOs::Linux),
            },
        )
        .unwrap();
    pending.clone().complete(refused).unwrap();
    let refusal = pending.complete(unresolved).expect_err(
        "a lane-resolution error carrying the namespace-refusal text is not an executed probe",
    );
    assert!(refusal.contains("no failing namespace probe"), "{refusal}");
}
