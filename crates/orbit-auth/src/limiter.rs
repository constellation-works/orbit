//! Bounded, transport-independent request budgets and coalesced rejection audits.
//!
//! Active windows are never evicted. At capacity new addresses share one
//! conservative overflow key; authenticated tokens have a separate overflow key.
//! Idle entries are reclaimed during periodic audit maintenance, not on every
//! new address. Callers supply resolved addresses and a monotonic clock; this
//! module never reads headers, tokens, sockets or the Orbit store.

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

/// A caller's non-secret rate-limit identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LimitKey {
    /// Address already resolved by the client-address policy.
    Address(IpAddr),
    /// Public token identifier, never a secret or digest.
    Token(String),
    /// Reserved shared key for new unauthenticated callers at capacity.
    AnonymousOverflow,
    /// Separate conservative budget for new authenticated tokens at capacity.
    TokenOverflow,
}

impl LimitKey {
    fn audit_key(&self) -> String {
        match self {
            Self::Address(ip) => format!("address:{ip}"),
            Self::Token(id) => format!("token:{id}"),
            Self::AnonymousOverflow => "address:overflow".into(),
            Self::TokenOverflow => "token:overflow".into(),
        }
    }
}

/// One aggregated rejection row. The store upserts snapshots of the same row.
#[derive(Debug, Clone)]
pub struct LimitAudit {
    /// Canonical public key (no token secret or principal).
    pub key: String,
    /// Stable limit or refusal name.
    pub limit: &'static str,
    /// First rejection, also the durable window identifier.
    pub first_at: DateTime<Utc>,
    /// Most recent rejection.
    pub last_at: DateTime<Utc>,
    /// Number of rejections in this window.
    pub count: u64,
}

struct AuditWindow {
    started: Instant,
    period: Duration,
    row: LimitAudit,
    dirty: bool,
}

struct Budget {
    started: Instant,
    period: Duration,
    used: u32,
}

struct KeyState {
    budgets: BTreeMap<&'static str, Budget>,
    audits: BTreeMap<&'static str, AuditWindow>,
    in_flight: Arc<AtomicUsize>,
    streams: Arc<AtomicUsize>,
}

impl Default for KeyState {
    fn default() -> Self {
        Self {
            budgets: BTreeMap::new(),
            audits: BTreeMap::new(),
            in_flight: Arc::new(AtomicUsize::new(0)),
            streams: Arc::new(AtomicUsize::new(0)),
        }
    }
}

/// A token allowance retained until its response body (including SSE) closes.
#[derive(Debug)]
pub struct TokenAllowance {
    in_flight: Arc<AtomicUsize>,
    streams: Option<Arc<AtomicUsize>>,
}

impl Drop for TokenAllowance {
    fn drop(&mut self) {
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
        if let Some(streams) = &self.streams {
            streams.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// Keyed fixed-window limiter. All state, including pending audits, is bounded.
pub struct KeyedLimiter {
    capacity: usize,
    ipv6_prefix: u8,
    keys: HashMap<LimitKey, KeyState>,
    completed: Vec<LimitAudit>,
    last_flush: Instant,
    urgent_flush: bool,
}

// Names are supplied by trusted surfaces, but cap slots even for a buggy caller.
const MAX_SLOTS_PER_KEY: usize = 16;

impl KeyedLimiter {
    /// Track at most `capacity` keys, including two reserved overflow buckets.
    pub fn new(capacity: usize, ipv6_prefix: u8, now: Instant) -> Self {
        let mut keys = HashMap::new();
        keys.insert(LimitKey::AnonymousOverflow, KeyState::default());
        keys.insert(LimitKey::TokenOverflow, KeyState::default());
        Self {
            capacity: capacity.max(3),
            ipv6_prefix: ipv6_prefix.min(128),
            keys,
            completed: Vec::new(),
            last_flush: now,
            urgent_flush: false,
        }
    }

    /// Canonicalize IPv4-mapped IPv6 and mask IPv6 to the configured prefix.
    pub fn address_key(&self, address: IpAddr) -> LimitKey {
        LimitKey::Address(match address.to_canonical() {
            IpAddr::V4(v4) => IpAddr::V4(v4),
            IpAddr::V6(v6) => {
                let mask = u128::MAX
                    .checked_shl(u32::from(128 - self.ipv6_prefix))
                    .unwrap_or(0);
                IpAddr::V6((u128::from(v6) & mask).into())
            }
        })
    }

    /// Return the tracked identity, or a shared bucket without evicting live keys.
    pub fn bounded_key(&mut self, key: LimitKey) -> LimitKey {
        if self.keys.contains_key(&key) {
            return key;
        }
        if self.keys.len() < self.capacity {
            self.keys.insert(key.clone(), KeyState::default());
            key
        } else {
            match key {
                LimitKey::Token(_) | LimitKey::TokenOverflow => LimitKey::TokenOverflow,
                _ => LimitKey::AnonymousOverflow,
            }
        }
    }

    /// Count a request (`consume = true`) or check a failure budget without
    /// incrementing it. Refusal returns the remaining window for Retry-After.
    pub fn check(
        &mut self,
        key: &LimitKey,
        limit: &'static str,
        quota: u32,
        period: Duration,
        now: Instant,
        consume: bool,
    ) -> Result<(), Duration> {
        let key = self.bounded_key(key.clone());
        let state = self.keys.entry(key).or_default();
        if !consume
            && state
                .budgets
                .get(limit)
                .is_none_or(|b| now.saturating_duration_since(b.started) >= b.period)
        {
            state.budgets.remove(limit);
            return Ok(());
        }
        if !state.budgets.contains_key(limit) && state.budgets.len() >= MAX_SLOTS_PER_KEY {
            return Err(period);
        }
        let budget = state.budgets.entry(limit).or_insert(Budget {
            started: now,
            period,
            used: 0,
        });
        if now.saturating_duration_since(budget.started) >= budget.period {
            *budget = Budget {
                started: now,
                period,
                used: 0,
            };
        }
        if budget.used >= quota {
            return Err(budget
                .period
                .saturating_sub(now.saturating_duration_since(budget.started)));
        }
        if consume {
            budget.used = budget.used.saturating_add(1);
        }
        Ok(())
    }

    /// Reserve token concurrency and, for a stream, its separate stream cap.
    /// The returned guard releases both counters on cancellation or body close.
    pub fn token_allowance(
        &mut self,
        key: &LimitKey,
        max_in_flight: usize,
        max_streams: usize,
        stream: bool,
    ) -> Result<TokenAllowance, &'static str> {
        let key = self.bounded_key(key.clone());
        let state = self.keys.entry(key).or_default();
        let acquire = |counter: &AtomicUsize, max| {
            counter
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                    (n < max).then(|| n + 1)
                })
                .is_ok()
        };
        if !acquire(&state.in_flight, max_in_flight) {
            return Err("token_in_flight");
        }
        if stream && !acquire(&state.streams, max_streams) {
            state.in_flight.fetch_sub(1, Ordering::Relaxed);
            return Err("token_streams");
        }
        Ok(TokenAllowance {
            in_flight: Arc::clone(&state.in_flight),
            streams: stream.then(|| Arc::clone(&state.streams)),
        })
    }

    /// Coalesce a refusal without performing I/O or allocating per request.
    pub fn record_rejection(
        &mut self,
        key: &LimitKey,
        limit: &'static str,
        period: Duration,
        now: Instant,
        at: DateTime<Utc>,
    ) {
        let key = self.bounded_key(key.clone());
        let audit_key = key.audit_key();
        let state = self.keys.entry(key).or_default();
        let limit =
            if state.audits.contains_key(limit) || state.audits.len() < MAX_SLOTS_PER_KEY - 1 {
                limit
            } else {
                "other_refusal"
            };
        let budget = state.budgets.get(limit).or_else(|| {
            matches!(
                limit,
                "unknown"
                    | "revoked"
                    | "expired"
                    | "wrong_workspace"
                    | "verb_not_granted"
                    | "missing_token"
            )
            .then(|| state.budgets.get("failed_auth"))
            .flatten()
        });
        let started = budget.map_or_else(
            || {
                state.audits.get(limit).map_or(now, |a| {
                    if now.saturating_duration_since(a.started) < a.period {
                        a.started
                    } else {
                        now
                    }
                })
            },
            |b| b.started,
        );
        if state
            .audits
            .get(limit)
            .is_some_and(|a| a.started != started)
            && self.completed.len() < self.capacity * MAX_SLOTS_PER_KEY
            && let Some(old) = state.audits.remove(limit)
            && old.dirty
        {
            self.completed.push(old.row);
        }
        let audit = state.audits.entry(limit).or_insert_with(|| {
            self.urgent_flush = true;
            AuditWindow {
                started,
                period,
                row: LimitAudit {
                    key: audit_key,
                    limit,
                    first_at: at,
                    last_at: at,
                    count: 0,
                },
                dirty: false,
            }
        });
        audit.row.count = audit.row.count.saturating_add(1);
        audit.row.last_at = at;
        audit.dirty = true;
    }

    /// Flush at most once a second, plus each newly observed rejection window.
    /// Call periodically and at shutdown; batch persistence happens in the surface.
    pub fn drain_audits(&mut self, now: Instant, force: bool) -> Vec<LimitAudit> {
        if !force
            && !self.urgent_flush
            && now.saturating_duration_since(self.last_flush) < Duration::from_secs(1)
        {
            return Vec::new();
        }
        self.last_flush = now;
        self.urgent_flush = false;
        let mut rows = std::mem::take(&mut self.completed);
        for state in self.keys.values_mut() {
            for audit in state.audits.values_mut().filter(|a| a.dirty) {
                rows.push(audit.row.clone());
                audit.dirty = false;
            }
        }
        self.keys.retain(|key, state| {
            matches!(key, LimitKey::AnonymousOverflow | LimitKey::TokenOverflow)
                || state.in_flight.load(Ordering::Relaxed) > 0
                || state
                    .budgets
                    .values()
                    .any(|b| now.saturating_duration_since(b.started) < b.period)
                || state
                    .audits
                    .values()
                    .any(|a| now.saturating_duration_since(a.started) < a.period)
        });
        rows
    }

    /// Retain a failed persistence batch for the next periodic flush, within
    /// the same fixed bound as rollover snapshots.
    pub fn retry_audits(&mut self, rows: Vec<LimitAudit>) {
        let remaining = (self.capacity * MAX_SLOTS_PER_KEY).saturating_sub(self.completed.len());
        self.completed.extend(rows.into_iter().take(remaining));
    }

    /// Tracked identities, including overflow buckets, for bounded-state evidence.
    pub fn tracked_keys(&self) -> usize {
        self.keys.len()
    }
}
