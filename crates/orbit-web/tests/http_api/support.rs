use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_core::{JobRun, JobRunState, OrbitRuntime};
use orbit_types::workspace::{
    Workspace, WorkspaceCheckout, WorkspaceCheckoutRole, WorkspaceRegistry, WorkspaceStatus,
};
use reqwest::blocking::{Client, RequestBuilder, Response};
use serde_json::{Value, json};

const CHILD_TEST: &str = "ORBIT_HTTP_TEST_CHILD";
const FIXTURE_ROOT: &str = "ORBIT_HTTP_FIXTURE_ROOT";

/// No environment mutation in the parent: managed authority is removed only on the child command.
pub(super) fn isolated(name: &str, body: impl FnOnce()) {
    if std::env::var(CHILD_TEST).ok().as_deref() == Some(name) {
        body();
        return;
    }
    let home = tempfile::tempdir().expect("isolated test home");
    let mut command = fixture_command(home.path());
    command
        // Explicitly selected browser fixtures are ignored in the ordinary
        // suite, but must still execute in their isolated child.
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
    // An explicit agent envelope prevents an inherited TTY granting operator authority.
    command
        .current_dir(home)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("ORBIT_AGENT_NAME", "http-fixture")
        .env("ORBIT_AGENT_MODEL", "http-fixture")
        .env_remove("ORBIT_LOG_PATH");
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
        Self::with_owner(None)
    }

    /// A replica checkout of a workspace `owner_machine_id` owns.
    pub(super) fn replica_of(owner_machine_id: &str) -> Self {
        Self::with_owner(Some(owner_machine_id))
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

    fn with_owner(remote_owner: Option<&str>) -> Self {
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
        orbit_registry::ensure_machine_identity(&global, || {
            Ok(orbit_registry::NewMachineIdentity {
                name: "http-fixture".into(),
                task_prefix: "HF".into(),
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

    pub(super) fn counted_task_server(&self) -> Server {
        self.server_impl_with_log(
            false,
            false,
            false,
            &self.path("process.log"),
            Some(&self.path("task-query.jsonl")),
        )
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
        orbit_common::test_env::assert_child_test_exists("server_child");
        let log = tempfile::NamedTempFile::new_in(self.temp.path()).unwrap();
        let mut command = fixture_command(self.temp.path());
        command
            .args(["--ignored", "--exact", "server_child", "--nocapture"])
            .env(FIXTURE_ROOT, self.temp.path())
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
        let mut server = Server {
            process: Process(command.spawn().unwrap()),
            task_query_log: task_query_log.map(Path::to_owned),
            client: Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            origin: String::new(),
            log,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
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
                Instant::now() < deadline,
                "server readiness exceeded 10 seconds: {output}"
            );
            thread::sleep(Duration::from_millis(20));
        }
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

    pub(super) fn request(&self, method: &str, path: &str) -> RequestBuilder {
        self.client
            .request(method.parse().unwrap(), format!("{}{path}", self.origin))
    }

    pub(super) fn get(&self, path: &str) -> Response {
        self.request("GET", path).send().unwrap()
    }

    pub(super) fn send(&self, method: &str, path: &str, body: Value) -> Response {
        self.request(method, path)
            .header("origin", &self.origin)
            .json(&body)
            .send()
            .unwrap()
    }
}

pub(super) fn json_ok(response: Response) -> Value {
    let status = response.status();
    let value: Value = response.json().expect("HTTP JSON body");
    assert!(status.is_success(), "HTTP {status}: {value}");
    value
}

pub(super) fn serve_fixture() {
    if let Some(path) = std::env::var_os("ORBIT_HTTP_TASK_QUERY_TRACE") {
        // Count real store operations across the server's blocking threads.
        tracing_subscriber::fmt()
            .json()
            .with_env_filter("orbit.store.task_query=trace")
            .with_span_events(tracing_subscriber::fmt::format::FmtSpan::NEW)
            .with_span_list(false)
            .with_writer(std::sync::Mutex::new(fs::File::create(path).unwrap()))
            .init();
    }
    let root = PathBuf::from(std::env::var_os(FIXTURE_ROOT).expect("fixture root"));
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
                    memory_percent: Some(50.0),
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
    } else {
        orbit_web::serve_from_env(args, Some(&root.join("global"))).unwrap();
    }
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
