//! Process-local single-flight TTL cache keyed by runtime identity.
//!
//! Every visible dashboard tab polls the same endpoints. Keying by the live
//! runtime plus a request key collapses overlapping polls into one compute,
//! and a runtime rebuild naturally starts a fresh cache namespace. Dashboard
//! memos use it: `/api/audit/summary` (keyed by the raw `since` window, so relative
//! cutoffs such as `24h` still hit), audited plugin panel reads,
//! `/api/diagnostics/errors` (keyed by its time range and row limit), and
//! `/api/diagnostics/friction` (keyed by month and row limit), and
//! `/api/scoreboard` (keyed by the canonical window).

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use orbit_core::{OrbitError, OrbitRuntime};
use serde_json::Value;

/// Freshness bound for a cached audit summary. Short enough that header tiles
/// still move, long enough that overlapping dashboard polls collapse.
pub(crate) const AUDIT_SUMMARY_TTL: Duration = Duration::from_secs(15);

/// Freshness bound for scoreboard summaries and their joined incident scans.
pub(crate) const SCOREBOARD_TTL: Duration = Duration::from_secs(15);

/// Freshness bound for a cached diagnostics error list. The Errors tab polls
/// every 30s and each miss scans up to 50k audit rows plus stderr blobs, so
/// this only needs to collapse overlapping tabs, not hide new errors long.
pub(crate) const DIAGNOSTICS_ERRORS_TTL: Duration = Duration::from_secs(15);

/// Freshness bound for cached friction rows. Matches the diagnostics errors
/// memo so overlapping Runs-tab polls share one bounded audit/blob scan.
pub(crate) const DIAGNOSTICS_FRICTION_TTL: Duration = Duration::from_secs(15);

/// In-process TTL cache and single-flight gate for one dashboard server.
pub(crate) struct RuntimeMemo<K> {
    /// Names the computation in a panic report and trace events.
    label: &'static str,
    slots: Mutex<HashMap<(usize, K), Arc<Slot>>>,
}

struct Slot {
    /// Per-key gate so concurrent misses share one compute. Never held while
    /// the map lock is held, and never held across a different key's work.
    gate: tokio::sync::Mutex<()>,
    value: Mutex<Option<Cached>>,
}

struct Cached {
    runtime: Weak<OrbitRuntime>,
    computed_at: Instant,
    ttl: Duration,
    body: Arc<Value>,
}

impl<K: Eq + Hash> RuntimeMemo<K> {
    pub(crate) fn new(label: &'static str) -> Self {
        Self {
            label,
            slots: Mutex::new(HashMap::new()),
        }
    }

    /// Return a payload computed for the same runtime and key within `ttl`, or
    /// run `compute` once while concurrent callers for that key wait. A failed
    /// compute is not stored, so the next waiter retries.
    pub(crate) async fn get_or_compute<F>(
        &self,
        runtime: &Arc<OrbitRuntime>,
        key: K,
        ttl: Duration,
        compute: F,
    ) -> Result<Arc<Value>, OrbitError>
    where
        F: FnOnce() -> Result<Value, OrbitError> + Send + 'static,
    {
        let slot = self.slot((Arc::as_ptr(runtime) as usize, key));
        if let Some(hit) = slot.hit(runtime) {
            tracing::debug!(memo = self.label, "cache hit");
            return Ok(hit);
        }

        let _gate = slot.gate.lock().await;
        if let Some(hit) = slot.hit(runtime) {
            tracing::debug!(memo = self.label, "cache hit");
            return Ok(hit);
        }

        tracing::debug!(memo = self.label, "cache miss");
        let computed = match tokio::task::spawn_blocking(compute).await {
            Ok(Ok(value)) => Arc::new(value),
            Ok(Err(error)) => return Err(error),
            Err(join_err) => {
                return Err(OrbitError::Execution(format!(
                    "{} panicked: {join_err}",
                    self.label
                )));
            }
        };
        *slot.value.lock().unwrap_or_else(PoisonError::into_inner) = Some(Cached {
            runtime: Arc::downgrade(runtime),
            computed_at: Instant::now(),
            ttl,
            body: Arc::clone(&computed),
        });
        Ok(computed)
    }

    fn slot(&self, key: (usize, K)) -> Arc<Slot> {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        // Callers own a clone before acquiring the gate and through compute.
        // Keep their slots so pruning cannot create a second gate for a key.
        slots.retain(|_, slot| Arc::strong_count(slot) > 1 || slot.keep());
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
        let cached = guard.as_ref().filter(|cached| cached.is_fresh())?;
        let live = cached.runtime.upgrade()?;
        Arc::ptr_eq(&live, runtime).then(|| Arc::clone(&cached.body))
    }

    /// Drop expired or orphaned ready entries once no caller owns the slot.
    /// Empty slots stay reusable after a failed compute.
    fn keep(&self) -> bool {
        let guard = self.value.lock().unwrap_or_else(PoisonError::into_inner);
        guard
            .as_ref()
            .is_none_or(|cached| cached.is_fresh() && cached.runtime.strong_count() > 0)
    }
}

impl Cached {
    fn is_fresh(&self) -> bool {
        self.computed_at.elapsed() < self.ttl
    }
}
