//! Who may delegate read-only exploration through `orbit.agent.invoke`, and
//! inside which bounds [ORB-16814].
//!
//! A managed implement run calls the tool with the managed-run environment
//! its MCP server inherits. That call gets a sandboxed explorer from the
//! explorer crew pool in the caller's own worktree, never a trusted-host
//! admission; every refusal is a typed error that leaves no run behind.
//! Each row runs in its own child process because it sets that environment.

#[cfg(unix)]
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::sync::{Arc, Mutex};
#[cfg(unix)]
use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use orbit_types::workflow::activity_job::TRUSTED_HOST_ADMISSION_KEY;
use orbit_types::workflow::{JobRun, JobRunState, PipelineState};
use serde_json::{Value, json};
use tempfile::TempDir;

use orbit_core::OrbitRuntime;
use orbit_core::application::job::{
    AgentInvocation, AgentInvokeRequest, AgentInvokeResult, EXPLORE_JOB_ID, ExploreSubmission,
    agent_invoke_result,
};

use super::review_support::git;
use super::store_support::jobs;

const CREWS: &str = r#"
[workflow]
default_crew = "opus"

[workflow.purpose_crews]
explorer = ["haiku"]

[crews.haiku]
provider = "claude"
model = "claude-haiku-4-5"
backend = "cli"

[crews.opus]
provider = "claude"
model = "claude-opus-4-6"
backend = "cli"
"#;

/// Two eligible explorers, so a retry can name the other member or omit
/// `crew` and redraw the pool.
const TWO_EXPLORER_CREWS: &str = r#"
[workflow]
default_crew = "opus"

[workflow.purpose_crews]
explorer = ["haiku", "sonnet"]

[crews.haiku]
provider = "claude"
model = "claude-haiku-4-5"
backend = "cli"

[crews.sonnet]
provider = "claude"
model = "claude-sonnet-4-6"
backend = "cli"

[crews.opus]
provider = "claude"
model = "claude-opus-4-6"
backend = "cli"
"#;

const BUDGETS: &str = r#"
[workflow.explore]
enabled = true
max_calls_per_run = 2
max_concurrent_per_run = 1
timeout_seconds = 300
"#;

struct Fixture {
    runtime: OrbitRuntime,
    repo: PathBuf,
    parent: JobRun,
    worktree: PathBuf,
    /// Shared marker. A substitute observes it only after Drop releases its handshake.
    worker_release: PathBuf,
    #[cfg(unix)]
    listener: std::net::TcpListener,
    #[cfg(unix)]
    workers: Mutex<Vec<Substitute>>,
    /// Set only after release was observed, the substitute was reaped, and the
    /// supervisor's observer finished. `root` is removed after `Drop::drop`.
    #[cfg(unix)]
    cleaned: Arc<AtomicBool>,
    // Fields drop in declaration order after `Drop::drop`. `root` stays last
    // so the directory outlives the workers Drop reaps.
    root: TempDir,
}

#[cfg(unix)]
struct Substitute {
    run: String,
    pid: u32,
    starttime: u64,
    process: orbit_common::process::signal::BoundProcess,
    control: std::net::TcpStream,
}

#[cfg(unix)]
#[derive(Clone)]
struct HeldWorker {
    run: String,
    pid: u32,
    starttime: u64,
}

/// Stops a substitute an assertion left alive. A reaped pid, or a pid whose
/// start time no longer matches, is left alone.
#[cfg(unix)]
struct StopSurvivors(Vec<HeldWorker>);

#[cfg(unix)]
impl Drop for StopSurvivors {
    fn drop(&mut self) {
        for worker in &self.0 {
            let still_ours = orbit_common::process::ancestry::process_start_and_parent(worker.pid)
                .is_some_and(|(key, parent)| {
                    key.starttime == worker.starttime && parent == std::process::id()
                });
            if still_ours {
                // SAFETY: the pid and start time still name the substitute this
                // fixture spawned, so the signal cannot land on a reused pid.
                unsafe { libc::kill(worker.pid as libc::pid_t, libc::SIGKILL) };
            }
        }
    }
}

#[cfg(unix)]
fn reaped(pid: u32) -> bool {
    // Signal zero is observational and, unlike process_is_alive, includes
    // zombies. ESRCH therefore proves the supervisor has reaped the child.
    // SAFETY: signal zero neither mutates memory nor delivers a signal.
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    result == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

impl Drop for Fixture {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let mut failure = None;
            if let Err(error) = self.own_outstanding() {
                self.root.disable_cleanup(true);
                failure = Some(format!("owned worker acquisition; root retained: {error}"));
            }
            if failure.is_none()
                && let Err(error) = self.release_reap_and_settle()
            {
                self.root.disable_cleanup(true);
                failure = Some(error);
            }
            if let Some(error) = &failure {
                self.log_owned_workers();
                let _ = writeln!(io::stderr(), "owned substitute cleanup failed: {error}");
            }
            self.cleaned.store(failure.is_none(), Ordering::SeqCst);
            if let Some(error) = failure
                && !std::thread::panicking()
            {
                panic!("{error}");
            }
        }
        #[cfg(not(unix))]
        {
            let _ = std::fs::write(&self.worker_release, b"");
        }
    }
}

/// A git workspace whose implement run is live in its own linked worktree,
/// with `explore` as the workspace's `[workflow.explore]` table.
fn fixture(explore: &str) -> Fixture {
    fixture_with(CREWS, explore)
}

fn fixture_with(crews: &str, explore: &str) -> Fixture {
    let (root, runtime, repo) =
        super::engine_support::runtime_with_workspace_config(Some(&format!("{crews}\n{explore}")));
    // The default sampler writes under this root and has no join handle, so
    // it can recreate the tree while `root` is removed. A calm probe does not.
    let runtime =
        runtime.with_host_resource_probe(crate::dispatch_admission::PressureProbe::calm());
    orbit_core::bootstrap::init::init_workspace_at_root(
        &runtime.global_root(),
        orbit_core::bootstrap::init::InitOptions {
            global_only: true,
            ..Default::default()
        },
    )
    .expect("seed the shipped job catalog");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Orbit Test"]);
    git(&repo, &["config", "user.email", "orbit-test@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join(".gitignore"), ".orbit/\n").expect("ignore orbit store");
    std::fs::create_dir_all(repo.join("src")).expect("source dir");
    std::fs::write(repo.join("src/lib.rs"), "pub fn retry() {}\n").expect("source");
    git(&repo, &["add", ".gitignore", "src/lib.rs"]);
    git(&repo, &["commit", "-qm", "seed"]);
    let (parent, worktree) = live_run(&runtime, &repo, "T-EXPLORE-1");
    // A worker that exits at once is settled as an interrupted startup while
    // its explorer is still pending, which races the row's own start. The
    // substitute blocks on its release handshake until this fixture drops, so
    // only the row decides the explorer's start and finish. Drop then releases
    // the handshake and reaps the process before `root` is removed.
    let worker_release = root.path().join("release-substitute-worker");
    #[cfg(unix)]
    let listener = crate::worker_fixture::install_owned(&worker_release);
    #[cfg(not(unix))]
    crate::worker_fixture::install(&worker_release, "created");
    Fixture {
        runtime,
        repo,
        parent,
        worktree,
        worker_release,
        #[cfg(unix)]
        listener,
        #[cfg(unix)]
        workers: Mutex::new(Vec::new()),
        #[cfg(unix)]
        cleaned: Arc::new(AtomicBool::new(false)),
        root,
    }
}

#[cfg(unix)]
impl Fixture {
    /// Accept substitutes that already spawned. A run that never started a
    /// worker has no log, so this does not wait for it.
    fn own_outstanding(&self) -> io::Result<()> {
        let mut pending: Vec<String> = self
            .spawned_runs()?
            .into_iter()
            .filter(|run| !self.is_owned(run))
            .collect();
        while !pending.is_empty() {
            let substitute = self.acquire_one(&pending)?;
            pending.retain(|run| run != &substitute.run);
            self.workers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(substitute);
        }
        Ok(())
    }

    fn spawned_runs(&self) -> io::Result<Vec<String>> {
        let logs = &self.runtime.paths().logs_dir;
        let entries = match std::fs::read_dir(logs) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut runs = Vec::new();
        for entry in entries {
            let name = entry?.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if let Some(run) = name.strip_suffix(".worker.log") {
                runs.push(run.to_string());
            }
        }
        Ok(runs)
    }

    fn is_owned(&self, run: &str) -> bool {
        self.workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|worker| worker.run == run)
    }

    fn held_workers(&self) -> Vec<HeldWorker> {
        self.workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|worker| HeldWorker {
                run: worker.run.clone(),
                pid: worker.pid,
                starttime: worker.starttime,
            })
            .collect()
    }

    fn acquire_one(&self, pending: &[String]) -> io::Result<Substitute> {
        let phase = std::cell::Cell::new("poll_listener");
        self.acquire_one_at(pending, &phase).inspect_err(|error| {
            let _ = writeln!(
                io::stderr(),
                "owned explorer worker acquisition failed: phase={} kind={:?} raw_os_error={:?} error={error}",
                phase.get(),
                error.kind(),
                error.raw_os_error()
            );
        })
    }

    fn acquire_one_at(
        &self,
        pending: &[String],
        phase: &std::cell::Cell<&'static str>,
    ) -> io::Result<Substitute> {
        use std::os::fd::AsRawFd;
        let mut ready = libc::pollfd {
            fd: self.listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // A bounded readiness wait, not a sleep that hides the marker race.
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
        reader.read_line(&mut identity)?;
        phase.set("parse_identity");
        let (run, pid) = identity
            .trim()
            .split_once(' ')
            .ok_or_else(|| io::Error::other("worker handshake identity"))?;
        let pid: u32 = pid.parse().map_err(io::Error::other)?;
        if !pending.iter().any(|expected| expected == run) {
            return Err(io::Error::other(format!(
                "worker is not a substitute this fixture spawned: {run}"
            )));
        }
        phase.set("kernel_identity");
        let (key, parent) = orbit_common::process::ancestry::process_start_and_parent(pid)
            .ok_or_else(|| io::Error::other("worker kernel identity"))?;
        if parent != std::process::id() {
            return Err(io::Error::other("worker is not this fixture's child"));
        }
        phase.set("bind_process");
        let process = orbit_common::process::signal::BoundProcess::bind(pid, None)?
            .ok_or_else(|| io::Error::other("worker exited before ownership"))?;
        phase.set("recheck_identity");
        if orbit_common::process::ancestry::process_start_and_parent(pid) != Some((key, parent)) {
            return Err(io::Error::other("worker changed during identity binding"));
        }
        Ok(Substitute {
            run: run.to_string(),
            pid,
            starttime: key.starttime,
            process,
            control: reader.into_inner(),
        })
    }

    /// Write the marker, release every handshake, and return only after each
    /// substitute has observed the marker, been reaped, and had its supervisor
    /// observer finish. A stop fallback is a failure: the caller keeps `root`.
    fn release_reap_and_settle(&mut self) -> Result<(), String> {
        let repo = self.runtime.paths().repo_root.clone();
        let workers = self
            .workers
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if workers.is_empty() {
            return Ok(());
        }
        let mut failure = None;
        if let Err(error) = std::fs::write(&self.worker_release, b"") {
            failure = Some(format!("write release marker: {error}"));
        }
        for worker in workers.iter_mut() {
            if failure.is_some() {
                let _ = worker.process.signal(libc::SIGKILL);
                continue;
            }
            if let Err(error) = worker.control.write_all(&[1]) {
                failure = Some(format!(
                    "run={} pid={} phase=write_release kind={:?} raw_os_error={:?} error={error}",
                    worker.run,
                    worker.pid,
                    error.kind(),
                    error.raw_os_error()
                ));
                let _ = worker.process.signal(libc::SIGKILL);
            }
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let release_deadline = Instant::now() + Duration::from_secs(1);
        let mut stopped = false;
        loop {
            if workers.iter().all(|worker| reaped(worker.pid)) {
                break;
            }
            if !stopped && (failure.is_some() || Instant::now() >= release_deadline) {
                stopped = true;
                for worker in workers.iter().filter(|worker| !reaped(worker.pid)) {
                    if let Err(error) = worker.process.signal(libc::SIGKILL) {
                        failure = Some(format!("stop {}: {error}", worker.run));
                    }
                }
                failure.get_or_insert_with(|| "owned worker did not observe release".to_string());
            }
            if Instant::now() >= deadline {
                return Err(failure
                    .unwrap_or_else(|| "owned worker reap deadline; root retained".to_string()));
            }
            // Bounded predicate observation, not an interleaving based on delay.
            std::thread::park_timeout(Duration::from_millis(20));
        }
        if let Some(error) = failure {
            return Err(error);
        }
        for worker in workers.iter() {
            if !release_observed(&self.runtime, &worker.run) {
                return Err(format!(
                    "run={} pid={} exited without observing the release marker",
                    worker.run, worker.pid
                ));
            }
        }
        let settle_deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let settled = workers.iter().all(|worker| {
                orbit_core::test_support::pipeline_worker_observer_completed(
                    &repo,
                    &worker.run,
                    worker.pid,
                )
            });
            if settled {
                return Ok(());
            }
            if Instant::now() >= settle_deadline {
                return Err("supervisor settlement deadline; root retained".to_string());
            }
            std::thread::park_timeout(Duration::from_millis(20));
        }
    }

    fn log_owned_workers(&mut self) {
        let workers = self
            .workers
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for worker in workers.iter() {
            let path = self
                .runtime
                .paths()
                .logs_dir
                .join(format!("{}.worker.log", worker.run));
            if let Ok(log) = std::fs::read_to_string(path) {
                let _ = writeln!(
                    io::stderr(),
                    "owned substitute log: run={} pid={}\n{log}",
                    worker.run,
                    worker.pid
                );
            }
        }
    }
}

#[cfg(unix)]
fn release_observed(runtime: &OrbitRuntime, run: &str) -> bool {
    let path = runtime.paths().logs_dir.join(format!("{run}.worker.log"));
    std::fs::read_to_string(path).is_ok_and(|log| log.contains("worker release observed:"))
}

/// The substitute is still at its release handshake: the marker is absent,
/// the process is not reaped, and its supervisor has not finished.
#[cfg(unix)]
fn assert_held_before_teardown(fx: &Fixture) {
    let held = fx.held_workers();
    assert!(
        !held.is_empty(),
        "the admission row started no substitute to reap"
    );
    assert!(
        !fx.worker_release.exists(),
        "release marker must stay absent until teardown"
    );
    let repo = fx.runtime.paths().repo_root.clone();
    for worker in &held {
        assert!(
            !reaped(worker.pid),
            "substitute was reaped before teardown released the handshake"
        );
        assert!(
            !orbit_core::test_support::pipeline_worker_observer_completed(
                &repo,
                &worker.run,
                worker.pid,
            ),
            "supervisor completed before teardown released the handshake"
        );
    }
}

/// Drop has returned, so `root` is already gone only when release was
/// observed, the substitute was reaped, and the supervisor finished first.
#[cfg(unix)]
fn assert_reaped_before_root_removal(
    held: &[HeldWorker],
    cleaned: &AtomicBool,
    repo: &Path,
    root: &Path,
) {
    assert!(
        cleaned.load(Ordering::SeqCst),
        "owned explorer substitute still live or zombie after teardown"
    );
    for worker in held {
        assert!(
            reaped(worker.pid),
            "owned explorer substitute still live or zombie after teardown"
        );
        assert!(
            orbit_core::test_support::pipeline_worker_observer_completed(
                repo,
                &worker.run,
                worker.pid,
            ),
            "supervisor completion must be observed before the temporary root is removed"
        );
    }
    assert!(
        !root.exists(),
        "the temporary root is removed only after the substitute is reaped"
    );
}

#[cfg(unix)]
fn prove_substitute_reaped_on_teardown(fx: Fixture) {
    fx.own_outstanding()
        .expect("own the explorer substitute before teardown");
    assert_held_before_teardown(&fx);
    let held = fx.held_workers();
    let root = fx.root.path().to_path_buf();
    let cleaned = Arc::clone(&fx.cleaned);
    let repo = fx.runtime.paths().repo_root.clone();
    // A failed assertion must not leave the substitute until its own deadline.
    let _guard = StopSurvivors(held.clone());
    drop(fx);
    assert_reaped_before_root_removal(&held, &cleaned, &repo, &root);
}

#[cfg(unix)]
fn prove_substitute_reaped_when_the_scenario_unwinds() {
    for panic in [false, true] {
        let fx = fixture(BUDGETS);
        as_managed_agent(&fx.parent.run_id, "agent_implement");
        let session = agent_session();
        let cwd = fx.worktree.display().to_string();
        let _submission = explore(&fx.runtime, ask(&cwd, &session));
        fx.own_outstanding()
            .expect("own the explorer substitute before teardown");
        assert_held_before_teardown(&fx);
        let held = fx.held_workers();
        let root = fx.root.path().to_path_buf();
        let cleaned = Arc::clone(&fx.cleaned);
        let repo = fx.runtime.paths().repo_root.clone();
        let _guard = StopSurvivors(held.clone());
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _fx = fx;
            if panic {
                panic!("injected explorer assertion unwind");
            }
            Err::<(), &str>("injected explorer observation error")
        }));
        assert_eq!(outcome.is_err(), panic);
        if !panic {
            let returned = outcome.expect("observation error stays inside the row");
            assert!(returned.is_err(), "observation error stays inside the row");
        }
        assert_reaped_before_root_removal(&held, &cleaned, &repo, &root);
    }
}

/// A running delivery of `task_id` with its worktree checked out.
fn live_run(runtime: &OrbitRuntime, repo: &Path, task_id: &str) -> (JobRun, PathBuf) {
    let run = jobs(runtime)
        .insert_job_run(
            "task_pr_pipeline",
            1,
            Utc::now(),
            Some(json!({ "task_ids": [task_id], "resource_class": "orbit-hard" })),
            None,
        )
        .expect("insert implement run");
    jobs(runtime)
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("start implement run");
    let worktree = orbit_engine::run_worktree_paths(repo, &run).expect("worktree path")[0].clone();
    git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            worktree.to_str().expect("utf-8 worktree"),
        ],
    );
    (run, worktree)
}

/// Present this process as the MCP server of `run_id`'s `activity` agent.
fn as_managed_agent(run_id: &str, activity: &str) {
    // SAFETY: each row of this table runs alone in its own child process with
    // one test thread, so nothing reads the environment concurrently.
    unsafe {
        std::env::set_var("ORBIT_MANAGED_RUN_CONTEXT", "1");
        std::env::set_var("ORBIT_RUN_ID", run_id);
        std::env::set_var("ORBIT_ACTIVITY_NAME", activity);
    }
}

fn agent_session() -> ToolSessionContext {
    ToolSessionContext {
        effective_capabilities: [McpCapability::Agent].into_iter().collect(),
        ..ToolSessionContext::default()
    }
}

fn ask<'a>(cwd: &'a str, session: &'a ToolSessionContext) -> AgentInvokeRequest<'a> {
    AgentInvokeRequest {
        prompt: "where is the retry budget enforced",
        cwd,
        crew: None,
        timeout_seconds: None,
        idempotency_key: None,
        provider_sandbox: None,
        actor: Some("claude-opus-4-6"),
        session_context: session,
    }
}

fn explore(runtime: &OrbitRuntime, request: AgentInvokeRequest<'_>) -> ExploreSubmission {
    match runtime
        .submit_agent_invocation(request)
        .expect("an eligible implement run may delegate")
    {
        AgentInvocation::Explore(submission) => submission,
        AgentInvocation::TrustedHost(_) => {
            panic!("a managed run must never receive a trusted-host invocation")
        }
    }
}

fn refusal(runtime: &OrbitRuntime, request: AgentInvokeRequest<'_>) -> OrbitError {
    match runtime.submit_agent_invocation(request) {
        Ok(_) => panic!("the delegation must be refused"),
        Err(error) => error,
    }
}

fn explorer_count(runtime: &OrbitRuntime) -> usize {
    runtime
        .list_job_runs(orbit_core::application::job::JobRunListParams::default())
        .expect("list runs")
        .iter()
        .filter(|run| run.job_id == EXPLORE_JOB_ID)
        .count()
}

fn finish(runtime: &OrbitRuntime, run_id: &str) {
    jobs(runtime)
        .mark_job_run_running(run_id, Utc::now(), std::process::id())
        .expect("start explorer");
    jobs(runtime)
        .finalize_job_run(run_id, JobRunState::Success, Utc::now(), Some(10))
        .expect("finish explorer");
}

fn input_of(runtime: &OrbitRuntime, run_id: &str) -> Value {
    jobs(runtime)
        .get_job_run(run_id)
        .expect("read explorer")
        .expect("explorer exists")
        .input
        .expect("explorer input")
}

// ------------------------------------------------------------------ admission

fn an_implement_run_delegates_to_a_sandboxed_pool_explorer_in_its_worktree() {
    let fx = fixture_with(TWO_EXPLORER_CREWS, BUDGETS);
    as_managed_agent(&fx.parent.run_id, "agent_implement");
    let session = agent_session();
    let cwd = fx.worktree.join("src");
    let cwd = cwd.display().to_string();
    let tests_dir = fx.worktree.join("tests");
    std::fs::create_dir_all(&tests_dir).expect("sibling directory for a retry");
    let tests_cwd = tests_dir.display().to_string();

    let submission = explore(
        &fx.runtime,
        AgentInvokeRequest {
            idempotency_key: Some("retry-budget"),
            crew: Some("haiku"),
            ..ask(&cwd, &session)
        },
    );
    assert_eq!(submission.job_id, EXPLORE_JOB_ID);
    assert_eq!(
        submission.crew, "haiku",
        "the crew comes from the explorer pool"
    );
    assert_eq!(submission.parent_run_id, fx.parent.run_id);
    assert_eq!(
        submission.timeout_seconds, 300,
        "the setting is the default"
    );
    assert_eq!((submission.calls_used, submission.calls_allowed), (1, 2));
    assert_eq!(
        submission.state,
        if submission.queued {
            "queued"
        } else {
            "submitted"
        },
        "a new explorer reports the admission word, not a later run state"
    );
    let worktree = fx.worktree.canonicalize().expect("canonical worktree");
    assert_eq!(PathBuf::from(&submission.workspace_path), worktree);
    assert_eq!(PathBuf::from(&submission.cwd), worktree.join("src"));

    let input = input_of(&fx.runtime, &submission.run_id);
    assert!(
        input.get(TRUSTED_HOST_ADMISSION_KEY).is_none(),
        "an explorer must never carry a trusted-host admission: that would run it outside \
         the executor sandbox"
    );
    assert!(
        input.get("task_id").is_none() && input.get("task_ids").is_none(),
        "an explorer owns no task it could change"
    );
    assert_eq!(input["parent_run_id"], json!(fx.parent.run_id));
    assert_eq!(input["parent_task_id"], json!("T-EXPLORE-1"));
    assert_eq!(input["parent_activity"], json!("agent_implement"));
    assert_eq!(
        input["resource_class"],
        json!("orbit-hard"),
        "the explorer's minutes count in its caller's work class"
    );

    // A retried call resolves the same explorer and spends no budget, even
    // when it names another eligible crew, a shorter timeout and a sibling
    // directory. The response is the run that was persisted, not the retry.
    let retried = explore(
        &fx.runtime,
        AgentInvokeRequest {
            idempotency_key: Some("retry-budget"),
            crew: Some("sonnet"),
            timeout_seconds: Some(30),
            ..ask(&tests_cwd, &session)
        },
    );
    assert!(retried.deduplicated);
    assert_eq!(retried.run_id, submission.run_id);
    assert_eq!(
        (
            retried.crew.as_str(),
            retried.timeout_seconds,
            retried.cwd.as_str()
        ),
        ("haiku", 300, submission.cwd.as_str()),
        "a retry's crew, timeout and directory must not replace the persisted ones"
    );
    assert_eq!(retried.workspace_path, submission.workspace_path);
    assert_eq!(retried.calls_used, 1);
    assert_eq!(explorer_count(&fx.runtime), 1);
    let persisted = input_of(&fx.runtime, &submission.run_id);
    assert_eq!(persisted["crew"], json!("haiku"));
    assert_eq!(persisted["timeout_seconds"], json!(300));
    assert_eq!(persisted["cwd"], json!(submission.cwd));

    // Omitting crew redraws the pool. Whichever member that draw returns,
    // the response stays the original run.
    let redrawn = explore(
        &fx.runtime,
        AgentInvokeRequest {
            idempotency_key: Some("retry-budget"),
            crew: None,
            timeout_seconds: Some(45),
            ..ask(&tests_cwd, &session)
        },
    );
    assert!(redrawn.deduplicated);
    assert_eq!(redrawn.run_id, submission.run_id);
    assert_eq!(
        (
            redrawn.crew.as_str(),
            redrawn.timeout_seconds,
            redrawn.cwd.as_str()
        ),
        ("haiku", 300, submission.cwd.as_str()),
        "a pool redraw on retry must not replace the persisted crew, timeout or directory"
    );
    assert_eq!(redrawn.calls_used, 1);

    // The tool itself, through the governed chokepoint the agent's MCP server
    // uses: the operator-only row does not stop a managed caller, whose call
    // reaches the explorer admission and never the trusted-host one.
    let response = fx
        .runtime
        .run_tool(
            "orbit.agent.invoke",
            json!({
                "prompt": "where is the retry budget enforced",
                "cwd": tests_cwd,
                "crew": "sonnet",
                "timeout_seconds": 30,
                "idempotency_key": "retry-budget"
            }),
        )
        .expect("a managed implement agent may call the tool");
    assert_eq!(response["run_id"], json!(submission.run_id));
    assert_eq!(response["deduplicated"], json!(true));
    assert_eq!(response["crew"], json!("haiku"));
    assert_eq!(response["timeout_seconds"], json!(300));
    assert_eq!(response["cwd"], json!(submission.cwd));
    assert_eq!(response["calls_used"], json!(1));
    assert_eq!(
        (&response["sandboxed"], &response["read_only"]),
        (&json!(true), &json!(true))
    );
    assert!(
        response.get("authorized_by").is_none(),
        "no operator authorized an explorer"
    );
    assert_eq!(explorer_count(&fx.runtime), 1);

    let recorded = jobs(&fx.runtime)
        .get_job_run(&submission.run_id)
        .expect("read explorer")
        .expect("explorer exists");
    if !recorded.state.is_terminal() {
        finish(&fx.runtime, &submission.run_id);
    }
    let recorded = jobs(&fx.runtime)
        .get_job_run(&submission.run_id)
        .expect("read finished explorer")
        .expect("explorer exists");
    assert!(
        recorded.state.is_terminal(),
        "the retry must be judged against a finished explorer, got {}",
        recorded.state
    );
    let finished = explore(
        &fx.runtime,
        AgentInvokeRequest {
            idempotency_key: Some("retry-budget"),
            crew: Some("sonnet"),
            timeout_seconds: Some(15),
            ..ask(&tests_cwd, &session)
        },
    );
    assert!(finished.deduplicated);
    assert_eq!(finished.run_id, submission.run_id);
    assert_eq!(finished.state, recorded.state.to_string());
    assert!(!finished.queued);
    assert_eq!(finished.queue_position, None);
    assert_eq!(
        (
            finished.crew.as_str(),
            finished.timeout_seconds,
            finished.cwd.as_str()
        ),
        ("haiku", 300, submission.cwd.as_str())
    );
    assert_eq!(finished.calls_used, 1);
    assert_eq!(explorer_count(&fx.runtime), 1);
    let finished_response = fx
        .runtime
        .run_tool(
            "orbit.agent.invoke",
            json!({
                "prompt": "where is the retry budget enforced",
                "cwd": tests_cwd,
                "crew": "sonnet",
                "timeout_seconds": 15,
                "idempotency_key": "retry-budget"
            }),
        )
        .expect("a retry of a finished explorer still resolves it");
    assert_eq!(
        finished_response["state"],
        json!(recorded.state.to_string())
    );
    assert_eq!(finished_response["queued"], json!(false));
    assert_eq!(finished_response["calls_used"], json!(1));

    let listed = fx
        .runtime
        .explore_invocation_summaries(&fx.parent.run_id)
        .expect("run detail lists the caller's explorers");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].run_id, submission.run_id);
    assert_eq!(listed[0].crew.as_deref(), Some("haiku"));
    assert_eq!(
        listed[0].parent_activity.as_deref(),
        Some("agent_implement")
    );

    #[cfg(unix)]
    {
        prove_substitute_reaped_on_teardown(fx);
        prove_substitute_reaped_when_the_scenario_unwinds();
        let _ = writeln!(
            io::stderr(),
            "explorer substitute observed release during teardown and was reaped before root removal"
        );
    }
}

/// Automatic exploration records the same pressure weights as other pools;
/// a named crew stays pinned and an idempotent retry keeps the original draw.
fn explorer_draws_record_pressure_and_retries_keep_the_original_pool() {
    let crews = TWO_EXPLORER_CREWS
        .replace(
            "[workflow]",
            "[workflow]\ncrew_selection = \"usage_pressure\"",
        )
        .replace(
            "explorer = [\"haiku\", \"sonnet\"]",
            "explorer = [\"haiku:1\", \"sonnet:3\", \"opus:0\"]",
        )
        .replace(
            "[crews.sonnet]\nprovider = \"claude\"",
            "[crews.sonnet]\nprovider = \"codex\"",
        );
    let fx = fixture_with(&crews, BUDGETS);
    as_managed_agent(&fx.parent.run_id, "agent_implement");
    crate::usage_pressure_draws::record_week(
        &fx.runtime,
        "claude",
        10.0,
        chrono::Duration::hours(6),
    );
    crate::usage_pressure_draws::record_week(&fx.runtime, "codex", 0.0, chrono::Duration::days(7));
    let session = agent_session();
    let cwd = fx.worktree.display().to_string();
    let submission = explore(
        &fx.runtime,
        AgentInvokeRequest {
            idempotency_key: Some("weighted"),
            ..ask(&cwd, &session)
        },
    );
    // Compare the persisted JSON representation, including its float decode.
    let recorded: Value =
        serde_json::from_slice(&serde_json::to_vec(&submission).unwrap()).unwrap();
    crate::usage_pressure_draws::assert_pool(
        &recorded["eligible_pool"],
        &[("haiku", 1, 8.0), ("opus", 0, 0.0), ("sonnet", 3, 3.0)],
    );
    assert_eq!(
        input_of(&fx.runtime, &submission.run_id)["eligible_pool"],
        recorded["eligible_pool"]
    );
    crate::usage_pressure_draws::record_week(
        &fx.runtime,
        "claude",
        80.0,
        chrono::Duration::days(6),
    );
    let retried = explore(
        &fx.runtime,
        AgentInvokeRequest {
            idempotency_key: Some("weighted"),
            crew: Some("sonnet"),
            ..ask(&cwd, &session)
        },
    );
    assert!(retried.deduplicated);
    assert_eq!(retried.crew, submission.crew);
    assert_eq!(json!(retried)["eligible_pool"], recorded["eligible_pool"]);
    let response = fx
        .runtime
        .run_tool(
            "orbit.agent.invoke",
            json!({
                "prompt": "inspect the retry budget", "cwd": cwd, "crew": "sonnet",
                "idempotency_key": "weighted",
            }),
        )
        .unwrap();
    assert_eq!(
        response["eligible_pool"], recorded["eligible_pool"],
        "the governed tool exposes the frozen draw too"
    );

    let current = jobs(&fx.runtime)
        .get_job_run(&submission.run_id)
        .expect("read explorer before finishing")
        .expect("explorer exists");
    if current.state == JobRunState::Pending {
        // The substitute worker can win the enqueue race and interrupt the
        // run before this fixture gets to its manual start.
        finish(&fx.runtime, &submission.run_id);
    }
    let pinned = explore(
        &fx.runtime,
        AgentInvokeRequest {
            crew: Some("sonnet"),
            ..ask(&cwd, &session)
        },
    );
    assert_eq!(pinned.crew, "sonnet");
    assert_eq!(
        json!(pinned)["eligible_pool"],
        json!([{"name": "sonnet", "weight": 3}])
    );
}

fn an_out_of_pool_crew_and_a_foreign_cwd_are_refused() {
    let fx = fixture(BUDGETS);
    let (other, other_worktree) = live_run(&fx.runtime, &fx.repo, "T-EXPLORE-2");
    as_managed_agent(&fx.parent.run_id, "agent_implement");
    let session = agent_session();
    let own = fx.worktree.display().to_string();

    let error = refusal(
        &fx.runtime,
        AgentInvokeRequest {
            crew: Some("opus"),
            ..ask(&own, &session)
        },
    );
    assert!(
        matches!(&error, OrbitError::CapabilityDenied(message)
            if message.contains("workflow.purpose_crews.explorer")),
        "an expensive crew outside the explorer pool is refused: {error:?}"
    );

    for foreign in [
        other_worktree.display().to_string(),
        fx.repo.display().to_string(),
        fx.repo.join("src").display().to_string(),
    ] {
        let error = refusal(&fx.runtime, ask(&foreign, &session));
        assert!(
            matches!(error, OrbitError::PolicyDenied(_)),
            "{foreign}: an explorer reads only its caller's own worktree, got {error:?}"
        );
    }
    let error = refusal(
        &fx.runtime,
        AgentInvokeRequest {
            provider_sandbox: Some("danger-full-access"),
            ..ask(&own, &session)
        },
    );
    assert!(
        matches!(error, OrbitError::InvalidInput(_)),
        "an explorer cannot widen its provider sandbox: {error:?}"
    );
    assert_eq!(explorer_count(&fx.runtime), 0, "a refusal leaves no run");
    let _ = other;
}

fn explorer_budgets_refuse_with_the_setting_named() {
    let fx = fixture(BUDGETS);
    as_managed_agent(&fx.parent.run_id, "agent_implement");
    let session = agent_session();
    let cwd = fx.worktree.display().to_string();
    let denied_naming = |error: OrbitError, key: &str| match error {
        OrbitError::CapabilityDenied(message) => assert!(
            message.contains(key),
            "the refusal must name `{key}` so the agent knows why: {message}"
        ),
        other => panic!("expected a budget refusal naming {key}, got {other:?}"),
    };

    let error = refusal(
        &fx.runtime,
        AgentInvokeRequest {
            timeout_seconds: Some(301),
            ..ask(&cwd, &session)
        },
    );
    assert!(
        matches!(&error, OrbitError::InvalidInput(message)
            if message.contains("workflow.explore.timeout_seconds")),
        "a timeout above the ceiling is refused: {error:?}"
    );

    let first = explore(&fx.runtime, ask(&cwd, &session));
    denied_naming(
        refusal(&fx.runtime, ask(&cwd, &session)),
        "workflow.explore.max_concurrent_per_run",
    );
    finish(&fx.runtime, &first.run_id);
    let second = explore(&fx.runtime, ask(&cwd, &session));
    assert_eq!(second.calls_used, 2);
    finish(&fx.runtime, &second.run_id);
    denied_naming(
        refusal(&fx.runtime, ask(&cwd, &session)),
        "workflow.explore.max_calls_per_run",
    );
    assert_eq!(explorer_count(&fx.runtime), 2);
}

fn the_switch_off_refuses_every_caller() {
    let fx = fixture("[workflow.explore]\nenabled = false\n");
    as_managed_agent(&fx.parent.run_id, "agent_implement");
    let session = agent_session();
    let cwd = fx.worktree.display().to_string();
    let error = refusal(&fx.runtime, ask(&cwd, &session));
    assert!(
        matches!(&error, OrbitError::CapabilityDenied(message)
            if message.contains("workflow.explore.enabled")),
        "with exploration off the caller is told so and continues: {error:?}"
    );
    assert_eq!(explorer_count(&fx.runtime), 0);
}

fn other_agents_and_explorers_themselves_are_refused() {
    let fx = fixture(BUDGETS);
    let session = agent_session();
    let cwd = fx.worktree.display().to_string();

    // A managed run of any other activity, such as a reviewer, is refused.
    as_managed_agent(&fx.parent.run_id, "agent_review");
    assert!(matches!(
        refusal(&fx.runtime, ask(&cwd, &session)),
        OrbitError::CapabilityDenied(_)
    ));

    // An explorer cannot invoke another agent.
    as_managed_agent(&fx.parent.run_id, "agent_implement");
    let explorer = explore(&fx.runtime, ask(&cwd, &session));
    as_managed_agent(&explorer.run_id, "agent_implement");
    let error = refusal(&fx.runtime, ask(&cwd, &session));
    assert!(
        matches!(&error, OrbitError::CapabilityDenied(message) if message.contains("another agent")),
        "explorers never nest: {error:?}"
    );

    // A finished run delegates nothing.
    jobs(&fx.runtime)
        .finalize_job_run(
            &fx.parent.run_id,
            JobRunState::Success,
            Utc::now(),
            Some(10),
        )
        .expect("finish implement run");
    as_managed_agent(&fx.parent.run_id, "agent_implement");
    assert!(matches!(
        refusal(&fx.runtime, ask(&cwd, &session)),
        OrbitError::CapabilityDenied(_)
    ));
    assert_eq!(explorer_count(&fx.runtime), 1);
}

fn the_operator_path_is_unchanged_by_exploration() {
    // With exploration on, an operator still gets its trusted-host invocation
    // and an unmanaged agent session is still refused.
    let fx = fixture(BUDGETS);
    let cwd = fx.repo.display().to_string();
    let error = fx
        .runtime
        .submit_agent_invocation(ask(&cwd, &agent_session()))
        .expect_err("an unmanaged agent never invokes an agent");
    assert!(
        matches!(&error, OrbitError::CapabilityDenied(message) if message.contains("orbit.agent.invoke")),
        "{error:?}"
    );
    let operator = ToolSessionContext {
        effective_capabilities: [McpCapability::Operator].into_iter().collect(),
        ..ToolSessionContext::default()
    };
    match fx
        .runtime
        .submit_agent_invocation(ask(&cwd, &operator))
        .expect("an operator invocation is admitted")
    {
        AgentInvocation::TrustedHost(submission) => {
            assert_eq!(
                submission.job_id,
                orbit_core::application::job::AGENT_INVOKE_JOB_ID
            );
            let input = input_of(&fx.runtime, &submission.run_id);
            assert!(
                input.get(TRUSTED_HOST_ADMISSION_KEY).is_some(),
                "the operator path keeps its per-invocation admission"
            );
        }
        AgentInvocation::Explore(_) => panic!("an operator never gets an explorer"),
    }
    assert_eq!(explorer_count(&fx.runtime), 0);
}

// --------------------------------------------------------------- answer bounds

fn empty_explorer_result() -> AgentInvokeResult {
    let run: JobRun = serde_json::from_value(json!({
        "run_id": "answer-fixture", "job_id": EXPLORE_JOB_ID, "attempt": 1,
        "state": "success", "scheduled_at": Utc::now(), "created_at": Utc::now()
    }))
    .expect("run fixture");
    let mut state = PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({}));
    state.record_step(0, JobRunState::Success, Some(json!({})), None);
    let mut result = agent_invoke_result(&run, Some(&state), None).expect("explorer result");
    result.answer.as_mut().expect("answer").paths_read = None;
    result
}

fn answer_caps_smaller_than_the_supported_shape_are_refused() {
    let minimum = serde_json::to_vec(&empty_explorer_result().answer.expect("answer"))
        .expect("serialize minimal answer")
        .len();
    let root = TempDir::new().expect("config root");
    let global = root.path().join("global");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&global).expect("global config dir");
    std::fs::create_dir_all(&workspace).expect("workspace config dir");
    for cap in [0, 1, minimum - 1, 262_145] {
        std::fs::write(
            workspace.join("config.toml"),
            format!("[workflow.explore]\nmax_answer_bytes = {cap}\n"),
        )
        .expect("write cap");
        let error = OrbitRuntime::from_roots(&global, &workspace)
            .err()
            .expect("an unrepresentable answer cap must be refused");
        assert!(
            matches!(&error, OrbitError::InvalidInput(message) if message.contains("workflow.explore.max_answer_bytes")),
            "cap {cap}: {error:?}"
        );
    }
}

fn serialized_explorer_answers_fit_at_the_minimum_and_larger_caps() {
    let minimum = serde_json::to_vec(&empty_explorer_result().answer.expect("answer"))
        .expect("serialize minimal answer")
        .len();
    for cap in [minimum, 256, 512, 16_384, 262_144] {
        let (_root, runtime, _) = super::engine_support::runtime_with_workspace_config(Some(
            &format!("[workflow.explore]\nmax_answer_bytes = {cap}\n"),
        ));
        for (summary, blob_ref) in [
            (None, None),
            (
                Some("\"\n\\雪🦀".repeat(cap)),
                Some("stdout-blob".to_string()),
            ),
            (Some("\"\n\\雪🦀".repeat(cap)), Some("\"雪".repeat(cap))),
        ] {
            let mut original = empty_explorer_result();
            let answer = original.answer.as_mut().expect("answer");
            answer.summary = summary.clone();
            if summary.is_some() {
                answer.findings = vec![json!("finding".repeat(cap))];
                answer.next_steps = vec![json!("next".repeat(cap))];
                answer
                    .extra
                    .insert("extra".into(), json!("extra".repeat(cap)));
                answer.paths_read = Some(vec!["path".repeat(cap)]);
                answer.final_message = Some("final".repeat(cap));
                answer.final_message_bytes = Some(u64::MAX);
            }
            original.summary = summary;
            original.preview = Some("captured output".into());
            original.stdout_blob_ref = blob_ref;
            let bounded = runtime.bound_explore_answer(original.clone());
            let answer = bounded
                .result
                .answer
                .as_ref()
                .expect("answer shape retained");
            let bytes = serde_json::to_vec(answer).expect("serialize bounded answer");
            assert!(bytes.len() <= cap, "{} bytes exceed cap {cap}", bytes.len());
            assert_eq!(bounded.result.summary, answer.summary);
            assert_eq!(bounded.result.stdout_blob_ref, original.stdout_blob_ref);
            if original.summary.is_some() {
                assert!(bounded.answer_truncated);
                assert!(answer.final_message_truncated);
                assert!(bounded.result.preview.is_none());
            } else {
                assert!(!bounded.answer_truncated);
                assert_eq!(bounded.result, original, "an answer that fits stays intact");
            }
        }
    }
}

// ---------------------------------------------------------------- denied write

/// The explorer's write boundary is the `fsProfile` its shipped activity
/// declares. Resolved for the caller's worktree, it must refuse every write
/// there that the implementer's own profile grants.
#[cfg(target_os = "linux")]
fn the_explorer_sandbox_denies_writes_the_implementer_may_make() {
    use orbit_engine::RuntimeHost;
    use orbit_types::workflow::ExecutorSandboxKind;

    let fx = fixture(BUDGETS);
    let shipped: serde_yaml::Value = serde_yaml::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/activities/agent_explore.yaml"),
        )
        .expect("read the shipped explorer activity"),
    )
    .expect("parse the shipped explorer activity");
    let profile = shipped["spec"]["fsProfile"]
        .as_str()
        .expect("the explorer declares its filesystem profile");
    super::engine_support::seed_executor(
        &fx.runtime,
        "claude",
        Some(ExecutorSandboxKind::LinuxBwrap),
    );
    let worktree = fx.worktree.canonicalize().expect("canonical worktree");
    let source = worktree.join("src/lib.rs");
    let writes = |profile: Option<&str>| {
        let resolved = fx
            .runtime
            .resolve_executor_sandbox("claude", profile, Some(&worktree))
            .expect("resolve sandbox")
            .expect("a sandboxed executor resolves a profile");
        orbit_exec::linux_bwrap_write_grant_diagnostic(&resolved.fs_profile, &source)
            .expect("diagnose write")
            .is_none()
    };
    assert!(
        !writes(Some(profile)),
        "the explorer's `{profile}` profile must deny writes in its caller's worktree: an \
         explorer that can edit would bypass the implementer's review and validation"
    );
    assert!(
        writes(None),
        "the implementer's own profile may write its worktree"
    );
}

// ------------------------------------------------------------------ analytics

fn implement_invocation(runtime: &OrbitRuntime, run_id: &str, activity: &str, ms: u64, usd: f64) {
    runtime
        .insert_invocation_trace_record(&orbit_store::contracts::InvocationInsertParams {
            job_run_id: run_id.to_string(),
            account: None,
            activity_id: activity.to_string(),
            agent: "claude".to_string(),
            provider: None,
            model: Some("claude-opus-4-6".to_string()),
            task_ids: Vec::new(),
            trace: orbit_types::telemetry::InvocationTrace {
                duration_ms: ms,
                provider_cost_usd: Some(usd),
                ..Default::default()
            },
        })
        .expect("record invocation");
}

fn cost_analytics_compare_runs_with_and_without_explorers() {
    compare_explorer_invocations("agent_implement", "agent_explore");
}

fn cost_analytics_compare_production_step_invocations() {
    // The dispatcher persists step.id, while explorer parent_activity carries
    // the catalog activity name. These are deliberately different identities.
    compare_explorer_invocations("implement_one", "explore");
}

fn compare_explorer_invocations(implement_step: &str, explore_step: &str) {
    let fx = fixture(BUDGETS);
    let empty = fx
        .runtime
        .explore_comparison(None)
        .expect("empty comparison");
    assert_eq!(empty.runs_scanned, 0);
    assert!(empty.crews.is_empty());
    let task = fx
        .runtime
        .add_task(orbit_core::application::task::TaskAddParams {
            title: "Explore analytics fixture".to_string(),
            ..Default::default()
        })
        .expect("create task")
        .id;
    let (plain, _) = live_run(&fx.runtime, &fx.repo, &task);
    as_managed_agent(&fx.parent.run_id, "agent_implement");
    let session = agent_session();
    let cwd = fx.worktree.display().to_string();
    let explorer = explore(&fx.runtime, ask(&cwd, &session));

    implement_invocation(&fx.runtime, &fx.parent.run_id, implement_step, 60_000, 2.0);
    implement_invocation(&fx.runtime, &plain.run_id, implement_step, 180_000, 4.0);
    implement_invocation(&fx.runtime, &plain.run_id, implement_step, 60_000, 1.0);
    implement_invocation(&fx.runtime, &explorer.run_id, explore_step, 5_000, 0.25);

    // Other pipeline steps in the same run must not inflate implement cost
    // or duration, or create an implementing cohort of their own.
    implement_invocation(&fx.runtime, &plain.run_id, "review", 900_000, 100.0);
    let empty_window = fx
        .runtime
        .explore_comparison(Some(Utc::now() + chrono::Duration::hours(1)))
        .expect("empty future window");
    assert_eq!(empty_window.runs_scanned, 0);
    assert!(empty_window.crews.is_empty());

    let comparison = fx.runtime.explore_comparison(None).expect("compare");
    assert_eq!(
        comparison.runs_scanned, 2,
        "explorer invocations are not implement runs"
    );
    let [crew] = comparison.crews.as_slice() else {
        panic!("one implementing crew: {comparison:?}");
    };
    assert_eq!(crew.crew, "claude/claude-opus-4-6");
    let with = &crew.with_explore;
    assert_eq!((with.runs, with.explore_calls), (1, 1));
    assert_eq!(with.mean_duration_ms, Some(60_000));
    assert_eq!(
        (with.mean_cost_usd, with.priced_runs),
        (Some(2.0), 1),
        "implementing cost excludes the explorer's own spend"
    );
    assert_eq!(with.explore_cost_usd, 0.25);
    assert_eq!(
        with.footprint_known_runs, 0,
        "a run whose task history is unreadable is no footprint sample"
    );
    let without = &crew.without_explore;
    assert_eq!((without.runs, without.explore_calls), (1, 0));
    assert_eq!(
        without.mean_duration_ms,
        Some(240_000),
        "a run's invocations add up"
    );
    assert_eq!(without.mean_cost_usd, Some(5.0));
    assert_eq!(
        (without.footprint_known_runs, without.footprint_miss_runs),
        (1, 0),
        "a task whose run never widened its footprint is a known non-miss"
    );
}

crate::scenario_table!(
    agent_explore_admission_scenarios,
    an_implement_run_delegates_to_a_sandboxed_pool_explorer_in_its_worktree,
    an_out_of_pool_crew_and_a_foreign_cwd_are_refused,
    explorer_draws_record_pressure_and_retries_keep_the_original_pool,
    explorer_budgets_refuse_with_the_setting_named,
    the_switch_off_refuses_every_caller,
    other_agents_and_explorers_themselves_are_refused,
    the_operator_path_is_unchanged_by_exploration,
    cost_analytics_compare_runs_with_and_without_explorers,
    cost_analytics_compare_production_step_invocations,
    answer_caps_smaller_than_the_supported_shape_are_refused,
    serialized_explorer_answers_fit_at_the_minimum_and_larger_caps
);

#[cfg(target_os = "linux")]
crate::scenario_table!(
    agent_explore_sandbox_scenarios,
    the_explorer_sandbox_denies_writes_the_implementer_may_make
);
