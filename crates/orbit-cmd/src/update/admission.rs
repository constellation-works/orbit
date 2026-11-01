//! Generation admission: which authorities an update locks, and pinning the
//! candidate in each.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::generation::{
    CandidateAdmission, CompatibilityIdentity, CurrentAdmission, GENERATION_CONTRACT,
    GenerationGuard, GenerationUpdate, HandoverCandidate, InstalledGeneration, ParticipantRecord,
    authority_root,
};

/// Every generation authority that can hold a live pin on the host binary.
///
/// The invocation's own resolution comes first — `--root`, then `ORBIT_ROOT`,
/// otherwise the host-global root (`~/.orbit`, or `ORBIT_REGISTRY_ROOT` in a
/// managed run) — because that is the authority this process would pin as a
/// client. The host-global root follows whenever the override named something
/// else: what `orbit update` replaces is `current_exe()`, which no root
/// override moves, and every client started without an override pins the
/// host-global root. The initialized workspace selected from the current
/// directory also needs admission: convergence runs there even when its root
/// differs from both the invocation and host-global roots.
///
/// Identical authorities spelled differently collapse to one entry: flock
/// would treat a second open of the same lock as a foreign holder and refuse
/// the update against itself.
pub fn admission_authorities(
    root_override: Option<&Path>,
    workspace_root: Option<&Path>,
) -> Result<Vec<PathBuf>, OrbitError> {
    let resolved = orbit_core::runtime::resolve_generation_root(root_override)?;
    let host_global = orbit_core::runtime::resolve_global_root()?;
    let mut roots = Vec::with_capacity(3);
    push_unique_authority(&mut roots, resolved)?;
    push_unique_authority(&mut roots, host_global)?;
    if let Some(workspace_root) = workspace_root {
        push_unique_authority(&mut roots, workspace_root.to_path_buf())?;
    }
    Ok(roots)
}

fn push_unique_authority(roots: &mut Vec<PathBuf>, root: PathBuf) -> Result<(), OrbitError> {
    let identity = orbit_common::fs::generation::authority_root(&root)?;
    for existing in roots.iter() {
        if orbit_common::fs::generation::authority_root(existing)? == identity {
            return Ok(());
        }
    }
    roots.push(root);
    Ok(())
}

/// Take exclusive admission on every authority, refusing if any long-lived
/// participant is live. Short-lived ones are waited for, up to the quiesce
/// bound.
///
/// Returns one admission per root, in the same order: with an override in play
/// the operator otherwise cannot tell which set of clients to quiesce, so both
/// this refusal and a later pin failure name the authority they came from.
///
/// Each admission is also asked up front whether it could record a candidate
/// generation at all. Writability is a property of the record rather than of
/// the lock, and `pin` only runs after the executable has been replaced: a
/// host-global `~/.orbit` on a read-only mount would otherwise let an override
/// invocation swap the binary and then strand every host-global client behind
/// a record naming the generation that is gone. Refusing here also makes
/// `--preflight`, which takes the same admissions, answer for the pin.
pub fn acquire_admissions(roots: &[PathBuf]) -> Result<Vec<GenerationUpdate>, OrbitError> {
    admit_each(roots, |root| {
        let admission = GenerationUpdate::acquire(root)?;
        admission.ensure_can_record()?;
        Ok(admission)
    })
}

/// Take admission on every authority for an update that renames nothing and
/// re-pins the installed executable `digest`, as re-running an update to
/// finish convergence does: as [`acquire_admissions`], except that a
/// long-lived process already running exactly that digest and the installed
/// `identity`, with a resume capability, is admitted beside (see
/// [`current_of`]) and asked to hand over in place. Pinning waits for the
/// exclusive generation lock, which proves no unregistered holder stands
/// behind it, as a candidate handover does.
pub(super) fn acquire_current_admissions(
    roots: &[PathBuf],
    digest: &str,
    identity: Option<&CompatibilityIdentity>,
) -> Result<Vec<CurrentAdmission>, OrbitError> {
    let installed = InstalledGeneration { digest, identity };
    admit_each(roots, |root| {
        let admission = GenerationUpdate::acquire_current(root, &installed)?;
        admission.ensure_can_record()?;
        Ok(admission)
    })
}

/// Each live process the admissions kept beside because it already runs the
/// installed generation, once, oldest first.
pub(super) fn current_of(admissions: &[CurrentAdmission]) -> Vec<ParticipantRecord> {
    once_each(
        admissions
            .iter()
            .flat_map(|admission| admission.current().iter().cloned())
            .collect(),
    )
}

/// Take admission on every authority for an update that renames a
/// candidate reporting `candidate` over the executable: as
/// [`acquire_admissions`], except that a live process that will hand over to
/// the candidate after the rename is admitted beside (see
/// [`handover_of`]). Pinning waits for those processes to hand over.
pub(super) fn acquire_candidate_admissions(
    roots: &[PathBuf],
    candidate: &HandoverCandidate,
) -> Result<Vec<CandidateAdmission>, OrbitError> {
    admit_each(roots, |root| {
        let admission = GenerationUpdate::acquire_for_candidate(root, candidate)?;
        admission.ensure_can_record()?;
        Ok(admission)
    })
}

/// Each live process the admissions let hand over, once, oldest first.
pub(super) fn handover_of(admissions: &[CandidateAdmission]) -> Vec<ParticipantRecord> {
    once_each(
        admissions
            .iter()
            .flat_map(|admission| admission.handover().iter().cloned())
            .collect(),
    )
}

fn once_each(mut holders: Vec<ParticipantRecord>) -> Vec<ParticipantRecord> {
    // A process registers once per authority it joined, and may join one
    // authority more than once.
    holders.sort_by_key(|holder| (holder.pid, holder.started_at));
    holders.dedup_by_key(|holder| holder.pid);
    holders.sort_by_key(|holder| (holder.started_at, holder.pid));
    holders
}

/// Observe admission for an installer that renames the executable at
/// `candidate` over this one, as `acquire_candidate_admissions` takes it,
/// and name each process that would hand over. The admissions are released
/// on return, so this reserves nothing.
pub fn candidate_preflight(
    roots: &[PathBuf],
    candidate: &Path,
) -> Result<Vec<ParticipantRecord>, OrbitError> {
    let probed = HandoverCandidate::probe(candidate).ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "the candidate '{}' did not report the {GENERATION_CONTRACT} admission contract \
             through `update --contract`, so no live Orbit process could hand over to it",
            candidate.display()
        ))
    })?;
    Ok(handover_of(&acquire_candidate_admissions(roots, &probed)?))
}

fn admit_each<T>(
    roots: &[PathBuf],
    admit: impl Fn(&Path) -> Result<T, OrbitError>,
) -> Result<Vec<T>, OrbitError> {
    if roots.is_empty() {
        return Err(OrbitError::InvalidInput(
            "no generation authority to admit against; refusing to replace an executable \
             no live client could be observed through"
                .to_string(),
        ));
    }
    roots
        .iter()
        .map(|root| admit(root).map_err(|error| naming_authority(root, &error)))
        .collect()
}

/// An admission that can pin the installed candidate's generation.
pub(super) trait Pinnable {
    fn pin(
        self,
        digest: &str,
        identity: Option<&CompatibilityIdentity>,
    ) -> Result<GenerationGuard, OrbitError>;
}

impl Pinnable for GenerationUpdate {
    fn pin(
        self,
        digest: &str,
        identity: Option<&CompatibilityIdentity>,
    ) -> Result<GenerationGuard, OrbitError> {
        GenerationUpdate::pin(self, digest, identity)
    }
}

impl Pinnable for CurrentAdmission {
    fn pin(
        self,
        digest: &str,
        identity: Option<&CompatibilityIdentity>,
    ) -> Result<GenerationGuard, OrbitError> {
        CurrentAdmission::pin(self, digest, identity)
    }
}

impl Pinnable for CandidateAdmission {
    fn pin(
        self,
        digest: &str,
        identity: Option<&CompatibilityIdentity>,
    ) -> Result<GenerationGuard, OrbitError> {
        CandidateAdmission::pin(self, digest, identity)
    }
}

/// Pin the candidate generation in every authority admission was taken on,
/// once any process admitted to hand over has.
///
/// `admissions` is what [`acquire_admissions`] or
/// [`acquire_candidate_admissions`] returned for `roots`, so the two are
/// index-aligned.
pub(super) fn pin_candidate(
    roots: &[PathBuf],
    admissions: Vec<impl Pinnable>,
    digest: &str,
    identity: Option<&CompatibilityIdentity>,
) -> Result<CandidatePins, OrbitError> {
    let pins = roots
        .iter()
        .zip(admissions)
        .map(|(root, admission)| {
            admission
                .pin(digest, identity)
                .map(|guard| (root.clone(), Some(guard)))
                .map_err(|error| naming_authority(root, &error))
        })
        .collect::<Result<_, _>>()?;
    Ok(CandidatePins {
        pins,
        layout_target: identity.map(|identity| identity.workspace_layout.version),
    })
}

/// The candidate generation pinned in each admitted authority, held through
/// convergence so no older executable enters between the updater and the
/// candidate's convergence children.
pub(super) struct CandidatePins {
    pins: Vec<(PathBuf, Option<GenerationGuard>)>,
    /// The workspace layout version the candidate migrates to, when it
    /// reported a compatibility identity.
    layout_target: Option<u32>,
}

impl CandidatePins {
    /// Hand to a `migrate --confirm` child, run under the generation
    /// authority `child_root` resolves to, every pinned authority its layout
    /// migration of `shared_root` will fence: release the pin there.
    ///
    /// The migration holds admission exclusively on every other authority
    /// that has used the workspace while it rewrites the layout, so no client
    /// of theirs is live meanwhile. An updater's pin is a shared hold that
    /// registers no participant, which that fence cannot tell from an
    /// unknown client, so keeping it would refuse the migration against the
    /// update that started it. The fence then takes over the protection the
    /// pin gave, and stronger: a client of any generation that joins in
    /// between is live when the fence looks, so the migration refuses rather
    /// than proceed under it. The pin on the child's own authority, which it
    /// joins rather than fences, stays, as does every pin when no layout
    /// migration is pending. Returns the released authorities.
    pub(super) fn yield_to_layout_migration(
        &mut self,
        child_root: &std::path::Path,
        shared_root: &std::path::Path,
    ) -> Vec<PathBuf> {
        let Some(layout_target) = self.layout_target else {
            return Vec::new();
        };
        // Unknowable here means the migration's own fence refuses on the same
        // fault; keeping every pin is the side that cannot weaken it.
        let fenced = match orbit_core::composition::layout_migration_fenced_authorities(
            child_root,
            shared_root,
            layout_target,
        ) {
            Ok(fenced) => fenced,
            Err(error) => {
                tracing::warn!(
                    target: "orbit.update",
                    root = %shared_root.display(),
                    error = %error,
                    "kept every generation pin: the layout migration's fence is unknown"
                );
                return Vec::new();
            }
        };
        let mut released = Vec::new();
        for (root, pin) in &mut self.pins {
            if pin.is_none() || !authority_root(root).is_ok_and(|root| fenced.contains(&root)) {
                continue;
            }
            *pin = None;
            released.push(root.clone());
        }
        released
    }
}

/// Say which authority an admission failure came from.
fn naming_authority(root: &Path, error: &OrbitError) -> OrbitError {
    OrbitError::Execution(format!(
        "{error}. The generation authority is '{}'",
        root.display()
    ))
}
