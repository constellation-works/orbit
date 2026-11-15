//! [ORB-17179] An interrupted run names how its worker process ended.
//!
//! Stale-run reconciliation interrupts a run whose recorded worker is gone
//! (`process_not_found`), but only the worker's launching supervisor can reap
//! its exit status. A signalled worker's run names the signal whichever of
//! the two settles the run first, and a worker nobody reaped says that no
//! exit status was observed, with no scope evidence invented where the worker
//! ran in no systemd scope.
//!
//! [ORB-18166] A worker's run names who sent the signal that killed it when
//! the worker captured the sender, and says the sender is unknown otherwise.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_core::OrbitRuntime;
use orbit_core::test_support::{
    WorkerReapPhase, install_substitute_pipeline_worker, install_worker_reap_hook,
    pipeline_worker_observer_completed,
};
use orbit_store::contracts::JobRunStoreBackend;
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::json;
use tempfile::TempDir;

use super::dispatch_admission::isolated;

const JOB: &str = "worker_exit_fixture";
const DEADLINE: Duration = Duration::from_secs(30);

struct Fixture {
    root: TempDir,
    runtime: OrbitRuntime,
    jobs: Arc<dyn JobRunStoreBackend>,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let global = root.path().join("home/.orbit");
        let repo = root.path().join("repo");
        let jobs_dir = global.join("resources/jobs");
        std::fs::create_dir_all(&jobs_dir).unwrap();
        std::fs::create_dir_all(repo.join(".orbit")).unwrap();
        std::fs::write(
            jobs_dir.join(format!("{JOB}.yaml")),
            format!(
                "schemaVersion: 2\nkind: Job\nmetadata:\n  name: {JOB}\nspec:\n  state: enabled\n  kind: workflow\n  steps:\n    - id: nap\n      default_input:\n        seconds: 60\n      spec:\n        type: deterministic\n        action: sleep\n        config: {{}}\n"
            ),
        )
        .unwrap();
        let runtime = runtime_at(root.path());
        let jobs = orbit_store::compose::workspace_job_run_store(
            runtime.sqlite_store().unwrap(),
            runtime.workspace_id().unwrap(),
        );
        Self {
            root,
            runtime,
            jobs,
        }
    }

    #[cfg(target_os = "linux")]
    fn running_pid(&self, run_id: &str) -> u32 {
        wait_for("the worker to start its run", || {
            let run = self.jobs.get_job_run(run_id).unwrap()?;
            (run.state == JobRunState::Running)
                .then_some(run.pid)
                .flatten()
        })
    }

    fn pid_file(&self, run_id: &str) -> PathBuf {
        self.root.path().join(format!("{run_id}.pid"))
    }

    /// The run's terminal diagnostic, as `orbit run show` reads it.
    fn diagnostic(&self, run: &JobRun) -> String {
        run.steps
            .iter()
            .find_map(|step| step.error_message.clone())
            .unwrap_or_else(|| panic!("run {} has no diagnostic: {run:?}", run.run_id))
    }
}

/// The fixture's runtime over the roots under `root`.
fn runtime_at(root: &Path) -> OrbitRuntime {
    OrbitRuntime::from_roots(&root.join("home/.orbit"), &root.join("repo/.orbit"))
        .unwrap()
        .with_host_resource_probe(crate::dispatch_admission::PressureProbe::calm())
}

fn wait_for<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn kill(pid: u32) {
    // SAFETY: the fixture owns `pid`; SIGKILL only ends that process.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) }, 0);
}

/// Submit a run whose substitute worker claims it, kill the worker with
/// SIGKILL, and let reconciliation interrupt the run while the supervisor is
/// held at `phase` just after reaping. Returns the run's final diagnostic.
fn killed_worker_reconciled_at(phase: WorkerReapPhase) -> String {
    let fixture = Fixture::new();
    // `exec` keeps the shell's PID, so the recorded PID is the worker the
    // supervisor spawned and reaps.
    install_substitute_pipeline_worker([
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!(
            "echo $$ > '{}'; exec sleep 60",
            fixture
                .pid_file(orbit_core::test_support::RUN_ID_PLACEHOLDER)
                .display()
        ),
    ]);
    let run_id = fixture
        .runtime
        .submit_job_run(JOB, json!({}), Some("test"))
        .unwrap()
        .run_id;
    let pid: u32 = wait_for("the worker's pid", || {
        std::fs::read_to_string(fixture.pid_file(&run_id))
            .ok()
            .and_then(|pid| pid.trim().parse().ok())
    });
    // The worker owns a running run, past the owner startup grace.
    fixture
        .jobs
        .mark_job_run_running(&run_id, Utc::now() - chrono::Duration::minutes(1), pid)
        .unwrap();

    let (reaped, at_phase) = mpsc::sync_channel::<String>(1);
    let (settled, resume) = mpsc::sync_channel::<()>(1);
    install_worker_reap_hook(phase, move |run_id| {
        reaped.send(run_id.to_string()).unwrap();
        resume.recv_timeout(DEADLINE).unwrap();
    });
    kill(pid);
    assert_eq!(at_phase.recv_timeout(DEADLINE).unwrap(), run_id);

    // Reconciliation, as any reader of the run performs it, wins the race.
    let reconciled = fixture.runtime.show_job_run(&run_id).unwrap();
    assert_eq!(reconciled.state, JobRunState::Interrupted, "{reconciled:?}");
    settled.send(()).unwrap();

    let workspace: &Path = &fixture.runtime.paths().repo_root;
    wait_for("the supervisor's settlement", || {
        pipeline_worker_observer_completed(workspace, &run_id, pid).then_some(())
    });
    let run = fixture.runtime.show_job_run(&run_id).unwrap();
    assert_eq!(run.state, JobRunState::Interrupted, "{run:?}");
    let diagnostic = fixture.diagnostic(&run);
    assert!(
        diagnostic.contains("reason=process_not_found"),
        "reconciliation interrupted the run: {diagnostic}"
    );
    assert!(
        diagnostic.contains(&format!("reaped pid {pid} at"))
            && diagnostic.contains(&format!(
                "killed by signal {} (SIGKILL), sender unknown: the worker captured none",
                libc::SIGKILL
            )),
        "the interrupted run names the signal that ended its worker, and invents no \
         sender for a SIGKILL nothing can catch: {diagnostic}"
    );
    diagnostic
}

#[test]
fn a_reconciled_run_names_the_signal_that_killed_its_worker() {
    if !isolated("worker_exit_diagnostic::a_reconciled_run_names_the_signal_that_killed_its_worker")
    {
        return;
    }
    // Reconciliation read the exit the supervisor had already recorded.
    let recorded_first = killed_worker_reconciled_at(WorkerReapPhase::AfterExitRecorded);
    assert!(
        recorded_first.contains("worker exit: its supervisor reaped pid")
            && !recorded_first.contains("no supervisor observed"),
        "{recorded_first}"
    );
    // Reconciliation won before the exit was recorded; the supervisor that
    // reaped the worker adds what it observed.
    let reaped_after = killed_worker_reconciled_at(WorkerReapPhase::BeforeExitRecorded);
    assert!(
        reaped_after.contains("worker exit: no supervisor observed or recorded its exit status")
            && reaped_after.contains("afterwards its supervisor reaped pid"),
        "{reaped_after}"
    );
}

#[test]
fn a_reconciled_run_whose_worker_exit_nobody_observed_says_so() {
    if !isolated(
        "worker_exit_diagnostic::a_reconciled_run_whose_worker_exit_nobody_observed_says_so",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let run = fixture
        .jobs
        .insert_job_run(JOB, 1, Utc::now(), Some(json!({})), None)
        .unwrap();
    // A worker no Orbit supervisor launched, reaped by someone else.
    let mut worker = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    fixture
        .jobs
        .mark_job_run_running(
            &run.run_id,
            Utc::now() - chrono::Duration::minutes(1),
            worker.id(),
        )
        .unwrap();
    worker.kill().unwrap();
    worker.wait().unwrap();

    let run = fixture.runtime.show_job_run(&run.run_id).unwrap();
    assert_eq!(run.state, JobRunState::Interrupted, "{run:?}");
    let diagnostic = fixture.diagnostic(&run);
    assert!(
        diagnostic.contains("reason=process_not_found")
            && diagnostic.contains(
                "worker exit: no supervisor observed or recorded its exit status; \
                 no worker scope evidence is available"
            ),
        "an unobserved exit says so, and invents no scope evidence: {diagnostic}"
    );
}

/// Where [`signalled_worker`] finds the fixture root and the run it executes.
#[cfg(target_os = "linux")]
const SIGNALLED_WORKER_ROOT: &str = "ORBIT_TEST_SIGNALLED_WORKER_ROOT";
#[cfg(target_os = "linux")]
const SIGNALLED_WORKER_RUN: &str = "ORBIT_TEST_SIGNALLED_WORKER_RUN";

/// Not a check on its own: the substitute pipeline worker of
/// [`a_terminated_run_names_who_sent_the_signal`] re-executes this binary
/// here, so the worker runs Orbit's own worker entry point.
#[cfg(target_os = "linux")]
#[test]
fn signalled_worker() {
    let (Ok(root), Ok(run_id)) = (
        std::env::var(SIGNALLED_WORKER_ROOT),
        std::env::var(SIGNALLED_WORKER_RUN),
    ) else {
        return;
    };
    let _ = runtime_at(Path::new(&root)).execute_pipeline_run_worker(&run_id);
}

#[cfg(target_os = "linux")]
#[test]
fn a_terminated_run_names_who_sent_the_signal() {
    if !isolated("worker_exit_diagnostic::a_terminated_run_names_who_sent_the_signal") {
        return;
    }
    let fixture = Fixture::new();
    // `env` execs, so the recorded PID is the worker the supervisor reaps.
    install_substitute_pipeline_worker([
        "/usr/bin/env".to_string(),
        format!("{SIGNALLED_WORKER_ROOT}={}", fixture.root.path().display()),
        format!(
            "{SIGNALLED_WORKER_RUN}={}",
            orbit_core::test_support::RUN_ID_PLACEHOLDER
        ),
        std::env::current_exe().unwrap().display().to_string(),
        "--exact".to_string(),
        "worker_exit_diagnostic::signalled_worker".to_string(),
        "--nocapture".to_string(),
        "--test-threads=1".to_string(),
    ]);
    let run_id = fixture
        .runtime
        .submit_job_run(JOB, json!({}), Some("test"))
        .unwrap()
        .run_id;
    let pid = fixture.running_pid(&run_id);

    // No cancellation request: this process is the unexplained sender.
    // SAFETY: the fixture owns `pid`; SIGTERM only ends that process.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) }, 0);
    let workspace: &Path = &fixture.runtime.paths().repo_root;
    wait_for("the supervisor's settlement", || {
        pipeline_worker_observer_completed(workspace, &run_id, pid).then_some(())
    });

    let sender = std::process::id();
    // SAFETY: `getuid` only reads this process's credentials.
    let uid = unsafe { libc::getuid() };
    let command = std::fs::read_to_string("/proc/self/comm").unwrap();
    let command = command.trim();
    let run = fixture.runtime.show_job_run(&run_id).unwrap();
    assert_eq!(run.state, JobRunState::Interrupted, "{run:?}");
    let step = run
        .steps
        .iter()
        .find(|step| step.error_message.is_some())
        .unwrap_or_else(|| panic!("run {run_id} has no diagnostic: {run:?}"));
    assert_eq!(
        step.error_code.as_deref(),
        Some("worker_terminated"),
        "{run:?}"
    );
    let diagnostic = step.error_message.as_deref().unwrap();
    assert!(
        diagnostic.contains(&format!(
            "exited with status signal: {} (SIGTERM) (sent by pid {sender} (uid {uid}, command \
             '{command}'))",
            libc::SIGTERM
        )),
        "the run names the SIGTERM's sender: {diagnostic}"
    );

    let exit = fixture
        .runtime
        .list_audit_events(None, Some("pipeline.worker.exited".into()), None, None, 20)
        .unwrap()
        .into_iter()
        .find(|row| row.target_id.as_deref() == Some(run_id.as_str()))
        .expect("the supervisor recorded the worker's exit");
    let exit: serde_json::Value =
        serde_json::from_str(exit.arguments_json.as_deref().unwrap()).unwrap();
    assert_eq!(exit["worker_pid"], pid, "{exit}");
    assert_eq!(
        exit["signal"],
        libc::SIGTERM,
        "the worker still died from the signal: {exit}"
    );
    assert_eq!(exit["signal_sender"]["pid"], sender, "{exit}");
    assert_eq!(exit["signal_sender"]["uid"], uid, "{exit}");
    assert_eq!(exit["signal_sender"]["command"], command, "{exit}");
}

/// The submitting process need not survive a detached worker. Reconcile a
/// real worker reaped outside Orbit, after its scope and supervisor evidence
/// are unavailable, using the worker's own durable lifecycle and signal log.
#[cfg(target_os = "linux")]
#[test]
fn reconciliation_keeps_worker_provenance_without_a_supervisor_exit_row() {
    if !isolated(
        "worker_exit_diagnostic::reconciliation_keeps_worker_provenance_without_a_supervisor_exit_row",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let run_id = fixture
        .jobs
        .insert_job_run(JOB, 1, Utc::now(), Some(json!({})), None)
        .unwrap()
        .run_id;
    std::fs::create_dir_all(&fixture.runtime.paths().logs_dir).unwrap();
    let log = std::fs::OpenOptions::new()
        .create_new(true)
        .append(true)
        .open(
            fixture
                .runtime
                .paths()
                .logs_dir
                .join(format!("{run_id}.worker.log")),
        )
        .unwrap();
    use std::os::unix::process::CommandExt;
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "worker_exit_diagnostic::signalled_worker",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(SIGNALLED_WORKER_ROOT, fixture.root.path())
        .env(SIGNALLED_WORKER_RUN, &run_id)
        .process_group(0)
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .unwrap();
    let mut worker = orbit_common::process::OwnedChild::from_spawned(child);
    let pid = fixture.running_pid(&run_id);
    assert_eq!(worker.pid(), pid);
    // The entry record should not be lost behind a large log either.
    use std::io::Write;
    let mut log = std::fs::OpenOptions::new()
        .append(true)
        .open(
            fixture
                .runtime
                .paths()
                .logs_dir
                .join(format!("{run_id}.worker.log")),
        )
        .unwrap();
    log.write_all(&vec![b' '; 32 * 1024]).unwrap();
    log.write_all(b"\n").unwrap();
    worker.signal_group(libc::SIGTERM).unwrap();
    let status = wait_for("the unobserved worker's exit", || {
        worker.child_mut().try_wait().unwrap()
    });
    assert!(!status.success());
    worker.mark_reaped();
    let run = wait_for("orphan reconciliation after startup grace", || {
        let run = fixture.runtime.show_job_run(&run_id).unwrap();
        run.state.is_terminal().then_some(run)
    });
    assert_eq!(run.state, JobRunState::Interrupted);
    let diagnostic = fixture.diagnostic(&run);
    assert!(
        diagnostic.contains("no supervisor observed or recorded its exit status"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains(&format!("worker pid {pid} entered at")),
        "worker lifetime must survive loss of its reaper: {diagnostic}"
    );
    assert!(
        diagnostic.contains(&format!("parent pid {}", std::process::id())),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains(&format!(
            "signal=15 worker_pid={pid} sender_pid={}",
            std::process::id()
        )),
        "captured signal provenance must reach the run failure: {diagnostic}"
    );
    assert_eq!(
        run.steps.iter().find_map(|step| step.error_code.as_deref()),
        Some("worker_terminated")
    );
}

/// An exit audit is best-effort during a store outage. Remove the isolated
/// fixture's audit after the reap, before reconciliation, to reproduce the
/// same missing-row boundary without creating host load or lock contention.
#[cfg(target_os = "linux")]
#[test]
fn reconciliation_recovers_a_reaped_status_when_the_exit_audit_is_missing() {
    if !isolated(
        "worker_exit_diagnostic::reconciliation_recovers_a_reaped_status_when_the_exit_audit_is_missing",
    ) {
        return;
    }
    let fixture = Fixture::new();
    install_substitute_pipeline_worker([
        "/usr/bin/env".to_string(),
        format!("{SIGNALLED_WORKER_ROOT}={}", fixture.root.path().display()),
        format!(
            "{SIGNALLED_WORKER_RUN}={}",
            orbit_core::test_support::RUN_ID_PLACEHOLDER
        ),
        std::env::current_exe().unwrap().display().to_string(),
        "--exact".to_string(),
        "worker_exit_diagnostic::signalled_worker".to_string(),
        "--nocapture".to_string(),
        "--test-threads=1".to_string(),
    ]);
    let (at_reap, reaped) = mpsc::sync_channel(1);
    let (resume, continue_observer) = mpsc::sync_channel(1);
    install_worker_reap_hook(WorkerReapPhase::AfterExitRecorded, move |_| {
        at_reap.send(()).unwrap();
        let _ = continue_observer.recv_timeout(DEADLINE);
    });
    let run_id = fixture
        .runtime
        .submit_job_run(JOB, json!({}), None)
        .unwrap()
        .run_id;
    let pid = fixture.running_pid(&run_id);
    kill(pid);
    reaped.recv_timeout(DEADLINE).unwrap();
    assert!(
        fixture
            .runtime
            .sqlite_store()
            .unwrap()
            .prune_audit_events(&(Utc::now() + chrono::Duration::seconds(1)))
            .unwrap()
            > 0
    );
    let reconciled = wait_for("reconciliation with a missing exit audit", || {
        let run = fixture.runtime.show_job_run(&run_id).unwrap();
        run.state.is_terminal().then_some(run)
    });
    resume.send(()).unwrap();
    wait_for("the supervisor's settlement", || {
        pipeline_worker_observer_completed(&fixture.runtime.paths().repo_root, &run_id, pid)
            .then_some(())
    });
    let diagnostic = fixture.diagnostic(&reconciled);
    assert!(
        diagnostic.contains("recovered from worker log; exit audit unavailable"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains(&format!("reaped pid {pid} at")) && diagnostic.contains("SIGKILL"),
        "{diagnostic}"
    );
    assert!(
        !diagnostic.contains("no supervisor observed or recorded its exit status"),
        "{diagnostic}"
    );
    assert_eq!(reconciled.state, JobRunState::Interrupted);
    assert_eq!(
        reconciled
            .steps
            .iter()
            .find_map(|step| step.error_code.as_deref()),
        Some("worker_terminated")
    );
}
