//! Remote access through trusted hosts: the policy the Host gate applies, and
//! what it saw since this server started (Observe › Fleet › Remote access).
//!
//! Everything here is in memory and resets on restart. Only requests whose
//! Host is a trusted host are recorded; the loopback path never reaches the
//! tracker. Memory is bounded: at most [`MAX_DEVICES`] devices and
//! [`MAX_REFUSAL_KEYS`] refusal keys, each with fixed-size time buckets.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::TrustedHosts;

/// Devices kept in the connections table; the least recently seen goes first.
const MAX_DEVICES: usize = 64;
/// Distinct (reason, attempted login) refusal keys kept; further unlisted
/// logins are counted without the login.
const MAX_REFUSAL_KEYS: usize = 64;
/// How long a device stays listed after its last request.
const DEVICE_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
/// The "active" window: requests per device and active devices.
pub(crate) const ACTIVE_WINDOW_MINUTES: i64 = 15;
/// The refusal window.
pub(crate) const REFUSAL_WINDOW_HOURS: i64 = 24;
/// Longest attempted login kept with an unlisted-login refusal.
const MAX_LOGIN_CHARS: usize = 128;

/// Why a request on the trusted-host path was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RefusalReason {
    /// No `Tailscale-User-Login`: Funnel, a tagged device, or not via Serve.
    NoIdentity,
    /// More than one `Tailscale-User-Login`, or one that is not text.
    InvalidIdentity,
    /// A login not in `machine.web.trusted_logins`.
    UnlistedLogin,
    /// `Origin` missing on an unsafe method, repeated, or not `https://<host>`.
    OriginMismatch,
    /// A browser `Sec-Fetch-Site` other than `same-origin` or `none`.
    SecFetchSite,
}

/// Marks a request the router dispatches to itself (the host forward's
/// local answer), so the gate does not record it a second time.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AlreadyRecorded;

/// The trusted-host policy and the remote-access activity seen under it.
#[derive(Debug, Default)]
pub(crate) struct RemoteAccess {
    policy: TrustedHosts,
    /// Whether the policy came from `machine.web.*` at serve start (false for
    /// the embedded entry point, which admits loopback only).
    from_config: bool,
    /// The bound port and when serving began, set once after the bind.
    bound: OnceLock<(u16, DateTime<Utc>)>,
    activity: Mutex<Activity>,
    tailscale: Mutex<TailscaleCache>,
}

impl RemoteAccess {
    /// `orbit web serve`'s policy, read from `machine.web.*` at start.
    pub(crate) fn from_config(policy: TrustedHosts) -> Self {
        Self {
            policy,
            from_config: true,
            ..Self::default()
        }
    }

    pub(crate) fn trusted_hosts(&self) -> &TrustedHosts {
        &self.policy
    }

    pub(crate) fn is_from_config(&self) -> bool {
        self.from_config
    }

    /// Record the port the server bound, once, after the bind; tracking
    /// counts from then.
    pub(crate) fn set_bound_port(&self, port: u16) {
        let _ = self.bound.set((port, Utc::now()));
    }

    pub(crate) fn bound_port(&self) -> Option<u16> {
        self.bound.get().map(|(port, _)| *port)
    }

    /// When this server started tracking, if it has bound.
    pub(crate) fn tracking_since(&self) -> Option<DateTime<Utc>> {
        self.bound.get().map(|(_, at)| *at)
    }

    fn activity(&self) -> std::sync::MutexGuard<'_, Activity> {
        self.activity.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Count one admitted trusted-host request from `device` as `login`.
    pub(crate) fn record_admitted(&self, device: Option<IpAddr>, login: &str) {
        self.activity().admit(device_key(device), login, Utc::now());
    }

    /// Count one open live stream for `device` until the guard drops.
    pub(crate) fn open_stream(self: &Arc<Self>, device: Option<IpAddr>) -> StreamGuard {
        let key = device_key(device);
        if let Some(entry) = self.activity().devices.get_mut(&key) {
            entry.open_streams += 1;
        }
        StreamGuard {
            access: Arc::clone(self),
            device: key,
        }
    }

    /// Count one refused trusted-host request. `login` is kept only for an
    /// unlisted login, truncated and stripped of control characters.
    pub(crate) fn record_refused(&self, reason: RefusalReason, login: Option<&str>) {
        let login = (reason == RefusalReason::UnlistedLogin)
            .then_some(login)
            .flatten()
            .map(|login| {
                login
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(MAX_LOGIN_CHARS)
                    .collect::<String>()
            });
        self.activity().refuse(reason, login, Utc::now());
    }

    /// The tracker's current view, pruned to its windows.
    pub(crate) fn snapshot(&self) -> ActivitySnapshot {
        self.activity().snapshot(Utc::now())
    }

    /// A cached `tailscale serve status` reading younger than `ttl`.
    pub(crate) fn cached_serve_status(&self, ttl: Duration) -> Option<TailscaleReading> {
        let cache = self
            .tailscale
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        cache
            .serve_status
            .as_ref()
            .filter(|(at, _)| at.elapsed() < ttl)
            .map(|(_, reading)| reading.clone())
    }

    pub(crate) fn store_serve_status(&self, reading: TailscaleReading) {
        let mut cache = self
            .tailscale
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        cache.serve_status = Some((Instant::now(), reading));
    }

    /// A cached `tailscale whois` reading for `ip` younger than `ttl`.
    pub(crate) fn cached_whois(&self, ip: IpAddr, ttl: Duration) -> Option<TailscaleReading> {
        let cache = self
            .tailscale
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        cache
            .whois
            .get(&ip)
            .filter(|(at, _)| at.elapsed() < ttl)
            .map(|(_, reading)| reading.clone())
    }

    pub(crate) fn store_whois(&self, ip: IpAddr, reading: TailscaleReading) {
        let mut cache = self
            .tailscale
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if cache.whois.len() >= MAX_DEVICES && !cache.whois.contains_key(&ip) {
            cache.whois.clear();
        }
        cache.whois.insert(ip, (Instant::now(), reading));
    }
}

/// One `tailscale` CLI reading: its JSON output, or why there is none.
#[derive(Debug, Clone)]
pub(crate) enum TailscaleReading {
    Json(serde_json::Value),
    /// The `tailscale` binary is not on `PATH`.
    Missing,
    /// It ran and failed, timed out or printed something unreadable.
    Error(String),
}

#[derive(Debug, Default)]
struct TailscaleCache {
    serve_status: Option<(Instant, TailscaleReading)>,
    whois: HashMap<IpAddr, (Instant, TailscaleReading)>,
}

/// Decrements a device's open live streams when the response body ends.
pub(crate) struct StreamGuard {
    access: Arc<RemoteAccess>,
    device: String,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        if let Some(entry) = self.access.activity().devices.get_mut(&self.device) {
            entry.open_streams = entry.open_streams.saturating_sub(1);
        }
    }
}

/// The connections table key: the device's tailnet IP, or `unknown` when
/// Serve's `X-Forwarded-For` is absent or unreadable.
fn device_key(device: Option<IpAddr>) -> String {
    device.map_or_else(|| "unknown".to_string(), |ip| ip.to_string())
}

#[derive(Debug, Default)]
struct Activity {
    devices: HashMap<String, Device>,
    refusals: HashMap<(RefusalReason, Option<String>), Refusals>,
}

#[derive(Debug)]
struct Device {
    login: String,
    open_streams: u32,
    /// (minute since epoch, requests) for the last [`ACTIVE_WINDOW_MINUTES`].
    minutes: VecDeque<(i64, u64)>,
    last_seen: DateTime<Utc>,
}

#[derive(Debug)]
struct Refusals {
    /// (hour since epoch, refusals) for the last [`REFUSAL_WINDOW_HOURS`].
    hours: VecDeque<(i64, u64)>,
    last_at: DateTime<Utc>,
}

/// Add one to the newest bucket and drop buckets older than `window`.
fn bump(buckets: &mut VecDeque<(i64, u64)>, slot: i64, window: i64) {
    match buckets.back_mut() {
        Some((last, count)) if *last == slot => *count += 1,
        _ => buckets.push_back((slot, 1)),
    }
    prune(buckets, slot, window);
}

fn prune(buckets: &mut VecDeque<(i64, u64)>, slot: i64, window: i64) {
    while buckets.front().is_some_and(|(at, _)| *at <= slot - window) {
        buckets.pop_front();
    }
}

fn total(buckets: &VecDeque<(i64, u64)>) -> u64 {
    buckets.iter().map(|(_, count)| count).sum()
}

impl Activity {
    fn admit(&mut self, key: String, login: &str, now: DateTime<Utc>) {
        if !self.devices.contains_key(&key) && self.devices.len() >= MAX_DEVICES {
            // Never evict a device with an open stream: its guard still counts it.
            let oldest = self
                .devices
                .iter()
                .filter(|(_, device)| device.open_streams == 0)
                .min_by_key(|(_, device)| device.last_seen)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                self.devices.remove(&oldest);
            }
        }
        let device = self.devices.entry(key).or_insert_with(|| Device {
            login: login.to_string(),
            open_streams: 0,
            minutes: VecDeque::new(),
            last_seen: now,
        });
        login.clone_into(&mut device.login);
        device.last_seen = now;
        bump(
            &mut device.minutes,
            now.timestamp() / 60,
            ACTIVE_WINDOW_MINUTES,
        );
    }

    fn refuse(&mut self, reason: RefusalReason, login: Option<String>, now: DateTime<Utc>) {
        let mut key = (reason, login);
        if !self.refusals.contains_key(&key) && self.refusals.len() >= MAX_REFUSAL_KEYS {
            key.1 = None;
        }
        let entry = self.refusals.entry(key).or_insert_with(|| Refusals {
            hours: VecDeque::new(),
            last_at: now,
        });
        entry.last_at = now;
        bump(
            &mut entry.hours,
            now.timestamp() / 3600,
            REFUSAL_WINDOW_HOURS,
        );
    }

    fn snapshot(&mut self, now: DateTime<Utc>) -> ActivitySnapshot {
        let minute = now.timestamp() / 60;
        let hour = now.timestamp() / 3600;
        let retention = chrono::Duration::from_std(DEVICE_RETENTION).unwrap_or_default();
        self.devices
            .retain(|_, device| device.open_streams > 0 || now - device.last_seen < retention);
        let mut devices: Vec<DeviceSnapshot> = self
            .devices
            .iter_mut()
            .map(|(key, device)| {
                prune(&mut device.minutes, minute, ACTIVE_WINDOW_MINUTES);
                DeviceSnapshot {
                    ip: (key != "unknown").then(|| key.clone()),
                    login: device.login.clone(),
                    open_streams: device.open_streams,
                    requests_15m: total(&device.minutes),
                    last_seen: device.last_seen,
                }
            })
            .collect();
        devices.sort_by_key(|device| std::cmp::Reverse(device.last_seen));
        for entry in self.refusals.values_mut() {
            prune(&mut entry.hours, hour, REFUSAL_WINDOW_HOURS);
        }
        self.refusals.retain(|_, entry| !entry.hours.is_empty());
        let mut refusals: Vec<RefusalSnapshot> = self
            .refusals
            .iter()
            .map(|((reason, login), entry)| RefusalSnapshot {
                reason: *reason,
                login: login.clone(),
                count_24h: total(&entry.hours),
                last_at: entry.last_at,
            })
            .collect();
        refusals.sort_by(|a, b| (a.reason, &a.login).cmp(&(b.reason, &b.login)));
        ActivitySnapshot { devices, refusals }
    }
}

/// Devices and refusals as the endpoint reports them.
#[derive(Debug, Serialize)]
pub(crate) struct ActivitySnapshot {
    pub(crate) devices: Vec<DeviceSnapshot>,
    pub(crate) refusals: Vec<RefusalSnapshot>,
}

#[derive(Debug, Serialize)]
pub(crate) struct DeviceSnapshot {
    /// The tailnet IP Serve forwarded for, or null when it sent none.
    pub(crate) ip: Option<String>,
    pub(crate) login: String,
    pub(crate) open_streams: u32,
    pub(crate) requests_15m: u64,
    pub(crate) last_seen: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RefusalSnapshot {
    pub(crate) reason: RefusalReason,
    /// The attempted login, for an unlisted login only.
    pub(crate) login: Option<String>,
    pub(crate) count_24h: u64,
    pub(crate) last_at: DateTime<Utc>,
}
