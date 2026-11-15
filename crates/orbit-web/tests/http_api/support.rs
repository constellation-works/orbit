use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use std::collections::BTreeSet;

use chrono::{DateTime, TimeZone, Utc};
use orbit_core::{
    JobRun, JobRunState, JobRunStep, JobTargetType, OrbitRuntime, V2AuditEventInsertParams,
};
use orbit_types::workflow::{ChildDispatch, PipelineState};
use orbit_types::workspace::{
    Workspace, WorkspaceCheckout, WorkspaceCheckoutRole, WorkspaceRegistry, WorkspaceStatus,
};
use reqwest::blocking::{Client, RequestBuilder, Response};
use serde_json::{Value, json};
use tracing_subscriber::prelude::*;

const CHILD_TEST: &str = "ORBIT_HTTP_TEST_CHILD";
const FIXTURE_ROOT: &str = "ORBIT_HTTP_FIXTURE_ROOT";

/// No environment mutation in the parent: managed authority is removed only on the child command.
/// Asks the server child to open its workspace with path leases enabled.
const LEASES_ENABLED: &str = "ORBIT_HTTP_LEASES_ENABLED";

/// The fixed lease policy with path leases enabled.
pub(super) fn leases_enabled() -> orbit_core::runtime::task::locks::lease::LeasePolicy {
    orbit_core::runtime::task::locks::lease::LeasePolicy {
        enabled: true,
        ..orbit_core::runtime::task::locks::lease::LeasePolicy::FIXED
    }
}

pub(super) fn isolated(name: &str, body: impl FnOnce()) {
    if std::env::var(CHILD_TEST).ok().as_deref() == Some(name) {
        body();
        return;
    }
    let home = tempfile::tempdir().expect("isolated test home");
    let mut command = fixture_command(home.path());
    command
        // Ignored tests are skipped in the ordinary suite, but a child
        // re-executed by name must still run them.
        .args([
            "--exact",
            name,
            "--include-ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_TEST, name);
    // The child leads its own process group and servers inherit it, so the
    // shared hang guard's group kill cannot orphan a server.
    let logs = tempfile::tempdir().expect("isolated test logs");
    let output = orbit_common::test_env::run_child_test(&mut command, name, logs.path());
    orbit_common::test_env::assert_child_test_passed(
        name,
        output.status,
        &output.stdout,
        &output.stderr,
    );
}

fn fixture_command(home: &Path) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    // An explicit agent envelope keeps an inherited TTY from granting operator
    // authority on surfaces that read the process envelope (the dashboard
    // itself grants only through `--operator`).
    command
        .current_dir(home)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("ORBIT_AGENT_NAME", "http-fixture")
        .env("ORBIT_AGENT_MODEL", "http-fixture")
        .env_remove("ORBIT_LOG_PATH")
        // libtest otherwise prefixes a single-threaded child's first output
        // line with its test status, hiding the server readiness announcement.
        .env_remove("RUST_TEST_THREADS");
    command
}

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        // Also runs on assertion failure or timeout. Always reap the direct child.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub(super) struct Fixture {
    temp: tempfile::TempDir,
    pub(super) global: PathBuf,
    pub(super) work: PathBuf,
    pub(super) runtime: OrbitRuntime,
}

impl Fixture {
    pub(super) fn new() -> Self {
        Self::with_owner(None, "http-fixture", "HF")
    }

    /// A second host: its own root and `[machine]` identity.
    pub(super) fn host(name: &str, task_prefix: &str) -> Self {
        Self::with_owner(None, name, task_prefix)
    }

    /// A replica checkout of a workspace `owner_machine_id` owns.
    pub(super) fn replica_of(owner_machine_id: &str) -> Self {
        Self::with_owner(Some(owner_machine_id), "http-fixture", "HF")
    }

    pub(super) fn machine_id(&self) -> String {
        orbit_registry::machine_identity::load_machine_identity(&self.global)
            .unwrap()
            .id
    }

    /// Appends `toml` to the workspace `config.toml` and reopens the runtime,
    /// so the in-process runtime reads it as well as any server started next.
    pub(super) fn with_workspace_config(self, toml: &str) -> Self {
        let Self {
            temp,
            global,
            work,
            runtime,
        } = self;
        drop(runtime);
        let config = work.join("config.toml");
        let mut text = fs::read_to_string(&config).unwrap_or_default();
        text.push_str(toml);
        fs::write(&config, text).unwrap();
        let registry = orbit_registry::workspace_registry::load_registry_from(
            &orbit_registry::workspace_registry::registry_path_for(&global),
        )
        .unwrap();
        let runtime =
            orbit_cmd::registry_runtime::RegisteredRuntimeFactory::open_registered_checkout(
                &global,
                &registry.workspaces[0],
                &registry.checkouts[0],
            )
            .expect("reopened disposable runtime");
        Self {
            temp,
            global,
            work,
            runtime,
        }
    }

    /// Reopens an owner-prepared checkout with replica authority for HTTP tests.
    pub(super) fn into_replica(self, owner_machine_id: &str) -> Self {
        let Self {
            temp,
            global,
            work,
            runtime,
        } = self;
        drop(runtime);

        let registry_path = orbit_registry::workspace_registry::registry_path_for(&global);
        let mut registry =
            orbit_registry::workspace_registry::load_registry_from(&registry_path).unwrap();
        registry.workspaces[0].owner_machine_id = Some(owner_machine_id.to_owned());
        registry.checkouts[0].role = Some(WorkspaceCheckoutRole::Replica);
        registry.checkouts[0].owner_machine_id = Some(owner_machine_id.to_owned());
        orbit_registry::workspace_registry::save_registry_to(&registry, &registry_path).unwrap();
        let runtime =
            orbit_cmd::registry_runtime::RegisteredRuntimeFactory::open_registered_checkout(
                &global,
                &registry.workspaces[0],
                &registry.checkouts[0],
            )
            .expect("reopened disposable replica runtime");

        Self {
            temp,
            global,
            work,
            runtime,
        }
    }

    fn with_owner(remote_owner: Option<&str>, machine_name: &str, task_prefix: &str) -> Self {
        assert!(
            std::env::var_os(CHILD_TEST).is_some(),
            "mutable fixture must be isolated"
        );
        let temp = tempfile::tempdir().unwrap();
        let global = temp.path().join("global");
        let repo = temp.path().join("repo");
        let work = repo.join(".orbit");
        fs::create_dir_all(&global).unwrap();
        fs::create_dir_all(&work).unwrap();
        fs::write(
            work.join("config.yaml"),
            "schema_version: 1\nworkspace_id: ws_http_fixture\n",
        )
        .unwrap();
        if task_prefix == orbit_types::identity::LEGACY_TASK_PREFIX {
            fs::write(
                global.join("config.toml"),
                format!("[machine]\nid = \"hm_0000000000000001\"\nname = \"{machine_name}\"\ntask_prefix = \"{task_prefix}\"\n"),
            )
            .unwrap();
        }
        orbit_registry::ensure_machine_identity(&global, || {
            Ok(orbit_registry::NewMachineIdentity {
                name: machine_name.into(),
                task_prefix: task_prefix.into(),
            })
        })
        .unwrap();
        let id = "ws_http_fixture".to_string();
        let machine = orbit_registry::machine_identity::load_machine_identity(&global).unwrap();
        let owner_machine_id = remote_owner.map_or(machine.id, str::to_owned);
        let mut checkout = WorkspaceCheckout::owner(id.clone(), repo, work.clone());
        if remote_owner.is_some() {
            checkout.role = Some(WorkspaceCheckoutRole::Replica);
            checkout.owner_machine_id = Some(owner_machine_id.clone());
        }
        let now = Utc::now();
        let registry = WorkspaceRegistry {
            workspaces: vec![Workspace {
                id: id.clone(),
                name: "fixture".into(),
                owner_machine_id: Some(owner_machine_id),
                git_remote: None,
                ship_mode: None,
                base_branch: "agent-main".into(),
                status: WorkspaceStatus::Active,
                created_at: now,
                updated_at: now,
            }],
            checkouts: vec![checkout],
            ..Default::default()
        };
        orbit_registry::workspace_registry::save_registry_to(
            &registry,
            &orbit_registry::workspace_registry::registry_path_for(&global),
        )
        .unwrap();
        let runtime =
            orbit_cmd::registry_runtime::RegisteredRuntimeFactory::open_registered_checkout(
                &global,
                &registry.workspaces[0],
                &registry.checkouts[0],
            )
            .expect("disposable registered runtime");
        Self {
            temp,
            global,
            work,
            runtime,
        }
    }

    pub(super) fn path(&self, relative: &str) -> PathBuf {
        self.temp.path().join(relative)
    }

    pub(super) fn server(&self, operator: bool) -> Server {
        self.server_impl(operator, false, false)
    }

    /// A server whose `PATH` starts with `bin`, so its `ssh` is a stub there.
    pub(super) fn server_with_path(&self, operator: bool, bin: &Path) -> Server {
        self.server_with_path_envs(operator, bin, &[])
    }

    /// [`Self::server_with_path`] plus environment the server child inherits.
    pub(super) fn server_with_path_envs(
        &self,
        operator: bool,
        bin: &Path,
        extra: &[(&str, &std::ffi::OsStr)],
    ) -> Server {
        let path = std::env::join_paths(std::iter::once(bin.to_path_buf()).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .unwrap();
        let mut env = Vec::with_capacity(extra.len() + 1);
        env.push(("PATH", path.as_os_str()));
        env.extend_from_slice(extra);
        self.server_impl_with_env(
            operator,
            false,
            false,
            &self.path("process.log"),
            None,
            &env,
        )
    }

    /// A server whose workspace opens with path leases enabled, which no
    /// configuration can do [ORB-22808].
    pub(super) fn server_with_leases(&self, operator: bool) -> Server {
        self.server_with_envs(operator, &[(LEASES_ENABLED, std::ffi::OsStr::new("1"))])
    }

    /// A server with extra environment for the server child.
    pub(super) fn server_with_envs(
        &self,
        operator: bool,
        env: &[(&str, &std::ffi::OsStr)],
    ) -> Server {
        self.server_impl_with_env(operator, false, false, &self.path("process.log"), None, env)
    }

    /// A server whose process runs from a linked git worktree of the
    /// fixture's workspace repository, inside a managed run's environment
    /// (`ORBIT_MANAGED_RUN_CONTEXT` and `ORBIT_RUN_ID`) naming `run_id`. That
    /// is the one process shape in which the owning-run relaxation of a task
    /// write's context check can apply to a caller bound to the run.
    pub(super) fn server_in_linked_worktree_of_run(&self, operator: bool, run_id: &str) -> Server {
        let repo = self.temp.path().join("repo");
        let worktree = self.temp.path().join("linked-worktree");
        let git = |dir: &Path, args: &[&str]| {
            let mut command = Command::new("git");
            orbit_common::test_env::clear_inherited_authority(|name| {
                command.env_remove(name);
            });
            let output = command
                .current_dir(dir)
                .args([
                    "-c",
                    "user.name=fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                ])
                .args(args)
                .output()
                .expect("run git");
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&repo, &["init", "--quiet"]);
        git(&repo, &["commit", "--quiet", "--allow-empty", "-m", "base"]);
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                worktree.to_str().unwrap(),
            ],
        );
        spawn_server_from(
            self.temp.path(),
            &worktree,
            ServerFeatures {
                operator,
                resources: false,
                replay_worker: false,
            },
            &self.path("process.log"),
            None,
            &[
                ("ORBIT_MANAGED_RUN_CONTEXT", std::ffi::OsStr::new("1")),
                ("ORBIT_RUN_ID", std::ffi::OsStr::new(run_id)),
            ],
        )
    }

    pub(super) fn counted_task_server(&self) -> Server {
        self.server_impl_with_log(
            false,
            false,
            false,
            &self.path("process.log"),
            Some(&self.path("task-query.jsonl")),
        )
    }

    pub(super) fn counted_job_run_server(&self) -> Server {
        self.server_impl_with_env(
            false,
            false,
            false,
            &self.path("process.log"),
            None,
            &[(
                "ORBIT_HTTP_RUN_QUERY_TRACE",
                self.path("run-query.jsonl").as_os_str(),
            )],
        )
    }

    /// A server that records each exact pipeline-state batch read.
    pub(super) fn counted_run_state_server(&self) -> Server {
        self.server_impl_with_env(
            false,
            false,
            false,
            &self.path("process.log"),
            None,
            &[(
                "ORBIT_HTTP_RUN_STATE_TRACE",
                self.path("run-state.jsonl").as_os_str(),
            )],
        )
    }

    pub(super) fn run_state_trace(&self) -> Vec<Value> {
        fs::read_to_string(self.path("run-state.jsonl"))
            .unwrap()
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    pub(super) fn job_run_query_trace(&self) -> Vec<Value> {
        fs::read_to_string(self.path("run-query.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    pub(super) fn replay_server(&self) -> Server {
        self.server_impl(true, false, true)
    }

    pub(super) fn resource_server(&self) -> Server {
        self.server_impl(false, true, false)
    }

    pub(super) fn split_log_server(&self) -> Server {
        self.server_impl_with_log(false, false, false, &self.path("orbit.jsonl"), None)
    }

    fn server_impl(&self, operator: bool, resources: bool, replay_worker: bool) -> Server {
        self.server_impl_with_log(
            operator,
            resources,
            replay_worker,
            &self.path("process.log"),
            None,
        )
    }

    fn server_impl_with_log(
        &self,
        operator: bool,
        resources: bool,
        replay_worker: bool,
        log_path: &Path,
        task_query_log: Option<&Path>,
    ) -> Server {
        self.server_impl_with_env(
            operator,
            resources,
            replay_worker,
            log_path,
            task_query_log,
            &[],
        )
    }

    fn server_impl_with_env(
        &self,
        operator: bool,
        resources: bool,
        replay_worker: bool,
        log_path: &Path,
        task_query_log: Option<&Path>,
        env: &[(&str, &std::ffi::OsStr)],
    ) -> Server {
        spawn_server(
            self.temp.path(),
            operator,
            resources,
            replay_worker,
            log_path,
            task_query_log,
            env,
        )
    }

    pub(super) fn job(&self, name: &str) {
        self.sleep_job(name, 0);
    }

    pub(super) fn sleep_job(&self, name: &str, seconds: u32) {
        let dir = self.global.join("resources/jobs");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{name}.yaml")), format!(
            "schemaVersion: 2\nkind: Job\nmetadata:\n  name: {name}\nspec:\n  state: enabled\n  kind: workflow\n  steps:\n    - id: nap\n      default_input:\n        seconds: {seconds}\n      spec:\n        type: deterministic\n        action: sleep\n        config: {{}}\n"
        )).unwrap();
    }

    pub(super) fn seed_run(&self, name: &str, job: &str, state: JobRunState) -> JobRun {
        let now = Utc::now();
        let run = JobRun {
            build: None,
            run_id: name.into(),
            job_id: job.into(),
            attempt: 1,
            state,
            scheduled_at: now,
            created_at: now,
            started_at: (state != JobRunState::Pending).then_some(now),
            finished_at: state.is_terminal().then_some(now),
            duration_ms: None,
            pid: None,
            pid_start_time: None,
            input: None,
            retry_source_run_id: None,
            knowledge_metrics: None,
            resolved_crew: None,
            crew_model: None,
            steps: Vec::new(),
            executed_on: None,
        };
        self.save_run(&run);
        run
    }

    pub(super) fn save_run(&self, run: &JobRun) {
        self.runtime
            .sqlite_store()
            .unwrap()
            .upsert_job_run_for_workspace(&self.runtime.workspace_id().unwrap(), run, None)
            .unwrap();
    }
}

/// Serve the fixture layout under `root` (`root/global` with a workspace named
/// `fixture`) from an isolated server child.
pub(super) fn spawn_server(
    root: &Path,
    operator: bool,
    resources: bool,
    replay_worker: bool,
    log_path: &Path,
    task_query_log: Option<&Path>,
    env: &[(&str, &std::ffi::OsStr)],
) -> Server {
    spawn_server_from(
        root,
        root,
        ServerFeatures {
            operator,
            resources,
            replay_worker,
        },
        log_path,
        task_query_log,
        env,
    )
}

/// The fixture features one server child enables.
struct ServerFeatures {
    operator: bool,
    resources: bool,
    replay_worker: bool,
}

/// [`spawn_server`] with the server process's working directory at `cwd`.
fn spawn_server_from(
    root: &Path,
    cwd: &Path,
    features: ServerFeatures,
    log_path: &Path,
    task_query_log: Option<&Path>,
    env: &[(&str, &std::ffi::OsStr)],
) -> Server {
    let ServerFeatures {
        operator,
        resources,
        replay_worker,
    } = features;
    orbit_common::test_env::assert_child_test_exists("server_child");
    let log = tempfile::NamedTempFile::new_in(root).unwrap();
    let mut command = fixture_command(root);
    command
        .current_dir(cwd)
        .args(["--ignored", "--exact", "server_child", "--nocapture"])
        .env(FIXTURE_ROOT, root)
        .env("ORBIT_HTTP_PORT", "0")
        .env(
            "ORBIT_HTTP_REPLAY_WORKER",
            if replay_worker { "1" } else { "0" },
        )
        .env(
            "ORBIT_HTTP_RESOURCE_FIXTURE",
            if resources { "1" } else { "0" },
        )
        .env("ORBIT_HTTP_OPERATOR", if operator { "1" } else { "0" })
        .env("ORBIT_LOG_PATH", log_path)
        .stdout(Stdio::from(log.as_file().try_clone().unwrap()))
        .stderr(Stdio::from(log.as_file().try_clone().unwrap()));
    if let Some(path) = task_query_log {
        command.env("ORBIT_HTTP_TASK_QUERY_TRACE", path);
    }
    command.envs(env.iter().copied());
    let mut server = Server {
        process: Process(command.spawn().unwrap()),
        task_query_log: task_query_log.map(Path::to_owned),
        // A hang guard on each request, including an SSE stream's reads.
        // It outlasts the dashboard's own forward bound, so a forwarded
        // request reports that typed failure instead of a client timeout.
        client: Client::builder()
            .no_proxy()
            .timeout(orbit_common::test_env::FIXTURE_STEP_DEADLINE)
            .build()
            .unwrap(),
        origin: String::new(),
        log,
    };
    let started = Instant::now();
    loop {
        let output = fs::read_to_string(server.log.path()).unwrap();
        assert!(
            server.process.0.try_wait().unwrap().is_none(),
            "server exited: {output}"
        );
        // The child owns the port before announcing it. A probe of a
        // released parent-selected port can accept another fixture's
        // health response and send the first API request to its store.
        let complete_output = output.rsplit_once('\n').map_or("", |(lines, _)| lines);
        if server.origin.is_empty()
            && let Some(authority) = complete_output
                .lines()
                .find_map(|line| line.strip_prefix("Dashboard listening on http://"))
        {
            let address: SocketAddr = authority.parse().expect("child listening address");
            assert!(address.ip().is_loopback());
            assert_ne!(address.port(), 0, "child must announce its bound port");
            server.origin = format!("http://{address}");
        }
        if !server.origin.is_empty()
            && server
                .client
                .get(format!("{}/healthz", server.origin))
                .send()
                .is_ok_and(|response| response.status().is_success())
        {
            return server;
        }
        assert!(
            started.elapsed() < orbit_common::test_env::FIXTURE_STEP_DEADLINE,
            "server not ready after {:.1?} ({}): {output}",
            started.elapsed(),
            orbit_common::test_env::host_load()
        );
        thread::sleep(Duration::from_millis(20));
    }
}

pub(super) struct Server {
    process: Process,
    task_query_log: Option<PathBuf>,
    log: tempfile::NamedTempFile,
    client: Client,
    pub(super) origin: String,
}

impl Server {
    pub(super) fn task_query_trace(&self) -> Vec<Value> {
        fs::read_to_string(self.task_query_log.as_ref().unwrap())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    pub(super) fn pid(&self) -> u32 {
        self.process.0.id()
    }

    /// SIGTERM the server (graceful shutdown) and wait, bounded, for its exit.
    #[cfg(unix)]
    pub(super) fn terminate(&mut self) -> std::process::ExitStatus {
        // SAFETY: signalling this fixture's own direct child.
        unsafe {
            libc::kill(self.process.0.id() as libc::pid_t, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = self.process.0.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "server did not exit within 30s of SIGTERM"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    pub(super) fn request(&self, method: &str, path: &str) -> RequestBuilder {
        self.client
            .request(method.parse().unwrap(), format!("{}{path}", self.origin))
    }

    pub(super) fn get(&self, path: &str) -> Response {
        sent("GET", path, self.request("GET", path))
    }

    pub(super) fn send(&self, method: &str, path: &str, body: Value) -> Response {
        sent(
            method,
            path,
            self.request(method, path)
                .header("origin", &self.origin)
                .json(&body),
        )
    }
}

/// Send `request`; a failure, such as the client's hang guard expiring, names
/// the request, how long it ran and the host load.
pub(super) fn sent(method: &str, path: &str, request: RequestBuilder) -> Response {
    let started = Instant::now();
    request.send().unwrap_or_else(|error| {
        panic!(
            "{method} {path} failed after {:.1?} ({}): {error}",
            started.elapsed(),
            orbit_common::test_env::host_load()
        )
    })
}

pub(super) fn json_ok(response: Response) -> Value {
    let status = response.status();
    let value: Value = response.json().expect("HTTP JSON body");
    assert!(status.is_success(), "HTTP {status}: {value}");
    value
}

/// Follows `x-audit-next-before` from `prefix` (`/api/audit`, or a forwarded
/// `/api/on/<host>/audit`) until the server stops offering one. Each entry is
/// one page's execution ids and the cursor that page carried, so two walks can
/// be compared page by page.
pub(super) fn walk_audit_pages(
    get: impl Fn(&str) -> Response,
    prefix: &str,
    query: &str,
    limit: usize,
) -> Vec<(Vec<String>, Option<String>)> {
    let mut pages = Vec::new();
    let mut before: Option<String> = None;
    loop {
        let cursor = before
            .as_deref()
            .map(|value| format!("&before={value}"))
            .unwrap_or_default();
        let response = get(&format!(
            "{prefix}?workspace=ws_http_fixture&since=24h&limit={limit}&{query}{cursor}"
        ));
        before = response
            .headers()
            .get("x-audit-next-before")
            .map(|value| value.to_str().unwrap().to_string());
        let page = json_ok(response);
        let page = page.as_array().unwrap();
        assert!(page.len() <= limit);
        let ids = page
            .iter()
            .map(|event| event["execution_id"].as_str().unwrap().to_string())
            .collect();
        pages.push((ids, before.clone()));
        assert!(pages.len() < 50, "cursor must make progress");
        if before.is_none() {
            return pages;
        }
    }
}

pub(super) fn serve_fixture() {
    let trace = std::env::var_os("ORBIT_HTTP_TASK_QUERY_TRACE")
        .map(|path| (path, &["orbit.store.task_query", "orbit.core.codebase"][..]))
        .or_else(|| {
            std::env::var_os("ORBIT_HTTP_RUN_QUERY_TRACE")
                .map(|path| (path, &["orbit.core.job_run.reconcile"][..]))
        })
        .or_else(|| {
            std::env::var_os("ORBIT_HTTP_RUN_STATE_TRACE")
                .map(|path| (path, &["orbit.core.job_run_state"][..]))
        });
    let trace = trace.or_else(|| {
        std::env::var_os("ORBIT_HTTP_HOST_PROJECTION_TRACE")
            .map(|path| (path, &["orbit.hosts.workspace_projection"][..]))
    });
    if let Some((path, targets)) = trace {
        // Count task-store operations (with the codebase scan's per-task
        // computes and landed diffs) or stale-run scans across blocking threads.
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_span_events(tracing_subscriber::fmt::format::FmtSpan::NEW)
                    .with_span_list(false)
                    .with_writer(std::sync::Mutex::new(fs::File::create(path).unwrap()))
                    .with_filter(tracing_subscriber::filter::filter_fn(move |metadata| {
                        metadata.is_span() && targets.contains(&metadata.target())
                    })),
            )
            .init();
    }
    if let Ok(parties) = std::env::var("ORBIT_HTTP_SELECTION_BARRIER") {
        install_selection_barrier(parties.parse().expect("selection barrier parties"));
    }
    let root = PathBuf::from(std::env::var_os(FIXTURE_ROOT).expect("fixture root"));
    if std::env::var(LEASES_ENABLED).as_deref() == Ok("1") {
        orbit_core::runtime::task::locks::lease::install_test_lease_policy(
            &root.join("repo/.orbit"),
            leases_enabled(),
        );
    }
    // Ordinary fixtures launch a harmless stub; replay fixtures execute only
    // their disposable sleep job through the real worker.
    if std::env::var("ORBIT_HTTP_REPLAY_WORKER").as_deref() == Ok("1") {
        orbit_common::test_env::assert_child_test_exists("replay_worker_child");
        orbit_core::test_support::install_substitute_pipeline_worker([
            "sh".to_string(), "-c".to_string(),
            "export ORBIT_HTTP_REPLAY_RUN=\"$1\" ORBIT_HTTP_FIXTURE_ROOT=\"$2\"; exec \"$3\" --ignored --exact replay_worker_child --nocapture".to_string(),
            "replay-worker".to_string(), "{run_id}".to_string(),
            root.to_string_lossy().into_owned(),
            std::env::current_exe().unwrap().to_string_lossy().into_owned(),
        ]);
    } else {
        orbit_core::test_support::install_substitute_pipeline_worker(["sh", "-c", "exit 0"]);
    }
    let args = orbit_web::ServeArgs {
        host: "127.0.0.1".parse().unwrap(),
        port: std::env::var("ORBIT_HTTP_PORT").unwrap().parse().unwrap(),
        no_open: true,
        global: true,
        workspace: Some("fixture".into()),
        operator: std::env::var("ORBIT_HTTP_OPERATOR").unwrap() == "1",
    };
    if std::env::var("ORBIT_HTTP_RESOURCE_FIXTURE").as_deref() == Ok("1") {
        use orbit_core::runtime::host_resource::{
            DiskSample, HostResourceProbe, HostResourceSample,
        };
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        struct Probe(AtomicUsize);
        impl HostResourceProbe for Probe {
            fn sample(&self, paths: &[PathBuf]) -> HostResourceSample {
                let phase = self.0.fetch_add(1, Ordering::SeqCst);
                HostResourceSample {
                    sampled_at: Utc::now()
                        - chrono::Duration::seconds(match phase {
                            0 => 10,
                            1 => 5,
                            4.. => 20,
                            _ => 0,
                        }),
                    cpu_percent: (phase != 3).then_some(95.0),
                    cpu_utilization_percent: None,
                    memory_percent: Some(50.0),
                    io_full_percent: None,
                    disks: paths
                        .iter()
                        .enumerate()
                        .map(|(index, path)| DiskSample {
                            path: path.clone(),
                            used_percent: (phase != 3).then_some(if index == 0 {
                                92.0
                            } else {
                                40.0
                            }),
                        })
                        .collect(),
                }
            }
        }
        let runtime = OrbitRuntime::in_memory()
            .unwrap()
            .with_host_resource_probe(Arc::new(Probe(AtomicUsize::new(0))));
        orbit_web::serve(&runtime, args).unwrap();
    } else if std::env::var("ORBIT_HTTP_EMBEDDED").as_deref() == Ok("1") {
        let global = root.join("global");
        let registry = orbit_registry::workspace_registry::load_registry_from(
            &orbit_registry::workspace_registry::registry_path_for(&global),
        )
        .unwrap();
        let runtime =
            orbit_cmd::registry_runtime::RegisteredRuntimeFactory::open_registered_checkout(
                &global,
                &registry.workspaces[0],
                &registry.checkouts[0],
            )
            .unwrap();
        // fixture_command runs from root, outside repo and its .orbit directory.
        assert_eq!(
            std::env::current_dir().unwrap(),
            root.canonicalize().unwrap()
        );
        assert_ne!(runtime.workspace_id().unwrap(), "default");
        orbit_web::serve(&runtime, args).unwrap();
    } else {
        orbit_web::serve_from_env(args, Some(&root.join("global"))).unwrap();
    }
}

/// Holds each observed workspace task-candidate selection until `parties` of
/// them are in flight together. A scan that waits out the bound instead fails,
/// which marks its workspace unavailable in the aggregate response.
fn install_selection_barrier(parties: usize) {
    let gate = Arc::new((Mutex::new(0_usize), Condvar::new()));
    orbit_core::test_support::install_candidate_selection_checkpoint(move || {
        let (entered, arrived) = &*gate;
        let mut count = entered.lock().unwrap_or_else(PoisonError::into_inner);
        *count += 1;
        arrived.notify_all();
        let (count, _) = arrived
            .wait_timeout_while(count, Duration::from_secs(5), |count| *count < parties)
            .unwrap_or_else(PoisonError::into_inner);
        if *count < parties {
            return Err(orbit_core::OrbitError::Execution(format!(
                "only {} of {parties} workspace selections were in flight; the per-workspace scans are serialised",
                *count
            )));
        }
        Ok(())
    });
}

pub(super) fn execute_replay_worker() {
    let root = PathBuf::from(std::env::var_os(FIXTURE_ROOT).unwrap());
    let global = root.join("global");
    let registry = orbit_registry::workspace_registry::load_registry_from(
        &orbit_registry::workspace_registry::registry_path_for(&global),
    )
    .unwrap();
    let runtime = orbit_cmd::registry_runtime::RegisteredRuntimeFactory::open_registered_checkout(
        &global,
        &registry.workspaces[0],
        &registry.checkouts[0],
    )
    .unwrap();
    runtime
        .execute_pipeline_run_worker(&std::env::var("ORBIT_HTTP_REPLAY_RUN").unwrap())
        .unwrap();
}

pub(super) fn write_json(path: &Path, value: Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

pub(super) fn error_code(response: Response, status: u16, code: &str) -> Value {
    let actual = response.status().as_u16();
    let value: Value = response.json().unwrap();
    assert_eq!(actual, status, "{value}");
    assert_eq!(value["code"], json!(code), "{value}");
    value
}

/// Restore the pre-retention-analytics store shape so the public open path
/// must backfill existing rows, rewinding the later executor census, crew
/// account readings, handoff write guards, task-run ownership and worker
/// resource evidence and agent execution identities with it. The fixture records
/// invocations with the current binary before the upgrade, so its nullable
/// `invocations.account` and `invocations.agent_execution_id` columns stay.
/// Fixture-only; every caller runs in a child.
pub(super) fn rewind_retention_analytics(fixture: &Fixture) {
    fixture
        .runtime
        .sqlite_store()
        .unwrap()
        .with_transaction(|tx| {
            tx.connection()
                .execute_batch(
                    "DROP TABLE IF EXISTS agent_executions;
             DROP TABLE IF EXISTS worker_resource_limits;
             DROP TABLE IF EXISTS assistance_attempts;
             DROP TABLE IF EXISTS assistance_slots;
             DROP TABLE IF EXISTS assistance_budgets;
             DROP TABLE IF EXISTS assistance_work;
             DROP TABLE IF EXISTS automation_condition_audit;
             DROP TABLE IF EXISTS automation_condition_states;
             DELETE FROM schema_meta WHERE key LIKE 'migration.v%' AND key >= 'migration.v0054';
             DROP TABLE landed_diff_stats;
             DROP TRIGGER job_run_tasks_insert;
             DROP TRIGGER job_run_tasks_update;
             DROP TRIGGER job_run_tasks_delete;
             DROP TABLE job_run_tasks;
             DROP TABLE IF EXISTS crew_performance_cells;
             DROP TABLE IF EXISTS crew_performance_snapshot;
             DROP TRIGGER handoff_settlement_insert_guard;
             DROP TRIGGER handoff_settlement_update_guard;
             DROP TABLE provider_account_limit_observations;
             DROP TABLE executor_census;
             DROP TRIGGER retain_task_lock_denial;
             DROP TRIGGER retain_run_commit_insert;
             DROP TRIGGER retain_run_commit_update;
             DROP TABLE task_lock_denials;
             ALTER TABLE job_runs DROP COLUMN commit_decision;
             ALTER TABLE job_runs DROP COLUMN commit_committed;",
                )
                .map_err(|error| orbit_core::OrbitError::Store(error.to_string()))?;
            Ok(())
        })
        .unwrap();
}

/// Run pending store migrations through the production open boundary.
pub(super) fn upgrade_retention_analytics(fixture: &Fixture) {
    orbit_store::Store::open(&fixture.global.join("orbit.db")).unwrap();
}

// --- Shared run-scenario fixture helpers ---

pub(super) fn get(server: &Server, path: &str) -> Value {
    json_ok(server.get(path))
}

pub(super) fn run_detail(server: &Server, run_id: &str) -> Value {
    get(server, &format!("/api/runs/{run_id}"))
}

pub(super) fn list_rows(page: &Value) -> &[Value] {
    page["items"].as_array().unwrap()
}

pub(super) fn run_ids(page: &Value) -> BTreeSet<String> {
    list_rows(page)
        .iter()
        .map(|run| run["run_id"].as_str().unwrap().to_string())
        .collect()
}

pub(super) fn id_set(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|id| id.to_string()).collect()
}

pub(super) fn ws_id(fixture: &Fixture) -> String {
    fixture.runtime.workspace_id().unwrap()
}

pub(super) fn store(fixture: &Fixture) -> orbit_store::Store {
    fixture.runtime.sqlite_store().unwrap()
}

/// The one `v2_audit_events` insert shape every scenario seeds through.
#[allow(clippy::too_many_arguments)]
pub(super) fn insert_audit(
    fixture: &Fixture,
    workspace: &str,
    source: &str,
    event_id: &str,
    run_id: &str,
    ts: DateTime<Utc>,
    event_type: &str,
    agent: &str,
    parent: Option<&str>,
    payload: Value,
) {
    fixture
        .runtime
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: workspace.into(),
            event_id: event_id.into(),
            source: source.into(),
            schema_version: 1,
            event_type: event_type.into(),
            ts,
            run_id: run_id.into(),
            agent_identity: agent.into(),
            parent_event_id: parent.map(str::to_string),
            workspace_path: None,
            payload_json: payload.to_string(),
        })
        .unwrap();
}

/// [`insert_audit`] with the default source and the fixture agent.
pub(super) fn insert_audit_default(
    fixture: &Fixture,
    workspace: &str,
    event_id: &str,
    run_id: &str,
    ts: DateTime<Utc>,
    event_type: &str,
    payload: Value,
) {
    insert_audit(
        fixture,
        workspace,
        "v2_envelope",
        event_id,
        run_id,
        ts,
        event_type,
        "fixture",
        None,
        payload,
    );
}

/// [`insert_audit_default`] in the fixture's own workspace.
pub(super) fn insert_audit_here(
    fixture: &Fixture,
    event_id: &str,
    run_id: &str,
    ts: DateTime<Utc>,
    event_type: &str,
    payload: Value,
) {
    insert_audit_default(
        fixture,
        &ws_id(fixture),
        event_id,
        run_id,
        ts,
        event_type,
        payload,
    );
}

pub(super) fn make_job_run(
    run_id: &str,
    job_id: &str,
    state: JobRunState,
    at: DateTime<Utc>,
    input: Option<Value>,
) -> JobRun {
    JobRun {
        build: None,
        run_id: run_id.into(),
        job_id: job_id.into(),
        attempt: 1,
        state,
        scheduled_at: at,
        created_at: at,
        started_at: Some(at),
        finished_at: state.is_terminal().then_some(at),
        duration_ms: None,
        pid: None,
        pid_start_time: None,
        input,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
        executed_on: None,
    }
}

pub(super) fn write_state(fixture: &Fixture, run: &JobRun, state: &PipelineState) {
    fixture.runtime.write_run_state(&run.run_id, state).unwrap();
}

pub(super) fn pipeline_for(run: &JobRun) -> PipelineState {
    PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({}))
}

/// Build a fresh `PipelineState` for `run`, mutate it, and persist it.
pub(super) fn state_with(fixture: &Fixture, run: &JobRun, edit: impl FnOnce(&mut PipelineState)) {
    let mut state = pipeline_for(run);
    edit(&mut state);
    write_state(fixture, run, &state);
}

pub(super) fn at(hour: u32, minute: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 10, hour, minute, 0).unwrap()
}

pub(super) fn dispatch(child: &JobRun) -> ChildDispatch {
    dispatch_ids(&child.run_id, &child.job_id)
}

pub(super) fn dispatch_ids(run_id: &str, job_id: &str) -> ChildDispatch {
    child_dispatch(run_id, job_id, "leaf_invoke", false, false, Utc::now())
}

/// A submitted child dispatch with an explicit step and flags.
pub(super) fn child_dispatch(
    run_id: &str,
    job_id: &str,
    step: &str,
    wait: bool,
    coordinator: bool,
    at: DateTime<Utc>,
) -> ChildDispatch {
    ChildDispatch::submitted(
        run_id.into(),
        job_id.into(),
        step.into(),
        wait,
        coordinator,
        at,
    )
}

pub(super) fn operations(trace: &[Value], name: &str) -> usize {
    trace
        .iter()
        .filter(|event| event["span"]["name"] == name)
        .count()
}

/// Record `child` as a dispatched child of `parent`.
pub(super) fn link_child(fixture: &Fixture, parent: &JobRun, child: &JobRun) {
    state_with(fixture, parent, |state| {
        state.record_child_dispatch(dispatch(child))
    });
}

/// A `test.event` audit row as the provider and log scenarios seed them.
pub(super) fn insert_test_event(
    fixture: &Fixture,
    workspace: &str,
    run_id: &str,
    event_id: &str,
    ts: &str,
    parent: Option<&str>,
    payload: Value,
) {
    let ts = ts.parse::<DateTime<Utc>>().unwrap();
    insert_audit(
        fixture,
        workspace,
        "v2_envelope",
        event_id,
        run_id,
        ts,
        "test.event",
        "codex",
        parent,
        payload,
    );
}

pub(super) fn add_workspace(fixture: &Fixture, id: &str, name: &str) -> OrbitRuntime {
    let path = orbit_registry::workspace_registry::registry_path_for(&fixture.global);
    let mut registry = orbit_registry::workspace_registry::load_registry_from(&path).unwrap();
    let mut workspace = registry.workspaces[0].clone();
    workspace.id = id.into();
    workspace.name = name.into();
    let repo = fixture.path(name);
    let work = repo.join(".orbit");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(
        work.join("config.yaml"),
        format!("schema_version: 1\nworkspace_id: {id}\n"),
    )
    .unwrap();
    let checkout = WorkspaceCheckout::owner(id.into(), repo, work);
    registry.workspaces.push(workspace.clone());
    registry.checkouts.push(checkout.clone());
    orbit_registry::workspace_registry::save_registry_to(&registry, &path).unwrap();
    orbit_cmd::registry_runtime::RegisteredRuntimeFactory::open_registered_checkout(
        &fixture.global,
        &workspace,
        &checkout,
    )
    .unwrap()
}

/// A successful `task_pr_pipeline` run for `ORB-2` in another workspace,
/// `offset_ms` after `created` (the cross-workspace list cases).
pub(super) fn seed_other_run(
    runtime: &OrbitRuntime,
    id: &str,
    offset_ms: i64,
    created: DateTime<Utc>,
) {
    let at = created + chrono::Duration::milliseconds(offset_ms);
    let run = make_job_run(
        id,
        "task_pr_pipeline",
        JobRunState::Success,
        at,
        Some(json!({"task_id": "ORB-2"})),
    );
    runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&runtime.workspace_id().unwrap(), &run, None)
        .unwrap();
}

/// A step record with no timing yet; callers set the fields they seed.
pub(super) fn run_step(
    target_type: JobTargetType,
    target_id: &str,
    state: JobRunState,
) -> JobRunStep {
    JobRunStep {
        lock_wait_ms: None,
        step_index: 0,
        target_type,
        target_id: target_id.into(),
        started_at: None,
        finished_at: None,
        duration_ms: None,
        exit_code: None,
        agent_response_json: None,
        state,
        error_code: None,
        failure_code: None,
        error_message: None,
    }
}

pub(super) fn save_record_step(fixture: &Fixture, run: &JobRun, step: &JobRunStep) {
    store(fixture)
        .upsert_job_run_step_for_workspace(&ws_id(fixture), &run.run_id, step)
        .unwrap();
}

/// One `step_started` or `step_finished` event in a run's audit trail.
pub(super) fn insert_step_audit(
    fixture: &Fixture,
    run_id: &str,
    step_id: &str,
    body_kind: &str,
    minute: u32,
) {
    let ts = at(6, minute);
    let event_id = format!("evt-{run_id}-{step_id}-{body_kind}");
    insert_audit_here(
        fixture,
        &event_id,
        run_id,
        ts,
        "activity.progress",
        json!({"event_id": event_id, "body_kind": body_kind, "step_id": step_id, "outcome": "success", "lock_wait_ms": 42, "ts": ts}),
    );
}

pub(super) fn insert_cancel_audit(fixture: &Fixture, run_id: &str, ts: DateTime<Utc>) {
    let payload = json!({
        "schemaVersion": 1, "event_type": "run.cancelled", "event_id": format!("evt-{run_id}"), "ts": ts, "run_id": run_id,
        "agent_identity": "not-the-actor", "body_kind": "run_cancelled", "actor": "on-call", "source": "cli",
        "reason": "stopped after the gate hung", "previous_state": "running", "final_state": "cancelled",
    });
    insert_audit(
        fixture,
        &ws_id(fixture),
        "v2_envelope",
        &format!("evt-{run_id}"),
        run_id,
        ts,
        "run.cancelled",
        "not-the-actor",
        None,
        payload,
    );
}

pub(super) fn pull_destination() -> orbit_store::contracts::PullDestination {
    orbit_store::contracts::PullDestination {
        owner_machine_id: "hm_owner".into(),
        owner_workspace_id: "ws_owner".into(),
        selector: "owner/workspace".into(),
        execution_machine_id: "hm_follower".into(),
    }
}

pub(super) fn pull_request(
    run: &JobRun,
    request_id: &str,
) -> orbit_store::contracts::AdmissionRequest {
    serde_json::from_value(json!({
        "request_id": request_id, "caller_version": env!("CARGO_PKG_VERSION"), "caller_schema": 1,
        "run_context": { "run_id": run.run_id, "job_name": run.job_id, "machine_name": "follower" },
        "ship": { "mode": "pr", "base_branch": "agent-main", "landing_branch": "agent-main", "completion": "review" },
    })).unwrap()
}

/// Bind `task_id`'s run to another host in the task document store.
pub(super) fn bind_task_run_to_host(fixture: &Fixture, task_id: &str, run_id: &str) {
    use orbit_store::contracts::TaskDocumentUpdateParams;
    use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};

    let documents = orbit_store::compose::workspace_coordinated_backends(
        TaskRegistryStore::open(&task_registry_path(&fixture.global)).unwrap(),
        ws_id(fixture),
        store(fixture),
    )
    .unwrap()
    .task
    .document;
    documents
        .update_task_document(
            task_id,
            TaskDocumentUpdateParams {
                actor: "fixture".into(),
                job_run_id: Some(Some(run_id.into())),
                job_run_machine: Some(Some(orbit_types::task::ExecutionLocation {
                    machine_id: "hm_follower_mini".into(),
                    machine_name: Some("follower-mini".into()),
                })),
                ..Default::default()
            },
        )
        .unwrap();
}

pub(super) fn persist_admission(
    store: &orbit_store::Store,
    workspace: &str,
    request_id: &str,
    admission: &orbit_store::contracts::LocalPullAdmission,
) {
    store.connection().lock().unwrap().execute(
        "UPDATE local_pull_admissions SET leaf_run_id=?1, record_json=?2 WHERE workspace_id=?3 AND request_id=?4",
        (admission.leaf_run_id.as_deref(), serde_json::to_string(admission).unwrap(), workspace, request_id),
    ).unwrap();
}
