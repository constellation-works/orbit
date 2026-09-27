//! Backend ownership for one host-dispatched agent invocation.
use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, PoisonError};

use super::McpBackend;

/// MCP children owned by one broker, separate even from another invocation
/// with identical workspace and tool policy. Dropping the pool reclaims them.
#[derive(Default)]
pub struct BrokerSessions {
    backends: Mutex<BTreeMap<usize, ScopedBackend>>,
}

struct ScopedBackend {
    // Retain the original so its address cannot be recycled as another key.
    _original: Arc<McpBackend>,
    backend: Arc<McpBackend>,
}

impl BrokerSessions {
    pub(crate) fn backend(&self, original: &Arc<McpBackend>) -> Arc<McpBackend> {
        let mut backends = self.backends.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(
            &backends
                .entry(Arc::as_ptr(original) as usize)
                .or_insert_with(|| ScopedBackend {
                    _original: Arc::clone(original),
                    backend: Arc::new(original.for_broker()),
                })
                .backend,
        )
    }
}

/// Host-owned call lifetime; neither component can be supplied on the wire.
#[derive(Clone)]
pub struct BrokerCall {
    pub sessions: Arc<BrokerSessions>,
    pub cancelled: Arc<AtomicBool>,
}
