use std::sync::{Arc, Mutex};

use orbit_common::{NotFoundKind, OrbitError};
use orbit_engine::WorktreeGcTaskLookup;
use orbit_tools::{DrainOwnerTransport, OwnerCoordinator};
use orbit_types::task::TaskStatus;
use orbit_types::tool::ToolSessionContext;
use serde_json::{Value, json};

use crate::OrbitRuntime;

const OWNER: &str = "hm_owner";

/// How the stub owner answers one task id.
enum OwnerAnswer {
    Fields(Value),
    /// The owner's store has no such task, surfaced locally.
    NotFound,
    /// The same miss, relayed as the destination's tool error.
    RemoteNotFound,
}

/// A replica's owner as its federated route would reach it: every call is
/// recorded, and each task id is answered from a script. An id with no
/// script entry fails as the route does when the owner is not reachable.
#[derive(Default)]
struct StubOwner {
    answers: Vec<(&'static str, OwnerAnswer)>,
    calls: Mutex<Vec<(String, String, Value)>>,
}

impl DrainOwnerTransport for StubOwner {
    fn call(&self, selector: &str, name: &str, input: Value) -> Result<Value, OrbitError> {
        self.calls
            .lock()
            .unwrap()
            .push((selector.to_string(), name.to_string(), input.clone()));
        let id = input["id"].as_str().unwrap_or_default();
        match self.answers.iter().find(|(task_id, _)| *task_id == id) {
            Some((_, OwnerAnswer::Fields(value))) => Ok(value.clone()),
            Some((_, OwnerAnswer::NotFound)) => Err(OrbitError::not_found(NotFoundKind::Task, id)),
            Some((_, OwnerAnswer::RemoteNotFound)) => Err(OrbitError::RemoteTool {
                code: "not_found".to_string(),
                message: format!("task not found: {id}"),
                payload: Value::Null,
            }),
            None => Err(OrbitError::UnreachableDestination(format!(
                "{selector}: ssh: connect to host timed out"
            ))),
        }
    }

    fn worker_coordinator(&self) -> Arc<dyn OwnerCoordinator> {
        Arc::new(NoWorkerRoute)
    }
}

struct NoWorkerRoute;

impl OwnerCoordinator for NoWorkerRoute {
    fn call(&self, _: &str, _: Value, _: ToolSessionContext) -> Result<Value, OrbitError> {
        Err(OrbitError::PolicyDenied(
            "worktree GC binds no worker".into(),
        ))
    }
}

fn replica_with_owner(owner: Arc<StubOwner>) -> OrbitRuntime {
    OrbitRuntime::in_memory()
        .expect("runtime")
        .with_coordination_write_owner(Some(OWNER.to_string()))
        .with_drain_owner_transport(owner)
}

/// [ORB-13658] A replica holds no task records, so every settled task read
/// locally as unresolved and GC reclaimed nothing on a follower. It must ask
/// its owner, addressed by the owner machine and this replica's logical
/// workspace, for the status the settled rule reads.
#[test]
fn replica_gc_resolves_task_status_from_its_owner() {
    let owner = Arc::new(StubOwner {
        answers: vec![
            (
                "ORB-DONE",
                OwnerAnswer::Fields(json!({ "status": "done", "pr_status": "merged" })),
            ),
            (
                "ORB-BLOCKED",
                OwnerAnswer::Fields(json!({ "status": "blocked", "pr_status": null })),
            ),
        ],
        ..StubOwner::default()
    });
    let replica = replica_with_owner(owner.clone());

    assert_eq!(
        replica.worktree_gc_task_lookup("ORB-DONE"),
        WorktreeGcTaskLookup::Found {
            status: TaskStatus::Done,
            pr_status: Some("merged".to_string()),
        }
    );
    assert_eq!(
        replica.worktree_gc_task_lookup("ORB-BLOCKED"),
        WorktreeGcTaskLookup::Found {
            status: TaskStatus::Blocked,
            pr_status: None,
        }
    );

    let calls = owner.calls.lock().unwrap();
    let logical = replica
        .workspace_runtime_binding()
        .map(|binding| binding.logical_workspace_id.clone())
        .expect("in-memory runtime is bound");
    assert!(
        calls.iter().all(|(selector, tool, _)| {
            *selector == format!("{OWNER}/{logical}") && tool == "orbit.task.show"
        }),
        "every lookup goes to this replica's owner workspace: {calls:?}"
    );
    assert_eq!(calls[0].2["id"], "ORB-DONE");
}

/// The owner answering "no such task" and the owner not answering are
/// different facts; only the first is `task_unresolved`.
#[test]
fn replica_gc_separates_an_unknown_task_from_an_unreachable_owner() {
    let owner = Arc::new(StubOwner {
        answers: vec![
            ("ORB-GONE", OwnerAnswer::NotFound),
            ("ORB-REMOTE-GONE", OwnerAnswer::RemoteNotFound),
        ],
        ..StubOwner::default()
    });
    let replica = replica_with_owner(owner);

    assert_eq!(
        replica.worktree_gc_task_lookup("ORB-GONE"),
        WorktreeGcTaskLookup::Unresolved
    );
    assert_eq!(
        replica.worktree_gc_task_lookup("ORB-REMOTE-GONE"),
        WorktreeGcTaskLookup::Unresolved
    );
    assert!(matches!(
        replica.worktree_gc_task_lookup("ORB-UNANSWERED"),
        WorktreeGcTaskLookup::OwnerUnreachable(reason) if reason.contains("unreachable")
    ));
}

#[test]
fn replica_without_an_owner_route_reports_the_owner_unreachable() {
    let replica = OrbitRuntime::in_memory()
        .expect("runtime")
        .with_coordination_write_owner(Some(OWNER.to_string()));

    assert!(matches!(
        replica.worktree_gc_task_lookup("ORB-ANY"),
        WorktreeGcTaskLookup::OwnerUnreachable(_)
    ));
}

/// An owner checkout keeps reading its own store, even with a route to
/// other owners installed.
#[test]
fn owner_checkout_gc_never_asks_a_remote_owner() {
    let owner = Arc::new(StubOwner::default());
    let runtime = OrbitRuntime::in_memory()
        .expect("runtime")
        .with_drain_owner_transport(owner.clone());

    assert_eq!(
        runtime.worktree_gc_task_lookup("ORB-LOCAL-MISSING"),
        WorktreeGcTaskLookup::Unresolved
    );
    assert!(owner.calls.lock().unwrap().is_empty());
}

#[test]
fn gc_worktrees_rejects_overflow_and_out_of_range_hours_without_unwinding() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");

    // i64::MAX causes Duration::hours to panic in chrono without checked conversion.
    let err = runtime
        .gc_worktrees(false, None, Some(i64::MAX as u64), false, false)
        .expect_err("i64::MAX must return InvalidInput");
    assert!(
        matches!(err, OrbitError::InvalidInput(ref msg) if msg.contains("--older-than-hours is too large")),
        "expected InvalidInput for i64::MAX, got: {err:?}"
    );

    // u64::MAX exceeds i64 range.
    let err = runtime
        .gc_worktrees(false, None, Some(u64::MAX), false, false)
        .expect_err("u64::MAX must return InvalidInput");
    assert!(
        matches!(err, OrbitError::InvalidInput(ref msg) if msg.contains("--older-than-hours is too large")),
        "expected InvalidInput for u64::MAX, got: {err:?}"
    );

    // Out-of-range value where Duration::try_hours returns None (overflows seconds in chrono::Duration)
    let err = runtime
        .gc_worktrees(false, None, Some(3_000_000_000), false, false)
        .expect_err("3B hours must return InvalidInput");
    assert!(
        matches!(err, OrbitError::InvalidInput(ref msg) if msg.contains("--older-than-hours is too large")),
        "expected InvalidInput for 3B hours, got: {err:?}"
    );
}

fn init_git_repo(path: &std::path::Path) {
    std::fs::create_dir_all(path).unwrap();
    let run = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success());
    };
    run(&["init"]);
    run(&["checkout", "-b", "agent-main"]);
    run(&["config", "user.name", "Orbit Test"]);
    run(&["config", "user.email", "orbit-test@example.com"]);
    std::fs::write(path.join("base.txt"), "base").unwrap();
    run(&["add", "base.txt"]);
    run(&["commit", "-m", "base"]);
}

#[test]
fn gc_worktrees_accepts_zero_ordinary_ages_and_none() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    init_git_repo(&runtime.paths().repo_root);

    // Zero hours
    let res = runtime.gc_worktrees(false, None, Some(0), false, false);
    assert!(res.is_ok(), "zero hours must succeed, got: {res:?}");

    // Ordinary ages
    let res = runtime.gc_worktrees(false, None, Some(1), false, false);
    assert!(res.is_ok(), "1 hour must succeed, got: {res:?}");

    let res = runtime.gc_worktrees(false, None, Some(24), false, false);
    assert!(res.is_ok(), "24 hours must succeed, got: {res:?}");

    let res = runtime.gc_worktrees(false, None, Some(168), false, false);
    assert!(res.is_ok(), "168 hours must succeed, got: {res:?}");

    // None (no age restriction)
    let res = runtime.gc_worktrees(false, None, None, false, false);
    assert!(res.is_ok(), "None must succeed, got: {res:?}");
}

#[test]
fn gc_worktrees_attempts_no_deletion_for_rejected_values() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");

    // Create a mock worktree directory that would otherwise be eligible for deletion if GC proceeded
    let worktree_dir = runtime
        .paths()
        .repo_root
        .join(".orbit")
        .join("state")
        .join("worktrees")
        .join("candidate-worktree");
    std::fs::create_dir_all(&worktree_dir).expect("create candidate worktree dir");
    assert!(worktree_dir.exists());

    // When delete: true is requested with an overflowing older_than_hours, validation must fail
    // before any collection or deletion is attempted.
    let err = runtime
        .gc_worktrees(true, None, Some(i64::MAX as u64), false, false)
        .expect_err("i64::MAX must return InvalidInput even with delete: true");
    assert!(
        matches!(err, OrbitError::InvalidInput(_)),
        "expected InvalidInput, got: {err:?}"
    );

    // Verify the directory is untouched
    assert!(
        worktree_dir.exists(),
        "worktree directory must not be deleted when input is rejected"
    );

    // Also check for u64::MAX
    let err = runtime
        .gc_worktrees(true, None, Some(u64::MAX), false, false)
        .expect_err("u64::MAX must return InvalidInput even with delete: true");
    assert!(
        matches!(err, OrbitError::InvalidInput(_)),
        "expected InvalidInput, got: {err:?}"
    );
    assert!(
        worktree_dir.exists(),
        "worktree directory must not be deleted when input is rejected"
    );
}
