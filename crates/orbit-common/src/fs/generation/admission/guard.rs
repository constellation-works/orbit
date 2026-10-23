//! Generation holds, registration, and process-wide participation.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

use chrono::Utc;
use fs2::FileExt;

use super::super::identity::{Access, CompatibilityIdentity};
use super::super::image::{process_digest, remember_process_digest};
use super::super::paths::validated_generation_root;
use super::super::records::{Record, read_generation};
use super::super::refusal::{refusal, switch_pending, upgrade_pending};
use super::super::registry::{
    self, ParticipantRecord, ParticipantRole, PendingSwitch, Registration, pending_switch,
};
use super::lock::quiesce_bound;
use crate::OrbitError;

/// A shared generation pin. Retain until all operations and replies finish.
///
/// Guards that [`GenerationGuard::for_process`] hands out for one
/// participation share its hold: the lock and record are released when the
/// last of them drops.
pub struct GenerationGuard {
    hold: Arc<Hold>,
    /// True when this process joined a recorded generation other than its own
    /// digest, without rewriting the record (read-only same-schema join).
    joined_foreign: bool,
}

/// The generation lock a join holds shared, and its registration.
struct Hold {
    /// Released in the order [`Hold::release`] gives.
    registration: Option<Registration>,
    record: Record,
    released: bool,
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
    /// Whether it runs inside an Orbit-managed activity: an agent's nested
    /// `orbit`, which may be a newer binary than the drain that started the
    /// step. It never records a pending switch nor waits behind one, since
    /// that drain yields only at the step boundary, which waits on this
    /// process. It is refused at once, under
    /// [`UPGRADE_PENDING_MARKER`](orbit_types::workflow::UPGRADE_PENDING_MARKER),
    /// which the step settles as transient.
    pub in_activity: bool,
}

/// The process-wide participation, for safe-point yield checks.
struct Participation {
    root: PathBuf,
    identity: CompatibilityIdentity,
    role: ParticipantRole,
    handover: Option<String>,
}

static PARTICIPATION: OnceLock<Participation> = OnceLock::new();

/// A live [`GenerationGuard::for_process`] join, which later joins of the
/// same root by this process reuse instead of registering again.
struct ProcessJoin {
    root: PathBuf,
    identity: CompatibilityIdentity,
    role: ParticipantRole,
    handover: Option<String>,
    access: Access,
    /// The digest `.generation.lock` recorded when this join was admitted.
    /// It cannot change while the join holds the lock shared.
    recorded: String,
    hold: Weak<Hold>,
}

static PROCESS_JOINS: Mutex<Vec<ProcessJoin>> = Mutex::new(Vec::new());

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

impl Hold {
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
        if std::mem::replace(&mut self.released, true) {
            return;
        }
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
}

impl Drop for Hold {
    fn drop(&mut self) {
        self.release(|| {});
    }
}

impl GenerationGuard {
    pub(in super::super) fn holding(record: Record, joined_foreign: bool) -> Self {
        Self {
            hold: Arc::new(Hold {
                registration: None,
                record,
                released: false,
            }),
            joined_foreign,
        }
    }

    pub(in super::super) fn registered(
        mut self,
        root: &Path,
        participant: &Participant<'_>,
    ) -> Self {
        let registration = registry::register(
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
        // Only `holding` has created this hold, so nothing shares it yet.
        if let Some(hold) = Arc::get_mut(&mut self.hold) {
            hold.registration = registration;
        }
        self
    }

    /// Drop this guard, running `paused` once its registration is withdrawn
    /// from the live set but before the generation lock is released.
    #[cfg(test)]
    pub(super) fn release_pausing(self, paused: impl FnOnce()) {
        let mut hold = Arc::into_inner(self.hold).expect("the only guard on its hold");
        hold.release(paused);
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
    /// `in_activity` is [`Participant::in_activity`].
    ///
    /// A process joins each root once. While an earlier join of this process
    /// for the same root, identity, role and handover is live, and its access
    /// covers `access` (a writer's covers a reader's), this returns another
    /// guard on that join: no admission, no second record. A pending switch
    /// for another identity still refuses it, as it would a new join.
    pub fn for_process<F>(
        root: &Path,
        identity: &CompatibilityIdentity,
        role: ParticipantRole,
        handover: Option<&str>,
        access: Access,
        in_activity: bool,
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
            in_activity,
        };
        let validated = validated_generation_root(root)?;
        if let Some(guard) = Self::rejoin(&validated, &participant)? {
            return Ok(guard);
        }
        let mut guard = Self::join(root, &participant, quiesce_bound(), store_schema)?;
        if access == Access::Write {
            remember_process_digest(root);
        }
        // Nothing else holds a fresh join's hold, so its record can be read.
        let recorded = Arc::get_mut(&mut guard.hold)
            .and_then(|hold| read_generation(&mut hold.record.file).ok());
        if let Some(recorded) = recorded {
            let mut joins = PROCESS_JOINS.lock().unwrap_or_else(PoisonError::into_inner);
            joins.retain(|join| join.hold.strong_count() > 0);
            joins.push(ProcessJoin {
                root: validated.clone(),
                identity: identity.clone(),
                role,
                handover: handover.map(str::to_string),
                access,
                recorded,
                hold: Arc::downgrade(&guard.hold),
            });
        }
        let _ = PARTICIPATION.set(Participation {
            root: validated,
            identity: identity.clone(),
            role,
            handover: handover.map(str::to_string),
        });
        Ok(guard)
    }

    /// Another guard on this process's live join that covers `participant`,
    /// if there is one.
    fn rejoin(root: &Path, participant: &Participant<'_>) -> Result<Option<Self>, OrbitError> {
        let covering = {
            let joins = PROCESS_JOINS.lock().unwrap_or_else(PoisonError::into_inner);
            joins.iter().find_map(|join| {
                let covers = join.root == root
                    && join.identity == *participant.identity
                    && join.role == participant.role
                    && join.handover.as_deref() == participant.handover
                    && (join.access == Access::Write || participant.access == Access::ReadOnly);
                covers
                    .then(|| join.hold.upgrade())
                    .flatten()
                    .map(|hold| (hold, join.recorded.clone()))
            })
        };
        let Some((hold, recorded)) = covering else {
            return Ok(None);
        };
        // The live join keeps every switch waiting anyway; refusing here
        // keeps a long-lived participant from starting new work behind one.
        if let Some(switch) = pending_switch(root)
            && switch.target != *participant.identity
        {
            return Err(if participant.in_activity {
                upgrade_pending(switch_pending(&switch))
            } else {
                refusal(switch_pending(&switch))
            });
        }
        // As a fresh join computes it: a reader of a record naming another
        // digest joins it foreign, whatever the covering join's access.
        let joined_foreign =
            participant.access == Access::ReadOnly && participant.digest != recorded;
        Ok(Some(Self {
            hold,
            joined_foreign,
        }))
    }
}
