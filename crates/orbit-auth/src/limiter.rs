//! A keyed, bounded, in-memory limit on failed authentication.
//!
//! Each client address gets a fixed window that starts at its first failure.
//! Once `limit` failures fall inside one window, every request from that
//! address is refused until the window ends, and only the first refusal of a
//! window is reported for audit, so a flood cannot fill the audit store.
//! IPv6 addresses are counted per /64, since one host commonly holds the
//! whole prefix. State is bounded by `capacity` keys: when full, ended
//! windows are dropped first, then the oldest. It lives in memory, so it
//! resets on restart and is not shared between processes.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// Whether a request from an address may proceed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailedAuthAdmission {
    /// Under the limit.
    Admit,
    /// Over the limit until the window ends.
    Limited {
        /// Time left in the window.
        retry_after: Duration,
        /// Whether this is the first refusal of the window, the one to audit.
        first_in_window: bool,
    },
}

#[derive(Debug, Clone, Copy)]
struct FailureWindow {
    started: Instant,
    failures: u32,
    refusal_reported: bool,
}

/// Failed-authentication limiter keyed by client address.
#[derive(Debug)]
pub struct FailedAuthLimiter {
    limit: u32,
    window: Duration,
    capacity: usize,
    windows: HashMap<IpAddr, FailureWindow>,
}

impl FailedAuthLimiter {
    /// Refuse an address after `limit` failures within `window`, tracking at
    /// most `capacity` addresses. `limit` and `capacity` are at least 1.
    pub fn new(limit: u32, window: Duration, capacity: usize) -> Self {
        Self {
            limit: limit.max(1),
            window,
            capacity: capacity.max(1),
            windows: HashMap::new(),
        }
    }

    /// Decide whether a request from `address` may proceed at `now`.
    pub fn check(&mut self, address: IpAddr, now: Instant) -> FailedAuthAdmission {
        let key = failed_auth_limiter_key(address);
        let window = self.window;
        let Some(entry) = self.windows.get_mut(&key) else {
            return FailedAuthAdmission::Admit;
        };
        let elapsed = now.saturating_duration_since(entry.started);
        if elapsed >= window {
            self.windows.remove(&key);
            return FailedAuthAdmission::Admit;
        }
        if entry.failures < self.limit {
            return FailedAuthAdmission::Admit;
        }
        let first_in_window = !entry.refusal_reported;
        entry.refusal_reported = true;
        FailedAuthAdmission::Limited {
            retry_after: window - elapsed,
            first_in_window,
        }
    }

    /// Count one failed authentication from `address` at `now`.
    pub fn record_failure(&mut self, address: IpAddr, now: Instant) {
        let key = failed_auth_limiter_key(address);
        if let Some(entry) = self.windows.get_mut(&key)
            && now.saturating_duration_since(entry.started) < self.window
        {
            entry.failures = entry.failures.saturating_add(1);
            return;
        }
        self.windows.remove(&key);
        if self.windows.len() >= self.capacity {
            let window = self.window;
            self.windows
                .retain(|_, entry| now.saturating_duration_since(entry.started) < window);
        }
        if self.windows.len() >= self.capacity
            && let Some(oldest) = self
                .windows
                .iter()
                .min_by_key(|(_, entry)| entry.started)
                .map(|(key, _)| *key)
        {
            self.windows.remove(&oldest);
        }
        self.windows.insert(
            key,
            FailureWindow {
                started: now,
                failures: 1,
                refusal_reported: false,
            },
        );
    }
}

/// The key an address is counted under: itself for IPv4, its /64 for IPv6.
fn failed_auth_limiter_key(address: IpAddr) -> IpAddr {
    match address.to_canonical() {
        IpAddr::V4(v4) => IpAddr::V4(v4),
        IpAddr::V6(v6) => IpAddr::V6((u128::from(v6) & !u128::from(u64::MAX)).into()),
    }
}
