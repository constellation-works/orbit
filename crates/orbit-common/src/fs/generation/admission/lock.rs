//! Shared/exclusive admission, pending switches, and upgrade probes.

use std::path::Path;
use std::time::{Duration, Instant};

use chrono::Utc;
use fs2::FileExt;

use super::super::identity::CompatibilityIdentity;
use super::super::paths::{ADMISSION_LOCK, GENERATION_LOCK};
use super::super::records::{Record, open};
use super::super::refusal::{
    SWITCH_PENDING, contended, refusal, switch_pending, upgrade_holds_admission, upgrade_pending,
};
use super::super::registry::pending_switch;
use super::super::{DEFAULT_QUIESCE_TIMEOUT, MAX_QUIESCE_TIMEOUT, QUIESCE_TIMEOUT_ENV};
use super::guard::Participant;
use super::waiters::{self as admission_waiters, ExclusiveWaiter};
use crate::OrbitError;

pub(super) const QUIESCE_POLL: Duration = Duration::from_millis(200);
const ADMISSION_POLL: Duration = Duration::from_millis(10);
/// How long admission may stay unavailable before a wait asks whether an
/// upgrade holds it. Ordinary holders release it within milliseconds.
const UPGRADE_PROBE: Duration = Duration::from_secs(1);
/// How long an [`upgrade_holding`] probe can hold the generation lock.
pub(in super::super) const PROBE_SETTLE: Duration = Duration::from_millis(20);
/// How long an exclusive generation hold must last before a waiting joiner
/// reads it as an upgrade. A pin, reseed or takeover holds it exclusively
/// only while it rewrites the record; a process resumed by a handover waits
/// behind exactly such a pin and must not mistake it for an update.
const EXCLUSIVE_SETTLE: Duration = Duration::from_millis(250);

/// The configured wait for a breaking upgrade, and for an updater waiting
/// on short-lived participants (see [`QUIESCE_TIMEOUT_ENV`]), at most
/// [`MAX_QUIESCE_TIMEOUT`].
pub fn quiesce_bound() -> Duration {
    std::env::var(QUIESCE_TIMEOUT_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map_or(DEFAULT_QUIESCE_TIMEOUT, Duration::from_secs)
        .min(MAX_QUIESCE_TIMEOUT)
}

/// How a process holds `.generation-admission.lock`.
///
/// Ordinary joins that write nothing shared hold it shared, so they never
/// wait for one another. Anything that rewrites the generation or compat
/// record, probes the generation lock exclusively, or records a pending
/// switch holds it exclusively, which waits for every in-flight join.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in super::super) enum Admission {
    Shared,
    Exclusive,
}

/// Take admission in `mode`, waiting up to `wait` while other startups hold
/// it.
///
/// Once admission has stayed unavailable past [`UPGRADE_PROBE`], the wait
/// asks why at that interval: a pending switch not targeting `joining`, or an
/// update holding the generation exclusively, refuses at once rather than
/// queueing behind the upgrade.
/// Exclusive waiters publish locked intent records before polling, preventing
/// a stream of shared admissions from overtaking a widening or reseeding join.
pub(in super::super) fn admission(
    root: &Path,
    mode: Admission,
    wait: Duration,
    joining: Option<&CompatibilityIdentity>,
) -> Result<Record, OrbitError> {
    // Admission is held by lock alone, so a read-only descriptor serves.
    let file = open(root, ADMISSION_LOCK)?;
    let _waiter = if mode == Admission::Exclusive {
        ExclusiveWaiter::publish(root)?
    } else {
        None
    };
    let started = Instant::now();
    let deadline = started + wait.min(MAX_QUIESCE_TIMEOUT);
    let mut probe_at = started + UPGRADE_PROBE;
    loop {
        let attempt = match mode {
            Admission::Shared if admission_waiters::pending(root, false)? => {
                Err(std::io::ErrorKind::WouldBlock.into())
            }
            Admission::Shared => FileExt::try_lock_shared(&file.file),
            Admission::Exclusive => FileExt::try_lock_exclusive(&file.file),
        };
        // Close the race with an exclusive waiter publishing between the
        // first intent check and this shared lock acquisition.
        let attempt = match attempt {
            Ok(()) if mode == Admission::Shared && admission_waiters::pending(root, false)? => {
                FileExt::unlock(&file.file).map_err(refusal)?;
                Err(std::io::ErrorKind::WouldBlock.into())
            }
            other => other,
        };
        match attempt {
            Ok(()) => return Ok(file),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                let now = Instant::now();
                if now >= deadline {
                    // Even the switch this joiner waits behind is the cause.
                    return Err(upgrade_holding(root, None).unwrap_or_else(|| contended(wait)));
                }
                if now >= probe_at {
                    if let Some(upgrade) = upgrade_holding(root, joining) {
                        return Err(upgrade);
                    }
                    probe_at = now + UPGRADE_PROBE;
                }
                std::thread::sleep(ADMISSION_POLL);
            }
            Err(e) => return Err(refusal(e)),
        }
    }
}

/// The upgrade holding admission, if one is: a pending switch other than the
/// one `joining` waits behind, or an updater or takeover holding the
/// generation exclusively (which happens only under exclusive admission).
fn upgrade_holding(root: &Path, joining: Option<&CompatibilityIdentity>) -> Option<OrbitError> {
    if let Some(switch) = pending_switch(root) {
        return (joining != Some(&switch.target)).then(|| upgrade_holds_admission(Some(&switch)));
    }
    let generation = open(root, GENERATION_LOCK).ok()?;
    let settle = Instant::now() + EXCLUSIVE_SETTLE;
    loop {
        let held = FileExt::try_lock_shared(&generation.file)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::WouldBlock);
        if !held {
            return None;
        }
        if Instant::now() >= settle {
            return Some(upgrade_holds_admission(None));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Take the generation lock exclusively under exclusive admission.
///
/// A waiting joiner's [`upgrade_holding`] probe may hold it shared for an
/// instant, so a short retry tells that apart from a live participant.
pub(super) fn lock_generation_exclusive(generation: &Record) -> bool {
    let settle = Instant::now() + PROBE_SETTLE;
    loop {
        match FileExt::try_lock_exclusive(&generation.file) {
            Ok(()) => return true,
            Err(_) if Instant::now() < settle => std::thread::sleep(Duration::from_millis(1)),
            Err(_) => return false,
        }
    }
}

/// Take admission in `mode` once no pending switch stands in the way.
///
/// The switch's own target waits for it to resolve and then joins the
/// generation it pinned; any other newcomer is refused so no participant the
/// switch would have to wait for is admitted meanwhile.
pub(super) fn admitted_past_pending(
    root: &Path,
    participant: &Participant<'_>,
    mode: Admission,
    wait: Duration,
) -> Result<Record, OrbitError> {
    loop {
        let held = match admission(root, mode, wait, Some(participant.identity)) {
            Ok(held) => held,
            // Whichever upgrade holds admission, a command inside an
            // activity is refused typed rather than as contention.
            Err(error) if participant.in_activity => {
                return Err(match pending_switch(root) {
                    Some(switch) => upgrade_pending(switch_pending(&switch)),
                    None if upgrade_holding(root, None).is_some() => {
                        upgrade_pending("an Orbit update or generation takeover holds admission")
                    }
                    None => error,
                });
            }
            Err(error) => return Err(error),
        };
        let Some(switch) = pending_switch(root) else {
            return Ok(held);
        };
        if participant.in_activity {
            return Err(upgrade_pending(switch_pending(&switch)));
        }
        if switch.target != *participant.identity {
            return Err(refusal(switch_pending(&switch)));
        }
        drop(held);
        // A waiter that outlives its own deadline (stopped, not exiting)
        // must not hold its successors forever.
        if switch
            .deadline
            .checked_add_signed(chrono::Duration::seconds(5))
            .is_some_and(|expired| Utc::now() > expired)
        {
            return Err(refusal(format!(
                "{SWITCH_PENDING}, and its waiter (pid {}) has outlived its deadline",
                switch.pid
            )));
        }
        std::thread::sleep(QUIESCE_POLL);
    }
}
