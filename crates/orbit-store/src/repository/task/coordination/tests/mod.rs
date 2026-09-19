//! The commit boundary's durability contract.
//!
//! `mod.rs` holds the shared fixture and the atomic-publication tests;
//! `recovery.rs` covers interrupted commits and fail-closed compensation;
//! `serialization.rs` covers the boundary shared by ordinary mutations across
//! store instances and processes. They are split by behaviour rather than by
//! source file because the module has a single source file and one test file
//! would run well past the size budget.

use std::path::{Path, PathBuf};

use chrono::Utc;
use orbit_types::task::{
    Task, TaskComplexity, TaskHistoryEntry, TaskPriority, TaskStatus, TaskType,
};
use tempfile::TempDir;

use super::*;
use crate::compose::{CoordinatedWorkspaceBackends, workspace_coordinated_backends};
use crate::contracts::{
    ActiveTaskReservation, TaskCoordinationCommitOutcome, TaskCoordinationCommitParams,
    TaskCoordinationRow, TaskCreateParams, TaskReservationReserveParams,
};
use crate::driver::sqlite::task_registry::{
    BindWorkspaceParams, TaskRegistryStore, task_registry_path,
};

mod admission;
mod handoff;
mod lifecycle;
mod recovery;
mod serialization;

const PARTITION_ID: &str = "orbit-test-123456";

/// One coordinated composition over a root directory. Opening a second one on
/// the same root is exactly what a second process (or a restart) does.
pub(super) struct Coordinated {
    pub(super) backends: CoordinatedWorkspaceBackends,
    pub(super) orbit_dir: PathBuf,
}

impl Coordinated {
    pub(super) fn open(root: &Path) -> Self {
        let registry = TaskRegistryStore::open(&task_registry_path(root)).expect("open registry");
        let repo_dir = root.join("repo");
        let orbit_dir = repo_dir.join(".orbit");
        std::fs::create_dir_all(&orbit_dir).expect("create orbit dir");
        let binding = registry
            .bind_workspace(BindWorkspaceParams {
                partition_id: Some(PARTITION_ID.to_string()),
                slug: "Orbit Test".to_string(),
                repo_root: repo_dir.clone(),
                workspace_path: repo_dir.clone(),
                orbit_dir: orbit_dir.clone(),
                repo_fingerprint: None,
            })
            .expect("bind workspace");
        let store = Store::open(&root.join("state.sqlite")).expect("open store");
        let backends =
            workspace_coordinated_backends(registry, binding.partition_id, store).expect("compose");
        Self {
            backends,
            orbit_dir,
        }
    }

    pub(super) fn boundary(&self) -> &TaskCommitBoundary {
        &self.backends.commit_boundary
    }

    pub(super) fn create_task(&self, title: &str) -> Task {
        self.backends
            .task
            .task
            .create_task(create_params(title))
            .expect("create task")
    }

    pub(super) fn task(&self, id: &str) -> Task {
        self.backends
            .task
            .task
            .get_task(id)
            .expect("read task")
            .expect("task exists")
    }

    pub(super) fn history(&self, id: &str) -> Vec<TaskHistoryEntry> {
        self.backends
            .task
            .history
            .get_task_history(id)
            .expect("read history")
            .expect("task exists")
    }

    pub(super) fn active_reservations(&self) -> Vec<ActiveTaskReservation> {
        self.backends
            .reservation
            .inspect_active_task_reservations(&self.orbit_dir.to_string_lossy(), Some(PARTITION_ID))
            .expect("read reservations")
    }

    pub(super) fn reservation_params(
        &self,
        task_id: &str,
        file: &str,
    ) -> TaskReservationReserveParams {
        TaskReservationReserveParams {
            workspace_orbit_dir: self.orbit_dir.to_string_lossy().to_string(),
            workspace_id: Some(PARTITION_ID.to_string()),
            task_ids: vec![task_id.to_string()],
            requested_files: vec![file.to_string()],
            actor: "codex".to_string(),
            ttl_seconds: 600,
            owner_run_id: None,
            owner_metadata_json: None,
        }
    }

    /// The shape an admission decision commits: a `backlog → in-progress`
    /// transition, its history, a reservation over the task's own files, and
    /// one dependent coordination row.
    pub(super) fn admission_params(
        &self,
        task_id: &str,
        file: &str,
    ) -> TaskCoordinationCommitParams {
        TaskCoordinationCommitParams {
            task_id: task_id.to_string(),
            actor: "codex".to_string(),
            expected_status: vec![TaskStatus::Backlog],
            status: Some(TaskStatus::InProgress),
            status_event: Some("pulled_by".to_string()),
            status_note: Some("machine=dk-server-1".to_string()),
            append_history: Vec::new(),
            reservation: Some(self.reservation_params(task_id, file)),
            rows: vec![TaskCoordinationRow {
                kind: "admission-receipt".to_string(),
                row_id: format!("request-{task_id}"),
                payload_json: "{\"idle\":false}".to_string(),
            }],
        }
    }
}

pub(super) fn create_params(title: &str) -> TaskCreateParams {
    TaskCreateParams {
        actor: "codex".to_string(),
        parent_id: None,
        title: title.to_string(),
        description: "Detailed task description".to_string(),
        acceptance_criteria: vec!["First criterion".to_string()],
        dependencies: Vec::new(),
        relations: Vec::new(),
        tags: vec!["distributed-drain".to_string()],
        required_tools: Vec::new(),
        plan: "1. Do the work".to_string(),
        execution_summary: String::new(),
        context_files: vec!["src/lib.rs".to_string()],
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
    }
}

pub(super) fn committed(outcome: TaskCoordinationCommitOutcome) -> TaskCoordinationCommit {
    match outcome {
        TaskCoordinationCommitOutcome::Committed(commit) => commit,
        other => panic!("expected a committed outcome, got {other:?}"),
    }
}

#[test]
fn one_commit_publishes_transition_history_reservation_and_rows() {
    let temp = TempDir::new().expect("tempdir");
    let coordinated = Coordinated::open(temp.path());
    let task = coordinated.create_task("Admit me");

    let commit = committed(
        coordinated
            .boundary()
            .commit_task_transition(&coordinated.admission_params(&task.id, "src/lib.rs"))
            .expect("commit"),
    );

    assert_eq!(commit.status, TaskStatus::InProgress);
    assert_eq!(
        coordinated.task(&task.id).status,
        TaskStatus::InProgress,
        "the transition is visible through the ordinary task API"
    );
    let history = coordinated.history(&task.id);
    let pulled = history
        .iter()
        .find(|entry| entry.event == "pulled_by")
        .expect("history carries the admission event");
    assert_eq!(pulled.from_status, Some(TaskStatus::Backlog));
    assert_eq!(pulled.to_status, Some(TaskStatus::InProgress));

    let reservations = coordinated.active_reservations();
    assert_eq!(reservations.len(), 1);
    assert_eq!(reservations[0].files, vec!["src/lib.rs".to_string()]);
    assert_eq!(
        commit.reservation.and_then(|result| result.reservation_id),
        Some(reservations[0].reservation_id.clone())
    );

    let store = coordinated.boundary().store_handle();
    assert_eq!(
        store
            .task_coordination_rows(PARTITION_ID, "admission-receipt")
            .expect("rows")
            .len(),
        1
    );
    assert_eq!(
        store
            .task_commit_journal_state(&commit.journal_id)
            .expect("journal state"),
        Some(TaskCommitJournalState::Applied)
    );
    assert!(
        !coordinated.boundary().pending_marker_exists(),
        "a settled commit leaves no pending marker"
    );
}

#[test]
fn a_stale_expected_status_publishes_nothing() {
    let temp = TempDir::new().expect("tempdir");
    let coordinated = Coordinated::open(temp.path());
    let task = coordinated.create_task("Already moved");
    let mut params = coordinated.admission_params(&task.id, "src/lib.rs");
    params.expected_status = vec![TaskStatus::Review];

    let outcome = coordinated
        .boundary()
        .commit_task_transition(&params)
        .expect("commit");

    assert!(matches!(
        outcome,
        TaskCoordinationCommitOutcome::Stale {
            current_status: TaskStatus::Backlog
        }
    ));
    assert_eq!(coordinated.task(&task.id).status, TaskStatus::Backlog);
    assert!(coordinated.active_reservations().is_empty());
    assert!(
        coordinated
            .boundary()
            .store_handle()
            .unsettled_task_commit_journal(PARTITION_ID)
            .expect("journal")
            .is_empty(),
        "a compare-and-set refusal never reaches the journal"
    );
}

#[test]
fn a_reservation_conflict_leaves_the_task_untouched() {
    let temp = TempDir::new().expect("tempdir");
    let coordinated = Coordinated::open(temp.path());
    let task = coordinated.create_task("Conflicted");
    coordinated
        .backends
        .reservation
        .reserve_task_reservation(coordinated.reservation_params("ORB-9999", "src/lib.rs"))
        .expect("hold a conflicting reservation");

    let outcome = coordinated
        .boundary()
        .commit_task_transition(&coordinated.admission_params(&task.id, "src/lib.rs"))
        .expect("commit");

    let TaskCoordinationCommitOutcome::Conflicted { conflicts, .. } = outcome else {
        panic!("expected a conflict, got {outcome:?}");
    };
    assert_eq!(conflicts.len(), 1);
    assert_eq!(coordinated.task(&task.id).status, TaskStatus::Backlog);
    assert!(
        coordinated
            .history(&task.id)
            .iter()
            .all(|entry| entry.event != "pulled_by"),
        "a refused admission writes no history"
    );
    assert!(
        coordinated
            .boundary()
            .store_handle()
            .task_coordination_rows(PARTITION_ID, "admission-receipt")
            .expect("rows")
            .is_empty()
    );
    assert!(!coordinated.boundary().pending_marker_exists());
}

#[test]
fn replaying_a_coordination_row_identity_is_refused_without_a_second_admission() {
    let temp = TempDir::new().expect("tempdir");
    let coordinated = Coordinated::open(temp.path());
    let first = coordinated.create_task("First");
    let second = coordinated.create_task("Second");
    committed(
        coordinated
            .boundary()
            .commit_task_transition(&coordinated.admission_params(&first.id, "src/lib.rs"))
            .expect("first commit"),
    );

    let mut replay = coordinated.admission_params(&second.id, "docs/design.md");
    replay.rows[0].row_id = format!("request-{}", first.id);
    let outcome = coordinated
        .boundary()
        .commit_task_transition(&replay)
        .expect("replay");

    assert!(matches!(
        outcome,
        TaskCoordinationCommitOutcome::RowExists { ref kind, .. } if kind == "admission-receipt"
    ));
    assert_eq!(
        coordinated.task(&second.id).status,
        TaskStatus::Backlog,
        "a replayed request never admits a second task"
    );
    assert_eq!(coordinated.active_reservations().len(), 1);
}

#[test]
fn ordinary_task_writes_keep_their_behaviour_inside_the_boundary() {
    let temp = TempDir::new().expect("tempdir");
    let coordinated = Coordinated::open(temp.path());
    let task = coordinated.create_task("Ordinary");

    coordinated
        .backends
        .task
        .history
        .update_task_history(
            &task.id,
            crate::contracts::TaskHistoryUpdateParams {
                actor: "codex".to_string(),
                status: Some(TaskStatus::InProgress),
                status_event: None,
                status_note: Some("started".to_string()),
                append_history: vec![TaskHistoryEntry {
                    at: Utc::now(),
                    by: "codex".to_string(),
                    event: "noted".to_string(),
                    note: None,
                    from_status: None,
                    to_status: None,
                }],
                append_comments: Vec::new(),
                expected_status: Some(vec![TaskStatus::Backlog]),
            },
        )
        .expect("ordinary history update");

    assert_eq!(coordinated.task(&task.id).status, TaskStatus::InProgress);
    let events: Vec<_> = coordinated
        .history(&task.id)
        .into_iter()
        .map(|entry| entry.event)
        .collect();
    assert_eq!(events, vec!["created", "noted", "status_changed"]);
    assert!(
        coordinated
            .boundary()
            .store_handle()
            .unsettled_task_commit_journal(PARTITION_ID)
            .expect("journal")
            .is_empty(),
        "an ordinary write participates in the boundary without using the journal"
    );
    assert!(
        coordinated
            .backends
            .task
            .task
            .delete_task(&task.id)
            .expect("delete")
    );
}

#[test]
fn unrelated_settled_corruption_still_errors() {
    let temp = TempDir::new().expect("tempdir");
    let coordinated = Coordinated::open(temp.path());
    let healthy = coordinated.create_task("Healthy");
    let damaged = coordinated.create_task("Damaged");

    // A settled event/envelope status mismatch with no pending-write record is
    // corruption, not an interrupted commit: the boundary must not launder it.
    let bundle_dir = coordinated
        .boundary()
        .bundle_store
        .bundle_path(&damaged.id)
        .expect("bundle path");
    let forged = TaskEventRowV2 {
        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        event_id: "EV-0001".to_string(),
        at: Utc::now(),
        by: "codex".to_string(),
        event_type: "status_changed".to_string(),
        note: None,
        from_status: Some(TaskStatus::Backlog),
        to_status: Some(TaskStatus::Done),
    };
    std::fs::write(
        bundle_dir.join("events.jsonl"),
        format!("{}\n", serde_json::to_string(&forged).expect("serialize")),
    )
    .expect("forge a mismatched event log");

    let error = coordinated
        .backends
        .task
        .task
        .get_task(&damaged.id)
        .expect_err("settled corruption must still fail the read");
    assert!(error.to_string().contains(&damaged.id), "{error}");

    committed(
        coordinated
            .boundary()
            .commit_task_transition(&coordinated.admission_params(&healthy.id, "src/lib.rs"))
            .expect("an unrelated damaged bundle must not block admission"),
    );
    assert_eq!(coordinated.task(&healthy.id).status, TaskStatus::InProgress);
}
