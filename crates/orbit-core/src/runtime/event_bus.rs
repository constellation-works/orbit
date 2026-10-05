use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use orbit_types::record::OrbitEvent;

/// Maximum number of recent events retained by a session event log.
pub const SESSION_EVENT_CAPACITY: usize = 1024;

/// In-process, session-scoped event log retaining the newest
/// [`SESSION_EVENT_CAPACITY`] events. Older events are discarded on append.
///
/// Appended to during the lifetime of a single [`crate::OrbitRuntime`] instance and discarded
/// when the process exits. It is **not persisted** to any store. Agents and callers reading
/// historical audit data should query the SQLite-backed audit event store via
/// [`crate::OrbitRuntime::list_audit_events`] instead.
#[derive(Clone, Default)]
pub struct EventLog {
    state: Arc<Mutex<EventLogState>>,
}

#[derive(Default)]
struct EventLogState {
    events: VecDeque<(i64, OrbitEvent)>,
    last_id: i64,
}

impl EventLog {
    pub fn append(&self, event: OrbitEvent) {
        if let Ok(mut state) = self.state.lock() {
            if state.events.len() == SESSION_EVENT_CAPACITY {
                state.events.pop_front();
            }
            state.last_id = state.last_id.saturating_add(1);
            let id = state.last_id;
            state.events.push_back((id, event));
        }
    }

    /// Clone the retained events in append order, oldest first.
    pub fn snapshot(&self) -> Vec<OrbitEvent> {
        self.state
            .lock()
            .map(|state| {
                state
                    .events
                    .iter()
                    .map(|(_, event)| event.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Clone at most `limit` retained events, newest first, with their session IDs.
    pub(crate) fn recent(&self, limit: usize) -> Vec<(i64, OrbitEvent)> {
        self.state
            .lock()
            .map(|state| state.events.iter().rev().take(limit).cloned().collect())
            .unwrap_or_default()
    }
}
