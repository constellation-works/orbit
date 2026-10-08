//! Heap policy for the long-running dashboard server.
//!
//! On Linux with glibc, a request handler on a tokio blocking-pool thread
//! allocates from that thread's malloc arena. glibc grows secondary arenas in
//! 64 MiB heaps and keeps their freed pages resident, so each burst of
//! dashboard polls left tens of megabytes behind in whichever arenas served it,
//! and resident memory ratcheted towards the arena count times the largest
//! request peak. Two settings bound that:
//!
//! - [`configure`] caps the arena count and pins the mmap threshold, so
//!   blocking threads share a few arenas and large buffers (response bodies,
//!   result vectors) are mapped and returned to the kernel when freed rather
//!   than parked in an arena.
//! - [`trim_after_requests`] releases the arenas' free pages a moment after
//!   each burst of requests settles.
//!
//! This is glibc tuning rather than a replacement allocator: it needs no C
//! build dependency and applies only to the dashboard server, so short-lived
//! CLI commands keep glibc's defaults. Elsewhere both functions are no-ops.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::Request;
use axum::middleware::{self, Next};
use tokio::sync::Notify;

/// Arenas shared by every server thread, in place of glibc's default of eight
/// per core, each of which can hold a 64 MiB heap. The handlers are mostly
/// SQLite and serialization work; on the soak fixture one, two and four
/// arenas settled at the same resident size.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
const ARENA_MAX: libc::c_int = 2;

/// Allocations at least this large are served by `mmap` and unmapped on free.
/// Setting it also stops glibc raising the threshold (up to 32 MiB) after a
/// large block is freed, which would otherwise keep later large buffers on
/// the heap.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
const MMAP_THRESHOLD: libc::c_int = 256 * 1024;

/// How long to wait after a response before trimming. Requests arriving in
/// the meantime fold into the same trim, so one dashboard poll costs one trim.
const TRIM_DELAY: Duration = Duration::from_secs(1);

/// Apply the server's allocator policy. Call before the server starts its
/// threads; arenas that already exist are not affected by the cap.
pub(crate) fn configure() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        for (param, value, name) in [
            (libc::M_ARENA_MAX, ARENA_MAX, "M_ARENA_MAX"),
            (libc::M_MMAP_THRESHOLD, MMAP_THRESHOLD, "M_MMAP_THRESHOLD"),
        ] {
            // SAFETY: `mallopt` only adjusts allocator tunables; both values
            // are within glibc's accepted ranges.
            if unsafe { libc::mallopt(param, value) } == 0 {
                tracing::warn!(param = name, value, "mallopt rejected heap setting");
            }
        }
    }
}

/// Wrap `app` so the heap is trimmed shortly after requests complete. Must be
/// called inside the server's tokio runtime, which owns the trim task.
pub(crate) fn trim_after_requests(app: Router) -> Router {
    if !cfg!(all(target_os = "linux", target_env = "gnu")) {
        return app;
    }
    let pending = Arc::new(Notify::new());
    tokio::spawn(trim_loop(Arc::clone(&pending)));
    app.layer(middleware::from_fn(move |request: Request, next: Next| {
        let pending = Arc::clone(&pending);
        async move {
            let response = next.run(request).await;
            // `notify_one` keeps a single permit, so a burst of responses
            // while a trim is waiting or running schedules at most one more.
            pending.notify_one();
            response
        }
    }))
}

async fn trim_loop(pending: Arc<Notify>) {
    loop {
        pending.notified().await;
        tokio::time::sleep(TRIM_DELAY).await;
        // Trimming walks every arena under its lock, so keep it off the
        // async workers.
        if let Err(error) = tokio::task::spawn_blocking(release_free_pages).await {
            tracing::warn!(%error, "heap trim task failed");
        }
    }
}

fn release_free_pages() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: `malloc_trim` only returns free pages to the kernel; it never
    // touches live allocations.
    unsafe {
        libc::malloc_trim(0);
    }
}
