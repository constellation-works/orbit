//! Exclusive admission for an updater, and pinning its candidate generation.
//!
//! An updater holds `.generation-admission.lock` exclusively, so no process
//! joins behind it, and then wants `.generation.lock` exclusively too. A
//! short-lived participant (a one-shot command or a clock tick) still holding
//! the generation is waited for, up to [`quiesce_bound`]. A long-lived one
//! refuses the updater at once, unless the admission names a candidate that
//! will be renamed over the executable and the participant registered a
//! resume capability that candidate reports: it hands over after the rename,
//! so [`GenerationUpdate::acquire_for_candidate`] admits beside it and names
//! it, and [`CandidateAdmission::pin`] waits for it to hand over before
//! recording the candidate. A holder that never registered (an
//! `executable-generation-v1` binary, or a sandboxed child that cannot write
//! the root) always refuses.

use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fs2::FileExt;

use super::admission::{
    Admission, GenerationGuard, PROBE_SETTLE, Participant, admission, quiesce_bound,
};
use super::handoff::HandoverCandidate;
use super::identity::{Access, CompatibilityIdentity, Envelope};
use super::paths::{GENERATION_LOCK, validated_generation_root};
use super::records::{Record, open, read_generation, write_compat};
use super::refusal::{
    SWITCH_PENDING, handover_outlasted, holders_outlasted, holders_refused, refusal, unwritable,
};
use super::registry::{self, ParticipantRecord, pending_switch};
use crate::OrbitError;

/// How often an updater looks again for short-lived participants to exit.
const SETTLE_POLL: Duration = Duration::from_millis(100);

/// Exclusive admission, before installing a candidate or mutating resources.
pub struct GenerationUpdate {
    pub(super) root: PathBuf,
    pub(super) generation: Record,
    pub(super) admission: Record,
}

/// Exclusive admission for an installer that renames `candidate` over the
/// executable: every live participant either exited or will hand over to
/// the candidate once it is installed. It reserves nothing after it drops.
/// The participants that hand over still hold the generation until they do,
/// so [`Self::pin`] waits for them after the rename.
pub struct CandidateAdmission {
    update: GenerationUpdate,
    handover: Vec<ParticipantRecord>,
}

impl CandidateAdmission {
    /// The live participants that will hand over to the candidate after the
    /// rename. Empty when nothing was live.
    pub fn handover(&self) -> &[ParticipantRecord] {
        &self.handover
    }

    /// See [`GenerationUpdate::ensure_can_record`].
    pub fn ensure_can_record(&self) -> Result<(), OrbitError> {
        self.update.ensure_can_record()
    }

    /// After the candidate is renamed over the executable, wait up to
    /// [`quiesce_bound`] for every participant in [`Self::handover`] to
    /// release the generation, by exec'ing into the candidate or exiting,
    /// then pin as [`GenerationUpdate::pin`] does.
    ///
    /// Admission stays exclusive meanwhile, so a process that has handed over
    /// queues behind it and joins the pinned generation once it is released.
    /// When one has not handed over by the bound, nothing is pinned and the
    /// admission is released.
    pub fn pin(
        self,
        digest: &str,
        identity: Option<&CompatibilityIdentity>,
    ) -> Result<GenerationGuard, OrbitError> {
        if !self.handover.is_empty() {
            self.update.await_handover()?;
        }
        self.update.pin(digest, identity)
    }
}

/// How one live participant stands toward an updater.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Standing {
    /// It finishes on its own; the updater waits for it.
    Finishes,
    /// It hands over to the candidate after the rename.
    HandsOver,
    /// Only its owner can end it.
    Blocks,
}

pub(super) fn standing(
    holder: &ParticipantRecord,
    candidate: Option<&HandoverCandidate>,
) -> Standing {
    if holder.role.is_short_lived() {
        return Standing::Finishes;
    }
    // Only a process that watches for a replaced installation hands over;
    // the `mcp listen` listener never does, whatever its record says.
    match (holder.handover.as_deref(), candidate) {
        (Some(capability), Some(candidate))
            if holder.role.is_long_lived() && candidate.resumes(capability) =>
        {
            Standing::HandsOver
        }
        _ => Standing::Blocks,
    }
}

impl GenerationUpdate {
    /// Refuse before installation/resource/store writes if a long-lived
    /// process is live or a generation switch is pending. Short-lived
    /// participants are waited for, up to [`quiesce_bound`].
    pub fn acquire(root: &Path) -> Result<Self, OrbitError> {
        let (update, handover) = Self::admit(root, None)?;
        debug_assert!(handover.is_empty(), "no candidate, so nothing hands over");
        Ok(update)
    }

    /// Admit an installer that will rename `candidate` over the executable:
    /// as [`Self::acquire`], except that a long-lived participant registered
    /// with a resume capability the candidate reports is admitted beside,
    /// and named in [`CandidateAdmission::handover`].
    pub fn acquire_for_candidate(
        root: &Path,
        candidate: &HandoverCandidate,
    ) -> Result<CandidateAdmission, OrbitError> {
        let (update, handover) = Self::admit(root, Some(candidate))?;
        Ok(CandidateAdmission { update, handover })
    }

    /// Take exclusive admission and then the generation lock, unless every
    /// live participant will hand over to `candidate`. Returns the
    /// participants that will; when it is empty, the generation is held
    /// exclusively.
    fn admit(
        root: &Path,
        candidate: Option<&HandoverCandidate>,
    ) -> Result<(Self, Vec<ParticipantRecord>), OrbitError> {
        let bound = quiesce_bound();
        // New joiners queue behind this from here on, so the participants
        // already live are the only ones left to wait for.
        let admission = admission(root, Admission::Exclusive, bound, None)?;
        if let Some(switch) = pending_switch(root) {
            return Err(refusal(format!(
                "{SWITCH_PENDING} (pid {} is waiting to migrate to {})",
                switch.pid, switch.target
            )));
        }
        let mut generation = open(root, GENERATION_LOCK)?;
        let deadline = Instant::now() + bound;
        let mut unregistered_since = None;
        let handover = loop {
            if FileExt::try_lock_exclusive(&generation.file).is_ok() {
                break Vec::new();
            }
            let live = registry::live_participants(root, None, false);
            if live.is_empty() {
                // A queued joiner's upgrade probe holds the lock shared for
                // an instant; only a hold that outlasts a short retry belongs
                // to a process that did not register.
                let since = *unregistered_since.get_or_insert_with(Instant::now);
                if since.elapsed() < PROBE_SETTLE {
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                }
                return Err(holders_refused(&[], candidate));
            }
            unregistered_since = None;
            let standings = live
                .iter()
                .map(|holder| standing(holder, candidate))
                .collect::<Vec<_>>();
            if standings.contains(&Standing::Blocks) {
                return Err(holders_refused(&live, candidate));
            }
            if !standings.contains(&Standing::Finishes) {
                break live;
            }
            if Instant::now() >= deadline {
                return Err(holders_outlasted(bound, &live, candidate));
            }
            std::thread::sleep(SETTLE_POLL);
        };
        read_generation(&mut generation.file)?;
        Ok((
            Self {
                root: validated_generation_root(root)?,
                admission,
                generation,
            },
            handover,
        ))
    }

    /// Take the generation exclusively once every participant admitted beside
    /// this update has released it. Nobody joins meanwhile, so the live set
    /// only shrinks.
    fn await_handover(&self) -> Result<(), OrbitError> {
        let bound = quiesce_bound();
        let deadline = Instant::now() + bound;
        loop {
            if FileExt::try_lock_exclusive(&self.generation.file).is_ok() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                let live = registry::live_participants(&self.root, None, false);
                return Err(handover_outlasted(bound, &live));
            }
            std::thread::sleep(SETTLE_POLL);
        }
    }

    /// Refuse an admission that could never record a candidate generation.
    ///
    /// [`Self::pin`] repeats this check, but an updater only reaches `pin`
    /// after it has already replaced the executable. A caller that admits
    /// against several authorities asks each of them this up front, so a root
    /// whose record is read-only refuses the run while nothing is staged.
    pub fn ensure_can_record(&self) -> Result<(), OrbitError> {
        if self.generation.writable {
            return Ok(());
        }
        Err(unwritable(
            "this authority's generation record cannot be written from here, so a \
             candidate generation could never be recorded there",
        ))
    }

    /// After replacement, pin the candidate through convergence. Old pinned
    /// executables cannot enter between the updater and its candidate children.
    ///
    /// `identity` is the candidate's reported compatibility; with it, other
    /// compatible builds may join once the updater releases its pin. Without
    /// it (a candidate that only speaks `executable-generation-v1`) the record
    /// admits only that digest.
    pub fn pin(
        self,
        digest: &str,
        identity: Option<&CompatibilityIdentity>,
    ) -> Result<GenerationGuard, OrbitError> {
        self.pin_as(
            digest,
            identity.map(|identity| (identity, Access::Write)),
            None,
        )
    }

    /// Record `digest` (and, with it, the envelope of `identity`) and hold
    /// the generation shared, registering `participant` before admission is
    /// released.
    pub(super) fn pin_as(
        mut self,
        digest: &str,
        identity: Option<(&CompatibilityIdentity, Access)>,
        participant: Option<&Participant<'_>>,
    ) -> Result<GenerationGuard, OrbitError> {
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(refusal("invalid executable digest"));
        }
        if !self.generation.writable {
            // Admitting here would leave joiners reading a record that names
            // a generation other than the one actually running.
            return Err(unwritable(
                "this executable generation differs from the recorded one and the record \
                 cannot be written from here",
            ));
        }
        if let Some((identity, access)) = identity {
            // Before the record: a crash in between leaves a compat record
            // naming a digest the record does not, which reads as absent.
            write_compat(&self.root, digest, &Envelope::of(identity, access))?;
        }
        self.generation
            .file
            .seek(SeekFrom::Start(0))
            .map_err(refusal)?;
        self.generation
            .file
            .write_all(format!("1:{digest}\n").as_bytes())
            .map_err(refusal)?;
        self.generation.file.set_len(67).map_err(refusal)?;
        self.generation.file.sync_all().map_err(refusal)?;
        FileExt::lock_shared(&self.generation.file).map_err(refusal)?;
        let guard = GenerationGuard::holding(self.generation, false);
        let guard = match participant {
            Some(participant) => guard.registered(&self.root, participant),
            None => guard,
        };
        drop(self.admission);
        Ok(guard)
    }
}
