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
use super::super::boundary::section_probe::{SECTIONS, SectionCost};
use super::super::commit::slow_apply;
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
struct Owner {
    root: TempDir,
    boundary: Arc<TaskCommitBoundary>,
    tasks: Arc<TaskV2Store>,
    repo: PathBuf,
    orbit_dir: PathBuf,
}

/// One admitted claim and the invocation its unbound leaf acts under.
struct Admitted {
    request_id: String,
    claim: ExecutionClaim,
    ship: AdmissionShipContract,
}

impl Owner {
    /// A partition holding `tasks` backlog tasks with disjoint footprints.
    fn open(tasks: usize) -> Self {
        let root = tempfile::tempdir_in(test_env::bulk_write_temp_dir()).expect("tempdir");
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
                comments: Vec::new(),
                context_creation: Vec::new(),
            })
            .expect("create task");
    }

    /// A second handle on this partition's boundary under `options`.
    fn boundary_with(&self, options: FileLockOptions) -> TaskCommitBoundary {
        TaskCommitBoundary::new(
            Store::open(&self.root.path().join("state.sqlite")).expect("open store"),
            TaskRegistryStore::open(&task_registry_path(self.root.path())).expect("open registry"),
            PARTITION.into(),
        )
        .expect("open boundary")
        .with_lock_options(options)
    }

    fn admit(&self, request_id: &str) -> Admitted {
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

    fn bind(&self, boundary: &TaskCommitBoundary) -> ClaimMutationResult {
        boundary
            .mutate_execution_claim(
                Some(&self.unbound()),
                "bind",
                &ClaimMutation::Bind {
                    run: self.run(),
                    ship: self.ship.clone(),
                },
            )
            .expect("bind")
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
        held_recovery: false,
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

/// Every lock diagnostic emitted on this thread while `op` runs, as fields.
fn lock_events<T>(op: impl FnOnce() -> T) -> (T, Vec<BTreeMap<String, String>>) {
    lock_events_observed(op, |_| {})
}

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
/// Let the channel-held apply step accumulate diagnostic time before the
/// reader's contention warning; this is not a bound on reader completion.
const CONTENTION_WARN_AFTER: Duration = Duration::from_millis(200);
const WAITING_FOR_LOCK: &str = "still waiting for advisory file lock; a holder may be hung";

/// Criterion: a `get_task` reader that arrives while a claim commit holds
/// the partition cannot return until that commit releases, and the
/// commit's hold report names the commit step that held it, not the
/// section's whole operation.
#[test]
fn a_reader_waits_out_a_slow_claim_commit_and_no_longer() {
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
    let reader_boundary = Arc::new(owner.boundary_with(FileLockOptions {
        timeout: HANDSHAKE_TIMEOUT,
        warn_after: CONTENTION_WARN_AFTER,
        record_shared_holders: true,
        warn_held_after: None,
        prefer_exclusive_waiters: true,
    }));
    let reader_tasks = TaskV2Store::with_commit_boundary(
        TaskRegistryStore::open(&task_registry_path(owner.root.path())).expect("open registry"),
        PARTITION.into(),
        reader_boundary,
    );

    let (applying_tx, applying) = mpsc::channel();
    let (release_tx, release) = mpsc::channel();
    let committer = std::thread::spawn(move || {
        slow_apply::set(move || {
            applying_tx.send(()).expect("signal the slow apply");
            release
                .recv_timeout(HANDSHAKE_TIMEOUT)
                .expect("release the apply after the reader contends");
        });
        lock_events(|| admitted.bind(&writer))
    });
    applying
        .recv_timeout(HANDSHAKE_TIMEOUT)
        .expect("the commit reaches its apply step");
    let (waiting_tx, waiting) = mpsc::channel();
    let (read_tx, read) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let (task, _) = lock_events_observed(
            || TaskStoreBackend::get_task(&reader_tasks, &task_id),
            move |event| {
                if event.get("message").map(String::as_str) == Some(WAITING_FOR_LOCK)
                    && event
                        .get("lock_path")
                        .is_some_and(|path| path.ends_with("/.task-commit.lock"))
                {
                    waiting_tx.send(()).expect("signal reader contention");
                }
            },
        );
        read_tx.send(task).expect("return the read result");
    });
    // The writer cannot leave apply even if the reader is descheduled before
    // entering get_task. A lock refusal, not a queued start message, proves
    // the reader reached the held partition.
    waiting
        .recv_timeout(HANDSHAKE_TIMEOUT)
        .expect("get_task waits for the held partition");
    assert!(
        matches!(read.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "a reader cannot finish while the bind is half-applied"
    );
    release_tx.send(()).expect("release the writer");
    let task = read
        .recv_timeout(HANDSHAKE_TIMEOUT)
        .expect("reader completes after release")
        .expect("read")
        .expect("task");
    reader.join().expect("reader thread");
    let (bound, events) = committer.join().expect("committer thread");

    assert_eq!(bound.phase, ExecutionClaimPhase::Running);
    assert_eq!(
        task.job_run_id.as_deref(),
        Some("leaf-slow"),
        "the reader sees the committed bind, never a half-applied one"
    );

    let report = events
        .iter()
        .find(|event| {
            event.get("message").map(String::as_str) == Some(HELD_PAST_THRESHOLD)
                && event
                    .get("lock_path")
                    .is_some_and(|path| path.ends_with("/.task-commit.lock"))
        })
        .unwrap_or_else(|| panic!("a partition hold report in {events:#?}"));
    assert_eq!(
        report.get("slowest_step").map(String::as_str),
        Some("commit apply"),
        "the report names the commit step the hold went to: {report:?}"
    );
}
