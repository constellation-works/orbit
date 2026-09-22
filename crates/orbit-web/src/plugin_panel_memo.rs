//! Process-local single-flight TTL cache for plugin dashboard panels.
//!
//! Every visible dashboard tab polls the same panels. Keying by runtime
//! identity plus panel collapses overlapping tabs into one audited tool call;
//! a runtime rebuild naturally starts a fresh cache namespace.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use orbit_core::{OrbitError, OrbitRuntime};
use serde_json::Value;

pub(crate) struct PluginPanelMemo {
    slots: Mutex<HashMap<MemoKey, Arc<Slot>>>,
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct MemoKey {
    runtime_ptr: usize,
    namespace: String,
    panel: String,
}

struct Slot {
    gate: tokio::sync::Mutex<()>,
    value: Mutex<Option<CachedPanel>>,
}

struct CachedPanel {
    runtime: Weak<OrbitRuntime>,
    computed_at: Instant,
    ttl: Duration,
    body: Arc<Value>,
}

impl PluginPanelMemo {
    pub(crate) fn new() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
        }
    }

    /// Return a fresh panel response, or run one computation while concurrent
    /// callers for the same panel wait. Failed computations are not cached.
    pub(crate) async fn get_or_compute<F>(
        &self,
        runtime: &Arc<OrbitRuntime>,
        namespace: &str,
        panel: &str,
        ttl: Duration,
        compute: F,
    ) -> Result<Arc<Value>, OrbitError>
    where
        F: FnOnce() -> Result<Value, OrbitError> + Send + 'static,
    {
        let key = MemoKey {
            runtime_ptr: Arc::as_ptr(runtime) as usize,
            namespace: namespace.to_string(),
            panel: panel.to_string(),
        };
        let slot = self.slot(key);
        if let Some(hit) = slot.hit(runtime) {
            return Ok(hit);
        }

        let _gate = slot.gate.lock().await;
        if let Some(hit) = slot.hit(runtime) {
            return Ok(hit);
        }

        let computed = match tokio::task::spawn_blocking(compute).await {
            Ok(Ok(value)) => Arc::new(value),
            Ok(Err(error)) => return Err(error),
            Err(join_err) => {
                return Err(OrbitError::Execution(format!(
                    "plugin panel execution panicked: {join_err}"
                )));
            }
        };
        *slot.value.lock().unwrap_or_else(PoisonError::into_inner) = Some(CachedPanel {
            runtime: Arc::downgrade(runtime),
            computed_at: Instant::now(),
            ttl,
            body: Arc::clone(&computed),
        });
        Ok(computed)
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
        if cached.computed_at.elapsed() >= cached.ttl {
            return None;
        }
        let live = cached.runtime.upgrade()?;
        Arc::ptr_eq(&live, runtime).then(|| Arc::clone(&cached.body))
    }

    fn keep(&self) -> bool {
        let guard = self.value.lock().unwrap_or_else(PoisonError::into_inner);
        match guard.as_ref() {
            None => true,
            Some(cached) => {
                cached.computed_at.elapsed() < cached.ttl && cached.runtime.strong_count() > 0
            }
        }
    }
}
