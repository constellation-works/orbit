//! ORB-14724: a pull admission holds its partition's commit boundary
//! exclusively, so the partition's task writes wait while it runs. The
//! section must cost what one decision needs, not what the partition holds,
//! and a candidate that changes between selection and the section must never
//! be admitted from the selection's stale view.
//!
//! ORB-15106: it held the host lock exclusively too, so every workspace's
//! task reads and writes on the host waited on one drain's admissions. Only a
//! decision that reads another partition's dependency may still stall other
//! partitions.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::io::FileLockOptions;
use orbit_common::test_env::{self, FixtureProgress};
use orbit_types::task::{Task, TaskComment, TaskHistoryEntry, TaskPriority, TaskStatus, TaskType};
use tempfile::TempDir;

use super::super::TaskCommitBoundary;
use super::super::admission::ValidationHold;
use super::super::boundary::section_probe::{SECTIONS, SectionCost};
use super::super::selection::after_selection;
use crate::Store;
use crate::compose::workspace_coordinated_backends;
use crate::contracts::*;
use crate::driver::sqlite::task_registry::{
    BindWorkspaceParams, TaskRegistryStore, task_registry_path,
};
use crate::repository::task::v2::TaskV2Store;

const PARTITION: &str = "orbit-test-123456";
const OTHER_PARTITION: &str = "other-test-654321";

/// One owner partition: its commit boundary and a task store inside it.
struct Owner {
    _root: Option<TempDir>,
    root: PathBuf,
    partition: &'static str,
    boundary: Arc<TaskCommitBoundary>,
    tasks: Arc<TaskV2Store>,
    repo: PathBuf,
    orbit_dir: PathBuf,
}

impl Owner {
    fn open() -> Self {
        // A partition seed fsyncs every task; keep it off a disk-backed `TMPDIR`.
        let root = tempfile::tempdir_in(test_env::bulk_write_temp_dir()).expect("tempdir");
        let mut owner = Self::open_in(root.path(), PARTITION);
        owner._root = Some(root);
        owner
    }

    /// Another workspace partition on the same host: the same registry,
    /// journal and host lock.
    fn neighbour(&self) -> Self {
        Self::open_in(&self.root, OTHER_PARTITION)
    }

    fn open_in(root: &Path, partition: &'static str) -> Self {
        let registry = TaskRegistryStore::open(&task_registry_path(root)).expect("open registry");
        let repo = root.join(partition);
        let orbit_dir = repo.join(".orbit");
        std::fs::create_dir_all(&orbit_dir).expect("create orbit dir");
        registry
            .bind_workspace(BindWorkspaceParams {
                partition_id: Some(partition.to_string()),
                slug: partition.to_string(),
                repo_root: repo.clone(),
                workspace_path: repo.clone(),
                orbit_dir: orbit_dir.clone(),
                repo_fingerprint: None,
            })
            .expect("bind workspace");
        let store = Store::open(&root.join("state.sqlite")).expect("open store");
        let backends = workspace_coordinated_backends(registry.clone(), partition.into(), store)
            .expect("compose");
        let tasks = Arc::new(TaskV2Store::with_commit_boundary(
            registry,
            partition.into(),
            Arc::clone(&backends.commit_boundary),
        ));
        Self {
            _root: None,
            root: root.to_path_buf(),
            partition,
            boundary: backends.commit_boundary,
            tasks,
            repo,
            orbit_dir,
        }
    }

    /// A second handle on this partition's boundary that gives up on a
    /// contended lock within milliseconds, standing in for an ordinary
    /// section that would otherwise wait out the production deadline.
    fn impatient_boundary(&self) -> TaskCommitBoundary {
        TaskCommitBoundary::new(
            Store::open(&self.root.join("state.sqlite")).expect("open store"),
            TaskRegistryStore::open(&task_registry_path(&self.root)).expect("open registry"),
            self.partition.into(),
        )
        .expect("open boundary")
        .with_lock_options(FileLockOptions {
            timeout: Duration::from_millis(300),
            warn_after: Duration::from_millis(20),
            record_shared_holders: true,
            warn_held_after: None,
        })
    }

    fn create(&self, spec: Spec<'_>) -> Task {
        self.tasks
            .create_task(TaskCreateParams {
                actor: "codex".into(),
                parent_id: None,
                title: spec.title.into(),
                description: "Detailed task description".into(),
                acceptance_criteria: vec!["First criterion".into()],
                dependencies: spec.dependencies,
                relations: Vec::new(),
                tags: Vec::new(),
                required_tools: Vec::new(),
                plan: "1. Do the work".into(),
                execution_summary: String::new(),
                context_files: vec![spec.file],
                repo_root: None,
                created_by: Some("codex".into()),
                planned_by: None,
                implemented_by: None,
                status: spec.status,
                priority: spec.priority,
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
            .expect("create task")
    }

    /// Admit one request, re-checking each committed candidate's holds with
    /// `hold`, and return the receipt with the one admission section's cost.
    fn admit(
        &self,
        request_id: &str,
        hold: &ValidationHold<'_>,
    ) -> (AdmissionReceipt, SectionCost) {
        let (receipt, sections) = self.admit_sections(request_id, hold);
        assert_eq!(sections.len(), 1, "one exclusive section per admission");
        (receipt, sections[0])
    }

    /// [`Self::admit`], returning every admission section it entered.
    fn admit_sections(
        &self,
        request_id: &str,
        hold: &ValidationHold<'_>,
    ) -> (AdmissionReceipt, Vec<SectionCost>) {
        SECTIONS.with(|sections| sections.borrow_mut().clear());
        let lookup = self
            .boundary
            .admit_task(
                &AdmissionIdentity::trusted_remote(ExecutionLocation {
                    machine_id: "machine-a".into(),
                    machine_name: None,
                }),
                &request(request_id),
                &AdmissionOrdering::default(),
                "test",
                &self.repo,
                &self.orbit_dir,
                &BTreeMap::new(),
                &BTreeMap::new(),
                hold,
            )
            .expect("admit");
        let AdmissionLookup::Found { receipt, .. } = lookup else {
            panic!("expected a receipt: {lookup:?}");
        };
        let sections = SECTIONS.with(|sections| sections.borrow().clone());
        (*receipt, sections)
    }

    fn status(&self, id: &str) -> TaskStatus {
        self.tasks.get_task(id).unwrap().unwrap().status
    }
}

struct Spec<'a> {
    title: &'a str,
    status: TaskStatus,
    priority: TaskPriority,
    file: String,
    dependencies: Vec<String>,
}

impl<'a> Spec<'a> {
    fn new(title: &'a str, status: TaskStatus, file: &str) -> Self {
        Self {
            title,
            status,
            priority: TaskPriority::Medium,
            file: file.into(),
            dependencies: Vec::new(),
        }
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
    }
}

const PARTITION_TASKS: usize = 5_000;
const BACKLOG: usize = 40;
const IN_PROGRESS: usize = 10;
const REVIEW: usize = 5;
/// Backlog tasks the selection itself rules out, ahead of the admitted one.
const CONFLICTING: usize = 5;

/// A 5,000-task partition shaped like the incident's: almost every task is
/// done, about 40 are backlog — each depending on one done task, the first
/// few overlapping an in-flight footprint — and 15 are in flight. One
/// admission's exclusive section reads the in-flight tasks, the one
/// candidate it re-checks, that candidate's dependency and the commit's own
/// read: nothing proportional to the partition, and nothing canonical. The
/// pilot re-check runs once, for the committed candidate, on the comments and
/// history the section already read.
#[test]
#[allow(clippy::print_stderr)]
fn one_admission_section_reads_only_the_candidate_in_flight_and_dependency_bundles() {
    let mut progress = FixtureProgress::start("admission partition");
    let owner = Owner::open();
    progress.phase("seed tasks", PARTITION_TASKS);
    let mut create = |spec: Spec<'_>| {
        let task = owner.create(spec);
        progress.advance();
        task
    };
    let done = (0..PARTITION_TASKS - BACKLOG - IN_PROGRESS - REVIEW)
        .map(|index| {
            create(Spec::new(
                "done",
                TaskStatus::Done,
                &format!("src/done_{index}.rs"),
            ))
            .id
        })
        .collect::<Vec<_>>();
    for (index, status) in std::iter::repeat_n(TaskStatus::InProgress, IN_PROGRESS)
        .chain(std::iter::repeat_n(TaskStatus::Review, REVIEW))
        .enumerate()
    {
        create(Spec::new(
            "in flight",
            status,
            &format!("src/flight_{index}.rs"),
        ));
    }
    let backlog = (0..BACKLOG)
        .map(|index| {
            let conflicting = index < CONFLICTING;
            create(Spec {
                priority: if conflicting {
                    TaskPriority::Critical
                } else {
                    TaskPriority::Medium
                },
                dependencies: vec![done[index].clone()],
                ..Spec::new(
                    "backlog",
                    TaskStatus::Backlog,
                    &if conflicting {
                        format!("src/flight_{index}.rs")
                    } else {
                        format!("src/backlog_{index}.rs")
                    },
                )
            })
        })
        .collect::<Vec<_>>();

    progress.phase("admission", 1);
    let rechecked = Rc::new(Cell::new(0));
    let counted = Rc::clone(&rechecked);
    let hold = move |_: &Task, _: &[TaskComment], _: &[TaskHistoryEntry]| {
        counted.set(counted.get() + 1);
        Ok(None)
    };
    let (receipt, cost) = owner.admit("pull-1", &hold);
    progress.advance();
    progress.finish();

    let claimed = receipt.claim.as_ref().expect("a claim").task_id.clone();
    assert_eq!(claimed, backlog[CONFLICTING].id, "{receipt:?}");
    assert_eq!(receipt.deferred_conflicts.len(), CONFLICTING, "{receipt:?}");
    assert_eq!(owner.status(&claimed), TaskStatus::InProgress);
    assert_eq!(
        cost.canonical_reads, 0,
        "ORB-14724: no artifact-hashing read inside the admission section"
    );
    assert_eq!(
        rechecked.get(),
        1,
        "ORB-14724: holds are re-checked for the committed candidate only, not per backlog task"
    );
    // In flight, the re-read candidate, its one dependency, and the commit.
    let bound = (IN_PROGRESS + REVIEW + 3) as u64;
    assert!(
        cost.lightweight_reads <= bound,
        "ORB-14724: the admission section read {} bundles of a {PARTITION_TASKS}-task partition; \
         it may read only in-flight, candidate and dependency bundles ({bound})",
        cost.lightweight_reads
    );
    eprintln!(
        "ORB-14724 admission section on {PARTITION_TASKS} tasks: held {:?}, {} lightweight and {} \
         canonical bundle reads",
        cost.held, cost.lightweight_reads, cost.canonical_reads
    );
}

/// What changes a selected backlog task between selection and the section.
#[derive(Clone, Copy)]
enum Change {
    Status,
    Dependency,
    Footprint,
    PilotHold,
}

/// Each way a selected candidate can change after selection is judged on
/// what the section reads, so the stale selection never admits it: a task
/// that left `backlog`, gained an unfinished dependency, moved onto an
/// in-flight footprint, or was held by a pilot apply is deferred or skipped.
#[test]
fn a_candidate_changed_after_selection_is_never_admitted_from_the_selection() {
    for change in [
        Change::Status,
        Change::Dependency,
        Change::Footprint,
        Change::PilotHold,
    ] {
        let owner = Owner::open();
        let in_flight = owner.create(Spec::new(
            "in flight",
            TaskStatus::InProgress,
            "src/flight.rs",
        ));
        let unfinished = owner.create(Spec::new(
            "unfinished",
            TaskStatus::Proposed,
            "src/other.rs",
        ));
        let candidate = owner.create(Spec {
            priority: TaskPriority::Critical,
            ..Spec::new("candidate", TaskStatus::Backlog, "src/candidate.rs")
        });
        let next = owner.create(Spec::new("next", TaskStatus::Backlog, "src/next.rs"));

        let tasks = Arc::clone(&owner.tasks);
        let id = candidate.id.clone();
        after_selection::set(move || {
            let actor = "human:fixture".to_string();
            match change {
                Change::Status => tasks.update_task_history(
                    &id,
                    &TaskHistoryUpdateParams {
                        actor,
                        status: Some(TaskStatus::Blocked),
                        ..TaskHistoryUpdateParams::default()
                    },
                ),
                Change::Dependency => tasks.update_task_document(
                    &id,
                    &TaskDocumentUpdateParams {
                        actor,
                        dependencies: Some(vec![unfinished.id]),
                        ..TaskDocumentUpdateParams::default()
                    },
                ),
                Change::Footprint => tasks.update_task_document(
                    &id,
                    &TaskDocumentUpdateParams {
                        actor,
                        context_files: Some(vec!["src/flight.rs".into()]),
                        ..TaskDocumentUpdateParams::default()
                    },
                ),
                Change::PilotHold => tasks.update_task_history(
                    &id,
                    &TaskHistoryUpdateParams {
                        actor,
                        append_comments: vec![TaskComment {
                            at: Utc::now(),
                            by: "task-pilot".into(),
                            message: "held".into(),
                        }],
                        ..TaskHistoryUpdateParams::default()
                    },
                ),
            }
            .expect("change the selected candidate");
        });
        let hold = |_: &Task, comments: &[TaskComment], _: &[TaskHistoryEntry]| {
            Ok(comments
                .iter()
                .any(|comment| comment.message == "held")
                .then(|| "held by a pilot apply".to_string()))
        };
        let (receipt, _) = owner.admit("pull-1", &hold);

        let claimed = receipt.claim.as_ref().map(|claim| claim.task_id.clone());
        assert_eq!(
            claimed.as_deref(),
            Some(next.id.as_str()),
            "the changed candidate is passed over for the next one: {receipt:?}"
        );
        assert_ne!(owner.status(&candidate.id), TaskStatus::InProgress);
        let diagnosed = receipt
            .invalid_candidates
            .iter()
            .chain(&receipt.deferred_conflicts)
            .find(|diagnostic| diagnostic.task_id == candidate.id);
        match change {
            Change::Status => assert!(diagnosed.is_none(), "{receipt:?}"),
            Change::Dependency => {
                assert_eq!(diagnosed.expect("diagnosed").blocked_by.len(), 1);
            }
            Change::Footprint => {
                assert_eq!(
                    diagnosed.expect("diagnosed").blocked_by,
                    vec![in_flight.id.clone()]
                );
            }
            Change::PilotHold => {
                assert_eq!(
                    diagnosed.expect("diagnosed").reason,
                    "held by a pilot apply"
                );
            }
        }
    }
}

/// What an ordinary section in each partition got while an admission
/// section was deciding.
struct Neighbours {
    own: Result<(), OrbitError>,
    other: Result<(), OrbitError>,
}

/// Admit one request from `owner` and, from inside its admission section,
/// enter an ordinary section in `owner`'s partition and in `other`'s.
fn admit_while_probing(
    owner: &Owner,
    other: &Owner,
) -> (AdmissionReceipt, Vec<SectionCost>, Neighbours) {
    let own = owner.impatient_boundary();
    let neighbour = other.impatient_boundary();
    let probed = std::cell::RefCell::new(None);
    let hold = |_: &Task, _: &[TaskComment], _: &[TaskHistoryEntry]| {
        // Ordinary sections run on their own threads, as another process's
        // task reads would.
        let neighbours = std::thread::scope(|scope| {
            let own = scope.spawn(|| own.enter_ordinary(|| Ok(())));
            let other = scope.spawn(|| neighbour.enter_ordinary(|| Ok(())));
            Neighbours {
                own: own.join().expect("own partition probe"),
                other: other.join().expect("other partition probe"),
            }
        });
        *probed.borrow_mut() = Some(neighbours);
        Ok(None)
    };
    let (receipt, sections) = owner.admit_sections("pull-1", &hold);
    let neighbours = probed
        .into_inner()
        .expect("the admission section ran its hold check");
    (receipt, sections, neighbours)
}

/// The section kind an ordinary section timed out behind.
fn blocked_by(result: &Result<(), OrbitError>) -> String {
    let error = result
        .as_ref()
        .expect_err("the ordinary section was excluded");
    let timeout = error.file_lock_timeout().expect("a typed lock timeout");
    timeout
        .holder
        .as_ref()
        .map(|holder| holder.label.clone())
        .unwrap_or_else(|| panic!("an exclusive holder named in {timeout:?}"))
}

/// A candidate whose dependency is in its own partition is decided in a
/// section that excludes only that partition: an ordinary section in another
/// workspace partition runs to completion while it decides, and one in the
/// admitting partition still waits for the decision.
#[test]
fn an_admission_within_its_partition_leaves_other_partitions_running() {
    let owner = Owner::open();
    let other = owner.neighbour();
    let prerequisite = owner.create(Spec::new("done", TaskStatus::Done, "src/done.rs"));
    let candidate = owner.create(Spec {
        dependencies: vec![prerequisite.id],
        ..Spec::new("candidate", TaskStatus::Backlog, "src/candidate.rs")
    });
    other.create(Spec::new(
        "neighbour",
        TaskStatus::Backlog,
        "src/neighbour.rs",
    ));

    let (receipt, sections, neighbours) = admit_while_probing(&owner, &other);

    assert_eq!(
        receipt.claim.as_ref().map(|claim| claim.task_id.as_str()),
        Some(candidate.id.as_str()),
        "{receipt:?}"
    );
    assert_eq!(
        sections
            .iter()
            .map(|section| section.kind)
            .collect::<Vec<_>>(),
        ["admission"],
        "ORB-15106: a decision within one partition never takes the host lock exclusively"
    );
    assert!(
        neighbours.other.is_ok(),
        "ORB-15106: another partition's ordinary section ran while the admission decided: {:?}",
        neighbours.other
    );
    assert!(
        blocked_by(&neighbours.own).contains(": admission at "),
        "the admitting partition stays excluded for the decision: {:?}",
        neighbours.own
    );
}

/// A candidate whose dependency another workspace partition holds is the
/// case that still needs the host lock: the partition-scoped section stops
/// at that dependency before writing anything, and the decision is taken
/// again in a host-wide section that excludes every partition's ordinary
/// sections, so the dependency cannot move while the decision rests on it.
#[test]
fn an_admission_reading_another_partitions_dependency_excludes_the_host() {
    let owner = Owner::open();
    let other = owner.neighbour();
    let prerequisite = other.create(Spec::new("done", TaskStatus::Done, "src/done.rs"));
    let candidate = owner.create(Spec {
        dependencies: vec![prerequisite.id.clone()],
        ..Spec::new("candidate", TaskStatus::Backlog, "src/candidate.rs")
    });

    let (receipt, sections, neighbours) = admit_while_probing(&owner, &other);

    assert_eq!(
        receipt.claim.as_ref().map(|claim| claim.task_id.as_str()),
        Some(candidate.id.as_str()),
        "the cross-partition dependency is satisfied: {receipt:?}"
    );
    assert_eq!(owner.status(&candidate.id), TaskStatus::InProgress);
    assert_eq!(other.status(&prerequisite.id), TaskStatus::Done);
    assert_eq!(
        sections
            .iter()
            .map(|section| section.kind)
            .collect::<Vec<_>>(),
        ["admission", "host admission"],
        "the partition-scoped section stops at the cross-partition dependency, then decides host-wide"
    );
    // The candidate, its dependency, and the commit.
    let bound = 3;
    assert!(
        sections[1].lightweight_reads <= bound,
        "the host-wide section read {} bundles; it may read only the candidate, its dependency \
         and the commit's own ({bound})",
        sections[1].lightweight_reads
    );
    assert!(
        blocked_by(&neighbours.other).contains(": host admission at "),
        "the cross-partition dependency read excludes other partitions' ordinary sections: {:?}",
        neighbours.other
    );
    assert!(
        blocked_by(&neighbours.own).contains(": host admission at "),
        "{:?}",
        neighbours.own
    );
}
