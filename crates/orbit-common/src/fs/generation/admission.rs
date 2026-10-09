//! Joining an authority: compatibility admission, the v1 fallback and quiesce.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use fs2::FileExt;

use super::admission_waiters::{self, ExclusiveWaiter};
use super::identity::{Access, CompatibilityIdentity, Envelope};
use super::image::process_digest;
use super::paths::{ADMISSION_LOCK, GENERATION_LOCK, validated_generation_root};
use super::records::{Record, open, read_compat, read_generation, write_compat};
use super::refusal::{
    SWITCH_PENDING, WRITES_WHILE_FOREIGN, contended, quiesce_timeout, refusal, switch_pending,
    unwritable, upgrade_holds_admission,
};
use super::registry::{
    self, ParticipantRecord, ParticipantRole, PendingClaim, PendingSwitch, Registration,
    pending_switch,
};
use super::update::GenerationUpdate;
use super::{DEFAULT_QUIESCE_TIMEOUT, MAX_QUIESCE_TIMEOUT, QUIESCE_TIMEOUT_ENV};
use crate::OrbitError;

const QUIESCE_POLL: Duration = Duration::from_millis(200);
const ADMISSION_POLL: Duration = Duration::from_millis(10);
/// How long admission may stay unavailable before a wait asks whether an
/// upgrade holds it. Ordinary holders release it within milliseconds.
const UPGRADE_PROBE: Duration = Duration::from_secs(1);
/// How long an [`upgrade_holding`] probe can hold the generation lock.
pub(super) const PROBE_SETTLE: Duration = Duration::from_millis(20);
/// How long an exclusive generation hold must last before a waiting joiner
/// reads it as an upgrade. A pin, reseed or takeover holds it exclusively
/// only while it rewrites the record; a process resumed by a handover waits
/// behind exactly such a pin and must not mistake it for an update.
const EXCLUSIVE_SETTLE: Duration = Duration::from_millis(250);

/// A shared generation pin. Retain until all operations and replies finish.
pub struct GenerationGuard {
    /// Released in the order [`Self::release`] gives.
    registration: Option<Registration>,
    record: Record,
    /// True when this process joined a recorded generation other than its own
    /// digest, without rewriting the record (read-only same-schema join).
    joined_foreign: bool,
}

/// One process asking to participate under the v2 protocol.
pub struct Participant<'a> {
    /// SHA-256 of the running executable image.
    pub digest: &'a str,
    /// The state compatibility it was compiled with.
    pub identity: &'a CompatibilityIdentity,
    pub role: ParticipantRole,
    pub access: Access,
    /// The resume capability it hands over with when a candidate is renamed
    /// over its executable, if it can (see [`ParticipantRecord::handover`]).
    pub handover: Option<&'a str>,
}

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
pub(super) enum Admission {
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
pub(super) fn admission(
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

/// The process-wide participation, for safe-point yield checks.
struct Participation {
    root: PathBuf,
    identity: CompatibilityIdentity,
    role: ParticipantRole,
    handover: Option<String>,
}

static PARTICIPATION: OnceLock<Participation> = OnceLock::new();

/// The authority root and role this process joined as, once it has.
pub fn process_participation() -> Option<(&'static Path, ParticipantRole)> {
    PARTICIPATION
        .get()
        .map(|participation| (participation.root.as_path(), participation.role))
}

/// The resume capability this process registered to hand over with, once it
/// has joined.
pub fn process_handover() -> Option<&'static str> {
    PARTICIPATION.get()?.handover.as_deref()
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
            record,
            joined_foreign,
            registration: None,
        }
    }

    pub(super) fn registered(mut self, root: &Path, participant: &Participant<'_>) -> Self {
        self.registration = registry::register(
            root,
            &ParticipantRecord {
                pid: std::process::id(),
                role: participant.role,
                started_at: Utc::now(),
                access: participant.access,
                digest: participant.digest.to_string(),
                identity: participant.identity.clone(),
                handover: participant.handover.map(str::to_string),
            },
        );
        self
    }

    /// Release the generation lock, then withdraw the registration.
    ///
    /// Two inferences read the registry against the generation lock. A shared
    /// join takes a live record to mean its owner holds the lock, so the
    /// authority is not empty. An updater that finds the lock held and no
    /// record visible refuses the holder as unregistered. Withdrawing first
    /// broke the second: a holder descheduled between withdrawing and
    /// releasing, for longer than the updater's settle, was refused as an
    /// unregistered v1 process instead of waited for [ORB-14869]. Releasing
    /// first would break the first. So the record is renamed to its releasing
    /// name, still locked: joiners stop counting it, an updater waits for it,
    /// and only after the lock is released is it withdrawn.
    /// `paused` runs between the rename and the release.
    fn release(&mut self, paused: impl FnOnce()) {
        if let Some(registration) = self.registration.as_mut()
            && !registration.begin_release()
        {
            // Without the releasing name, a live record must still mean a
            // held lock.
            self.registration = None;
        }
        paused();
        let _ = FileExt::unlock(&self.record.file);
        self.registration = None;
    }

    /// Drop this guard, running `paused` once its registration is withdrawn
    /// from the live set but before the generation lock is released.
    #[cfg(test)]
    pub(super) fn release_pausing(mut self, paused: impl FnOnce()) {
        self.release(paused);
    }

    /// Whether this pin joined a live generation it may not write: a
    /// read-only join under v1 rules, of a record naming another digest.
    pub fn joined_foreign_generation(&self) -> bool {
        self.joined_foreign
    }

    /// Join `root` for this process's lifetime, before runtime bootstrap, and
    /// remember the participation for [`pending_switch_for_this_process`] and
    /// [`process_handover`]. `handover` is the resume capability this process
    /// hands over with, if it can.
    pub fn for_process<F>(
        root: &Path,
        identity: &CompatibilityIdentity,
        role: ParticipantRole,
        handover: Option<&str>,
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
            handover,
        };
        let guard = Self::join(root, &participant, quiesce_bound(), store_schema)?;
        let _ = PARTICIPATION.set(Participation {
            root: validated_generation_root(root)?,
            identity: identity.clone(),
            role,
            handover: handover.map(str::to_string),
        });
        Ok(guard)
    }

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

impl Drop for GenerationGuard {
    fn drop(&mut self) {
        self.release(|| {});
    }
}

/// A v1-owned record admits a reader whose compiled store schema is live.
fn legacy_schema_check(identity: &CompatibilityIdentity, live: u32) -> Result<(), OrbitError> {
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

/// Take admission in `mode` once no pending switch stands in the way.
///
/// The switch's own target waits for it to resolve and then joins the
/// generation it pinned; any other newcomer is refused so no participant the
/// switch would have to wait for is admitted meanwhile.
fn admitted_past_pending(
    root: &Path,
    participant: &Participant<'_>,
    mode: Admission,
    wait: Duration,
) -> Result<Record, OrbitError> {
    loop {
        let held = admission(root, mode, wait, Some(participant.identity))?;
        let Some(switch) = pending_switch(root) else {
            return Ok(held);
        };
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
