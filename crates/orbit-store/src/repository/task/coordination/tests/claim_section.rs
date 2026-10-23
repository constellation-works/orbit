//! ORB-15797: under drain load a claim mutation held its partition
//! exclusively for seconds, every reader queued behind it, and the hold's
//! report blamed the whole operation. The section must not decode the
//! partition's whole claim lifecycle or sync more than its commit needs, a
//! reader must see the committed bind after the section releases, and a long
//! hold must name the step that cost it.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use orbit_common::fs::io::FileLockOptions;
use orbit_common::test_env;
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::ClaimFailureClass;
use tempfile::TempDir;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

use super::super::TaskCommitBoundary;
use super::super::apply::slow_apply;
use super::super::boundary::section_probe::{SECTIONS, SectionCost};
use crate::Store;
use crate::compose::workspace_coordinated_backends;
use crate::contracts::*;
use crate::driver::sqlite::task_registry::{
    BindWorkspaceParams, TaskRegistryStore, task_registry_path,
};
use crate::repository::task::v2::TaskV2Store;

const PARTITION: &str = "orbit-test-123456";
const MACHINE: &str = "machine-a";
const HELD_PAST_THRESHOLD: &str = "advisory file lock held past its threshold";

/// One owner partition: its commit boundary and a task store inside it.
pub(super) struct Owner {
    pub(super) root: TempDir,
    pub(super) boundary: Arc<TaskCommitBoundary>,
    pub(super) tasks: Arc<TaskV2Store>,
    repo: PathBuf,
    orbit_dir: PathBuf,
}

/// One admitted claim and the invocation its unbound leaf acts under.
pub(super) struct Admitted {
    request_id: String,
    pub(super) claim: ExecutionClaim,
    ship: AdmissionShipContract,
}

impl Owner {
    /// A partition holding `tasks` backlog tasks with disjoint footprints.
    pub(super) fn open(tasks: usize) -> Self {
        Self::open_in(
            tempfile::tempdir_in(test_env::bulk_write_temp_dir()).expect("tempdir"),
            tasks,
        )
    }

    fn open_in(root: TempDir, tasks: usize) -> Self {
        let registry =
            TaskRegistryStore::open(&task_registry_path(root.path())).expect("open registry");
        let repo = root.path().join(PARTITION);
        let orbit_dir = repo.join(".orbit");
        std::fs::create_dir_all(&orbit_dir).expect("create orbit dir");
        registry
            .bind_workspace(BindWorkspaceParams {
                partition_id: Some(PARTITION.to_string()),
                slug: PARTITION.to_string(),
                repo_root: repo.clone(),
                workspace_path: repo.clone(),
                orbit_dir: orbit_dir.clone(),
                repo_fingerprint: None,
            })
            .expect("bind workspace");
        let store = Store::open(&root.path().join("state.sqlite")).expect("open store");
        let backends = workspace_coordinated_backends(registry.clone(), PARTITION.into(), store)
            .expect("compose");
        let owner = Self {
            tasks: Arc::new(TaskV2Store::with_commit_boundary(
                registry,
                PARTITION.into(),
                Arc::clone(&backends.commit_boundary),
            )),
            boundary: backends.commit_boundary,
            root,
            repo,
            orbit_dir,
        };
        for index in 0..tasks {
            owner.create(index);
        }
        owner
    }

    fn create(&self, index: usize) {
        self.tasks
            .create_task(TaskCreateParams {
                actor: "codex".into(),
                parent_id: None,
                title: format!("Task {index}"),
                description: "Detailed task description".into(),
                acceptance_criteria: vec!["First criterion".into()],
                dependencies: Vec::new(),
                relations: Vec::new(),
                tags: Vec::new(),
                required_tools: Vec::new(),
                plan: "1. Do the work".into(),
                execution_summary: String::new(),
                context_files: vec![format!("file:src/task_{index}.rs")],
                repo_root: None,
                created_by: Some("codex".into()),
                planned_by: None,
                implemented_by: None,
                status: TaskStatus::Backlog,
                priority: TaskPriority::Medium,
                complexity: None,
                task_type: TaskType::Feature,
                external_refs: Vec::new(),
                source_task_id: None,
                crew: None,
                crew_source: None,
                orchestrator: None,
                orchestrator_model: None,
                orchestrator_model_source: None,
                origin: None,
                comments: Vec::new(),
                context_creation: Vec::new(),
                creation_decision: None,
            })
            .expect("create task");
    }

    /// A second handle on this partition's boundary under `options`.
    pub(super) fn boundary_with(&self, options: FileLockOptions) -> TaskCommitBoundary {
        TaskCommitBoundary::new(
            Store::open(&self.root.path().join("state.sqlite")).expect("open store"),
            TaskRegistryStore::open(&task_registry_path(self.root.path())).expect("open registry"),
            PARTITION.into(),
        )
        .expect("open boundary")
        .with_lock_options(options)
    }

    pub(super) fn admit(&self, request_id: &str) -> Admitted {
        let request = request(request_id);
        let lookup = self
            .boundary
            .admit_task(
                &AdmissionIdentity::trusted_remote(ExecutionLocation {
                    machine_id: MACHINE.into(),
                    machine_name: None,
                }),
                &request,
                &AdmissionOrdering::default(),
                "test",
                &self.repo,
                &self.orbit_dir,
                &BTreeMap::new(),
                &BTreeMap::new(),
                &|_, _, _| Ok(None),
            )
            .expect("admit");
        let AdmissionLookup::Found { receipt, .. } = lookup else {
            panic!("expected a receipt: {lookup:?}");
        };
        Admitted {
            request_id: request_id.into(),
            claim: receipt.claim.expect("a claim"),
            ship: request.ship,
        }
    }
}

impl Admitted {
    fn unbound(&self) -> ClaimInvocation {
        ClaimInvocation::trusted_worker(
            self.claim.task_id.clone(),
            self.claim.claim_id.clone(),
            MACHINE.into(),
            None,
        )
    }

    fn run(&self) -> ClaimRun {
        ClaimRun {
            machine_id: MACHINE.into(),
            run_id: format!("leaf-{}", self.request_id),
        }
    }

    fn worker(&self) -> ClaimInvocation {
        ClaimInvocation::trusted_worker(
            self.claim.task_id.clone(),
            self.claim.claim_id.clone(),
            MACHINE.into(),
            Some(self.run()),
        )
    }

    pub(super) fn bind(&self, boundary: &TaskCommitBoundary) -> ClaimMutationResult {
        self.try_bind(boundary).expect("bind")
    }

    pub(super) fn try_bind(
        &self,
        boundary: &TaskCommitBoundary,
    ) -> Result<ClaimMutationResult, orbit_common::OrbitError> {
        boundary.mutate_execution_claim(
            Some(&self.unbound()),
            "bind",
            &ClaimMutation::Bind {
                run: self.run(),
                ship: self.ship.clone(),
            },
        )
    }
}

fn request(id: &str) -> AdmissionRequest {
    AdmissionRequest {
        request_id: id.into(),
        caller_version: "test".into(),
        caller_schema: DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
        caller_fingerprint: None,
        caller_before_pr: false,
        review_gate: false,
        run_context: AdmissionRunContext {
            run_id: "drain".into(),
            job_name: "auto".into(),
            machine_name: None,
        },
        ship: AdmissionShipContract {
            mode: "pr".into(),
            base_branch: "agent-main".into(),
            landing_branch: "agent-main".into(),
            before_pr: false,
            before_landing: false,
            completion: "review".into(),
            authorization_reference: None,
            review: None,
        },
        crews: None,
        os: None,
        browser_kit: false,
        admissible: None,
    }
}

/// The one exclusive section `op` entered on this thread.
fn section_of<T>(op: impl FnOnce() -> T) -> (T, SectionCost) {
    SECTIONS.with(|sections| sections.borrow_mut().clear());
    let value = op();
    let sections = SECTIONS.with(|sections| sections.borrow().clone());
    assert_eq!(sections.len(), 1, "one exclusive section: {sections:?}");
    (value, sections[0])
}

/// Claims already bound in the partition before the measured one.
const BOUND_CLAIMS: usize = 12;

/// Criterion: the admission sections of a claim's bind and of its budgeted
/// release decode only the claim's own lifecycle row, however many other
/// claims the partition holds, and request fewer durability syncs than before
/// ORB-15797. With 12 other bound claims a bind cost 6 syncs and 12 lifecycle
/// decodes, now 4 and 0; a release 8 and 14, now 5 and 2. A retried mutation
/// replays its receipt without an exclusive section at all.
#[test]
fn a_claim_mutation_section_decodes_its_own_claim_and_syncs_only_its_commit() {
    let owner = Owner::open(BOUND_CLAIMS + 1);
    for index in 0..BOUND_CLAIMS {
        owner
            .admit(&format!("earlier-{index}"))
            .bind(&owner.boundary);
    }
    let measured = owner.admit("measured");

    let (bound, bind) = section_of(|| measured.bind(&owner.boundary));
    assert_eq!(bound.phase, ExecutionClaimPhase::Running);
    assert_eq!(bind.kind, "admission", "{bind:?}");
    assert!(
        bind.state_decodes <= 1,
        "a bind decodes its own lifecycle row, not the partition's {BOUND_CLAIMS} others: \
         {bind:?}"
    );
    assert!(
        bind.durability_syncs <= 4,
        "a bind syncs its pending marker, its events and its envelope (was 6): {bind:?}"
    );

    // A retried bind is a read: no exclusive section, the recorded outcome.
    SECTIONS.with(|sections| sections.borrow_mut().clear());
    assert_eq!(measured.bind(&owner.boundary), bound);
    assert!(
        SECTIONS.with(|sections| sections.borrow().is_empty()),
        "a replayed mutation takes no exclusive section"
    );

    let (released, release) = section_of(|| {
        owner.boundary.mutate_execution_claim(
            Some(&measured.worker()),
            "release",
            &ClaimMutation::Release(ClaimEvidence {
                summary: Some("the leaf's worker died".into()),
                comment: Some("released for another attempt".into()),
                failure: Some(ClaimFailure {
                    class: ClaimFailureClass::Transient,
                    reason: "worker died".into(),
                    crew: None,
                    candidate: None,
                    provider_limit: false,
                    provider_refusal: false,
                    owner_busy: false,
                }),
                ..Default::default()
            }),
        )
    });
    assert_eq!(
        released.expect("release").phase,
        ExecutionClaimPhase::Revoked
    );
    assert!(
        release.state_decodes <= 2,
        "a budgeted release decodes its own claim's lifecycle row, for its authority and \
         its task's release budget, not the partition's {BOUND_CLAIMS} others: {release:?}"
    );
    assert!(
        release.durability_syncs <= 5,
        "a release syncs its marker, its events, its comment and its envelope (was 8): \
         {release:?}"
    );
}

type Events = Arc<Mutex<Vec<BTreeMap<String, String>>>>;

/// Observe diagnostics synchronously on the calling thread, including a
/// refused acquisition before the reader can return from `get_task`.
fn lock_events_observed<T>(
    op: impl FnOnce() -> T,
    observe: impl Fn(&BTreeMap<String, String>) + Send + Sync + 'static,
) -> (T, Vec<BTreeMap<String, String>>) {
    struct Capture<F>(Events, F);
    struct Fields<'a>(&'a mut BTreeMap<String, String>);
    impl tracing::field::Visit for Fields<'_> {
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.0.insert(field.name().into(), value.into());
        }
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.0.insert(
                field.name().into(),
                format!("{value:?}").trim_matches('"').into(),
            );
        }
    }
    impl<S: tracing::Subscriber, F: Fn(&BTreeMap<String, String>) + 'static> Layer<S> for Capture<F> {
        fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
            if event.metadata().target() != "orbit.common.fs.file_lock" {
                return;
            }
            let mut fields = BTreeMap::new();
            event.record(&mut Fields(&mut fields));
            (self.1)(&fields);
            self.0.lock().expect("events").push(fields);
        }
    }
    let events = Events::default();
    let dispatch =
        tracing::Dispatch::new(Registry::default().with(Capture(Arc::clone(&events), observe)));
    let value = tracing::dispatcher::with_default(&dispatch, op);
    let events = events.lock().expect("events").clone();
    (value, events)
}

/// Only a hang guard for channel handshakes and lock acquisition, not a
/// reader-performance assertion.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);
const WAITING_FOR_LOCK: &str = "still waiting for advisory file lock; a holder may be hung";

/// [ORB-16389] Criterion: a claim commit's apply runs after its admission
/// section has released the partition. While a slow apply holds the task's
/// bundle, another admission in the partition decides and an envelope read
/// already sees the decided bind, but a full task read waits on the bundle
/// lock and returns the whole bind, never a half-applied one.
#[test]
fn a_slow_claim_apply_holds_its_bundle_and_not_its_partition() {
    let owner = Owner::open(1);
    let admitted = owner.admit("slow");
    let task_id = admitted.claim.task_id.clone();
    let writer = owner.boundary_with(FileLockOptions {
        timeout: HANDSHAKE_TIMEOUT,
        warn_after: HANDSHAKE_TIMEOUT,
        record_shared_holders: true,
        warn_held_after: Some(Duration::ZERO),
        prefer_exclusive_waiters: true,
    });
    let other = Arc::new(owner.boundary_with(FileLockOptions {
        timeout: HANDSHAKE_TIMEOUT,
        warn_after: HANDSHAKE_TIMEOUT,
        record_shared_holders: true,
        warn_held_after: None,
        prefer_exclusive_waiters: true,
    }));
    let reader_tasks = Arc::new(TaskV2Store::with_commit_boundary(
        TaskRegistryStore::open(&task_registry_path(owner.root.path())).expect("open registry"),
        PARTITION.into(),
        Arc::clone(&other),
    ));
    let history_before = reader_tasks
        .get_task_row(&task_id, false)
        .expect("read")
        .expect("task")
        .history
        .len();

    let (released_tx, released) = mpsc::channel();
    let (applying_tx, applying) = mpsc::channel();
    let (release_tx, release) = mpsc::channel();
    let committer = std::thread::spawn(move || {
        slow_apply::set(move || {
            applying_tx.send(()).expect("signal the slow apply");
            release
                .recv_timeout(HANDSHAKE_TIMEOUT)
                .expect("release the apply after the reader contends");
        });
        lock_events_observed(
            || admitted.bind(&writer),
            move |event| {
                if event.get("message").map(String::as_str) == Some(HELD_PAST_THRESHOLD)
                    && event
                        .get("lock_path")
                        .is_some_and(|path| path.ends_with("/.task-commit.lock"))
                {
                    let _ = released_tx.send(());
                }
            },
        )
    });
    applying
        .recv_timeout(HANDSHAKE_TIMEOUT)
        .expect("the commit reaches its apply");
    released
        .try_recv()
        .expect("the admission section released the partition before its apply");

    // The partition admits another decision while the apply is held.
    other
        .with_admission(|| Ok(()))
        .expect("an admission passes a held apply");
    let envelopes = reader_tasks
        .task_envelopes_for_ids(&[task_id.clone()].into())
        .expect("envelope read");
    assert_eq!(
        envelopes[0].job_run_id.as_deref(),
        Some("leaf-slow"),
        "an envelope read sees the decided bind before its apply finishes"
    );

    let (waiting_tx, waiting) = mpsc::channel();
    let (read_tx, read) = mpsc::channel();
    let reader = {
        let reader_tasks = Arc::clone(&reader_tasks);
        let bundle_lock = format!("/.{task_id}.bundle.lock");
        std::thread::spawn(move || {
            let (task, _) = lock_events_observed(
                || reader_tasks.get_task_row(&task_id, false),
                move |event| {
                    if event.get("message").map(String::as_str) == Some(WAITING_FOR_LOCK)
                        && event
                            .get("lock_path")
                            .is_some_and(|path| path.ends_with(&bundle_lock))
                    {
                        waiting_tx.send(()).expect("signal reader contention");
                    }
                },
            );
            read_tx.send(task).expect("return the read result");
        })
    };
    // A lock warning, not a queued start message, proves the reader reached
    // the held bundle.
    waiting
        .recv_timeout(HANDSHAKE_TIMEOUT)
        .expect("a full read waits for the held bundle");
    assert!(
        matches!(read.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "a full reader cannot finish while the bind is half-applied"
    );
    release_tx.send(()).expect("release the writer");
    let row = read
        .recv_timeout(HANDSHAKE_TIMEOUT)
        .expect("reader completes after release")
        .expect("read")
        .expect("task");
    reader.join().expect("reader thread");
    let (bound, _) = committer.join().expect("committer thread");

    assert_eq!(bound.phase, ExecutionClaimPhase::Running);
    assert_eq!(
        row.task.job_run_id.as_deref(),
        Some("leaf-slow"),
        "the reader sees the committed bind"
    );
    assert!(
        row.history.len() > history_before,
        "the reader sees the bind's history with its envelope: {:?}",
        row.history
    );
}

/// Writers in the hold-time fixture, each mutating only its own claims.
const HOLD_WRITERS: usize = 4;
const HOLD_CLAIMS_PER_WRITER: usize = 2;
const HOLD_ROUNDS: usize = 10;

/// A directory on a real disk for the hold-time fixture: tmpfs syncs cost
/// nothing, so a hold measured there says nothing about a busy disk.
/// `ORBIT_HOLD_FIXTURE_DIR` overrides the default, the workspace `target/`.
fn hold_fixture_dir() -> PathBuf {
    let dir = std::env::var_os("ORBIT_HOLD_FIXTURE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
            manifest
                .ancestors()
                .nth(2)
                .unwrap_or(manifest)
                .join("target")
        });
    orbit_common::fs::io::create_private_dir_all(&dir).expect("create fixture dir");
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).expect("path");
        let mut stat = std::mem::MaybeUninit::<libc::statfs>::zeroed();
        // Safety: `statfs` reads the NUL-terminated path and fills only the
        // struct it is handed; a zero return means it did.
        assert_eq!(unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) }, 0);
        assert_ne!(
            unsafe { stat.assume_init() }.f_type,
            libc::TMPFS_MAGIC,
            "the hold-time fixture needs a real disk, not tmpfs: {}",
            dir.display()
        );
    }
    dir
}

fn percentile(sorted: &[Duration], fraction: f64) -> Duration {
    let rank = ((sorted.len() as f64 * fraction).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

/// [ORB-16389] Criterion 1, measured on demand: concurrent claim mutations
/// on a real disk, with every durability sync real, so each writer's syncs
/// are the others' fsync load. Reports the exclusive admission sections'
/// hold p50/p95 beside the mutations' own latency. Before the apply moved out
/// of the section the holds were p50 ~24 ms, p95 ~26-29 ms on an ext4 disk;
/// after, p50 ~1.2-1.3 ms and p95 ~1.5-1.9 ms. Run it explicitly:
/// `cargo test -p orbit-store --lib exclusive_claim_sections_stay_short -- --ignored --nocapture`.
#[test]
#[ignore = "measurement on a real disk with real syncs; run explicitly"]
#[allow(
    clippy::print_stderr,
    reason = "the measurement's report is its output"
)]
fn exclusive_claim_sections_stay_short_under_concurrent_durable_mutations() {
    let _durable = test_env::require_durable_syncs();
    let owner = Owner::open_in(
        tempfile::tempdir_in(hold_fixture_dir()).expect("tempdir"),
        HOLD_WRITERS * HOLD_CLAIMS_PER_WRITER,
    );
    let mut claims = (0..HOLD_WRITERS * HOLD_CLAIMS_PER_WRITER)
        .map(|index| {
            let admitted = owner.admit(&format!("hold-{index}"));
            admitted.bind(&owner.boundary);
            admitted
        })
        .collect::<Vec<_>>();
    let barrier = Arc::new(std::sync::Barrier::new(HOLD_WRITERS));
    let writers = (0..HOLD_WRITERS)
        .map(|_| {
            let mine = claims.split_off(claims.len() - HOLD_CLAIMS_PER_WRITER);
            let boundary = Arc::clone(&owner.boundary);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                SECTIONS.with(|sections| sections.borrow_mut().clear());
                barrier.wait();
                let mut latencies = Vec::new();
                for round in 0..HOLD_ROUNDS {
                    for claim in &mine {
                        let started = std::time::Instant::now();
                        let result = boundary
                            .mutate_execution_claim(
                                Some(&claim.worker()),
                                &format!("evidence-{round}"),
                                &ClaimMutation::Evidence(ClaimEvidence {
                                    summary: Some(format!("summary after round {round}")),
                                    comment: Some(format!("progress in round {round}")),
                                    ..Default::default()
                                }),
                            )
                            .expect("evidence");
                        latencies.push(started.elapsed());
                        assert_eq!(result.phase, ExecutionClaimPhase::Running);
                    }
                }
                let holds = SECTIONS.with(|sections| sections.borrow().clone());
                (mine, holds, latencies)
            })
        })
        .collect::<Vec<_>>();
    let mut holds = Vec::new();
    let mut syncs = Vec::new();
    let mut latencies = Vec::new();
    for writer in writers {
        let (mine, sections, mutation) = writer.join().expect("writer thread");
        for claim in &mine {
            let bundle = owner
                .boundary
                .bundle_store
                .read_bundle_lightweight(&claim.claim.task_id)
                .expect("read bundle");
            assert_eq!(
                bundle.comments.len(),
                HOLD_ROUNDS,
                "every committed comment landed on {}",
                claim.claim.task_id
            );
            assert_eq!(
                bundle.execution_summary.trim(),
                format!("summary after round {}", HOLD_ROUNDS - 1)
            );
        }
        let admissions = sections
            .iter()
            .filter(|section| section.kind == "admission");
        syncs.extend(admissions.clone().map(|section| section.durability_syncs));
        holds.extend(admissions.map(|section| section.held));
        latencies.extend(mutation);
    }
    assert_eq!(
        holds.len(),
        HOLD_WRITERS * HOLD_CLAIMS_PER_WRITER * HOLD_ROUNDS,
        "one exclusive section per mutation"
    );
    holds.sort();
    latencies.sort();
    let (hold_p50, hold_p95) = (percentile(&holds, 0.5), percentile(&holds, 0.95));
    let mutation_p50 = percentile(&latencies, 0.5);
    syncs.sort_unstable();
    assert_eq!(
        syncs.last(),
        Some(&0),
        "a claim mutation's admission section leaves every durability sync to its apply, \
         which runs after the section"
    );
    eprintln!(
        "exclusive admission hold over {} sections: p50 {hold_p50:?}, p95 {hold_p95:?}, \
         durability syncs {}..={}; mutation latency p50 {mutation_p50:?}, p95 {:?}",
        holds.len(),
        syncs[0],
        syncs[syncs.len() - 1],
        percentile(&latencies, 0.95),
    );
}
