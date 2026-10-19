// Admitted unit test: deterministic interleaving of expiry and single-flight
// refresh through the production entry point, without exposing memo internals.

use std::future::{Future, poll_fn};
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;
use std::time::Duration;

use orbit_core::OrbitRuntime;
use serde_json::json;
use tokio::sync::oneshot;

use crate::runtime_memo::RuntimeMemo;

async fn poll_once<F: Future>(mut future: Pin<&mut F>) -> Poll<F::Output> {
    poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await
}

#[tokio::test]
async fn expired_entry_keeps_in_flight_refresh_single_flight() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let runtime = Arc::new(OrbitRuntime::in_memory().expect("build runtime"));
        let memo = RuntimeMemo::new("single-flight regression");
        let refreshes = Arc::new(AtomicUsize::new(0));
        let ttl = Duration::from_secs(60);

        // Queue a refresh while the slot is still empty, then publish a value
        // with zero TTL. The queued caller now owns a slot with an expired
        // entry, the interleaving that expiry pruning must preserve.
        let (publish, publication) = oneshot::channel();
        let mut initial = pin!(
            memo.get_or_compute(&runtime, "key", Duration::ZERO, move || {
                publication.blocking_recv().expect("release publication");
                Ok(json!({"generation": 0}))
            })
        );
        assert!(poll_once(initial.as_mut()).await.is_pending());

        let (started, computation_started) = oneshot::channel();
        let (release, computation_released) = oneshot::channel();
        let first_count = Arc::clone(&refreshes);
        let mut first = pin!(memo.get_or_compute(&runtime, "key", ttl, move || {
            first_count.fetch_add(1, Ordering::SeqCst);
            started.send(()).expect("signal refresh started");
            computation_released
                .blocking_recv()
                .expect("release refresh");
            Ok(json!({"generation": 1}))
        }));
        assert!(poll_once(first.as_mut()).await.is_pending());
        publish.send(()).expect("publish expired entry");
        assert_eq!(
            *initial.await.expect("initial compute").body,
            json!({"generation": 0})
        );

        assert!(poll_once(first.as_mut()).await.is_pending());
        computation_started.await.expect("refresh is in flight");

        // While this refresh holds its gate, another workspace runtime and
        // another window must compute independently instead of waiting on it.
        let other_runtime = Arc::new(OrbitRuntime::in_memory().unwrap());
        let other_workspace = memo
            .get_or_compute(&other_runtime, "key", ttl, || {
                Ok(json!({"generation": "other workspace"}))
            })
            .await
            .unwrap();
        let other_window = memo
            .get_or_compute(&runtime, "other window", ttl, || {
                Ok(json!({"generation": "other window"}))
            })
            .await
            .unwrap();
        assert_eq!(
            *other_workspace.body,
            json!({"generation": "other workspace"})
        );
        assert_eq!(*other_window.body, json!({"generation": "other window"}));

        let second_count = Arc::clone(&refreshes);
        let mut second = pin!(memo.get_or_compute(&runtime, "key", ttl, move || {
            second_count.fetch_add(1, Ordering::SeqCst);
            Ok(json!({"generation": 2}))
        }));
        // Explicit polling guarantees the overlapping caller has looked up
        // its slot before the first refresh can finish; no sleeps are needed.
        let second_poll = poll_once(second.as_mut()).await;
        release.send(()).expect("finish refresh");
        let first = first.await.expect("first refresh result");
        let second = match second_poll {
            Poll::Ready(result) => result,
            Poll::Pending => second.await,
        }
        .expect("overlapping caller result");

        assert_eq!(
            refreshes.load(Ordering::SeqCst),
            1,
            "an expired entry must retain the in-flight gate and run one refresh"
        );
        assert_eq!(*first.body, json!({"generation": 1}));
        assert!(
            Arc::ptr_eq(&first.body, &second.body),
            "callers share the refreshed payload"
        );
    })
    .await
    .expect("single-flight interleaving must complete within the watchdog");
}

/// The codebase and agent scans: an entry past its TTL is answered at once
/// while one background refresh replaces it, so a dashboard request after
/// the first scan never waits on a scan, however slow.
#[tokio::test]
async fn stale_entry_is_served_while_one_refresh_replaces_it() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let runtime = Arc::new(OrbitRuntime::in_memory().expect("build runtime"));
        let memo = RuntimeMemo::new("stale-while-revalidate regression");
        let stale_for = Duration::from_secs(3600);
        let ttl = Duration::from_secs(60);
        memo.get_or_refresh(&runtime, "key", Duration::ZERO, stale_for, || {
            Ok(json!({"generation": 0}))
        })
        .await
        .expect("first scan");

        let refreshes = Arc::new(AtomicUsize::new(0));
        let (started, refresh_started) = oneshot::channel();
        let (release, refresh_released) = oneshot::channel();
        let count = Arc::clone(&refreshes);
        let stale = memo
            .get_or_refresh(&runtime, "key", ttl, stale_for, move || {
                count.fetch_add(1, Ordering::SeqCst);
                started.send(()).expect("signal refresh started");
                refresh_released.blocking_recv().expect("release refresh");
                Ok(json!({"generation": 1}))
            })
            .await
            .expect("stale read");
        assert_eq!(
            *stale.body,
            json!({"generation": 0}),
            "the stale scan is served while the refresh is still blocked"
        );
        refresh_started.await.expect("refresh is in flight");

        let count = Arc::clone(&refreshes);
        let again = memo
            .get_or_refresh(&runtime, "key", ttl, stale_for, move || {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(json!({"generation": "second refresh"}))
            })
            .await
            .expect("second stale read");
        assert_eq!(*again.body, json!({"generation": 0}));

        release.send(()).expect("finish refresh");
        // A miss waits on the refresh's gate and then finds its result.
        let refreshed = memo
            .get_or_compute(&runtime, "key", ttl, || {
                Ok(json!({"generation": "recomputed"}))
            })
            .await
            .expect("refreshed read");
        assert_eq!(*refreshed.body, json!({"generation": 1}));
        assert_eq!(
            refreshes.load(Ordering::SeqCst),
            1,
            "stale reads during a refresh must not start another"
        );
    })
    .await
    .expect("stale-while-revalidate must complete within the watchdog");
}
