//! [ORB-16373] A late delivery step outlasts a long hold of the owner's
//! task-commit lock instead of failing a complete candidate.
//!
//! A child process of this test binary holds a real advisory lock. The
//! host's owner calls acquire it the way the owner's task-commit boundary
//! does, with a short deadline standing in for the 30 s one. A holder that
//! lets go only after two acquisitions lost to it, well within the step's
//! retry cap, lets `claim_validate` and `pr_complete` complete and record the
//! wait in their output; a holder that outlasts the cap fails them
//! `transient` with the holder named. `git_rebase` is covered the same way on a real checkout in
//! `v2_worktree_lifecycle`.
//!
//! Runs under `cargo nextest run -p orbit-engine --test engine -E 'test(/owner_lock/)'`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_common::fs::io::{
    FileLockOptions, acquire_exclusive_file_lock, with_shared_file_lock_options,
};
use orbit_common::fs::lock_retry::{
    LockBusyRetryPolicy, retry_busy_acquisition, with_lock_busy_retry_policy,
};
use orbit_common::{NotFoundKind, OrbitError};
use orbit_engine::{ClaimExecutionContext, TaskActivityUpdate, execute_deterministic_action};
use orbit_types::task::{Task, TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::ClaimFailureClass;
use serde_json::{Value, json};

const HOLDER_LOCK_ENV: &str = "ORBIT_TEST_OWNER_LOCK_HOLDER_LOCK";
const HOLDER_READY_ENV: &str = "ORBIT_TEST_OWNER_LOCK_HOLDER_READY";
const HOLDER_RELEASE_ENV: &str = "ORBIT_TEST_OWNER_LOCK_HOLDER_RELEASE";
/// What the holder records as its operation, as an owner admission would.
const HOLDER_LABEL: &str = "task commit boundary: admission (fixture holder)";
/// One owner acquisition's deadline, standing in for the owner's 30 s.
const ATTEMPT: Duration = Duration::from_millis(250);
/// Lost acquisitions after which an owner call asks a releasable holder to
/// let go, so the hold always spans more than one deadline.
const LOST_BEFORE_RELEASE: u32 = 2;
/// A cap the releasing holder's hold stays well within.
pub(crate) const GENEROUS_CAP: Duration = Duration::from_secs(30);
/// A cap a holder that never lets go outlasts.
pub(crate) const SHORT_CAP: Duration = Duration::from_millis(900);
/// Hang guard for the holder's startup handshake only.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);

const TASK_ID: &str = "T-LOCK";
const RUN_ID: &str = "jrun-owner-lock";

/// A process of this test binary holding the owner lock in `dir`. It is
/// killed and reaped on drop.
pub(crate) struct LockHolder {
    child: Child,
}

impl LockHolder {
    /// Hold the owner lock in `dir` until an owner call has lost
    /// [`LOST_BEFORE_RELEASE`] acquisitions to it when `releasable`, or else
    /// until dropped.
    pub(crate) fn spawn(dir: &Path, releasable: bool) -> Self {
        let ready = dir.join("holder.ready");
        for stale in [&ready, &release_path(dir)] {
            match std::fs::remove_file(stale) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => panic!("{error}"),
                _ => {}
            }
        }
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "owner_lock_retry::lock_holder_child",
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(HOLDER_LOCK_ENV, owner_lock_path(dir))
            .env(HOLDER_READY_ENV, &ready)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        if releasable {
            command.env(HOLDER_RELEASE_ENV, release_path(dir));
        }
        let child = command.spawn().unwrap();
        let mut holder = Self { child };
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        while !ready.exists() {
            assert!(
                holder.child.try_wait().unwrap().is_none(),
                "the lock holder exited before taking the lock"
            );
            assert!(
                Instant::now() < deadline,
                "the lock holder never took the lock"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        holder
    }

    pub(crate) fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for LockHolder {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

/// The holder [`LockHolder::spawn`] starts: take the lock, say so, keep it.
#[test]
#[ignore = "run only as the lock holder a scenario spawns"]
fn lock_holder_child() {
    let Some(lock) = std::env::var_os(HOLDER_LOCK_ENV) else {
        return;
    };
    let _guard =
        acquire_exclusive_file_lock(Path::new(&lock), HOLDER_LABEL, FileLockOptions::default())
            .unwrap();
    std::fs::write(std::env::var_os(HOLDER_READY_ENV).unwrap(), b"").unwrap();
    let release = std::env::var_os(HOLDER_RELEASE_ENV).map(PathBuf::from);
    while !release.as_deref().is_some_and(Path::exists) {
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn owner_lock_path(dir: &Path) -> PathBuf {
    dir.join(".task-commit.lock")
}

fn release_path(dir: &Path) -> PathBuf {
    dir.join("holder.release")
}

/// An owner call through the lock in `dir`: an acquisition with a short
/// deadline that, like the owner's boundary entries, is retried while the
/// step's lock-busy retry scope has time left. After
/// [`LOST_BEFORE_RELEASE`] lost acquisitions it asks a releasable holder to
/// let go.
pub(crate) fn through_owner_lock(dir: &Path) -> Result<(), OrbitError> {
    let options = FileLockOptions {
        timeout: ATTEMPT,
        warn_after: ATTEMPT,
        ..FileLockOptions::default()
    };
    let mut lost = 0;
    retry_busy_acquisition(|| {
        match with_shared_file_lock_options(&dir.join("task-commit"), "owner read", options, || {
            Ok::<_, OrbitError>(())
        }) {
            Err(error) if error.is_lock_busy() => {
                lost += 1;
                if lost == LOST_BEFORE_RELEASE {
                    std::fs::write(release_path(dir), b"").unwrap();
                }
                Err(error)
            }
            result => Ok(result),
        }
    })
}

/// Run `op` with a step retry cap of `cap` and short pauses.
pub(crate) fn with_cap<T>(cap: Duration, op: impl FnOnce() -> T) -> T {
    with_lock_busy_retry_policy(
        LockBusyRetryPolicy {
            cap,
            first_pause: Duration::from_millis(50),
            longest_pause: Duration::from_millis(200),
        },
        op,
    )
}

/// The released case's output records at least one retry and its wait.
pub(crate) fn assert_wait_recorded(output: &Value) {
    let wait = &output["owner_lock_wait"];
    assert!(
        wait["retries"]
            .as_u64()
            .is_some_and(|retries| retries >= u64::from(LOST_BEFORE_RELEASE))
            && wait["waited_ms"]
                .as_u64()
                .is_some_and(|waited| waited >= ATTEMPT.as_millis() as u64)
            && wait["cap_ms"] == json!(GENEROUS_CAP.as_millis() as u64),
        "the step records the retries that outlasted the hold: {output:#}"
    );
}

/// The capped case fails `transient`, naming the holder's process and
/// operation.
pub(crate) fn assert_transient_naming(error: &OrbitError, holder: &LockHolder) {
    let message = error.to_string();
    assert_eq!(
        ClaimFailureClass::of_step_failure(None, Some(&message)),
        Some(ClaimFailureClass::Transient),
        "a lock the step outwaited its cap on is a transient failure: {message}"
    );
    assert!(
        message.contains(&format!("held by pid {}", holder.pid()))
            && message.contains(HOLDER_LABEL),
        "the failure names the holder: {message}"
    );
}

/// The owner a claimed leaf's `claim_validate` and an owner's `pr_complete`
/// reach: every task read, task write and claim context read goes through
/// the owner lock.
struct OwnerLockHost {
    lock_dir: PathBuf,
    tasks: Mutex<BTreeMap<String, Task>>,
}

impl OwnerLockHost {
    fn new(lock_dir: &Path, status: TaskStatus) -> Self {
        let now = Utc::now();
        let task = Task {
            job_run_machine: None,
            id: TASK_ID.to_string(),
            title: "Confirm the base already covers the change".to_string(),
            description: String::new(),
            acceptance_criteria: Vec::new(),
            tags: vec!["no-diff-expected".to_string()],
            required_tools: Vec::new(),
            plan: String::new(),
            execution_summary: "Outcome: success".to_string(),
            context_files: Vec::new(),
            created_by: None,
            planned_by: None,
            implemented_by: None,
            status,
            priority: TaskPriority::Medium,
            complexity: None,
            task_type: TaskType::Chore,
            pr_status: None,
            external_refs: Vec::new(),
            relations: Vec::new(),
            job_run_id: Some(RUN_ID.to_string()),
            crew: None,
            crew_source: None,
            orchestrator: None,
            created_at: now,
            updated_at: now,
        };
        Self {
            lock_dir: lock_dir.to_path_buf(),
            tasks: Mutex::new(BTreeMap::from([(TASK_ID.to_string(), task)])),
        }
    }

    fn status(&self) -> TaskStatus {
        self.tasks.lock().unwrap()[TASK_ID].status
    }
}

impl orbit_engine::test_support::NullRuntimeHost for OwnerLockHost {
    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        through_owner_lock(&self.lock_dir)?;
        self.tasks
            .lock()
            .unwrap()
            .get(task_id)
            .cloned()
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, task_id.to_string()))
    }

    fn update_task_from_activity(
        &self,
        task_id: &str,
        update: TaskActivityUpdate,
    ) -> Result<Task, OrbitError> {
        through_owner_lock(&self.lock_dir)?;
        let mut tasks = self.tasks.lock().unwrap();
        let task = tasks
            .get_mut(task_id)
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, task_id.to_string()))?;
        assert_eq!(task.status, update.expected_status);
        task.status = update.status;
        Ok(task.clone())
    }

    fn claim_execution_context(&self) -> Result<ClaimExecutionContext, OrbitError> {
        through_owner_lock(&self.lock_dir)?;
        Ok(ClaimExecutionContext {
            footprint: Vec::new(),
            workspace_id: "ws_owner".to_string(),
            task_id: TASK_ID.to_string(),
            claim_id: "claim-owner-lock".to_string(),
            machine_id: "hm_follower".to_string(),
            run_id: RUN_ID.to_string(),
            ship_mode: "pr".to_string(),
            base_branch: "agent-main".to_string(),
            landing_branch: "agent-main".to_string(),
            required_commands: Vec::new(),
        })
    }
}

fn action(host: &OwnerLockHost, name: &str, input: &Value) -> Result<Value, OrbitError> {
    execute_deterministic_action(host, name, &json!({}), input, false, &HashMap::new(), None)
}

/// The pin step of a claimed delivery: it carries the pre-publication
/// validation once the owner's claim context is read.
fn pin_validation(host: &OwnerLockHost, workspace: &Path) -> Result<Value, OrbitError> {
    action(
        host,
        "claim_validate",
        &json!({
            "workspace_path": workspace,
            "carry": {"decision": "passed", "tested_head": "c0ffee"},
        }),
    )
}

/// The owner's completion of a `no-diff-expected` delivery: it reads the
/// task and moves it from review to done.
fn complete(host: &OwnerLockHost, workspace: &Path) -> Result<Value, OrbitError> {
    action(
        host,
        "pr_complete",
        &json!({
            "workspace_path": workspace,
            "job_run_id": RUN_ID,
            "completed_task_ids": [TASK_ID],
            "no_diff_expected": true,
        }),
    )
}

#[test]
fn claim_validate_outlasts_an_owner_lock_hold_within_its_cap_and_fails_transient_past_it() {
    let root = tempfile::tempdir().unwrap();
    let host = OwnerLockHost::new(root.path(), TaskStatus::InProgress);

    let _holder = LockHolder::spawn(root.path(), true);
    let output = with_cap(GENEROUS_CAP, || pin_validation(&host, root.path()))
        .expect("the pin completes once the holder lets go");
    assert_eq!(output["decision"], "passed", "{output:#}");
    assert_wait_recorded(&output);

    let holder = LockHolder::spawn(root.path(), false);
    let error = with_cap(SHORT_CAP, || pin_validation(&host, root.path()))
        .expect_err("a holder that outlasts the cap fails the pin");
    assert_transient_naming(&error, &holder);
}

#[test]
fn pr_complete_outlasts_an_owner_lock_hold_within_its_cap_and_fails_transient_past_it() {
    let root = tempfile::tempdir().unwrap();
    let host = OwnerLockHost::new(root.path(), TaskStatus::Review);

    let _holder = LockHolder::spawn(root.path(), true);
    let output = with_cap(GENEROUS_CAP, || complete(&host, root.path()))
        .expect("completion finishes once the holder lets go");
    assert_eq!(output["completed_task_ids"], json!([TASK_ID]), "{output:#}");
    assert_eq!(host.status(), TaskStatus::Done);
    assert_wait_recorded(&output);

    let host = OwnerLockHost::new(root.path(), TaskStatus::Review);
    let holder = LockHolder::spawn(root.path(), false);
    let error = with_cap(SHORT_CAP, || complete(&host, root.path()))
        .expect_err("a holder that outlasts the cap fails completion");
    assert_transient_naming(&error, &holder);
    assert_eq!(
        host.status(),
        TaskStatus::Review,
        "a completion that never reached the owner leaves the task in review"
    );
}
