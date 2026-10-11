//! Process-local single-flight TTL cache keyed by runtime identity or host inputs.
//!
//! Every visible dashboard tab polls the same endpoints. Keying by the live
//! runtime plus a request key collapses overlapping polls into one compute,
//! and a runtime rebuild naturally starts a fresh cache namespace. Dashboard
//! memos use it: `/api/audit/summary` (keyed by the raw `since` window, so relative
//! cutoffs such as `24h` still hit), audited plugin panel reads,
//! `/api/diagnostics/errors` (keyed by its time range and row limit), and
//! `/api/diagnostics/friction` (keyed by month and row limit), and
//! `/api/scoreboard` (keyed by the canonical window), and the codebase
//! analytics scan behind `/api/codebase/analytics`, which caches typed facts
//! rather than a response body. `/api/routines` uses a host namespace keyed by
//! definition, config and database stamps instead of a selected runtime. Every
//! payload served from a memo reports its age in an `Age` header.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use axum::http::{HeaderName, header};
use orbit_core::{OrbitError, OrbitRuntime};
use serde_json::Value;

/// Freshness bound for the dashboard memos the 30 s poll reads: audit summary,
/// scoreboard, diagnostics errors and diagnostics friction. A viewer's polls
/// land about 31 s apart (the interval plus the refresh's own duration), so
/// three poll intervals keep the second and third poll on the entry the first
/// one computed. A miss scans up to 50k audit rows plus stderr blobs, so a new
/// failure can stay hidden for up to this long; the `Age` header shows how old
/// the served payload is.
pub(crate) const DASHBOARD_POLL_MEMO_TTL: Duration = Duration::from_secs(90);

/// Freshness bound for the codebase analytics scan. A miss reads every task
/// bundle's history, and the rankings it feeds move over days, so the 30 s
/// dashboard poll must not repeat it.
pub(crate) const CODEBASE_ANALYTICS_TTL: Duration = Duration::from_secs(300);

/// A memoized body and how long ago it was computed.
pub(crate) struct Served<V> {
    pub(crate) body: Arc<V>,
    /// Time since `body` was computed, read when it was served. Zero on a miss.
    pub(crate) age: Duration,
}

impl<V> Served<V> {
    /// `Age` header (RFC 9111 section 5.1) in whole seconds. A header rather
    /// than a body field because the friction payload is a bare JSON array.
    pub(crate) fn age_header(&self) -> [(HeaderName, String); 1] {
        [(header::AGE, self.age.as_secs().to_string())]
    }
}

/// In-process TTL cache and single-flight gate for one dashboard server.
pub(crate) struct RuntimeMemo<K, V = Value> {
    /// Names the computation in a panic report and trace events.
    label: &'static str,
    slots: Mutex<Slots<K, V>>,
}

/// One slot per (runtime identity, key); zero names the host namespace.
type Slots<K, V> = HashMap<(usize, K), Arc<Slot<V>>>;

struct Slot<V> {
    /// Per-key gate so concurrent misses share one compute. Never held while
    /// the map lock is held, and never held across a different key's work.
    gate: tokio::sync::Mutex<()>,
    value: Mutex<Option<Cached<V>>>,
}

struct Cached<V> {
    runtime: Option<Weak<OrbitRuntime>>,
    computed_at: Instant,
    ttl: Duration,
    body: Arc<V>,
}

impl<K: Eq + Hash, V: Send + Sync + 'static> RuntimeMemo<K, V> {
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
    ) -> Result<Served<V>, OrbitError>
    where
        F: FnOnce() -> Result<V, OrbitError> + Send + 'static,
    {
        self.compute(Some(runtime), key, ttl, compute).await
    }

    /// Host-wide projection, independent of the workspace selected by a tab.
    pub(crate) async fn get_or_compute_for_host<F>(
        &self,
        key: K,
        ttl: Duration,
        compute: F,
    ) -> Result<Served<V>, OrbitError>
    where
        F: FnOnce() -> Result<V, OrbitError> + Send + 'static,
    {
        self.compute(None, key, ttl, compute).await
    }

    async fn compute<F>(
        &self,
        runtime: Option<&Arc<OrbitRuntime>>,
        key: K,
        ttl: Duration,
        compute: F,
    ) -> Result<Served<V>, OrbitError>
    where
        F: FnOnce() -> Result<V, OrbitError> + Send + 'static,
    {
        let slot = self.slot((
            runtime.map_or(0, |runtime| Arc::as_ptr(runtime) as usize),
            key,
        ));
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
            runtime: runtime.map(Arc::downgrade),
            computed_at: Instant::now(),
            ttl,
            body: Arc::clone(&computed),
        });
        Ok(Served {
            body: computed,
            age: Duration::ZERO,
        })
    }

    fn slot(&self, key: (usize, K)) -> Arc<Slot<V>> {
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

impl<V> Slot<V> {
    fn hit(&self, runtime: Option<&Arc<OrbitRuntime>>) -> Option<Served<V>> {
        let guard = self.value.lock().unwrap_or_else(PoisonError::into_inner);
        let cached = guard.as_ref().filter(|cached| cached.is_fresh())?;
        let same_runtime = match (&cached.runtime, runtime) {
            (None, None) => true,
            (Some(cached), Some(runtime)) => cached
                .upgrade()
                .is_some_and(|live| Arc::ptr_eq(&live, runtime)),
            _ => false,
        };
        same_runtime.then(|| Served {
            body: Arc::clone(&cached.body),
            age: cached.computed_at.elapsed(),
        })
    }

    /// Drop empty, expired or orphaned entries once no caller owns the slot.
    /// An empty slot left by a failed compute is dropped too: host input keys
    /// change with every stamped write, so retaining them would grow the map
    /// without bound while a compute keeps failing.
    fn keep(&self) -> bool {
        let guard = self.value.lock().unwrap_or_else(PoisonError::into_inner);
        guard.as_ref().is_some_and(|cached| {
            cached.is_fresh()
                && cached
                    .runtime
                    .as_ref()
                    .is_none_or(|runtime| runtime.strong_count() > 0)
        })
    }
}

impl<V> Cached<V> {
    fn is_fresh(&self) -> bool {
        self.computed_at.elapsed() < self.ttl
    }
}
