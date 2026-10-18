//! [ORB-15435] The `pipeline.child_wait` audit row a parent records when the
//! child it blocked on settles without success.
//!
//! A held child's wait entry carries no `error`, and neither does a wait that
//! timed out on a running child, so the row once said only that the parent's
//! wait failed. Every non-success row now names the child run, its status and,
//! when the child recorded one, its reason. A held or cancelled child is a
//! settlement the parent handles, not a failed observation, so it no longer
//! reaches the incident list; a failed child stays an unexpected incident.
//!
//! Each case submits a real child through `invoke_and_wait` and executes or
//! cancels it in process while the parent's wait polls it. The fixture retains
//! each substitute's kernel identity behind a release-observation handshake,
//! then waits for the supervisor's reap before deleting the temporary root,
//! including when an observation returns an error or an assertion unwinds.
//! The accepted control stream explicitly uses blocking I/O: on Darwin it
//! inherits the listener's nonblocking flag, so readiness alone cannot prove
//! the identity is readable. A gated real worker covers that preparation gap.

use std::collections::BTreeSet;
#[cfg(unix)]
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;
#[cfg(unix)]
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_core::{FailureClass, OrbitRuntime};
use orbit_engine::RuntimeHost;
use orbit_store::contracts::{JobRunStoreBackend, classify};
use orbit_tools::ToolContext;
use orbit_types::telemetry::{AuditEvent, AuditEventStatus};
use orbit_types::tool::{McpCapability, ToolSessionContext};
use orbit_types::workflow::{BaselineRedHold, PipelineState};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::dispatch_admission::isolated;

const PARENT: &str = "child_wait_parent";
const CHILD: &str = "child_wait_child";

struct Fixture {
    global: PathBuf,
    repo: PathBuf,
    runtime: OrbitRuntime,
    jobs: Arc<dyn JobRunStoreBackend>,
    parent: String,
    #[cfg(unix)]
    listener: std::net::TcpListener,
    #[cfg(unix)]
    workers: Mutex<Vec<Substitute>>,
    #[cfg(unix)]
    cleaned: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(unix)]
    acquisition_events: Option<std::sync::mpsc::SyncSender<&'static str>>,
    // Fields drop in declaration order after `Drop::drop`. `root` stays
    // last as a backstop, but the guarantee comes from `Fixture::drop`:
    // it reaps the workers and waits for each detached supervisor's own
    // observer completion, and the fixture runtimes carry no background
    // host-resource sampler (see `Fixture::new`), so no owner is left
    // that can recreate a path inside the tree while `TempDir` removes it.
    root: TempDir,
}

#[cfg(unix)]
struct Substitute {
    run: String,
    pid: u32,
    process: orbit_common::process::signal::BoundProcess,
    control: std::net::TcpStream,
}

#[cfg(unix)]
fn reaped(pid: u32) -> bool {
    // Signal zero is observational and, unlike process_is_alive, includes
    // zombies. ESRCH therefore proves the supervisor has reaped the child.
    // SAFETY: signal zero neither mutates memory nor delivers a signal.
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    result == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        // A submitted child's substitute worker stays alive until the test
        // has settled the child, so the supervisor never interrupts it.
        #[cfg(unix)]
        let listener = crate::worker_fixture::install_owned(&root.path().join("ran-{run_id}"));
        #[cfg(not(unix))]
        crate::worker_fixture::install(root.path().join("ran-{run_id}").as_path(), "created");
        let global = root.path().join("global");
        let repo = root.path().join("repo");
        let jobs_dir = global.join("resources/jobs");
        std::fs::create_dir_all(&jobs_dir).unwrap();
        std::fs::create_dir_all(repo.join(".orbit")).unwrap();
        // The child runs the guard a gate runs over its own child's result,
        // so its outcome follows the result named in its input.
        std::fs::write(
            jobs_dir.join(format!("{CHILD}.yaml")),
            serde_json::to_string(&json!({
                "schemaVersion": 2, "kind": "Job",
                "metadata": {"name": CHILD},
                "spec": {"state": "enabled", "steps": [{
                    "id": "require_child_success",
                    "spec": {
                        "type": "deterministic",
                        "action": "pipeline_success_guard",
                        "config": {},
                    },
                    "default_input": {
                        "context": "fixture leaf",
                        "results": "{{ input.results }}",
                    },
                }]}
            }))
            .unwrap(),
        )
        .unwrap();
        // No background host-resource sampler for this fixture: the
        // default runtime's sampler thread holds the runtime through a
        // `Weak` upgrade and writes `global/cache/host-resource-pressure.json`
        // via an atomic write that recreates parent directories. It has
        // no join handle, so it could recreate the tree after teardown.
        // An injected probe keeps the monitor unsampled in the background.
        let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit"))
            .unwrap()
            .with_host_resource_probe(crate::dispatch_admission::PressureProbe::calm());
        let jobs = orbit_store::compose::workspace_job_run_store(
            runtime.sqlite_store().unwrap(),
            runtime.workspace_id().unwrap(),
        );
        let parent = jobs
            .insert_job_run(PARENT, 1, Utc::now(), Some(json!({})), None)
            .unwrap();
        jobs.write_run_state(
            &parent.run_id,
            &PipelineState::new(parent.run_id.clone(), parent.job_id.clone(), json!({})),
        )
        .unwrap();
        Self {
            root,
            global,
            repo,
            runtime,
            jobs,
            parent: parent.run_id,
            #[cfg(unix)]
            listener,
            #[cfg(unix)]
            workers: Mutex::new(Vec::new()),
            #[cfg(unix)]
            cleaned: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            #[cfg(unix)]
            acquisition_events: None,
        }
    }

    /// Submit a child over `results` and wait on it, while `settle` acts on
    /// the submitted child from another thread. Returns the child's
    /// `pipeline.child_wait` row and the activity's output.
    fn observe(
        &self,
        results: Value,
        timeout_seconds: u64,
        settle: impl FnOnce(&OrbitRuntime, &str) + Send,
    ) -> (String, AuditEvent, Value) {
        let before = self.child_runs();
        let (child, output) = std::thread::scope(|scope| {
            let settler = scope.spawn(|| {
                let child = self.await_new_child(&before);
                #[cfg(unix)]
                self.own_worker(&child).unwrap();
                // A second runtime on the same roots, as a worker opens.
                // Same no-background-sampler rule as `Fixture::new`.
                let runtime = OrbitRuntime::from_roots(&self.global, &self.repo.join(".orbit"))
                    .unwrap()
                    .with_host_resource_probe(crate::dispatch_admission::PressureProbe::calm());
                settle(&runtime, &child);
                child
            });
            let output = crate::scenarios::with_prompt_pipeline_polls(|| {
                self.runtime
                    .run_deterministic(
                        "invoke_and_wait",
                        &json!({}),
                        &json!({
                            "job_name": CHILD,
                            "run_input": {"results": results},
                            "run_id": self.parent,
                            "step_id": "deliver",
                            "timeout_seconds": timeout_seconds,
                            "poll_interval_seconds": 1,
                        }),
                        // The engine dispatches a gate's child with operator authority.
                        ToolContext {
                            session_context: ToolSessionContext {
                                effective_capabilities: BTreeSet::from([McpCapability::Operator]),
                                ..ToolSessionContext::default()
                            },
                            ..ToolContext::default()
                        },
                    )
                    .expect("the wait returns the child's settlement as data")
            });
            (settler.join().unwrap(), output)
        });
        std::fs::write(self.root.path().join(format!("ran-{child}")), "").unwrap();
        let row = self
            .runtime
            .list_audit_events(None, None, None, None, 1000)
            .unwrap()
            .into_iter()
            .find(|event| {
                event.command == "pipeline.child_wait"
                    && event.target_id.as_deref() == Some(child.as_str())
            })
            .unwrap_or_else(|| panic!("no pipeline.child_wait row for {child}"));
        (child, row, output)
    }

    #[cfg(unix)]
    fn own_worker(&self, child: &str) -> io::Result<()> {
        let phase = std::cell::Cell::new("poll_listener");
        let acquired = self.acquire_worker(child, &phase).inspect_err(|error| {
            // Preserve the OS error itself, rather than wrapping it and losing
            // raw_os_error(). Keep the phase in the same failure record.
            let _ = writeln!(
                io::stderr(),
                "owned worker acquisition failed: run={child} phase={} kind={:?} raw_os_error={:?} error={error} root={}",
                phase.get(), error.kind(), error.raw_os_error(), self.root.path().display()
            );
            if let Ok(log) = std::fs::read_to_string(
                self.runtime.paths().logs_dir.join(format!("{child}.worker.log")),
            ) {
                let _ = writeln!(io::stderr(), "unacquired worker log: run={child}\n{log}");
            }
        });
        if let Some(events) = &self.acquisition_events {
            let _ = events.send(if acquired.is_ok() {
                "acquired"
            } else {
                "failed"
            });
        }
        acquired
    }

    #[cfg(unix)]
    fn acquire_worker(&self, child: &str, phase: &std::cell::Cell<&'static str>) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        let mut ready = libc::pollfd {
            fd: self.listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // A bounded readiness wait, not a sleep to provoke the marker race.
        phase.set("poll_listener");
        // SAFETY: one initialized pollfd, retained listener, bounded timeout.
        let polled = unsafe { libc::poll(&mut ready, 1, 5_000) };
        if polled <= 0 {
            return Err(if polled == 0 {
                io::Error::new(io::ErrorKind::TimedOut, "owned worker handshake")
            } else {
                io::Error::last_os_error()
            });
        }
        let _ = writeln!(
            io::stderr(),
            "owned worker readiness: run={child} revents={:#x}",
            ready.revents
        );
        phase.set("accept");
        let (control, _) = self.listener.accept()?;
        // Darwin inherits O_NONBLOCK from this fixture's listener. Readiness
        // proves a connection is queued, not that the peer has written its
        // identity. SO_RCVTIMEO does not make a nonblocking read wait.
        phase.set("set_blocking");
        control.set_nonblocking(false)?;
        phase.set("set_read_timeout");
        control.set_read_timeout(Some(Duration::from_secs(5)))?;
        phase.set("set_write_timeout");
        control.set_write_timeout(Some(Duration::from_secs(5)))?;
        let mut reader = io::BufReader::new(control);
        let mut identity = String::new();
        phase.set("read_identity");
        if let Some(events) = &self.acquisition_events {
            let _ = events.send("reading");
        }
        reader.read_line(&mut identity).inspect_err(|_| {
            let _ = writeln!(
                io::stderr(),
                "partial worker identity: run={child} bytes={identity:?}"
            );
        })?;
        phase.set("parse_identity");
        let (run, pid) = identity
            .trim()
            .split_once(' ')
            .ok_or_else(|| io::Error::other("worker handshake identity"))?;
        let pid: u32 = pid.parse().map_err(io::Error::other)?;
        let _ = writeln!(
            io::stderr(),
            "owned worker identity: run={child} reported_run={run} pid={pid}"
        );
        phase.set("kernel_identity");
        let (key, parent) = orbit_common::process::ancestry::process_start_and_parent(pid)
            .ok_or_else(|| io::Error::other("worker kernel identity"))?;
        if run != child || parent != std::process::id() {
            return Err(io::Error::other("worker is not this fixture's child"));
        }
        phase.set("bind_process");
        let process = orbit_common::process::signal::BoundProcess::bind(pid, None)?
            .ok_or_else(|| io::Error::other("worker exited before ownership"))?;
        phase.set("recheck_identity");
        if orbit_common::process::ancestry::process_start_and_parent(pid) != Some((key, parent)) {
            return Err(io::Error::other("worker changed during identity binding"));
        }
        self.workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Substitute {
                run: child.to_string(),
                pid,
                process,
                control: reader.into_inner(),
            });
        let _ = writeln!(
            io::stderr(),
            "owned substitute: run={child} pid={pid} parent={parent} start={} root={}",
            key.starttime,
            self.root.path().display()
        );
        Ok(())
    }

    #[cfg(unix)]
    fn worker_pids(&self) -> Vec<u32> {
        self.workers
            .lock()
            .unwrap()
            .iter()
            .map(|worker| worker.pid)
            .collect()
    }

    fn child_runs(&self) -> Vec<String> {
        self.jobs
            .list_job_runs(CHILD)
            .unwrap()
            .into_iter()
            .map(|run| run.run_id)
            .collect()
    }

    fn await_new_child(&self, before: &[String]) -> String {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(child) = self
                .child_runs()
                .into_iter()
                .find(|run_id| !before.contains(run_id))
            {
                return child;
            }
            assert!(Instant::now() < deadline, "the child was never submitted");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[cfg(unix)]
impl Drop for Fixture {
    fn drop(&mut self) {
        // A failed observation can unwind before its settler acquired the
        // substitute. Acquire every submitted child while the root still lives.
        let mut failure = None;
        let children = match self.jobs.list_job_runs(CHILD) {
            Ok(runs) => runs.into_iter().map(|run| run.run_id).collect::<Vec<_>>(),
            Err(error) => {
                self.root.disable_cleanup(true);
                failure = Some(format!("enumerate workers; root retained: {error}"));
                Vec::new()
            }
        };
        for child in children {
            let owned = self
                .workers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .any(|worker| worker.run == child);
            if !owned && let Err(error) = self.own_worker(&child) {
                self.root.disable_cleanup(true);
                failure = Some(format!("owned worker acquisition; root retained: {error}"));
            }
        }
        let workers = self
            .workers
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let deadline = Instant::now() + Duration::from_secs(5);
        let release_deadline = Instant::now() + Duration::from_secs(1);
        for worker in workers.iter_mut() {
            let release = std::fs::write(self.root.path().join(format!("ran-{}", worker.run)), "")
                .map_err(|error| ("write_release_marker", error))
                .and_then(|()| {
                    worker
                        .control
                        .write_all(&[1])
                        .map_err(|error| ("write_release", error))
                });
            if let Err((phase, error)) = release {
                failure = Some(format!(
                    "run={} pid={} phase={phase} kind={:?} raw_os_error={:?} error={error}",
                    worker.run,
                    worker.pid,
                    error.kind(),
                    error.raw_os_error()
                ));
                let _ = worker.process.signal(libc::SIGKILL);
            }
        }
        let mut stopped = false;
        loop {
            if workers.iter().all(|worker| reaped(worker.pid)) {
                break;
            }
            if !stopped && Instant::now() >= release_deadline {
                stopped = true;
                for worker in workers.iter().filter(|worker| !reaped(worker.pid)) {
                    // BoundProcess fails closed on PID reuse, including during
                    // unwinding; it never targets another fixture or a group.
                    if let Err(error) = worker.process.signal(libc::SIGKILL) {
                        failure = Some(format!("stop {}: {error}", worker.run));
                    }
                }
                failure.get_or_insert_with(|| "owned worker did not observe release".to_string());
            }
            if Instant::now() >= deadline {
                self.root.disable_cleanup(true);
                failure = Some("owned worker reap deadline; root retained".to_string());
                break;
            }
            // Bounded predicate observation, not an interleaving based on delay.
            std::thread::park_timeout(Duration::from_millis(20));
        }
        // Reaping a worker does not finish its detached supervisor: the
        // observer thread spawned per worker rereads and settles the run
        // after `wait()` returns. A run can already be terminal before
        // that post-reap work runs, so terminal state is not the
        // supervisor's completion signal. Wait for each worker's own
        // observer completion, recorded only after its post-reap work,
        // before teardown proceeds.
        if failure.is_none() {
            let settle_deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let settled = workers.iter().all(|worker| {
                    orbit_core::test_support::pipeline_worker_observer_completed(&worker.run)
                });
                if settled {
                    break;
                }
                if Instant::now() >= settle_deadline {
                    self.root.disable_cleanup(true);
                    failure = Some("supervisor settlement deadline; root retained".to_string());
                    break;
                }
                std::thread::park_timeout(Duration::from_millis(20));
            }
        }
        for worker in workers.iter() {
            if let Ok(log) = std::fs::read_to_string(
                self.runtime
                    .paths()
                    .logs_dir
                    .join(format!("{}.worker.log", worker.run)),
            ) {
                let _ = writeln!(
                    io::stderr(),
                    "owned substitute log: run={} pid={}\n{log}",
                    worker.run,
                    worker.pid
                );
            }
            let _ = writeln!(
                io::stderr(),
                "owned substitute cleanup: run={} pid={} reaped={} root_present={}",
                worker.run,
                worker.pid,
                reaped(worker.pid),
                self.root.path().exists()
            );
        }
        self.cleaned
            .store(failure.is_none(), std::sync::atomic::Ordering::SeqCst);
        if let Some(error) = failure {
            let _ = writeln!(io::stderr(), "owned substitute cleanup failed: {error}");
            if !std::thread::panicking() {
                panic!("{error}");
            }
        }
    }
}

fn execute(runtime: &OrbitRuntime, child: &str) {
    // The worker reports an unsuccessful run as an error; the parent's wait
    // reads the stored outcome.
    let _ = runtime.execute_pipeline_run_worker(child);
}

fn payload(row: &AuditEvent) -> Value {
    serde_json::from_str(row.arguments_json.as_deref().unwrap()).unwrap()
}

fn baseline_red_text() -> String {
    BaselineRedHold {
        base_ref: "origin/agent-main".to_string(),
        base_sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
        command: "make ci-fast".to_string(),
        run_id: "jrun-observed".to_string(),
        selection: None,
        failures: Default::default(),
    }
    .text("the required check fails on the base exactly as on the candidate")
}

#[test]
fn a_child_wait_row_names_the_child_and_why_and_only_failures_are_incidents() {
    if !isolated(
        "child_wait_audit::a_child_wait_row_names_the_child_and_why_and_only_failures_are_incidents",
    ) {
        return;
    }
    #[cfg(unix)]
    {
        // Hold the real substitute between connect and identity preparation.
        // Listener readiness must not let acquisition fail before it writes.
        let gate = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        gate.set_nonblocking(true).unwrap();
        let mut fixture = Fixture::new();
        fixture.listener = crate::worker_fixture::install_owned_with_identity_gate(
            &fixture.root.path().join("ran-{run_id}"),
            &gate.local_addr().unwrap().to_string(),
        );
        let (events, observed) = std::sync::mpsc::sync_channel(2);
        fixture.acquisition_events = Some(events);
        std::thread::scope(|scope| {
            let preparation = scope.spawn(move || {
                use std::os::fd::AsRawFd;
                let mut ready = libc::pollfd {
                    fd: gate.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one initialized pollfd and a retained listener.
                assert!(unsafe { libc::poll(&mut ready, 1, 5_000) } > 0);
                let (mut permit, _) = gate.accept().unwrap();
                permit.set_nonblocking(false).unwrap();
                permit
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let reading = observed.recv_timeout(Duration::from_secs(5));
                let premature = observed.recv_timeout(Duration::from_millis(100));
                // Always let the worker finish, even when the regression fires.
                permit.write_all(&[1]).unwrap();
                assert_eq!(reading.unwrap(), "reading");
                assert!(
                    matches!(premature, Err(std::sync::mpsc::RecvTimeoutError::Timeout)),
                    "identity acquisition must wait for peer preparation: {premature:?}"
                );
                assert_eq!(
                    observed.recv_timeout(Duration::from_secs(5)).unwrap(),
                    "acquired"
                );
            });
            let (_, row, _) = fixture.observe(json!([]), 60, |runtime, child| {
                runtime.cancel_job_run(child).unwrap();
            });
            assert_eq!(payload(&row)["status"], "cancelled");
            assert_eq!(row.status, AuditEventStatus::Success);
            preparation.join().unwrap();
        });
        let pids = fixture.worker_pids();
        let root = fixture.root.path().to_path_buf();
        let cleaned = fixture.cleaned.clone();
        drop(fixture);
        assert!(cleaned.load(std::sync::atomic::Ordering::SeqCst));
        assert!(pids.iter().all(|pid| reaped(*pid)));
        assert!(!root.exists());
    }
    let fixture = Fixture::new();

    // The fixture guard supplies the leaf's own error without a child run
    // identity; a guard with one would echo that child's typed identity.
    // Held on a red base: the wait entry carries no error, so the reason
    // comes from the child's own hold step.
    let (child, row, _) = fixture.observe(
        json!([{"status": "failed", "error": baseline_red_text()}]),
        60,
        execute,
    );
    let recorded = payload(&row);
    assert_eq!(recorded["status"], "held", "{recorded}");
    assert_eq!(recorded["error"], Value::Null, "{recorded}");
    assert_eq!(
        row.status,
        AuditEventStatus::Success,
        "a held child passes its parent's guard and is not a failed observation"
    );
    assert_eq!(row.error_message, None);
    let message = recorded["message"].as_str().unwrap();
    assert!(
        message.starts_with(&format!("child run {child} status held: baseline_red: ")),
        "{message}"
    );

    // A genuine failure keeps the child's error and stays unexpected.
    let (_, row, _) = fixture.observe(
        json!([{"status": "failed", "error": "cargo test failed"}]),
        60,
        execute,
    );
    assert_eq!(payload(&row)["status"], "failed");
    assert_eq!(row.status, AuditEventStatus::Failure);
    assert!(
        row.error_message
            .as_deref()
            .is_some_and(|message| message.contains("cargo test failed")),
        "{:?}",
        row.error_message
    );
    assert_eq!(
        classify(&row),
        FailureClass::Unexpected,
        "a failed child stays an unexpected incident"
    );

    // An operator cancel ends the parent cancelled, not failed.
    let (child, row, _) = fixture.observe(json!([]), 60, |runtime, child| {
        runtime.cancel_job_run(child).unwrap();
    });
    let recorded = payload(&row);
    assert_eq!(recorded["status"], "cancelled", "{recorded}");
    assert_eq!(row.status, AuditEventStatus::Success);
    assert_eq!(row.error_message, None);
    assert_eq!(
        recorded["message"],
        format!("child run {child} status cancelled: the child recorded no reason"),
        "an operator cancel records no reason, so the row still names the child"
    );

    // The wait's deadline projects a still-pending child as `timeout`. A
    // caller that did not ask for it to be cancelled is told it is still
    // live, so it frees nothing the child uses [ORB-15643].
    let (child, row, output) = fixture.observe(json!([]), 1, |_, _| {});
    assert_eq!(payload(&row)["status"], "timeout");
    assert_eq!(output["child_live"], true, "{output}");
    assert_eq!(row.status, AuditEventStatus::Failure);
    assert_eq!(
        row.error_message.as_deref(),
        Some(
            format!(
                "child run {child} status timeout: no reason recorded; the wait deadline passed \
                 before the child settled"
            )
            .as_str()
        )
    );
    assert_eq!(classify(&row), FailureClass::Unexpected);

    #[cfg(unix)]
    {
        let root = fixture.root.path().to_path_buf();
        let pids = fixture.worker_pids();
        let cleaned = fixture.cleaned.clone();
        // All substitutes are still behind the observation handshake after
        // their markers were written, including the genuine live timeout row.
        assert!(pids.iter().all(|pid| !reaped(*pid)));
        drop(fixture);
        assert!(
            cleaned.load(std::sync::atomic::Ordering::SeqCst),
            "release must succeed without fallback"
        );
        assert!(
            pids.iter().all(|pid| reaped(*pid)),
            "owned workers must be reaped before root teardown"
        );
        assert!(!root.exists());

        // Exercise both error return and assertion unwind in this admitted
        // identity, without a new test, production seam, deadline or sleep.
        for panic in [false, true] {
            let fixture = Fixture::new();
            let _ = fixture.observe(json!([]), 60, |runtime, child| {
                runtime.cancel_job_run(child).unwrap();
            });
            let root = fixture.root.path().to_path_buf();
            let pids = fixture.worker_pids();
            let cleaned = fixture.cleaned.clone();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _fixture = fixture;
                if panic {
                    panic!("injected audit assertion unwind");
                }
                Err::<(), _>("injected audit observation error")
            }));
            assert!(
                cleaned.load(std::sync::atomic::Ordering::SeqCst),
                "error/unwind cleanup must observe release and reap"
            );
            assert_eq!(outcome.is_err(), panic);
            if !panic {
                assert!(outcome.unwrap().is_err());
            }
            assert!(
                pids.iter().all(|pid| reaped(*pid)),
                "error/unwind left an owned live or zombie worker"
            );
            assert!(
                !root.exists(),
                "cleanup finishes before the temporary root is removed"
            );
        }
    }
}
