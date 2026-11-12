//! A delivery's CI poll reads its CI retry journal through
//! `RuntimeHost::observe_task_artifact`. On 2026-11-07 that read, then an
//! ordinary task-store section, queued behind owner admissions that were
//! themselves waiting on long dashboard reads: one PR completion accumulated
//! 62.5 s of lock wait, including a 30 s acquisition timeout. The observed
//! read passes a queued admission and still returns the stored journal.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use orbit_common::fs::io::{
    FileLockOptions, with_exclusive_file_lock_options, with_shared_file_lock_options,
};
use orbit_core::application::task::TaskUpdateParams;
use orbit_engine::RuntimeHost;
use orbit_types::task::TaskArtifact;

use super::review_gate_audit::Fixture;

const JOURNAL: &str = "ci-rerun/jrun-fixture/diagnostics.json";
const JOURNAL_CONTENT: &str = r#"{"schema_version":1,"execution":"jrun-fixture"}"#;

/// The lock target of the fixture's one task partition.
fn partition_lock_target(root: &Path) -> PathBuf {
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .file_name()
                .is_some_and(|name| name == ".task-commit.lock")
                && path.parent().is_some_and(|dir| {
                    dir.parent()
                        .is_some_and(|parent| parent.ends_with("workspaces"))
                })
            {
                return path.with_file_name("task-commit");
            }
        }
    }
    panic!("task partition boundary exists after seeding");
}

/// The turnstile an exclusive contender holds from its first refused
/// attempt until it takes the lock (`<lock>.turnstile` beside `<lock>`).
fn queued_turnstile_path(target: &Path) -> PathBuf {
    let file_name = target.file_name().unwrap().to_str().unwrap();
    let lock_name = format!(".{file_name}.lock");
    target.with_file_name(format!("{lock_name}.turnstile"))
}

/// Wait, bounded, until the exclusive contender has genuinely queued:
/// its turnstile holder record appears only after its first `WouldBlock`
/// and turnstile acquisition, so a thread that has not yet run (or has
/// not yet attempted the lock) does not satisfy this observation.
fn wait_for_queued_exclusive(target: &Path) {
    let turnstile = queued_turnstile_path(target);
    let deadline = Instant::now() + Duration::from_secs(10);
    while orbit_common::fs::file_lock::read_file_lock_holder(&turnstile).is_none() {
        assert!(
            Instant::now() < deadline,
            "exclusive contender never queued at {}",
            turnstile.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn boundary_options() -> FileLockOptions {
    FileLockOptions {
        record_shared_holders: true,
        prefer_exclusive_waiters: true,
        ..Default::default()
    }
}

/// A long ordinary read holding the partition, as the dashboard's listing
/// did, and an admission queued exclusively behind it.
struct Convoy {
    release: Option<mpsc::Sender<()>>,
    holder: Option<std::thread::JoinHandle<()>>,
    admission: Option<std::thread::JoinHandle<()>>,
}

impl Convoy {
    fn form(target: PathBuf) -> Self {
        let (ready_tx, ready) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let holder_target = target.clone();
        let holder = std::thread::spawn(move || {
            with_shared_file_lock_options(
                &holder_target,
                "task commit boundary: ordinary (long read fixture)",
                boundary_options(),
                || -> Result<(), std::io::Error> {
                    ready_tx.send(()).unwrap();
                    // Sender drop also releases the holder if an assertion panics.
                    let _ = released.recv_timeout(Duration::from_secs(60));
                    Ok(())
                },
            )
            .unwrap();
        });
        ready.recv_timeout(Duration::from_secs(10)).unwrap();
        let queue_target = target.clone();
        let admission = std::thread::spawn(move || {
            with_exclusive_file_lock_options(
                &target,
                "task commit boundary: admission (queued fixture)",
                FileLockOptions {
                    timeout: Duration::from_secs(60),
                    ..boundary_options()
                },
                || -> Result<(), std::io::Error> { Ok(()) },
            )
            .unwrap();
        });
        // Do not assume the admission thread has run after a fixed delay:
        // observe its turnstile holder, which exists only once it has
        // attempted the lock, been refused, and queued behind the reader.
        wait_for_queued_exclusive(&queue_target);
        Self {
            release: Some(release),
            holder: Some(holder),
            admission: Some(admission),
        }
    }

    fn release(&mut self) {
        self.release.take();
        for thread in [self.holder.take(), self.admission.take()]
            .into_iter()
            .flatten()
        {
            thread.join().unwrap();
        }
    }
}

impl Drop for Convoy {
    fn drop(&mut self) {
        self.release();
    }
}

fn ci_retry_journal_read_passes_a_queued_admission() {
    let fixture = Fixture::new();
    fixture
        .runtime
        .update_task_with_identity(
            &fixture.task_id,
            TaskUpdateParams {
                upsert_artifacts: vec![TaskArtifact::from_text(JOURNAL, JOURNAL_CONTENT)],
                ..Default::default()
            },
            None,
            Some("codex".into()),
        )
        .unwrap();
    let mut convoy = Convoy::form(partition_lock_target(fixture._root.path()));

    // An ordinary read still yields to the queued admission: it cannot enter
    // while the long reader holds the partition.
    let ordinary =
        orbit_core::test_support::with_task_commit_lock_timeout(Duration::from_millis(500), || {
            RuntimeHost::get_task_artifact(&fixture.runtime, &fixture.task_id, JOURNAL)
        });
    let error = ordinary.expect_err("an ordinary read queues behind the admission");
    assert!(error.to_string().contains("timed out"), "{error}");

    // The journal read passes it. The deadline makes a regression fail
    // instead of waiting out the holder.
    let started = Instant::now();
    let (observed, lock_wait_ms) = orbit_common::fs::lock_wait::measure(|| {
        orbit_core::test_support::with_task_commit_lock_timeout(Duration::from_secs(2), || {
            RuntimeHost::observe_task_artifact(&fixture.runtime, &fixture.task_id, JOURNAL)
        })
    });
    let observed = observed
        .expect("the CI retry journal read must not queue behind an admission")
        .expect("the stored journal is returned");
    assert_eq!(observed.content, JOURNAL_CONTENT.as_bytes());
    assert!(
        lock_wait_ms < 2_000 && started.elapsed() < Duration::from_secs(2),
        "journal read waited {lock_wait_ms} ms behind a queued admission"
    );
    let missing = RuntimeHost::observe_task_artifact(
        &fixture.runtime,
        &fixture.task_id,
        "ci-rerun/other/diagnostics.json",
    )
    .unwrap();
    assert!(missing.is_none());

    // The admission still runs once the reader leaves, and ordinary reads see
    // the same journal.
    convoy.release();
    let ordinary = RuntimeHost::get_task_artifact(&fixture.runtime, &fixture.task_id, JOURNAL)
        .unwrap()
        .unwrap();
    assert_eq!(ordinary.content, observed.content);
}

crate::scenario_table!(
    ci_retry_journal_wait,
    ci_retry_journal_read_passes_a_queued_admission
);
