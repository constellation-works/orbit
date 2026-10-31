//! Shared and exclusive joins, the v1 fallback, and quiescing participants.

use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use fs2::FileExt;

use super::super::MAX_QUIESCE_TIMEOUT;
use super::super::identity::{Access, Envelope};
use super::super::paths::{GENERATION_LOCK, validated_generation_root};
use super::super::records::{Record, open, read_compat, read_generation, write_compat};
use super::super::refusal::{
    INCOMPATIBLE, WRITES_WHILE_FOREIGN, quiesce_timeout, refusal, unwritable, upgrade_pending,
};
use super::super::registry::{self, PendingClaim, PendingSwitch};
use super::super::update::GenerationUpdate;
use super::guard::{GenerationGuard, Participant};
use super::legacy::{legacy_schema_check, reseed_if_unheld};
use super::lock::{
    Admission, QUIESCE_POLL, admission, admitted_past_pending, lock_generation_exclusive,
};
use crate::OrbitError;

/// The live store schema, read at most once per join.
struct LiveSchema<F> {
    read: Option<F>,
    value: Option<u32>,
}

impl<F: FnOnce() -> Result<u32, OrbitError>> LiveSchema<F> {
    fn get(&mut self) -> Option<u32> {
        if let Some(read) = self.read.take() {
            self.value = read().ok();
        }
        self.value
    }
}

impl GenerationGuard {
    /// Join under `compatibility-generation-v2`; see the module docs.
    ///
    /// `quiesce` bounds how long a superseding writer waits for incompatible
    /// participants to yield, and how long any join waits for admission.
    /// `store_schema` reads the live store schema and is consulted only for a
    /// read-only join of a v1-owned record.
    pub fn join<F>(
        root: &Path,
        participant: &Participant<'_>,
        quiesce: Duration,
        store_schema: F,
    ) -> Result<Self, OrbitError>
    where
        F: FnOnce() -> Result<u32, OrbitError>,
    {
        let mut store_schema = LiveSchema {
            read: Some(store_schema),
            value: None,
        };
        if let Some(guard) = Self::join_shared(root, participant, quiesce, &mut store_schema)? {
            return Ok(guard);
        }
        Self::join_exclusive(root, participant, quiesce, &mut store_schema)
    }

    /// Join under shared admission when joining writes nothing shared: the
    /// recorded envelope already admits this identity unchanged and would not
    /// be reseeded, or a v1-owned record admits this reader. `None` means the
    /// join needs exclusive admission.
    fn join_shared<F>(
        root: &Path,
        participant: &Participant<'_>,
        quiesce: Duration,
        store_schema: &mut LiveSchema<F>,
    ) -> Result<Option<Self>, OrbitError>
    where
        F: FnOnce() -> Result<u32, OrbitError>,
    {
        let admission = admitted_past_pending(root, participant, Admission::Shared, quiesce)?;
        let mut generation = open(root, GENERATION_LOCK)?;
        // Only exclusive admission takes the generation lock exclusively.
        FileExt::try_lock_shared(&generation.file).map_err(refusal)?;
        let recorded = read_generation(&mut generation.file)?;
        let (identity, access) = (participant.identity, participant.access);
        let joined_foreign = match read_compat(root, &recorded) {
            Some(envelope) => {
                if envelope.refusal(identity, access).is_some()
                    || envelope.widened(identity, access) != envelope
                {
                    return Ok(None);
                }
                // A reseed is a no-op when it would record this envelope, or
                // when another registered participant (which holds the
                // generation lock until it withdraws) keeps the authority
                // non-empty. Otherwise only an exclusive probe can tell.
                if envelope != Envelope::of(identity, access)
                    && registry::live_participants(root, None, false).is_empty()
                {
                    return Ok(None);
                }
                access == Access::ReadOnly && participant.digest != recorded
            }
            None if access == Access::ReadOnly
                && !recorded.is_empty()
                && recorded != participant.digest =>
            {
                let Some(live) = store_schema.get() else {
                    return Ok(None);
                };
                legacy_schema_check(identity, live)?;
                true
            }
            None => return Ok(None),
        };
        let guard = Self::holding(generation, joined_foreign).registered(root, participant);
        drop(admission);
        Ok(Some(guard))
    }

    /// Join under exclusive admission: every join that may rewrite a record,
    /// reseed an empty authority, take over or quiesce.
    fn join_exclusive<F>(
        root: &Path,
        participant: &Participant<'_>,
        quiesce: Duration,
        store_schema: &mut LiveSchema<F>,
    ) -> Result<Self, OrbitError>
    where
        F: FnOnce() -> Result<u32, OrbitError>,
    {
        let admission = admitted_past_pending(root, participant, Admission::Exclusive, quiesce)?;
        let mut generation = open(root, GENERATION_LOCK)?;
        FileExt::try_lock_shared(&generation.file).map_err(refusal)?;
        let recorded = read_generation(&mut generation.file)?;
        let Some(envelope) = read_compat(root, &recorded) else {
            return Self::join_legacy(
                root,
                admission,
                generation,
                participant,
                &recorded,
                store_schema,
            );
        };
        // Admission is still held, so this probe cannot race another joiner.
        // An exited process keeps its place in the envelope until then; the
        // lock, not the file, is what shows the authority is empty.
        let envelope = reseed_if_unheld(root, &generation, &recorded, participant, envelope)?;
        let (identity, access) = (participant.identity, participant.access);
        match envelope.refusal(identity, access) {
            None => {
                let widened = envelope.widened(identity, access);
                if widened != envelope {
                    let written = if generation.writable {
                        write_compat(root, &recorded, &widened)
                    } else {
                        Err(unwritable(
                            "joining would widen the recorded generation, which cannot be \
                             written from here",
                        ))
                    };
                    // A reader that cannot record itself still joins, as v1
                    // read-only joins did; a writer must be visible to the
                    // next newcomer's compatibility check.
                    if let Err(error) = written
                        && access == Access::Write
                    {
                        return Err(error);
                    }
                }
                let joined_foreign = access == Access::ReadOnly && participant.digest != recorded;
                let guard = Self::holding(generation, joined_foreign).registered(root, participant);
                drop(admission);
                Ok(guard)
            }
            Some(reason) if access == Access::Write && envelope.is_superseded_by(identity) => {
                if participant.in_activity {
                    return Err(upgrade_pending(format!(
                        "this binary ({identity}) would switch the store generation: {reason}"
                    )));
                }
                Self::quiesce(root, admission, generation, participant, quiesce, &reason)
            }
            Some(reason) => Err(refusal(format!(
                "this binary ({identity}) {INCOMPATIBLE}: {reason}"
            ))),
        }
    }

    /// Admission when no v2 participant recorded the live generation: the
    /// record is empty or owned by `executable-generation-v1` processes,
    /// which never yield, so a mismatch refuses at once instead of waiting.
    fn join_legacy<F>(
        root: &Path,
        admission: Record,
        generation: Record,
        participant: &Participant<'_>,
        recorded: &str,
        store_schema: &mut LiveSchema<F>,
    ) -> Result<Self, OrbitError>
    where
        F: FnOnce() -> Result<u32, OrbitError>,
    {
        let access = participant.access;
        if recorded == participant.digest {
            // Only this build writes this digest, so every holder is this
            // identity. Recording the envelope lets other builds join.
            if generation.writable {
                let _ = write_compat(root, recorded, &Envelope::of(participant.identity, access));
            }
            let guard = Self::holding(generation, false).registered(root, participant);
            drop(admission);
            return Ok(guard);
        }
        if access == Access::ReadOnly
            && !recorded.is_empty()
            && let Some(live) = store_schema.get()
        {
            legacy_schema_check(participant.identity, live)?;
            let guard = Self::holding(generation, true).registered(root, participant);
            drop(admission);
            return Ok(guard);
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
        .pin_as(
            participant.digest,
            Some((participant.identity, access)),
            Some(participant),
        )
    }

    /// Record a pending switch, release this process's own hold, and retry
    /// exclusive admission until the live participants yield or `bound`
    /// expires.
    fn quiesce(
        root: &Path,
        admission: Record,
        generation: Record,
        participant: &Participant<'_>,
        bound: Duration,
        reason: &str,
    ) -> Result<Self, OrbitError> {
        let requested_at = Utc::now();
        let deadline = Instant::now() + bound.min(MAX_QUIESCE_TIMEOUT);
        let switch = PendingSwitch {
            pid: std::process::id(),
            role: participant.role,
            digest: participant.digest.to_string(),
            target: participant.identity.clone(),
            requested_at,
            deadline: chrono::Duration::from_std(bound)
                .ok()
                .and_then(|bound| requested_at.checked_add_signed(bound))
                .unwrap_or(DateTime::<Utc>::MAX_UTC),
        };
        let claim: PendingClaim = registry::claim_pending(root, &switch)?;
        FileExt::unlock(&generation.file).map_err(refusal)?;
        tracing::info!(
            target: "orbit.generation",
            wait_secs = bound.as_secs(),
            "waiting for live Orbit processes to yield to a breaking migration: {reason}"
        );
        let mut admission = Some(admission);
        loop {
            let held = match admission.take() {
                Some(held) => Some(held),
                None => self::admission(
                    root,
                    Admission::Exclusive,
                    QUIESCE_POLL,
                    Some(participant.identity),
                )
                .ok(),
            };
            if let Some(held) = held {
                if FileExt::try_lock_exclusive(&generation.file).is_ok() {
                    let pinned = GenerationUpdate {
                        root: validated_generation_root(root)?,
                        admission: held,
                        generation,
                    }
                    .pin_as(
                        participant.digest,
                        Some((participant.identity, Access::Write)),
                        Some(participant),
                    );
                    drop(claim);
                    return pinned;
                }
                if Instant::now() >= deadline {
                    let blockers = registry::live_participants(root, None, true);
                    drop(held);
                    drop(claim);
                    return Err(quiesce_timeout(reason, bound, &blockers));
                }
            } else if Instant::now() >= deadline {
                let blockers = registry::live_participants(root, None, false);
                drop(claim);
                return Err(quiesce_timeout(reason, bound, &blockers));
            }
            std::thread::sleep(QUIESCE_POLL);
        }
    }
}
