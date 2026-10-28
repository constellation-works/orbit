//! Bounded lease answers: the response budget a lease token's transport
//! imposes, and the JSON a lease is answered as.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{
    LeaseQueueKind, LeaseRecord, TaskLockConflict, TaskLockHolder, TaskReservationReleaseReason,
};
use serde::Serialize;
use serde_json::{Value, json};

/// Room kept free in a token answer's size estimate for what the estimate
/// cannot know before the write: the response envelope, the final lease id
/// and timestamps, and a queue position.
const TOKEN_RESPONSE_MARGIN_BYTES: usize = 512;

/// A timestamp at least as long as any the store writes, for sizing answers.
pub(super) const PLACEHOLDER_TIMESTAMP: &str = "0000-00-00T00:00:00.000000000+00:00";

/// What remains of a lease-token answer's size limit for its holder lists
/// (`overlaps`, `conflicts`, `queued_ahead`), measured as serialized JSON.
/// The lists are built only to a prefix that fits, and `truncated` says so;
/// every holder still decides grants and queueing and gets its notice. SSH
/// and dashboard callers are unbounded.
pub(super) struct ResponseBudget {
    remaining: Option<usize>,
    pub(super) truncated: bool,
}

impl ResponseBudget {
    pub(super) fn unbounded() -> Self {
        Self {
            remaining: None,
            truncated: false,
        }
    }

    /// The budget left by `fixed`, the largest answer the operation can give
    /// with empty holder lists. When that alone cannot fit `max_bytes` the
    /// operation is refused here, before it writes anything.
    pub(super) fn for_token(max_bytes: usize, fixed: &Value) -> Result<Self, OrbitError> {
        let needed = serialized_len(fixed) + TOKEN_RESPONSE_MARGIN_BYTES;
        if needed > max_bytes {
            return Err(OrbitError::InvalidInput(format!(
                "the answer to this lease operation would need about {needed} bytes, over the \
                 {max_bytes}-byte limit of the lease token transport; use fewer or shorter \
                 selectors, tags or branch names"
            )));
        }
        Ok(Self {
            remaining: Some(max_bytes - needed),
            truncated: false,
        })
    }

    /// Whether `item` still fits; once one does not, nothing more does, so
    /// every list stays a prefix.
    pub(super) fn admit(&mut self, item: &impl Serialize) -> bool {
        let Some(remaining) = self.remaining.as_mut() else {
            return true;
        };
        // One more byte for the separating comma.
        let size = serialized_len(item) + 1;
        if self.truncated || size > *remaining {
            self.truncated = true;
            return false;
        }
        *remaining -= size;
        true
    }

    /// The bytes left for the store to fill `conflicts` and `queued_ahead`
    /// with; `None` when unbounded.
    pub(super) fn holder_list_bytes(&self) -> Option<usize> {
        self.remaining
            .map(|remaining| if self.truncated { 0 } else { remaining })
    }

    /// The most conflicts the rest of the budget could show, plus one bounded
    /// witness for omitted conflicts; `None` when unbounded.
    pub(super) fn conflicts_shown(&self) -> Option<usize> {
        let smallest = serialized_len(&TaskLockConflict {
            file: String::new(),
            held_by: TaskLockHolder::Task,
            held_by_id: String::new(),
        }) + 1;
        self.holder_list_bytes().map(|bytes| bytes / smallest + 1)
    }

    pub(super) fn take<T: Serialize>(&mut self, items: Vec<T>) -> Vec<T> {
        items
            .into_iter()
            .take_while(|item| self.admit(item))
            .collect()
    }
}

fn serialized_len(value: &impl Serialize) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX / 2, |bytes| bytes.len())
}

pub(super) fn lease_active(lease: &LeaseRecord) -> bool {
    lease.released_at.is_none()
        && DateTime::parse_from_rfc3339(&lease.expires_at)
            .is_ok_and(|expires_at| expires_at.with_timezone(&Utc) > Utc::now())
}

/// A lease's fields plus its derived `state`: `queued` while a request
/// waits, `granted` while it holds its selectors (an expansion may still
/// wait in `queued_selectors`), then `released` or `expired`.
pub(super) fn lease_json(lease: &LeaseRecord) -> Value {
    let state = match (&lease.released_at, lease.release_reason) {
        (None, _) if lease_active(lease) && lease.queue_kind == Some(LeaseQueueKind::Request) => {
            "queued"
        }
        (None, _) if lease_active(lease) => "granted",
        (None, _) | (Some(_), Some(TaskReservationReleaseReason::TtlExpired)) => "expired",
        (Some(_), _) => "released",
    };
    let mut value = json!(lease);
    value["state"] = json!(state);
    value
}
