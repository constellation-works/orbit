//! The broker through the real agent spawn path: `run_cli_backend` starts it,
//! spawns a fake provider under Bubblewrap (Linux) or `sandbox-exec` (macOS),
//! and tears it down when the provider exits. The provider runs this test
//! binary's `broker_client` inside the sandbox and records what the broker
//! answered.
//!
//! Each test skips, naming the reason on stderr, where the platform sandbox
//! cannot run (no Bubblewrap user namespaces, e.g. inside another sandbox).

use std::fs;
#[cfg(not(target_os = "linux"))]
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use orbit_engine::activity_job::cli_runner::run_cli_backend;
use orbit_engine::{
    DispatchError, DispatchOutcome, PLUGIN_BROKER_ENV, PluginBrokerHandle, PluginBrokerRun,
    ResolvedCliExecutor, ResolvedSandbox, RuntimeHost, V2AuditWriter,
};
use orbit_tools::{FsAuditLogger, ToolContext};
use orbit_types::policy::ResolvedFsProfile;
use orbit_types::workflow::ExecutorSandboxKind;
use orbit_types::workflow::activity_job::{AgentLoopSpec, OnDenial, Provider};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::super::PluginBroker;
use super::super::protocol::{MAX_REQUEST_BYTES, read_frame, write_frame};
use super::EchoDispatch;

const CLIENT_TEST: &str = "runtime::plugin::broker::tests::sandbox::broker_client";
const CLIENT_ENV: &str = "ORBIT_BROKER_TEST_CLIENT";
const RESULT_ENV: &str = "ORBIT_BROKER_TEST_RESULT";
const TARGET_ENV: &str = "ORBIT_BROKER_TEST_TARGET";
const HOLD_ENV: &str = "ORBIT_BROKER_TEST_HOLD";
const WAIT: Duration = Duration::from_secs(60);

/// The in-sandbox half, invoked by `client_provider` with an exact test filter.
#[test]
#[ignore = "client half of the sandboxed broker test; runs inside the agent sandbox"]
fn broker_client() {
    if std::env::var_os(CLIENT_ENV).is_none() {
        return;
    }
    let result = PathBuf::from(std::env::var_os(RESULT_ENV).expect("result path"));
    let partial = result.with_extension("partial");
    fs::write(&partial, client_outcome()).expect("write outcome");
    fs::rename(&partial, &result).expect("publish outcome");
    if let Some(hold) = std::env::var_os(HOLD_ENV) {
        // Keep this run, and so its broker, alive while the outer test
        // probes it from elsewhere.
        wait_for(Path::new(&hold));
    }
}

fn client_outcome() -> String {
    let Some(target) = std::env::var_os(TARGET_ENV).or_else(|| std::env::var_os(PLUGIN_BROKER_ENV))
    else {
        return "absent".to_string();
    };
    match UnixStream::connect(&target) {
        Ok(mut stream) => request_outcome(&mut stream),
        Err(error) => format!("connect failed: {error}"),
    }
}

fn request_outcome(stream: &mut UnixStream) -> String {
    let _ = stream.set_read_timeout(Some(WAIT));
    let request =
        json!({"schema_version": 1, "tool": "pulsar.post", "input": {}, "cwd": "/"}).to_string();
    // A refusing broker may close before the request is written; the read
    // below still tells the two outcomes apart.
    let _ = write_frame(stream, request.as_bytes());
    match read_frame(stream, MAX_REQUEST_BYTES) {
        Ok(body) => serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|reply| match reply["ok"].as_bool() {
                Some(true) => Some("ok".to_string()),
                _ => reply["error"]["code"].as_str().map(str::to_string),
            })
            .unwrap_or_else(|| "unreadable reply".to_string()),
        Err(_) => "closed".to_string(),
    }
}

fn wait_for(path: &Path) -> bool {
    let deadline = Instant::now() + WAIT;
    while !path.exists() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true
}

/// Which sandbox this platform can actually apply here, or why none can.
fn platform_sandbox() -> Result<ExecutorSandboxKind, String> {
    #[cfg(target_os = "linux")]
    {
        let probe = orbit_exec::probe_bwrap();
        if probe.available {
            Ok(ExecutorSandboxKind::LinuxBwrap)
        } else {
            Err(probe.detail)
        }
    }
    #[cfg(target_os = "macos")]
    {
        let applies = orbit_exec::sandbox_exec_path().is_some_and(|path| {
            std::process::Command::new(path)
                .args(["-p", "(version 1)\n(allow default)\n", "/usr/bin/true"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        });
        if applies {
            Ok(ExecutorSandboxKind::MacosSandboxExec)
        } else {
            Err("sandbox-exec cannot apply a profile on this host".to_string())
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err("no agent sandbox on this platform".to_string())
    }
}

/// A scratch tree for one test, or `None` after reporting why it is skipped.
fn scratch_or_skip(test: &str) -> Option<Scratch> {
    match platform_sandbox() {
        Ok(kind) => Some(Scratch::new(kind)),
        Err(reason) => {
            #[cfg(target_os = "linux")]
            orbit_exec::report_bwrap_deferral(test, &reason);
            #[cfg(not(target_os = "linux"))]
            let _ = writeln!(
                std::io::stderr(),
                "skipped {test}: the agent sandbox is unavailable: {reason}"
            );
            None
        }
    }
}

/// Paths the sandboxed provider reads and writes. The root is disk-backed
/// outside `/tmp`, which Bubblewrap replaces with a private tmpfs, and short
/// enough to keep the socket inside macOS's 104-byte `sun_path`.
struct Scratch {
    kind: ExecutorSandboxKind,
    root: PathBuf,
    _dir: TempDir,
}

impl Scratch {
    fn new(kind: ExecutorSandboxKind) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("obk")
            .tempdir_in("/var/tmp")
            .expect("scratch under /var/tmp");
        let root = dir.path().canonicalize().expect("canonical scratch");
        fs::create_dir(root.join("results")).expect("results dir");
        Self {
            kind,
            root,
            _dir: dir,
        }
    }

    fn result(&self, name: &str) -> PathBuf {
        self.root.join("results").join(name)
    }

    fn host(&self, global_root: PathBuf, provider: &str) -> BrokerHost {
        BrokerHost {
            global_root,
            worktree: self.root.clone(),
            command: self.root.join(provider),
            audit_root: self.root.join("audit"),
            sandbox: ResolvedSandbox {
                kind: self.kind,
                fs_profile: ResolvedFsProfile {
                    name: "broker-test".to_string(),
                    read: vec!["/**".to_string()],
                    modify: vec![format!("{}/**", self.root.join("results").display())],
                },
                allow_fallback: false,
                managed_worktree: false,
                runtime_write_authority: Vec::new(),
                mask: None,
            },
            sockets: Mutex::new(Vec::new()),
        }
    }

    /// Write a provider that runs [`broker_client`] and requires its result.
    fn client_provider(&self, name: &str, env: &[(&str, &Path)]) {
        orbit_common::test_env::assert_child_test_exists(CLIENT_TEST);
        let result = env
            .iter()
            .find_map(|(key, path)| (*key == RESULT_ENV).then_some(*path))
            .expect("broker client result sentinel");
        let mut assignments = format!("{CLIENT_ENV}=1");
        for (key, value) in env {
            assignments.push_str(&format!(" {key}={}", quote(value)));
        }
        let exe = std::env::current_exe().expect("test binary");
        self.provider(
            name,
            &format!(
                "set -e\ncat > /dev/null\n{assignments} {} --ignored --exact {CLIENT_TEST} \
                 --test-threads=1 >&2\n[ -f {} ] || {{ \
                 printf '%s\\n' 'child test {CLIENT_TEST} did not write its result sentinel' >&2; \
                 exit 1; }}\n",
                quote(&exe),
                quote(result)
            ),
        );
    }

    fn provider(&self, name: &str, body: &str) {
        let path = self.root.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}")).expect("write provider");
        let mut permissions = fs::metadata(&path).expect("provider").permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        fs::set_permissions(&path, permissions).expect("make provider executable");
    }
}

fn quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

/// The engine seam with the real broker behind it. Every other host call
/// keeps its default.
struct BrokerHost {
    global_root: PathBuf,
    /// The run's worktree; a broker serves only a run that has one.
    worktree: PathBuf,
    command: PathBuf,
    audit_root: PathBuf,
    sandbox: ResolvedSandbox,
    sockets: Mutex<Vec<PathBuf>>,
}

impl BrokerHost {
    fn run(&self, run_id: &str, timeout: Duration) -> Result<DispatchOutcome, DispatchError> {
        let audit = V2AuditWriter::with_disk_sinks(
            &self.audit_root.join(run_id),
            Arc::new(orbit_store::Store::open_in_memory().expect("audit store")),
            "ws_test",
            run_id,
            "codex:test",
            None,
        )
        .expect("audit writer");
        let spec = AgentLoopSpec {
            tool_disallow_list: None,
            instruction: String::new(),
            tools: Vec::new(),
            on_denial: OnDenial::Terminate,
            model: None,
            reasoning_effort: None,
            max_iterations: 1,
            backend: None,
            provider: Provider::Codex,
            wall_clock_timeout_seconds: timeout.as_secs(),
            require_response_envelope: false,
            require_completion_envelope: false,
            proc_allowed_programs: None,
            proc_disallowed_programs: None,
            trusted_host_execution: false,
        };
        run_cli_backend(
            self,
            &spec,
            "broker_probe",
            run_id,
            audit,
            &json!({"prompt": "probe the plugin broker"}),
            None,
        )
    }

    fn socket(&self) -> PathBuf {
        self.sockets
            .lock()
            .expect("sockets")
            .last()
            .cloned()
            .expect("a broker was started")
    }
}

impl RuntimeHost for BrokerHost {
    fn start_plugin_broker(
        &self,
        run: &PluginBrokerRun,
    ) -> Result<Option<Box<dyn PluginBrokerHandle>>, OrbitError> {
        let broker = PluginBroker::start(&self.global_root, &run.run_id, EchoDispatch::shared())?;
        self.sockets
            .lock()
            .expect("sockets")
            .push(broker.socket_path().to_path_buf());
        Ok(Some(Box::new(broker)))
    }

    fn tool_context_for_activity(
        &self,
        _run_id: Option<&str>,
        _fs_profile: Option<&str>,
        _fs_audit: Option<Arc<dyn FsAuditLogger>>,
        _proc_allowed_programs: Option<&[String]>,
    ) -> ToolContext {
        ToolContext {
            workspace_root: Some(self.worktree.clone()),
            ..Default::default()
        }
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
        _subprocess_cwd: Option<&Path>,
    ) -> Result<Option<ResolvedSandbox>, DispatchError> {
        Ok(Some(self.sandbox.clone()))
    }
}

fn read_result(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| format!("no result: {error}"))
}

fn assert_removed(socket: &Path) {
    let dir = socket.parent().expect("run dir");
    assert!(
        !dir.exists(),
        "the run's broker directory must be removed when the step ends: {}",
        dir.display()
    );
}

#[test]
fn another_runs_sandbox_and_the_host_are_refused_without_a_reply() {
    let Some(scratch) =
        scratch_or_skip("another_runs_sandbox_and_the_host_are_refused_without_a_reply")
    else {
        return;
    };
    let owner_result = scratch.result("owner");
    let release = scratch.root.join("release");
    scratch.client_provider(
        "codex",
        &[(RESULT_ENV, &owner_result), (HOLD_ENV, &release)],
    );
    let owner = scratch.host(scratch.root.clone(), "codex");

    std::thread::scope(|scope| {
        let owner_run = scope.spawn(|| owner.run("run-owner", Duration::from_secs(120)));
        // Once the owner has its reply, its broker is bound and stays up
        // until `release` exists.
        let owner_ready = wait_for(&owner_result);
        let socket = owner.sockets.lock().expect("sockets").last().cloned();

        let probes = socket.filter(|_| owner_ready).map(|socket| {
            let intruder_result = scratch.result("intruder");
            fs::create_dir(scratch.root.join("intruder")).expect("intruder provider dir");
            scratch.client_provider(
                "intruder/codex",
                &[(RESULT_ENV, &intruder_result), (TARGET_ENV, &socket)],
            );
            let intruder = scratch.host(scratch.root.clone(), "intruder/codex");
            let intruder_outcome = intruder.run("run-intruder", Duration::from_secs(60));

            let mut host_stream = UnixStream::connect(&socket).expect("host connects");
            let host_outcome = request_outcome(&mut host_stream);
            (
                intruder_outcome,
                read_result(&intruder_result),
                host_outcome,
            )
        });

        fs::write(&release, "").expect("release the owner run");
        let owner_outcome = owner_run.join().expect("owner run thread");

        assert!(
            owner_outcome.is_ok(),
            "owner step failed: {owner_outcome:?}"
        );
        assert_eq!(read_result(&owner_result), "ok");
        let (intruder_outcome, intruder_result, host_outcome) =
            probes.expect("the owner run never reached its broker");
        assert!(
            intruder_outcome.is_ok(),
            "intruder step failed: {intruder_outcome:?}"
        );
        assert_eq!(
            intruder_result, "closed",
            "a second run's sandbox must get no reply from this run's broker"
        );
        assert_eq!(
            host_outcome, "closed",
            "a same-UID host process outside the sandbox must get no reply"
        );
        assert_removed(&owner.socket());
    });
}
