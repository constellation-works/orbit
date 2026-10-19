//! Process-local single-flight TTL cache keyed by runtime identity or host inputs.
//!
//! Every visible dashboard tab polls the same endpoints. Keying by the live
//! runtime plus a request key collapses overlapping polls into one compute,
//! and a runtime rebuild naturally starts a fresh cache namespace. Dashboard
//! memos use it: `/api/audit/summary` (keyed by the raw `since` window, so relative
//! cutoffs such as `24h` still hit), audited plugin panel reads,
//! `/api/diagnostics/errors` (keyed by its time range and row limit), and
//! `/api/diagnostics/friction` (keyed by month and row limit), and
//! `/api/cost` (keyed by the canonical window), and the codebase
//! analytics scan behind `/api/codebase/analytics`, the orchestrator
//! scorecard scan behind `/api/orchestrators/*` and the agent scorecard scan
//! behind `/api/agents/*`, which cache typed facts rather than a response
//! body. The agent scan alone is served stale while one background refresh
//! replaces it. `/api/routines` uses a host namespace keyed by
//! definition, config and database stamps instead of a selected runtime. Every
//! payload served from a memo reports its age in an `Age` header.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use axum::http::{HeaderName, header};
use orbit_core::{OrbitError, OrbitRuntime};
use serde_json::Value;

/// Freshness bound for the dashboard memos the 30 s poll reads: audit summary,
/// cost, diagnostics errors and diagnostics friction. A viewer's polls
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

/// Freshness bound for the orchestrator scorecard scan. A miss reads every
/// attributed task's history and comments, and the scorecard moves over days,
/// so neither the 30 s poll nor a drill-down repeats it.
pub(crate) const ORCHESTRATOR_SCORECARD_TTL: Duration = Duration::from_secs(300);

/// Freshness bound for the agent scorecard scan. A miss reads the history
/// and comments of every task active in the last 90 days, and the rankings
/// move over days, so no role, window or detail change repeats it.
pub(crate) const AGENT_SCAN_TTL: Duration = Duration::from_secs(300);

/// How long past [`AGENT_SCAN_TTL`] a stale agent scan is still served while
/// one background refresh runs. Older than this, a request waits for a fresh
/// scan instead of reading hours-old rankings.
pub(crate) const AGENT_SCAN_STALE_LIMIT: Duration = Duration::from_secs(3600);

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
    /// Set while a background refresh of a stale entry runs, so stale reads
    /// start only one.
    refreshing: AtomicBool,
}

struct Cached<V> {
    runtime: Option<Weak<OrbitRuntime>>,
    computed_at: Instant,
    ttl: Duration,
    /// How long past `ttl` the entry may still be served stale.
    stale_for: Duration,
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

    /// Like [`Self::get_or_compute`], but an entry past `ttl` and within
    /// `stale_for` beyond it is served at once while one background refresh
    /// replaces it. A failed refresh keeps the stale entry, and the next stale
    /// read retries.
    pub(crate) async fn get_or_refresh<F>(
        &self,
        runtime: &Arc<OrbitRuntime>,
        key: K,
        ttl: Duration,
        stale_for: Duration,
        compute: F,
    ) -> Result<Served<V>, OrbitError>
    where
        F: FnOnce() -> Result<V, OrbitError> + Send + 'static,
    {
        let slot = self.slot((Arc::as_ptr(runtime) as usize, key));
        if let Some(hit) = slot.hit(Some(runtime)) {
            tracing::debug!(memo = self.label, "cache hit");
            return Ok(hit);
        }
        if let Some(stale) = slot.stale(runtime) {
            if !slot.refreshing.swap(true, Ordering::AcqRel) {
                tracing::debug!(memo = self.label, "stale hit; refreshing in background");
                let refresh = Arc::clone(&slot);
                let owner = Arc::downgrade(runtime);
                let label = self.label;
                tokio::spawn(async move {
                    let _gate = refresh.gate.lock().await;
                    match tokio::task::spawn_blocking(compute).await {
                        Ok(Ok(value)) => {
                            refresh.store(Some(owner), ttl, stale_for, Arc::new(value));
                        }
                        Ok(Err(error)) => {
                            tracing::warn!(memo = label, %error, "background refresh failed");
                        }
                        Err(join_err) => {
                            tracing::warn!(memo = label, %join_err, "background refresh panicked");
                        }
                    }
                    refresh.refreshing.store(false, Ordering::Release);
                });
            }
            return Ok(stale);
        }
        self.compute_in(&slot, Some(runtime), ttl, stale_for, compute)
            .await
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
        self.compute_in(&slot, runtime, ttl, Duration::ZERO, compute)
            .await
    }

    /// Compute into `slot` once while concurrent callers wait on its gate.
    async fn compute_in<F>(
        &self,
        slot: &Slot<V>,
        runtime: Option<&Arc<OrbitRuntime>>,
        ttl: Duration,
        stale_for: Duration,
        compute: F,
    ) -> Result<Served<V>, OrbitError>
    where
        F: FnOnce() -> Result<V, OrbitError> + Send + 'static,
    {
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
        slot.store(
            runtime.map(Arc::downgrade),
            ttl,
            stale_for,
            Arc::clone(&computed),
        );
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
                refreshing: AtomicBool::new(false),
            })
        }))
    }
}

impl<V> Slot<V> {
    fn hit(&self, runtime: Option<&Arc<OrbitRuntime>>) -> Option<Served<V>> {
        self.served(runtime, Cached::is_fresh)
    }

    /// An entry past its freshness bound that may still be served stale.
    fn stale(&self, runtime: &Arc<OrbitRuntime>) -> Option<Served<V>> {
        self.served(Some(runtime), Cached::is_servable)
    }

    fn served(
        &self,
        runtime: Option<&Arc<OrbitRuntime>>,
        usable: fn(&Cached<V>) -> bool,
    ) -> Option<Served<V>> {
        let guard = self.value.lock().unwrap_or_else(PoisonError::into_inner);
        let cached = guard.as_ref().filter(|cached| usable(cached))?;
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

    fn store(
        &self,
        runtime: Option<Weak<OrbitRuntime>>,
        ttl: Duration,
        stale_for: Duration,
        body: Arc<V>,
    ) {
        *self.value.lock().unwrap_or_else(PoisonError::into_inner) = Some(Cached {
            runtime,
            computed_at: Instant::now(),
            ttl,
            stale_for,
            body,
        });
    }

    /// Drop empty, expired or orphaned entries once no caller owns the slot.
    /// An empty slot left by a failed compute is dropped too: host input keys
    /// change with every stamped write, so retaining them would grow the map
    /// without bound while a compute keeps failing.
    fn keep(&self) -> bool {
        let guard = self.value.lock().unwrap_or_else(PoisonError::into_inner);
        guard.as_ref().is_some_and(|cached| {
            cached.is_servable()
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

    /// Fresh, or stale within the period it may still be served.
    fn is_servable(&self) -> bool {
        self.computed_at.elapsed() < self.ttl + self.stale_for
    }
}
