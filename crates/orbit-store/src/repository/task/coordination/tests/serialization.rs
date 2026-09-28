//! The boundary itself: ordinary task and reservation mutations from other
//! store instances and other processes wait for an admission section, and a
//! nested task lock inside one does not deadlock against it.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tempfile::TempDir;

use super::*;
use crate::contracts::{TaskDocumentUpdateParams, TaskHistoryUpdateParams};
use crate::repository::task::coordination::boundary::BoundaryDepth;

/// How long the test lets a blocked writer prove it is blocked. Well under
/// the 30-second advisory-lock timeout it would otherwise hit.
const HOLD: Duration = Duration::from_millis(250);
const POLL: Duration = Duration::from_millis(5);

#[cfg(unix)]
const ADMISSION_HOLDER_CHILD_TEST: &str =
    "repository::task::coordination::tests::serialization::admission_holder_child";

thread_local! {
    /// While set, the partition whose boundary this thread must hold, and
    /// whether each statement it ran on a traced writer connection did.
    static STATEMENT_PROBE: RefCell<Option<(PathBuf, Vec<bool>)>> = const { RefCell::new(None) };
}

fn probe_statement(_sql: &str) {
    STATEMENT_PROBE.with(|probe| {
        if let Some((partition, inside)) = probe.borrow_mut().as_mut() {
            inside.push(BoundaryDepth::active(partition));
        }
    });
}

/// Run `op` on this thread and report, for each statement it ran on `store`'s
/// writer connection, whether that statement ran inside `store`'s boundary.
fn statements_inside_boundary(store: &Coordinated, op: impl FnOnce()) -> Vec<bool> {
    store
        .boundary()
        .store_handle()
        .conn
        .lock()
        .expect("writer connection")
        .trace(Some(probe_statement));
    STATEMENT_PROBE.with(|probe| {
        *probe.borrow_mut() = Some((store.boundary().partition_dir.clone(), Vec::new()));
    });
    op();
    STATEMENT_PROBE
        .with(|probe| probe.borrow_mut().take())
        .map(|(_, inside)| inside)
        .unwrap_or_default()
}

fn history_update(actor: &str, note: &str) -> TaskHistoryUpdateParams {
    TaskHistoryUpdateParams {
        actor: actor.to_string(),
        status: None,
        status_event: Some("noted".to_string()),
        status_note: Some(note.to_string()),
        append_history: Vec::new(),
        append_comments: Vec::new(),
        expected_status: None,
    }
}

fn wait_for(flag: &AtomicBool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !flag.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(POLL);
    }
}

/// Hold an admission section through `holder` while `operation` runs through
/// `other`, and assert the operation waits for the section and then completes.
///
/// Both stores are open before the section begins: constructing a coordinated
/// store takes the partition lock itself, so opening one inside the section
/// would block there and hide an operation that bypasses its boundary.
fn assert_waits_for_admission<T: Send>(
    holder: &Coordinated,
    other: &Coordinated,
    what: &str,
    operation: impl FnOnce(&Coordinated) -> T + Send,
) -> T {
    let entered = AtomicBool::new(false);
    let started = AtomicBool::new(false);
    let release = AtomicBool::new(false);
    let finished = AtomicBool::new(false);

    let (finished_while_held, output) = std::thread::scope(|scope| {
        let section = scope.spawn(|| {
            holder
                .boundary()
                .with_admission(|| {
                    entered.store(true, Ordering::SeqCst);
                    // Deadline-bounded on purpose: a failing check must fail
                    // the test, not hang the suite waiting for a release that
                    // a panicking thread never sets.
                    let deadline = Instant::now() + Duration::from_secs(20);
                    while !release.load(Ordering::SeqCst) && Instant::now() < deadline {
                        std::thread::sleep(POLL);
                    }
                    Ok(())
                })
                .expect("admission section");
        });
        let waiter = scope.spawn(|| {
            wait_for(&entered, "the admission section to start");
            started.store(true, Ordering::SeqCst);
            let output = operation(other);
            finished.store(true, Ordering::SeqCst);
            output
        });

        wait_for(&started, what);
        std::thread::sleep(HOLD);
        let finished_while_held = finished.load(Ordering::SeqCst);
        // Release before asserting, so a failure ends the section now rather
        // than at the holder's deadline.
        release.store(true, Ordering::SeqCst);
        section.join().expect("holder thread");
        let output = waiter.join().expect("operation thread");
        (finished_while_held, output)
    });

    assert!(
        !finished_while_held,
        "{what} must not interleave with another store's admission section"
    );
    assert!(
        finished.load(Ordering::SeqCst),
        "{what} lands once the section ends"
    );
    output
}

#[test]
fn task_history_writes_from_another_store_instance_wait_for_an_admission_section() {
    let temp = TempDir::new().expect("tempdir");
    let coordinated = Coordinated::open(temp.path());
    let other = Coordinated::open(temp.path());
    let task = coordinated.create_task("Serialized");

    assert_waits_for_admission(&coordinated, &other, "a task history write", |other| {
        other
            .backends
            .task
            .history
            .update_task_history(&task.id, history_update("codex", "ordinary"))
            .expect("ordinary task write");
    });

    assert!(
        coordinated
            .history(&task.id)
            .iter()
            .any(|entry| entry.event == "noted"),
        "the waiting write still lands once the section ends"
    );
}

#[test]
fn reservation_writes_from_another_store_instance_wait_for_an_admission_section() {
    let temp = TempDir::new().expect("tempdir");
    let coordinated = Coordinated::open(temp.path());
    let other = Coordinated::open(temp.path());
    let task = coordinated.create_task("Serialized");

    let inside = assert_waits_for_admission(&coordinated, &other, "a reservation write", |other| {
        statements_inside_boundary(other, || {
            other
                .backends
                .reservation
                .reserve_task_reservation(other.reservation_params(&task.id, "docs/x.md"))
                .expect("ordinary reservation write");
        })
    });

    assert_eq!(coordinated.active_reservations().len(), 1);
    // The frozen-claim read enters the boundary on its own, so waiting alone
    // would not notice the write escaping it; check the write's statements.
    assert!(
        !inside.is_empty() && inside.iter().all(|inside| *inside),
        "every reservation write statement must run inside the ordinary boundary: {inside:?}"
    );
}

#[test]
fn show_workspace_claim_waits_for_an_admission_section() {
    let temp = TempDir::new().expect("tempdir");
    let coordinated = Coordinated::open(temp.path());
    let other = Coordinated::open(temp.path());

    // Showing expires stale claims, so it must not run underneath a section.
    assert_waits_for_admission(&coordinated, &other, "show_workspace_claim", |other| {
        other
            .backends
            .reservation
            .show_workspace_claim(&other.orbit_dir.to_string_lossy(), Some(PARTITION_ID))
            .expect("show workspace claim");
    });
}

#[test]
fn nested_task_and_reservation_writes_inside_an_admission_section_do_not_deadlock() {
    let temp = TempDir::new().expect("tempdir");
    let coordinated = Coordinated::open(temp.path());
    let task = coordinated.create_task("Nested");

    coordinated
        .boundary()
        .with_admission(|| {
            coordinated.backends.task.document.update_task_document(
                &task.id,
                TaskDocumentUpdateParams {
                    actor: "codex".to_string(),
                    plan: Some("1. Admit".to_string()),
                    ..Default::default()
                },
            )?;
            // A caller that already holds the task write lock and then writes
            // through it: bundle lock outside, boundary re-entered inside.
            coordinated
                .backends
                .task
                .task
                .with_task_write_lock(&task.id, &mut || {
                    coordinated
                        .backends
                        .task
                        .history
                        .update_task_history(&task.id, history_update("codex", "nested"))
                })?;
            coordinated
                .backends
                .reservation
                .reserve_task_reservation(coordinated.reservation_params(&task.id, "docs/x.md"))?;
            // And the commit re-enters the very section it is running in.
            let outcome = coordinated
                .boundary()
                .commit_task_transition(&coordinated.admission_params(&task.id, "src/lib.rs"))?;
            assert!(matches!(
                outcome,
                TaskCoordinationCommitOutcome::Committed(_)
            ));
            Ok(())
        })
        .expect("nested boundary entries must not deadlock");

    assert_eq!(coordinated.task(&task.id).status, TaskStatus::InProgress);
    assert_eq!(coordinated.task(&task.id).plan, "1. Admit");
    assert_eq!(coordinated.active_reservations().len(), 2);
}

/// Child half of [`ordinary_writes_from_another_process_wait_for_an_admission_section`]:
/// hold an admission section in a separate process until the parent releases it.
#[cfg(unix)]
#[test]
#[ignore = "child process of the cross-process boundary test"]
fn admission_holder_child() {
    let root = std::path::PathBuf::from(
        std::env::var("ORBIT_COMMIT_BOUNDARY_ROOT").expect("root path from the parent"),
    );
    let ready = std::path::PathBuf::from(
        std::env::var("ORBIT_COMMIT_BOUNDARY_READY").expect("ready path from the parent"),
    );
    let release = std::path::PathBuf::from(
        std::env::var("ORBIT_COMMIT_BOUNDARY_RELEASE").expect("release path from the parent"),
    );
    let coordinated = Coordinated::open(&root);
    coordinated
        .boundary()
        .with_admission(|| {
            std::fs::write(&ready, "held").expect("signal the parent");
            let deadline = Instant::now() + Duration::from_secs(20);
            while !release.exists() && Instant::now() < deadline {
                std::thread::sleep(POLL);
            }
            Ok(())
        })
        .expect("admission section");
}

#[cfg(unix)]
#[test]
fn ordinary_writes_from_another_process_wait_for_an_admission_section() {
    let temp = TempDir::new().expect("tempdir");
    let coordinated = Coordinated::open(temp.path());
    let task = coordinated.create_task("Cross process");
    let ready = temp.path().join("holder-ready");
    let release = temp.path().join("holder-release");

    let exe = std::env::current_exe().expect("current test exe");
    let mut child = std::process::Command::new(exe)
        .args(["--exact", ADMISSION_HOLDER_CHILD_TEST, "--ignored"])
        .env("ORBIT_COMMIT_BOUNDARY_ROOT", temp.path())
        .env("ORBIT_COMMIT_BOUNDARY_READY", &ready)
        .env("ORBIT_COMMIT_BOUNDARY_RELEASE", &release)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn the holder process");

    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the holder process"
        );
        std::thread::sleep(POLL);
    }

    let finished = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let writer = scope.spawn(|| {
            coordinated
                .backends
                .task
                .history
                .update_task_history(&task.id, history_update("codex", "cross-process"))
                .expect("ordinary task write");
            finished.store(true, Ordering::SeqCst);
        });
        std::thread::sleep(HOLD);
        assert!(
            !finished.load(Ordering::SeqCst),
            "another process holding the boundary must exclude ordinary writes"
        );
        std::fs::write(&release, "go").expect("release the holder");
        writer.join().expect("writer thread");
    });

    let status = child.wait().expect("await the holder process");
    assert!(status.success(), "holder process failed: {status}");
    assert!(
        coordinated
            .history(&task.id)
            .iter()
            .any(|entry| entry.event == "noted")
    );
}
