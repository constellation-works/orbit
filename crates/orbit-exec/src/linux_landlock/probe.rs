//! Landlock ABI requirements, the kernel probe, and the diagnostics a host
//! that cannot enforce a ruleset reports instead of spawning unconfined.

use orbit_common::OrbitError;

/// Lowest Landlock ABI this enforcement accepts.
///
/// ABI 2 (Linux 5.19) is the first to expose `LANDLOCK_ACCESS_FS_REFER`.
/// Below it a confined child could relocate a denied file into a readable
/// directory, so an older kernel is reported as unavailable rather than
/// enforced with a known hole.
pub const MINIMUM_LANDLOCK_ABI: i64 = 2;

/// First Landlock ABI that can confine standalone `truncate(2)` and `O_TRUNC`.
/// A plugin write boundary must handle this right even when it grants no
/// writable path, or a child could truncate files outside its write roots.
pub const WRITE_LANDLOCK_ABI: i64 = 3;

/// First Landlock ABI (Linux 6.7) that can refuse TCP bind and connect, which
/// is what holds a plugin's `network: none` at the kernel.
pub const NETWORK_LANDLOCK_ABI: i64 = 4;

/// Which rights a ruleset takes over beyond the read set every ruleset
/// handles.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RulesetScope {
    /// Handle every write-side right, so a path without a write grant is
    /// read-only to the child.
    pub(crate) confine_writes: bool,
    /// Handle TCP bind and connect with no rules, refusing every endpoint.
    pub(crate) deny_tcp: bool,
}

impl RulesetScope {
    pub(super) fn require_abi(self, abi: i64) -> Result<(), OrbitError> {
        if self.confine_writes && abi < WRITE_LANDLOCK_ABI {
            return Err(OrbitError::PolicyDenied(format!(
                "confining plugin writes requires Landlock ABI {WRITE_LANDLOCK_ABI} or later \
                 to enforce truncate; this kernel supports ABI {abi}"
            )));
        }
        if self.deny_tcp && abi < NETWORK_LANDLOCK_ABI {
            return Err(OrbitError::PolicyDenied(format!(
                "refusing TCP at the process boundary requires Landlock ABI \
                 {NETWORK_LANDLOCK_ABI} or later; this kernel supports ABI {abi}"
            )));
        }
        Ok(())
    }
}

/// Result of asking the running kernel whether it can enforce a ruleset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandlockProbeOutcome {
    pub available: bool,
    pub abi: i64,
    pub detail: String,
}

/// Ask the running kernel which Landlock ABI it supports.
pub fn probe_landlock() -> LandlockProbeOutcome {
    let abi = landlock_abi();
    if abi >= MINIMUM_LANDLOCK_ABI {
        LandlockProbeOutcome {
            available: true,
            abi,
            detail: format!("Landlock ABI {abi}"),
        }
    } else {
        LandlockProbeOutcome {
            available: false,
            abi,
            detail: unavailable_detail(abi),
        }
    }
}

/// Message for a host that cannot enforce an activity-scoped filesystem
/// profile. Activity-scoped `proc.spawn` fails with this rather than running
/// the child unconfined.
pub fn landlock_unavailable_message(probe: &LandlockProbeOutcome) -> String {
    format!(
        "activity-scoped proc.spawn cannot run here: enforcing the filesystem profile at the \
         process boundary requires Linux Landlock ABI {MINIMUM_LANDLOCK_ABI} or later ({})",
        probe.detail
    )
}

#[cfg(target_os = "linux")]
fn landlock_abi() -> i64 {
    super::ruleset::abi_version()
}

#[cfg(not(target_os = "linux"))]
fn landlock_abi() -> i64 {
    -1
}

#[cfg(target_os = "linux")]
fn unavailable_detail(abi: i64) -> String {
    if abi >= 1 {
        format!("this kernel supports only Landlock ABI {abi}")
    } else {
        format!(
            "landlock_create_ruleset version probe failed: {}",
            std::io::Error::last_os_error()
        )
    }
}

#[cfg(not(target_os = "linux"))]
fn unavailable_detail(_abi: i64) -> String {
    format!(
        "Landlock is a Linux facility and {} is not Linux",
        std::env::consts::OS
    )
}
