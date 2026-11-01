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
//! the root) always refuses; one whose record is releasing is waited for.
//!
//! An updater that renames nothing, because the installed executable already
//! is the generation it pins (re-running an update to finish convergence),
//! takes [`GenerationUpdate::acquire_current`] instead: a long-lived
//! participant already running exactly that executable digest and
//! compatibility identity, with a resume capability, is admitted beside.
//! Nothing is renamed, so nothing would hand it over; the admission records a
//! pending refresh, which asks exactly those participants to hand over in
//! place, to the installed executable they already run. Flock cannot count a
//! shared lock's holders, so only the exclusive generation lock proves that
//! no holder that never registered stands behind them:
//! [`CurrentAdmission::pin`] waits for it as [`CandidateAdmission::pin`]
//! does, and refuses when the registered participants are gone and the
//! generation is still held.

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
    SWITCH_PENDING, handover_outlasted, holders_outlasted, holders_refused, refresh_outlasted,
    refusal, unwritable,
};
use super::registry::{
    self, ParticipantRecord, ParticipantRole, PendingClaim, PendingSwitch, pending_switch,
};
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

/// The executable generation an updater that renames nothing re-pins: the
/// digest and reported compatibility identity of the installed executable.
pub struct InstalledGeneration<'a> {
    pub digest: &'a str,
    /// `None` for an executable that reports no compatibility identity: then
    /// no live participant is known to run its generation.
    pub identity: Option<&'a CompatibilityIdentity>,
}

impl InstalledGeneration<'_> {
    /// Whether `holder` already runs exactly this generation, stays up
    /// across operations and can hand over in place. A short-lived one is
    /// waited for as ever; one without a resume capability still refuses.
    fn runs(&self, holder: &ParticipantRecord) -> bool {
        !holder.role.is_short_lived()
            && holder.handover.is_some()
            && holder.digest == self.digest
            && self.identity == Some(&holder.identity)
    }
}

/// Exclusive admission for an updater that renames nothing: every live
/// participant either exited or already runs the installed generation and was
/// asked to hand over to it in place, see [`GenerationUpdate::acquire_current`].
pub struct CurrentAdmission {
    update: GenerationUpdate,
    digest: String,
    current: Vec<ParticipantRecord>,
    /// The pending refresh those participants hand over on. Dropping it
    /// clears the request.
    refresh: Option<PendingClaim>,
}

impl CurrentAdmission {
    /// The live participants admitted beside because they already run the
    /// installed generation; each hands over in place before the pin. Empty
    /// when nothing long-lived was live.
    pub fn current(&self) -> &[ParticipantRecord] {
        &self.current
    }

    /// See [`GenerationUpdate::ensure_can_record`].
    pub fn ensure_can_record(&self) -> Result<(), OrbitError> {
        self.update.ensure_can_record()
    }

    /// Pin the installed generation through convergence, as
    /// [`GenerationUpdate::pin`] does, once the generation is held
    /// exclusively.
    ///
    /// With participants in [`Self::current`], this first waits up to
    /// [`quiesce_bound`] for each to hand over in place, which releases its
    /// hold. Admission stays exclusive meanwhile, so they queue behind it and
    /// join what it pinned. A hold that outlasts them belongs to a process
    /// that never registered (or one that joined some other way), so the pin
    /// refuses as admission would have refused it alone; nothing is pinned
    /// and admission is released.
    pub fn pin(
        mut self,
        digest: &str,
        identity: Option<&CompatibilityIdentity>,
    ) -> Result<GenerationGuard, OrbitError> {
        if digest != self.digest {
            return Err(refusal(
                "the generation to pin is not the one admission was taken for",
            ));
        }
        if !self.current.is_empty() {
            self.update.await_refresh(&self.current)?;
        }
        // The participants that handed over wait on admission, not on the
        // request; clearing it first leaves nothing pending once it opens.
        drop(self.refresh.take());
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
    /// It already runs the generation an updater that renames nothing pins.
    Current,
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
        let (update, handover) = Self::admit(root, None, None)?;
        debug_assert!(handover.is_empty(), "no candidate, so nothing hands over");
        Ok(update)
    }

    /// Admit an updater that renames nothing and re-pins `installed`: as
    /// [`Self::acquire`], except that a long-lived participant already
    /// running exactly that digest and compatibility identity, with a resume
    /// capability, is admitted beside and named in
    /// [`CurrentAdmission::current`]. Any other long-lived participant, one
    /// that never registered, or a pending switch still refuses.
    ///
    /// When any is admitted beside, a pending refresh is recorded under the
    /// exclusive admission: those participants hand over to the installed
    /// executable at their next idle boundary, and every other newcomer of
    /// another identity is refused while it stands.
    pub fn acquire_current(
        root: &Path,
        installed: &InstalledGeneration<'_>,
    ) -> Result<CurrentAdmission, OrbitError> {
        let (update, current) = Self::admit(root, None, Some(installed))?;
        let refresh = match (current.is_empty(), installed.identity) {
            (false, Some(identity)) => {
                let bound = quiesce_bound();
                let requested_at = chrono::Utc::now();
                let switch = PendingSwitch {
                    pid: std::process::id(),
                    role: ParticipantRole::Command,
                    digest: installed.digest.to_string(),
                    target: identity.clone(),
                    requested_at,
                    deadline: chrono::Duration::from_std(bound)
                        .ok()
                        .and_then(|bound| requested_at.checked_add_signed(bound))
                        .unwrap_or(chrono::DateTime::<chrono::Utc>::MAX_UTC),
                    refresh: true,
                };
                Some(registry::claim_pending(&update.root, &switch)?)
            }
            _ => None,
        };
        Ok(CurrentAdmission {
            update,
            digest: installed.digest.to_string(),
            current,
            refresh,
        })
    }

    /// Admit an installer that will rename `candidate` over the executable:
    /// as [`Self::acquire`], except that a long-lived participant registered
    /// with a resume capability the candidate reports is admitted beside,
    /// and named in [`CandidateAdmission::handover`].
    pub fn acquire_for_candidate(
        root: &Path,
        candidate: &HandoverCandidate,
    ) -> Result<CandidateAdmission, OrbitError> {
        let (update, handover) = Self::admit(root, Some(candidate), None)?;
        Ok(CandidateAdmission { update, handover })
    }

    /// Take exclusive admission and then the generation lock, unless every
    /// live participant will hand over to `candidate`, or already runs
    /// `installed`. Returns those participants; when it is empty, the
    /// generation is held exclusively.
    fn admit(
        root: &Path,
        candidate: Option<&HandoverCandidate>,
        installed: Option<&InstalledGeneration<'_>>,
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
            if live.is_empty() && registry::releasing(root) {
                // A registered participant has withdrawn its record and is
                // releasing the lock: it is finishing, not unregistered.
                unregistered_since = None;
                if Instant::now() >= deadline {
                    return Err(holders_outlasted(bound, &live, candidate));
                }
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
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
                .map(|holder| match installed {
                    Some(installed) if installed.runs(holder) => Standing::Current,
                    _ => standing(holder, candidate),
                })
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

    /// Take the generation exclusively once every participant admitted beside
    /// an updater that renames nothing has handed over in place. Nobody joins
    /// meanwhile, so the live set only shrinks; once it is empty and no
    /// record is releasing, a hold that outlasts a joiner's probe belongs to
    /// a process that never registered, and refuses at once.
    fn await_refresh(&self, current: &[ParticipantRecord]) -> Result<(), OrbitError> {
        let bound = quiesce_bound();
        let deadline = Instant::now() + bound;
        let mut unregistered_since = None;
        loop {
            if FileExt::try_lock_exclusive(&self.generation.file).is_ok() {
                return Ok(());
            }
            let live = registry::live_participants(&self.root, None, false);
            if live.is_empty() && !registry::releasing(&self.root) {
                let since = *unregistered_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= PROBE_SETTLE {
                    return Err(holders_refused(&[], None));
                }
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            unregistered_since = None;
            if Instant::now() >= deadline {
                return Err(refresh_outlasted(bound, current, &live));
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
        crate::fs::io::sync_file(&self.generation.file).map_err(refusal)?;
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
