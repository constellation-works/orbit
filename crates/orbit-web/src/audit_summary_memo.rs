//! Process-local memo for `GET /api/audit/summary`.
//!
//! The dashboard polls this endpoint on every visible tab every 30s. The
//! payload is seven independent scans of the same window, so concurrent
//! waiters share one compute and a short TTL skips the scans when nothing
//! about the request key has changed. Relative windows such as `24h` parse to
//! a new cutoff on every call; the key is the raw window string plus the live
//! runtime identity, not that cutoff.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use orbit_core::{OrbitError, OrbitRuntime};
use serde_json::Value;

/// Freshness bound for a cached summary payload. Short enough that header
/// tiles still move, long enough that overlapping dashboard polls collapse.
pub(crate) const AUDIT_SUMMARY_TTL: Duration = Duration::from_secs(15);

/// In-process TTL cache and single-flight gate for one dashboard server.
pub(crate) struct AuditSummaryMemo {
    slots: Mutex<HashMap<MemoKey, Arc<Slot>>>,
    computes: AtomicU64,
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct MemoKey {
    runtime_ptr: usize,
    window: String,
}

struct Slot {
    /// Per-key gate so concurrent misses share one compute. Never held while
    /// the map lock is held, and never held across a different key's work.
    gate: tokio::sync::Mutex<()>,
    value: Mutex<Option<CachedSummary>>,
}

struct CachedSummary {
    runtime: Weak<OrbitRuntime>,
    computed_at: Instant,
    body: Arc<Value>,
}

impl AuditSummaryMemo {
    /// Empty memo; each [`crate::state::DashboardState`] owns one.
    pub(crate) fn new() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            computes: AtomicU64::new(0),
        }
    }

    /// Return a cached payload when the same runtime and window were computed
    /// within [`AUDIT_SUMMARY_TTL`]; otherwise run `compute` once and publish.
    /// A failed compute is not stored, so the next waiter retries.
    pub(crate) async fn get_or_compute<F>(
        &self,
        runtime: &Arc<OrbitRuntime>,
        window: &str,
        compute: F,
    ) -> Result<Arc<Value>, OrbitError>
    where
        F: FnOnce() -> Result<Value, OrbitError> + Send + 'static,
    {
        let key = MemoKey {
            runtime_ptr: Arc::as_ptr(runtime) as usize,
            window: window.to_string(),
        };
        let slot = self.slot(key);
        if let Some(hit) = slot.hit(runtime) {
            tracing::debug!(window, "audit summary cache hit");
            return Ok(hit);
        }

        let _gate = slot.gate.lock().await;
        if let Some(hit) = slot.hit(runtime) {
            tracing::debug!(window, "audit summary cache hit");
            return Ok(hit);
        }

        tracing::debug!(window, "audit summary cache miss");
        self.computes.fetch_add(1, Ordering::Relaxed);
        let computed = match tokio::task::spawn_blocking(compute).await {
            Ok(Ok(value)) => Arc::new(value),
            Ok(Err(error)) => return Err(error),
            Err(join_err) => {
                return Err(OrbitError::Execution(format!(
                    "audit summary aggregation panicked: {join_err}"
                )));
            }
        };
        *slot.value.lock().unwrap_or_else(PoisonError::into_inner) = Some(CachedSummary {
            runtime: Arc::downgrade(runtime),
            computed_at: Instant::now(),
            body: Arc::clone(&computed),
        });
        Ok(computed)
    }

    /// Compute attempts started since this memo was created, including failures.
    /// Test seam for proving coalescing; production never reads it.
    #[cfg(test)]
    pub(crate) fn compute_count(&self) -> u64 {
        self.computes.load(Ordering::Relaxed)
    }

    fn slot(&self, key: MemoKey) -> Arc<Slot> {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        slots.retain(|_, slot| slot.keep());
        Arc::clone(slots.entry(key).or_insert_with(|| {
            Arc::new(Slot {
                gate: tokio::sync::Mutex::new(()),
                value: Mutex::new(None),
            })
        }))
    }
}

impl Slot {
    fn hit(&self, runtime: &Arc<OrbitRuntime>) -> Option<Arc<Value>> {
        let guard = self.value.lock().unwrap_or_else(PoisonError::into_inner);
        let cached = guard.as_ref()?;
        if cached.computed_at.elapsed() > AUDIT_SUMMARY_TTL {
            return None;
        }
        let live = cached.runtime.upgrade()?;
        if !Arc::ptr_eq(&live, runtime) {
            return None;
        }
        Some(Arc::clone(&cached.body))
    }

    /// Drop expired or orphaned ready entries. Empty/`None` slots stay: they
    /// are either in-flight or reusable after a failed compute.
    fn keep(&self) -> bool {
        let guard = self.value.lock().unwrap_or_else(PoisonError::into_inner);
        match guard.as_ref() {
            None => true,
            Some(cached) => {
                cached.computed_at.elapsed() <= AUDIT_SUMMARY_TTL
                    && cached.runtime.strong_count() > 0
            }
        }
    }
}
