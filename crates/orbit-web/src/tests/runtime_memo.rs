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
            *initial.await.expect("initial compute"),
            json!({"generation": 0})
        );

        assert!(poll_once(first.as_mut()).await.is_pending());
        computation_started.await.expect("refresh is in flight");

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
        assert_eq!(*first, json!({"generation": 1}));
        assert!(
            Arc::ptr_eq(&first, &second),
            "callers share the refreshed payload"
        );
    })
    .await
    .expect("single-flight interleaving must complete within the watchdog");
}
