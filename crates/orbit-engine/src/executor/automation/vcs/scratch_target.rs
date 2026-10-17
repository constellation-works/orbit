//! Durable ownership of the host validation targets detached scratch
//! checkouts build into.
//!
//! A host validation target is keyed by its checkout's canonical path
//! ([`super::build_output::host_validation_target`]). Worktree GC
//! attributes a run checkout's target by re-deriving the path from the run
//! record. A scratch checkout is named by no run record and is removed after
//! use, so its target outlives it. GC reclaims such a target only through a
//! record of what built it, and only while it holds the lock every user holds
//! while building there:
//!
//! - a base rerun runs under its base-result cache lock, and that lock's file
//!   names the scratch checkout the run used ([`super::baseline`]);
//! - host evidence and recovery validation hold a [`ScratchHostTargetLease`]:
//!   an exclusive lock beside a lineage record naming the checkout's kind and
//!   canonical source, written before the target is created.
//!
//! A target name neither record derives is never inferred to be scratch
//! output.

use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_common::fs::file_lock::{FileLockGuard, read_file_lock_holder};
#[cfg(target_os = "linux")]
use orbit_common::fs::file_lock::{FileLockOptions, acquire_exclusive_file_lock};
#[cfg(target_os = "linux")]
use orbit_common::fs::io::atomic_write_bytes;
use serde::{Deserialize, Serialize};

use super::build_output::{host_validation_source, host_validation_target_key};

/// Where leases live, relative to the primary checkout: beside the targets,
/// never inside them, so listing the targets lists only targets.
const LEASES_DIR: [&str; 3] = [".orbit", "state", "host-validation-leases"];
const LEASE_SCHEMA_VERSION: u32 = 1;
/// Long enough to outlast GC removing a large target under the lock.
#[cfg(target_os = "linux")]
const LEASE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
#[cfg(target_os = "linux")]
pub(super) const LEASE_LABEL: &str = "host validation target lease";

/// A detached scratch checkout that builds into a host validation target
/// under a lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScratchHostTarget {
    /// A host-evidence checkout of a candidate.
    HostEvidence,
    /// A recovery or review-reconciliation checkout.
    Recovery,
}

impl ScratchHostTarget {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::HostEvidence => "host-evidence",
            Self::Recovery => "recovery",
        }
    }
}

/// What built one leased target.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct LeaseRecord {
    schema_version: u32,
    pub(super) kind: ScratchHostTarget,
    /// The canonical checkout path the target's name hashes.
    pub(super) source: PathBuf,
}

/// A leased target's lineage record and lock.
#[cfg(target_os = "linux")]
pub(super) struct LeasedTarget {
    pub(super) record: LeaseRecord,
    pub(super) record_path: PathBuf,
    pub(super) lock: PathBuf,
}

/// Held while a scratch checkout builds into its host validation target; GC
/// never reclaims the target while any lease on it is held.
#[must_use = "the lease is released as soon as it is dropped"]
pub struct ScratchHostTargetLease {
    _lock: Option<FileLockGuard>,
}

/// Lease the host validation target of scratch `checkout`, recording what
/// built it before the target exists. The descriptor-bound collector runs
/// only on Linux; elsewhere output is retained and the lease holds nothing.
pub fn lease_scratch_host_target(
    checkout: &Path,
    kind: ScratchHostTarget,
) -> Result<ScratchHostTargetLease, OrbitError> {
    #[cfg(target_os = "linux")]
    {
        let (primary, source) = host_validation_source(checkout)?;
        let (record_path, lock) = lease_paths(&primary, &host_validation_target_key(&source));
        let guard = acquire_exclusive_file_lock(
            &lock,
            LEASE_LABEL,
            FileLockOptions {
                timeout: LEASE_TIMEOUT,
                warn_after: Duration::from_secs(30),
                ..FileLockOptions::default()
            },
        )
        .map_err(|error| OrbitError::Execution(format!("lease {}: {error}", lock.display())))?;
        let record = LeaseRecord {
            schema_version: LEASE_SCHEMA_VERSION,
            kind,
            source,
        };
        if read_record(&record_path).as_ref() != Some(&record) {
            let bytes = serde_json::to_vec(&record)
                .map_err(|error| OrbitError::Execution(format!("encode lease record: {error}")))?;
            atomic_write_bytes(&record_path, &bytes).map_err(|error| {
                OrbitError::Execution(format!("record {}: {error}", record_path.display()))
            })?;
        }
        Ok(ScratchHostTargetLease { _lock: Some(guard) })
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (checkout, kind);
        Ok(ScratchHostTargetLease { _lock: None })
    }
}

/// The lease that names target `key` under `primary`, when its record is a
/// regular file whose source derives exactly that key.
#[cfg(target_os = "linux")]
pub(super) fn leased_target(primary: &Path, key: &str) -> Option<LeasedTarget> {
    let (record_path, lock) = lease_paths(primary, key);
    // A no-follow stat sees a redirect as neither a directory nor a file.
    if !std::fs::symlink_metadata(record_path.parent()?).is_ok_and(|meta| meta.is_dir())
        || !std::fs::symlink_metadata(&record_path).is_ok_and(|meta| meta.is_file())
    {
        return None;
    }
    let record = read_record(&record_path)?;
    (host_validation_target_key(&record.source) == key).then_some(LeasedTarget {
        record,
        record_path,
        lock,
    })
}

fn lease_paths(primary: &Path, key: &str) -> (PathBuf, PathBuf) {
    let directory = LEASES_DIR
        .iter()
        .fold(primary.to_path_buf(), |path, component| {
            path.join(component)
        });
    (
        directory.join(format!("{key}.json")),
        directory.join(format!("{key}.lock")),
    )
}

fn read_record(path: &Path) -> Option<LeaseRecord> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice::<LeaseRecord>(&bytes)
        .ok()
        .filter(|record| record.schema_version == LEASE_SCHEMA_VERSION)
}

/// A live host-validation lease naming `checkout` as its source.
///
/// The lineage record must hash to this checkout. A recorded holder whose
/// process has exited does not pin it: the operating system released that
/// lock with the process, and a leftover file must not keep a gigabyte
/// checkout. Where process liveness cannot be observed, a recorded holder
/// counts as live.
pub(super) fn held_checkout_lease(checkout: &Path) -> Option<String> {
    let (primary, source) = host_validation_source(checkout).ok()?;
    let key = host_validation_target_key(&source);
    let (record_path, lock) = lease_paths(&primary, &key);
    let record = read_record(&record_path)?;
    if host_validation_target_key(&record.source) != key {
        return None;
    }
    let holder = read_file_lock_holder(&lock)?;
    if !holder_still_running(holder.pid) {
        return None;
    }
    Some(format!(
        "{} lease held by pid {} ({})",
        record.kind.label(),
        holder.pid,
        holder.label
    ))
}

fn holder_still_running(pid: u32) -> bool {
    #[cfg(unix)]
    {
        orbit_common::process::identity::process_is_alive(pid)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}
