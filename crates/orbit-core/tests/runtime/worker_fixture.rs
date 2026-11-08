//! Detached substitute workers wait in this process, with a deadline, rather
//! than forking a shell's sleep command on every marker check.

use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

const WAIT_TEST: &str = "worker_fixture::wait_for_release";
const PATH_ENV: &str = "ORBIT_TEST_WORKER_WAIT_PATH";
const MODE_ENV: &str = "ORBIT_TEST_WORKER_WAIT_MODE";
const CONTROL_ENV: &str = "ORBIT_TEST_WORKER_WAIT_CONTROL";
const RUN_ENV: &str = "ORBIT_TEST_WORKER_WAIT_RUN";
const IDENTITY_GATE_ENV: &str = "ORBIT_TEST_WORKER_IDENTITY_GATE";

pub(super) fn install(path: &Path, mode: &str) {
    install_with_control(path, mode, "", "");
}

/// Gate marker observation until the audit fixture releases its owned worker.
/// Other callers retain the original three modes and their polling contracts.
#[cfg(unix)]
pub(super) fn install_owned(path: &Path) -> std::net::TcpListener {
    install_owned_with_identity_gate(path, "")
}

/// Hold identity preparation behind a real socket for the audit regression.
#[cfg(unix)]
pub(super) fn install_owned_with_identity_gate(
    path: &Path,
    identity_gate: &str,
) -> std::net::TcpListener {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    install_with_control(
        path,
        "created",
        &listener.local_addr().unwrap().to_string(),
        identity_gate,
    );
    listener
}

/// Owned substitute workers for a fixture that spawns an unknown number of
/// child runs. A background acceptor records each worker's `(run, pid)`
/// identity from the owned handshake; teardown writes the release marker,
/// releases every accepted worker over its control socket, then waits until
/// each worker is reaped and its supervisor observer has completed, so the
/// caller may delete the marker and temporary root only after no substitute
/// remains. Drop performs the same bounded cleanup on failure/unwind paths.
#[cfg(unix)]
pub(super) struct OwnedWorkers {
    marker: std::path::PathBuf,
    repo: std::path::PathBuf,
    workers: std::sync::Arc<std::sync::Mutex<Vec<OwnedWorker>>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    acceptor: Option<std::thread::JoinHandle<()>>,
    cleaned: bool,
    // Rendezvous for the delayed-reap regression, consumed at most once.
    before_kill_signal: Option<Box<dyn FnOnce() + Send>>,
    after_kill_deadline: Option<Box<dyn FnOnce() + Send>>,
}

#[cfg(unix)]
struct OwnedWorker {
    run: String,
    pid: u32,
    process: Option<orbit_common::process::signal::BoundProcess>,
    control: std::net::TcpStream,
    released: bool,
    kill_at: Option<Instant>,
}

#[cfg(unix)]
fn reaped(pid: u32) -> bool {
    // SAFETY: signal zero neither mutates memory nor delivers a signal.
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(unix)]
pub(super) fn install_owned_workers(path: &Path, repo: std::path::PathBuf) -> OwnedWorkers {
    use std::io::BufRead;
    use std::sync::atomic::Ordering;
    let listener = install_owned(path);
    let workers: std::sync::Arc<std::sync::Mutex<Vec<OwnedWorker>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let acceptor = {
        let workers = workers.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((control, _)) => {
                        let _ = control.set_nonblocking(false);
                        let _ = control.set_read_timeout(Some(Duration::from_secs(10)));
                        let _ = control.set_write_timeout(Some(Duration::from_secs(10)));
                        let mut reader = std::io::BufReader::new(control);
                        let mut identity = String::new();
                        if reader.read_line(&mut identity).is_err() {
                            continue;
                        }
                        let Some((run, pid)) = identity.trim().split_once(' ') else {
                            continue;
                        };
                        let Ok(pid) = pid.parse::<u32>() else {
                            continue;
                        };
                        let process = orbit_common::process::signal::BoundProcess::bind(pid, None)
                            .ok()
                            .flatten();
                        let _ =
                            writeln!(std::io::stderr(), "owned substitute: run={run} pid={pid}");
                        workers
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(OwnedWorker {
                                run: run.to_string(),
                                pid,
                                process,
                                control: reader.into_inner(),
                                released: false,
                                kill_at: None,
                            });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        })
    };
    OwnedWorkers {
        marker: path.to_path_buf(),
        repo,
        workers,
        stop,
        acceptor: Some(acceptor),
        cleaned: false,
        before_kill_signal: None,
        after_kill_deadline: None,
    }
}

#[cfg(unix)]
impl OwnedWorkers {
    /// Success-path teardown: every run in `expected` must have an owned
    /// worker, and every owned worker must be reaped and observed.
    pub(super) fn release_and_wait(&mut self, expected: &[String]) {
        if let Err(error) = self.cleanup(expected, Duration::from_secs(30)) {
            panic!("owned worker teardown failed: {error}");
        }
    }

    fn cleanup(&mut self, expected: &[String], timeout: Duration) -> Result<(), String> {
        use std::io::Write as _;
        use std::sync::atomic::Ordering;
        if self.cleaned {
            return Ok(());
        }
        self.cleaned = true;
        std::fs::write(&self.marker, "")
            .map_err(|error| format!("write release marker: {error}"))?;
        let deadline = Instant::now() + timeout;
        let mut killed = false;
        let mut quiet_since: Option<Instant> = None;
        let mut last_count = 0usize;
        let result = loop {
            // Release workers accepted so far; late arrivals are released
            // on a later pass while the acceptor is still running.
            let mut release_error: Option<String> = None;
            {
                let mut workers = self
                    .workers
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                for worker in workers.iter_mut() {
                    if !worker.released {
                        match worker.control.write_all(&[1]) {
                            Ok(()) => {
                                worker.released = true;
                                worker.kill_at = Some(Instant::now() + Duration::from_secs(2));
                            }
                            Err(error) => {
                                release_error = Some(format!(
                                    "release run={} pid={}: {error}",
                                    worker.run, worker.pid
                                ));
                                break;
                            }
                        }
                    }
                }
                if workers.len() != last_count {
                    last_count = workers.len();
                    quiet_since = None;
                }
            }
            if let Some(error) = release_error {
                break Err(error);
            }
            let snapshot: Vec<(String, u32)> = self
                .workers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .map(|worker| (worker.run.clone(), worker.pid))
                .collect();
            let expected_owned = expected
                .iter()
                .all(|run| snapshot.iter().any(|(owned, _)| owned == run));
            let all_settled = snapshot.iter().all(|(run, pid)| {
                reaped(*pid)
                    && orbit_core::test_support::pipeline_worker_observer_completed(
                        &self.repo, run, *pid,
                    )
            });
            // On the Drop path the expected set is unknown: require a
            // quiet period with no new arrivals before declaring done.
            let quiet_done = if expected.is_empty() {
                if all_settled {
                    let since = quiet_since.get_or_insert_with(Instant::now);
                    since.elapsed() >= Duration::from_millis(500)
                } else {
                    quiet_since = None;
                    false
                }
            } else {
                true
            };
            if expected_owned && all_settled && quiet_done {
                break if killed {
                    Err("owned worker did not observe release".to_string())
                } else {
                    Ok(())
                };
            }
            {
                let workers = self
                    .workers
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                for worker in workers.iter() {
                    if !reaped(worker.pid)
                        && worker.kill_at.is_some_and(|at| Instant::now() >= at)
                        && let Some(process) = &worker.process
                    {
                        // A zombie still has a PID, and Linux accepts a
                        // pidfd signal for it. It already observed release.
                        if matches!(process.is_alive(), Ok(true)) {
                            if let Some(checkpoint) = self.before_kill_signal.take() {
                                checkpoint();
                            }
                            if matches!(process.signal(libc::SIGKILL), Ok(true)) {
                                killed = true;
                            }
                        }
                        if let Some(checkpoint) = self.after_kill_deadline.take() {
                            checkpoint();
                        }
                    }
                }
            }
            if Instant::now() >= deadline {
                break Err(format!(
                    "workers not reaped/observed before deadline: {snapshot:?} expected_owned={expected_owned}"
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        self.stop.store(true, Ordering::SeqCst);
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
        result
    }
}

#[cfg(unix)]
impl Drop for OwnedWorkers {
    fn drop(&mut self) {
        if self.cleaned {
            return;
        }
        if let Err(error) = self.cleanup(&[], Duration::from_secs(10)) {
            let _ = writeln!(std::io::stderr(), "owned worker cleanup failed: {error}");
            if !std::thread::panicking() {
                panic!("owned worker cleanup failed: {error}");
            }
        }
    }
}

/// Drive a submitted worker through both exit windows at the kill deadline.
/// The observer rendezvous holds a real zombie; no scheduler load is needed.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn released_workers_exiting_around_the_kill_deadline_are_not_forced_kills() {
    use orbit_common::process::signal::BoundProcess;
    use orbit_core::test_support::{WorkerReapPhase, install_worker_reap_hook};
    use orbit_types::workflow::JobRunState;
    use std::sync::mpsc;

    if !crate::dispatch_admission::isolated(
        "worker_fixture::released_workers_exiting_around_the_kill_deadline_are_not_forced_kills",
    ) {
        return;
    }
    fn until(mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !ready() {
            assert!(
                Instant::now() < deadline,
                "delayed-reap rendezvous timed out"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    for exited_before_probe in [true, false] {
        let root =
            tempfile::TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let global = root.path().join("global");
        let repo = root.path().join("repo");
        let jobs_dir = global.join("resources/jobs");
        std::fs::create_dir_all(&jobs_dir).unwrap();
        std::fs::create_dir_all(repo.join(".orbit")).unwrap();
        std::fs::write(
            jobs_dir.join("delayed_reap.yaml"),
            "schemaVersion: 2\nkind: Job\nmetadata:\n  name: delayed_reap\nspec:\n  state: enabled\n  steps:\n    - id: nap\n      spec:\n        type: deterministic\n        action: sleep\n        config: {}\n",
        )
        .unwrap();
        let runtime = orbit_core::OrbitRuntime::from_roots(&global, &repo.join(".orbit"))
            .unwrap()
            .with_host_resource_probe(crate::dispatch_admission::PressureProbe::calm());
        let marker = root.path().join("worker-release");
        let mut owned = install_owned_workers(&marker, repo.clone());
        // Control the marker separately so the second case can stay live
        // until the checkpoint between the liveness probe and signal.
        owned.marker = root.path().join("teardown-release");
        let (paused, at_observer) = mpsc::sync_channel(1);
        let (resume, permit_reap) = mpsc::sync_channel(1);
        install_worker_reap_hook(WorkerReapPhase::BeforeReap, move |run| {
            // Disconnection on an assertion unwind also releases the observer.
            if paused.send(run.to_string()).is_ok() {
                let _ = permit_reap.recv_timeout(Duration::from_secs(30));
            }
        });
        let run = runtime
            .submit_pipeline_run("delayed_reap", serde_json::json!({}), None, None)
            .unwrap()
            .run_id;
        assert_eq!(
            at_observer.recv_timeout(Duration::from_secs(30)).unwrap(),
            run
        );
        until(|| !owned.workers.lock().unwrap().is_empty());
        let pid = {
            let mut workers = owned.workers.lock().unwrap();
            let worker = &mut workers[0];
            assert_eq!(worker.run, run);
            worker.control.write_all(&[1]).unwrap();
            worker.released = true;
            worker.kill_at = Some(Instant::now() + Duration::from_secs(2));
            worker.pid
        };
        let process = BoundProcess::bind(pid, None).unwrap().unwrap();
        // The in-process executor has already settled this run, as in the
        // drain tests. Only substitute/observer teardown remains.
        let jobs = orbit_store::compose::workspace_job_run_store(
            runtime.sqlite_store().unwrap(),
            runtime.workspace_id().unwrap(),
        );
        jobs.mark_job_run_running(&run, chrono::Utc::now(), std::process::id())
            .unwrap();
        jobs.finalize_job_run(&run, JobRunState::Success, chrono::Utc::now(), None)
            .unwrap();
        if exited_before_probe {
            std::fs::write(&marker, "").unwrap();
            until(|| !process.is_alive().unwrap());
            assert!(!reaped(pid), "observer must still hold the zombie");
            owned.after_kill_deadline = Some(Box::new(move || {
                assert!(!reaped(pid), "kill deadline must see the unreaped exit");
                resume.send(()).unwrap();
            }));
        } else {
            owned.before_kill_signal = Some(Box::new(move || {
                std::fs::write(marker, "").unwrap();
                until(|| !process.is_alive().unwrap());
                resume.send(()).unwrap();
                until(|| reaped(pid));
                assert!(
                    !process.signal(libc::SIGKILL).unwrap(),
                    "reaped signal result"
                );
            }));
        }
        owned.release_and_wait(std::slice::from_ref(&run));
        assert!(orbit_core::test_support::pipeline_worker_observer_completed(&repo, &run, pid));
        assert_eq!(
            runtime.show_job_run(&run).unwrap().state,
            JobRunState::Success
        );
    }
}

/// Non-Unix fallback: no owned handshake exists there, so retain the
/// original marker-only contract.
#[cfg(not(unix))]
pub(super) struct OwnedWorkers {
    marker: std::path::PathBuf,
}

#[cfg(not(unix))]
pub(super) fn install_owned_workers(path: &Path, _repo: std::path::PathBuf) -> OwnedWorkers {
    install(path, "created");
    OwnedWorkers {
        marker: path.to_path_buf(),
    }
}

#[cfg(not(unix))]
impl OwnedWorkers {
    pub(super) fn release_and_wait(&mut self, _expected: &[String]) {
        std::fs::write(&self.marker, "").unwrap();
    }
}

fn install_with_control(path: &Path, mode: &str, control: &str, identity_gate: &str) {
    orbit_common::test_env::assert_child_test_exists(WAIT_TEST);
    orbit_core::test_support::install_substitute_pipeline_worker([
        "env".to_string(),
        format!("{PATH_ENV}={}", path.display()),
        format!("{MODE_ENV}={mode}"),
        format!("{CONTROL_ENV}={control}"),
        format!("{IDENTITY_GATE_ENV}={identity_gate}"),
        format!("{RUN_ENV}={}", orbit_core::test_support::RUN_ID_PLACEHOLDER),
        std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        "--exact".to_string(),
        WAIT_TEST.to_string(),
        "--ignored".to_string(),
        "--nocapture".to_string(),
    ]);
}

#[test]
#[ignore = "entry point for detached substitute workers"]
fn wait_for_release() {
    let path = std::env::var_os(PATH_ENV).expect("substitute worker wait path");
    let path = std::path::PathBuf::from(path);
    let mode = std::env::var(MODE_ENV).unwrap();
    let run = std::env::var(RUN_ENV).unwrap();
    let lifetime = if mode == "removed" { 60 } else { 120 };
    let deadline = Instant::now() + Duration::from_secs(lifetime);
    let control = std::env::var(CONTROL_ENV).unwrap_or_default();
    let owned = !control.is_empty();
    if owned {
        use std::io::Read;
        fn handshake<T>(run: &str, phase: &str, result: std::io::Result<T>) -> T {
            result.unwrap_or_else(|error| {
                panic!(
                    "owned worker handshake: run={run} pid={} phase={phase} kind={:?} raw_os_error={:?} error={error}",
                    std::process::id(), error.kind(), error.raw_os_error()
                )
            })
        }
        let _ = writeln!(
            std::io::stderr(),
            "worker handshake starting: run={run} pid={}",
            std::process::id()
        );
        let mut control = handshake(
            run.as_str(),
            "connect",
            std::net::TcpStream::connect_timeout(
                &control.parse().unwrap(),
                deadline.saturating_duration_since(Instant::now()),
            ),
        );
        handshake(
            &run,
            "set_read_timeout",
            control.set_read_timeout(Some(deadline.saturating_duration_since(Instant::now()))),
        );
        handshake(
            &run,
            "set_write_timeout",
            control.set_write_timeout(Some(deadline.saturating_duration_since(Instant::now()))),
        );
        let _ = writeln!(
            std::io::stderr(),
            "worker identity preparation: run={run} pid={}",
            std::process::id()
        );
        let gate = std::env::var(IDENTITY_GATE_ENV).unwrap_or_default();
        if !gate.is_empty() {
            let mut gate = handshake(
                &run,
                "connect_identity_gate",
                std::net::TcpStream::connect_timeout(
                    &gate.parse().unwrap(),
                    deadline.saturating_duration_since(Instant::now()),
                ),
            );
            handshake(
                &run,
                "set_identity_gate_timeout",
                gate.set_read_timeout(Some(deadline.saturating_duration_since(Instant::now()))),
            );
            let mut permit = [0];
            handshake(&run, "read_identity_permit", gate.read_exact(&mut permit));
            assert_eq!(permit, [1], "identity preparation permit");
        }
        // The private fixture listener learns the actual substitute's identity,
        // not the in-process executor PID persisted on the child run.
        handshake(
            &run,
            "write_identity",
            writeln!(control, "{run} {}", std::process::id()),
        );
        let mut release = [0];
        handshake(&run, "read_release", control.read_exact(&mut release));
        assert_eq!(release, [1], "owned fixture release");
    }
    loop {
        let released = match mode.as_str() {
            "removed" => !path.exists(),
            "created" => path.exists(),
            "drain" => {
                if path.join("leaves-exit").exists() && !path.join(format!("drain-{run}")).exists()
                {
                    // A leaf exits before claiming; the drain stays alive.
                    std::process::exit(3);
                }
                path.join(format!("started-{run}")).exists()
            }
            _ => panic!("unknown fixture wait mode: {mode}"),
        };
        if released {
            if owned {
                let _ = writeln!(
                    std::io::stderr(),
                    "worker release observed: run={run} pid={} exit=0",
                    std::process::id()
                );
            }
            return;
        }
        assert!(
            Instant::now() < deadline,
            "substitute worker release deadline"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
