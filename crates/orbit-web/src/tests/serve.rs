use crate::serve::{build_app, drain_with_grace_period, teardown_runtime};
use orbit_core::OrbitError;
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::Instant;

// Paused time makes the shutdown ordering and lost-wakeup regressions deterministic.
const GRACE_PERIOD: Duration = Duration::from_secs(10);
const JUST_BEFORE_GRACE: Duration = GRACE_PERIOD.saturating_sub(Duration::from_millis(1));

/// A never-completing "server" future, standing in for `axum::serve(..)`
/// while its connections never close on their own.
fn pending_drain() -> impl std::future::Future<Output = std::io::Result<()>> {
    std::future::pending()
}

/// Assert `drain` stays pending for `interval` of virtual time, then that it
/// resolves before a watchdog of one further `GRACE_PERIOD` expires.
async fn assert_backstop_fires_after(
    drain: &mut (impl std::future::Future<Output = Result<(), OrbitError>> + Unpin),
    interval: Duration,
) {
    let started = Instant::now();
    assert!(
        tokio::time::timeout(interval, &mut *drain).await.is_err(),
        "drain_with_grace_period resolved {:?} after shutdown was signaled, \
         before the {GRACE_PERIOD:?} grace period elapsed",
        started.elapsed()
    );
    let result = tokio::time::timeout(GRACE_PERIOD, &mut *drain)
        .await
        .expect("grace-period backstop never fired after shutdown was signaled");
    assert!(result.is_ok(), "backstop must exit cleanly: {result:?}");
    assert!(
        started.elapsed() >= GRACE_PERIOD,
        "backstop fired after {:?}, before the full {GRACE_PERIOD:?} grace period",
        started.elapsed()
    );
}

#[tokio::test(start_paused = true)]
async fn grace_period_does_not_start_before_shutdown_is_signaled() {
    let notify = Arc::new(Notify::new());

    // Nobody ever calls `notify.notify_one()`, i.e. no shutdown was
    // requested. Waiting well past the grace period must not resolve the
    // future -- this is the exact PR1328 regression: a timeout that starts
    // counting down when serving begins, not when shutdown is requested,
    // would fire here and tear down a healthy server.
    let result = tokio::time::timeout(
        GRACE_PERIOD * 4,
        drain_with_grace_period(pending_drain(), notify, GRACE_PERIOD),
    )
    .await;
    assert!(
        result.is_err(),
        "drain_with_grace_period resolved without a shutdown signal ever firing"
    );
}

#[tokio::test(start_paused = true)]
async fn early_signal_before_first_poll_is_not_a_lost_wakeup() {
    // Regression for a real race the reviewer flagged: in production, `drain`
    // is polled as part of the `select!` inside `drain_with_grace_period` and
    // can resolve the shutdown signal (calling the notify) before
    // `grace_elapsed`'s `notified().await` has ever been polled for the first
    // time. `Notify::notify_waiters` only wakes *already-registered* waiters
    // and would silently drop a notification that arrives this early,
    // leaving the grace timer never started -- an uncooperative drain would
    // then hang forever, reintroducing the exact ORB-11246 bug this whole
    // mechanism exists to prevent. `notify_one` stores a permit instead, so
    // the grace period starts at the first poll.
    let notify = Arc::new(Notify::new());
    notify.notify_one(); // fires before `drain_with_grace_period` even exists

    let mut drain = pin!(drain_with_grace_period(
        pending_drain(),
        notify,
        GRACE_PERIOD
    ));
    assert_backstop_fires_after(&mut drain, JUST_BEFORE_GRACE).await;
}

#[tokio::test(start_paused = true)]
async fn teardown_returns_after_grace_period_with_blocking_work_in_flight() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("build blocking-task runtime");
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    runtime.spawn_blocking(move || {
        started_tx.send(()).expect("report blocking task start");
        release_rx.recv().expect("wait for test release");
        finished_tx.send(()).expect("report blocking task finish");
    });
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("blocking task did not start");

    // Drain the server through its grace-period backstop while the independent
    // runtime still owns a blocking closure that cannot finish on its own.
    let notify = Arc::new(Notify::new());
    notify.notify_one();
    let grace_started = Instant::now();
    let drain_result = drain_with_grace_period(pending_drain(), notify, GRACE_PERIOD).await;
    assert!(
        drain_result.is_ok(),
        "grace-period drain failed: {drain_result:?}"
    );
    assert!(
        grace_started.elapsed() >= GRACE_PERIOD,
        "drain returned before its grace period elapsed"
    );

    let teardown_started = std::time::Instant::now();
    teardown_runtime(runtime);
    assert!(
        teardown_started.elapsed() < Duration::from_secs(1),
        "runtime teardown waited for the in-flight blocking closure"
    );

    release_tx.send(()).expect("release blocking task");
    finished_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("blocking task did not finish after release");
}

async fn app_response(uri: &str) -> axum::response::Response {
    use tower::ServiceExt;

    let runtime = orbit_core::OrbitRuntime::in_memory().expect("build runtime");
    let state = crate::state::DashboardState::single(Arc::new(runtime));
    build_app(state)
        .expect("build app")
        .oneshot(
            axum::http::Request::builder()
                .uri(uri)
                .header("host", "localhost:7878")
                .body(axum::body::Body::empty())
                .expect("request"),
        )
        .await
        .expect("response")
}

/// Every response, API and static alike, carries the baseline hardening
/// headers, and the static asset keeps its own CSP.
#[tokio::test]
async fn api_and_static_responses_carry_baseline_security_headers() {
    for uri in ["/api/tasks", "/static/dashboard.css", "/no-such-route"] {
        let response = app_response(uri).await;
        let headers = response.headers();
        assert_eq!(headers["x-content-type-options"], "nosniff", "{uri}");
        assert_eq!(headers["referrer-policy"], "no-referrer", "{uri}");
        assert_eq!(
            headers["cross-origin-resource-policy"], "same-origin",
            "{uri}"
        );
    }
    let asset = app_response("/static/dashboard.css").await;
    assert!(
        asset.headers().contains_key("content-security-policy"),
        "static assets keep their CSP"
    );
}
