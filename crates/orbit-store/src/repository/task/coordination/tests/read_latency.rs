//! Synchronized reproduction of report I/O extending a nested read boundary.
//! The operation holds for 80 ms; only the subscriber is stalled for seconds.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use orbit_common::fs::io::{FileLockOptions, hold_step, try_acquire_exclusive_file_lock};
use orbit_common::test_env;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

use super::claim_section::Owner;
use crate::contracts::TaskStoreBackend;

const REPORT: &str = "advisory file lock held past its threshold";

type Events = Arc<Mutex<Vec<BTreeMap<String, String>>>>;

struct SlowReport {
    entered: mpsc::SyncSender<()>,
    release: Mutex<mpsc::Receiver<()>>,
    first: AtomicBool,
    events: Events,
}

impl<S: tracing::Subscriber> Layer<S> for SlowReport {
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        if event.metadata().target() != "orbit.common.fs.file_lock" {
            return;
        }
        struct Fields(BTreeMap<String, String>);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0.insert(
                    field.name().into(),
                    format!("{value:?}").trim_matches('"').into(),
                );
            }
        }
        let mut fields = Fields(BTreeMap::new());
        event.record(&mut fields);
        let report = fields
            .0
            .get("message")
            .is_some_and(|message| message == REPORT);
        self.events.lock().expect("events").push(fields.0);
        if report && !self.first.swap(true, Ordering::Relaxed) {
            self.entered.send(()).expect("report entered");
            self.release
                .lock()
                .expect("release channel")
                .recv_timeout(Duration::from_secs(10))
                .expect("release slow report");
        }
    }
}

fn isolated(test: &str, ignored: bool) -> bool {
    let test = test.strip_prefix("orbit_store::").expect("crate prefix");
    const MARKER: &str = "ORBIT_READ_LATENCY_CHILD";
    if std::env::var(MARKER).as_deref() == Ok(test) {
        return true;
    }
    let root = tempfile::tempdir_in(test_env::canonical_temp_dir()).expect("home");
    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .args(["--exact", test, "--nocapture"])
        .env(MARKER, test)
        .env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .current_dir(root.path());
    if ignored {
        command.arg("--ignored");
    }
    let output = test_env::run_child_test(&mut command, test, root.path());
    if ignored {
        use std::io::Write;
        std::io::stdout()
            .write_all(&output.stdout)
            .expect("forward performance measurements");
    }
    test_env::assert_child_test_passed(test, output.status, output.stdout, output.stderr);
    false
}

/// Start a real admission with short work and park its first release report.
/// All threads are joined before the fixture can return or assert results.
fn stalled_report<T>(
    owner: &Owner,
    delay: Option<Duration>,
    op: impl FnOnce() -> T,
) -> (T, Vec<BTreeMap<String, String>>) {
    let boundary = owner.boundary_with(FileLockOptions {
        record_shared_holders: true,
        warn_held_after: Some(Duration::from_millis(20)),
        ..FileLockOptions::default()
    });
    let events = Events::default();
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let dispatch = tracing::Dispatch::new(Registry::default().with(SlowReport {
        entered: entered_tx,
        release: Mutex::new(release_rx),
        first: AtomicBool::new(false),
        events: Arc::clone(&events),
    }));
    std::thread::scope(|scope| {
        let holder = scope.spawn(move || {
            tracing::dispatcher::with_default(&dispatch, || {
                boundary
                    .with_admission(|| {
                        hold_step("bounded operation", || {
                            std::thread::sleep(Duration::from_millis(80))
                        });
                        Ok(())
                    })
                    .expect("admission")
            })
        });
        entered_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("report reached");
        let timer_tx = release_tx.clone();
        let timer = delay.map(|delay| {
            scope.spawn(move || {
                std::thread::sleep(delay);
                timer_tx.send(()).expect("release slow report");
            })
        });
        let value = op();
        if let Some(timer) = timer {
            timer.join().expect("release timer joined");
        } else {
            release_tx.send(()).expect("release report");
        }
        holder.join().expect("holder joined");
        let events = events.lock().expect("events").clone();
        (value, events)
    })
}

#[test]
fn slow_release_reports_do_not_hold_task_boundaries() {
    let test = concat!(
        module_path!(),
        "::slow_release_reports_do_not_hold_task_boundaries"
    );
    if !isolated(test, false) {
        return;
    }
    let owner = Owner::open(1);
    let expected = TaskStoreBackend::list_tasks(owner.tasks.as_ref()).expect("tasks");
    let (locks, _) = stalled_report(&owner, None, || {
        let partition = owner.boundary.partition_dir.join(".task-commit.lock");
        let host = partition
            .ancestors()
            .nth(3)
            .expect("tasks root")
            .join(".host-task-commit.lock");
        let locks = [
            try_acquire_exclusive_file_lock(&partition, "partition probe")
                .expect("partition probe")
                .is_some(),
            try_acquire_exclusive_file_lock(&host, "host probe")
                .expect("host probe")
                .is_some(),
        ];
        if locks == [true, true] {
            let task = TaskStoreBackend::get_task(owner.tasks.as_ref(), &expected[0].id)
                .expect("task")
                .expect("present");
            assert_eq!(
                serde_json::to_value(task).unwrap(),
                serde_json::to_value(&expected[0]).unwrap()
            );
            let search =
                TaskStoreBackend::search_tasks(owner.tasks.as_ref(), "Task").expect("search");
            assert_eq!(search.len(), 1);
            assert_eq!(search[0].id, expected[0].id);
        }
        locks
    });
    assert_eq!(
        locks,
        [true, true],
        "slow report I/O must release both nested task boundaries first (ORB-19458)"
    );
}

/// Run explicitly on base and candidate with the same representative bundles.
/// Prints one JSON record per sequential public API read for task artifacts.
#[test]
#[ignore = "bounded before/after performance experiment; 20 sequential samples"]
#[allow(
    clippy::print_stdout,
    reason = "the performance experiment emits JSON measurements"
)]
fn task_read_report_latency_probe() {
    let test = concat!(module_path!(), "::task_read_report_latency_probe");
    if !isolated(test, true) {
        return;
    }
    let owner = Owner::open(8);
    let expected = TaskStoreBackend::list_tasks(owner.tasks.as_ref()).expect("tasks");
    for method in ["task show", "search"] {
        for sample in 0..10 {
            let started = Instant::now();
            let (elapsed, events) =
                stalled_report(&owner, Some(Duration::from_millis(2200)), || {
                    let started = Instant::now();
                    if method == "task show" {
                        let task =
                            TaskStoreBackend::get_task(owner.tasks.as_ref(), &expected[0].id)
                                .expect("read")
                                .expect("task");
                        assert_eq!(
                            serde_json::to_value(task).unwrap(),
                            serde_json::to_value(&expected[0]).unwrap()
                        );
                    } else {
                        let tasks = TaskStoreBackend::search_tasks(owner.tasks.as_ref(), "Task")
                            .expect("search");
                        let ids = tasks
                            .iter()
                            .map(|task| &task.id)
                            .collect::<std::collections::BTreeSet<_>>();
                        assert_eq!(ids, expected.iter().map(|task| &task.id).collect());
                    }
                    started.elapsed().as_secs_f64() * 1000.0
                });
            println!(
                "{}",
                serde_json::json!({"method":method,"sample":sample,"wall_ms":elapsed,"exit_code":0,"harness_ms":started.elapsed().as_secs_f64()*1000.0,"events":events})
            );
        }
    }
}
