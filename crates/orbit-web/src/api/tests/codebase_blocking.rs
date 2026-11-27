//! File-rollup codebase responses evaluate checkout paths off the async
//! executor, on memo hits as well as misses.
//!
//! The history scan is already off the executor inside the memo. The slicing
//! that follows it — including the file-rollup `canonicalize` checkout
//! lookups — used to run inline in `codebase_analytics`/`codebase_show`, so a
//! slow checkout parked the request worker even on a cache hit. Both handlers
//! now run that slicing in `blocking_join`.
//!
//! This test pins the boundary deterministically without timing: a runtime
//! with a single blocking thread is held by a gated `spawn_blocking`, and a
//! warm (memo-hit) file-rollup request must stay pending until the gate
//! opens. Before the offload the warm request needed no blocking thread, so
//! the same poll completed at once. The offloaded closure is unconditional,
//! shared by the miss and hit paths, so the warm pend covers both; the cold
//! miss payload below locks that the miss path still answers correctly.

use std::future::{Future, poll_fn};
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use orbit_core::test_support::{FixtureRuntime, fixture_core};
use tokio::sync::oneshot;
use tower::ServiceExt;

use super::super::router;
use super::test_support::{body_json, enter_isolated_child};
use crate::state::DashboardState;

async fn poll_once<F: Future>(mut future: Pin<&mut F>) -> Poll<F::Output> {
    poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await
}

fn analytics_request() -> Request<Body> {
    Request::builder()
        .uri("/codebase/analytics?workspace=default&window=all&min_support=1&rollup=file")
        .header(header::HOST, "localhost:7878")
        .header(header::ORIGIN, "http://localhost:7878")
        .body(Body::empty())
        .expect("analytics request")
}

fn show_request(path: &str) -> Request<Body> {
    Request::builder()
        .uri(format!(
            "/codebase/show?workspace=default&window=all&min_support=1&rollup=file&path={path}"
        ))
        .header(header::HOST, "localhost:7878")
        .header(header::ORIGIN, "http://localhost:7878")
        .body(Body::empty())
        .expect("show request")
}

#[test]
fn codebase_file_rollup_rank_runs_on_blocking_pool_on_memo_hit() {
    if !enter_isolated_child(
        module_path!(),
        "codebase_file_rollup_rank_runs_on_blocking_pool_on_memo_hit",
    ) {
        return;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .expect("constrained runtime");
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(30), async {
            let runtime = Arc::new(FixtureRuntime::in_memory().expect("build runtime"));
            let state = DashboardState::single(Arc::new(fixture_core(&runtime)));

            // Cold file-rollup analytics: a scan miss that also locks the miss
            // payload. The memo holds the scan afterwards, so the requests
            // below are hits whose only blocking-pool use is the rank and its
            // checkout lookups.
            let cold = router(state.clone())
                .with_state(state.clone())
                .oneshot(analytics_request())
                .await
                .expect("cold analytics response");
            assert_eq!(cold.status(), StatusCode::OK);
            assert_eq!(
                cold.headers()
                    .get(header::AGE)
                    .map(|value| value.to_str().expect("age value")),
                Some("0"),
                "a fresh scan reports its age"
            );
            let cold_body = body_json(cold).await;
            assert_eq!(cold_body["cache_ttl_seconds"], 300);

            // Hold the only blocking thread. No sleeps: the gate signals once
            // it owns the thread, so the pending assertions below are exact.
            let (release_tx, release_rx) = oneshot::channel::<()>();
            let (held_tx, held_rx) = oneshot::channel::<()>();
            let _held = tokio::task::spawn_blocking(move || {
                held_tx.send(()).expect("signal pool held");
                // A failed assertion below drops the release half; exiting
                // then is the clean failure path, not a second panic.
                let _ = release_rx.blocking_recv();
            });
            held_rx.await.expect("blocking pool held");

            // Warm analytics: memo hit, so a wait here is the offloaded rank.
            let mut warm_analytics = pin!(
                router(state.clone())
                    .with_state(state.clone())
                    .oneshot(analytics_request())
            );
            assert!(
                poll_once(warm_analytics.as_mut()).await.is_pending(),
                "warm file-rollup analytics must wait for the blocking pool: \
                 its checkout lookups run off the executor"
            );
            // Warm show error path: the unknown-path 400 is computed inside
            // the same offloaded detail call, so it waits too.
            let mut warm_show = pin!(
                router(state.clone())
                    .with_state(state.clone())
                    .oneshot(show_request("src/gone.rs"))
            );
            assert!(
                poll_once(warm_show.as_mut()).await.is_pending(),
                "warm show detail must wait for the blocking pool: \
                 its checkout lookup runs off the executor"
            );
            // Request validation stays on the executor: a bad selection is
            // rejected at once without touching the held pool.
            let mut invalid = pin!(
                router(state.clone()).with_state(state.clone()).oneshot(
                    Request::builder()
                        .uri("/codebase/analytics?workspace=default&window=1y")
                        .header(header::HOST, "localhost:7878")
                        .header(header::ORIGIN, "http://localhost:7878")
                        .body(Body::empty())
                        .expect("invalid request"),
                )
            );
            match poll_once(invalid.as_mut()).await {
                Poll::Ready(response) => {
                    let response = response.expect("invalid analytics response");
                    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
                }
                Poll::Pending => panic!("selection errors must not wait for the blocking pool"),
            }

            release_tx.send(()).expect("release pool");
            let warm = warm_analytics.await.expect("warm analytics response");
            assert_eq!(warm.status(), StatusCode::OK);
            let warm_body = body_json(warm).await;
            assert_eq!(
                warm_body["computed_at"], cold_body["computed_at"],
                "a request inside the cache period reuses the scan"
            );
            assert_eq!(warm_body["cache_ttl_seconds"], 300);

            let show = warm_show.await.expect("warm show response");
            assert_eq!(
                show.status(),
                StatusCode::BAD_REQUEST,
                "the offloaded detail keeps the unknown-path 400"
            );
        })
        .await
        .expect("blocking-boundary interleaving must complete within the watchdog");
    });
}
