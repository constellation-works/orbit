use std::fs::{self, File};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_core::{JobRun, JobRunState, OrbitRuntime};
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceRegistry, WorkspaceStatus};
use reqwest::blocking::{Client, RequestBuilder, Response};
use serde_json::{Value, json};
use wait_timeout::ChildExt;

const CHILD_TEST: &str = "ORBIT_HTTP_TEST_CHILD";
const FIXTURE_ROOT: &str = "ORBIT_HTTP_FIXTURE_ROOT";

/// No environment mutation in the parent: managed authority is removed only on the child command.
pub(super) fn isolated(name: &str, body: impl FnOnce()) {
    if std::env::var(CHILD_TEST).ok().as_deref() == Some(name) {
        body();
        return;
    }
    let home = tempfile::tempdir().expect("isolated test home");
    let stdout = home.path().join("stdout.log");
    let stderr = home.path().join("stderr.log");
    let mut command = fixture_command(home.path());
    command
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(CHILD_TEST, name)
        .stdout(File::create(&stdout).unwrap())
        .stderr(File::create(&stderr).unwrap());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = Process(command.spawn().expect("spawn isolated HTTP test"), true);
    let status = child
        .0
        .wait_timeout(Duration::from_secs(60))
        .expect("wait for isolated HTTP test")
        .unwrap_or_else(|| panic!("isolated HTTP test {name} exceeded 60 seconds"));
    let output = fs::read_to_string(stdout).unwrap();
    assert!(
        status.success(),
        "{name}:\n{output}\n{}",
        fs::read_to_string(stderr).unwrap()
    );
    assert!(
        output.contains("test result: ok. 1 passed;"),
        "the isolated child must execute exactly `{name}`: {output}"
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

struct Process(Child, bool);

impl Drop for Process {
    fn drop(&mut self) {
        // Also runs on assertion failure or timeout. Always reap the direct child.
        if self.1 {
            #[cfg(unix)]
            // SAFETY: the isolated child was spawned into its own process group;
            // servers inherit it, so a child-test timeout cannot orphan a server.
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGKILL);
            }
        }
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
        let now = Utc::now();
        let registry = WorkspaceRegistry {
            workspaces: vec![Workspace {
                id: id.clone(),
                name: "fixture".into(),
                owner_machine_id: Some(machine.id),
                git_remote: None,
                ship_mode: None,
                base_branch: "agent-main".into(),
                status: WorkspaceStatus::Active,
                created_at: now,
                updated_at: now,
            }],
            checkouts: vec![WorkspaceCheckout::owner(id, repo, work.clone())],
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
        self.server_impl(operator, false)
    }

    pub(super) fn resource_server(&self) -> Server {
        self.server_impl(false, true)
    }

    fn server_impl(&self, operator: bool, resources: bool) -> Server {
        orbit_common::test_env::assert_child_test_exists("server_child");
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let log = self.path(&format!("server-{port}.log"));
        let mut command = fixture_command(self.temp.path());
        command
            .args(["--ignored", "--exact", "server_child", "--nocapture"])
            .env(FIXTURE_ROOT, self.temp.path())
            .env("ORBIT_HTTP_PORT", port.to_string())
            .env(
                "ORBIT_HTTP_RESOURCE_FIXTURE",
                if resources { "1" } else { "0" },
            )
            .env("ORBIT_HTTP_OPERATOR", if operator { "1" } else { "0" })
            .env("ORBIT_LOG_PATH", self.path("process.log"))
            .stdout(Stdio::from(File::create(&log).unwrap()))
            .stderr(Stdio::from(
                File::options().append(true).open(&log).unwrap(),
            ));
        let mut server = Server {
            process: Process(command.spawn().unwrap(), false),
            client: Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            origin: format!("http://127.0.0.1:{port}"),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if server
                .client
                .get(format!("{}/healthz", server.origin))
                .send()
                .is_ok_and(|response| response.status().is_success())
            {
                return server;
            }
            assert!(
                server.process.0.try_wait().unwrap().is_none(),
                "server exited: {}",
                fs::read_to_string(&log).unwrap()
            );
            assert!(
                Instant::now() < deadline,
                "server readiness exceeded 10 seconds: {}",
                fs::read_to_string(&log).unwrap()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    pub(super) fn job(&self, name: &str) {
        let dir = self.global.join("resources/jobs");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{name}.yaml")), format!(
            "schemaVersion: 2\nkind: Job\nmetadata:\n  name: {name}\nspec:\n  state: enabled\n  kind: workflow\n  steps:\n    - id: nap\n      spec:\n        type: deterministic\n        action: sleep\n        config: {{}}\n"
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
    client: Client,
    pub(super) origin: String,
}

impl Server {
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
    let root = PathBuf::from(std::env::var_os(FIXTURE_ROOT).expect("fixture root"));
    // Any accidentally unguarded submission still launches only this harmless stub.
    orbit_core::test_support::install_substitute_pipeline_worker(["sh", "-c", "exit 0"]);
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
