//! Joining an authority: compatibility admission, the v1 fallback and quiesce.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use fs2::FileExt;

use super::identity::{Access, CompatibilityIdentity, Envelope};
use super::image::process_digest;
use super::paths::{ADMISSION_LOCK, GENERATION_LOCK, validated_generation_root};
use super::records::{Record, open, read_compat, read_generation, write_compat};
use super::refusal::{SWITCH_PENDING, WRITES_WHILE_FOREIGN, quiesce_timeout, refusal, unwritable};
use super::registry::{
    self, ParticipantRecord, ParticipantRole, PendingClaim, PendingSwitch, Registration,
    pending_switch,
};
use super::update::GenerationUpdate;
use super::{DEFAULT_QUIESCE_TIMEOUT, QUIESCE_TIMEOUT_ENV};
use crate::OrbitError;

const QUIESCE_POLL: Duration = Duration::from_millis(200);

/// A shared generation pin. Retain until all operations and replies finish.
pub struct GenerationGuard {
    _record: Record,
    /// True when this process joined a recorded generation other than its own
    /// digest, without rewriting the record (read-only same-schema join).
    joined_foreign: bool,
    _registration: Option<Registration>,
}

/// One process asking to participate under the v2 protocol.
pub struct Participant<'a> {
    /// SHA-256 of the running executable image.
    pub digest: &'a str,
    /// The state compatibility it was compiled with.
    pub identity: &'a CompatibilityIdentity,
    pub role: ParticipantRole,
    pub access: Access,
}

/// The configured wait for a breaking upgrade (see [`QUIESCE_TIMEOUT_ENV`]).
pub fn quiesce_bound() -> Duration {
    std::env::var(QUIESCE_TIMEOUT_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map_or(DEFAULT_QUIESCE_TIMEOUT, Duration::from_secs)
}

pub(super) fn admission(root: &Path) -> Result<Record, OrbitError> {
    // Admission is held by lock alone, so a read-only descriptor serves.
    let file = open(root, ADMISSION_LOCK)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match FileExt::try_lock_exclusive(&file.file) {
            Ok(()) => return Ok(file),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(refusal(e)),
        }
    }
}

/// The process-wide participation, for safe-point yield checks.
struct Participation {
    root: PathBuf,
    identity: CompatibilityIdentity,
    role: ParticipantRole,
}

static PARTICIPATION: OnceLock<Participation> = OnceLock::new();

/// The authority root and role this process joined as, once it has.
pub fn process_participation() -> Option<(&'static Path, ParticipantRole)> {
    PARTICIPATION
        .get()
        .map(|participation| (participation.root.as_path(), participation.role))
}

/// A switch this process should yield to at its next safe point: pending on
/// the authority it joined, requested by another process for an identity
/// other than its own.
pub fn pending_switch_for_this_process() -> Option<PendingSwitch> {
    let participation = PARTICIPATION.get()?;
    pending_switch(&participation.root).filter(|switch| {
        switch.pid != std::process::id() && switch.target != participation.identity
    })
}

impl GenerationGuard {
    pub(super) fn holding(record: Record, joined_foreign: bool) -> Self {
        Self {
            _record: record,
            joined_foreign,
            _registration: None,
        }
    }

    pub(super) fn registered(mut self, root: &Path, participant: &Participant<'_>) -> Self {
        self._registration = registry::register(
            root,
            &ParticipantRecord {
                pid: std::process::id(),
                role: participant.role,
                started_at: Utc::now(),
                access: participant.access,
                digest: participant.digest.to_string(),
                identity: participant.identity.clone(),
            },
        );
        self
    }

    /// Whether this pin joined a live generation it may not write: a
    /// read-only join under v1 rules, of a record naming another digest.
    pub fn joined_foreign_generation(&self) -> bool {
        self.joined_foreign
    }

    /// Join `root` for this process's lifetime, before runtime bootstrap, and
    /// remember the participation for [`pending_switch_for_this_process`].
    pub fn for_process<F>(
        root: &Path,
        identity: &CompatibilityIdentity,
        role: ParticipantRole,
        access: Access,
        store_schema: F,
    ) -> Result<Self, OrbitError>
    where
        F: FnOnce() -> Result<u32, OrbitError>,
    {
        let participant = Participant {
            digest: process_digest(Some(root), access != Access::ReadOnly)?,
            identity,
            role,
            access,
        };
        let guard = Self::join(root, &participant, quiesce_bound(), store_schema)?;
        let _ = PARTICIPATION.set(Participation {
            root: validated_generation_root(root)?,
            identity: identity.clone(),
            role,
        });
        Ok(guard)
    }

    /// Join under `compatibility-generation-v2`; see the module docs.
    ///
    /// `quiesce` bounds how long a superseding writer waits for incompatible
    /// participants to yield. `store_schema` reads the live store schema and
    /// is consulted only for a read-only join of a v1-owned record.
    pub fn join<F>(
        root: &Path,
        participant: &Participant<'_>,
        quiesce: Duration,
        store_schema: F,
    ) -> Result<Self, OrbitError>
    where
        F: FnOnce() -> Result<u32, OrbitError>,
    {
        let admission = admitted_past_pending(root, participant)?;
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
                Self::quiesce(root, admission, generation, participant, quiesce, &reason)
            }
            Some(reason) => Err(refusal(format!(
                "this binary ({identity}) is incompatible with the live Orbit processes: {reason}"
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
        store_schema: F,
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
            && let Ok(live) = store_schema()
        {
            let compiled = participant.identity.store_schema.version;
            if compiled != live {
                return Err(refusal(format!(
                    "another executable generation is still running \
                     (store schema {live} differs from compiled schema {compiled})"
                )));
            }
            let guard = Self::holding(generation, true).registered(root, participant);
            drop(admission);
            return Ok(guard);
        }
        FileExt::unlock(&generation.file).map_err(refusal)?;
        if FileExt::try_lock_exclusive(&generation.file).is_err() {
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
        let deadline = Instant::now() + bound;
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
                None => self::admission(root).ok(),
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

    /// Pin an exact digest under `executable-generation-v1` rules. The
    /// admission mutex makes lock conversion atomic with respect to other
    /// participants (flock conversion alone is not atomic).
    pub fn acquire(root: &Path, digest: &str) -> Result<Self, OrbitError> {
        let admission = admission(root)?;
        let mut generation = open(root, GENERATION_LOCK)?;
        FileExt::try_lock_shared(&generation.file).map_err(refusal)?;
        if read_generation(&mut generation.file)? == digest {
            return Ok(Self::holding(generation, false));
        }
        FileExt::unlock(&generation.file).map_err(refusal)?;
        FileExt::try_lock_exclusive(&generation.file).map_err(|_| refusal(WRITES_WHILE_FOREIGN))?;
        GenerationUpdate {
            root: validated_generation_root(root)?,
            admission,
            generation,
        }
        .pin(digest, None)
    }
}

/// Replace `envelope` when no other process holds the generation lock.
///
/// The caller holds admission. Releasing this descriptor cannot admit another
/// participant, and a live participant holds the lock until it exits, so a
/// successful exclusive probe means every identity in `envelope` has exited.
/// The v1 digest in the lock file stays: the compat record is valid only
/// while its `record_digest` matches that digest.
fn reseed_if_unheld(
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

/// Take the admission mutex once no pending switch stands in the way.
///
/// The switch's own target waits for it to resolve and then joins the
/// generation it pinned; any other newcomer is refused so no participant the
/// switch would have to wait for is admitted meanwhile.
fn admitted_past_pending(root: &Path, participant: &Participant<'_>) -> Result<Record, OrbitError> {
    loop {
        let held = admission(root)?;
        let Some(switch) = pending_switch(root) else {
            return Ok(held);
        };
        if switch.target != *participant.identity {
            return Err(refusal(format!(
                "{SWITCH_PENDING}: pid {} ({}) is waiting until {} to migrate to {}",
                switch.pid,
                switch.role,
                switch
                    .deadline
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                switch.target
            )));
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
