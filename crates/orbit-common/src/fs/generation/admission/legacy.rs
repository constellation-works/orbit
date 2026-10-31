//! Exact-digest v1 admission and legacy schema/envelope checks.

use std::path::Path;

use fs2::FileExt;

use super::super::identity::{Access, CompatibilityIdentity, Envelope};
use super::super::paths::{GENERATION_LOCK, validated_generation_root};
use super::super::records::{Record, open, read_generation, write_compat};
use super::super::refusal::{WRITES_WHILE_FOREIGN, refusal, unwritable};
use super::super::update::GenerationUpdate;
use super::guard::{GenerationGuard, Participant};
use super::lock::{Admission, admission, lock_generation_exclusive, quiesce_bound};
use crate::OrbitError;

impl GenerationGuard {
    /// Pin an exact digest under `executable-generation-v1` rules. Exclusive
    /// admission makes lock conversion atomic with respect to other
    /// participants (flock conversion alone is not atomic).
    pub fn acquire(root: &Path, digest: &str) -> Result<Self, OrbitError> {
        let admission = admission(root, Admission::Exclusive, quiesce_bound(), None)?;
        let mut generation = open(root, GENERATION_LOCK)?;
        FileExt::try_lock_shared(&generation.file).map_err(refusal)?;
        if read_generation(&mut generation.file)? == digest {
            return Ok(Self::holding(generation, false));
        }
        FileExt::unlock(&generation.file).map_err(refusal)?;
        if !lock_generation_exclusive(&generation) {
            return Err(refusal(WRITES_WHILE_FOREIGN));
        }
        GenerationUpdate {
            root: validated_generation_root(root)?,
            admission,
            generation,
        }
        .pin(digest, None)
    }
}

/// A v1-owned record admits a reader whose compiled store schema is live.
pub(super) fn legacy_schema_check(
    identity: &CompatibilityIdentity,
    live: u32,
) -> Result<(), OrbitError> {
    let compiled = identity.store_schema.version;
    if compiled != live {
        return Err(refusal(format!(
            "another executable generation is still running \
             (store schema {live} differs from compiled schema {compiled})"
        )));
    }
    Ok(())
}

/// Replace `envelope` when no other process holds the generation lock.
///
/// The caller holds exclusive admission. Releasing this descriptor cannot
/// admit another participant, and a live participant holds the lock until it
/// exits, so a successful exclusive probe means every identity in `envelope`
/// has exited.
/// The v1 digest in the lock file stays: the compat record is valid only
/// while its `record_digest` matches that digest.
pub(super) fn reseed_if_unheld(
    root: &Path,
    generation: &Record,
    recorded: &str,
    participant: &Participant<'_>,
    envelope: Envelope,
) -> Result<Envelope, OrbitError> {
    FileExt::unlock(&generation.file).map_err(refusal)?;
    if FileExt::try_lock_exclusive(&generation.file).is_err() {
        FileExt::try_lock_shared(&generation.file).map_err(refusal)?;
        return Ok(envelope);
    }
    let fresh = Envelope::of(participant.identity, participant.access);
    if fresh != envelope {
        let written = if generation.writable {
            write_compat(root, recorded, &fresh)
        } else {
            Err(unwritable(
                "joining an empty authority would replace the recorded generation, which \
                 cannot be written from here",
            ))
        };
        // A reader that cannot record itself still joins, as when a widening
        // cannot be written. A writer must be visible to the next newcomer.
        if let Err(error) = written
            && participant.access == Access::Write
        {
            return Err(error);
        }
    }
    FileExt::lock_shared(&generation.file).map_err(refusal)?;
    Ok(fresh)
}
