//! Identity allocation and coordinated admission through the store's public
//! composition.
//!
//! - Task ids come from the host registry's allocator: monotonic, never
//!   wrapped past the ceiling, and exhausted cleanly, including when an import
//!   lands the maximum id.
//! - Job-run ids are never minted twice, even after the run they named is
//!   archived or deleted and the store reopened; local pull admission holds
//!   one slot per admitted request under a shared ceiling, which is the
//!   drain's live worker limit and nothing else.
//! - The owner's commit boundary admits at most one claim per request and per
//!   task under concurrent pulls, and a handoff is authorized only by the
//!   policy its claim was admitted under and only over unchanged evidence.
//!
//! Every test re-runs itself in a child of this binary with inherited Orbit
//! authority cleared, a disposable `HOME`, and a bounded wait.

#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(missing_docs)]

use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use orbit_common::OrbitError;
use orbit_store::Store;
use orbit_store::compose::{
    CoordinatedWorkspaceBackends, workspace_coordinated_backends, workspace_job_run_store,
};
use orbit_store::contracts::{
    ActiveTaskReservation, AdmissionIdentity, AdmissionLookup, AdmissionReceipt, AdmissionRequest,
    AdmissionRunContext, AdmissionShipContract, ClaimEvidence, ClaimInvocation, ClaimMutation,
    ClaimMutationResult, ClaimRun, ExecutionClaim, ExecutionLocation, HandoffObservation,
    JobRunStoreBackend, PullDestination, TaskCreateParams,
};
use orbit_store::maintenance::task_registry::{
    BindWorkspaceParams, TaskRegistryStore, WorkspaceCheckoutBinding, task_registry_path,
};
use orbit_store::workflow::task::{
    ExportSelection, ImportConflictPolicy, export_tasks, import_tasks,
};
use orbit_types::task::{
    CONTEXT_FILES_WIDENED_EVENT, ContextFilesWidening, ContextWideningStep, ORB_TASK_ID_MAX,
    TaskArtifact, TaskComplexity, TaskPriority, TaskStatus, TaskType,
};
use orbit_types::workflow::handoff::{
    HandoffArtifactRef, HandoffCandidate, HandoffDelivery, HandoffReview, HandoffReviewDisposition,
    HandoffValidationLog, LandingStartRequest, LandingStartState, TaskHandoff,
};
use orbit_types::workflow::{JobRunState, PipelineState, ReviewTiming, automation::SourceRevision};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

/// How long one isolated test may run before it is killed and fails.
const CHILD_DEADLINE: Duration = Duration::from_secs(120);

/// Run `test` alone in a child of this binary with inherited Orbit authority
/// cleared and a disposable `HOME`; `true` inside that child. The parent
/// waits up to [`CHILD_DEADLINE`] and reaps the child on any exit.
fn isolated(test: &str) -> bool {
    const MARKER: &str = "ORBIT_TEST_STORE_ALLOCATION_CHILD";
    if std::env::var(MARKER).as_deref() == Ok(test) {
        return true;
    }
    let home = TempDir::new().unwrap();
    let stdout_path = home.path().join("stdout.log");
    let stderr_path = home.path().join("stderr.log");
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(MARKER, test)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::fs::File::create(&stdout_path).unwrap())
        .stderr(std::fs::File::create(&stderr_path).unwrap());
    let mut child = ChildGuard(command.spawn().unwrap());
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break Some(status);
        }
        if started.elapsed() > CHILD_DEADLINE {
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(child);
    let read = |path: &Path| {
        let mut text = String::new();
        std::fs::File::open(path)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        text
    };
    let (stdout, stderr) = (read(&stdout_path), read(&stderr_path));
    let status = status
        .unwrap_or_else(|| panic!("`{test}` ran past {CHILD_DEADLINE:?}:\n{stdout}\n{stderr}"));
    assert!(status.success(), "`{test}` failed:\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("test result: ok. 1 passed;"),
        "the child must run `{test}` itself:\n{stdout}"
    );
    false
}

/// Kills and reaps the isolated child however the parent leaves.
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn is_exhausted<T: std::fmt::Debug>(result: Result<T, OrbitError>) -> bool {
    matches!(result, Err(OrbitError::Store(ref message)) if message.contains("exhausted"))
}

// ---------------------------------------------------------------------------
// Task id allocation
// ---------------------------------------------------------------------------

const PARTITION_ID: &str = "orbit-test-123456";

fn bind(registry: &TaskRegistryStore, root: &Path, partition_id: &str) -> WorkspaceCheckoutBinding {
    let repo = root.join(partition_id);
    let orbit_dir = repo.join(".orbit");
    std::fs::create_dir_all(&orbit_dir).unwrap();
    registry
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some(partition_id.to_string()),
            slug: "Orbit Test".to_string(),
            repo_root: repo.clone(),
            workspace_path: repo,
            orbit_dir,
            repo_fingerprint: None,
        })
        .expect("bind workspace")
}

#[test]
fn task_ids_are_monotonic_across_workspaces_and_batches() {
    if !isolated("task_ids_are_monotonic_across_workspaces_and_batches") {
        return;
    }
    let root = TempDir::new().unwrap();
    let registry = TaskRegistryStore::open(&task_registry_path(root.path())).unwrap();
    let first = bind(&registry, root.path(), "first-aaaaaa");
    let second = bind(&registry, root.path(), "second-bbbbbb");

    let mut minted = vec![
        registry.allocate_task_id(&first.partition_id).unwrap(),
        registry.allocate_task_id(&second.partition_id).unwrap(),
    ];
    minted.extend(registry.allocate_task_ids(&first.partition_id, 3).unwrap());
    minted.push(registry.allocate_task_id(&second.partition_id).unwrap());
    assert_eq!(
        minted,
        [
            "ORB-00000",
            "ORB-00001",
            "ORB-00002",
            "ORB-00003",
            "ORB-00004",
            "ORB-00005"
        ],
        "one host-wide counter serves every workspace and batch, in order"
    );

    // Reopening the registry (a later process) continues from the counter.
    drop(registry);
    let registry = TaskRegistryStore::open(&task_registry_path(root.path())).unwrap();
    assert_eq!(
        registry.allocate_task_id(&first.partition_id).unwrap(),
        "ORB-00006"
    );
    // Seeding never lowers the counter.
    assert!(registry.seed_allocator_start(3).is_err());
    assert_eq!(
        registry.allocate_task_id(&first.partition_id).unwrap(),
        "ORB-00007"
    );
}

/// The last id is minted once; every later allocation is refused as
/// exhausted, never wrapped to a low id that may already be taken. A batch
/// that would cross the ceiling is refused whole and leaves the counter.
#[test]
fn task_id_allocation_exhausts_at_the_ceiling_without_wrapping() {
    if !isolated("task_id_allocation_exhausts_at_the_ceiling_without_wrapping") {
        return;
    }
    let root = TempDir::new().unwrap();
    let registry = TaskRegistryStore::open(&task_registry_path(root.path())).unwrap();
    let workspace = bind(&registry, root.path(), PARTITION_ID);
    registry.seed_allocator_start(ORB_TASK_ID_MAX - 1).unwrap();

    assert!(
        is_exhausted(registry.allocate_task_ids(&workspace.partition_id, 3)),
        "a reservation past the ceiling is refused whole"
    );
    assert_eq!(
        registry.allocator_next_number().unwrap(),
        ORB_TASK_ID_MAX - 1
    );
    assert_eq!(
        registry.allocate_task_id(&workspace.partition_id).unwrap(),
        format!("ORB-{}", ORB_TASK_ID_MAX - 1)
    );
    assert_eq!(
        registry.allocate_task_id(&workspace.partition_id).unwrap(),
        format!("ORB-{ORB_TASK_ID_MAX}")
    );
    for _ in 0..2 {
        assert!(
            is_exhausted(registry.allocate_task_id(&workspace.partition_id)),
            "the allocator must stay exhausted rather than wrap"
        );
    }
    assert!(is_exhausted(registry.allocator_next_number()));
    assert!(is_exhausted(registry.seed_allocator_start(ORB_TASK_ID_MAX)));

    drop(registry);
    let registry = TaskRegistryStore::open(&task_registry_path(root.path())).unwrap();
    assert!(
        is_exhausted(registry.allocate_task_id(&workspace.partition_id)),
        "exhaustion survives a reopen"
    );
}

/// Importing a task that holds the maximum id leaves the target's allocator
/// exhausted, not wrapped to `ORB-00000`.
#[test]
fn importing_the_maximum_task_id_exhausts_the_target_allocator() {
    if !isolated("importing_the_maximum_task_id_exhausts_the_target_allocator") {
        return;
    }
    let source_root = TempDir::new().unwrap();
    let target_root = TempDir::new().unwrap();
    let archive = source_root.path().join("tasks.tar.zst");
    let max_id = format!("ORB-{ORB_TASK_ID_MAX}");

    let source = Coordinated::open(source_root.path());
    source
        .registry
        .seed_allocator_start(ORB_TASK_ID_MAX)
        .unwrap();
    assert_eq!(source.create_task("final id").id, max_id);
    export_tasks(
        &source.registry,
        PARTITION_ID,
        ExportSelection::All,
        &archive,
        Utc.with_ymd_and_hms(2026, 10, 3, 12, 0, 0).unwrap(),
    )
    .expect("export");

    let target = TaskRegistryStore::open(&task_registry_path(target_root.path())).unwrap();
    let outcome =
        import_tasks(&target, &archive, None, ImportConflictPolicy::Fail).expect("import");
    assert_eq!(outcome.tasks[0].final_id, max_id);
    assert!(target.find_task_binding(&max_id).unwrap().is_some());
    assert!(is_exhausted(target.allocator_next_number()));
    assert!(
        is_exhausted(target.allocate_task_id(PARTITION_ID)),
        "the next id after an imported maximum must be refused, not wrapped"
    );
}

// ---------------------------------------------------------------------------
// Job-run ids and pull occupancy
// ---------------------------------------------------------------------------

fn minute_stem(run_id: &str) -> &str {
    run_id.rsplit_once('-').expect("run id has a sequence").0
}

fn finish(jobs: &dyn JobRunStoreBackend, run_id: &str) {
    let at = Utc::now();
    assert!(
        jobs.mark_job_run_running(run_id, at, 42)
            .unwrap()
            .owns_execution()
    );
    assert!(
        jobs.finalize_job_run(run_id, JobRunState::Success, at, Some(1))
            .unwrap()
    );
}

/// [ORB-13599] Archive and delete remove a run's row; the id must stay
/// reserved so the next run minted in the same minute, by this process or a
/// later one, never takes it.
#[test]
fn removed_job_run_ids_are_never_reallocated() {
    if !isolated("removed_job_run_ids_are_never_reallocated") {
        return;
    }
    let root = TempDir::new().unwrap();
    let db = root.path().join("orbit.db");
    let mut minted = Vec::new();
    for delete in [false, true] {
        // The id stem is the wall-clock minute; retry the cycle if it crossed
        // a minute boundary, which would make the comparison vacuous.
        let (removed, next) = (0..3)
            .find_map(|_| {
                let jobs = workspace_job_run_store(Store::open(&db).unwrap(), "ws_a");
                let removed = jobs
                    .insert_job_run("job-a", 1, Utc::now(), None, None)
                    .unwrap();
                finish(jobs.as_ref(), &removed.run_id);
                if delete {
                    jobs.delete_job_run(&removed.run_id).unwrap();
                } else {
                    jobs.archive_job_run(&removed.run_id).unwrap();
                }
                assert!(jobs.get_job_run(&removed.run_id).unwrap().is_none());
                drop(jobs);

                let reopened = workspace_job_run_store(Store::open(&db).unwrap(), "ws_a");
                let next = reopened
                    .insert_job_run("job-a", 1, Utc::now(), None, None)
                    .unwrap();
                (minute_stem(&removed.run_id) == minute_stem(&next.run_id))
                    .then_some((removed.run_id, next.run_id))
            })
            .expect("two runs minted within one minute");
        assert_ne!(next, removed, "a removed run's id was minted again");
        minted.extend([removed, next]);
    }
    let unique = minted.iter().collect::<HashSet<_>>();
    assert_eq!(
        unique.len(),
        minted.len(),
        "every id minted once: {minted:?}"
    );
}

fn pull_request(run_id: &str, request_id: &str) -> AdmissionRequest {
    AdmissionRequest {
        request_id: request_id.into(),
        caller_version: "1".into(),
        caller_schema: orbit_store::contracts::DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
        caller_review_policy: "none".into(),
        run_context: AdmissionRunContext {
            run_id: run_id.into(),
            job_name: "workspace_auto_pipeline".into(),
            machine_name: None,
        },
        ship: AdmissionShipContract {
            mode: "local".into(),
            base_branch: "main".into(),
            landing_branch: "main".into(),
            review_policy: "none".into(),
            completion: "review".into(),
            authorization_reference: None,
        },
        crews: None,
    }
}

/// Twelve independent connections race for local pull admission under a
/// ceiling of three: exactly three are admitted and the store's occupancy
/// reading counts each admitted request as one slot.
#[test]
fn concurrent_pull_admission_obeys_the_shared_ceiling() {
    if !isolated("concurrent_pull_admission_obeys_the_shared_ceiling") {
        return;
    }
    const RACERS: usize = 12;
    const CEILING: usize = 3;
    let root = TempDir::new().unwrap();
    let db = root.path().join("pull.db");
    let jobs = workspace_job_run_store(Store::open(&db).unwrap(), "ws");
    let parent = jobs
        .insert_job_run("workspace_auto_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    jobs.write_run_state(
        &parent.run_id,
        &PipelineState::new(parent.run_id.clone(), parent.job_id, serde_json::json!({})),
    )
    .unwrap();
    let destination = PullDestination {
        owner_machine_id: "owner".into(),
        owner_workspace_id: "ws".into(),
        selector: "owner/ws".into(),
        execution_machine_id: "owner".into(),
    };
    // Bootstrap the feature before independent connections race for admission.
    jobs.local_pull_admissions().unwrap();

    let barrier = Arc::new(Barrier::new(RACERS));
    let racers = (0..RACERS)
        .map(|index| {
            let jobs = workspace_job_run_store(Store::open(&db).unwrap(), "ws");
            let destination = destination.clone();
            let request = pull_request(&parent.run_id, &format!("request-{index}"));
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                jobs.allocate_pull_request(&destination, &request, CEILING)
                    .unwrap()
                    .is_some()
            })
        })
        .collect::<Vec<_>>();
    let admitted = racers
        .into_iter()
        .map(|racer| usize::from(racer.join().unwrap()))
        .sum::<usize>();

    assert_eq!(admitted, CEILING);
    assert_eq!(jobs.local_pull_admissions().unwrap().len(), CEILING);
    assert_eq!(jobs.drain_leaf_occupancy().unwrap().occupied, CEILING);
    let late = pull_request(&parent.run_id, "late");
    assert!(
        jobs.allocate_pull_request(&destination, &late, CEILING)
            .unwrap()
            .is_none(),
        "a full ceiling admits nothing more"
    );
}

/// The drain's own limit is the only ceiling on pulled leaves: one drain
/// admits well past the ten a leaf definition used to allow, under the limit
/// an operator retuned on the drain's state rather than the one it was
/// submitted with.
#[test]
fn pull_admission_is_bound_only_by_the_drains_live_limit() {
    if !isolated("pull_admission_is_bound_only_by_the_drains_live_limit") {
        return;
    }
    const SUBMITTED: usize = 3;
    const RETUNED: u32 = 24;
    let root = TempDir::new().unwrap();
    let jobs = workspace_job_run_store(Store::open(&root.path().join("pull.db")).unwrap(), "ws");
    let parent = jobs
        .insert_job_run("workspace_pull_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    let mut state = PipelineState::new(
        parent.run_id.clone(),
        parent.job_id.clone(),
        serde_json::json!({}),
    );
    assert!(state.set_drain_worker_limit(RETUNED, SUBMITTED as u32, "cli".into(), None, None));
    jobs.write_run_state(&parent.run_id, &state).unwrap();
    let destination = PullDestination {
        owner_machine_id: "owner".into(),
        owner_workspace_id: "ws".into(),
        selector: "owner/ws".into(),
        execution_machine_id: "owner".into(),
    };

    let admitted = (0..RETUNED + 6)
        .filter(|index| {
            let request = pull_request(&parent.run_id, &format!("request-{index}"));
            jobs.allocate_pull_request(&destination, &request, SUBMITTED)
                .unwrap()
                .is_some()
        })
        .count();

    assert_eq!(admitted, RETUNED as usize, "the retuned limit governs");
    let occupancy = jobs.drain_leaf_occupancy().unwrap();
    assert_eq!(occupancy.occupied, RETUNED as usize);
    assert_eq!(
        occupancy.per_pipeline.get("task_claimed_local_pipeline"),
        Some(&(RETUNED as usize)),
        "one leaf definition holds every slot, with no ceiling of its own"
    );
}

/// A claimed leaf waiting for retry still owns one drain slot, while terminal
/// leaf history releases its slot and does not affect the capacity reading.
#[test]
fn retrying_claimed_leaf_occupies_capacity_and_terminal_history_does_not() {
    if !isolated("retrying_claimed_leaf_occupies_capacity_and_terminal_history_does_not") {
        return;
    }
    const CEILING: usize = 1;
    let root = TempDir::new().unwrap();
    let store = Store::open(&root.path().join("pull.db")).unwrap();
    let jobs = workspace_job_run_store(store.clone(), "ws");
    let parent = jobs
        .insert_job_run("workspace_pull_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    let mut state = PipelineState::new(
        parent.run_id.clone(),
        parent.job_id.clone(),
        serde_json::json!({}),
    );
    assert!(state.set_drain_worker_limit(CEILING as u32, CEILING as u32, "cli".into(), None, None));
    jobs.write_run_state(&parent.run_id, &state).unwrap();

    let retrying = jobs
        .insert_job_run(
            "task_claimed_local_pipeline",
            1,
            Utc::now(),
            Some(serde_json::json!({"task_ids": ["ORB-TEST"]})),
            None,
        )
        .unwrap();
    let terminal = jobs
        .insert_job_run(
            "task_claimed_pr_pipeline",
            1,
            Utc::now(),
            Some(serde_json::json!({"task_ids": ["ORB-OLD"]})),
            None,
        )
        .unwrap();
    finish(jobs.as_ref(), &terminal.run_id);
    let retrying_run_id = retrying.run_id.clone();
    store
        .with_transaction(|tx| {
            let changed = tx
                .connection()
                .execute(
                    "UPDATE job_runs SET state = 'retrying' WHERE workspace_id = ?1 AND run_id = ?2",
                    ["ws", retrying_run_id.as_str()],
                )
                .unwrap();
            assert_eq!(changed, 1, "retrying fixture run exists");
            Ok(())
        })
        .unwrap();

    let occupancy = jobs.drain_leaf_occupancy().unwrap();
    assert_eq!(
        occupancy.occupied, CEILING,
        "the retrying leaf occupies capacity and the terminal leaf is excluded"
    );
    assert_eq!(
        occupancy.per_pipeline.get("task_claimed_local_pipeline"),
        Some(&1),
        "retrying claimed leaves count toward their pipeline"
    );
    assert_eq!(
        occupancy.per_pipeline.get("task_claimed_pr_pipeline"),
        None,
        "terminal leaf history does not count"
    );

    let destination = PullDestination {
        owner_machine_id: "owner".into(),
        owner_workspace_id: "ws".into(),
        selector: "owner/ws".into(),
        execution_machine_id: "owner".into(),
    };
    let request = pull_request(&parent.run_id, "retrying-leaf-at-capacity");
    assert!(
        jobs.allocate_pull_request(&destination, &request, CEILING)
            .unwrap()
            .is_none(),
        "a retrying claimed leaf prevents admission at the ceiling"
    );
}

// ---------------------------------------------------------------------------
// Coordinated admission and handoff
// ---------------------------------------------------------------------------

/// One owner composition over a root directory. Opening a second one on the
/// same root is what a second process (or a restart) does.
struct Coordinated {
    registry: TaskRegistryStore,
    backends: CoordinatedWorkspaceBackends,
    orbit_dir: PathBuf,
}

impl Coordinated {
    fn open(root: &Path) -> Self {
        let registry = TaskRegistryStore::open(&task_registry_path(root)).unwrap();
        let binding = bind(&registry, root, PARTITION_ID);
        let store = Store::open(&root.join("state.sqlite")).unwrap();
        let backends =
            workspace_coordinated_backends(registry.clone(), binding.partition_id, store)
                .expect("compose");
        Self {
            registry,
            backends,
            orbit_dir: binding.orbit_dir,
        }
    }

    fn create_task(&self, title: &str) -> orbit_types::task::Task {
        self.create_task_in(title, &["src/lib.rs"])
    }

    fn create_task_in(&self, title: &str, selectors: &[&str]) -> orbit_types::task::Task {
        self.backends
            .task
            .task
            .create_task(TaskCreateParams {
                actor: "codex".to_string(),
                parent_id: None,
                title: title.to_string(),
                description: "Detailed task description".to_string(),
                acceptance_criteria: vec!["First criterion".to_string()],
                dependencies: Vec::new(),
                relations: Vec::new(),
                tags: Vec::new(),
                required_tools: Vec::new(),
                plan: "1. Do the work".to_string(),
                execution_summary: String::new(),
                context_files: selectors.iter().map(|s| (*s).to_string()).collect(),
                repo_root: None,
                created_by: Some("codex".to_string()),
                planned_by: None,
                implemented_by: None,
                status: TaskStatus::Backlog,
                priority: TaskPriority::High,
                complexity: Some(TaskComplexity::Medium),
                task_type: TaskType::Feature,
                external_refs: Vec::new(),
                source_task_id: None,
                crew: None,
                orchestrator: None,
                comments: Vec::new(),
            })
            .expect("create task")
    }

    fn task_status(&self, id: &str) -> TaskStatus {
        self.backends
            .task
            .task
            .get_task(id)
            .unwrap()
            .unwrap()
            .status
    }

    fn active_reservations(&self) -> Vec<ActiveTaskReservation> {
        self.backends
            .reservation
            .inspect_active_task_reservations(&self.orbit_dir.to_string_lossy(), Some(PARTITION_ID))
            .unwrap()
    }

    fn claims(&self) -> Vec<ExecutionClaim> {
        self.backends.commit_boundary.execution_claims().unwrap()
    }

    fn pull(&self, request: &AdmissionRequest) -> AdmissionReceipt {
        let location = ExecutionLocation {
            machine_id: "machine-a".into(),
            machine_name: Some("display".into()),
        };
        let lookup = self
            .backends
            .commit_boundary
            .admit_task(
                &AdmissionIdentity::trusted_remote(location),
                request,
                "test",
                self.orbit_dir.parent().unwrap(),
                &self.orbit_dir,
                &BTreeMap::new(),
            )
            .expect("pull");
        match lookup {
            AdmissionLookup::Found { receipt, .. } => *receipt,
            other => panic!("expected a receipt: {other:?}"),
        }
    }

    fn landing_starts(&self) -> Vec<LandingStartRequest> {
        self.backends.task.task.landing_start_requests().unwrap()
    }
}

fn owner_request(id: &str) -> AdmissionRequest {
    AdmissionRequest {
        request_id: id.into(),
        caller_version: "test".into(),
        caller_schema: orbit_store::contracts::DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
        caller_review_policy: "none".into(),
        run_context: AdmissionRunContext {
            run_id: "drain".into(),
            job_name: "auto".into(),
            machine_name: Some("untrusted-label".into()),
        },
        ship: AdmissionShipContract {
            mode: "pr".into(),
            base_branch: "agent-main".into(),
            landing_branch: "agent-main".into(),
            review_policy: "none".into(),
            completion: "review".into(),
            authorization_reference: None,
        },
        crews: None,
    }
}

/// Two owner compositions (two processes) replay the same request at the
/// same instant: one claim and one reservation exist, and both callers get
/// the identical receipt.
#[test]
fn simultaneous_retries_of_one_request_create_one_claim() {
    if !isolated("simultaneous_retries_of_one_request_create_one_claim") {
        return;
    }
    let root = TempDir::new().unwrap();
    let owner = Coordinated::open(root.path());
    owner.create_task("work");
    let first = Coordinated::open(root.path());
    let second = Coordinated::open(root.path());
    let barrier = Barrier::new(2);
    let request = owner_request("same");
    let (a, b) = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            first.pull(&request)
        });
        let b = scope.spawn(|| {
            barrier.wait();
            second.pull(&request)
        });
        (a.join().unwrap(), b.join().unwrap())
    });
    assert!(a.claim.is_some());
    assert_eq!(a, b, "a retry replays the original receipt");
    assert_eq!(owner.active_reservations().len(), 1);
    assert_eq!(owner.claims().len(), 1);
}

/// Distinct concurrent requests race over two tasks that share a file: at
/// most one claims, the other is deferred, and only one reservation exists.
#[test]
fn distinct_concurrent_requests_never_claim_overlapping_tasks() {
    if !isolated("distinct_concurrent_requests_never_claim_overlapping_tasks") {
        return;
    }
    let root = TempDir::new().unwrap();
    let owner = Coordinated::open(root.path());
    owner.create_task("one");
    owner.create_task("two");
    let other = Coordinated::open(root.path());
    let barrier = Barrier::new(2);
    let receipts = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            owner.pull(&owner_request("a"))
        });
        let b = scope.spawn(|| {
            barrier.wait();
            other.pull(&owner_request("b"))
        });
        [a.join().unwrap(), b.join().unwrap()]
    });
    assert_eq!(
        receipts.iter().filter(|r| r.claim.is_some()).count(),
        1,
        "overlapping footprints admit one claim: {receipts:?}"
    );
    assert_eq!(owner.active_reservations().len(), 1);
    assert_eq!(owner.claims().len(), 1);
}

const OWNER_POLICY: &str = "workspace-config:workflow.distributed_completion";

/// An admitted and bound claim with its owner-held validation logs and the
/// handoff a worker would submit for it.
struct Delivery {
    _root: TempDir,
    owner: Coordinated,
    claim: ExecutionClaim,
    handoff: TaskHandoff,
    /// The owner's `workflow.required_validation_commands`.
    required: Vec<String>,
}

impl Delivery {
    fn admit(ship: AdmissionShipContract) -> Self {
        Self::admit_in(ship, &["src/lib.rs"])
    }

    fn admit_in(ship: AdmissionShipContract, selectors: &[&str]) -> Self {
        Self::admit_requiring(ship, selectors, &["build", "test"])
    }

    /// An admitted claim whose owner requires `required`, handed off with one
    /// passing log per required command.
    fn admit_requiring(ship: AdmissionShipContract, selectors: &[&str], required: &[&str]) -> Self {
        let root = TempDir::new().unwrap();
        let owner = Coordinated::open(root.path());
        owner.create_task_in("typed handoff", selectors);
        let mut request = owner_request("first");
        request.ship = ship.clone();
        let claim = owner.pull(&request).claim.expect("claim");
        let unbound = ClaimInvocation::trusted_worker(
            claim.task_id.clone(),
            claim.claim_id.clone(),
            claim.executed_on.machine_id.clone(),
            None,
        );
        owner
            .backends
            .commit_boundary
            .mutate_execution_claim(
                Some(&unbound),
                "bind",
                &ClaimMutation::Bind {
                    run: leaf_run(&claim),
                    ship,
                },
            )
            .expect("bind");
        let handoff = handoff_with_logs(&owner, &claim, required);
        Self {
            _root: root,
            owner,
            claim,
            handoff,
            required: required.iter().map(ToString::to_string).collect(),
        }
    }

    fn observation(&self, policy: Option<&str>) -> HandoffObservation {
        HandoffObservation {
            footprint_widening: self.handoff.footprint_widening.clone(),
            candidate: self.handoff.candidate.clone(),
            required_commands: self.required.clone(),
            owner_completion_authority: policy.map(str::to_string),
        }
    }

    fn worker(&self) -> ClaimInvocation {
        ClaimInvocation::trusted_worker(
            self.claim.task_id.clone(),
            self.claim.claim_id.clone(),
            self.claim.executed_on.machine_id.clone(),
            Some(leaf_run(&self.claim)),
        )
    }

    fn operator(&self) -> ClaimInvocation {
        ClaimInvocation::trusted_operator(
            self.claim.task_id.clone(),
            self.claim.claim_id.clone(),
            "owner-operator".into(),
        )
    }

    fn accept(&self, policy: Option<&str>) -> Result<ClaimMutationResult, OrbitError> {
        self.owner.backends.commit_boundary.mutate_execution_claim(
            Some(
                &self
                    .worker()
                    .with_handoff_observation(self.observation(policy)),
            ),
            "handoff",
            &ClaimMutation::AcceptHandoff(self.handoff.clone()),
        )
    }

    fn approve(&self) -> Result<ClaimMutationResult, OrbitError> {
        let accepted = self
            .owner
            .backends
            .commit_boundary
            .accepted_handoff(&self.claim.claim_id)?;
        self.owner.backends.commit_boundary.mutate_execution_claim(
            Some(
                &self
                    .operator()
                    .with_handoff_observation(self.observation(None)),
            ),
            "approval",
            &ClaimMutation::ApproveHandoff {
                handoff_id: accepted.handoff_id,
                candidate: self.handoff.candidate.clone(),
            },
        )
    }

    fn merge_intent(&self, policy: Option<&str>) -> Result<ClaimMutationResult, OrbitError> {
        self.owner.backends.commit_boundary.mutate_execution_claim(
            Some(
                &self
                    .operator()
                    .with_handoff_observation(self.observation(policy)),
            ),
            "merge-intent",
            &ClaimMutation::MergeIntent {
                intent_id: "sent-merge".into(),
                resolved: false,
                evidence: "pinned provider request".into(),
            },
        )
    }
}

fn leaf_run(claim: &ExecutionClaim) -> ClaimRun {
    ClaimRun {
        machine_id: claim.executed_on.machine_id.clone(),
        run_id: "leaf".into(),
    }
}

fn done_ship() -> AdmissionShipContract {
    let mut ship = owner_request("first").ship;
    ship.completion = "done".into();
    ship.authorization_reference = Some(OWNER_POLICY.into());
    ship
}

/// The worker's handoff over a pinned candidate, with one validation log per
/// command in `commands` persisted on the owner as claim evidence.
fn handoff_with_logs(
    owner: &Coordinated,
    claim: &ExecutionClaim,
    commands: &[&str],
) -> TaskHandoff {
    let mut handoff = TaskHandoff {
        schema_version: 1,
        workspace_id: PARTITION_ID.into(),
        task_id: claim.task_id.clone(),
        claim_id: claim.claim_id.clone(),
        machine_id: claim.executed_on.machine_id.clone(),
        run_id: "leaf".into(),
        candidate: HandoffCandidate {
            repository: "owner/repository".into(),
            source_branch: "attempt/leaf".into(),
            base_branch: "agent-main".into(),
            landing_branch: "agent-main".into(),
            candidate: SourceRevision {
                commit: "a".repeat(40),
                tree: "b".repeat(40),
            },
            base: SourceRevision {
                commit: "c".repeat(40),
                tree: "d".repeat(40),
            },
            delivery: HandoffDelivery::PullRequest { number: 42 },
        },
        review: HandoffReview {
            policy: ReviewTiming::None,
            disposition: HandoffReviewDisposition::NotRequired,
        },
        execution_summary: "Outcome: success\nRequired checks passed for pinned candidate".into(),
        validation: vec![],
        footprint_widening: vec![],
    };
    let artifacts = commands
        .iter()
        .map(|command| {
            let log = HandoffValidationLog {
                schema_version: 1,
                workspace_id: handoff.workspace_id.clone(),
                task_id: handoff.task_id.clone(),
                claim_id: handoff.claim_id.clone(),
                machine_id: handoff.machine_id.clone(),
                run_id: handoff.run_id.clone(),
                candidate: handoff.candidate.clone(),
                tested_head: handoff.candidate.candidate.commit.clone(),
                command: (*command).into(),
                exit_code: 0,
                output: format!("{command} captured output"),
            };
            TaskArtifact {
                path: format!("{command}.json"),
                content: serde_json::to_vec(&log).unwrap(),
                media_type: "application/json".into(),
                created_by: None,
            }
        })
        .collect::<Vec<_>>();
    handoff.validation = artifacts
        .iter()
        .map(|artifact| HandoffArtifactRef {
            path: artifact.path.clone(),
            sha256: format!("{:x}", Sha256::digest(&artifact.content)),
        })
        .collect();
    if artifacts.is_empty() {
        return handoff;
    }
    let worker = ClaimInvocation::trusted_worker(
        claim.task_id.clone(),
        claim.claim_id.clone(),
        claim.executed_on.machine_id.clone(),
        Some(leaf_run(claim)),
    );
    owner
        .backends
        .commit_boundary
        .mutate_execution_claim(
            Some(&worker),
            "validation-logs",
            &ClaimMutation::Evidence(ClaimEvidence {
                artifacts,
                ..Default::default()
            }),
        )
        .expect("persist owner logs");
    handoff
}

/// Completion authority is the owner policy the claim was admitted under,
/// still granted by the owner when the handoff arrives and when it lands.
#[test]
fn handoff_authorization_follows_the_owner_policy_at_admission() {
    if !isolated("handoff_authorization_follows_the_owner_policy_at_admission") {
        return;
    }
    // A `done` contract admitted under the policy is authorized at acceptance
    // and its landing request is pending.
    let granted = Delivery::admit(done_ship());
    granted.accept(Some(OWNER_POLICY)).expect("handoff");
    assert_eq!(
        granted.owner.task_status(&granted.claim.task_id),
        TaskStatus::Review
    );
    let starts = granted.owner.landing_starts();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].state, LandingStartState::Pending);
    // Landing rechecks the owner's current policy.
    let withdrawn = granted
        .merge_intent(None)
        .expect_err("withdrawn policy fences landing");
    assert!(
        withdrawn.to_string().contains("policy withdrawn"),
        "{withdrawn}"
    );
    granted
        .merge_intent(Some(OWNER_POLICY))
        .expect("policy still granted");

    // The same contract, accepted while the owner grants no policy or another
    // one, waits in review for an operator.
    for policy in [None, Some("workspace-config:some.other_key")] {
        let waiting = Delivery::admit(done_ship());
        waiting
            .accept(policy)
            .expect("a valid delivery is still accepted");
        assert_eq!(
            waiting.owner.task_status(&waiting.claim.task_id),
            TaskStatus::Review
        );
        assert!(
            waiting.owner.landing_starts().is_empty(),
            "no authority for {policy:?}"
        );
        waiting.approve().expect("operator approves");
        assert_eq!(waiting.owner.landing_starts().len(), 1);
    }

    // A `review` contract is never authorized by a policy the owner grants
    // after admission.
    let review = Delivery::admit(owner_request("first").ship);
    review.accept(Some(OWNER_POLICY)).expect("handoff");
    assert!(review.owner.landing_starts().is_empty());
}

/// An owner whose `workflow.required_validation_commands` is empty requires
/// no check: it accepts a handoff carrying no validation logs, authorizes it
/// under its completion policy and lets it land. The handoff's other checks
/// still hold — a candidate the owner observes differently is refused, and an
/// owner that does require commands refuses the same log-free handoff.
#[test]
fn an_owner_requiring_no_commands_accepts_and_lands_a_handoff_without_logs() {
    if !isolated("an_owner_requiring_no_commands_accepts_and_lands_a_handoff_without_logs") {
        return;
    }
    let delivery = Delivery::admit_requiring(done_ship(), &["src/lib.rs"], &[]);
    assert!(delivery.handoff.validation.is_empty());

    let mut moved = delivery.observation(Some(OWNER_POLICY));
    moved.candidate.candidate.commit = "e".repeat(40);
    let moved = delivery
        .owner
        .backends
        .commit_boundary
        .mutate_execution_claim(
            Some(&delivery.worker().with_handoff_observation(moved)),
            "moved",
            &ClaimMutation::AcceptHandoff(delivery.handoff.clone()),
        )
        .expect_err("candidate integrity still gates a handoff with no required commands");
    assert!(moved.to_string().contains("candidate"), "{moved}");

    delivery
        .accept(Some(OWNER_POLICY))
        .expect("no required command is no gate");
    assert_eq!(
        delivery.owner.task_status(&delivery.claim.task_id),
        TaskStatus::Review
    );
    let starts = delivery.owner.landing_starts();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].state, LandingStartState::Pending);
    delivery
        .merge_intent(Some(OWNER_POLICY))
        .expect("landing rechecks the same empty requirement");

    let mut requiring = Delivery::admit_requiring(done_ship(), &["src/lib.rs"], &[]);
    requiring.required = vec!["build".into()];
    let refused = requiring
        .accept(Some(OWNER_POLICY))
        .expect_err("a required command needs its log");
    assert!(
        refused.to_string().contains("required validation missing"),
        "{refused}"
    );
    assert_eq!(
        requiring.owner.task_status(&requiring.claim.task_id),
        TaskStatus::InProgress
    );
}

/// Validation evidence replaced in owner storage after acceptance blocks an
/// operator's approval and, once approved, the landing itself.
#[test]
fn replaced_handoff_evidence_blocks_approval_and_landing() {
    if !isolated("replaced_handoff_evidence_blocks_approval_and_landing") {
        return;
    }
    for approved in [false, true] {
        let delivery = Delivery::admit(owner_request("first").ship);
        delivery.accept(None).expect("handoff");
        if approved {
            delivery.approve().expect("approve over intact evidence");
        }
        // Owner storage replaced behind the claim, not an authorized
        // post-handoff worker write: a well-formed log for the same candidate
        // whose bytes no longer match the digest the handoff pinned.
        let log = delivery
            .owner
            .registry
            .canonical_task_bundle_path(PARTITION_ID, &delivery.claim.task_id)
            .unwrap()
            .join("artifacts/files/build.json");
        assert!(
            log.is_file(),
            "the owner holds the validation log at {}",
            log.display()
        );
        let mut replaced: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&log).unwrap()).unwrap();
        replaced["output"] = "replaced output".into();
        std::fs::write(&log, serde_json::to_vec(&replaced).unwrap()).unwrap();
        if approved {
            assert!(
                delivery.merge_intent(None).is_err(),
                "landing must refuse replaced evidence"
            );
        } else {
            assert!(
                delivery.approve().is_err(),
                "approval must refuse replaced evidence"
            );
            assert!(delivery.owner.landing_starts().is_empty());
        }
    }
}

/// Owner acceptance journals selectors, provenance history and the enlarged
/// review lock together. ORB-13990: a claimed implementer may change any path
/// the work requires, so every path an owner can track widens — another
/// crate, docs, a new top-level `tests/` file. Only metadata, environment
/// and non-normal paths are refused, leaving the claim and task unchanged.
#[test]
fn handoff_widening_accepts_any_safe_path_and_replays_once() {
    if !isolated("handoff_widening_accepts_any_safe_path_and_replays_once") {
        return;
    }
    for (path, allowed) in [
        ("crates/touched/src/split/new.rs", true),
        ("crates/other/tests/test_env.rs", true),
        ("crates/other/src/lib.rs", true),
        ("docs/new.md", true),
        ("tests/claim_refusal.rs", true),
        ("crates/touched/.env", false),
        ("crates/touched/.orbit/new", false),
        ("crates/touched/../other/src/lib.rs", false),
    ] {
        let mut delivery = Delivery::admit_in(
            owner_request("template").ship,
            &["file:crates/touched/src/lib.rs"],
        );
        delivery.handoff.footprint_widening = vec![path.into()];
        let before = delivery
            .owner
            .backends
            .task
            .task
            .get_task(&delivery.claim.task_id)
            .unwrap()
            .unwrap();
        let result = delivery.accept(None);
        if allowed {
            result.unwrap_or_else(|error| panic!("{path} widening accepted: {error}"));
            delivery.accept(None).expect("lost response replay");
            let task = delivery
                .owner
                .backends
                .task
                .task
                .get_task(&delivery.claim.task_id)
                .unwrap()
                .unwrap();
            assert_eq!(task.status, TaskStatus::Review);
            assert!(task.context_files.contains(&format!("file:{path}")));
            assert!(
                task.context_files
                    .contains(&"file:crates/touched/src/lib.rs".into())
            );
            let history = delivery
                .owner
                .backends
                .task
                .history
                .get_task_history(&delivery.claim.task_id)
                .unwrap()
                .unwrap();
            let widenings = history
                .iter()
                .filter(|e| e.event == CONTEXT_FILES_WIDENED_EVENT)
                .map(|e| ContextFilesWidening::from_note(e.note.as_deref().unwrap()).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                widenings.len(),
                1,
                "replay must not duplicate widening history"
            );
            assert_eq!(widenings[0].step, ContextWideningStep::Implement);
            assert_eq!(widenings[0].run_id, delivery.handoff.run_id);
            assert_eq!(widenings[0].selectors, vec![format!("file:{path}")]);
            let current = delivery.owner.claims().pop().unwrap();
            assert!(current.footprint.contains(&format!("file:{path}")));
            // Admission replay remains the immutable original receipt.
            let mut request = owner_request("first");
            request.ship = owner_request("template").ship;
            assert_eq!(
                delivery.owner.pull(&request).claim.unwrap().footprint,
                delivery.claim.footprint
            );
        } else {
            let error = result.unwrap_err().to_string();
            assert!(error.contains(path), "exact refused path: {error}");
            assert_eq!(
                delivery
                    .owner
                    .backends
                    .task
                    .task
                    .get_task(&delivery.claim.task_id)
                    .unwrap()
                    .unwrap()
                    .context_files,
                before.context_files
            );
            assert_eq!(
                delivery.owner.task_status(&delivery.claim.task_id),
                TaskStatus::InProgress
            );
        }
    }
}

/// The widening request must equal the owner's independently observed diff,
/// while a competing claim or lock on the added path no longer refuses it:
/// footprint locks are a scheduling hint, not a delivery gate.
#[test]
fn handoff_widening_accepts_locked_paths_and_refuses_observation_mismatch() {
    if !isolated("handoff_widening_accepts_locked_paths_and_refuses_observation_mismatch") {
        return;
    }
    let path = "src/new/nested.rs";
    let mut delivery = Delivery::admit(owner_request("template").ship);
    delivery.handoff.footprint_widening = vec![path.into()];
    let mut observed = delivery.observation(None);
    observed.footprint_widening.clear();
    let error = delivery
        .owner
        .backends
        .commit_boundary
        .mutate_execution_claim(
            Some(&delivery.worker().with_handoff_observation(observed)),
            "mismatch",
            &ClaimMutation::AcceptHandoff(delivery.handoff.clone()),
        )
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(path),
        "request must equal independently observed diff: {error}"
    );
    assert_eq!(
        delivery.owner.task_status(&delivery.claim.task_id),
        TaskStatus::InProgress
    );

    for lock in ["claim", "reservation", "status"] {
        let mut delivery = Delivery::admit(owner_request("template").ship);
        let competitor = delivery
            .owner
            .create_task_in("competing", &["file:src/new/nested.rs"]);
        match lock {
            "claim" => {
                delivery
                    .owner
                    .pull(&owner_request("second"))
                    .claim
                    .expect("second claim");
            }
            "reservation" => {
                let reserved = delivery
                    .owner
                    .backends
                    .reservation
                    .reserve_task_reservation(
                        orbit_store::contracts::TaskReservationReserveParams {
                            workspace_orbit_dir: delivery
                                .owner
                                .orbit_dir
                                .to_string_lossy()
                                .into_owned(),
                            workspace_id: Some(PARTITION_ID.into()),
                            task_ids: vec![competitor.id.clone()],
                            requested_files: vec![format!("file:{path}")],
                            actor: "owner".into(),
                            ttl_seconds: 600,
                            owner_run_id: None,
                            owner_metadata_json: None,
                        },
                    )
                    .unwrap();
                assert!(reserved.reserved);
            }
            _ => {
                delivery
                    .owner
                    .backends
                    .commit_boundary
                    .commit_task_transition(&orbit_store::contracts::TaskCoordinationCommitParams {
                        task_id: competitor.id.clone(),
                        actor: "owner".into(),
                        expected_status: vec![TaskStatus::Backlog],
                        status: Some(TaskStatus::InProgress),
                        ..Default::default()
                    })
                    .unwrap();
            }
        }
        delivery.handoff.footprint_widening = vec![path.into()];
        delivery
            .accept(None)
            .unwrap_or_else(|error| panic!("a live {lock} lock does not gate delivery: {error}"));
        assert_eq!(
            delivery.owner.task_status(&delivery.claim.task_id),
            TaskStatus::Review
        );
    }
}
