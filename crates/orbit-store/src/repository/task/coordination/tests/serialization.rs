//! The boundary itself: ordinary task and reservation mutations from other
//! store instances and other processes wait for an admission section, and a
//! nested task lock inside one does not deadlock against it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tempfile::TempDir;

use super::*;
use crate::contracts::{TaskDocumentUpdateParams, TaskHistoryUpdateParams};

/// How long the test lets a blocked writer prove it is blocked. Well under
/// the 30-second advisory-lock timeout it would otherwise hit.
const HOLD: Duration = Duration::from_millis(250);
const POLL: Duration = Duration::from_millis(5);

#[cfg(unix)]
const ADMISSION_HOLDER_CHILD_TEST: &str =
    "repository::task::coordination::tests::serialization::admission_holder_child";

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

#[test]
fn ordinary_writes_from_another_store_instance_wait_for_an_admission_section() {
    let temp = TempDir::new().expect("tempdir");
    let coordinated = Coordinated::open(temp.path());
    let task = coordinated.create_task("Serialized");
    let entered = AtomicBool::new(false);
    let release = AtomicBool::new(false);
    let finished = AtomicBool::new(false);

    std::thread::scope(|scope| {
        let holder = scope.spawn(|| {
            // A separate composition over the same files: separate SQLite
            // handles, separate registry handle, same boundary.
            let held = Coordinated::open(temp.path());
            held.boundary()
                .with_admission(|| {
                    entered.store(true, Ordering::SeqCst);
                    // Deadline-bounded on purpose: a failing assertion below
                    // must fail the test, not hang the suite waiting for a
                    // release that the panicking thread never sets.
                    let deadline = Instant::now() + Duration::from_secs(20);
                    while !release.load(Ordering::SeqCst) && Instant::now() < deadline {
                        std::thread::sleep(POLL);
                    }
                    Ok(())
                })
                .expect("admission section");
        });
        let writer = scope.spawn(|| {
            wait_for(&entered, "the admission section to start");
            let other = Coordinated::open(temp.path());
            other
                .backends
                .task
                .history
                .update_task_history(&task.id, history_update("codex", "ordinary"))
                .expect("ordinary task write");
            other
                .backends
                .reservation
                .reserve_task_reservation(other.reservation_params(&task.id, "docs/x.md"))
                .expect("ordinary reservation write");
            finished.store(true, Ordering::SeqCst);
        });

        wait_for(&entered, "the admission section to start");
        std::thread::sleep(HOLD);
        assert!(
            !finished.load(Ordering::SeqCst),
            "ordinary task and reservation writes must not interleave with an admission section"
        );
        release.store(true, Ordering::SeqCst);
        holder.join().expect("holder thread");
        writer.join().expect("writer thread");
    });

    assert!(finished.load(Ordering::SeqCst));
    assert!(
        coordinated
            .history(&task.id)
            .iter()
            .any(|entry| entry.event == "noted"),
        "the waiting write still lands once the section ends"
    );
    assert_eq!(coordinated.active_reservations().len(), 1);
}

#[test]
fn show_workspace_claim_waits_for_an_admission_section() {
    let temp = TempDir::new().expect("tempdir");
    let _setup = Coordinated::open(temp.path());
    let entered = AtomicBool::new(false);
    let release = AtomicBool::new(false);
    let finished = AtomicBool::new(false);

    std::thread::scope(|scope| {
        let holder = scope.spawn(|| {
            let held = Coordinated::open(temp.path());
            held.boundary()
                .with_admission(|| {
                    entered.store(true, Ordering::SeqCst);
                    let deadline = Instant::now() + Duration::from_secs(20);
                    while !release.load(Ordering::SeqCst) && Instant::now() < deadline {
                        std::thread::sleep(POLL);
                    }
                    Ok(())
                })
                .expect("admission section");
        });
        let writer = scope.spawn(|| {
            wait_for(&entered, "the admission section to start");
            let other = Coordinated::open(temp.path());
            other
                .backends
                .reservation
                .show_workspace_claim(&other.orbit_dir.to_string_lossy(), Some(PARTITION_ID))
                .expect("show workspace claim");
            finished.store(true, Ordering::SeqCst);
        });

        wait_for(&entered, "the admission section to start");
        std::thread::sleep(HOLD);
        assert!(
            !finished.load(Ordering::SeqCst),
            "show_workspace_claim must not expire claims underneath an admission section"
        );
        release.store(true, Ordering::SeqCst);
        holder.join().expect("holder thread");
        writer.join().expect("writer thread");
    });

    assert!(finished.load(Ordering::SeqCst));
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
