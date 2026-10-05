//! Exclusive admission for an updater, and pinning its candidate generation.

use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;

use super::admission::{GenerationGuard, Participant, admission};
use super::identity::{Access, CompatibilityIdentity, Envelope};
use super::paths::{GENERATION_LOCK, validated_generation_root};
use super::records::{Record, open, read_generation, write_compat};
use super::refusal::{SWITCH_PENDING, describe_blockers, refusal, unwritable};
use super::registry::{self, pending_switch};
use crate::OrbitError;

/// Exclusive admission, before installing a candidate or mutating resources.
pub struct GenerationUpdate {
    pub(super) root: PathBuf,
    pub(super) generation: Record,
    pub(super) admission: Record,
}

impl GenerationUpdate {
    /// Refuse before installation/resource/store writes if any process is
    /// live or a generation switch is pending.
    pub fn acquire(root: &Path) -> Result<Self, OrbitError> {
        let admission = admission(root)?;
        if let Some(switch) = pending_switch(root) {
            return Err(refusal(format!(
                "{SWITCH_PENDING} (pid {} is waiting to migrate to {})",
                switch.pid, switch.target
            )));
        }
        let mut generation = open(root, GENERATION_LOCK)?;
        if FileExt::try_lock_exclusive(&generation.file).is_err() {
            let live = registry::live_participants(root, None, false);
            return Err(refusal(format!(
                "Orbit clients or commands are still running: {}",
                describe_blockers(&live)
            )));
        }
        read_generation(&mut generation.file)?;
        Ok(Self {
            root: validated_generation_root(root)?,
            admission,
            generation,
        })
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
