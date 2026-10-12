//! `orbit config show` reads the after-landing consumer's open instance,
//! wedged action and last batch from the task store. Each read used to enter
//! the store's commit boundary on its own, taking the partition lock and
//! writing a holder record every time; one view now takes it once.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::Utc;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

use crate::auto_task_lifecycle_cli::git;
use crate::isolated_cli_fixture::Fixture;
use crate::review_after_landing_cli::{
    commit, enable_review_crew, in_isolated_child, land_and_evaluate, open_runtime, retarget,
    set_policy, toggle, trigger,
};

/// Counts acquisitions of the task store's partition commit lock: the file
/// lock records one event per acquisition a caller paid for, and none for a
/// re-entrant nested section.
struct CommitLockAcquisitions(Arc<AtomicU64>);

impl<S: tracing::Subscriber> Layer<S> for CommitLockAcquisitions {
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        struct LockPath(String);
        impl tracing::field::Visit for LockPath {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "lock_path" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        if event.metadata().target() != "orbit.common.fs.file_lock" {
            return;
        }
        let mut path = LockPath(String::new());
        event.record(&mut path);
        if path.0.ends_with("/.task-commit.lock") {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Commit-lock acquisitions `op` paid for on this thread.
fn acquisitions<T>(op: impl FnOnce() -> T) -> (T, u64) {
    let count = Arc::new(AtomicU64::new(0));
    let subscriber =
        tracing_subscriber::registry().with(CommitLockAcquisitions(Arc::clone(&count)));
    let output = tracing::subscriber::with_default(subscriber, op);
    (output, count.load(Ordering::Relaxed))
}

#[test]
fn one_config_show_takes_the_task_commit_lock_once() {
    const TEST: &str = "config_show_task_reads::one_config_show_takes_the_task_commit_lock_once";
    if !in_isolated_child(TEST) {
        return;
    }
    let fixture = Fixture::new();
    enable_review_crew(&fixture);
    git(&fixture, &["checkout", "-b", "fixture-delivery"]);
    commit(&fixture, "baseline\n");
    retarget(&fixture, &trigger());
    set_policy(&fixture, "operation.review_crew", "sonnet");
    toggle(&fixture, "on");
    let runtime = open_runtime(&fixture);
    land_and_evaluate(&fixture, &runtime, "landed\n")
        .expect("an enabled consumer mints a review task for the landed batch");
    drop(runtime);

    // What `orbit config show` resolves its review section from.
    let runtime = open_runtime(&fixture);
    let (view, taken) = acquisitions(|| {
        orbit_core::application::review::review_switches_view(
            &runtime,
            runtime.operation_policy(),
            Utc::now(),
        )
    });
    let health = &view["after_landing"]["health"];
    assert!(
        health["last_batch_minted_at"].is_string(),
        "the view read the consumer's batch from the task store: {view}"
    );
    assert_eq!(
        taken, 1,
        "every task read of one config show shares one commit-lock acquisition"
    );

    // The dashboard's effective view embeds the same section.
    let (effective, taken) =
        acquisitions(|| orbit_core::application::config::effective_view(&runtime).unwrap());
    assert!(effective["review"]["after_landing"]["health"].is_object());
    assert_eq!(taken, 1, "the effective config view takes the lock once");
}
