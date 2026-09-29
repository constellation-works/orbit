use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;
use std::pin::pin;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::Utc;
use orbit_core::OrbitError;
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceRegistry, WorkspaceStatus};
use tokio::sync::Notify;
use tokio::time::Instant;

use super::super::{build_state, check_bindable_host, drain_with_grace_period};
use crate::serve::{build_app, security_headers};

#[test]
fn allows_ipv4_loopback() {
    let host = IpAddr::V4(Ipv4Addr::LOCALHOST);
    assert!(check_bindable_host(host, 7878).is_ok());
}

#[test]
fn allows_ipv6_loopback() {
    let host = IpAddr::V6(Ipv6Addr::LOCALHOST);
    assert!(check_bindable_host(host, 7878).is_ok());
}

#[test]
fn allows_127_0_0_x_range() {
    // The whole 127.0.0.0/8 block is loopback.
    let host = IpAddr::V4(Ipv4Addr::new(127, 5, 5, 5));
    assert!(check_bindable_host(host, 7878).is_ok());
}

#[test]
fn rejects_unspecified_address() {
    // `--host 0.0.0.0` is the exact exposure the guard exists to block.
    let host = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
    let err = check_bindable_host(host, 7878).expect_err("0.0.0.0 must be rejected");
    assert!(matches!(err, OrbitError::InvalidInput(_)));
}

#[test]
fn rejects_lan_address() {
    let host = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50));
    let err = check_bindable_host(host, 7878).expect_err("LAN address must be rejected");
    assert!(matches!(err, OrbitError::InvalidInput(_)));
}

// ORB-11255: `drain_with_grace_period` is the seam that lets the grace-period
// regression run without the real `SHUTDOWN_GRACE_PERIOD` (see
// `crates/orbit-cli/tests/web_serve_shutdown.rs` for the real-server smoke
// test that exercises the same behavior end to end). These tests run on
// paused Tokio time: timers auto-advance only when the runtime is idle, so
// every bound below is exact virtual time, independent of host scheduling,
// and a missing notification or backstop trips a watchdog immediately.

const GRACE_PERIOD: Duration = Duration::from_secs(10);

/// The last virtual instant at which the grace-period backstop must still
/// be pending. Paused time advances in whole milliseconds.
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
async fn grace_period_starts_only_once_shutdown_is_signaled() {
    let notify = Arc::new(Notify::new());
    let mut drain = pin!(drain_with_grace_period(
        pending_drain(),
        Arc::clone(&notify),
        GRACE_PERIOD
    ));

    // Serve for longer than the grace period before the signal arrives: if
    // the grace-period clock started when serving began (the bug), it would
    // already be exhausted.
    assert!(
        tokio::time::timeout(GRACE_PERIOD * 2, &mut drain)
            .await
            .is_err(),
        "drain_with_grace_period resolved before shutdown was signaled"
    );

    notify.notify_one();
    // A drain that returns as soon as it is notified would abandon open
    // connections immediately; the backstop must allow the full interval.
    assert_backstop_fires_after(&mut drain, JUST_BEFORE_GRACE).await;
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
async fn drain_completing_first_wins_even_after_shutdown_is_signaled() {
    let notify = Arc::new(Notify::new());
    notify.notify_one(); // shutdown already requested at first poll

    // Connections finish closing halfway through the grace period. The
    // backstop also returns `Ok(())`, so record completion separately to
    // prove the drain's result is the one returned.
    let drained = Arc::new(AtomicBool::new(false));
    let drained_by_server = Arc::clone(&drained);
    let drain = async move {
        tokio::time::sleep(GRACE_PERIOD / 2).await;
        drained_by_server.store(true, Ordering::SeqCst);
        Ok(())
    };

    let started = Instant::now();
    let result = tokio::time::timeout(
        GRACE_PERIOD * 2,
        drain_with_grace_period(drain, notify, GRACE_PERIOD),
    )
    .await
    .expect("drain_with_grace_period never resolved");
    assert!(result.is_ok(), "completed drain must succeed: {result:?}");
    assert!(
        drained.load(Ordering::SeqCst),
        "returned after {:?} before the drain completed",
        started.elapsed()
    );
    assert_eq!(
        started.elapsed(),
        GRACE_PERIOD / 2,
        "a drain that completes first must return as soon as it completes"
    );
}

#[tokio::test]
async fn drain_error_propagates_as_execution_error() {
    let notify = Arc::new(Notify::new());
    let drain = async { Err(std::io::Error::other("boom")) };
    let err = drain_with_grace_period(drain, notify, Duration::from_secs(10))
        .await
        .expect_err("serve error must propagate");
    assert!(matches!(err, OrbitError::Execution(msg) if msg.contains("boom")));
}

// ── explicit `--root` isolation (ORB-11388) ───────────────────────────────

/// Register one active workspace named `id` in `<root>/workspaces.json`.
fn seed_registry(root: &Path, id: &str) {
    let now = Utc::now();
    let repo_root = root.join(id);
    let registry = WorkspaceRegistry {
        workspaces: vec![Workspace {
            id: id.to_string(),
            name: id.to_string(),
            owner_machine_id: None,
            git_remote: None,
            ship_mode: None,
            base_branch: "main".to_string(),
            status: WorkspaceStatus::Active,
            created_at: now,
            updated_at: now,
        }],
        checkouts: vec![WorkspaceCheckout::owner(
            id.to_string(),
            repo_root.clone(),
            repo_root.join(".orbit"),
        )],
        ..Default::default()
    };
    orbit_registry::workspace_registry::save_registry_to(&registry, &root.join("workspaces.json"))
        .expect("save registry");
}

/// `orbit --root <ROOT> web serve` must enumerate `<ROOT>/workspaces.json` and
/// nothing from the machine-global registry. Two scratch roots are served in
/// turn: each must expose only its own workspace, which cannot happen if the
/// server resolves the registry from `~/.orbit` (both calls would then return
/// the same host-global set, matching neither).
#[test]
fn explicit_root_serves_only_that_roots_registry() {
    let alpha_root = tempfile::tempdir().expect("tempdir");
    let beta_root = tempfile::tempdir().expect("tempdir");
    seed_registry(alpha_root.path(), "alpha");
    seed_registry(beta_root.path(), "beta");

    let alpha = build_state(Some(alpha_root.path()), None).expect("state for alpha root");
    let beta = build_state(Some(beta_root.path()), None).expect("state for beta root");

    assert_eq!(
        alpha
            .entries()
            .iter()
            .map(|entry| entry.id.clone())
            .collect::<Vec<_>>(),
        vec!["alpha".to_string()]
    );
    assert_eq!(
        beta.entries()
            .iter()
            .map(|entry| entry.id.clone())
            .collect::<Vec<_>>(),
        vec!["beta".to_string()]
    );

    // Per-workspace runtimes are opened under the served root too, so a
    // dashboard write lands in the isolated data directory rather than the
    // host-global one.
    assert_eq!(alpha.global_root(), alpha_root.path());
    assert_eq!(beta.global_root(), beta_root.path());
}

/// Without `--root`, serve reads the registry under the machine-global root.
/// Run the state constructor in a child so its home and managed-run authority
/// cannot affect the test runner's registry.
#[test]
fn without_root_override_serves_the_global_registry() {
    let home = tempfile::tempdir().expect("disposable home");
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let output = command
        .args([
            "--ignored",
            "--exact",
            "tests::serve::without_root_override_child",
        ])
        .current_dir(home.path())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("ORBIT_WEB_TEST_NO_ROOT_HOME", home.path())
        .output()
        .expect("run isolated no-root case");
    assert!(
        output.status.success(),
        "isolated no-root case failed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        home.path().join(".orbit/child-case-ran").exists(),
        "requested no-root child case did not complete"
    );
}

/// Invoked only by the wrapper above, with a disposable child-local home.
#[test]
#[ignore = "spawned by without_root_override_serves_the_global_registry"]
fn without_root_override_child() {
    let home = std::env::var_os("ORBIT_WEB_TEST_NO_ROOT_HOME")
        .expect("no-root child must be launched by its wrapper");
    let home = Path::new(&home);
    let global_root = home.join(".orbit");
    assert_eq!(
        orbit_cmd::registry_runtime::global_root_for(None).expect("global root"),
        global_root,
        "omitted --root must resolve inside the disposable home"
    );
    std::fs::create_dir_all(&global_root).expect("disposable global root");
    seed_registry(&global_root, "isolated-global");

    let state = build_state(None, None).expect("state for disposable global root");
    assert_eq!(state.global_root(), global_root);
    assert_eq!(
        state
            .entries()
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        vec!["isolated-global"]
    );
    std::fs::write(global_root.join("child-case-ran"), b"ok").expect("record completed child case");
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

#[tokio::test]
async fn security_headers_do_not_overwrite_a_handler_value() {
    let mut response = axum::response::Response::new(axum::body::Body::empty());
    response.headers_mut().insert(
        "referrer-policy",
        axum::http::HeaderValue::from_static("same-origin"),
    );
    let response: axum::response::Response = security_headers(response).await;
    assert_eq!(response.headers()["referrer-policy"], "same-origin");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
}
