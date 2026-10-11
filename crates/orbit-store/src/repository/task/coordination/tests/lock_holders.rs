//! ORB-15088: under drain load the partition lock was held past the 30 s
//! deadline, and 657 of 659 contention warnings named its holder `unknown`:
//! only an exclusive holder recorded itself, while most holders are ordinary
//! sections holding it shared. A waiter must name whoever holds the lock, in
//! either mode, by the section that entered the boundary, and a section that
//! holds it too long must say so on release.

use std::collections::BTreeMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_common::fs::io::{FileLockOptions, hold_step, with_exclusive_file_lock_options};
use tempfile::TempDir;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

use super::super::{Section, TaskCommitBoundary};
use crate::Store;
use crate::driver::sqlite::task_registry::{
    BindWorkspaceParams, TaskRegistryStore, task_registry_path,
};

const PARTITION: &str = "orbit-test-123456";
const LOCK_TARGET: &str = "orbit.common.fs.file_lock";
const STILL_WAITING: &str = "still waiting for advisory file lock; a holder may be hung";
const HELD_PAST_THRESHOLD: &str = "advisory file lock held past its threshold";

/// One activated partition's boundary under `options`.
fn boundary(options: FileLockOptions) -> (TempDir, Arc<TaskCommitBoundary>) {
    let root = tempfile::tempdir().expect("tempdir");
    let registry =
        TaskRegistryStore::open(&task_registry_path(root.path())).expect("open registry");
    let repo = root.path().join("repo");
    let orbit_dir = repo.join(".orbit");
    std::fs::create_dir_all(&orbit_dir).expect("create orbit dir");
    registry
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some(PARTITION.to_string()),
            slug: "Orbit Test".to_string(),
            repo_root: repo.clone(),
            workspace_path: repo,
            orbit_dir,
            repo_fingerprint: None,
        })
        .expect("bind workspace");
    let store = Store::open(&root.path().join("state.sqlite")).expect("open store");
    let boundary = TaskCommitBoundary::new(store, registry, PARTITION.into())
        .expect("activate partition")
        .with_lock_options(options);
    (root, Arc::new(boundary))
}

/// Contend within milliseconds: warn almost at once, give up soon after.
fn contending() -> FileLockOptions {
    FileLockOptions {
        timeout: Duration::from_millis(500),
        warn_after: Duration::from_millis(20),
        record_shared_holders: true,
        warn_held_after: None,
        prefer_exclusive_waiters: true,
    }
}

type Events = Arc<Mutex<Vec<BTreeMap<String, String>>>>;

/// Every lock diagnostic emitted on this thread while `op` runs, as fields.
fn lock_events<T>(op: impl FnOnce() -> T) -> (T, Vec<BTreeMap<String, String>>) {
    struct Capture(Events);
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
    impl<S: tracing::Subscriber> Layer<S> for Capture {
        fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
            if event.metadata().target() != LOCK_TARGET {
                return;
            }
            let mut fields = BTreeMap::new();
            event.record(&mut Fields(&mut fields));
            self.0.lock().expect("events").push(fields);
        }
    }
    let events = Events::default();
    let dispatch = tracing::Dispatch::new(Registry::default().with(Capture(Arc::clone(&events))));
    let value = tracing::dispatcher::with_default(&dispatch, op);
    let events = events.lock().expect("events").clone();
    (value, events)
}

/// Hold one boundary section on another thread until the returned sender
/// fires; returns once the section holds its locks.
fn hold(
    enter: impl FnOnce(mpsc::Sender<()>, mpsc::Receiver<()>) + Send + 'static,
) -> (mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let holder = std::thread::spawn(move || enter(held_tx, release_rx));
    held_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("holder entered its section");
    (release_tx, holder)
}

fn partition_wait(events: &[BTreeMap<String, String>]) -> &BTreeMap<String, String> {
    events
        .iter()
        .find(|event| {
            event.get("message").map(String::as_str) == Some(STILL_WAITING)
                && event
                    .get("lock_path")
                    .is_some_and(|path| path.ends_with("/.task-commit.lock"))
        })
        .unwrap_or_else(|| panic!("a partition-lock wait warning in {events:#?}"))
}

/// The holder's own section label, as the boundary records it.
fn section_label(kind: &str) -> String {
    format!("task commit boundary: {kind} at {}:", file!())
}

#[test]
fn a_partition_wait_names_its_shared_holders() {
    let (_root, boundary) = boundary(contending());
    let reader = Arc::clone(&boundary);
    let (release, holder) = hold(move |held, release| {
        reader
            .enter_ordinary(|| {
                held.send(()).expect("signal held");
                release.recv().expect("release");
                Ok(())
            })
            .expect("ordinary section");
    });

    // Recovery holds the host lock shared and then needs the partition
    // exclusively, so the ordinary section's shared hold is what it waits on.
    let (result, events) = lock_events(|| boundary.recover());
    release.send(()).expect("release holder");
    holder.join().expect("holder thread");

    let pid = std::process::id();
    let warning = partition_wait(&events);
    let named = warning.get("holder").expect("holder field");
    assert!(
        named.starts_with(&format!("shared: pid {pid} since ")),
        "the waiter names the shared holder's pid and acquisition time: {named}"
    );
    assert!(
        named.contains(&section_label("ordinary")),
        "the waiter names the shared holder's section: {named}"
    );
    assert!(
        warning
            .get("label")
            .is_some_and(|label| label.starts_with(&section_label("recovery"))),
        "the warning names the waiting section too: {warning:?}"
    );

    let error = result.expect_err("recovery cannot take the partition past a live reader");
    let timeout = error.file_lock_timeout().expect("a typed lock timeout");
    assert!(timeout.holder.is_none(), "no exclusive holder: {timeout:?}");
    assert_eq!(timeout.shared_holders.len(), 1, "{timeout:?}");
    assert!(
        timeout.shared_holders[0]
            .label
            .starts_with(&section_label("ordinary")),
        "the timeout carries the shared holder's section: {timeout:?}"
    );

    // Released holders take their records with them.
    let (_, events) = lock_events(|| boundary.recover());
    assert!(
        events
            .iter()
            .all(|event| event.get("message").map(String::as_str) != Some(STILL_WAITING)),
        "nothing holds the partition once the reader left: {events:#?}"
    );
}

#[test]
fn a_partition_wait_names_its_exclusive_holder() {
    let (_root, boundary) = boundary(contending());
    let writer = Arc::clone(&boundary);
    let (release, holder) = hold(move |held, release| {
        // The partition-exclusive section that leaves the host lock to
        // readers: an interrupted commit's replay.
        writer
            .exclusive(&writer.lock_target(), Section::here("recovery"), || {
                held.send(()).expect("signal held");
                release.recv().expect("release");
                Ok::<_, OrbitError>(())
            })
            .expect("exclusive section");
    });

    // A pending marker sends the ordinary section through recovery's reader
    // check, which waits on the partition behind the replay.
    std::fs::write(boundary.pending_marker_path(), "commit-test").expect("pending marker");
    let (result, events) = lock_events(|| boundary.enter_ordinary(|| Ok(())));
    release.send(()).expect("release holder");
    holder.join().expect("holder thread");

    let pid = std::process::id();
    let named = partition_wait(&events)
        .get("holder")
        .expect("holder field")
        .clone();
    assert!(
        named.starts_with(&format!("pid {pid} since ")),
        "the waiter names the exclusive holder's pid and acquisition time: {named}"
    );
    assert!(
        named.contains(&section_label("recovery")),
        "the waiter names the exclusive holder's section: {named}"
    );
    let error = result.expect_err("a reader cannot enter past a live replay");
    let timeout = error.file_lock_timeout().expect("a typed lock timeout");
    assert!(
        timeout
            .holder
            .as_ref()
            .is_some_and(|holder| holder.label.starts_with(&section_label("recovery"))),
        "the timeout carries the exclusive holder's section: {timeout:?}"
    );
}

/// The held-past-threshold report on `lock_file`, as fields.
fn hold_report<'a>(
    events: &'a [BTreeMap<String, String>],
    lock_file: &str,
) -> &'a BTreeMap<String, String> {
    events
        .iter()
        .find(|event| {
            event.get("message").map(String::as_str) == Some(HELD_PAST_THRESHOLD)
                && event
                    .get("lock_path")
                    .is_some_and(|path| path.ends_with(&format!("/{lock_file}")))
        })
        .unwrap_or_else(|| panic!("a {lock_file} hold report in {events:#?}"))
}

fn report_millis(report: &BTreeMap<String, String>, field: &str) -> u64 {
    report
        .get(field)
        .and_then(|millis| millis.parse().ok())
        .unwrap_or_else(|| panic!("{field} in {report:?}"))
}

/// ORB-15534: ordinary sections still held the boundary for seconds under
/// drain load, and the report named only the section. It must also name
/// where the hold went: the slowest step by its own time, so a step nested
/// inside the operation is named rather than the operation that ran it, and
/// a wait for the partition is named as one.
#[test]
fn a_section_held_past_its_threshold_reports_its_label_and_duration() {
    let (_root, overdue) = boundary(FileLockOptions {
        timeout: Duration::from_secs(10),
        warn_held_after: Some(Duration::from_millis(30)),
        ..contending()
    });
    let held_for = Duration::from_millis(80);

    let (_, events) = lock_events(|| {
        overdue.enter_ordinary(|| {
            hold_step("slow step", || std::thread::sleep(held_for));
            hold_step("quick step", || ());
            Ok(())
        })
    });

    let report = hold_report(&events, ".task-commit.lock");
    assert!(
        report
            .get("label")
            .is_some_and(|label| label.starts_with(&section_label("ordinary"))),
        "the report names the section: {report:?}"
    );
    assert_eq!(report.get("mode").map(String::as_str), Some("shared"));
    assert!(
        report_millis(report, "held_ms") >= held_for.as_millis() as u64,
        "the report carries the held duration: {report:?}"
    );
    for lock_file in [".task-commit.lock", ".host-task-commit.lock"] {
        let report = hold_report(&events, lock_file);
        assert_eq!(
            report.get("slowest_step").map(String::as_str),
            Some("slow step"),
            "{report:?}"
        );
        assert!(
            report_millis(report, "slowest_step_ms") >= held_for.as_millis() as u64,
            "the report carries the slowest step's own time: {report:?}"
        );
    }

    // A section that waited for its partition behind an exclusive holder
    // held the host lock for that wait. The holder takes the partition
    // without a hold report of its own.
    let partition = overdue.lock_target();
    let (release, holder) = hold(move |held, release| {
        with_exclusive_file_lock_options(
            &partition,
            "exclusive holder",
            FileLockOptions::default(),
            || {
                held.send(()).expect("signal held");
                release.recv().expect("release");
                Ok::<_, OrbitError>(())
            },
        )
        .expect("exclusive holder");
    });
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(held_for);
        release.send(()).expect("release holder");
    });
    let (result, events) = lock_events(|| overdue.enter_ordinary(|| Ok(())));
    releaser.join().expect("releaser thread");
    holder.join().expect("holder thread");
    result.expect("the ordinary section runs once the holder releases");
    let report = hold_report(&events, ".host-task-commit.lock");
    assert_eq!(
        report.get("slowest_step").map(String::as_str),
        Some(".task-commit.lock wait"),
        "{report:?}"
    );

    // A section inside its threshold says nothing. The margin is wide so a
    // scheduling stall on a loaded host cannot push an empty section past it.
    let (_root, patient) = boundary(FileLockOptions {
        warn_held_after: Some(Duration::from_secs(60)),
        ..contending()
    });
    let (_, events) = lock_events(|| patient.enter_ordinary(|| Ok(())));
    assert!(
        events
            .iter()
            .all(|event| event.get("message").map(String::as_str) != Some(HELD_PAST_THRESHOLD)),
        "{events:#?}"
    );
}
