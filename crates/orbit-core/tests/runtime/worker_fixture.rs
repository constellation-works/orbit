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
/// Launches count once the parent publishes their record, whether or not the
/// worker has identified itself, so a worker still held there is settled like
/// an identified one. When settling fails, a root given to
/// [`OwnedWorkers::retain_workspace`] is kept.
/// A release marker that cannot be written is reported after the same settling.
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
    // Launches that predate this guard belong to someone else's teardown.
    before_install: Vec<(String, u32)>,
    // This guard's share of the workspace root, kept when teardown fails.
    root: Option<std::sync::Arc<tempfile::TempDir>>,
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
    install_owned_workers_with_identity_gate(path, repo, "")
}

/// As [`install_owned_workers`], with each substitute held at `identity_gate`
/// before its identity line (see [`install_owned_with_identity_gate`]).
#[cfg(unix)]
pub(super) fn install_owned_workers_with_identity_gate(
    path: &Path,
    repo: std::path::PathBuf,
    identity_gate: &str,
) -> OwnedWorkers {
    use std::io::BufRead;
    use std::sync::atomic::Ordering;
    let listener = install_owned_with_identity_gate(path, identity_gate);
    let before_install = orbit_core::test_support::pipeline_workers_spawned(&repo);
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
        before_install,
        root: None,
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

    /// Hand the guard a share of the fixture's workspace root. When teardown
    /// cannot confirm that every worker is reaped and observed, the guard
    /// keeps its share, so the root outlives a worker that may still use it.
    pub(super) fn retain_workspace(&mut self, root: std::sync::Arc<tempfile::TempDir>) {
        self.root = Some(root);
    }

    /// Launches recorded for this workspace since the guard was installed.
    fn launched_since_install(&self) -> Vec<(String, u32)> {
        orbit_core::test_support::pipeline_workers_spawned(&self.repo)
            .into_iter()
            .filter(|launch| !self.before_install.contains(launch))
            .collect()
    }

    fn cleanup(&mut self, expected: &[String], timeout: Duration) -> Result<(), String> {
        use std::io::Write as _;
        use std::sync::atomic::Ordering;
        if self.cleaned {
            return Ok(());
        }
        self.cleaned = true;
        // A marker that cannot be written must not skip settling: the workers
        // still need release, the acceptor still needs joining, and the root
        // still needs its retention decision. The failure is reported last.
        let marker_error = std::fs::write(&self.marker, "")
            .err()
            .map(|error| format!("write release marker: {error}"));
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
            // A launch counts once its record is published, so a worker still
            // held before identifying itself is settled like an identified one.
            let launched = self.launched_since_install();
            let all_settled = snapshot.iter().chain(&launched).all(|(run, pid)| {
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
                    "workers not reaped/observed before deadline: {snapshot:?} launched={launched:?} expected_owned={expected_owned}"
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        self.stop.store(true, Ordering::SeqCst);
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
        if result.is_err()
            && let Some(root) = self.root.take()
        {
            // Never release this share: the root stays while a worker may use it.
            std::mem::forget(root);
        }
        match (marker_error, result) {
            (None, result) => result,
            (Some(marker), Ok(())) => Err(marker),
            (Some(marker), Err(settle)) => Err(format!("{marker}; {settle}")),
        }
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
        let runtime = orbit_core::runtime::OrbitRuntime::from_roots(&global, &repo.join(".orbit"))
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
        let jobs = orbit_core::runtime::JobRuns::shared(
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

/// A substitute launched and held before its identity handshake must keep its
/// workspace through an assertion unwind: teardown cannot confirm its exit, so
/// the guard keeps the root. The test removes that root only after the
/// substitute is reaped and its startup observer has completed.
#[cfg(unix)]
#[test]
fn a_held_pre_handshake_worker_keeps_its_workspace_through_an_assertion_unwind() {
    use orbit_core::runtime::OrbitRuntime;
    use orbit_core::test_support::{pipeline_worker_observer_completed, pipeline_workers_spawned};
    use std::sync::Arc;

    if !crate::dispatch_admission::isolated(
        "worker_fixture::a_held_pre_handshake_worker_keeps_its_workspace_through_an_assertion_unwind",
    ) {
        return;
    }
    fn until(what: &str, mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !ready() {
            assert!(Instant::now() < deadline, "{what} timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    let root =
        Arc::new(tempfile::TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap());
    let root_path = root.path().to_path_buf();
    let global = root_path.join("global");
    let repo = root_path.join("repo");
    let jobs_dir = global.join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).unwrap();
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    std::fs::write(
        jobs_dir.join("held_identity.yaml"),
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: held_identity\nspec:\n  state: enabled\n  steps:\n    - id: nap\n      spec:\n        type: deterministic\n        action: sleep\n        config: {}\n",
    )
    .unwrap();
    let gate = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    gate.set_nonblocking(true).unwrap();
    let gate_addr = gate.local_addr().unwrap().to_string();
    let marker = root_path.join("worker-release");
    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit"))
        .unwrap()
        .with_host_resource_probe(crate::dispatch_admission::PressureProbe::calm());

    let mut held = None;
    let mut launched = None;
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Moved in: the unwind drops the guard, then this share of the root.
        let root = root;
        let mut owned = install_owned_workers_with_identity_gate(&marker, repo.clone(), &gate_addr);
        owned.retain_workspace(root.clone());
        let run = runtime
            .submit_pipeline_run("held_identity", serde_json::json!({}), None, None)
            .unwrap()
            .run_id;
        // The substitute connects to the gate only after its control socket,
        // and it writes no identity until the gate permits it.
        let mut accepted = None;
        until("the substitute reached its identity gate", || {
            gate.accept()
                .map(|(stream, _)| accepted = Some(stream))
                .is_ok()
        });
        held = accepted;
        let pid = pipeline_workers_spawned(&repo)
            .into_iter()
            .find_map(|(recorded, pid)| (recorded == run).then_some(pid))
            .expect("the launch is recorded before submission returns");
        launched = Some((run, pid));
        assert!(
            owned.workers.lock().unwrap().is_empty(),
            "no identity is announced while the substitute is held"
        );
        panic!("assertion unwind while the substitute is held before its identity");
    }));
    assert!(unwound.is_err(), "the unwind reached the guard");
    let (run, pid) = launched.expect("the substitute was launched before the unwind");
    assert!(
        root_path.exists(),
        "an unwind must not delete a workspace whose substitute is still held"
    );
    assert!(!reaped(pid), "the held substitute is still alive");
    assert!(
        !pipeline_worker_observer_completed(&repo, &run, pid),
        "its startup observer is still active"
    );

    // Opening the gate lets the substitute identify itself. Its control socket
    // is closed, so it exits on the release read and its observer reaps it.
    let mut held: std::net::TcpStream = held.expect("the gate connection");
    held.write_all(&[1]).unwrap();
    until("the held substitute is reaped", || reaped(pid));
    until("its startup observer completes", || {
        pipeline_worker_observer_completed(&repo, &run, pid)
    });
    // Nothing uses the root now, so the test removes it; the guard's share
    // was retained, so this is the only deletion.
    std::fs::remove_dir_all(&root_path).unwrap();
}

/// A release marker that cannot be written (a directory stands at its path)
/// must not skip teardown: cleanup reports the I/O failure, joins the acceptor
/// within a bound, and keeps the workspace while the gated substitute and its
/// observer are still active.
#[cfg(unix)]
#[test]
fn an_unwritable_release_marker_still_settles_and_keeps_the_workspace() {
    use orbit_core::runtime::OrbitRuntime;
    use orbit_core::test_support::{pipeline_worker_observer_completed, pipeline_workers_spawned};
    use std::sync::Arc;

    if !crate::dispatch_admission::isolated(
        "worker_fixture::an_unwritable_release_marker_still_settles_and_keeps_the_workspace",
    ) {
        return;
    }
    fn until(what: &str, mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !ready() {
            assert!(Instant::now() < deadline, "{what} timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    let root =
        Arc::new(tempfile::TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap());
    let root_path = root.path().to_path_buf();
    let global = root_path.join("global");
    let repo = root_path.join("repo");
    let jobs_dir = global.join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).unwrap();
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    std::fs::write(
        jobs_dir.join("held_marker.yaml"),
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: held_marker\nspec:\n  state: enabled\n  steps:\n    - id: nap\n      spec:\n        type: deterministic\n        action: sleep\n        config: {}\n",
    )
    .unwrap();
    let gate = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    gate.set_nonblocking(true).unwrap();
    let gate_addr = gate.local_addr().unwrap().to_string();
    // A directory at the marker path makes the marker write fail with EISDIR.
    let marker = root_path.join("worker-release");
    std::fs::create_dir(&marker).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit"))
        .unwrap()
        .with_host_resource_probe(crate::dispatch_admission::PressureProbe::calm());

    let mut owned = install_owned_workers_with_identity_gate(&marker, repo.clone(), &gate_addr);
    owned.retain_workspace(root.clone());
    let run = runtime
        .submit_pipeline_run("held_marker", serde_json::json!({}), None, None)
        .unwrap()
        .run_id;
    let mut held = None;
    until("the substitute reached its identity gate", || {
        gate.accept().map(|(stream, _)| held = Some(stream)).is_ok()
    });
    let pid = pipeline_workers_spawned(&repo)
        .into_iter()
        .find_map(|(recorded, pid)| (recorded == run).then_some(pid))
        .expect("the launch is recorded before submission returns");

    let started = Instant::now();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        owned.release_and_wait(std::slice::from_ref(&run));
    }))
    .expect_err("an unwritable marker is reported to the caller");
    let error = panic.downcast_ref::<String>().expect("teardown panic text");
    assert!(
        error.contains("write release marker"),
        "the marker failure is reported: {error}"
    );
    assert!(
        error.contains("before deadline"),
        "the held worker is still unsettled: {error}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(45),
        "teardown is bounded: {:?}",
        started.elapsed()
    );
    drop(owned);
    drop(runtime);
    drop(root);
    assert!(root_path.exists(), "the workspace outlives the held worker");
    assert!(!reaped(pid), "the held substitute is still alive");
    assert!(
        !pipeline_worker_observer_completed(&repo, &run, pid),
        "its startup observer is still active"
    );

    // Opening the gate lets the substitute continue; its control socket is
    // closed, so it exits and its observer reaps it.
    let mut held: std::net::TcpStream = held.expect("the gate connection");
    held.write_all(&[1]).unwrap();
    until("the held substitute is reaped", || reaped(pid));
    until("its startup observer completes", || {
        pipeline_worker_observer_completed(&repo, &run, pid)
    });
    std::fs::remove_dir_all(&root_path).unwrap();
}

/// The ownership shape of the drain fixtures: the fixture struct owns the
/// workspace root as an `Arc`, and the guard, declared after it, holds a
/// second share. Locals drop in reverse order, so the guard settles first.
#[cfg(unix)]
struct CallerWorkspace {
    root: std::sync::Arc<tempfile::TempDir>,
    repo: std::path::PathBuf,
    runtime: orbit_core::runtime::OrbitRuntime,
}

#[cfg(unix)]
impl CallerWorkspace {
    fn new(job: &str) -> Self {
        let root = std::sync::Arc::new(
            tempfile::TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap(),
        );
        let global = root.path().join("global");
        let repo = root.path().join("repo");
        let jobs_dir = global.join("resources/jobs");
        std::fs::create_dir_all(&jobs_dir).unwrap();
        std::fs::create_dir_all(repo.join(".orbit")).unwrap();
        std::fs::write(
            jobs_dir.join(format!("{job}.yaml")),
            format!(
                "schemaVersion: 2\nkind: Job\nmetadata:\n  name: {job}\nspec:\n  state: enabled\n  steps:\n    - id: nap\n      spec:\n        type: deterministic\n        action: sleep\n        config: {{}}\n"
            ),
        )
        .unwrap();
        let runtime = orbit_core::runtime::OrbitRuntime::from_roots(&global, &repo.join(".orbit"))
            .unwrap()
            .with_host_resource_probe(crate::dispatch_admission::PressureProbe::calm());
        Self {
            root,
            repo,
            runtime,
        }
    }
}

#[cfg(unix)]
fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready() {
        assert!(Instant::now() < deadline, "{what} timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A fixture that owns its root, as the drain fixtures do, keeps it through
/// an assertion unwind while a held substitute may still use it. Only the
/// guard's retained share survives the fixture's own drop.
#[cfg(unix)]
#[test]
fn a_fixture_owned_root_survives_an_unwind_with_a_held_worker() {
    use orbit_core::test_support::{pipeline_worker_observer_completed, pipeline_workers_spawned};

    if !crate::dispatch_admission::isolated(
        "worker_fixture::a_fixture_owned_root_survives_an_unwind_with_a_held_worker",
    ) {
        return;
    }
    let gate = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    gate.set_nonblocking(true).unwrap();
    let gate_addr = gate.local_addr().unwrap().to_string();

    let mut root_path = None;
    let mut held = None;
    let mut launched = None;
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let fixture = CallerWorkspace::new("held_fixture_root");
        root_path = Some(fixture.root.path().to_path_buf());
        let marker = fixture.root.path().join("release-workers");
        let mut owned =
            install_owned_workers_with_identity_gate(&marker, fixture.repo.clone(), &gate_addr);
        owned.retain_workspace(fixture.root.clone());
        let run = fixture
            .runtime
            .submit_pipeline_run("held_fixture_root", serde_json::json!({}), None, None)
            .unwrap()
            .run_id;
        let mut accepted = None;
        until("the substitute reached its identity gate", || {
            gate.accept()
                .map(|(stream, _)| accepted = Some(stream))
                .is_ok()
        });
        held = accepted;
        let pid = pipeline_workers_spawned(&fixture.repo)
            .into_iter()
            .find_map(|(recorded, pid)| (recorded == run).then_some(pid))
            .expect("the launch is recorded before submission returns");
        launched = Some((run, fixture.repo.clone(), pid));
        panic!("assertion unwind while the substitute is held before its identity");
    }));
    assert!(unwound.is_err(), "the unwind reached the guard");
    let root_path = root_path.expect("the fixture was built");
    let (run, repo, pid) = launched.expect("the substitute was launched before the unwind");
    assert!(
        root_path.exists(),
        "the fixture's drop must not delete a root a held substitute may use"
    );
    assert!(!reaped(pid), "the held substitute is still alive");
    assert!(
        !pipeline_worker_observer_completed(&repo, &run, pid),
        "its startup observer is still active"
    );

    let mut held: std::net::TcpStream = held.expect("the gate connection");
    held.write_all(&[1]).unwrap();
    until("the held substitute is reaped", || reaped(pid));
    until("its startup observer completes", || {
        pipeline_worker_observer_completed(&repo, &run, pid)
    });
    std::fs::remove_dir_all(&root_path).unwrap();
}

/// Ordinary teardown settles every worker, so the guard's share is released
/// with it and the fixture's drop removes the root.
#[cfg(unix)]
#[test]
fn ordinary_teardown_releases_a_fixture_owned_root() {
    use orbit_core::test_support::pipeline_worker_observer_completed;
    use orbit_types::workflow::JobRunState;

    if !crate::dispatch_admission::isolated(
        "worker_fixture::ordinary_teardown_releases_a_fixture_owned_root",
    ) {
        return;
    }
    let fixture = CallerWorkspace::new("ordinary_fixture_root");
    let root_path = fixture.root.path().to_path_buf();
    let marker = root_path.join("release-workers");
    let mut owned = install_owned_workers(&marker, fixture.repo.clone());
    owned.retain_workspace(fixture.root.clone());
    let run = fixture
        .runtime
        .submit_pipeline_run("ordinary_fixture_root", serde_json::json!({}), None, None)
        .unwrap()
        .run_id;
    until("the substitute identified itself", || {
        !owned.workers.lock().unwrap().is_empty()
    });
    let pid = owned.workers.lock().unwrap()[0].pid;
    // The in-process executor would settle the run; only substitute and
    // observer teardown remain.
    let jobs = orbit_core::runtime::JobRuns::shared(
        fixture.runtime.sqlite_store().unwrap(),
        fixture.runtime.workspace_id().unwrap(),
    );
    jobs.mark_job_run_running(&run, chrono::Utc::now(), std::process::id())
        .unwrap();
    jobs.finalize_job_run(&run, JobRunState::Success, chrono::Utc::now(), None)
        .unwrap();
    owned.release_and_wait(std::slice::from_ref(&run));
    assert!(reaped(pid));
    assert!(pipeline_worker_observer_completed(&fixture.repo, &run, pid));
    assert!(root_path.exists(), "the fixture still owns the root");
    assert!(
        std::sync::Arc::strong_count(&fixture.root) > 1,
        "the guard still holds its share until it drops"
    );
    drop(owned);
    assert_eq!(
        std::sync::Arc::strong_count(&fixture.root),
        1,
        "settled teardown gives its share back"
    );
    drop(fixture);
    assert!(!root_path.exists(), "ordinary teardown removes the root");
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

    /// No launch record exists here to confirm an exit, so the root is not
    /// retained: the non-Unix contract stays marker-only.
    pub(super) fn retain_workspace(&mut self, _root: std::sync::Arc<tempfile::TempDir>) {}
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
